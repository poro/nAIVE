use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::Vec3;
use mlua::prelude::*;

use crate::input::InputState;
use crate::physics::PhysicsWorld;

/// Script component attached to entities.
#[derive(Debug, Clone)]
pub struct Script {
    pub source: PathBuf,
    pub initialized: bool,
}

/// Central scripting runtime managing all Lua VMs.
pub struct ScriptRuntime {
    pub lua: Lua,
    /// Per-entity script environments stored as Lua registry keys.
    pub entity_envs: HashMap<hecs::Entity, LuaRegistryKey>,
    /// Cached script sources for hot-reload comparison.
    pub script_sources: HashMap<PathBuf, String>,
}

impl ScriptRuntime {
    pub fn new() -> Self {
        let lua = Lua::new();

        // Disable dangerous standard library functions
        lua.globals().set("os", LuaNil).unwrap_or(());
        lua.globals().set("io", LuaNil).unwrap_or(());
        lua.globals().set("loadfile", LuaNil).unwrap_or(());
        lua.globals().set("dofile", LuaNil).unwrap_or(());

        Self {
            lua,
            entity_envs: HashMap::new(),
            script_sources: HashMap::new(),
        }
    }

    /// Load and initialize a script for an entity.
    pub fn load_script(
        &mut self,
        entity: hecs::Entity,
        project_root: &Path,
        source: &Path,
    ) -> Result<(), String> {
        let full_path = project_root.join(source);
        let code = std::fs::read_to_string(&full_path)
            .map_err(|e| format!("Failed to read script {:?}: {}", full_path, e))?;

        self.script_sources.insert(source.to_path_buf(), code.clone());

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
        let chunk = self.lua.load(&code).set_name(source.to_string_lossy());
        chunk
            .set_environment(env.clone())
            .exec()
            .map_err(|e| format!("Script error in {:?}: {}", source, e))?;

        // Store the environment
        let key = self.lua.create_registry_value(env).map_err(|e| e.to_string())?;
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
        if let Err(e) = func.call::<()>(args) {
            tracing::error!("Script error in {:?}.{}: {}", entity, name, e);
        }
    }

    /// Register the engine API functions into a Lua environment.
    pub fn register_api(&self) -> Result<(), String> {
        let globals = self.lua.globals();

        // Log function
        let log_fn = self.lua.create_function(|_, msg: String| {
            tracing::info!("[Lua] {}", msg);
            Ok(())
        }).map_err(|e| e.to_string())?;
        globals.set("log", log_fn).map_err(|e| e.to_string())?;

        // Print override
        let print_fn = self.lua.create_function(|_, args: LuaMultiValue| {
            let strs: Vec<String> = args.iter().map(|v| format!("{:?}", v)).collect();
            tracing::info!("[Lua] {}", strs.join("\t"));
            Ok(())
        }).map_err(|e| e.to_string())?;
        globals.set("print", print_fn).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Register input API functions that read from the input state.
    pub fn register_input_api(&self, input_ptr: *const InputState) -> Result<(), String> {
        let globals = self.lua.globals();

        // input.pressed(action) -> bool
        let input_table = self.lua.create_table().map_err(|e| e.to_string())?;

        let pressed_fn = self.lua.create_function(move |_, action: String| {
            let input = unsafe { &*input_ptr };
            Ok(input.pressed(&action))
        }).map_err(|e| e.to_string())?;
        input_table.set("pressed", pressed_fn).map_err(|e| e.to_string())?;

        let just_pressed_fn = self.lua.create_function(move |_, action: String| {
            let input = unsafe { &*input_ptr };
            Ok(input.just_pressed(&action))
        }).map_err(|e| e.to_string())?;
        input_table.set("just_pressed", just_pressed_fn).map_err(|e| e.to_string())?;

        let mouse_delta_fn = self.lua.create_function(move |_, ()| {
            let input = unsafe { &*input_ptr };
            let delta = input.mouse_delta();
            Ok((delta.x, delta.y))
        }).map_err(|e| e.to_string())?;
        input_table.set("mouse_delta", mouse_delta_fn).map_err(|e| e.to_string())?;

        globals.set("input", input_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register physics API functions.
    pub fn register_physics_api(&self, physics_ptr: *const PhysicsWorld) -> Result<(), String> {
        let globals = self.lua.globals();
        let physics_table = self.lua.create_table().map_err(|e| e.to_string())?;

        let raycast_fn = self.lua.create_function(move |_, (ox, oy, oz, dx, dy, dz, max_dist): (f32, f32, f32, f32, f32, f32, f32)| {
            let physics = unsafe { &*physics_ptr };
            match physics.raycast(Vec3::new(ox, oy, oz), Vec3::new(dx, dy, dz), max_dist) {
                Some((_entity, distance, normal)) => {
                    Ok((true, distance, normal.x, normal.y, normal.z))
                }
                None => Ok((false, 0.0, 0.0, 0.0, 0.0)),
            }
        }).map_err(|e| e.to_string())?;
        physics_table.set("raycast", raycast_fn).map_err(|e| e.to_string())?;

        globals.set("physics", physics_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Hot-reload a script if it has changed.
    pub fn hot_reload_script(
        &mut self,
        entity: hecs::Entity,
        project_root: &Path,
        source: &Path,
    ) -> Result<bool, String> {
        let full_path = project_root.join(source);
        let new_code = std::fs::read_to_string(&full_path)
            .map_err(|e| format!("Failed to read script {:?}: {}", full_path, e))?;

        // Check if source changed
        if let Some(old_code) = self.script_sources.get(source) {
            if *old_code == new_code {
                return Ok(false); // No change
            }
        }

        tracing::info!("Hot-reloading script: {:?}", source);

        // Preserve state from old environment
        let old_state: Option<LuaTable> = self.entity_envs.get(&entity).and_then(|key| {
            self.lua.registry_value::<LuaTable>(key).ok().and_then(|env| {
                env.get::<LuaTable>("self").ok()
            })
        });

        // Remove old environment
        if let Some(key) = self.entity_envs.remove(&entity) {
            let _ = self.lua.remove_registry_value(key);
        }

        // Reload
        self.load_script(entity, project_root, source)?;

        // Restore state
        if let Some(state) = old_state {
            if let Some(key) = self.entity_envs.get(&entity) {
                if let Ok(env) = self.lua.registry_value::<LuaTable>(key) {
                    let _ = env.set("self", state);
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
        if let Some(key) = self.entity_envs.remove(&entity) {
            let _ = self.lua.remove_registry_value(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_script_runtime_creation() {
        let runtime = ScriptRuntime::new();
        assert!(runtime.entity_envs.is_empty());
    }

    #[test]
    fn test_register_api() {
        let runtime = ScriptRuntime::new();
        runtime.register_api().unwrap();

        // Verify log function exists
        let globals = runtime.lua.globals();
        let log_fn: LuaFunction = globals.get("log").unwrap();
        log_fn.call::<()>("test message").unwrap();
    }

    #[test]
    fn test_load_and_call_script() {
        let mut runtime = ScriptRuntime::new();
        runtime.register_api().unwrap();

        // Create a temp script file
        let dir = std::env::temp_dir().join("naive_test_scripts");
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("test.lua");
        std::fs::write(&script_path, r#"
            self.count = 0
            function init()
                self.count = 1
            end
            function update(dt)
                self.count = self.count + 1
            end
        "#).unwrap();

        let world = hecs::World::new();
        let entity = world.reserve_entity();

        runtime.load_script(entity, &dir, Path::new("test.lua")).unwrap();
        runtime.call_init(entity);
        runtime.call_update(entity, 0.016);

        // Verify state
        let key = runtime.entity_envs.get(&entity).unwrap();
        let env: LuaTable = runtime.lua.registry_value(key).unwrap();
        let self_table: LuaTable = env.get("self").unwrap();
        let count: i32 = self_table.get("count").unwrap();
        assert_eq!(count, 2); // init sets to 1, update increments to 2

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
