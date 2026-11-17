use super::*;

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("naive-assets-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.0.join(path), contents).unwrap();
    }
    fn scene(&self, mesh: &str, material: &str) {
        self.write("scene.yaml", &format!("name: Assets\nentities:\n  - id: floor\n    components:\n      transform: {{position: [0,0,0], scale: [4,1,4]}}\n      mesh_renderer: {{mesh: '{mesh}', material: '{material}'}}\n      collider: {{shape: trimesh}}\n"));
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn invalid_assets_fail_before_scene_replacement() {
    let p = Project::new();
    p.scene("procedural:plane", "procedural:default");
    let mut runner = TestRunner::new(&p.0);
    runner.load_scene("scene.yaml").unwrap();
    let original = runner.scene_world.borrow().entity_registry["floor"];
    for (mesh, material) in [
        ("missing.glb", "procedural:default"),
        ("procedural:unknown", "procedural:default"),
        ("procedural:cube", "missing.yaml"),
        ("procedural:cube", "broken.yaml"),
        ("procedural:cube", "textured.yaml"),
    ] {
        p.write("broken.yaml", "properties: [");
        p.write("textured.yaml", "properties: {albedo_map: missing.png}");
        p.scene(mesh, material);
        assert!(
            runner.load_scene("scene.yaml").is_err(),
            "{mesh}, {material}"
        );
        assert_eq!(
            runner.scene_world.borrow().entity_registry["floor"],
            original
        );
    }
}

#[test]
fn trimesh_uses_scaled_mesh_geometry_for_raycast() {
    let p = Project::new();
    p.scene("procedural:plane", "procedural:default");
    let mut runner = TestRunner::new(&p.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.step_frames(1);
    let world = runner.scene_world.borrow();
    let floor = world.entity_registry["floor"];
    let collider = world.world.get::<&crate::physics::Collider>(floor).unwrap();
    match &collider.shape {
        crate::physics::PhysicsShape::Trimesh { vertices, indices } => {
            assert!(!indices.is_empty());
            assert!(vertices.iter().any(|v| v.x.abs() > 1.5));
            assert!(vertices.iter().all(|v| v.y == 0.0));
        }
        other => panic!("Expected actual mesh triangles, got {other:?}"),
    }
    drop(collider);
    drop(world);
    assert!(runner.script_runtime.lua.load("local hit, distance = physics.raycast(1.5,2,0,0,-1,0,10); assert(hit and math.abs(distance - 2) < 0.001); local hit, id = physics.hitscan(1.5,2,0,0,-1,0,10); assert(hit and id == 'floor')").exec().is_ok());
}

#[test]
fn deferred_spawn_validates_assets_before_creating_any_entity_or_body() {
    let p = Project::new();
    p.write("scene.yaml", "name: Empty\nentities: []\n");
    let mut runner = TestRunner::new(&p.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.script_runtime.lua.load("entity.spawn_ex({id='bad', mesh='missing.glb', material='procedural:default', collider={shape='box'}})").exec().unwrap();
    runner.step_frames(1);
    assert!(!runner
        .scene_world
        .borrow()
        .entity_registry
        .contains_key("bad"));
    assert!(!runner.take_errors().is_empty());
    assert_eq!(runner.physics_world.borrow().rigid_body_set.len(), 0);
}

#[test]
fn stl_mesh_data_is_decoded_and_invalid_gltf_buffers_are_rejected() {
    let p = Project::new();
    p.write("triangle.stl", "solid t\nfacet normal 0 1 0\nouter loop\nvertex 0 0 0\nvertex 0 0 1\nvertex 1 0 0\nendloop\nendfacet\nendsolid t\n");
    let mesh = crate::assets::load_mesh(&p.0, "triangle.stl").unwrap();
    assert_eq!(mesh.indices.len(), 3);
    assert_eq!(mesh.vertices.len(), 3);
    p.write(
        "broken.gltf",
        r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"missing.bin","byteLength":36}]}"#,
    );
    assert!(crate::assets::load_mesh(&p.0, "broken.gltf").is_err());
    p.write(
        "escaping.gltf",
        r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"../outside.bin","byteLength":36}]}"#,
    );
    let error = crate::assets::load_mesh(&p.0, "escaping.gltf")
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("parent") || error.contains("traversal") || error.contains(".."),
        "{error}"
    );
}

#[test]
fn headless_deferred_spawns_reuse_cooking_and_respect_mesh_scale_across_scene_reloads() {
    let project = Project::new();
    project.scene("procedural:plane", "procedural:default");
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    assert_eq!(
        runner
            .physics_world
            .borrow()
            .collider_cooking_stats()
            .mesh_cooks,
        1
    );

    runner
        .script_runtime
        .lua
        .load(
            r#"
        entity.spawn_ex({id='same', mesh='procedural:plane', material='procedural:default',
            position={10,0,0}, scale={4,1,4}, collider={shape='trimesh'}})
        entity.spawn_ex({id='larger', mesh='procedural:plane', material='procedural:default',
            position={20,0,0}, scale={8,1,8}, collider={shape='trimesh'}})
    "#,
        )
        .exec()
        .unwrap();
    runner.step_frame();
    let stats = runner.physics_world.borrow().collider_cooking_stats();
    // Preparation cooks or finds each shape; construction then finds it.
    assert_eq!((stats.mesh_cooks, stats.cached_meshes), (2, 2));
    assert!(stats.cache_hits >= 3, "{stats:?}");
    assert!(runner.take_errors().is_empty());

    runner
        .script_runtime
        .lua
        .load("entity.destroy('same')")
        .exec()
        .unwrap();
    runner.step_frame();
    runner
        .script_runtime
        .lua
        .load(
            r#"
        entity.spawn_ex({id='replacement', mesh='procedural:plane', material='procedural:default',
            position={10,0,0}, scale={4,1,4}, collider={shape='trimesh'}})
    "#,
        )
        .exec()
        .unwrap();
    runner.step_frame();
    let stats = runner.physics_world.borrow().collider_cooking_stats();
    assert_eq!(stats.mesh_cooks, 2);
    assert!(runner.take_errors().is_empty());

    // Reloading resets the bodies but keeps the shapes preflight cooked.
    runner.load_scene("scene.yaml").unwrap();
    let physics = runner.physics_world.borrow();
    let stats = physics.collider_cooking_stats();
    assert_eq!((stats.mesh_cooks, stats.cached_meshes), (2, 2));
    assert_eq!(physics.collider_set.len(), 1);
}

#[test]
fn uncookable_trimesh_colliders_reject_the_load_and_keep_the_current_scene() {
    let project = Project::new();
    project.scene("procedural:plane", "procedural:default");
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    for scale in ["[0, 0, 0]", "[1, 0, 0]", "[1e30, 1e30, 1e30]"] {
        project.write(
            "bad.yaml",
            &format!(
                "name: Bad\nentities:\n  - id: wall\n    components:\n      transform: {{position: [0,0,0], scale: {scale}}}\n      mesh_renderer: {{mesh: 'procedural:cube', material: 'procedural:default'}}\n      collider: {{shape: trimesh}}\n"
            ),
        );
        let started = std::time::Instant::now();
        let error = runner.load_scene("bad.yaml").unwrap_err();
        assert!(
            error.contains("wall") && error.contains("trimesh"),
            "{error}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{scale}"
        );
        // Rejected before teardown: the current scene and its body remain.
        assert!(runner
            .scene_world
            .borrow()
            .entity_registry
            .contains_key("floor"));
        assert_eq!(runner.physics_world.borrow().collider_set.len(), 1);
    }

    // A runtime spawn fails on its own instead of crashing the host.
    runner
        .script_runtime
        .lua
        .load(
            "entity.spawn_ex({id='flat', mesh='procedural:cube', material='procedural:default', \
             position={0,0,0}, scale={0,0,0}, collider={shape='trimesh'}})",
        )
        .exec()
        .unwrap();
    runner.step_frame();
    assert!(!runner
        .scene_world
        .borrow()
        .entity_registry
        .contains_key("flat"));
    let errors = runner.take_errors();
    assert!(errors.iter().any(|e| e.contains("trimesh")), "{errors:?}");
}

/// A wedge whose slanted face no stock shape has: a ray down at (0.25, 0.25)
/// meets it at height 0.75.
const WEDGE_SCRIPT: &str = r#"
function init()
    mesh.create('wedge', {{0,0,0},{2,0,0},{0,0,2},{0,1,0}}, {0,2,1, 0,1,3, 0,3,2, 1,2,3}, {})
    entity.spawn_ex({id='wedge', mesh='runtime:wedge', material='procedural:default',
        position={0,0,0}, collider={shape='trimesh'}})
end
"#;

fn scripted_scene(project: &Project, script: &str) {
    project.write("game.lua", script);
    project.write(
        "scene.yaml",
        "name: Runtime\nentities:\n  - id: author\n    components:\n      transform: {}\n      script: {source: game.lua}\n",
    );
}

#[test]
fn runtime_meshes_are_available_to_spawns_in_the_same_frame() {
    let project = Project::new();
    scripted_scene(&project, WEDGE_SCRIPT);
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.step_frames(2);
    assert!(runner.take_errors().is_empty());

    let world = runner.scene_world.borrow();
    let entity = world.entity_registry["wedge"];
    let mesh_handle = world
        .world
        .get::<&crate::components::MeshRenderer>(entity)
        .unwrap()
        .mesh_handle;
    let catalog = runner.asset_catalog.borrow();
    assert_eq!(
        catalog.mesh_name_for_handle(mesh_handle).as_deref(),
        Some("runtime:wedge")
    );
    let mesh = catalog.mesh_data(mesh_handle).unwrap();
    let positions: Vec<_> = mesh.vertices.iter().map(|v| v.position).collect();
    assert_eq!(
        positions,
        [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0],
            [0.0, 1.0, 0.0]
        ]
    );
    assert_eq!(mesh.indices, [0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3]);

    // Physics uses the same triangles.
    let (_, distance, _) = runner
        .physics_world
        .borrow()
        .raycast(glam::Vec3::new(0.25, 5.0, 0.25), glam::Vec3::NEG_Y, 10.0)
        .unwrap();
    assert!((distance - 4.25).abs() < 0.05, "{distance}");
}

#[test]
fn bad_or_missing_runtime_meshes_fail_with_their_reason() {
    let project = Project::new();
    scripted_scene(
        &project,
        "function init() mesh.create('broken', {{0,0,0},{1,0,0},{0,1,0}}, {0,1,7}, {}) end",
    );
    let mut runner = TestRunner::new(&project.0);
    let error = runner.load_scene("scene.yaml").unwrap_err();
    assert!(error.contains("mesh.create 'broken'"), "{error}");

    scripted_scene(
        &project,
        "function init() entity.spawn_ex({id='ghost', mesh='runtime:ghost', material='procedural:default'}) end",
    );
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.step_frame();
    let errors = runner.take_errors();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("runtime mesh 'ghost'") && e.contains("mesh.create")),
        "{errors:?}"
    );
}

/// The demos that create meshes at run time get those meshes, not a stand-in.
#[test]
fn demo_runtime_meshes_come_from_their_scripts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../project");
    let mut runner = TestRunner::new(&root);

    runner.load_scene("scenes/tier3_showcase.yaml").unwrap();
    runner.step_frames(2);
    {
        let world = runner.scene_world.borrow();
        let catalog = runner.asset_catalog.borrow();
        for i in 1..=5 {
            let entity = world.entity_registry[&format!("diamond_{i}")];
            let renderer = world
                .world
                .get::<&crate::components::MeshRenderer>(entity)
                .unwrap();
            assert_eq!(
                catalog
                    .mesh_name_for_handle(renderer.mesh_handle)
                    .as_deref(),
                Some("runtime:diamond")
            );
            let mesh = catalog.mesh_data(renderer.mesh_handle).unwrap();
            assert_eq!((mesh.vertices.len(), mesh.indices.len()), (6, 24));
            assert_eq!(mesh.vertices[0].position, [0.0, 1.2, 0.0]);
        }
    }

    runner.load_scene("scenes/procedural_world.yaml").unwrap();
    runner.step_seconds(1.0);
    let world = runner.scene_world.borrow();
    let catalog = runner.asset_catalog.borrow();
    let (_, &bridge) = world
        .entity_registry
        .iter()
        .find(|(id, _)| id.starts_with("bridge_"))
        .expect("the procedural world builds its bridge");
    let renderer = world
        .world
        .get::<&crate::components::MeshRenderer>(bridge)
        .unwrap();
    assert_eq!(
        catalog
            .mesh_name_for_handle(renderer.mesh_handle)
            .as_deref(),
        Some("runtime:bridge")
    );
    let mesh = catalog.mesh_data(renderer.mesh_handle).unwrap();
    // 13 arch segments' edges, two vertices each, two triangles per segment.
    assert_eq!((mesh.vertices.len(), mesh.indices.len()), (26, 72));
    assert_eq!(mesh.vertices[0].position, [-4.0, 0.0, -0.75]);
}
