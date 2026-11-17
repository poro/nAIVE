//! Native asset-name adapter for renderer-independent scene snapshots.
//!
//! The shared generator and authoring rules live in `naive-sim`. This module
//! preserves the native entry point used by save, undo and editor commands.

use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::PhysicsWorld;
use crate::scene::SceneFile;
use crate::world::SceneWorld;

pub use naive_sim::scene_snapshot::SnapshotAssetLookup;

/// Read-only asset names from the native renderer caches.
pub struct NativeSnapshotAssets<'a> {
    meshes: &'a MeshCache,
    materials: &'a MaterialCache,
}

impl<'a> NativeSnapshotAssets<'a> {
    pub fn new(meshes: &'a MeshCache, materials: &'a MaterialCache) -> Self {
        Self { meshes, materials }
    }
}

impl SnapshotAssetLookup for NativeSnapshotAssets<'_> {
    fn mesh_name(&self, handle: crate::components::MeshHandle) -> Option<String> {
        self.meshes.name_for_handle(handle)
    }

    fn material_name(&self, handle: crate::components::MaterialHandle) -> Option<String> {
        self.materials.name_for_handle(handle)
    }
}

/// Preserve the native snapshot API while sharing all document generation.
pub fn snapshot_scene(
    world: &SceneWorld,
    meshes: &MeshCache,
    materials: &MaterialCache,
    physics: Option<&PhysicsWorld>,
) -> Result<SceneFile, String> {
    naive_sim::scene_snapshot::snapshot_scene(
        world,
        &NativeSnapshotAssets::new(meshes, materials),
        physics,
    )
}

#[cfg(test)]
mod parity_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::physics::RigidBody;
    use crate::world::{spawn_all_entities_headless, spawn_entity_headless, UnsavedRuntimeState};

    fn snapshot(world: &SceneWorld, physics: &PhysicsWorld) -> SceneFile {
        snapshot_scene(
            world,
            &MeshCache::new(),
            &MaterialCache::new(),
            Some(physics),
        )
        .unwrap()
    }

    #[test]
    fn snapshot_preserves_every_authored_component_and_unknown_data() {
        let scene = crate::scene::parse_scene(r#"
name: Round trip
settings:
  gravity: [0, -3, 0]
  ambient_light: [0.2, 0.3, 0.4]
  fog: {enabled: true, color: [0.2, 0.2, 0.3], density: 0.1}
entities:
  - id: actor
    tags: [player, friendly]
    components:
      transform: {position: [1, 2, 3], rotation: [20, 400, -15], scale: [2, 2, 2]}
      mesh_renderer: {mesh: assets/actor.glb, material: assets/actor.yaml, cast_shadows: false, receive_shadows: false}
      camera: {mode: third_person, distance: 8, height_offset: 2, pitch_limits: [-40, 60]}
      point_light: {color: [1, 0.5, 0.3], intensity: 7, range: 12}
      directional_light: {direction: [0, -2, 0], color: [1, 1, 1], intensity: 2}
      rigid_body: {type: kinematic, mass: 4, ccd: false}
      collider: {shape: capsule, radius: 0.3, half_height: 0.8, friction: 0.7, restitution: 0.2}
      script: {source: logic/actor.lua}
      health: {max: 100, current: 70}
      collision_damage: {damage: 5, destroy_on_hit: true}
      particle_emitter: {max_particles: 20, spawn_rate: 5, enabled: false}
      animator: {state: walk, speed: 2, looping: false}
      custom_component: {nested: {enabled: true}, values: [a, b]}
  - id: splat
    components:
      gaussian_splat: {source: assets/world.ply}
      transform: {position: [5, 0, 0]}
  - id: character
    components:
      character_controller: {move_speed: 8, height: 2, radius: 0.4}
  - id: swarm
    components:
      gpu_swarm: {count: 20, mesh: "procedural:cube", material: "procedural:default", initial: 3}
"#).unwrap();
        let mut world = SceneWorld::new();
        let mut physics = PhysicsWorld::new(scene.settings.gravity.into());
        spawn_all_entities_headless(&mut world, &scene, &mut physics);
        let actor = world.entity_registry["actor"];
        assert!(world.world.get::<&Camera>(actor).is_ok());
        assert!(world.world.get::<&PointLight>(actor).is_ok());
        assert!(world.world.get::<&DirectionalLight>(actor).is_ok());
        assert!(!world.world.get::<&Script>(actor).unwrap().initialized);
        let body = world.world.get::<&RigidBody>(actor).unwrap();
        assert!(physics.rigid_body_set[body.handle].is_kinematic());
        drop(body);
        let saved = snapshot(&world, &physics);
        assert_eq!(
            serde_json::to_value(&saved.settings).unwrap(),
            serde_json::to_value(&scene.settings).unwrap()
        );
        for source in &scene.entities {
            let saved = saved
                .entities
                .iter()
                .find(|saved| saved.id == source.id)
                .unwrap();
            assert_eq!(
                serde_json::to_value(&saved.components).unwrap(),
                serde_json::to_value(&source.components).unwrap(),
                "component loss on {}",
                source.id
            );
            assert_eq!(saved.tags, source.tags);
        }
        let yaml = serde_norway::to_string(&saved).unwrap();
        let reloaded = crate::scene::parse_scene(&yaml).unwrap();
        let mut second = SceneWorld::new();
        let mut second_physics = PhysicsWorld::new(reloaded.settings.gravity.into());
        spawn_all_entities_headless(&mut second, &reloaded, &mut second_physics);
        assert_eq!(
            serde_json::to_value(snapshot(&second, &second_physics)).unwrap(),
            serde_json::to_value(saved).unwrap()
        );
    }

    #[test]
    fn snapshot_includes_live_values_and_runtime_additions_in_stable_order() {
        let scene = crate::scene::parse_scene("name: Live\nsettings: {}\nentities:\n  - id: z\n    components:\n      health: {max: 10}\n      script: {source: logic/z.lua}\n").unwrap();
        let mut world = SceneWorld::new();
        let mut physics = PhysicsWorld::new(scene.settings.gravity.into());
        spawn_all_entities_headless(&mut world, &scene, &mut physics);
        let actor = world.entity_registry["z"];
        world.world.get::<&mut Transform>(actor).unwrap().position.x = 9.0;
        world.world.get::<&mut Health>(actor).unwrap().current = 4.0;
        let added = crate::world::SpawnCommand {
            id: "a".into(),
            mesh: "procedural:cube".into(),
            material: "procedural:default".into(),
            position: [3., 2., 1.],
            scale: [1.; 3],
            collider: None,
            rigid_body: None,
            script: Some("logic/new.lua".into()),
        }
        .entity_def();
        spawn_entity_headless(&mut world, &added, &mut physics);
        let saved = snapshot(&world, &physics);
        assert_eq!(
            saved
                .entities
                .iter()
                .map(|entity| entity.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            saved.entities[0].components.script.as_ref().unwrap().source,
            "logic/new.lua"
        );
        assert_eq!(
            saved.entities[0]
                .components
                .mesh_renderer
                .as_ref()
                .unwrap()
                .mesh,
            "procedural:cube"
        );
        assert_eq!(
            saved.entities[1]
                .components
                .health
                .as_ref()
                .unwrap()
                .current,
            Some(4.)
        );
        assert_eq!(
            saved.entities[1]
                .components
                .transform
                .as_ref()
                .unwrap()
                .position[0],
            9.
        );
        assert_eq!(
            serde_norway::to_string(&saved).unwrap(),
            serde_norway::to_string(&snapshot(&world, &physics)).unwrap()
        );
    }

    #[test]
    fn unsupported_runtime_state_is_an_error_instead_of_data_loss() {
        let mut world = SceneWorld::new();
        let mut physics = PhysicsWorld::new(glam::Vec3::ZERO);
        let definition = crate::scene::parse_scene("name: Unsupported\nentities: [{id: actor}]")
            .unwrap()
            .entities
            .remove(0);
        spawn_entity_headless(&mut world, &definition, &mut physics);
        let entity = world.entity_registry["actor"];
        world
            .world
            .insert_one(
                entity,
                MaterialOverride {
                    roughness: Some(0.2),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(snapshot_scene(
            &world,
            &MeshCache::new(),
            &MaterialCache::new(),
            Some(&physics)
        )
        .unwrap_err()
        .contains("material override"));
        world.world.remove_one::<MaterialOverride>(entity).unwrap();
        world
            .world
            .insert_one(
                entity,
                UnsavedRuntimeState("texture overrides have no scene representation".into()),
            )
            .unwrap();
        assert!(snapshot_scene(
            &world,
            &MeshCache::new(),
            &MaterialCache::new(),
            Some(&physics)
        )
        .unwrap_err()
        .contains("texture overrides"));
        world
            .world
            .remove_one::<UnsavedRuntimeState>(entity)
            .unwrap();
        world
            .world
            .insert_one(
                entity,
                Projectile {
                    damage: 1.,
                    lifetime: 2.,
                    age: 0.,
                    owner_id: "owner".into(),
                },
            )
            .unwrap();
        assert!(snapshot_scene(
            &world,
            &MeshCache::new(),
            &MaterialCache::new(),
            Some(&physics)
        )
        .unwrap_err()
        .contains("transient projectile"));
    }
}
