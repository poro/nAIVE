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

type LuaListeners = Rc<RefCell<HashMap<String, Vec<LuaRegistryKey>>>>;
type LuaListenerIds = Rc<RefCell<HashMap<u64, (String, usize)>>>;

struct EventSubscriptions {
    listeners: LuaListeners,
    ids: LuaListenerIds,
}

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

/// Copy script state before on_reload so a failed replacement cannot mutate the
/// live self table. Preserve aliases and cycles within nested state tables.
fn copy_state_table(
    lua: &Lua,
    source: &LuaTable,
    seen: &mut HashMap<usize, LuaTable>,
    depth: usize,
) -> LuaResult<LuaTable> {
    let address = source.to_pointer() as usize;
    if let Some(table) = seen.get(&address) {
        return Ok(table.clone());
    }
    if depth >= 256 {
        return Err(LuaError::runtime(
            "script state exceeds the reload depth limit of 256 tables",
        ));
    }
    let target = lua.create_table()?;
    seen.insert(address, target.clone());
    for pair in source.clone().pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        let key = match key {
            LuaValue::Table(table) => {
                LuaValue::Table(copy_state_table(lua, &table, seen, depth + 1)?)
            }
            value => value,
        };
        let value = match value {
            LuaValue::Table(table) => {
                LuaValue::Table(copy_state_table(lua, &table, seen, depth + 1)?)
            }
            value => value,
        };
        target.raw_set(key, value)?;
    }
    target.set_metatable(source.metatable());
    Ok(target)
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
    errors: RefCell<Vec<String>>,
    active_entity: Rc<Cell<Option<hecs::Entity>>>,
    listener_owners: Rc<RefCell<HashMap<u64, hecs::Entity>>>,
    listener_environments: Rc<RefCell<HashMap<u64, LuaTable>>>,
    subscriptions: RefCell<Option<EventSubscriptions>>,
    staged_listener_removals: Rc<RefCell<Option<Vec<u64>>>>,
    next_pool_id: Rc<Cell<u64>>,
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
            errors: RefCell::new(Vec::new()),
            active_entity: Rc::new(Cell::new(None)),
            listener_owners: Rc::new(RefCell::new(HashMap::new())),
            listener_environments: Rc::new(RefCell::new(HashMap::new())),
            subscriptions: RefCell::new(None),
            staged_listener_removals: Rc::new(RefCell::new(None)),
            next_pool_id: Rc::new(Cell::new(0)),
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
        self.record_error(message.clone());
        self.limit_reports.borrow_mut().push(message);
    }

    /// Retain gameplay failures so headless tests cannot pass by merely logging them.
    pub fn record_error(&self, message: String) {
        let mut errors = self.errors.borrow_mut();
        if errors.len() < 256 {
            errors.push(message);
        }
    }

    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.borrow_mut())
    }

    fn run_as_entity<R>(
        &self,
        entity: hecs::Entity,
        f: impl FnOnce() -> LuaResult<R>,
    ) -> LuaResult<R> {
        struct Restore<'a>(&'a Cell<Option<hecs::Entity>>, Option<hecs::Entity>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _restore = Restore(
            &self.active_entity,
            self.active_entity.replace(Some(entity)),
        );
        self.run_limited(f)
    }

    fn owned_listener_ids(&self, entity: hecs::Entity) -> Vec<u64> {
        self.listener_owners
            .borrow()
            .iter()
            .filter_map(|(&id, &owner)| (owner == entity).then_some(id))
            .collect()
    }

    fn remove_listener(&self, id: u64) {
        self.listener_owners.borrow_mut().remove(&id);
        self.listener_environments.borrow_mut().remove(&id);
        if let Some(subscriptions) = self.subscriptions.borrow().as_ref() {
            let mut ids = subscriptions.ids.borrow_mut();
            if let Some((event, index)) = ids.remove(&id) {
                if let Some(list) = subscriptions.listeners.borrow_mut().get_mut(&event) {
                    if index < list.len() {
                        let _ = self.lua.remove_registry_value(list.remove(index));
                        for (kind, other) in ids.values_mut() {
                            if *kind == event && *other > index {
                                *other -= 1;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Remove the live subscription matching a dispatched callback. Callbacks
    /// may unsubscribe while running, so their old vector index is not stable.
    pub fn remove_event_listener(
        &self,
        event_type: &str,
        callback: &LuaFunction,
    ) -> Result<bool, String> {
        let id = {
            let subscriptions = self.subscriptions.borrow();
            let Some(subscriptions) = subscriptions.as_ref() else {
                return Ok(false);
            };
            let listeners = subscriptions.listeners.borrow();
            let Some(list) = listeners.get(event_type) else {
                return Ok(false);
            };
            let mut index = None;
            for (candidate_index, key) in list.iter().enumerate() {
                let candidate: LuaFunction =
                    self.lua.registry_value(key).map_err(|e| e.to_string())?;
                if candidate.to_pointer() == callback.to_pointer() {
                    index = Some(candidate_index);
                    break;
                }
            }
            let Some(index) = index else {
                return Ok(false);
            };
            let ids = subscriptions.ids.borrow();
            ids.iter()
                .find_map(|(&id, (event, candidate))| {
                    (event == event_type && *candidate == index).then_some(id)
                })
                .ok_or_else(|| {
                    format!("Event listener for '{event_type}' has no subscription ID")
                })?
        };
        self.remove_listener(id);
        Ok(true)
    }

    /// Release script state and owned subscriptions without calling user code.
    /// Used after scene reconciliation has already removed the ECS entity.
    pub fn forget_entity(&mut self, entity: hecs::Entity) {
        for id in self.owned_listener_ids(entity) {
            self.remove_listener(id);
        }
        self.script_sources.remove(&entity);
        if let Some(key) = self.entity_envs.remove(&entity) {
            let _ = self.lua.remove_registry_value(key);
        }
    }

    /// Take the messages recorded since the last call, for display.
    pub fn take_limit_reports(&self) -> Vec<String> {
        std::mem::take(&mut *self.limit_reports.borrow_mut())
    }

    /// Prepare a replacement without releasing the last working environment.
    fn prepare_script(
        &self,
        entity: hecs::Entity,
        source: &Path,
        code: &str,
    ) -> Result<LuaTable, String> {
        let env = self.lua.create_table().map_err(|e| e.to_string())?;
        let meta = self.lua.create_table().map_err(|e| e.to_string())?;
        meta.set("__index", self.lua.globals())
            .map_err(|e| e.to_string())?;
        env.set_metatable(Some(meta));
        env.set("_entity_id", entity.id() as u64)
            .map_err(|e| e.to_string())?;
        env.set("self", self.lua.create_table().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let chunk = self
            .lua
            .load(code)
            .set_name(source.to_string_lossy())
            .set_environment(env.clone());
        let result = self.run_as_entity(entity, || chunk.exec());
        let limit = self.limit_exceeded(result.as_ref().err());
        result.map_err(|error| format!("Script error in {:?}: {}", source, error))?;
        if let Some(limit) = limit {
            return Err(format!(
                "Script error in {:?}: {}",
                source,
                self.describe_limit(limit)
            ));
        }
        Ok(env)
    }

    fn discard_candidate_listeners(&self, entity: hecs::Entity, old_listeners: &[u64]) {
        for id in self.owned_listener_ids(entity) {
            if !old_listeners.contains(&id) {
                self.remove_listener(id);
            }
        }
    }

    fn commit_script(
        &mut self,
        entity: hecs::Entity,
        env: LuaTable,
        code: String,
        old_listeners: &[u64],
        replace_listeners: bool,
    ) -> Result<(), String> {
        let key = self.lua.create_registry_value(env).map_err(|error| {
            self.discard_candidate_listeners(entity, old_listeners);
            self.staged_listener_removals.borrow_mut().take();
            error.to_string()
        })?;
        if replace_listeners {
            for &id in old_listeners {
                self.remove_listener(id);
            }
        }
        if let Some(removals) = self.staged_listener_removals.borrow_mut().take() {
            for id in removals {
                self.remove_listener(id);
            }
        }
        if let Some(old_key) = self.entity_envs.insert(entity, key) {
            let _ = self.lua.remove_registry_value(old_key);
        }
        self.script_sources.insert(entity, code);
        Ok(())
    }

    /// Load an entity's script. Call init separately after releasing ECS borrows.
    pub fn load_script(
        &mut self,
        entity: hecs::Entity,
        project_root: &Path,
        source: &Path,
    ) -> Result<(), String> {
        let full_path = crate::project_path::resolve(project_root, source)?;
        let code = std::fs::read_to_string(&full_path)
            .map_err(|error| format!("Failed to read script {:?}: {}", full_path, error))?;
        let old_listeners = self.owned_listener_ids(entity);
        *self.staged_listener_removals.borrow_mut() = Some(Vec::new());
        let env = self
            .prepare_script(entity, source, &code)
            .inspect_err(|_| {
                self.discard_candidate_listeners(entity, &old_listeners);
                self.staged_listener_removals.borrow_mut().take();
            })?;
        self.commit_script(entity, env, code, &old_listeners, true)?;
        tracing::info!("Loaded script: {:?} for entity {:?}", source, entity);
        Ok(())
    }

    /// Call the `init` lifecycle hook on an entity's script.
    pub fn call_init(&self, entity: hecs::Entity) {
        self.call_hook(entity, "init", ());
    }

    /// Initialization must fail visibly to callers constructing a live entity.
    pub fn call_init_checked(&self, entity: hecs::Entity) -> Result<(), String> {
        self.try_call_hook(entity, "init", ())
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
        let _ = self.try_call_hook(entity, name, args);
    }

    fn try_call_hook<A: IntoLuaMulti>(
        &self,
        entity: hecs::Entity,
        name: &str,
        args: A,
    ) -> Result<(), String> {
        let key = match self.entity_envs.get(&entity) {
            Some(k) => k,
            None => return Ok(()),
        };
        let env: LuaTable = match self.lua.registry_value(key) {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let func: LuaFunction = match env.get(name) {
            Ok(f) => f,
            Err(_) => return Ok(()), // Hook not defined, that's fine
        };
        let result = self.run_as_entity(entity, || func.call::<()>(args));
        match self.limit_exceeded(result.as_ref().err()) {
            Some(limit) => {
                self.stop_script(entity, &env, name, limit);
                Err(format!(
                    "Script {:?}.{} {}",
                    entity,
                    name,
                    self.describe_limit(limit)
                ))
            }
            None => {
                if let Err(e) = result {
                    let error = format!("Script error in {:?}.{}: {}", entity, name, e);
                    self.record_error(error.clone());
                    tracing::error!("{error}");
                    Err(error)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Stop an entity's script after it exceeded a limit. Its functions and
    /// state are dropped, so it is not called again and its memory can be
    /// reclaimed. Saving the script file loads it afresh.
    fn stop_script(&self, entity: hecs::Entity, env: &LuaTable, hook: &str, limit: LimitExceeded) {
        for id in self.owned_listener_ids(entity) {
            self.remove_listener(id);
        }
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

    /// Hot-reload only after the candidate code and on_reload both succeed.
    pub fn hot_reload_script(
        &mut self,
        entity: hecs::Entity,
        project_root: &Path,
        source: &Path,
    ) -> Result<bool, String> {
        let full_path = crate::project_path::resolve(project_root, source)?;
        let code = std::fs::read_to_string(&full_path)
            .map_err(|error| format!("Failed to read script {:?}: {}", full_path, error))?;
        if self.script_sources.get(&entity) == Some(&code) {
            return Ok(false);
        }
        let old_env = self
            .entity_envs
            .get(&entity)
            .and_then(|key| self.lua.registry_value::<LuaTable>(key).ok());
        let old_listeners = self.owned_listener_ids(entity);
        *self.staged_listener_removals.borrow_mut() = Some(Vec::new());
        let candidate = (|| {
            let env = self.prepare_script(entity, source, &code)?;
            if let Some(old_env) = &old_env {
                if let Ok(state) = old_env.raw_get::<LuaTable>("self") {
                    let copied = copy_state_table(&self.lua, &state, &mut HashMap::new(), 0)
                        .map_err(|error| error.to_string())?;
                    env.set("self", copied).map_err(|error| error.to_string())?;
                }
                let id = old_env
                    .raw_get::<LuaValue>("_entity_string_id")
                    .unwrap_or(LuaNil);
                env.set("_entity_string_id", id)
                    .map_err(|error| error.to_string())?;
            }
            if let Ok(hook) = env.get::<LuaFunction>("on_reload") {
                let result = self.run_as_entity(entity, || hook.call::<()>(()));
                if let Some(limit) = self.limit_exceeded(result.as_ref().err()) {
                    return Err(format!(
                        "Reload {:?} {}",
                        source,
                        self.describe_limit(limit)
                    ));
                }
                result.map_err(|error| format!("Reload {:?}: {}", source, error))?;
            }
            Ok(env)
        })();
        let env = candidate.inspect_err(|_| {
            self.discard_candidate_listeners(entity, &old_listeners);
            self.staged_listener_removals.borrow_mut().take();
        })?;
        // Keep init-only subscriptions when the replacement supplies none.
        // Re-running init would repeat gameplay side effects.
        let replace_listeners = self
            .owned_listener_ids(entity)
            .iter()
            .any(|id| !old_listeners.contains(id));
        let state = env.get::<LuaTable>("self").ok();
        self.commit_script(entity, env, code, &old_listeners, replace_listeners)?;
        if !replace_listeners {
            if let Some(state) = state {
                for id in old_listeners {
                    if let Some(env) = self.listener_environments.borrow().get(&id) {
                        let _ = env.raw_set("self", state.clone());
                    }
                }
            }
        }
        tracing::info!("Hot-reloaded script: {:?}", source);
        Ok(true)
    }

    /// Remove a script environment for an entity.
    pub fn remove_entity(&mut self, entity: hecs::Entity) {
        self.call_on_destroy(entity);
        self.forget_entity(entity);
    }
}

impl Default for ScriptRuntime {
    fn default() -> Self {
        Self::new()
    }
}
