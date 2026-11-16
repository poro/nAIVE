//! Lua physics API registration.

use super::{ScriptRuntime, SharedPhysicsWorld, SharedSceneWorld};
use glam::Vec3;

impl ScriptRuntime {
    /// Register physics API functions.
    pub fn register_physics_api(
        &self,
        physics: SharedPhysicsWorld,
        scene_world: SharedSceneWorld,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let physics_table = self.lua.create_table().map_err(|e| e.to_string())?;

        let pw = physics.clone();
        let raycast_fn =
            self.lua
                .create_function(
                    move |_,
                          (ox, oy, oz, dx, dy, dz, max_dist): (
                        f32,
                        f32,
                        f32,
                        f32,
                        f32,
                        f32,
                        f32,
                    )| {
                        match pw.borrow().raycast(
                            Vec3::new(ox, oy, oz),
                            Vec3::new(dx, dy, dz),
                            max_dist,
                        ) {
                            Some((_entity, distance, normal)) => {
                                Ok((true, distance, normal.x, normal.y, normal.z))
                            }
                            None => Ok((false, 0.0, 0.0, 0.0, 0.0)),
                        }
                    },
                )
                .map_err(|e| e.to_string())?;
        physics_table
            .set("raycast", raycast_fn)
            .map_err(|e| e.to_string())?;

        // physics.hitscan(ox, oy, oz, dx, dy, dz, range) -> (hit, entity_id, distance, hx, hy, hz, nx, ny, nz)
        let pw = physics.clone();
        let sw = scene_world.clone();
        let hitscan_fn = self
            .lua
            .create_function(
                move |_, (ox, oy, oz, dx, dy, dz, range): (f32, f32, f32, f32, f32, f32, f32)| {
                    let pw = pw.borrow();
                    let sw = sw.borrow();
                    match pw.raycast_detailed(
                        Vec3::new(ox, oy, oz),
                        Vec3::new(dx, dy, dz),
                        range,
                        None,
                    ) {
                        Some((entity, distance, hit_point, normal)) => {
                            let entity_id = sw
                                .entity_registry
                                .iter()
                                .find(|(_, &e)| e == entity)
                                .map(|(id, _)| id.clone())
                                .unwrap_or_default();
                            Ok((
                                true,
                                entity_id,
                                distance,
                                hit_point.x,
                                hit_point.y,
                                hit_point.z,
                                normal.x,
                                normal.y,
                                normal.z,
                            ))
                        }
                        None => Ok((
                            false,
                            String::new(),
                            0.0f32,
                            0.0f32,
                            0.0f32,
                            0.0f32,
                            0.0f32,
                            0.0f32,
                            0.0f32,
                        )),
                    }
                },
            )
            .map_err(|e| e.to_string())?;
        physics_table
            .set("hitscan", hitscan_fn)
            .map_err(|e| e.to_string())?;

        // physics.apply_impulse(id, fx, fy, fz)
        let pw = physics.clone();
        let sw = scene_world.clone();
        let apply_impulse_fn = self
            .lua
            .create_function(move |_, (id, fx, fy, fz): (String, f32, f32, f32)| {
                let mut pw = pw.borrow_mut();
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        pw.apply_impulse(rb.handle, Vec3::new(fx, fy, fz));
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        physics_table
            .set("apply_impulse", apply_impulse_fn)
            .map_err(|e| e.to_string())?;

        // physics.apply_force(id, fx, fy, fz)
        let pw = physics.clone();
        let sw = scene_world.clone();
        let apply_force_fn = self
            .lua
            .create_function(move |_, (id, fx, fy, fz): (String, f32, f32, f32)| {
                let mut pw = pw.borrow_mut();
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        pw.apply_force(rb.handle, Vec3::new(fx, fy, fz));
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        physics_table
            .set("apply_force", apply_force_fn)
            .map_err(|e| e.to_string())?;

        // physics.set_velocity(id, vx, vy, vz)
        let pw = physics.clone();
        let sw = scene_world.clone();
        let set_velocity_fn = self
            .lua
            .create_function(move |_, (id, vx, vy, vz): (String, f32, f32, f32)| {
                let mut pw = pw.borrow_mut();
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        pw.set_linvel(rb.handle, Vec3::new(vx, vy, vz), false);
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        physics_table
            .set("set_velocity", set_velocity_fn)
            .map_err(|e| e.to_string())?;

        // physics.get_velocity(id) -> vx, vy, vz
        let pw = physics.clone();
        let sw = scene_world.clone();
        let get_velocity_fn = self
            .lua
            .create_function(move |_, id: String| {
                let pw = pw.borrow();
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        if let Some(v) = pw.get_linvel(rb.handle) {
                            return Ok((v.x, v.y, v.z));
                        }
                    }
                }
                Ok((0.0f32, 0.0f32, 0.0f32))
            })
            .map_err(|e| e.to_string())?;
        physics_table
            .set("get_velocity", get_velocity_fn)
            .map_err(|e| e.to_string())?;

        // physics.set_restitution(id, value)
        let pw = physics.clone();
        let sw = scene_world.clone();
        let set_restitution_fn = self
            .lua
            .create_function(move |_, (id, value): (String, f32)| {
                let mut pw = pw.borrow_mut();
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        pw.set_restitution(rb.handle, value);
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        physics_table
            .set("set_restitution", set_restitution_fn)
            .map_err(|e| e.to_string())?;

        // physics.set_friction(id, value)
        let pw = physics.clone();
        let sw = scene_world.clone();
        let set_friction_fn = self
            .lua
            .create_function(move |_, (id, value): (String, f32)| {
                let mut pw = pw.borrow_mut();
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        pw.set_friction(rb.handle, value);
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        physics_table
            .set("set_friction", set_friction_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("physics", physics_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
