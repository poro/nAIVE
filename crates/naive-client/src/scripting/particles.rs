//! Lua particles API registration.

use super::{ScriptRuntime, SharedParticleSystem, SharedSceneWorld};
use crate::components::ParticleEmitter;
use mlua::prelude::*;

impl ScriptRuntime {
    /// Register particle system API (entity emitter control + one-shot bursts).
    pub fn register_particle_api(
        &self,
        scene_world: SharedSceneWorld,
        particle_system: SharedParticleSystem,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let entity_table: LuaTable = globals.get("entity").map_err(|e| e.to_string())?;

        // entity.set_emitter_enabled(id, enabled)
        let sw = scene_world.clone();
        let set_emitter_fn = self
            .lua
            .create_function(move |_, (id, enabled): (String, bool)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut emitter) = sw.world.get::<&mut ParticleEmitter>(entity) {
                        emitter.enabled = enabled;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_emitter_enabled", set_emitter_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_emitter_rate(id, rate)
        let sw = scene_world.clone();
        let set_rate_fn = self
            .lua
            .create_function(move |_, (id, rate): (String, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut emitter) = sw.world.get::<&mut ParticleEmitter>(entity) {
                        emitter.config.spawn_rate = rate;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_emitter_rate", set_rate_fn)
            .map_err(|e| e.to_string())?;

        // entity.burst(id, count) — spawn N particles immediately on an emitter entity
        let sw = scene_world.clone();
        let ps = particle_system.clone();
        let burst_fn = self
            .lua
            .create_function(move |_, (id, count): (String, u32)| {
                let sw = sw.borrow();
                let mut ps = ps.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    ps.burst_on_entity(entity, count, &sw);
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("burst", burst_fn)
            .map_err(|e| e.to_string())?;

        // particles table for non-entity operations
        let particles_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // particles.spawn_burst(x, y, z, count, config_table)
        let ps = particle_system.clone();
        let spawn_burst_fn = self
            .lua
            .create_function(
                move |_, (x, y, z, count, config_tbl): (f32, f32, f32, u32, LuaTable)| {
                    let mut ps = ps.borrow_mut();
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
                },
            )
            .map_err(|e| e.to_string())?;
        particles_table
            .set("spawn_burst", spawn_burst_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("particles", particles_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
