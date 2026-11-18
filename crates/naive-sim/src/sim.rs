//! Frame steps that need no GPU, shared by the engine and the headless test
//! runner so that both run the same simulation code.
//!
//! Scripts can call back into the physics and entity APIs, so none of these
//! functions keeps engine state borrowed while a script runs.
//!
//! [`frame`] runs them in the one order both hosts share.

pub mod frame;

use std::cell::RefCell;
use std::collections::HashMap;

use crate::components::{CameraMode, CollisionDamage, Health, Player, Projectile, Transform};
use crate::physics::{
    CharacterController, Collider as ColliderComp, RigidBody as RigidBodyComp, FIXED_TIMESTEP,
};
use crate::scripting::{
    LimitExceeded, Script, ScriptRuntime, SharedEntityCommandQueue, SharedEventBus,
    SharedInputState, SharedPhysicsWorld, SharedSceneWorld,
};

/// What `update_fps_controller` works out for one player before moving it.
struct PlayerMove {
    entity: hecs::Entity,
    desired: glam::Vec3,
    yaw: f32,
    pitch: f32,
    body: rapier3d::prelude::RigidBodyHandle,
    collider: rapier3d::prelude::ColliderHandle,
    vel_y: f32,
}

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
    let mut player_updates: Vec<PlayerMove> = Vec::new();

    for (entity, (player, cc, rb, col)) in scene_world
        .world
        .query::<(
            hecs::Entity,
            (&Player, &CharacterController, &RigidBodyComp, &ColliderComp),
        )>()
        .iter()
    {
        // Mouse look
        let mouse_delta = input.mouse_delta();
        let sensitivity = 0.002;
        let new_yaw = player.yaw - mouse_delta.x * sensitivity;

        // Get pitch limits from CameraMode if available
        let (pitch_min, pitch_max) =
            if let Ok(camera_mode) = scene_world.world.get::<&CameraMode>(entity) {
                match &*camera_mode {
                    CameraMode::ThirdPerson {
                        pitch_min,
                        pitch_max,
                        ..
                    } => (*pitch_min, *pitch_max),
                    CameraMode::FirstPerson => (
                        -std::f32::consts::FRAC_PI_2 + 0.01,
                        std::f32::consts::FRAC_PI_2 - 0.01,
                    ),
                }
            } else {
                (
                    -std::f32::consts::FRAC_PI_2 + 0.01,
                    std::f32::consts::FRAC_PI_2 - 0.01,
                )
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

        player_updates.push(PlayerMove {
            entity,
            desired,
            yaw: new_yaw,
            pitch: new_pitch,
            body: rb.handle,
            collider: col.handle,
            vel_y,
        });
    }

    // Apply updates
    for PlayerMove {
        entity,
        desired,
        yaw: new_yaw,
        pitch: new_pitch,
        body: rb_handle,
        collider: col_handle,
        vel_y,
    } in player_updates
    {
        let (_effective, grounded) =
            physics_world.move_character(rb_handle, col_handle, desired, dt);

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
        let mut query = sw.world.query::<(hecs::Entity, &Script)>();
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

/// Deliver physics hooks through the same path in native and headless runs.
pub fn dispatch_physics_events(
    scene_world: &SharedSceneWorld,
    physics_world: &SharedPhysicsWorld,
    script_runtime: &ScriptRuntime,
) {
    dispatch_collision_events(scene_world, physics_world, script_runtime);
    dispatch_trigger_events(scene_world, physics_world, script_runtime);
}

/// Deliver trigger enter/exit hooks once, without holding engine borrows.
/// Both sides hear about an overlap: the sensor's script gets the other
/// entity's ID, and the other entity's script gets the sensor's (so a player
/// script can react to `on_trigger_enter("portal")`). Two overlapping sensors
/// each receive exactly one call per change, from their own sensor events.
/// When a sensor is removed (destroyed or dropped by a hot reload), entities
/// inside it get their exit with the sensor's last ID.
pub fn dispatch_trigger_events(
    scene_world: &SharedSceneWorld,
    physics_world: &SharedPhysicsWorld,
    script_runtime: &ScriptRuntime,
) {
    dispatch_trigger_events_with_retired_ids(
        scene_world,
        physics_world,
        script_runtime,
        &HashMap::new(),
    );
}

/// Hot reload removes entities before hooks can run. Retain their previous
/// names only for exits; live handles always resolve through the current world.
#[doc(hidden)]
pub fn dispatch_trigger_events_with_retired_ids(
    scene_world: &SharedSceneWorld,
    physics_world: &SharedPhysicsWorld,
    script_runtime: &ScriptRuntime,
    retired_ids: &HashMap<hecs::Entity, String>,
) {
    let events = std::mem::take(&mut physics_world.borrow_mut().trigger_events);
    if events.is_empty() {
        return;
    }
    // A sensor already receives its own directed event for every overlap.
    let sensors: std::collections::HashSet<hecs::Entity> = {
        let physics = physics_world.borrow();
        physics
            .collider_set
            .iter()
            .filter(|(_, collider)| collider.is_sensor())
            .filter_map(|(handle, _)| physics.collider_to_entity.get(&handle).copied())
            .collect()
    };
    let callbacks: Vec<_> = {
        let sw = scene_world.borrow();
        let entity_to_id: HashMap<_, _> = sw
            .entity_registry
            .iter()
            .map(|(id, &entity)| (entity, id.as_str()))
            .collect();
        events
            .into_iter()
            .filter_map(|event| {
                // A stale event must never address a replacement entity that
                // happens to reuse its string ID after destruction or reload.
                // Removed entities keep their previous names, for exits only.
                let name = |entity: hecs::Entity| {
                    entity_to_id.get(&entity).copied().or_else(|| {
                        (!event.entered)
                            .then(|| retired_ids.get(&entity).map(String::as_str))
                            .flatten()
                    })
                };
                let sensor_alive = sw.world.contains(event.trigger_entity);
                let sensor_call = sensor_alive
                    .then(|| name(event.other_entity))
                    .flatten()
                    .map(|other_id| (event.trigger_entity, other_id.to_string()));
                let other_call = (!sensors.contains(&event.other_entity)
                    && sw.world.contains(event.other_entity))
                .then(|| name(event.trigger_entity))
                .flatten()
                .map(|sensor_id| (event.other_entity, sensor_id.to_string()));
                (sensor_call.is_some() || other_call.is_some()).then_some((
                    sensor_call,
                    other_call,
                    event.entered,
                ))
            })
            .collect()
    };
    for (sensor_call, other_call, entered) in callbacks {
        for (entity, id) in [sensor_call, other_call].into_iter().flatten() {
            if entered {
                script_runtime.call_on_trigger_enter(entity, &id);
            } else {
                script_runtime.call_on_trigger_exit(entity, &id);
            }
        }
    }
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
            entity_to_id
                .get(entity)
                .copied()
                .unwrap_or("unknown")
                .to_string()
        };
        pw.collision_events
            .iter()
            .map(|event| {
                (
                    event.entity_a,
                    event.entity_b,
                    id_of(&event.entity_a),
                    id_of(&event.entity_b),
                )
            })
            .collect()
    };

    for (entity_a, entity_b, id_a, id_b) in contacts {
        // Dispatch both directions
        script_runtime.call_on_collision(entity_a, &id_b);
        script_runtime.call_on_collision(entity_b, &id_a);
    }
}

/// Apply collision damage for the contacts that began this frame. An entity
/// with `CollisionDamage` hurts whatever it touches that has `Health`, the
/// target's `on_damage` runs, and a `destroy_on_hit` damager is queued for
/// destruction.
pub fn process_collision_damage(
    scene_world: &SharedSceneWorld,
    physics_world: &SharedPhysicsWorld,
    script_runtime: &ScriptRuntime,
    entity_commands: &SharedEntityCommandQueue,
) {
    // Gather the contact pairs and ids first, so that nothing stays borrowed
    // while `on_damage` runs: it may call back into the entity and physics APIs.
    let (pairs, entity_to_id) = {
        let sw = scene_world.borrow();
        let pw = physics_world.borrow();
        let entity_to_id: HashMap<hecs::Entity, String> = sw
            .entity_registry
            .iter()
            .map(|(id, &e)| (e, id.clone()))
            .collect();
        let mut pairs = Vec::new();
        for event in pw.collision_events.iter().filter(|event| event.started) {
            // Check both directions
            pairs.push((event.entity_a, event.entity_b));
            pairs.push((event.entity_b, event.entity_a));
        }
        (pairs, entity_to_id)
    };

    let mut destroys: Vec<String> = Vec::new();
    for (dmg_entity, target_entity) in pairs {
        let hit = {
            let sw = scene_world.borrow();
            apply_collision_damage(&sw.world, dmg_entity, target_entity, &entity_to_id)
        };
        let Some((damage, destroy_on_hit)) = hit else {
            continue;
        };

        // Call on_damage on the target
        let source_id = entity_to_id.get(&dmg_entity).cloned().unwrap_or_default();
        script_runtime.call_on_damage(target_entity, damage, source_id);

        // Queue destruction of damage entity if destroy_on_hit
        if destroy_on_hit {
            if let Some(id) = entity_to_id.get(&dmg_entity) {
                if !destroys.contains(id) {
                    destroys.push(id.clone());
                }
            }
        }
    }

    entity_commands.borrow_mut().destroys.extend(destroys);
}

/// Take the damage `dmg_entity` deals off `target_entity`'s health, if the
/// pair qualifies. Returns the damage dealt and whether the damager destroys
/// itself on a hit.
fn apply_collision_damage(
    world: &hecs::World,
    dmg_entity: hecs::Entity,
    target_entity: hecs::Entity,
    entity_to_id: &HashMap<hecs::Entity, String>,
) -> Option<(f32, bool)> {
    let (damage, destroy_on_hit) = {
        let cd = world.get::<&CollisionDamage>(dmg_entity).ok()?;
        (cd.damage, cd.destroy_on_hit)
    };

    // Owner skip: a projectile does not hurt the entity that fired it.
    if let Ok(proj) = world.get::<&Projectile>(dmg_entity) {
        let target_id = entity_to_id
            .get(&target_entity)
            .map(|s| s.as_str())
            .unwrap_or("");
        if target_id == proj.owner_id {
            return None;
        }
    }

    // Skip if target has no health
    let mut health = world.get::<&mut Health>(target_entity).ok()?;
    health.current = (health.current - damage).max(0.0);
    Some((damage, destroy_on_hit))
}

/// Age every projectile by `dt` and queue the expired ones for destruction.
pub fn update_projectiles(
    scene_world: &SharedSceneWorld,
    entity_commands: &SharedEntityCommandQueue,
    dt: f32,
) {
    let scene_world = scene_world.borrow();

    let mut expired = Vec::new();
    for (entity, projectile) in scene_world
        .world
        .query::<(hecs::Entity, &mut Projectile)>()
        .iter()
    {
        projectile.age += dt;
        if projectile.age >= projectile.lifetime {
            expired.push(entity);
        }
    }
    if expired.is_empty() {
        return;
    }

    // Build entity->id lookup for expired projectiles
    let entity_to_id: HashMap<hecs::Entity, String> = scene_world
        .entity_registry
        .iter()
        .map(|(id, &e)| (e, id.clone()))
        .collect();

    let mut commands = entity_commands.borrow_mut();
    for entity in expired {
        if let Some(id) = entity_to_id.get(&entity) {
            commands.destroys.push(id.clone());
        }
    }
}

/// Turn what swarms reported into gameplay: members' attacks damage the
/// entity they target (its `on_damage` runs, with the swarm's id as the
/// source), and `swarm.kills`, `swarm.hit` and `swarm.cleared` events go out.
pub fn process_swarm_reports(
    swarm_bus: &crate::swarm_api::SharedSwarmBus,
    scene_world: &SharedSceneWorld,
    script_runtime: &ScriptRuntime,
    event_bus: &SharedEventBus,
) {
    use serde_json::Value;
    let reports = swarm_bus.borrow_mut().take_reports();
    for report in reports {
        let swarm = Value::String(report.swarm.clone());
        if report.kills > 0 {
            let data = HashMap::from([
                ("swarm".to_string(), swarm.clone()),
                ("count".to_string(), Value::from(report.kills)),
                ("alive".to_string(), Value::from(report.alive)),
            ]);
            event_bus.borrow_mut().emit("swarm.kills", data);
        }
        if report.hits > 0 {
            let target = report.target.as_ref().and_then(|id| {
                let sw = scene_world.borrow();
                let entity = *sw.entity_registry.get(id)?;
                let mut health = sw.world.get::<&mut Health>(entity).ok()?;
                if health.dead {
                    return None;
                }
                health.current = (health.current - report.damage).max(0.0);
                Some(entity)
            });
            // Nothing is borrowed while `on_damage` runs.
            if let Some(entity) = target {
                script_runtime.call_on_damage(entity, report.damage, report.swarm.clone());
            }
            let data = HashMap::from([
                ("swarm".to_string(), swarm.clone()),
                (
                    "target".to_string(),
                    report
                        .target
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                ("hits".to_string(), Value::from(report.hits)),
                ("damage".to_string(), Value::from(f64::from(report.damage))),
            ]);
            event_bus.borrow_mut().emit("swarm.hit", data);
        }
        if report.cleared {
            event_bus.borrow_mut().emit(
                "swarm.cleared",
                HashMap::from([("swarm".to_string(), swarm)]),
            );
        }
    }
}

/// Mark entities whose health reached zero as dead and call `on_death` on each, once.
pub fn process_health(scene_world: &SharedSceneWorld, script_runtime: &ScriptRuntime) {
    // Collect first: `on_death` may call back into the entity API.
    let newly_dead: Vec<hecs::Entity> = {
        let sw = scene_world.borrow();
        let mut newly_dead = Vec::new();
        for (entity, health) in sw.world.query::<(hecs::Entity, &mut Health)>().iter() {
            if health.current <= 0.0 && !health.dead {
                health.dead = true;
                newly_dead.push(entity);
            }
        }
        newly_dead
    };

    for entity in newly_dead {
        script_runtime.call_on_death(entity);
    }
}

/// Destroy the entities queued for destruction: run `on_destroy`, remove the
/// physics body, then remove the entity.
pub fn process_destroys(
    scene_world: &SharedSceneWorld,
    physics_world: Option<&SharedPhysicsWorld>,
    mut script_runtime: Option<&mut ScriptRuntime>,
    entity_commands: &SharedEntityCommandQueue,
) {
    let destroys: Vec<String> = std::mem::take(&mut entity_commands.borrow_mut().destroys);
    for id in destroys {
        destroy_entity(
            scene_world,
            physics_world,
            script_runtime.as_deref_mut(),
            &id,
        );
    }
}

/// One destruction path for Lua, MCP and scene transitions. User callbacks run
/// before taking any mutable world/physics borrow, while the entity still exists.
pub fn destroy_entity(
    scene_world: &SharedSceneWorld,
    physics_world: Option<&SharedPhysicsWorld>,
    mut script_runtime: Option<&mut ScriptRuntime>,
    id: &str,
) -> bool {
    let Some(entity) = scene_world.borrow().entity_registry.get(id).copied() else {
        return false;
    };
    if let Some(runtime) = script_runtime.as_deref_mut() {
        runtime.remove_entity(entity);
    }
    let body = scene_world
        .borrow()
        .world
        .get::<&RigidBodyComp>(entity)
        .ok()
        .map(|rb| rb.handle);
    if let (Some(handle), Some(physics)) = (body, physics_world) {
        physics.borrow_mut().remove_body(handle);
        // The removed entity's name remains available to surviving sensors,
        // while its own script has already completed teardown.
        if let Some(runtime) = script_runtime {
            dispatch_trigger_events(scene_world, physics, runtime);
        }
    }
    crate::world::destroy_runtime_entity(&mut scene_world.borrow_mut(), id)
}

/// The per-scene state [`clear_scene`] resets.
pub struct SceneTeardown<'a> {
    pub scene_world: &'a SharedSceneWorld,
    pub physics_world: Option<&'a SharedPhysicsWorld>,
    pub script_runtime: Option<&'a mut ScriptRuntime>,
    pub entity_commands: &'a SharedEntityCommandQueue,
    pub event_bus: &'a SharedEventBus,
    pub pool_manager: &'a RefCell<crate::world::EntityPoolManager>,
    pub particle_system: &'a RefCell<dyn AsMut<crate::particles::ParticleSystem>>,
    pub lua_event_listeners: &'a RefCell<HashMap<String, Vec<mlua::RegistryKey>>>,
    pub next_lua_listener_id: &'a RefCell<u64>,
    pub lua_listener_id_map: &'a RefCell<HashMap<u64, (String, usize)>>,
    pub camera_shake: &'a RefCell<crate::presentation::CameraShakeState>,
    pub ui_frame: &'a RefCell<crate::presentation::UiFrame>,
    pub swarm_bus: &'a crate::swarm_api::SharedSwarmBus,
}

/// Empty the running scene before a game script's `scene.load` (or the
/// editor's undo) builds the next one, the same way in the engine and the
/// headless runner. Every entity is destroyed through the usual path, so its
/// `on_destroy` runs; then per-scene state is cleared in place, so the `Rc`
/// handles Lua closures captured stay valid and no old entity handle can
/// name a new entity. Physics restarts with `gravity` and keeps its cooked
/// colliders. The Lua VM and its globals (such as the shared `game` table),
/// the event log and input carry over to the next scene; GPU or CPU swarm
/// instances are the host's to clear.
pub fn clear_scene(teardown: SceneTeardown<'_>, gravity: glam::Vec3) {
    let SceneTeardown {
        scene_world,
        physics_world,
        mut script_runtime,
        entity_commands,
        event_bus,
        pool_manager,
        particle_system,
        lua_event_listeners,
        next_lua_listener_id,
        lua_listener_id_map,
        camera_shake,
        ui_frame,
        swarm_bus,
    } = teardown;
    // Never hold world borrows while invoking user callbacks.
    let ids: Vec<_> = scene_world
        .borrow()
        .entity_registry
        .keys()
        .cloned()
        .collect();
    for id in ids {
        destroy_entity(
            scene_world,
            physics_world,
            script_runtime.as_deref_mut(),
            &id,
        );
    }
    if let Some(runtime) = script_runtime {
        let stale: Vec<_> = runtime.entity_envs.keys().copied().collect();
        for entity in stale {
            runtime.forget_entity(entity);
        }
    }
    entity_commands.borrow_mut().clear();
    // Events from the previous scene, including teardown hooks, must not
    // reach subscriptions registered by the replacement scene.
    event_bus.borrow_mut().discard_pending();
    {
        let mut world = scene_world.borrow_mut();
        world.world.clear();
        world.entity_registry.clear();
        world.current_scene = None;
    }
    if let Some(physics) = physics_world {
        physics.borrow_mut().reset(gravity);
    }
    *pool_manager.borrow_mut() = crate::world::EntityPoolManager::new();
    *particle_system.borrow_mut().as_mut() = crate::particles::ParticleSystem::new();
    lua_event_listeners.borrow_mut().clear();
    *next_lua_listener_id.borrow_mut() = 0;
    lua_listener_id_map.borrow_mut().clear();
    *camera_shake.borrow_mut() = crate::presentation::CameraShakeState::new();
    *ui_frame.borrow_mut() = Default::default();
    *swarm_bus.borrow_mut() = crate::swarm_api::SwarmBus::new();
}

/// Initialize newly attached scripts after construction has released its world
/// borrows. Failed initialization is reported once and remains visible to tests.
pub fn initialize_pending_scripts(
    scene_world: &SharedSceneWorld,
    runtime: &mut ScriptRuntime,
    project_root: &std::path::Path,
) -> Result<(), String> {
    let pending: Vec<_> = {
        let world = scene_world.borrow();
        let pending = world
            .world
            .query::<(hecs::Entity, &Script)>()
            .iter()
            .filter(|(_, script)| !script.initialized)
            .map(|(entity, script)| {
                (
                    entity,
                    script.source.clone(),
                    world
                        .entity_registry
                        .iter()
                        .find_map(|(id, &handle)| (entity == handle).then(|| id.clone())),
                )
            })
            .collect();
        pending
    };
    let mut failures = Vec::new();
    for (entity, source, id) in pending {
        // Mark first: an init callback that queues more work cannot initialize
        // the same component twice. A source change/reload explicitly resets it.
        if let Ok(mut script) = scene_world.borrow().world.get::<&mut Script>(entity) {
            script.initialized = true;
        }
        if let Err(error) = runtime.load_script(entity, project_root, &source) {
            let error = format!(
                "Failed to load script for '{}': {error}",
                id.as_deref().unwrap_or("unknown")
            );
            runtime.record_error(error.clone());
            failures.push(error);
            continue;
        }
        if let Some(id) = id {
            if let Err(error) = runtime.set_entity_string_id(entity, &id) {
                runtime.record_error(error.clone());
                runtime.forget_entity(entity);
                failures.push(error);
                continue;
            }
        }
        if let Err(error) = runtime.call_init_checked(entity) {
            runtime.forget_entity(entity);
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// Apply the same pool bookkeeping after native and headless construction.
pub fn finish_pool_spawn(
    scene_world: &SharedSceneWorld,
    pool_manager: &crate::scripting::SharedEntityPoolManager,
    command: &crate::world::SpawnCommand,
) {
    let Some((pool, _counter)) = command
        .id
        .strip_prefix("_pool_")
        .and_then(|id| id.rsplit_once('_'))
    else {
        return;
    };
    let mut pools = pool_manager.borrow_mut();
    if !pools.has_pool(pool) {
        return;
    }
    let mut world = scene_world.borrow_mut();
    let Some(&entity) = world.entity_registry.get(&command.id) else {
        return;
    };
    let prewarm = command.position == [0.0, -1000.0, 0.0];
    let _ = world.world.insert_one(
        entity,
        crate::components::Pooled {
            pool_name: pool.to_string(),
            active: !prewarm,
        },
    );
    if prewarm {
        let _ = world.world.insert_one(entity, crate::components::Hidden);
    }
    pools.register_entity(pool, &command.id);
    if prewarm {
        pools.release(pool, &command.id);
    }
}

/// Call `update(dt)` on every scripted entity.
pub fn update_scripts(scene_world: &SharedSceneWorld, script_runtime: &ScriptRuntime, dt: f32) {
    let scripted: Vec<hecs::Entity> = {
        let sw = scene_world.borrow();
        let mut query = sw.world.query::<(hecs::Entity, &Script)>();
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
            let listener_keys: Vec<_> = listeners
                .borrow()
                .get(&event.event_type)
                .map(|keys| {
                    keys.iter()
                        .map(|k| script_runtime.lua.registry_value::<mlua::Function>(k))
                        .collect()
                })
                .unwrap_or_default();
            for func_result in listener_keys {
                let result = (|| -> mlua::Result<()> {
                    let func = func_result?;
                    let lua = &script_runtime.lua;
                    let tbl = lua.create_table()?;
                    tbl.set("type", event.event_type.clone())?;
                    let data_tbl = lua.create_table()?;
                    for (key, value) in &event.data {
                        match value {
                            serde_json::Value::Number(number) => {
                                if let Some(value) = number.as_f64() {
                                    data_tbl.set(key.as_str(), value)?;
                                }
                            }
                            serde_json::Value::String(value) => {
                                data_tbl.set(key.as_str(), value.as_str())?
                            }
                            serde_json::Value::Bool(value) => data_tbl.set(key.as_str(), *value)?,
                            _ => {}
                        }
                    }
                    tbl.set("data", data_tbl)?;
                    let result = script_runtime.run_limited(|| func.call::<()>(tbl));
                    if let Some(limit) = script_runtime.limit_exceeded(result.as_ref().err()) {
                        disable_listener(script_runtime, &event.event_type, &func, limit);
                        return Ok(()); // report_limit already retains the failure.
                    }
                    result
                })();
                if let Err(error) = result {
                    let message = format!(
                        "Lua event listener for '{}' failed: {error}",
                        event.event_type
                    );
                    script_runtime.record_error(message.clone());
                    tracing::error!("{message}");
                }
            }
        }
    }
}

/// Remove a listener by live callback identity after it exceeded a limit.
/// It may already have unsubscribed itself while running.
fn disable_listener(
    script_runtime: &ScriptRuntime,
    event_type: &str,
    callback: &mlua::Function,
    limit: LimitExceeded,
) {
    tracing::error!(
        "Event listener for '{}' removed: it {}.",
        event_type,
        script_runtime.describe_limit(limit)
    );
    let limit_name = match limit {
        LimitExceeded::Cpu => "CPU",
        LimitExceeded::Memory => "memory",
    };
    script_runtime.report_limit(format!(
        "Listener removed: '{}' hit the {} limit",
        event_type, limit_name
    ));

    if let Err(error) = script_runtime.remove_event_listener(event_type, callback) {
        let message = format!("Failed to remove event listener for '{event_type}': {error}");
        script_runtime.record_error(message.clone());
        tracing::error!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::rc::Rc;

    use glam::{Quat, Vec3};
    use mlua::prelude::*;

    use crate::events::EventBus;
    use crate::physics::{PhysicsBodyType, PhysicsShape, PhysicsWorld};
    use crate::scripting::ScriptLimits;
    use crate::world::SceneWorld;

    #[test]
    fn pending_scripts_initialize_once_and_destroy_without_borrowing_worlds() {
        let scene: SharedSceneWorld = Rc::new(RefCell::new(SceneWorld::new()));
        let physics: SharedPhysicsWorld = Rc::new(RefCell::new(PhysicsWorld::new(Vec3::ZERO)));
        let bus = Rc::new(RefCell::new(EventBus::new(100)));
        let listeners = Rc::new(RefCell::new(HashMap::new()));
        let ids = Rc::new(RefCell::new(HashMap::new()));
        let commands = Rc::new(RefCell::new(crate::world::EntityCommandQueue::default()));
        let dir = std::env::temp_dir().join(format!(
            "naive_lifecycle_initialize_destroy_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("entity.lua"),
            r#"
            function init()
                counts.init = counts.init + 1
                entity.set_position(_entity_string_id, 1, 2, 3)
                events.on('ping', function()
                    counts.events = counts.events + 1
                    events.on('nested', function() counts.events = counts.events + 100 end)
                end)
            end
            function update() counts.update = counts.update + 1 end
            function on_destroy()
                counts.destroy = counts.destroy + 1
                local x = entity.get_position(_entity_string_id)
                assert(x == 1)
                entity.set_position(_entity_string_id, 4, 5, 6)
                physics.set_velocity(_entity_string_id, 0, 0, 0)
                events.on('ping', function() error('destroy listener escaped') end)
            end
        "#,
        )
        .unwrap();
        let entity = scene.borrow_mut().world.spawn((
            crate::components::Transform::default(),
            Script {
                source: "entity.lua".into(),
                initialized: false,
            },
        ));
        scene
            .borrow_mut()
            .entity_registry
            .insert("hero".to_string(), entity);
        let (body, _) = physics.borrow_mut().add_dynamic_body(
            entity,
            Vec3::ZERO,
            Quat::IDENTITY,
            PhysicsShape::Sphere { radius: 0.5 },
            1.0,
            0.0,
            0.5,
            false,
        );
        scene
            .borrow_mut()
            .world
            .insert_one(
                entity,
                RigidBodyComp {
                    handle: body,
                    body_type: PhysicsBodyType::Dynamic,
                },
            )
            .unwrap();
        let mut runtime = ScriptRuntime::new();
        runtime
            .register_entity_api(scene.clone(), physics.clone())
            .unwrap();
        runtime
            .register_physics_api(physics.clone(), scene.clone())
            .unwrap();
        runtime
            .register_event_api(
                bus.clone(),
                listeners.clone(),
                Rc::new(RefCell::new(0)),
                ids.clone(),
            )
            .unwrap();
        runtime
            .lua
            .load("counts = {init=0, update=0, destroy=0, events=0}")
            .exec()
            .unwrap();

        initialize_pending_scripts(&scene, &mut runtime, &dir).unwrap();
        initialize_pending_scripts(&scene, &mut runtime, &dir).unwrap();
        update_scripts(&scene, &runtime, 0.0);
        bus.borrow_mut().emit("ping", HashMap::new());
        dispatch_lua_events(&bus, &listeners, Some(&runtime), 0.0);
        let counts: LuaTable = runtime.lua.globals().get("counts").unwrap();
        assert_eq!(counts.get::<i64>("init").unwrap(), 1);
        assert_eq!(counts.get::<i64>("update").unwrap(), 1);
        assert_eq!(counts.get::<i64>("events").unwrap(), 1);

        commands
            .borrow_mut()
            .destroys
            .extend(["hero".to_string(), "hero".to_string()]);
        process_destroys(&scene, Some(&physics), Some(&mut runtime), &commands);
        assert_eq!(counts.get::<i64>("destroy").unwrap(), 1);
        assert!(!scene.borrow().world.contains(entity));
        assert!(!physics.borrow().rigid_body_set.contains(body));
        assert!(!runtime.entity_envs.contains_key(&entity));
        assert!(!runtime.script_sources.contains_key(&entity));
        assert!(ids.borrow().is_empty());
        assert!(listeners.borrow().values().all(Vec::is_empty));
        for kind in ["ping", "nested"] {
            bus.borrow_mut().emit(kind, HashMap::new());
        }
        dispatch_lua_events(&bus, &listeners, Some(&runtime), 0.0);
        assert_eq!(counts.get::<i64>("events").unwrap(), 1);
        assert!(runtime.take_errors().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_init_is_reported_once_and_drops_subscriptions() {
        let scene: SharedSceneWorld = Rc::new(RefCell::new(SceneWorld::new()));
        let listeners = Rc::new(RefCell::new(HashMap::new()));
        let dir = std::env::temp_dir().join(format!(
            "naive_lifecycle_failed_init_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bad.lua"),
            "function init() events.on('ping', function() end); error('init failed') end",
        )
        .unwrap();
        let entity = scene.borrow_mut().world.spawn((Script {
            source: "bad.lua".into(),
            initialized: false,
        },));
        let mut runtime = ScriptRuntime::new();
        runtime
            .register_event_api(
                Rc::new(RefCell::new(EventBus::new(100))),
                listeners.clone(),
                Rc::new(RefCell::new(0)),
                Rc::new(RefCell::new(HashMap::new())),
            )
            .unwrap();
        assert!(initialize_pending_scripts(&scene, &mut runtime, &dir)
            .unwrap_err()
            .contains("init failed"));
        assert_eq!(runtime.take_errors().len(), 1);
        assert!(!runtime.entity_envs.contains_key(&entity));
        assert!(listeners.borrow().values().all(Vec::is_empty));
        initialize_pending_scripts(&scene, &mut runtime, &dir).unwrap();
        assert!(runtime.take_errors().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runaway_event_listener_is_cut_off_and_removed() {
        let event_bus: SharedEventBus = Rc::new(RefCell::new(EventBus::new(100)));
        let listeners = Rc::new(RefCell::new(HashMap::new()));
        let runtime = ScriptRuntime::with_limits(ScriptLimits {
            instructions_per_call: 200_000,
            memory_bytes: 16 * 1024 * 1024,
            ..ScriptLimits::default()
        });
        runtime
            .register_event_api(
                event_bus.clone(),
                listeners.clone(),
                Rc::new(RefCell::new(0)),
                Rc::new(RefCell::new(HashMap::new())),
            )
            .unwrap();
        runtime
            .lua
            .load(
                r#"
                heard = 0
                events.on("boom", function() while true do end end)
                events.on("boom", function() heard = heard + 1 end)
            "#,
            )
            .exec()
            .unwrap();

        for _ in 0..2 {
            event_bus.borrow_mut().emit("boom", HashMap::new());
            dispatch_lua_events(&event_bus, &listeners, Some(&runtime), 1.0 / 60.0);
        }

        // The runaway listener was cut off once and then removed, so it is
        // reported once. The healthy listener heard both events.
        assert_eq!(runtime.take_limit_reports().len(), 1);
        let heard: i64 = runtime.lua.globals().get("heard").unwrap();
        assert_eq!(heard, 2);
    }

    #[test]
    fn listener_removal_during_a_runaway_callback_does_not_disable_healthy_listeners() {
        for remove in ["runaway", "earlier"] {
            let event_bus: SharedEventBus = Rc::new(RefCell::new(EventBus::new(100)));
            let listeners = Rc::new(RefCell::new(HashMap::new()));
            let runtime = ScriptRuntime::with_limits(ScriptLimits {
                instructions_per_call: 200_000,
                memory_bytes: 16 * 1024 * 1024,
                ..ScriptLimits::default()
            });
            runtime
                .register_event_api(
                    event_bus.clone(),
                    listeners.clone(),
                    Rc::new(RefCell::new(0)),
                    Rc::new(RefCell::new(HashMap::new())),
                )
                .unwrap();
            runtime
                .lua
                .load(format!(
                    r#"
                heard = 0
                runaway_calls = 0
                local earlier = events.on('boom', function() end)
                local runaway
                runaway = events.on('boom', function()
                    runaway_calls = runaway_calls + 1
                    events.off({remove})
                    while true do end
                end)
                events.on('boom', function() heard = heard + 1 end)
            "#
                ))
                .exec()
                .unwrap();
            for _ in 0..2 {
                event_bus.borrow_mut().emit("boom", HashMap::new());
                dispatch_lua_events(&event_bus, &listeners, Some(&runtime), 0.0);
            }
            assert_eq!(runtime.take_limit_reports().len(), 1, "remove {remove}");
            assert_eq!(
                runtime.lua.globals().get::<u32>("runaway_calls").unwrap(),
                1,
                "remove {remove}"
            );
            assert_eq!(
                runtime.lua.globals().get::<u32>("heard").unwrap(),
                2,
                "remove {remove}"
            );
        }
    }

    #[test]
    fn event_payload_allocation_and_invalid_registry_entries_record_errors() {
        let event_bus: SharedEventBus = Rc::new(RefCell::new(EventBus::new(100)));
        let listeners = Rc::new(RefCell::new(HashMap::new()));
        let runtime = ScriptRuntime::new();
        runtime
            .register_event_api(
                event_bus.clone(),
                listeners.clone(),
                Rc::new(RefCell::new(0)),
                Rc::new(RefCell::new(HashMap::new())),
            )
            .unwrap();
        runtime
            .lua
            .load("events.on('boom', function() end)")
            .exec()
            .unwrap();
        runtime.lua.gc_stop();
        let old_limit = runtime
            .lua
            .set_memory_limit(runtime.lua.used_memory() + 1)
            .unwrap();
        let mut payload = HashMap::new();
        payload.insert(
            "large".into(),
            serde_json::Value::String("x".repeat(1024 * 1024)),
        );
        event_bus.borrow_mut().emit("boom", payload);
        dispatch_lua_events(&event_bus, &listeners, Some(&runtime), 0.0);
        runtime.lua.set_memory_limit(old_limit).unwrap();
        runtime.lua.gc_restart();
        assert!(
            !runtime.take_errors().is_empty(),
            "event allocation failure was hidden"
        );

        let invalid = runtime.lua.create_registry_value("not a function").unwrap();
        listeners
            .borrow_mut()
            .insert("invalid".into(), vec![invalid]);
        event_bus.borrow_mut().emit("invalid", HashMap::new());
        dispatch_lua_events(&event_bus, &listeners, Some(&runtime), 0.0);
        assert!(
            !runtime.take_errors().is_empty(),
            "invalid event callback was hidden"
        );
    }

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
        let dir = std::env::temp_dir().join(format!(
            "naive_test_sim_on_collision_{}",
            std::process::id()
        ));
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
        runtime
            .load_script(ball, &dir, Path::new("ball.lua"))
            .unwrap();
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
