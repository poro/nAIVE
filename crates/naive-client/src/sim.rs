//! Frame steps that need no GPU, shared by the engine and the headless test
//! runner so that both run the same simulation code.
//!
//! Scripts can call back into the physics and entity APIs, so none of these
//! functions keeps engine state borrowed while a script runs.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::components::{CameraMode, Player, Transform};
use crate::physics::{
    CharacterController, Collider as ColliderComp, RigidBody as RigidBodyComp, FIXED_TIMESTEP,
};
use crate::scripting::{
    Script, ScriptRuntime, SharedEventBus, SharedInputState, SharedPhysicsWorld, SharedSceneWorld,
};

/// Move every player-controlled character for a frame that lasted `dt`
/// seconds: mouse look, WASD movement, jumping and gravity, resolved against
/// the world by the character controller.
pub fn update_fps_controller(
    input: &SharedInputState,
    scene_world: &SharedSceneWorld,
    physics_world: &SharedPhysicsWorld,
    dt: f32,
) {
    let input = input.borrow();
    let scene_world = scene_world.borrow_mut();
    let mut physics_world = physics_world.borrow_mut();

    // Collect player entity data
    let mut player_updates: Vec<(hecs::Entity, glam::Vec3, f32, f32, rapier3d::prelude::RigidBodyHandle, rapier3d::prelude::ColliderHandle, f32, f32, f32, bool, glam::Vec3)> = Vec::new();

    for (entity, (player, cc, rb, col)) in scene_world
        .world
        .query::<(&Player, &CharacterController, &RigidBodyComp, &ColliderComp)>()
        .iter()
    {
        // Mouse look
        let mouse_delta = input.mouse_delta();
        let sensitivity = 0.002;
        let new_yaw = player.yaw - mouse_delta.x * sensitivity;

        // Get pitch limits from CameraMode if available
        let (pitch_min, pitch_max) = if let Ok(camera_mode) = scene_world.world.get::<&CameraMode>(entity) {
            match &*camera_mode {
                CameraMode::ThirdPerson { pitch_min, pitch_max, .. } => (*pitch_min, *pitch_max),
                CameraMode::FirstPerson => (-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01),
            }
        } else {
            (-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01)
        };
        let new_pitch = (player.pitch - mouse_delta.y * sensitivity).clamp(pitch_min, pitch_max);

        // Movement
        let move_input = input.axis_2d("move_forward", "move_backward", "move_left", "move_right");
        let speed = if input.pressed("sprint") {
            cc.move_speed * cc.sprint_multiplier
        } else {
            cc.move_speed
        };

        // Calculate movement direction relative to yaw
        let forward = glam::Vec3::new(-new_yaw.sin(), 0.0, -new_yaw.cos());
        let right = glam::Vec3::new(-forward.z, 0.0, forward.x);
        let move_dir = (forward * move_input.y + right * move_input.x).normalize_or_zero();
        let mut desired = move_dir * speed * dt;

        // Vertical velocity for gravity + jump
        let mut vel_y = cc.velocity.y;
        if cc.grounded {
            vel_y = 0.0;
            if input.just_pressed("jump") {
                vel_y = cc.jump_impulse;
            }
        }
        vel_y += physics_world.gravity.y * dt;
        desired.y = vel_y * dt;

        player_updates.push((
            entity,
            desired,
            new_yaw,
            new_pitch,
            rb.handle,
            col.handle,
            vel_y,
            player.height,
            player.radius,
            cc.grounded,
            cc.velocity,
        ));
    }

    // Apply updates
    for (entity, desired, new_yaw, new_pitch, rb_handle, col_handle, vel_y, _height, _radius, _was_grounded, _old_vel) in player_updates {
        let (_effective, grounded) = physics_world.move_character(rb_handle, col_handle, desired, dt);

        // Update ECS components
        if let Ok(mut player) = scene_world.world.get::<&mut Player>(entity) {
            player.yaw = new_yaw;
            player.pitch = new_pitch;
        }
        if let Ok(mut cc) = scene_world.world.get::<&mut CharacterController>(entity) {
            cc.grounded = grounded;
            cc.velocity.y = if grounded && vel_y < 0.0 { 0.0 } else { vel_y };
        }

        // The physics body position is already updated by move_character
        // Sync it back to the transform
        if let Some(body) = physics_world.rigid_body_set.get(rb_handle) {
            let pos = body.position().translation;
            if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                transform.position = glam::Vec3::new(pos.x, pos.y, pos.z);
                transform.rotation = glam::Quat::from_rotation_y(new_yaw);
                transform.dirty = true;
            }
        }
    }
}

/// Advance physics for a frame that lasted `frame_dt` seconds, in fixed steps.
/// Before each step, `fixed_update` is called on every scripted entity; after
/// it, transforms are synced back from the physics bodies. Returns the number
/// of steps run, which is zero on a frame shorter than a step.
///
/// The engine loop and the headless test runner both go through here, so they
/// step physics the same way. No engine state stays borrowed while scripts
/// run, because `fixed_update` may call back into the physics and entity APIs.
pub fn run_fixed_steps(
    physics_world: &SharedPhysicsWorld,
    scene_world: &SharedSceneWorld,
    script_runtime: Option<&ScriptRuntime>,
    frame_dt: f32,
) -> u32 {
    let steps = physics_world.borrow_mut().begin_frame(frame_dt);

    let scripted: Vec<hecs::Entity> = if steps > 0 && script_runtime.is_some() {
        let sw = scene_world.borrow();
        let mut query = sw.world.query::<&Script>();
        query.iter().map(|(e, _)| e).collect()
    } else {
        Vec::new()
    };

    let sync_transforms = || {
        let pw = physics_world.borrow();
        let mut sw = scene_world.borrow_mut();
        pw.sync_to_ecs(&mut sw.world);
    };

    for _ in 0..steps {
        if let Some(runtime) = script_runtime {
            for &entity in &scripted {
                runtime.call_fixed_update(entity, FIXED_TIMESTEP);
            }
        }
        physics_world.borrow_mut().step_fixed();
        sync_transforms();
    }
    if steps == 0 {
        // Transforms stay pinned to their bodies even when no step ran.
        sync_transforms();
    }

    steps
}

/// Call `on_collision` on both scripts for each contact that began this frame.
pub fn dispatch_collision_events(
    scene_world: &SharedSceneWorld,
    physics_world: &SharedPhysicsWorld,
    script_runtime: &ScriptRuntime,
) {
    // Collect the contacts first and release both borrows: `on_collision` may
    // call physics functions that need the physics world mutably.
    let contacts: Vec<(hecs::Entity, hecs::Entity, String, String)> = {
        let sw = scene_world.borrow();
        let pw = physics_world.borrow();
        // Build reverse lookup: entity -> string id
        let entity_to_id: HashMap<hecs::Entity, &str> = sw
            .entity_registry
            .iter()
            .map(|(id, &e)| (e, id.as_str()))
            .collect();
        let id_of = |entity: &hecs::Entity| {
            entity_to_id.get(entity).copied().unwrap_or("unknown").to_string()
        };
        pw.collision_events
            .iter()
            .map(|event| (event.entity_a, event.entity_b, id_of(&event.entity_a), id_of(&event.entity_b)))
            .collect()
    };

    for (entity_a, entity_b, id_a, id_b) in contacts {
        // Dispatch both directions
        script_runtime.call_on_collision(entity_a, &id_b);
        script_runtime.call_on_collision(entity_b, &id_a);
    }
}

/// Call `update(dt)` on every scripted entity.
pub fn update_scripts(scene_world: &SharedSceneWorld, script_runtime: &ScriptRuntime, dt: f32) {
    let scripted: Vec<hecs::Entity> = {
        let sw = scene_world.borrow();
        let mut query = sw.world.query::<&Script>();
        query.iter().map(|(e, _)| e).collect()
    };
    for entity in scripted {
        script_runtime.call_update(entity, dt);
    }
}

/// Advance the event bus by `dt` and hand this frame's events to the Lua
/// listeners registered with `events.on`.
pub fn dispatch_lua_events(
    event_bus: &SharedEventBus,
    listeners: &RefCell<HashMap<String, Vec<mlua::RegistryKey>>>,
    script_runtime: Option<&ScriptRuntime>,
    dt: f32,
) {
    event_bus.borrow_mut().tick(dt as f64);
    let flushed_events = event_bus.borrow_mut().flush();
    if let Some(script_runtime) = script_runtime {
        for event in &flushed_events {
            let listener_keys: Vec<_> = listeners.borrow()
                .get(&event.event_type)
                .map(|keys| keys.iter().map(|k| script_runtime.lua.registry_value::<mlua::Function>(k)).collect())
                .unwrap_or_default();
            for func_result in listener_keys {
                if let Ok(func) = func_result {
                    let lua = &script_runtime.lua;
                    if let Ok(tbl) = lua.create_table() {
                        let _ = tbl.set("type", event.event_type.clone());
                        if let Ok(data_tbl) = lua.create_table() {
                            for (k, v) in &event.data {
                                match v {
                                    serde_json::Value::Number(n) => {
                                        if let Some(f) = n.as_f64() {
                                            let _ = data_tbl.set(k.as_str(), f);
                                        }
                                    }
                                    serde_json::Value::String(s) => {
                                        let _ = data_tbl.set(k.as_str(), s.as_str());
                                    }
                                    serde_json::Value::Bool(b) => {
                                        let _ = data_tbl.set(k.as_str(), *b);
                                    }
                                    _ => {}
                                }
                            }
                            let _ = tbl.set("data", data_tbl);
                        }
                        if let Err(e) = func.call::<()>(tbl) {
                            tracing::error!("Lua event listener error: {}", e);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::rc::Rc;

    use glam::{Quat, Vec3};
    use mlua::prelude::*;

    use crate::physics::{PhysicsBodyType, PhysicsShape, PhysicsWorld};
    use crate::world::SceneWorld;

    #[test]
    fn on_collision_can_call_back_into_physics() {
        let scene_world: SharedSceneWorld = Rc::new(RefCell::new(SceneWorld::new()));
        let physics_world: SharedPhysicsWorld =
            Rc::new(RefCell::new(PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0))));

        // A ball half a metre above a floor, both known to the scene by id.
        let (ball, ball_body) = {
            let mut sw = scene_world.borrow_mut();
            let mut pw = physics_world.borrow_mut();

            let floor = sw.world.spawn(());
            pw.add_static_body(
                floor,
                Vec3::new(0.0, -0.5, 0.0),
                Quat::IDENTITY,
                PhysicsShape::Box {
                    half_extents: Vec3::new(10.0, 0.5, 10.0),
                },
                false,
                0.0,
                0.5,
            );

            let ball = sw.world.spawn(());
            let (handle, _) = pw.add_dynamic_body(
                ball,
                Vec3::new(0.0, 1.0, 0.0),
                Quat::IDENTITY,
                PhysicsShape::Sphere { radius: 0.5 },
                1.0,
                0.0,
                0.5,
                false,
            );
            let body = RigidBodyComp {
                handle,
                body_type: PhysicsBodyType::Dynamic,
            };
            sw.world.insert_one(ball, body).unwrap();

            sw.entity_registry.insert("floor".to_string(), floor);
            sw.entity_registry.insert("ball".to_string(), ball);
            (ball, handle)
        };

        // The ball's script answers the contact with a physics call that needs
        // the physics world mutably.
        let dir = std::env::temp_dir().join("naive_test_sim_on_collision");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ball.lua"),
            r#"
            hits = 0
            function on_collision(other_id)
                hits = hits + 1
                physics.set_velocity(_entity_string_id, 0, 5, 0)
            end
        "#,
        )
        .unwrap();

        let mut runtime = ScriptRuntime::new();
        runtime.register_api().unwrap();
        runtime
            .register_physics_api(physics_world.clone(), scene_world.clone())
            .unwrap();
        runtime.load_script(ball, &dir, Path::new("ball.lua")).unwrap();
        runtime.set_entity_string_id(ball, "ball").unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        // Run frames until the ball lands, dispatching contacts after each one
        // as the engine does.
        let mut velocity_after_hit = None;
        for _ in 0..120 {
            run_fixed_steps(&physics_world, &scene_world, Some(&runtime), 1.0 / 60.0);
            let landed = !physics_world.borrow().collision_events.is_empty();
            dispatch_collision_events(&scene_world, &physics_world, &runtime);
            if landed {
                let pw = physics_world.borrow();
                velocity_after_hit = Some(pw.rigid_body_set[ball_body].linvel().y);
                break;
            }
        }

        let env: LuaTable = runtime
            .lua
            .registry_value(runtime.entity_envs.get(&ball).unwrap())
            .unwrap();
        let hits: i32 = env.get("hits").unwrap();
        assert_eq!(hits, 1, "on_collision should have run once");
        assert_eq!(
            velocity_after_hit,
            Some(5.0),
            "the script's set_velocity call should have taken effect"
        );
    }
}
