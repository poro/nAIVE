//! Exercise the real renderer caches, rather than assigning fake GPU handles.

use super::{snapshot_scene, NativeSnapshotAssets, SnapshotAssetLookup};
use crate::components::*;
use crate::material::MaterialCache;
use crate::mesh::{MeshCache, TextureResources};
use crate::physics::{PhysicsWorld, RigidBody};
use crate::scene::{parse_scene, SceneFile};
use crate::splat::SplatCache;
use crate::texture_cache::TextureCache;
use crate::world::{
    spawn_entity_headless_with_assets, spawn_entity_native, SceneEntityDefinition, SceneWorld,
    UnsavedRuntimeState,
};
use naive_sim::assets::AssetCatalog;
use std::path::PathBuf;

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "naive-snapshot-gpu-parity-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("authored.yaml"), "properties: {metallic: 0.2}").unwrap();
        std::fs::write(
            path.join("edited.yaml"),
            "properties: {base_color: [0.2, 0.4, 0.6], albedo_map: color.png}",
        )
        .unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([12, 34, 56, 255]))
            .save(path.join("color.png"))
            .unwrap();
        Self(path)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct SnapshotPair {
    native: SceneWorld,
    headless: SceneWorld,
    native_physics: PhysicsWorld,
    headless_physics: PhysicsWorld,
    meshes: MeshCache,
    materials: MaterialCache,
    assets: AssetCatalog,
}

impl SnapshotPair {
    fn snapshots(&self) -> (Result<SceneFile, String>, Result<SceneFile, String>) {
        (
            snapshot_scene(
                &self.native,
                &self.meshes,
                &self.materials,
                Some(&self.native_physics),
            ),
            naive_sim::scene_snapshot::snapshot_scene(
                &self.headless,
                &self.assets,
                Some(&self.headless_physics),
            ),
        )
    }

    fn assert_yaml(&self) -> SceneFile {
        let (native, headless) = self.snapshots();
        let native = native.unwrap();
        let native_yaml = serde_norway::to_string(&native).unwrap();
        assert_eq!(
            native_yaml,
            serde_norway::to_string(&headless.unwrap()).unwrap()
        );
        assert_eq!(
            native_yaml,
            serde_norway::to_string(&self.snapshots().0.unwrap()).unwrap(),
            "snapshot YAML must be deterministic"
        );
        native
    }

    fn assert_error(&self, expected: &str) {
        let (native, headless) = self.snapshots();
        assert_eq!(native.unwrap_err(), expected);
        assert_eq!(headless.unwrap_err(), expected);
    }
}

#[test]
#[ignore = "requires native GPU/Vulkan"]
fn native_gpu_caches_match_headless_snapshot() {
    // Match the swarm device setup, while honoring CI's WGPU_BACKEND=vulkan.
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("no GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    let project = Project::new();
    let textures = TextureResources::new(&device, &queue);
    let mut texture_cache = TextureCache::new();
    let mut splats = SplatCache::new();
    let mut pair = SnapshotPair {
        native: SceneWorld::new(),
        headless: SceneWorld::new(),
        native_physics: PhysicsWorld::new(glam::Vec3::ZERO),
        headless_physics: PhysicsWorld::new(glam::Vec3::ZERO),
        meshes: MeshCache::new(),
        materials: MaterialCache::new(),
        assets: AssetCatalog::new(&project.0),
    };
    // Unrelated native assets make matching numeric handles an invalid shortcut.
    pair.meshes
        .get_or_load(&device, &queue, &project.0, "procedural:sphere", None)
        .unwrap();
    pair.materials
        .get_or_load(
            &device,
            &queue,
            &project.0,
            "procedural:default",
            None,
            None,
        )
        .unwrap();
    let scene = parse_scene(
        r#"
name: Native and headless authoring
settings: {gravity: [0, 0, 0], ambient_light: [0.2, 0.3, 0.4]}
entities:
  - id: z_actor
    tags: [authored]
    components:
      transform: {position: [1, 2, 3], rotation: [15, 400, -10], scale: [2, 3, 4]}
      mesh_renderer: {mesh: 'procedural:cube', material: authored.yaml, cast_shadows: false, receive_shadows: false}
      script: {source: logic/authored.lua}
      health: {max: 90}
      rigid_body: {type: dynamic, mass: 2, ccd: true}
      collider: {shape: sphere, radius: 0.4, friction: 0.5}
      authoring_only: {nested: [one, {enabled: true}]}
  - id: m_light
    components:
      point_light: {intensity: 7}
"#,
    )
    .unwrap();
    for definition in &scene.entities {
        spawn_entity_native(
            &mut pair.native,
            definition,
            &device,
            &queue,
            &project.0,
            &mut pair.meshes,
            &mut pair.materials,
            &mut splats,
            Some(&mut pair.native_physics),
            Some(&textures),
            Some(&mut texture_cache),
        )
        .unwrap();
    }
    for definition in scene.entities.iter().rev() {
        spawn_entity_headless_with_assets(
            &mut pair.headless,
            definition,
            &mut pair.headless_physics,
            &mut pair.assets,
        )
        .unwrap();
    }
    pair.native.current_scene = Some(scene.clone());
    pair.headless.current_scene = Some(scene.clone());
    let native_mesh = pair
        .meshes
        .get_or_load(&device, &queue, &project.0, "procedural:plane", None)
        .unwrap();
    let native_material = pair
        .materials
        .get_or_load(
            &device,
            &queue,
            &project.0,
            "edited.yaml",
            Some(&mut texture_cache),
            Some(&textures.bind_group_layout),
        )
        .unwrap();
    let headless_mesh = pair.assets.mesh("procedural:plane").unwrap();
    let headless_material = pair.assets.material("edited.yaml").unwrap();
    assert_ne!(native_mesh, headless_mesh);
    assert_ne!(native_material, headless_material);
    assert!(pair.meshes.get(native_mesh).vertex_buffer.size() > 0);
    assert!(pair.meshes.get(native_mesh).index_buffer.size() > 0);
    assert!(pair.materials.get(native_material).albedo_texture.is_some());
    let native_names = NativeSnapshotAssets::new(&pair.meshes, &pair.materials);
    assert_eq!(
        native_names.mesh_name(native_mesh).as_deref(),
        Some("procedural:plane")
    );
    assert_eq!(
        native_names.material_name(native_material).as_deref(),
        Some("edited.yaml")
    );

    for (world, physics, mesh_handle, material_handle) in [
        (
            &mut pair.native,
            &mut pair.native_physics,
            native_mesh,
            native_material,
        ),
        (
            &mut pair.headless,
            &mut pair.headless_physics,
            headless_mesh,
            headless_material,
        ),
    ] {
        let actor = world.entity_registry["z_actor"];
        world.world.get::<&mut Transform>(actor).unwrap().position = glam::vec3(9., 8., 7.);
        world.world.get::<&mut Health>(actor).unwrap().current = 17.;
        world.world.get::<&mut Script>(actor).unwrap().source = "logic/edited.lua".into();
        world.world.get::<&mut Tags>(actor).unwrap().0 = vec!["live".into()];
        *world.world.get::<&mut MeshRenderer>(actor).unwrap() = MeshRenderer {
            mesh_handle,
            material_handle,
        };
        let handle = world.world.get::<&RigidBody>(actor).unwrap().handle;
        physics.set_friction(handle, 0.25);
        physics.set_restitution(handle, 0.75);
        world
            .world
            .remove_one::<PointLight>(world.entity_registry["m_light"])
            .unwrap();
        // No retained definition: both names must come from the actual caches.
        let runtime = world.world.spawn((
            EntityId("a_runtime".into()),
            Transform::default(),
            MeshRenderer {
                mesh_handle,
                material_handle,
            },
            Script {
                source: "logic/runtime.lua".into(),
                initialized: true,
            },
        ));
        world.entity_registry.insert("a_runtime".into(), runtime);
    }
    let saved = pair.assert_yaml();
    assert_eq!(saved.name, scene.name);
    assert_eq!(
        serde_json::to_value(&saved.settings).unwrap(),
        serde_json::to_value(&scene.settings).unwrap()
    );
    assert_eq!(
        saved
            .entities
            .iter()
            .map(|entity| entity.id.as_str())
            .collect::<Vec<_>>(),
        ["a_runtime", "m_light", "z_actor"]
    );
    let actor = &saved.entities[2];
    assert_eq!(actor.tags, ["live"]);
    assert_eq!(
        actor.components.transform.as_ref().unwrap().position,
        [9., 8., 7.]
    );
    assert_eq!(
        actor.components.transform.as_ref().unwrap().rotation,
        [15., 400., -10.]
    );
    let mesh = actor.components.mesh_renderer.as_ref().unwrap();
    assert_eq!(
        mesh.mesh, "procedural:plane",
        "live handle must override retained cube"
    );
    assert_eq!(
        mesh.material, "edited.yaml",
        "live handle must override retained material"
    );
    assert!(!mesh.cast_shadows && !mesh.receive_shadows);
    assert_eq!(actor.components.health.as_ref().unwrap().current, Some(17.));
    assert_eq!(
        actor.components.script.as_ref().unwrap().source,
        "logic/edited.lua"
    );
    assert_eq!(actor.components.collider.as_ref().unwrap().friction, 0.25);
    assert_eq!(
        actor.components.collider.as_ref().unwrap().restitution,
        0.75
    );
    assert_eq!(
        serde_json::to_value(&actor.components).unwrap()["authoring_only"],
        serde_json::json!({"nested": ["one", {"enabled": true}]})
    );
    assert!(saved.entities[1].components.point_light.is_none());
    assert_eq!(
        saved.entities[0]
            .components
            .mesh_renderer
            .as_ref()
            .unwrap()
            .mesh,
        "procedural:plane"
    );
    assert_eq!(
        saved.entities[0].components.script.as_ref().unwrap().source,
        "logic/runtime.lua"
    );

    for world in [&mut pair.native, &mut pair.headless] {
        world
            .world
            .insert_one(world.entity_registry["a_runtime"], Hidden)
            .unwrap();
    }
    pair.assert_error(
        "Cannot save hidden entity 'a_runtime': scene files do not represent visibility overrides",
    );
    for world in [&mut pair.native, &mut pair.headless] {
        let entity = world.entity_registry["a_runtime"];
        world.world.remove_one::<Hidden>(entity).unwrap();
        world
            .world
            .insert_one(entity, UnsavedRuntimeState("runtime texture edit".into()))
            .unwrap();
    }
    pair.assert_error("Cannot save entity 'a_runtime': runtime texture edit");
    for world in [&mut pair.native, &mut pair.headless] {
        let entity = world.entity_registry["a_runtime"];
        world
            .world
            .remove_one::<UnsavedRuntimeState>(entity)
            .unwrap();
        world
            .world
            .get::<&mut MeshRenderer>(entity)
            .unwrap()
            .mesh_handle = MeshHandle(usize::MAX);
    }
    pair.assert_error("Cannot resolve mesh asset for 'a_runtime'");
    for (world, mesh) in [
        (&mut pair.native, native_mesh),
        (&mut pair.headless, headless_mesh),
    ] {
        let entity = world.entity_registry["a_runtime"];
        let mut renderer = world.world.get::<&mut MeshRenderer>(entity).unwrap();
        renderer.mesh_handle = mesh;
        renderer.material_handle = MaterialHandle(usize::MAX);
    }
    pair.assert_error("Cannot resolve material asset for 'a_runtime'");

    let triangle = naive_sim::assets::runtime_mesh(
        &[[0., 0., 0.], [1., 0., 0.], [0., 0., 1.]],
        &[],
        &[],
        &[0, 1, 2],
    )
    .unwrap();
    let generated = pair
        .meshes
        .insert_runtime_mesh(&device, "triangle", &triangle);
    for (world, mesh_handle, material_handle) in [
        (&mut pair.native, generated, native_material),
        (
            &mut pair.headless,
            MeshHandle(usize::MAX),
            headless_material,
        ),
    ] {
        let entity = world.entity_registry["a_runtime"];
        let mut definition = saved.entities[0].clone();
        definition.components.mesh_renderer.as_mut().unwrap().mesh = "runtime:triangle".into();
        world
            .world
            .insert_one(entity, SceneEntityDefinition(definition))
            .unwrap();
        *world.world.get::<&mut MeshRenderer>(entity).unwrap() = MeshRenderer {
            mesh_handle,
            material_handle,
        };
    }
    // Neither snapshot can represent a mesh made by mesh.create.
    pair.assert_error("Cannot save generated mesh on 'a_runtime': export it to a mesh asset first");
}
