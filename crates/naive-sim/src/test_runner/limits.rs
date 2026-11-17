//! Limits shared by a test's Lua VM and its Rust simulation callbacks.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::prelude::*;
use mlua::{HookTriggers, VmState};

const HOOK_INTERVAL: u32 = 10_000;

#[derive(Clone, Copy)]
pub(super) struct TestLimits {
    pub instructions: u64,
    pub memory_bytes: usize,
    pub wall_time: Duration,
}

impl Default for TestLimits {
    fn default() -> Self {
        Self {
            instructions: 50_000_000,
            memory_bytes: 256 * 1024 * 1024,
            wall_time: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy)]
enum Exceeded {
    Instructions,
    Memory,
    Deadline,
}

pub(super) struct TestBudget {
    limits: TestLimits,
    deadline: Instant,
    used: Cell<u64>,
    // Never cleared: loading another scene or catching an error cannot pass a test.
    exceeded: Cell<Option<Exceeded>>,
}

impl TestBudget {
    pub fn new(limits: TestLimits) -> Rc<Self> {
        Rc::new(Self {
            limits,
            deadline: Instant::now() + limits.wall_time,
            used: Cell::new(0),
            exceeded: Cell::new(None),
        })
    }

    fn record(&self, exceeded: Exceeded) {
        if self.exceeded.get().is_none() {
            self.exceeded.set(Some(exceeded));
        }
    }

    /// Also called in Rust loops, where a Lua instruction hook cannot run.
    pub fn check(&self) -> LuaResult<()> {
        if Instant::now() >= self.deadline {
            self.record(Exceeded::Deadline);
        }
        let message = match self.exceeded.get() {
            Some(Exceeded::Instructions) => format!(
                "test exceeded its CPU limit of {} instructions",
                self.limits.instructions
            ),
            Some(Exceeded::Memory) => format!(
                "test exceeded its Lua memory limit of {} bytes",
                self.limits.memory_bytes
            ),
            Some(Exceeded::Deadline) => format!(
                "test exceeded its wall-clock deadline of {:.3}s",
                self.limits.wall_time.as_secs_f64()
            ),
            None => return Ok(()),
        };
        Err(LuaError::RuntimeError(message))
    }

    pub fn finish<T>(&self, result: LuaResult<T>) -> LuaResult<T> {
        if let Err(error) = &result {
            if is_memory_error(error) {
                self.record(Exceeded::Memory);
            }
        }
        self.check()?;
        result
    }
}

fn is_memory_error(error: &LuaError) -> bool {
    match error {
        LuaError::MemoryError(_) => true,
        LuaError::CallbackError { cause, .. } | LuaError::WithContext { cause, .. } => {
            is_memory_error(cause)
        }
        _ => false,
    }
}

impl TestBudget {
    /// Charge `units` of work: VM instructions from the hook, or library work
    /// from the sandbox (`crate::sandbox`).
    fn charge(self: &Rc<Self>, lua: &Lua, units: u64, interval: u32) -> LuaResult<()> {
        let used = self.used.get().saturating_add(units);
        self.used.set(used);
        if used > self.limits.instructions {
            self.record(Exceeded::Instructions);
        }
        if let Err(error) = self.check() {
            // A retry loop around pcall must not execute between limit errors.
            if interval != 1 {
                install_hook(lua, self.clone(), 1)?;
            }
            return Err(error);
        }
        Ok(())
    }
}

fn install_hook(lua: &Lua, budget: Rc<TestBudget>, interval: u32) -> LuaResult<()> {
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(interval),
        move |lua, _| {
            budget.charge(lua, u64::from(interval), interval)?;
            Ok(VmState::Continue)
        },
    )
}

pub(super) fn create_lua(budget: Rc<TestBudget>) -> LuaResult<Lua> {
    // The shared sandbox (`crate::sandbox`) omits coroutine, debug and the
    // filesystem/process libraries, refuses finalizers and charges library
    // work to this budget. Runtime code generation is unavailable here:
    // load(reader) can swallow a compiler allocation failure by returning nil
    // instead of raising it.
    let charge: crate::sandbox::Charge = {
        let budget = budget.clone();
        // Library charges always switch to per-instruction checks once the
        // budget is gone, like the hook.
        Rc::new(move |lua, units| budget.charge(lua, units, HOOK_INTERVAL))
    };
    let lua = crate::sandbox::create_lua(
        crate::sandbox::SandboxOptions {
            memory_bytes: budget.limits.memory_bytes,
            allow_load: false,
        },
        charge,
    )?;

    let observed_budget = budget.clone();
    let observe_error = lua.create_function(move |_, error: LuaValue| {
        // Native Lua protected calls turn allocation failures into this
        // preallocated string, rather than an mlua Error::MemoryError.
        let memory_error = match error {
            LuaValue::String(message) => message.as_bytes().as_ref() == b"not enough memory",
            LuaValue::Error(error) => is_memory_error(&error),
            _ => false,
        };
        if memory_error {
            observed_budget.record(Exceeded::Memory);
        }
        Ok(())
    })?;

    // Keep the original protected calls and packing functions in inaccessible
    // upvalues. Observing xpcall's result is necessary because Lua does not call
    // the message handler for an allocation failure.
    let wrap: LuaFunction = lua
        .load(
            r#"
            local protected_call, protected_xcall = pcall, xpcall
            local pack, unpack = table.pack, table.unpack
            return function(observe)
                pcall = function(...)
                    local result = pack(protected_call(...))
                    if not result[1] then observe(result[2]) end
                    return unpack(result, 1, result.n)
                end
                xpcall = function(fn, handler, ...)
                    local result = pack(protected_xcall(fn, function(err)
                        observe(err)
                        return handler(err)
                    end, ...))
                    if not result[1] then observe(result[2]) end
                    return unpack(result, 1, result.n)
                end
            end
            "#,
        )
        .eval()?;
    wrap.call::<()>(observe_error)?;
    install_hook(&lua, budget, HOOK_INTERVAL)?;
    Ok(lua)
}
