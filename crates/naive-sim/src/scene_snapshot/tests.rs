use super::*;
use crate::assets::AssetCatalog;
use crate::world::euler_degrees_to_quat;
use glam::{Quat, Vec3};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn catalog() -> AssetCatalog {
    AssetCatalog::new(Path::new("."))
}

fn definition(yaml: &str) -> EntityDef {
    serde_norway::from_str(yaml).unwrap()
}

fn register(world: &mut SceneWorld, definition: &EntityDef) -> hecs::Entity {
    let authored = definition.components.transform.as_ref();
    let transform = Transform {
        position: authored.map_or(Vec3::ZERO, |value| value.position.into()),
        rotation: authored.map_or(Quat::IDENTITY, |value| {
            euler_degrees_to_quat(value.rotation)
        }),
        scale: authored.map_or(Vec3::ONE, |value| value.scale.into()),
        ..Default::default()
    };
    let entity = world.world.spawn((
        transform,
        Tags(definition.tags.clone()),
        SceneEntityDefinition(definition.clone()),
    ));
    world.entity_registry.insert(definition.id.clone(), entity);
    entity
}

fn add_mesh(world: &mut SceneWorld, entity: hecs::Entity, assets: &mut AssetCatalog) {
    world
        .world
        .insert_one(
            entity,
            MeshRenderer {
                mesh_handle: assets.mesh("procedural:cube").unwrap(),
                material_handle: assets.material("procedural:default").unwrap(),
            },
        )
        .unwrap();
}

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "naive-snapshot-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("assets")).unwrap();
        Self(path)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cpu_catalog_reverse_names_preserve_real_asset_paths_and_handle_identity() {
    let project = Project::new();
    std::fs::write(
        project.0.join("assets/triangle.stl"),
        "solid triangle\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid triangle\n",
    )
    .unwrap();
    std::fs::write(
        project.0.join("assets/paint.yaml"),
        "properties: {base_color: [0.2, 0.3, 0.4]}\n",
    )
    .unwrap();
    let mut assets = AssetCatalog::new(&project.0);
    let mesh = assets.mesh("assets/triangle.stl").unwrap();
    let material = assets.material("assets/paint.yaml").unwrap();
    assert_eq!(assets.mesh("assets/triangle.stl").unwrap(), mesh);
    assert_eq!(assets.material("assets/paint.yaml").unwrap(), material);
    assert_eq!(assets.mesh_data(mesh).unwrap().vertices.len(), 3);
    assert_eq!(
        assets.mesh_name_for_handle(mesh).as_deref(),
        Some("assets/triangle.stl")
    );
    assert_eq!(
        assets.material_name_for_handle(material).as_deref(),
        Some("assets/paint.yaml")
    );
    assert_eq!(assets.mesh_name_for_handle(MeshHandle(usize::MAX)), None);
    assert_eq!(
        assets.material_name_for_handle(MaterialHandle(usize::MAX)),
        None
    );
    let mut world = SceneWorld::new();
    let entity = world.world.spawn((MeshRenderer {
        mesh_handle: mesh,
        material_handle: material,
    },));
    world.entity_registry.insert("triangle".into(), entity);
    let saved = snapshot_scene(&world, &assets, None).unwrap();
    let renderer = saved.entities[0].components.mesh_renderer.as_ref().unwrap();
    assert_eq!(renderer.mesh, "assets/triangle.stl");
    assert_eq!(renderer.material, "assets/paint.yaml");
    assert!(renderer.cast_shadows && renderer.receive_shadows);
}

#[test]
fn authored_metadata_unknown_components_and_noncanonical_values_round_trip() {
    let scene = crate::scene::parse_scene(
        r#"
name: Shared snapshot
settings:
  gravity: [0, -3, 0]
  ambient_light: [0.2, 0.3, 0.4]
  fog: {enabled: true, color: [0.2, 0.2, 0.3], density: 0.1}
entities:
  - id: actor
    tags: [friendly, player]
    components:
      transform: {position: [1, 2, 3], rotation: [20, 400, -15], scale: [2, 3, 4]}
      mesh_renderer: {mesh: "procedural:cube", material: "procedural:default", cast_shadows: false, receive_shadows: false}
      camera: {mode: third_person, distance: 8, height_offset: 2, pitch_limits: [-40, 60]}
      point_light: {color: [1, 0.5, 0.3], intensity: 7, range: 12}
      directional_light: {direction: [0, -2, 0], intensity: 2}
      script: {source: logic/actor.lua}
      health: {max: 100}
      collision_damage: {damage: 5, destroy_on_hit: true}
      particle_emitter: {max_particles: 20, spawn_rate: 5, enabled: false}
      animator: {state: walk, speed: 2, looping: false}
      custom_component: {nested: {enabled: true}, values: [a, b]}
  - id: splat
    components:
      gaussian_splat: {source: assets/world.ply}
  - id: swarm
    components:
      gpu_swarm: {count: 20, initial: 3}
"#,
    )
    .unwrap();
    let mut world = SceneWorld::new();
    world.current_scene = Some(scene.clone());
    let mut assets = catalog();
    let actor = register(&mut world, &scene.entities[0]);
    add_mesh(&mut world, actor, &mut assets);
    world.world.get::<&mut Transform>(actor).unwrap().rotation =
        -euler_degrees_to_quat([20., 400., -15.]);
    world
        .world
        .insert(
            actor,
            (
                Camera::default(),
                CameraMode::ThirdPerson {
                    distance: 8.,
                    height_offset: 2.,
                    pitch_min: -40_f32.to_radians(),
                    pitch_max: 60_f32.to_radians(),
                },
                PointLight {
                    color: Vec3::new(1., 0.5, 0.3),
                    intensity: 7.,
                    range: 12.,
                },
                DirectionalLight {
                    direction: -Vec3::Y,
                    intensity: 2.,
                    ..Default::default()
                },
                Script {
                    source: "logic/actor.lua".into(),
                    initialized: true,
                },
                Health {
                    current: 100.,
                    max: 100.,
                    dead: false,
                },
                CollisionDamage {
                    damage: 5.,
                    destroy_on_hit: true,
                },
                ParticleEmitter {
                    config: ParticleConfig {
                        max_particles: 20,
                        spawn_rate: 5.,
                        ..Default::default()
                    },
                    enabled: false,
                },
            ),
        )
        .unwrap();
    register(&mut world, &scene.entities[1]);
    register(&mut world, &scene.entities[2]);
    let saved = snapshot_scene(&world, &assets, None).unwrap();
    assert_eq!(
        serde_json::to_value(&saved).unwrap(),
        serde_json::to_value(&scene).unwrap()
    );
    let reloaded = crate::scene::parse_scene(&serde_norway::to_string(&saved).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(reloaded).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
}

#[test]
fn live_edits_runtime_entities_and_resolved_names_replace_authored_values() {
    let mut world = SceneWorld::new();
    let mut assets = catalog();
    let authored = definition(
        "id: z\ntags: [old]\ncomponents:\n  health: {max: 10}\n  script: {source: logic/old.lua}\n  mesh_renderer: {mesh: old.glb, material: old.yaml, cast_shadows: false}\n",
    );
    let entity = register(&mut world, &authored);
    add_mesh(&mut world, entity, &mut assets);
    world
        .world
        .get::<&mut Transform>(entity)
        .unwrap()
        .position
        .x = 9.;
    world.world.get::<&mut Tags>(entity).unwrap().0 = vec!["edited".into()];
    world
        .world
        .insert(
            entity,
            (
                Health {
                    max: 20.,
                    current: 4.,
                    dead: false,
                },
                Script {
                    source: "logic/new.lua".into(),
                    initialized: false,
                },
            ),
        )
        .unwrap();
    let runtime = world.world.spawn((Transform::default(),));
    world.entity_registry.insert("a".into(), runtime);
    add_mesh(&mut world, runtime, &mut assets);
    let saved = snapshot_scene(&world, &assets, None).unwrap();
    assert_eq!(
        saved
            .entities
            .iter()
            .map(|value| value.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    let edited = &saved.entities[1];
    assert_eq!(edited.tags, ["edited"]);
    assert_eq!(
        edited.components.transform.as_ref().unwrap().position,
        [9., 0., 0.]
    );
    assert_eq!(edited.components.health.as_ref().unwrap().current, Some(4.));
    assert_eq!(edited.components.health.as_ref().unwrap().max, 20.);
    assert_eq!(
        edited.components.script.as_ref().unwrap().source,
        "logic/new.lua"
    );
    let renderer = edited.components.mesh_renderer.as_ref().unwrap();
    assert_eq!(renderer.mesh, "procedural:cube");
    assert_eq!(renderer.material, "procedural:default");
    assert!(!renderer.cast_shadows);
    assert!(
        saved.entities[0]
            .components
            .mesh_renderer
            .as_ref()
            .unwrap()
            .cast_shadows
    );
    assert_eq!(
        serde_norway::to_string(&saved).unwrap(),
        serde_norway::to_string(&snapshot_scene(&world, &assets, None).unwrap()).unwrap()
    );
}

#[test]
fn retained_definition_precedes_scene_fallback_and_flattens_inheritance() {
    let scene = crate::scene::parse_scene(
        "name: Fallback\nentities:\n- id: actor\n  components:\n    mesh_renderer: {mesh: authored.glb, material: authored.yaml, receive_shadows: false}\n    unknown: {value: 7}\n",
    )
    .unwrap();
    let mut world = SceneWorld::new();
    world.current_scene = Some(scene.clone());
    let entity = world.world.spawn((MeshRenderer {
        mesh_handle: MeshHandle(usize::MAX),
        material_handle: MaterialHandle(usize::MAX),
    },));
    world.entity_registry.insert("actor".into(), entity);
    let assets = catalog();
    let saved = snapshot_scene(&world, &assets, None).unwrap();
    assert_eq!(
        serde_json::to_value(&saved.entities[0]).unwrap(),
        serde_json::to_value(&scene.entities[0]).unwrap()
    );
    let mut retained = scene.entities[0].clone();
    retained.id = "old-id".into();
    retained.extends = Some("deleted-template".into());
    retained.components.mesh_renderer.as_mut().unwrap().mesh = "retained.glb".into();
    world
        .world
        .insert_one(entity, SceneEntityDefinition(retained))
        .unwrap();
    let saved = snapshot_scene(&world, &assets, None).unwrap();
    assert_eq!(saved.entities[0].id, "actor");
    assert!(saved.entities[0].extends.is_none());
    assert_eq!(
        saved.entities[0]
            .components
            .mesh_renderer
            .as_ref()
            .unwrap()
            .mesh,
        "retained.glb"
    );
}

#[test]
fn defaults_removed_components_and_registry_membership_keep_existing_save_rules() {
    let mut world = SceneWorld::new();
    let authored = definition(
        "id: actor\ncomponents:\n  camera: {}\n  script: {source: old.lua}\n  point_light: {}\n  directional_light: {}\n  health: {max: 10}\n  collision_damage: {damage: 5}\n",
    );
    let entity = register(&mut world, &authored);
    world.world.spawn((Hidden,)); // Unregistered ECS entities are not scene authoring state.
    let assets = catalog();
    let saved = snapshot_scene(&world, &assets, None).unwrap();
    assert_eq!(saved.name, "Editor Scene");
    assert_eq!(saved.version, SCENE_FORMAT_VERSION);
    assert_eq!(saved.entities.len(), 1);
    let components = &saved.entities[0].components;
    assert!(components.transform.is_none());
    assert!(components.camera.is_none());
    assert!(components.script.is_none());
    assert!(components.point_light.is_none());
    assert!(components.directional_light.is_none());
    assert!(components.health.is_none());
    assert!(components.collision_damage.is_none());
    world.world.despawn(entity).unwrap();
    assert_eq!(
        snapshot_scene(&world, &assets, None).unwrap_err(),
        "Entity registry contains missing entity 'actor'"
    );
    world.entity_registry.clear();
    assert!(snapshot_scene(&world, &assets, None)
        .unwrap()
        .entities
        .is_empty());
}

#[test]
fn live_physics_mass_ccd_material_and_shape_are_saved_without_a_renderer() {
    let mut world = SceneWorld::new();
    let authored = definition(
        "id: body\ncomponents:\n  rigid_body: {type: dynamic, mass: 4}\n  collider: {shape: sphere, radius: 0.5}\n",
    );
    let entity = register(&mut world, &authored);
    let mut assets = catalog();
    add_mesh(&mut world, entity, &mut assets);
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    let shape = PhysicsShape::Sphere { radius: 0.9 };
    let (body, collider) = physics.add_dynamic_body_with_sensor(
        entity,
        Vec3::ZERO,
        Quat::IDENTITY,
        shape.clone(),
        7.,
        0.3,
        0.8,
        true,
        true,
    );
    world
        .world
        .insert(
            entity,
            (
                RigidBody {
                    handle: body,
                    body_type: PhysicsBodyType::Dynamic,
                },
                Collider {
                    handle: collider,
                    shape,
                    is_trigger: true,
                },
            ),
        )
        .unwrap();
    physics.step_fixed();
    let saved = snapshot_scene(&world, &assets, Some(&physics)).unwrap();
    let components = &saved.entities[0].components;
    let body = components.rigid_body.as_ref().unwrap();
    assert_eq!(body.body_type, "dynamic");
    assert!((body.mass - 7.).abs() < 0.00001);
    assert!(body.ccd);
    let collider = components.collider.as_ref().unwrap();
    assert_eq!(collider.shape, "sphere");
    assert_eq!(collider.radius, Some(0.9));
    assert!(collider.is_trigger);
    assert_eq!(collider.restitution, 0.3);
    assert_eq!(collider.friction, 0.8);
    let without_physics = snapshot_scene(&world, &assets, None).unwrap();
    assert_eq!(
        without_physics.entities[0]
            .components
            .rigid_body
            .as_ref()
            .unwrap()
            .mass,
        4.
    );
}

#[test]
fn unresolved_mesh_and_material_handles_report_errors_without_authored_fallback() {
    let mut world = SceneWorld::new();
    let mut assets = catalog();
    let entity = world.world.spawn((MeshRenderer {
        mesh_handle: MeshHandle(usize::MAX),
        material_handle: MaterialHandle(usize::MAX),
    },));
    world.entity_registry.insert("actor".into(), entity);
    assert_eq!(
        snapshot_scene(&world, &assets, None).unwrap_err(),
        "Cannot resolve mesh asset for 'actor'"
    );
    world
        .world
        .get::<&mut MeshRenderer>(entity)
        .unwrap()
        .mesh_handle = assets.mesh("procedural:cube").unwrap();
    assert_eq!(
        snapshot_scene(&world, &assets, None).unwrap_err(),
        "Cannot resolve material asset for 'actor'"
    );
    world
        .world
        .get::<&mut MeshRenderer>(entity)
        .unwrap()
        .material_handle = assets.material("procedural:default").unwrap();
    assert!(snapshot_scene(&world, &assets, None).is_ok());
}

#[test]
fn unrepresentable_runtime_state_is_rejected_instead_of_discarded() {
    let mut world = SceneWorld::new();
    let entity = register(&mut world, &definition("id: actor"));
    let mut assets = catalog();
    add_mesh(&mut world, entity, &mut assets);
    world
        .world
        .insert_one(entity, MaterialOverride::default())
        .unwrap();
    assert!(snapshot_scene(&world, &assets, None).is_ok());
    for material in [
        MaterialOverride {
            base_color: Some([1., 0., 0.]),
            ..Default::default()
        },
        MaterialOverride {
            emission: Some([1.; 3]),
            ..Default::default()
        },
        MaterialOverride {
            roughness: Some(0.2),
            ..Default::default()
        },
        MaterialOverride {
            metallic: Some(0.5),
            ..Default::default()
        },
    ] {
        *world.world.get::<&mut MaterialOverride>(entity).unwrap() = material;
        assert_eq!(
            snapshot_scene(&world, &assets, None).unwrap_err(),
            "Cannot save material override on 'actor': write a material asset first"
        );
    }
    world.world.remove_one::<MaterialOverride>(entity).unwrap();
    world.world.insert_one(entity, Hidden).unwrap();
    assert_eq!(
        snapshot_scene(&world, &assets, None).unwrap_err(),
        "Cannot save hidden entity 'actor': scene files do not represent visibility overrides"
    );
    world.world.remove_one::<Hidden>(entity).unwrap();
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
    assert_eq!(snapshot_scene(&world, &assets, None).unwrap_err(), "Cannot save transient projectile 'actor': scene files do not represent projectile lifetime or velocity");
    world.world.remove_one::<Projectile>(entity).unwrap();
    world
        .world
        .insert_one(
            entity,
            UnsavedRuntimeState("texture edits have no scene representation".into()),
        )
        .unwrap();
    assert_eq!(
        snapshot_scene(&world, &assets, None).unwrap_err(),
        "Cannot save entity 'actor': texture edits have no scene representation"
    );
}

#[test]
fn generated_authored_fallback_is_still_unsavable() {
    let mut world = SceneWorld::new();
    for name in ["runtime:mesh", "generated:mesh"] {
        let mut authored = definition(
            "id: actor\ncomponents:\n  mesh_renderer: {mesh: old.glb, material: old.yaml}\n",
        );
        authored.components.mesh_renderer.as_mut().unwrap().mesh = name.into();
        let entity = register(&mut world, &authored);
        world
            .world
            .insert_one(
                entity,
                MeshRenderer {
                    mesh_handle: MeshHandle(usize::MAX),
                    material_handle: MaterialHandle(usize::MAX),
                },
            )
            .unwrap();
        assert_eq!(
            snapshot_scene(&world, &catalog(), None).unwrap_err(),
            "Cannot save generated mesh on 'actor': export it to a mesh asset first"
        );
        world.world.despawn(entity).unwrap();
    }
}
