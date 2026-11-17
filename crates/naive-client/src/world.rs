mod native_assets;
#[cfg(test)]
mod parity_tests;
mod spawning;

pub(crate) use naive_sim::world::preflight_scene;
pub use naive_sim::world::{
    destroy_runtime_entity, euler_degrees_to_quat, finish_dynamic_spawn, finish_projectile_spawn,
    parse_collider_shape, spawn_entity_headless_with_assets, spawn_entity_with_resolver,
    DynamicSpawnCommand, EntityCommandQueue, EntityPoolManager, MeshCreateCommand, PoolOp,
    ProjectileSpawnCommand, SceneEntityDefinition, SceneWorld, SpawnAssetResolver,
    SpawnColliderSpec, SpawnCommand, TextureSwapCommand, UnsavedRuntimeState,
};
#[cfg(test)]
pub use naive_sim::world::{spawn_all_entities_headless, spawn_entity_headless};
pub(crate) use spawning::preflight_scene_native;
pub use spawning::{
    spawn_dynamic_entity, spawn_entity, spawn_entity_native, spawn_projectile_entity,
    spawn_runtime_entity,
};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::components::*;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::{self, PhysicsWorld};
use crate::scene::{EntityDef, SceneFile};
use crate::splat::SplatCache;

/// Spawn all entities from a parsed scene into the ECS world.
#[allow(clippy::too_many_arguments)]
pub fn spawn_all_entities(
    scene_world: &mut SceneWorld,
    scene: &SceneFile,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    mut physics_world: Option<&mut PhysicsWorld>,
    texture_resources: Option<&crate::mesh::TextureResources>,
    mut texture_cache: Option<&mut crate::texture_cache::TextureCache>,
) {
    if let Err(error) = preflight_scene_native(
        scene,
        device,
        queue,
        project_root,
        mesh_cache,
        material_cache,
        splat_cache,
        texture_resources,
        texture_cache.as_deref_mut(),
        physics_world.as_deref_mut(),
    ) {
        tracing::error!("Scene creation failed: {error}");
        return;
    }
    for entity_def in &scene.entities {
        spawn_entity(
            scene_world,
            entity_def,
            device,
            queue,
            project_root,
            mesh_cache,
            material_cache,
            splat_cache,
            physics_world.as_deref_mut(),
            texture_resources,
            texture_cache.as_deref_mut(),
        );
    }
    scene_world.current_scene = Some(scene.clone());
    tracing::info!(
        "Scene '{}' loaded: {} entities",
        scene.name,
        scene.entities.len()
    );
}

/// Reconcile a scene update: diff old vs new, spawn/despawn/patch entities.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_scene(
    scene_world: &mut SceneWorld,
    new_scene: &SceneFile,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    mut physics_world: Option<&mut PhysicsWorld>,
    texture_resources: Option<&crate::mesh::TextureResources>,
) {
    let old_scene = match &scene_world.current_scene {
        Some(s) => s.clone(),
        None => {
            spawn_all_entities(
                scene_world,
                new_scene,
                device,
                queue,
                project_root,
                mesh_cache,
                material_cache,
                splat_cache,
                physics_world,
                texture_resources,
                None,
            );
            return;
        }
    };

    let old_ids: HashSet<&str> = old_scene.entities.iter().map(|e| e.id.as_str()).collect();
    let new_ids: HashSet<&str> = new_scene.entities.iter().map(|e| e.id.as_str()).collect();

    // 1. Remove entities no longer in the scene (clean up physics bodies too)
    for id in old_ids.difference(&new_ids) {
        if despawn_scene_entity(scene_world, id, physics_world.as_deref_mut()) {
            tracing::info!("Hot-reload: destroyed entity '{}'", id);
        }
    }

    // 2. Spawn new entities (with physics)
    for entity_def in &new_scene.entities {
        if !old_ids.contains(entity_def.id.as_str()) {
            spawn_entity(
                scene_world,
                entity_def,
                device,
                queue,
                project_root,
                mesh_cache,
                material_cache,
                splat_cache,
                physics_world.as_deref_mut(),
                texture_resources,
                None,
            );
            tracing::info!("Hot-reload: spawned entity '{}'", entity_def.id);
        }
    }

    // 3. Patch modified entities (may trigger respawn for structural changes)
    let old_map: HashMap<&str, &EntityDef> = old_scene
        .entities
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();

    let mut respawn_ids: Vec<String> = Vec::new();

    for new_def in &new_scene.entities {
        if let Some(&old_def) = old_map.get(new_def.id.as_str()) {
            if let Some(&entity) = scene_world.entity_registry.get(&new_def.id) {
                let needs_respawn = patch_entity(
                    &mut scene_world.world,
                    entity,
                    old_def,
                    new_def,
                    device,
                    queue,
                    project_root,
                    mesh_cache,
                    material_cache,
                );
                if needs_respawn {
                    respawn_ids.push(new_def.id.clone());
                }
            }
        }
    }

    // 4. Respawn entities that had structural component changes
    for id in &respawn_ids {
        despawn_scene_entity(scene_world, id, physics_world.as_deref_mut());
        if let Some(new_def) = new_scene.entities.iter().find(|e| &e.id == id) {
            spawn_entity(
                scene_world,
                new_def,
                device,
                queue,
                project_root,
                mesh_cache,
                material_cache,
                splat_cache,
                physics_world.as_deref_mut(),
                texture_resources,
                None,
            );

            tracing::info!("Hot-reload: respawned entity '{}' (structural change)", id);
        }
    }

    scene_world.current_scene = Some(new_scene.clone());
}

fn despawn_scene_entity(
    scene_world: &mut SceneWorld,
    id: &str,
    physics_world: Option<&mut PhysicsWorld>,
) -> bool {
    let Some(entity) = scene_world.entity_registry.remove(id) else {
        return false;
    };
    if let Some(physics) = physics_world {
        let handle = scene_world
            .world
            .get::<&physics::RigidBody>(entity)
            .ok()
            .map(|body| body.handle);
        if let Some(handle) = handle {
            physics.remove_body(handle);
        }
    }
    let _ = scene_world.world.despawn(entity);
    true
}

/// Shared with lifecycle callers so script teardown happens before the entity
/// is replaced and before mutable world/physics borrows are held.
pub fn entity_needs_respawn(old_def: &EntityDef, new_def: &EntityDef) -> bool {
    let old = &old_def.components;
    let new = &new_def.components;
    old.mesh_renderer.is_some() != new.mesh_renderer.is_some()
        || old.camera.is_some() != new.camera.is_some()
        || old.point_light.is_some() != new.point_light.is_some()
        || old.script.is_some() != new.script.is_some()
        || definition_changed(&old.rigid_body, &new.rigid_body)
        || definition_changed(&old.collider, &new.collider)
        || definition_changed(&old.character_controller, &new.character_controller)
        || definition_changed(&old.gaussian_splat, &new.gaussian_splat)
        || definition_changed(&old.directional_light, &new.directional_light)
        || definition_changed(&old.health, &new.health)
        || definition_changed(&old.collision_damage, &new.collision_damage)
        || definition_changed(&old.particle_emitter, &new.particle_emitter)
        || definition_changed(&old.animator, &new.animator)
        // Any swarm change rebuilds the swarm, which respawns its members.
        || old.gpu_swarm != new.gpu_swarm
        // Physics transforms and trimesh geometry need their corresponding
        // Rapier bodies rebuilt; an ECS-only patch would move just the picture.
        || ((new.collider.is_some() || new.character_controller.is_some())
            && definition_changed(&old.transform, &new.transform))
        || (new.collider.as_ref().is_some_and(|collider| collider.shape == "trimesh")
            && old.mesh_renderer.as_ref().map(|mesh| &mesh.mesh)
                != new.mesh_renderer.as_ref().map(|mesh| &mesh.mesh))
}

fn definition_changed<T: serde::Serialize>(old: &T, new: &T) -> bool {
    match (serde_norway::to_value(old), serde_norway::to_value(new)) {
        (Ok(old), Ok(new)) => old != new,
        // If a future component cannot be compared, rebuilding is safer than
        // leaving stale runtime state that a later save could resurrect.
        _ => true,
    }
}

/// Patch an existing entity's components in-place.
/// Returns `true` if the entity needs a full destroy+respawn (structural change).
#[allow(clippy::too_many_arguments)]
fn patch_entity(
    world: &mut hecs::World,
    entity: hecs::Entity,
    old_def: &EntityDef,
    new_def: &EntityDef,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
) -> bool {
    // Detect structural changes: components added or removed require a full respawn.
    if entity_needs_respawn(old_def, new_def) {
        return true;
    }

    let _ = world.insert_one(entity, SceneEntityDefinition(new_def.clone()));
    let _ = world.insert_one(entity, Tags(new_def.tags.clone()));

    // --- Fast-path patches for mutable component values ---

    // Patch transform
    if let Ok(mut transform) = world.get::<&mut Transform>(entity) {
        if let Some(t) = &new_def.components.transform {
            transform.position = glam::Vec3::from(t.position);
            transform.rotation = euler_degrees_to_quat(t.rotation);
            transform.scale = glam::Vec3::from(t.scale);
        } else {
            transform.position = glam::Vec3::ZERO;
            transform.rotation = glam::Quat::IDENTITY;
            transform.scale = glam::Vec3::ONE;
        }
        transform.dirty = true;
    }

    // Patch camera
    if let Some(cam) = &new_def.components.camera {
        if let Ok(mut camera) = world.get::<&mut Camera>(entity) {
            camera.fov_degrees = cam.fov;
            camera.near = cam.near;
            camera.far = cam.far;
            camera.role = if cam.role == "main" {
                CameraRole::Main
            } else {
                CameraRole::Other(cam.role.clone())
            };
        }
        let mode = if cam.mode == "third_person" {
            let limits = cam.pitch_limits.unwrap_or([-60.0, 75.0]);
            CameraMode::ThirdPerson {
                distance: cam.distance,
                height_offset: cam.height_offset,
                pitch_min: limits[0].to_radians(),
                pitch_max: limits[1].to_radians(),
            }
        } else {
            CameraMode::FirstPerson
        };
        let _ = world.insert_one(entity, mode);
    }

    // Patch point light
    if let Some(pl) = &new_def.components.point_light {
        if let Ok(mut point_light) = world.get::<&mut PointLight>(entity) {
            point_light.color = glam::Vec3::from(pl.color);
            point_light.intensity = pl.intensity;
            point_light.range = pl.range;
        }
    }

    // Patch mesh renderer (mesh and/or material changes)
    if let (Some(old_mr), Some(new_mr)) = (
        &old_def.components.mesh_renderer,
        &new_def.components.mesh_renderer,
    ) {
        if old_mr.mesh != new_mr.mesh || old_mr.material != new_mr.material {
            let mesh_handle = if old_mr.mesh != new_mr.mesh {
                mesh_cache
                    .get_or_load(device, queue, project_root, &new_mr.mesh, None)
                    .ok()
            } else {
                None
            };
            let material_handle = if old_mr.material != new_mr.material {
                material_cache
                    .get_or_load(device, queue, project_root, &new_mr.material, None, None)
                    .ok()
            } else {
                None
            };
            if let Ok(mut mr) = world.get::<&mut MeshRenderer>(entity) {
                if let Some(mh) = mesh_handle {
                    mr.mesh_handle = mh;
                }
                if let Some(mat) = material_handle {
                    mr.material_handle = mat;
                }
            }
        }
    }

    // Patch script source path
    if let (Some(_old_script), Some(new_script)) =
        (&old_def.components.script, &new_def.components.script)
    {
        if let Ok(mut script) = world.get::<&mut crate::scripting::Script>(entity) {
            let new_path = std::path::PathBuf::from(&new_script.source);
            if script.source != new_path {
                script.source = new_path;
                script.initialized = false;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_replacement_exits_surviving_sensors_with_the_retired_name() {
        let root = std::env::temp_dir().join(format!(
            "naive-trigger-structural-reload-{}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("sensor.lua"),
            r#"
            enters = 0
            exits = {}
            function on_trigger_enter(other)
                enters = enters + 1
                physics.set_velocity(_entity_string_id, 0, 0, 0)
            end
            function on_trigger_exit(other)
                exits[#exits + 1] = other
                physics.set_velocity(_entity_string_id, 0, 0, 0)
            end
        "#,
        )
        .unwrap();
        std::fs::write(
            root.join("scene.yaml"),
            r#"
name: Reload
settings: {gravity: [0, 0, 0]}
entities:
  - id: survivor
    components:
      collider: {shape: sphere, radius: 1, is_trigger: true}
      script: {source: sensor.lua}
  - id: replaced
    components:
      collider: {shape: sphere, radius: 1, is_trigger: true}
      script: {source: sensor.lua}
"#,
        )
        .unwrap();
        let mut runner = crate::test_runner::TestRunner::new(&root);
        runner.load_scene("scene.yaml").unwrap();
        runner.step_frame();
        let (survivor, retired) = {
            let world = runner.scene_world.borrow();
            (
                world.entity_registry["survivor"],
                world.entity_registry["replaced"],
            )
        };
        let survivor_env: mlua::Table = runner
            .script_runtime
            .lua
            .registry_value(&runner.script_runtime.entity_envs[&survivor])
            .unwrap();
        let retired_env: mlua::Table = runner
            .script_runtime
            .lua
            .registry_value(&runner.script_runtime.entity_envs[&retired])
            .unwrap();
        let previous_ids = runner
            .scene_world
            .borrow()
            .entity_registry
            .iter()
            .map(|(id, &entity)| (entity, id.clone()))
            .collect();
        runner.script_runtime.remove_entity(retired);
        {
            let mut world = runner.scene_world.borrow_mut();
            let mut physics = runner.physics_world.borrow_mut();
            let mut replacement = world.current_scene.as_ref().unwrap().entities[1].clone();
            replacement.components.collider.as_mut().unwrap().radius = Some(2.0);
            assert!(despawn_scene_entity(
                &mut world,
                "replaced",
                Some(&mut physics)
            ));
            spawn_entity_headless(&mut world, &replacement, &mut physics);
            assert_ne!(world.entity_registry["replaced"], retired);
        }
        crate::sim::dispatch_trigger_events_with_retired_ids(
            &runner.scene_world,
            &runner.physics_world,
            &runner.script_runtime,
            &previous_ids,
        );
        let exits: mlua::Table = survivor_env.get("exits").unwrap();
        assert_eq!(exits.raw_len(), 1);
        assert_eq!(exits.get::<String>(1).unwrap(), "replaced");
        assert_eq!(
            retired_env.get::<mlua::Table>("exits").unwrap().raw_len(),
            0
        );
        assert!(!runner.script_runtime.entity_envs.contains_key(&retired));
        crate::sim::initialize_pending_scripts(
            &runner.scene_world,
            &mut runner.script_runtime,
            &root,
        )
        .unwrap();
        runner.step_frame();
        runner.step_frame();
        assert_eq!(survivor_env.get::<i64>("enters").unwrap(), 2);
        assert_eq!(exits.raw_len(), 1);
        assert!(runner.take_errors().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn structural_replacement_removes_old_physics_and_authoring_state() {
        let old = crate::scene::parse_scene(
            "name: Old\nentities:\n  - id: actor\n    components:\n      collider: {shape: sphere, radius: 1}\n      rigid_body: {type: dynamic}\n      custom: {version: old}\n",
        ).unwrap();
        let new = crate::scene::parse_scene(
            "name: New\nentities:\n  - id: actor\n    components:\n      mesh_renderer: {mesh: 'procedural:cube', material: materials/default.yaml}\n      collider: {shape: box}\n      custom: {version: new}\n",
        ).unwrap();
        let mut world = SceneWorld::new();
        let mut physics = PhysicsWorld::new(glam::Vec3::ZERO);
        spawn_all_entities_headless(&mut world, &old, &mut physics);
        let original = world.entity_registry["actor"];
        let old_body = world
            .world
            .get::<&physics::RigidBody>(original)
            .unwrap()
            .handle;
        let old_collider = world
            .world
            .get::<&physics::Collider>(original)
            .unwrap()
            .handle;
        assert!(entity_needs_respawn(&old.entities[0], &new.entities[0]));

        // Both removal and structural reload use this same destruction path.
        assert!(despawn_scene_entity(
            &mut world,
            "actor",
            Some(&mut physics)
        ));
        assert!(!world.world.contains(original));
        assert!(physics.rigid_body_set.get(old_body).is_none());
        assert!(physics.collider_set.get(old_collider).is_none());
        assert!(world.world.get::<&SceneEntityDefinition>(original).is_err());

        spawn_entity_headless(&mut world, &new.entities[0], &mut physics);
        assert_eq!(physics.rigid_body_set.len(), 1);
        assert_eq!(physics.collider_set.len(), 1);
        let replacement = world.entity_registry["actor"];
        assert_ne!(replacement, original);
        let definition = world
            .world
            .get::<&SceneEntityDefinition>(replacement)
            .unwrap();
        assert_eq!(definition.0.components.extra["custom"]["version"], "new");
    }

    #[test]
    fn duplicate_spawn_keeps_original_entity_and_physics() {
        let scene = crate::scene::parse_scene(
            "name: Duplicate\nentities: [{id: actor, components: {collider: {shape: sphere}}}]",
        )
        .unwrap();
        let mut world = SceneWorld::new();
        let mut physics = PhysicsWorld::new(glam::Vec3::ZERO);
        spawn_entity_headless(&mut world, &scene.entities[0], &mut physics);
        let original = world.entity_registry["actor"];
        spawn_entity_headless(&mut world, &scene.entities[0], &mut physics);
        assert_eq!(world.entity_registry["actor"], original);
        assert_eq!(world.world.len(), 1);
        assert_eq!(physics.rigid_body_set.len(), 1);
    }

    #[test]
    fn reload_removes_authored_components_instead_of_resurrecting_them_on_save() {
        let components = [
            ("camera", serde_json::json!({})),
            ("point_light", serde_json::json!({})),
            ("directional_light", serde_json::json!({})),
            ("health", serde_json::json!({"max": 10})),
            ("collision_damage", serde_json::json!({"damage": 5})),
            ("particle_emitter", serde_json::json!({})),
            ("animator", serde_json::json!({"state": "walk"})),
            ("character_controller", serde_json::json!({})),
            ("rigid_body", serde_json::json!({"type": "dynamic"})),
            ("collider", serde_json::json!({"shape": "sphere"})),
            ("gaussian_splat", serde_json::json!({"source": "world.ply"})),
            ("gpu_swarm", serde_json::json!({"count": 1})),
            ("script", serde_json::json!({"source": "main.lua"})),
            (
                "mesh_renderer",
                serde_json::json!({"mesh": "procedural:cube", "material": "default.yaml"}),
            ),
        ];
        for (name, component) in components {
            let source: EntityDef = serde_json::from_value(serde_json::json!({
                "id": "actor", "components": {name: component},
            }))
            .unwrap();
            let replacement = EntityDef {
                components: Default::default(),
                ..source.clone()
            };
            assert!(
                entity_needs_respawn(&source, &replacement),
                "{name} removal was not applied"
            );
            let mut world = SceneWorld::new();
            let mut physics = PhysicsWorld::new(glam::Vec3::ZERO);
            spawn_entity_headless(&mut world, &source, &mut physics);
            despawn_scene_entity(&mut world, &source.id, Some(&mut physics));
            spawn_entity_headless(&mut world, &replacement, &mut physics);
            let saved = crate::scene_snapshot::snapshot_scene(
                &world,
                &MeshCache::new(),
                &MaterialCache::new(),
                Some(&physics),
            )
            .unwrap();
            assert!(
                serde_json::to_value(&saved.entities[0].components).unwrap()[name].is_null(),
                "save resurrected {name}"
            );
        }
    }

    #[test]
    fn reload_rebuilds_unpatched_values_but_keeps_faithful_fast_paths() {
        let cases = [
            (
                "rigid_body",
                serde_json::json!({"mass": 1}),
                serde_json::json!({"mass": 2}),
            ),
            (
                "collider",
                serde_json::json!({"radius": 1}),
                serde_json::json!({"radius": 2}),
            ),
            (
                "character_controller",
                serde_json::json!({"height": 1.8}),
                serde_json::json!({"height": 2}),
            ),
            (
                "gaussian_splat",
                serde_json::json!({"source": "a.ply"}),
                serde_json::json!({"source": "b.ply"}),
            ),
            (
                "directional_light",
                serde_json::json!({"intensity": 1}),
                serde_json::json!({"intensity": 2}),
            ),
            (
                "health",
                serde_json::json!({"max": 10}),
                serde_json::json!({"max": 20}),
            ),
            (
                "collision_damage",
                serde_json::json!({"damage": 1}),
                serde_json::json!({"damage": 2}),
            ),
            (
                "particle_emitter",
                serde_json::json!({"spawn_rate": 1}),
                serde_json::json!({"spawn_rate": 2}),
            ),
            (
                "animator",
                serde_json::json!({"state": "idle"}),
                serde_json::json!({"state": "walk"}),
            ),
        ];
        for (name, old, new) in cases {
            let source: EntityDef = serde_json::from_value(
                serde_json::json!({"id": "actor", "components": {name: old}}),
            )
            .unwrap();
            let replacement: EntityDef = serde_json::from_value(
                serde_json::json!({"id": "actor", "components": {name: new}}),
            )
            .unwrap();
            assert!(
                entity_needs_respawn(&source, &replacement),
                "{name} edit was not applied"
            );
        }
        let source: EntityDef = serde_json::from_value(serde_json::json!({"id": "actor", "components": {
            "camera": {}, "point_light": {}, "mesh_renderer": {"mesh": "procedural:cube", "material": "a.yaml"},
        }})).unwrap();
        let replacement: EntityDef = serde_json::from_value(serde_json::json!({"id": "actor", "components": {
            "camera": {"mode": "third_person", "role": "secondary"}, "point_light": {"intensity": 2},
            "mesh_renderer": {"mesh": "procedural:sphere", "material": "b.yaml"},
            "transform": {"position": [1, 2, 3]},
        }})).unwrap();
        assert!(!entity_needs_respawn(&source, &replacement));
    }
}
