use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::Vec3;
use mlua::prelude::*;

use crate::audio::AudioSystem;
use crate::components::{EntityId, Health, MaterialOverride, ParticleEmitter, PointLight, Tags, Transform};
use crate::events::EventBus;
use crate::font::BitmapFont;
use crate::input::InputState;
use crate::physics::PhysicsWorld;
use crate::ui::UiRenderer;
use crate::world::{EntityCommandQueue, EntityPoolManager, PoolOp, ProjectileSpawnCommand, SceneWorld};

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

        // math.lerp(a, b, t) -> number
        let math_table: LuaTable = globals.get("math").map_err(|e| e.to_string())?;
        let lerp_fn = self.lua.create_function(|_, (a, b, t): (f64, f64, f64)| {
            Ok(a + (b - a) * t)
        }).map_err(|e| e.to_string())?;
        math_table.set("lerp", lerp_fn).map_err(|e| e.to_string())?;

        // math.clamp(value, min, max) -> number
        let clamp_fn = self.lua.create_function(|_, (value, min, max): (f64, f64, f64)| {
            Ok(value.max(min).min(max))
        }).map_err(|e| e.to_string())?;
        math_table.set("clamp", clamp_fn).map_err(|e| e.to_string())?;

        // Shared game state table (accessible from all script environments via globals metatable)
        let game_table = self.lua.create_table().map_err(|e| e.to_string())?;
        game_table.set("player_health", 100).map_err(|e| e.to_string())?;
        game_table.set("game_over", false).map_err(|e| e.to_string())?;
        game_table.set("level_complete", false).map_err(|e| e.to_string())?;
        globals.set("game", game_table).map_err(|e| e.to_string())?;

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

        // input.any_just_pressed() -> bool
        let any_pressed_fn = self.lua.create_function(move |_, ()| {
            let input = unsafe { &*input_ptr };
            Ok(input.any_just_pressed())
        }).map_err(|e| e.to_string())?;
        input_table.set("any_just_pressed", any_pressed_fn).map_err(|e| e.to_string())?;

        globals.set("input", input_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register physics API functions.
    pub fn register_physics_api(
        &self,
        physics_ptr: *mut PhysicsWorld,
        scene_world_ptr: *const SceneWorld,
    ) -> Result<(), String> {
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

        // physics.hitscan(ox, oy, oz, dx, dy, dz, range) -> (hit, entity_id, distance, hx, hy, hz, nx, ny, nz)
        let hitscan_fn = self.lua.create_function(move |_, (ox, oy, oz, dx, dy, dz, range): (f32, f32, f32, f32, f32, f32, f32)| {
            let physics = unsafe { &*physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            match physics.raycast_detailed(
                Vec3::new(ox, oy, oz),
                Vec3::new(dx, dy, dz),
                range,
                None,
            ) {
                Some((entity, distance, hit_point, normal)) => {
                    // Reverse lookup: entity -> string ID
                    let entity_id = sw.entity_registry
                        .iter()
                        .find(|(_, &e)| e == entity)
                        .map(|(id, _)| id.clone())
                        .unwrap_or_default();
                    Ok((true, entity_id, distance, hit_point.x, hit_point.y, hit_point.z, normal.x, normal.y, normal.z))
                }
                None => Ok((false, String::new(), 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32)),
            }
        }).map_err(|e| e.to_string())?;
        physics_table.set("hitscan", hitscan_fn).map_err(|e| e.to_string())?;

        // physics.apply_impulse(id, fx, fy, fz)
        let apply_impulse_fn = self.lua.create_function(move |_, (id, fx, fy, fz): (String, f32, f32, f32)| {
            let physics = unsafe { &mut *physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                    physics.apply_impulse(rb.handle, Vec3::new(fx, fy, fz));
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        physics_table.set("apply_impulse", apply_impulse_fn).map_err(|e| e.to_string())?;

        // physics.apply_force(id, fx, fy, fz)
        let apply_force_fn = self.lua.create_function(move |_, (id, fx, fy, fz): (String, f32, f32, f32)| {
            let physics = unsafe { &mut *physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                    physics.apply_force(rb.handle, Vec3::new(fx, fy, fz));
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        physics_table.set("apply_force", apply_force_fn).map_err(|e| e.to_string())?;

        // physics.set_velocity(id, vx, vy, vz)
        let set_velocity_fn = self.lua.create_function(move |_, (id, vx, vy, vz): (String, f32, f32, f32)| {
            let physics = unsafe { &mut *physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                    physics.set_linvel(rb.handle, Vec3::new(vx, vy, vz), false);
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        physics_table.set("set_velocity", set_velocity_fn).map_err(|e| e.to_string())?;

        // physics.get_velocity(id) -> vx, vy, vz
        let get_velocity_fn = self.lua.create_function(move |_, id: String| {
            let physics = unsafe { &*physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                    if let Some(v) = physics.get_linvel(rb.handle) {
                        return Ok((v.x, v.y, v.z));
                    }
                }
            }
            Ok((0.0f32, 0.0f32, 0.0f32))
        }).map_err(|e| e.to_string())?;
        physics_table.set("get_velocity", get_velocity_fn).map_err(|e| e.to_string())?;

        // physics.set_restitution(id, value)
        let set_restitution_fn = self.lua.create_function(move |_, (id, value): (String, f32)| {
            let physics = unsafe { &mut *physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                    physics.set_restitution(rb.handle, value);
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        physics_table.set("set_restitution", set_restitution_fn).map_err(|e| e.to_string())?;

        // physics.set_friction(id, value)
        let set_friction_fn = self.lua.create_function(move |_, (id, value): (String, f32)| {
            let physics = unsafe { &mut *physics_ptr };
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                    physics.set_friction(rb.handle, value);
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        physics_table.set("set_friction", set_friction_fn).map_err(|e| e.to_string())?;

        globals.set("physics", physics_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register entity manipulation API (get/set position, rotation, light).
    pub fn register_entity_api(&self, scene_world_ptr: *mut SceneWorld) -> Result<(), String> {
        let globals = self.lua.globals();
        let entity_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // entity.get_position(entity_string_id) -> x, y, z
        let get_pos_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(transform) = sw.world.get::<&Transform>(entity) {
                    return Ok((transform.position.x, transform.position.y, transform.position.z));
                }
            }
            Ok((0.0f32, 0.0f32, 0.0f32))
        }).map_err(|e| e.to_string())?;
        entity_table.set("get_position", get_pos_fn).map_err(|e| e.to_string())?;

        // entity.set_position(entity_string_id, x, y, z)
        let set_pos_fn = self.lua.create_function(move |_, (id, x, y, z): (String, f32, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
                    transform.position = glam::Vec3::new(x, y, z);
                    transform.dirty = true;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_position", set_pos_fn).map_err(|e| e.to_string())?;

        // entity.get_rotation(entity_string_id) -> pitch_deg, yaw_deg, roll_deg
        let get_rot_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(transform) = sw.world.get::<&Transform>(entity) {
                    let (yaw_rad, pitch_rad, roll_rad) =
                        transform.rotation.to_euler(glam::EulerRot::YXZ);
                    return Ok((pitch_rad.to_degrees(), yaw_rad.to_degrees(), roll_rad.to_degrees()));
                }
            }
            Ok((0.0f32, 0.0f32, 0.0f32))
        }).map_err(|e| e.to_string())?;
        entity_table.set("get_rotation", get_rot_fn).map_err(|e| e.to_string())?;

        // entity.exists(entity_string_id) -> bool
        let exists_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            Ok(sw.entity_registry.contains_key(&id))
        }).map_err(|e| e.to_string())?;
        entity_table.set("exists", exists_fn).map_err(|e| e.to_string())?;

        // entity.set_rotation(entity_string_id, pitch_deg, yaw_deg, roll_deg)
        let set_rot_fn = self.lua.create_function(move |_, (id, pitch, yaw, roll): (String, f32, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
                    transform.rotation = crate::world::euler_degrees_to_quat([pitch, yaw, roll]);
                    transform.dirty = true;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_rotation", set_rot_fn).map_err(|e| e.to_string())?;

        // entity.set_light(entity_string_id, intensity)
        let set_light_fn = self.lua.create_function(move |_, (id, intensity): (String, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut light) = sw.world.get::<&mut PointLight>(entity) {
                    light.intensity = intensity;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_light", set_light_fn).map_err(|e| e.to_string())?;

        // entity.set_light_color(entity_string_id, r, g, b)
        let set_light_color_fn = self.lua.create_function(move |_, (id, r, g, b): (String, f32, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut light) = sw.world.get::<&mut PointLight>(entity) {
                    light.color = glam::Vec3::new(r, g, b);
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_light_color", set_light_color_fn).map_err(|e| e.to_string())?;

        // entity.set_emission(entity_string_id, r, g, b)
        let set_emission_fn = self.lua.create_function(move |_, (id, r, g, b): (String, f32, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                if has_override {
                    if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity) {
                        mat_override.emission = Some([r, g, b]);
                    }
                } else {
                    let _ = sw.world.insert_one(entity, MaterialOverride {
                        emission: Some([r, g, b]),
                        ..Default::default()
                    });
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_emission", set_emission_fn).map_err(|e| e.to_string())?;

        // entity.set_roughness(entity_string_id, value)
        let set_roughness_fn = self.lua.create_function(move |_, (id, value): (String, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                if has_override {
                    if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity) {
                        mat_override.roughness = Some(value);
                    }
                } else {
                    let _ = sw.world.insert_one(entity, MaterialOverride {
                        roughness: Some(value),
                        ..Default::default()
                    });
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_roughness", set_roughness_fn).map_err(|e| e.to_string())?;

        // entity.set_metallic(entity_string_id, value)
        let set_metallic_fn = self.lua.create_function(move |_, (id, value): (String, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                if has_override {
                    if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity) {
                        mat_override.metallic = Some(value);
                    }
                } else {
                    let _ = sw.world.insert_one(entity, MaterialOverride {
                        metallic: Some(value),
                        ..Default::default()
                    });
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_metallic", set_metallic_fn).map_err(|e| e.to_string())?;

        // entity.set_base_color(entity_string_id, r, g, b)
        let set_base_color_fn = self.lua.create_function(move |_, (id, r, g, b): (String, f32, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                if has_override {
                    if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity) {
                        mat_override.base_color = Some([r, g, b]);
                    }
                } else {
                    let _ = sw.world.insert_one(entity, MaterialOverride {
                        base_color: Some([r, g, b]),
                        ..Default::default()
                    });
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_base_color", set_base_color_fn).map_err(|e| e.to_string())?;

        // entity.get_health(id) -> current, max
        let get_health_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(health) = sw.world.get::<&Health>(entity) {
                    return Ok((health.current, health.max));
                }
            }
            Ok((0.0f32, 0.0f32))
        }).map_err(|e| e.to_string())?;
        entity_table.set("get_health", get_health_fn).map_err(|e| e.to_string())?;

        // entity.set_health(id, current, max)
        let set_health_fn = self.lua.create_function(move |_, (id, current, max): (String, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                    health.current = current;
                    health.max = max;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_health", set_health_fn).map_err(|e| e.to_string())?;

        // entity.damage(id, amount) -> new_current
        let damage_fn = self.lua.create_function(move |_, (id, amount): (String, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                    health.current = (health.current - amount).max(0.0);
                    return Ok(health.current);
                }
            }
            Ok(0.0f32)
        }).map_err(|e| e.to_string())?;
        entity_table.set("damage", damage_fn).map_err(|e| e.to_string())?;

        // entity.heal(id, amount) -> new_current
        let heal_fn = self.lua.create_function(move |_, (id, amount): (String, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                    health.current = (health.current + amount).min(health.max);
                    return Ok(health.current);
                }
            }
            Ok(0.0f32)
        }).map_err(|e| e.to_string())?;
        entity_table.set("heal", heal_fn).map_err(|e| e.to_string())?;

        // entity.is_alive(id) -> bool
        let is_alive_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(health) = sw.world.get::<&Health>(entity) {
                    return Ok(health.current > 0.0 && !health.dead);
                }
            }
            Ok(true) // entities without health are considered alive
        }).map_err(|e| e.to_string())?;
        entity_table.set("is_alive", is_alive_fn).map_err(|e| e.to_string())?;

        // entity.has_tag(id, tag) -> bool
        let has_tag_fn = self.lua.create_function(move |_, (id, tag): (String, String)| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(tags) = sw.world.get::<&Tags>(entity) {
                    return Ok(tags.0.contains(&tag));
                }
            }
            Ok(false)
        }).map_err(|e| e.to_string())?;
        entity_table.set("has_tag", has_tag_fn).map_err(|e| e.to_string())?;

        // entity.add_tag(id, tag)
        let add_tag_fn = self.lua.create_function(move |_, (id, tag): (String, String)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut tags) = sw.world.get::<&mut Tags>(entity) {
                    if !tags.0.contains(&tag) {
                        tags.0.push(tag);
                    }
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("add_tag", add_tag_fn).map_err(|e| e.to_string())?;

        // entity.remove_tag(id, tag)
        let remove_tag_fn = self.lua.create_function(move |_, (id, tag): (String, String)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut tags) = sw.world.get::<&mut Tags>(entity) {
                    tags.0.retain(|t| t != &tag);
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("remove_tag", remove_tag_fn).map_err(|e| e.to_string())?;

        // entity.get_tag(id) -> first tag string or nil
        let get_tag_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(tags) = sw.world.get::<&Tags>(entity) {
                    if let Some(first) = tags.0.first() {
                        return Ok(Some(first.clone()));
                    }
                }
            }
            Ok(None)
        }).map_err(|e| e.to_string())?;
        entity_table.set("get_tag", get_tag_fn).map_err(|e| e.to_string())?;

        // entity.get_tags(id) -> table of all tags
        let get_tags_fn = self.lua.create_function(move |lua, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            let result = lua.create_table()?;
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(tags) = sw.world.get::<&Tags>(entity) {
                    for (i, tag) in tags.0.iter().enumerate() {
                        result.set(i + 1, tag.clone())?;
                    }
                }
            }
            Ok(result)
        }).map_err(|e| e.to_string())?;
        entity_table.set("get_tags", get_tags_fn).map_err(|e| e.to_string())?;

        globals.set("entity", entity_table).map_err(|e| e.to_string())?;

        // --- scene table (Tier 2: Runtime Entity Queries) ---
        let scene_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // scene.find_by_tag(tag) -> {entity_id1, entity_id2, ...}
        let find_by_tag_fn = self.lua.create_function(move |lua, tag: String| {
            let sw = unsafe { &*scene_world_ptr };
            let result = lua.create_table()?;
            let mut idx = 1;
            for (_entity, (tags, entity_id)) in sw.world.query::<(&Tags, &EntityId)>().iter() {
                if tags.0.contains(&tag) {
                    result.set(idx, entity_id.0.clone())?;
                    idx += 1;
                }
            }
            Ok(result)
        }).map_err(|e| e.to_string())?;
        scene_table.set("find_by_tag", find_by_tag_fn).map_err(|e| e.to_string())?;

        // scene.find_by_tags(tag1, tag2, ...) -> entities with ALL specified tags
        let find_by_tags_fn = self.lua.create_function(move |lua, tags_arg: LuaMultiValue| {
            let sw = unsafe { &*scene_world_ptr };
            let required_tags: Vec<String> = tags_arg.into_iter().filter_map(|v| {
                if let LuaValue::String(s) = v { Some(s.to_string_lossy().to_string()) } else { None }
            }).collect();
            let result = lua.create_table()?;
            let mut idx = 1;
            for (_entity, (tags, entity_id)) in sw.world.query::<(&Tags, &EntityId)>().iter() {
                if required_tags.iter().all(|rt| tags.0.contains(rt)) {
                    result.set(idx, entity_id.0.clone())?;
                    idx += 1;
                }
            }
            Ok(result)
        }).map_err(|e| e.to_string())?;
        scene_table.set("find_by_tags", find_by_tags_fn).map_err(|e| e.to_string())?;

        globals.set("scene", scene_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register event bus API (events.emit, events.on, events.off).
    pub fn register_event_api(
        &self,
        event_bus_ptr: *mut EventBus,
        lua_listeners_ptr: *mut HashMap<String, Vec<mlua::RegistryKey>>,
        next_id_ptr: *mut u64,
        id_map_ptr: *mut HashMap<u64, (String, usize)>,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let events_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // events.emit(event_type, data_table)
        let emit_fn = self.lua.create_function(move |_, (event_type, data): (String, LuaTable)| {
            let bus = unsafe { &mut *event_bus_ptr };
            let mut map = HashMap::new();
            for pair in data.pairs::<String, LuaValue>() {
                if let Ok((key, val)) = pair {
                    let json_val = match val {
                        LuaValue::Integer(i) => serde_json::Value::Number(serde_json::Number::from(i)),
                        LuaValue::Number(n) => serde_json::Number::from_f64(n)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                        LuaValue::String(s) => serde_json::Value::String(s.to_string_lossy().to_string()),
                        LuaValue::Boolean(b) => serde_json::Value::Bool(b),
                        _ => serde_json::Value::Null,
                    };
                    map.insert(key, json_val);
                }
            }
            bus.emit(&event_type, map);
            Ok(())
        }).map_err(|e| e.to_string())?;
        events_table.set("emit", emit_fn).map_err(|e| e.to_string())?;

        // events.on(event_type, callback) -> listener_id
        let on_fn = self.lua.create_function(move |lua, (event_type, callback): (String, LuaFunction)| {
            let listeners = unsafe { &mut *lua_listeners_ptr };
            let next_id = unsafe { &mut *next_id_ptr };
            let id_map = unsafe { &mut *id_map_ptr };

            let owned_key = lua.create_registry_value(callback)
                .map_err(|e| mlua::Error::external(e))?;
            let list = listeners.entry(event_type.clone()).or_default();
            let index = list.len();
            list.push(owned_key);

            let listener_id = *next_id;
            *next_id += 1;
            id_map.insert(listener_id, (event_type, index));

            Ok(listener_id)
        }).map_err(|e| e.to_string())?;
        events_table.set("on", on_fn).map_err(|e| e.to_string())?;

        // events.off(listener_id) - remove a listener
        let off_fn = self.lua.create_function(move |_, listener_id: u64| {
            let listeners: &mut HashMap<String, Vec<mlua::RegistryKey>> = unsafe { &mut *lua_listeners_ptr };
            let id_map: &mut HashMap<u64, (String, usize)> = unsafe { &mut *id_map_ptr };

            if let Some((event_type, index)) = id_map.remove(&listener_id) {
                if let Some(list) = listeners.get_mut(&event_type) {
                    if index < list.len() {
                        list.remove(index);
                        // Reindex the id_map entries for this event type
                        for (_id, (et, idx)) in id_map.iter_mut() {
                            if *et == event_type && *idx > index {
                                *idx -= 1;
                            }
                        }
                    }
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        events_table.set("off", off_fn).map_err(|e| e.to_string())?;

        globals.set("events", events_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register audio API functions that control the audio system from Lua.
    pub fn register_audio_api(&self, audio_ptr: *mut AudioSystem, project_root: PathBuf) -> Result<(), String> {
        let globals = self.lua.globals();
        let audio_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // audio.play_sfx(id, path, volume)
        let root1 = project_root.clone();
        let play_sfx_fn = self.lua.create_function(move |_, (id, path, volume): (String, String, f32)| {
            let audio = unsafe { &mut *audio_ptr };
            if let Err(e) = audio.play_sfx(&id, &root1, &path, volume) {
                tracing::error!("[Lua] audio.play_sfx error: {}", e);
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        audio_table.set("play_sfx", play_sfx_fn).map_err(|e| e.to_string())?;

        // audio.play_music(path, volume, fade_in_secs)
        let root2 = project_root.clone();
        let play_music_fn = self.lua.create_function(move |_, (path, volume, fade_in): (String, f32, f32)| {
            let audio = unsafe { &mut *audio_ptr };
            if let Err(e) = audio.play_music(&root2, &path, volume, fade_in) {
                tracing::error!("[Lua] audio.play_music error: {}", e);
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        audio_table.set("play_music", play_music_fn).map_err(|e| e.to_string())?;

        // audio.stop_sound(id, fade_out_secs)
        let stop_sound_fn = self.lua.create_function(move |_, (id, fade_out): (String, f32)| {
            let audio = unsafe { &mut *audio_ptr };
            audio.stop_sound(&id, fade_out);
            Ok(())
        }).map_err(|e| e.to_string())?;
        audio_table.set("stop_sound", stop_sound_fn).map_err(|e| e.to_string())?;

        // audio.stop_music(fade_out_secs)
        let stop_music_fn = self.lua.create_function(move |_, fade_out: f32| {
            let audio = unsafe { &mut *audio_ptr };
            audio.stop_music(fade_out);
            Ok(())
        }).map_err(|e| e.to_string())?;
        audio_table.set("stop_music", stop_music_fn).map_err(|e| e.to_string())?;

        globals.set("audio", audio_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register entity lifecycle commands (spawn, destroy, visibility, pooling)
    /// that are deferred via the EntityCommandQueue.
    pub fn register_entity_command_api(
        &self,
        scene_world_ptr: *mut SceneWorld,
        cmd_ptr: *mut EntityCommandQueue,
        pool_mgr_ptr: *mut EntityPoolManager,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let entity_table: LuaTable = globals.get("entity").map_err(|e| e.to_string())?;

        // entity.spawn(id, mesh, material, x, y, z, sx, sy, sz)
        let spawn_fn = self.lua.create_function(move |_, (id, mesh, mat, x, y, z, sx, sy, sz): (String, String, String, f32, f32, f32, f32, f32, f32)| {
            let cmd = unsafe { &mut *cmd_ptr };
            cmd.spawns.push(crate::world::SpawnCommand {
                id, mesh, material: mat,
                position: [x, y, z],
                scale: [sx, sy, sz],
            });
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("spawn", spawn_fn).map_err(|e| e.to_string())?;

        // entity.destroy(id)
        let destroy_fn = self.lua.create_function(move |_, id: String| {
            let cmd = unsafe { &mut *cmd_ptr };
            cmd.destroys.push(id);
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("destroy", destroy_fn).map_err(|e| e.to_string())?;

        // entity.set_scale(id, sx, sy, sz)
        let set_scale_fn = self.lua.create_function(move |_, (id, sx, sy, sz): (String, f32, f32, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
                    transform.scale = glam::Vec3::new(sx, sy, sz);
                    transform.dirty = true;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_scale", set_scale_fn).map_err(|e| e.to_string())?;

        // entity.get_scale(id) -> sx, sy, sz
        let get_scale_fn = self.lua.create_function(move |_, id: String| {
            let sw = unsafe { &*scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(transform) = sw.world.get::<&Transform>(entity) {
                    return Ok((transform.scale.x, transform.scale.y, transform.scale.z));
                }
            }
            Ok((1.0f32, 1.0f32, 1.0f32))
        }).map_err(|e| e.to_string())?;
        entity_table.set("get_scale", get_scale_fn).map_err(|e| e.to_string())?;

        // entity.set_visible(id, visible)
        let set_vis_fn = self.lua.create_function(move |_, (id, visible): (String, bool)| {
            let cmd = unsafe { &mut *cmd_ptr };
            cmd.visibility_updates.push((id, visible));
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_visible", set_vis_fn).map_err(|e| e.to_string())?;

        // entity.spawn_projectile(owner_id, mesh, material, ox, oy, oz, dx, dy, dz, speed, damage, lifetime, gravity)
        let spawn_proj_fn = self.lua.create_function(move |_, (owner_id, mesh, material, ox, oy, oz, dx, dy, dz, speed, damage, lifetime, gravity): (String, String, String, f32, f32, f32, f32, f32, f32, f32, f32, f32, bool)| {
            let cmd = unsafe { &mut *cmd_ptr };
            cmd.projectile_counter += 1;
            let id = format!("proj_{}", cmd.projectile_counter);
            cmd.projectile_spawns.push(ProjectileSpawnCommand {
                id,
                mesh,
                material,
                position: [ox, oy, oz],
                direction: [dx, dy, dz],
                speed,
                damage,
                lifetime,
                gravity,
                owner_id,
                scale: [1.0, 1.0, 1.0],
            });
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("spawn_projectile", spawn_proj_fn).map_err(|e| e.to_string())?;

        // entity.destroy_by_prefix(prefix) - bulk destroy all entities whose ID starts with prefix
        let destroy_prefix_fn = self.lua.create_function(move |_, prefix: String| {
            let sw = unsafe { &*scene_world_ptr };
            let cmd = unsafe { &mut *cmd_ptr };
            let ids: Vec<String> = sw.entity_registry.keys()
                .filter(|id| id.starts_with(&prefix))
                .cloned()
                .collect();
            for id in ids {
                cmd.destroys.push(id);
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("destroy_by_prefix", destroy_prefix_fn).map_err(|e| e.to_string())?;

        // --- Tier 2: Entity Pool API ---

        // entity.pool_create(name, mesh, material, count) — create pool and pre-warm
        let pool_create_fn = self.lua.create_function(move |_, (name, mesh, material, count): (String, String, String, u32)| {
            let pool_mgr = unsafe { &mut *pool_mgr_ptr };
            let cmd = unsafe { &mut *cmd_ptr };
            pool_mgr.create_pool(&name, &mesh, &material);
            // Pre-warm by spawning `count` hidden entities
            for i in 0..count {
                let entity_id = format!("_pool_{}_{}", name, i);
                cmd.spawns.push(crate::world::SpawnCommand {
                    id: entity_id,
                    mesh: mesh.clone(),
                    material: material.clone(),
                    position: [0.0, -1000.0, 0.0], // offscreen
                    scale: [1.0, 1.0, 1.0],
                });
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("pool_create", pool_create_fn).map_err(|e| e.to_string())?;

        // entity.pool_acquire(name) -> entity_id or nil
        let pool_acquire_fn = self.lua.create_function(move |_, name: String| {
            let pool_mgr = unsafe { &mut *pool_mgr_ptr };
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(entity_id) = pool_mgr.try_acquire(&name) {
                // Unhide the entity, mark as active
                if let Some(&entity) = sw.entity_registry.get(&entity_id) {
                    let _ = sw.world.remove_one::<crate::components::Hidden>(entity);
                    if let Ok(mut pooled) = sw.world.get::<&mut crate::components::Pooled>(entity) {
                        pooled.active = true;
                    }
                    // Reset health if present
                    if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                        health.current = health.max;
                        health.dead = false;
                    }
                }
                Ok(Some(entity_id))
            } else {
                // Pool empty — spawn a new entity
                let cmd = unsafe { &mut *cmd_ptr };
                if let Some((mesh, material)) = pool_mgr.get_pool_assets(&name) {
                    let entity_id = format!("_pool_{}_{}", name, sw.entity_registry.len());
                    cmd.spawns.push(crate::world::SpawnCommand {
                        id: entity_id.clone(),
                        mesh,
                        material,
                        position: [0.0, 0.0, 0.0],
                        scale: [1.0, 1.0, 1.0],
                    });
                    pool_mgr.register_entity(&name, &entity_id);
                    Ok(Some(entity_id))
                } else {
                    Ok(None)
                }
            }
        }).map_err(|e| e.to_string())?;
        entity_table.set("pool_acquire", pool_acquire_fn).map_err(|e| e.to_string())?;

        // entity.pool_release(id) — hide + return to pool
        let pool_release_fn = self.lua.create_function(move |_, id: String| {
            let cmd = unsafe { &mut *cmd_ptr };
            cmd.pool_ops.push(PoolOp::Release(id));
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("pool_release", pool_release_fn).map_err(|e| e.to_string())?;

        // entity.pool_size(name) -> total, available
        let pool_size_fn = self.lua.create_function(move |_, name: String| {
            let pool_mgr = unsafe { &*pool_mgr_ptr };
            let (total, available) = pool_mgr.pool_size(&name);
            Ok((total as u32, available as u32))
        }).map_err(|e| e.to_string())?;
        entity_table.set("pool_size", pool_size_fn).map_err(|e| e.to_string())?;

        // --- scene.load(path) — deferred scene loading ---
        let scene_table: LuaTable = globals.get("scene").map_err(|e| e.to_string())?;
        let scene_load_fn = self.lua.create_function(move |_, path: String| {
            let cmd = unsafe { &mut *cmd_ptr };
            cmd.pending_scene_load = Some(path);
            Ok(())
        }).map_err(|e| e.to_string())?;
        scene_table.set("load", scene_load_fn).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Register camera API (world_to_screen projection).
    pub fn register_camera_api(
        &self,
        camera_state_ptr: *const crate::camera::CameraState,
        config_ptr: *const wgpu::SurfaceConfiguration,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let camera_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // camera.world_to_screen(x, y, z) -> sx, sy, visible
        let w2s_fn = self.lua.create_function(move |_, (x, y, z): (f32, f32, f32)| {
            let cs = unsafe { &*camera_state_ptr };
            let config = unsafe { &*config_ptr };
            let vp = glam::Mat4::from_cols_array_2d(&cs.uniform.view_projection);
            let clip = vp * glam::Vec4::new(x, y, z, 1.0);
            if clip.w <= 0.0 {
                return Ok((0.0f32, 0.0f32, false));
            }
            let ndc_x = clip.x / clip.w;
            let ndc_y = clip.y / clip.w;
            let ndc_z = clip.z / clip.w;
            let visible = ndc_x >= -1.0 && ndc_x <= 1.0
                && ndc_y >= -1.0 && ndc_y <= 1.0
                && ndc_z >= 0.0 && ndc_z <= 1.0;
            let sx = (ndc_x * 0.5 + 0.5) * config.width as f32;
            let sy = (1.0 - (ndc_y * 0.5 + 0.5)) * config.height as f32;
            Ok((sx, sy, visible))
        }).map_err(|e| e.to_string())?;
        camera_table.set("world_to_screen", w2s_fn).map_err(|e| e.to_string())?;

        globals.set("camera", camera_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register camera shake API (camera.shake).
    pub fn register_camera_shake_api(
        &self,
        shake_ptr: *mut CameraShakeState,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let camera_table: LuaTable = globals.get("camera").map_err(|e| e.to_string())?;

        // camera.shake(intensity, duration)
        let shake_fn = self.lua.create_function(move |_, (intensity, duration): (f32, f32)| {
            let shake = unsafe { &mut *shake_ptr };
            shake.intensity = intensity;
            shake.duration = duration;
            shake.timer = duration;
            shake.seed = shake.seed.wrapping_add(1);
            Ok(())
        }).map_err(|e| e.to_string())?;
        camera_table.set("shake", shake_fn).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Register particle system API (entity emitter control + one-shot bursts).
    pub fn register_particle_api(
        &self,
        scene_world_ptr: *mut SceneWorld,
        particle_system_ptr: *mut crate::particles::ParticleSystem,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let entity_table: LuaTable = globals.get("entity").map_err(|e| e.to_string())?;

        // entity.set_emitter_enabled(id, enabled)
        let set_emitter_fn = self.lua.create_function(move |_, (id, enabled): (String, bool)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut emitter) = sw.world.get::<&mut ParticleEmitter>(entity) {
                    emitter.enabled = enabled;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_emitter_enabled", set_emitter_fn).map_err(|e| e.to_string())?;

        // entity.set_emitter_rate(id, rate)
        let set_rate_fn = self.lua.create_function(move |_, (id, rate): (String, f32)| {
            let sw = unsafe { &mut *scene_world_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                if let Ok(mut emitter) = sw.world.get::<&mut ParticleEmitter>(entity) {
                    emitter.config.spawn_rate = rate;
                }
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("set_emitter_rate", set_rate_fn).map_err(|e| e.to_string())?;

        // entity.burst(id, count) — spawn N particles immediately on an emitter entity
        let burst_fn = self.lua.create_function(move |_, (id, count): (String, u32)| {
            let sw = unsafe { &*scene_world_ptr };
            let ps = unsafe { &mut *particle_system_ptr };
            if let Some(&entity) = sw.entity_registry.get(&id) {
                ps.burst_on_entity(entity, count, sw);
            }
            Ok(())
        }).map_err(|e| e.to_string())?;
        entity_table.set("burst", burst_fn).map_err(|e| e.to_string())?;

        // particles table for non-entity operations
        let particles_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // particles.spawn_burst(x, y, z, count, config_table)
        let spawn_burst_fn = self.lua.create_function(move |_, (x, y, z, count, config_tbl): (f32, f32, f32, u32, LuaTable)| {
            let ps = unsafe { &mut *particle_system_ptr };
            let config = crate::components::ParticleConfig {
                max_particles: count * 2,
                spawn_rate: 0.0,
                lifetime: [
                    config_tbl.get::<f32>("lifetime_min").unwrap_or(0.5),
                    config_tbl.get::<f32>("lifetime_max").unwrap_or(1.5),
                ],
                initial_speed: [
                    config_tbl.get::<f32>("speed_min").unwrap_or(1.0),
                    config_tbl.get::<f32>("speed_max").unwrap_or(3.0),
                ],
                direction: glam::Vec3::new(
                    config_tbl.get::<f32>("dir_x").unwrap_or(0.0),
                    config_tbl.get::<f32>("dir_y").unwrap_or(1.0),
                    config_tbl.get::<f32>("dir_z").unwrap_or(0.0),
                ),
                spread: config_tbl.get::<f32>("spread").unwrap_or(360.0),
                size: [
                    config_tbl.get::<f32>("size_start").unwrap_or(0.2),
                    config_tbl.get::<f32>("size_end").unwrap_or(0.05),
                ],
                color_start: [
                    config_tbl.get::<f32>("r").unwrap_or(1.0),
                    config_tbl.get::<f32>("g").unwrap_or(1.0),
                    config_tbl.get::<f32>("b").unwrap_or(1.0),
                    config_tbl.get::<f32>("a").unwrap_or(1.0),
                ],
                color_end: [
                    config_tbl.get::<f32>("r_end").unwrap_or(1.0),
                    config_tbl.get::<f32>("g_end").unwrap_or(1.0),
                    config_tbl.get::<f32>("b_end").unwrap_or(1.0),
                    0.0,
                ],
                gravity_scale: config_tbl.get::<f32>("gravity_scale").unwrap_or(0.0),
            };
            ps.spawn_burst(glam::Vec3::new(x, y, z), count, &config);
            Ok(())
        }).map_err(|e| e.to_string())?;
        particles_table.set("spawn_burst", spawn_burst_fn).map_err(|e| e.to_string())?;

        globals.set("particles", particles_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register UI overlay API (text, rect, flash, screen dimensions).
    pub fn register_ui_api(
        &self,
        ui_ptr: *mut UiRenderer,
        font_ptr: *const BitmapFont,
        config_ptr: *const wgpu::SurfaceConfiguration,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let ui_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // ui.text(x, y, text, size, r, g, b, a)
        let text_fn = self.lua.create_function(move |_, (x, y, text, size, r, g, b, a): (f32, f32, String, f32, f32, f32, f32, f32)| {
            let ui = unsafe { &mut *ui_ptr };
            let font = unsafe { &*font_ptr };
            ui.draw_text(x, y, &text, size, [r, g, b, a], font);
            Ok(())
        }).map_err(|e| e.to_string())?;
        ui_table.set("text", text_fn).map_err(|e| e.to_string())?;

        // ui.rect(x, y, w, h, r, g, b, a)
        let rect_fn = self.lua.create_function(move |_, (x, y, w, h, r, g, b, a): (f32, f32, f32, f32, f32, f32, f32, f32)| {
            let ui = unsafe { &mut *ui_ptr };
            ui.draw_rect(x, y, w, h, [r, g, b, a]);
            Ok(())
        }).map_err(|e| e.to_string())?;
        ui_table.set("rect", rect_fn).map_err(|e| e.to_string())?;

        // ui.flash(r, g, b, a, duration)
        let flash_fn = self.lua.create_function(move |_, (r, g, b, a, dur): (f32, f32, f32, f32, f32)| {
            let ui = unsafe { &mut *ui_ptr };
            ui.set_flash([r, g, b, a], dur);
            Ok(())
        }).map_err(|e| e.to_string())?;
        ui_table.set("flash", flash_fn).map_err(|e| e.to_string())?;

        // ui.text_width(text, font_size) -> pixels
        let text_width_fn = self.lua.create_function(move |_, (text, font_size): (String, f32)| {
            let font = unsafe { &*font_ptr };
            let scale = font_size / font.glyph_h;
            let width = text.len() as f32 * font.glyph_w * scale;
            Ok(width)
        }).map_err(|e| e.to_string())?;
        ui_table.set("text_width", text_width_fn).map_err(|e| e.to_string())?;

        // ui.screen_width() -> number
        let width_fn = self.lua.create_function(move |_, ()| {
            let config = unsafe { &*config_ptr };
            Ok(config.width as f32)
        }).map_err(|e| e.to_string())?;
        ui_table.set("screen_width", width_fn).map_err(|e| e.to_string())?;

        // ui.screen_height() -> number
        let height_fn = self.lua.create_function(move |_, ()| {
            let config = unsafe { &*config_ptr };
            Ok(config.height as f32)
        }).map_err(|e| e.to_string())?;
        ui_table.set("screen_height", height_fn).map_err(|e| e.to_string())?;

        globals.set("ui", ui_table).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Set the _entity_string_id variable in an entity's script environment.
    pub fn set_entity_string_id(&self, entity: hecs::Entity, string_id: &str) -> Result<(), String> {
        if let Some(key) = self.entity_envs.get(&entity) {
            let env: LuaTable = self.lua.registry_value(key).map_err(|e| e.to_string())?;
            env.set("_entity_string_id", string_id.to_string()).map_err(|e| e.to_string())?;
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
