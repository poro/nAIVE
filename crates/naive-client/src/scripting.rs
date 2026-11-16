//! Sandboxed Lua runtime, script lifecycle, and shared API state.
//! Domain API registration lives in the `scripting/` modules.

mod animation;
mod audio;
mod base;
mod camera;
mod entity;
mod events;
mod input;
mod lifecycle;
mod mesh;
mod particles;
mod physics;
mod ui;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mlua::prelude::*;
use mlua::{HookTriggers, LuaOptions, StdLib, VmState};

use crate::audio::AudioSystem;
use crate::events::EventBus;
use crate::font::BitmapFont;
use crate::input::InputState;
use crate::physics::PhysicsWorld;
use crate::ui::UiRenderer;
use crate::world::{EntityCommandQueue, EntityPoolManager, SceneWorld};

// Shared reference types for safe Lua closure captures.
pub type SharedSceneWorld = Rc<RefCell<SceneWorld>>;
pub type SharedPhysicsWorld = Rc<RefCell<PhysicsWorld>>;
pub type SharedInputState = Rc<RefCell<InputState>>;
pub type SharedEntityCommandQueue = Rc<RefCell<EntityCommandQueue>>;
pub type SharedEntityPoolManager = Rc<RefCell<EntityPoolManager>>;
pub type SharedUiRenderer = Rc<RefCell<UiRenderer>>;
pub type SharedBitmapFont = Rc<RefCell<BitmapFont>>;
pub type SharedCameraShakeState = Rc<RefCell<CameraShakeState>>;
pub type SharedEventBus = Rc<RefCell<EventBus>>;
pub type SharedAudioSystem = Rc<RefCell<AudioSystem>>;
pub type SharedParticleSystem = Rc<RefCell<crate::particles::ParticleSystem>>;

/// Script component attached to entities.
#[derive(Debug, Clone)]
pub struct Script {
    pub source: PathBuf,
    pub initialized: bool,
}

/// Camera shake state, stored in Engine and passed by pointer to Lua.
pub struct CameraShakeState {
    pub intensity: f32,
    pub duration: f32,
    pub timer: f32,
    pub seed: u32,
}

impl CameraShakeState {
    pub fn new() -> Self {
        Self {
            intensity: 0.0,
            duration: 0.0,
            timer: 0.0,
            seed: 0,
        }
    }
}

impl Default for CameraShakeState {
    fn default() -> Self {
        Self::new()
    }
}

/// How often the CPU limit is checked, in Lua VM instructions.
const HOOK_INTERVAL: u32 = 10_000;

/// Resource limits for scripts. Every script shares one Lua state, so the
/// memory limit is a total, while the CPU limit applies to each call.
#[derive(Debug, Clone, Copy)]
pub struct ScriptLimits {
    /// Lua VM instructions that one call from the engine may execute. A call
    /// is one hook such as `update`, a script's top-level code, one event
    /// listener, or one `run_lua` command.
    pub instructions_per_call: u64,
    /// Memory the Lua state may hold, in bytes.
    pub memory_bytes: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            instructions_per_call: 50_000_000,
            memory_bytes: 256 * 1024 * 1024,
        }
    }
}

/// The limit a script ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitExceeded {
    Cpu,
    Memory,
}

/// Instruction count of the call in progress, shared with the VM hook.
struct CallBudget {
    limit: u64,
    used: Cell<u64>,
    /// Set once the call in progress has gone over `limit`.
    exhausted: Cell<bool>,
    /// How many `run_limited` calls are on the stack. Zero means no script
    /// call is in progress.
    depth: Cell<u32>,
}

/// Install the VM hook that enforces the CPU limit, checking every
/// `interval` instructions.
fn install_cpu_hook(lua: &Lua, budget: Rc<CallBudget>, interval: u32) {
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(interval),
        move |lua, _debug| {
            if budget.depth.get() == 0 {
                // Engine setup code, not a script call.
                return Ok(VmState::Continue);
            }
            if !budget.exhausted.get() {
                let used = budget.used.get() + u64::from(interval);
                budget.used.set(used);
                if used <= budget.limit {
                    return Ok(VmState::Continue);
                }
                budget.exhausted.set(true);
                // Check before every instruction from here on. With a wider
                // interval, a script that catches the error with `pcall`
                // inside a loop would keep running between checks.
                install_cpu_hook(lua, budget.clone(), 1);
            }
            Err(LuaError::RuntimeError(format!(
                "script exceeded its CPU limit of {} instructions in one call",
                budget.limit
            )))
        },
    );
}

/// Whether `error` is, or wraps, a Lua out-of-memory error.
fn is_memory_error(error: &LuaError) -> bool {
    match error {
        LuaError::MemoryError(_) => true,
        LuaError::CallbackError { cause, .. } => is_memory_error(cause),
        LuaError::WithContext { cause, .. } => is_memory_error(cause),
        _ => false,
    }
}

/// Central scripting runtime managing all Lua VMs.
pub struct ScriptRuntime {
    pub lua: Lua,
    /// Per-entity script environments stored as Lua registry keys.
    pub entity_envs: HashMap<hecs::Entity, LuaRegistryKey>,
    /// The source each entity's script was loaded from, to detect a change
    /// on hot reload. Kept per entity because several can share one file.
    pub script_sources: HashMap<hecs::Entity, String>,
    limits: ScriptLimits,
    budget: Rc<CallBudget>,
    /// One line per script stopped for exceeding a limit, until the engine
    /// takes them for display.
    limit_reports: RefCell<Vec<String>>,
}

impl ScriptRuntime {
    pub fn new() -> Self {
        Self::with_limits(ScriptLimits::default())
    }

    /// Create a runtime with specific CPU and memory limits.
    pub fn with_limits(limits: ScriptLimits) -> Self {
        // Load only the libraries scripts need. `os` and `io` would give
        // scripts the shell and the filesystem, and `package` would hand both
        // back through `require` and `package.loaded`. `coroutine` is left out
        // because the CPU hook only covers the main thread, so a loop inside
        // a coroutine would escape the limit. `debug` could remove the hook.
        let libs = StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
        let lua =
            Lua::new_with(libs, LuaOptions::default()).expect("failed to create the Lua state");

        // The base library is always loaded; drop its file loaders.
        lua.globals().set("loadfile", LuaNil).unwrap_or(());
        lua.globals().set("dofile", LuaNil).unwrap_or(());

        if let Err(e) = lua.set_memory_limit(limits.memory_bytes) {
            tracing::warn!("Could not set the Lua memory limit: {}", e);
        }

        let budget = Rc::new(CallBudget {
            limit: limits.instructions_per_call,
            used: Cell::new(0),
            exhausted: Cell::new(false),
            depth: Cell::new(0),
        });
        install_cpu_hook(&lua, budget.clone(), HOOK_INTERVAL);

        Self {
            lua,
            entity_envs: HashMap::new(),
            script_sources: HashMap::new(),
            limits,
            budget,
            limit_reports: RefCell::new(Vec::new()),
        }
    }

    /// Run `f`, which calls into Lua, under the per-call CPU limit. Every
    /// call from the engine into script code goes through here. A nested
    /// call shares the budget of the outermost one.
    pub fn run_limited<R>(&self, f: impl FnOnce() -> LuaResult<R>) -> LuaResult<R> {
        struct Leave<'a>(&'a CallBudget);
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                self.0.depth.set(self.0.depth.get() - 1);
            }
        }

        let budget = &self.budget;
        if budget.depth.get() == 0 {
            budget.used.set(0);
            budget.exhausted.set(false);
        }
        budget.depth.set(budget.depth.get() + 1);
        let result = {
            let _leave = Leave(budget);
            f()
        };

        if budget.depth.get() == 0 && budget.exhausted.get() {
            // The hook was checking every instruction; go back to the normal
            // interval for the next call.
            install_cpu_hook(&self.lua, budget.clone(), HOOK_INTERVAL);
        }
        result
    }

    /// The limit that the `run_limited` call which just returned ran into, if
    /// any. Pass that call's error. A script can swallow the CPU error with
    /// `pcall`, so that limit is reported even when the call returned `Ok`.
    pub fn limit_exceeded(&self, error: Option<&LuaError>) -> Option<LimitExceeded> {
        if self.budget.exhausted.get() {
            return Some(LimitExceeded::Cpu);
        }
        match error {
            Some(e) if is_memory_error(e) => Some(LimitExceeded::Memory),
            _ => None,
        }
    }

    /// Describe `limit` for a log line or an on-screen message.
    pub fn describe_limit(&self, limit: LimitExceeded) -> String {
        match limit {
            LimitExceeded::Cpu => format!(
                "ran more than {} instructions in one call",
                self.limits.instructions_per_call
            ),
            LimitExceeded::Memory => format!(
                "pushed Lua memory past {} MB",
                self.limits.memory_bytes / (1024 * 1024)
            ),
        }
    }

    /// Record a one-line message about a script that hit a limit.
    pub fn report_limit(&self, message: String) {
        self.limit_reports.borrow_mut().push(message);
    }

    /// Take the messages recorded since the last call, for display.
    pub fn take_limit_reports(&self) -> Vec<String> {
        std::mem::take(&mut *self.limit_reports.borrow_mut())
    }

    /// Load and initialize a script for an entity.
    pub fn load_script(
        &mut self,
        entity: hecs::Entity,
        project_root: &Path,
        source: &Path,
    ) -> Result<(), String> {
        let full_path = crate::project_path::resolve(project_root, source)?;
        let code = std::fs::read_to_string(&full_path)
            .map_err(|e| format!("Failed to read script {:?}: {}", full_path, e))?;

        self.script_sources.insert(entity, code.clone());

        // Create per-entity environment table
        let env = self.lua.create_table().map_err(|e| e.to_string())?;

        // Set up environment with access to globals (math, string, table, etc.)
        let globals = self.lua.globals();
        let meta = self.lua.create_table().map_err(|e| e.to_string())?;
        meta.set("__index", globals).map_err(|e| e.to_string())?;
        env.set_metatable(Some(meta));

        // Store entity ID in the environment
        env.set("_entity_id", entity.id() as u64)
            .map_err(|e| e.to_string())?;

        // Initialize the entity table for storing per-entity state
        let entity_table = self.lua.create_table().map_err(|e| e.to_string())?;
        env.set("self", entity_table).map_err(|e| e.to_string())?;

        // Load and execute the script in the environment
        let chunk = self
            .lua
            .load(&code)
            .set_name(source.to_string_lossy())
            .set_environment(env.clone());
        self.run_limited(|| chunk.exec())
            .map_err(|e| format!("Script error in {:?}: {}", source, e))?;

        // Store the environment
        let key = self
            .lua
            .create_registry_value(env)
            .map_err(|e| e.to_string())?;
        self.entity_envs.insert(entity, key);

        tracing::info!("Loaded script: {:?} for entity {:?}", source, entity);
        Ok(())
    }

    /// Call the `init` lifecycle hook on an entity's script.
    pub fn call_init(&self, entity: hecs::Entity) {
        self.call_hook(entity, "init", ());
    }

    /// Call the `update` lifecycle hook with delta time.
    pub fn call_update(&self, entity: hecs::Entity, dt: f32) {
        self.call_hook(entity, "update", dt);
    }

    /// Call the `fixed_update` lifecycle hook with fixed dt.
    pub fn call_fixed_update(&self, entity: hecs::Entity, dt: f32) {
        self.call_hook(entity, "fixed_update", dt);
    }

    /// Call the `on_destroy` lifecycle hook.
    pub fn call_on_destroy(&self, entity: hecs::Entity) {
        self.call_hook(entity, "on_destroy", ());
    }

    /// Call the `on_collision` hook.
    pub fn call_on_collision(&self, entity: hecs::Entity, other_entity_id: &str) {
        self.call_hook(entity, "on_collision", other_entity_id.to_string());
    }

    /// Call the `on_trigger_enter` hook.
    pub fn call_on_trigger_enter(&self, entity: hecs::Entity, other_entity_id: &str) {
        self.call_hook(entity, "on_trigger_enter", other_entity_id.to_string());
    }

    /// Call the `on_trigger_exit` hook.
    pub fn call_on_trigger_exit(&self, entity: hecs::Entity, other_entity_id: &str) {
        self.call_hook(entity, "on_trigger_exit", other_entity_id.to_string());
    }

    /// Call the `on_damage` hook with damage amount and source entity ID.
    pub fn call_on_damage(&self, entity: hecs::Entity, amount: f32, source_id: String) {
        self.call_hook(entity, "on_damage", (amount, source_id));
    }

    /// Call the `on_death` hook.
    pub fn call_on_death(&self, entity: hecs::Entity) {
        self.call_hook(entity, "on_death", ());
    }

    /// Internal: call a named function in an entity's environment.
    fn call_hook<A: IntoLuaMulti>(&self, entity: hecs::Entity, name: &str, args: A) {
        let key = match self.entity_envs.get(&entity) {
            Some(k) => k,
            None => return,
        };
        let env: LuaTable = match self.lua.registry_value(key) {
            Ok(t) => t,
            Err(_) => return,
        };
        let func: LuaFunction = match env.get(name) {
            Ok(f) => f,
            Err(_) => return, // Hook not defined, that's fine
        };
        let result = self.run_limited(|| func.call::<()>(args));
        match self.limit_exceeded(result.as_ref().err()) {
            Some(limit) => self.stop_script(entity, &env, name, limit),
            None => {
                if let Err(e) = result {
                    tracing::error!("Script error in {:?}.{}: {}", entity, name, e);
                }
            }
        }
    }

    /// Stop an entity's script after it exceeded a limit. Its functions and
    /// state are dropped, so it is not called again and its memory can be
    /// reclaimed. Saving the script file loads it afresh.
    fn stop_script(&self, entity: hecs::Entity, env: &LuaTable, hook: &str, limit: LimitExceeded) {
        let numeric_id = env.raw_get::<LuaValue>("_entity_id").unwrap_or(LuaNil);
        let string_id = env
            .raw_get::<LuaValue>("_entity_string_id")
            .unwrap_or(LuaNil);
        let name = match &string_id {
            LuaValue::String(s) => s.to_string_lossy(),
            _ => format!("{:?}", entity),
        };

        tracing::error!(
            "Script stopped: '{}' {} (in `{}`). Fix the script and save it to reload.",
            name,
            self.describe_limit(limit),
            hook
        );
        let limit_name = match limit {
            LimitExceeded::Cpu => "CPU",
            LimitExceeded::Memory => "memory",
        };
        self.report_limit(format!(
            "Script stopped: {} hit the {} limit",
            name, limit_name
        ));

        // Empty the environment but keep the ids, which a reload carries over.
        let _ = env.clear();
        let _ = env.raw_set("_entity_id", numeric_id);
        let _ = env.raw_set("_entity_string_id", string_id);

        // Two passes: the second frees what the first one's finalizers released.
        let _ = self.lua.gc_collect();
        let _ = self.lua.gc_collect();
    }

    /// Set the _entity_string_id variable in an entity's script environment.
    pub fn set_entity_string_id(
        &self,
        entity: hecs::Entity,
        string_id: &str,
    ) -> Result<(), String> {
        if let Some(key) = self.entity_envs.get(&entity) {
            let env: LuaTable = self.lua.registry_value(key).map_err(|e| e.to_string())?;
            env.set("_entity_string_id", string_id.to_string())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Hot-reload a script if it has changed.
    pub fn hot_reload_script(
        &mut self,
        entity: hecs::Entity,
        project_root: &Path,
        source: &Path,
    ) -> Result<bool, String> {
        let full_path = crate::project_path::resolve(project_root, source)?;
        let new_code = std::fs::read_to_string(&full_path)
            .map_err(|e| format!("Failed to read script {:?}: {}", full_path, e))?;

        // Check if source changed
        if let Some(old_code) = self.script_sources.get(&entity) {
            if *old_code == new_code {
                return Ok(false); // No change
            }
        }

        tracing::info!("Hot-reloading script: {:?}", source);

        // Preserve state and the entity's string id from the old environment
        let old_env: Option<LuaTable> = self
            .entity_envs
            .get(&entity)
            .and_then(|key| self.lua.registry_value::<LuaTable>(key).ok());
        let old_state: Option<LuaTable> = old_env
            .as_ref()
            .and_then(|env| env.raw_get::<LuaTable>("self").ok());
        let old_string_id: Option<String> = old_env.as_ref().and_then(|env| {
            env.raw_get::<Option<String>>("_entity_string_id")
                .ok()
                .flatten()
        });

        // Remove old environment
        if let Some(key) = self.entity_envs.remove(&entity) {
            let _ = self.lua.remove_registry_value(key);
        }

        // Reload
        self.load_script(entity, project_root, source)?;

        // Restore state and id
        if let Some(key) = self.entity_envs.get(&entity) {
            if let Ok(env) = self.lua.registry_value::<LuaTable>(key) {
                if let Some(state) = old_state {
                    let _ = env.set("self", state);
                }
                if let Some(string_id) = old_string_id {
                    let _ = env.set("_entity_string_id", string_id);
                }
            }
        }

        // Call on_reload hook
        self.call_hook(entity, "on_reload", ());

        Ok(true)
    }

    /// Remove a script environment for an entity.
    pub fn remove_entity(&mut self, entity: hecs::Entity) {
        self.call_on_destroy(entity);
        self.script_sources.remove(&entity);
        if let Some(key) = self.entity_envs.remove(&entity) {
            let _ = self.lua.remove_registry_value(key);
        }
    }
}

impl Default for ScriptRuntime {
    fn default() -> Self {
        Self::new()
    }
}
