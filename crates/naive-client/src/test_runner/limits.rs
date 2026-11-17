//! Limits shared by a test's Lua VM and its Rust simulation callbacks.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::prelude::*;
use mlua::{HookTriggers, LuaOptions, StdLib, VmState};

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

fn install_hook(lua: &Lua, budget: Rc<TestBudget>, interval: u32) -> LuaResult<()> {
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(interval),
        move |lua, _| {
            let used = budget.used.get().saturating_add(u64::from(interval));
            budget.used.set(used);
            if used > budget.limits.instructions {
                budget.record(Exceeded::Instructions);
            }
            if let Err(error) = budget.check() {
                // A retry loop around pcall must not execute between limit errors.
                if interval != 1 {
                    install_hook(lua, budget.clone(), 1)?;
                }
                return Err(error);
            }
            Ok(VmState::Continue)
        },
    )
}

pub(super) fn create_lua(budget: Rc<TestBudget>) -> LuaResult<Lua> {
    // The hook covers the main Lua thread. Omit coroutine and debug, which
    // could otherwise escape or remove it, and filesystem/process libraries.
    // Runtime code generation is unavailable: load(reader) can swallow a
    // compiler allocation failure by returning nil instead of raising it.
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.globals().set("loadfile", LuaNil)?;
    lua.globals().set("dofile", LuaNil)?;
    lua.globals().set("load", LuaNil)?;
    lua.set_memory_limit(budget.limits.memory_bytes)?;

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
            local set_meta, raw_get, value_type, raise = setmetatable, rawget, type, error
            return function(observe)
                setmetatable = function(value, metatable)
                    -- Lua disables hooks and suppresses errors in finalizers.
                    -- It marks an object for finalization only when assigning
                    -- its metatable, so checking the raw field here also blocks
                    -- metatables that gained __gc after an earlier assignment.
                    if value_type(metatable) == 'table' and raw_get(metatable, '__gc') ~= nil then
                        raise('test VM does not support __gc finalizers', 2)
                    end
                    return set_meta(value, metatable)
                end
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
