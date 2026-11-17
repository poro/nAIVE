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
fn headless_deferred_spawns_reuse_cooking_and_respect_mesh_scale_and_world_reset() {
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
    assert_eq!(
        (stats.mesh_cooks, stats.cache_hits, stats.cached_meshes),
        (2, 1, 2)
    );
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
    assert_eq!((stats.mesh_cooks, stats.cache_hits), (2, 2));
    assert!(runner.take_errors().is_empty());

    runner.load_scene("scene.yaml").unwrap();
    let physics = runner.physics_world.borrow();
    let stats = physics.collider_cooking_stats();
    assert_eq!(
        (stats.mesh_cooks, stats.cache_hits, stats.cached_meshes),
        (1, 0, 1)
    );
    assert_eq!(physics.collider_set.len(), 1);
}

#[test]
fn splat_entities_with_trimesh_colliders_take_their_shape_from_the_authored_mesh() {
    let project = Project::new();
    project.write(
        "cloud.ply",
        "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n0 0 0\n",
    );
    project.write(
        "scene.yaml",
        "name: Splat\nentities:\n  - id: statue\n    components:\n      transform: {position: [0,0,0], scale: [4,1,4]}\n      gaussian_splat: {source: cloud.ply}\n      mesh_renderer: {mesh: 'procedural:plane', material: 'procedural:default'}\n      collider: {shape: trimesh}\n",
    );
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    {
        let world = runner.scene_world.borrow();
        let entity = world.entity_registry["statue"];
        // The splat draws; the mesh only shapes the collider.
        assert!(world
            .world
            .get::<&crate::components::GaussianSplat>(entity)
            .is_ok());
        assert!(world
            .world
            .get::<&crate::components::MeshRenderer>(entity)
            .is_err());
    }
    let (_, distance, _) = runner
        .physics_world
        .borrow()
        .raycast(glam::Vec3::new(1.5, 2.0, 0.0), glam::Vec3::NEG_Y, 5.0)
        .expect("the scaled plane collider is there before any physics step");
    assert!((distance - 2.0).abs() < 1e-3, "{distance}");
}

#[test]
fn init_can_raycast_against_the_scene_it_belongs_to() {
    let project = Project::new();
    project.write(
        "probe.lua",
        "function init()\n  local hit, distance = physics.raycast(0, 5, 0, 0, -1, 0, 10)\n  _G.probe = {hit = hit, distance = distance}\nend\n",
    );
    project.write(
        "scene.yaml",
        "name: Probe\nentities:\n  - id: floor\n    components:\n      transform: {position: [0, 0, 0]}\n      collider: {shape: box, half_extents: [5, 0.5, 5]}\n  - id: probe\n    components:\n      transform: {}\n      script: {source: probe.lua}\n",
    );
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    let probe: mlua::Table = runner.script_runtime.lua.globals().get("probe").unwrap();
    assert!(probe.get::<bool>("hit").unwrap());
    assert!((probe.get::<f32>("distance").unwrap() - 4.5).abs() < 1e-4);
}

#[test]
fn both_sides_of_a_trigger_overlap_hear_about_it() {
    let project = Project::new();
    project.write(
        "log.lua",
        "local function log(kind, other)\n  _G.trigger_log = _G.trigger_log or {}\n  table.insert(_G.trigger_log, _entity_string_id .. ' ' .. kind .. ' ' .. other)\nend\nfunction on_trigger_enter(other) log('enter', other) end\nfunction on_trigger_exit(other) log('exit', other) end\n",
    );
    project.write(
        "scene.yaml",
        "name: Portal\nentities:\n  - id: portal\n    components:\n      transform: {position: [0, 0, 0]}\n      collider: {shape: box, half_extents: [1, 0.25, 1], is_trigger: true}\n      script: {source: log.lua}\n  - id: player\n    components:\n      transform: {position: [0, 2, 0]}\n      rigid_body: {type: dynamic}\n      collider: {shape: sphere, radius: 0.25}\n      script: {source: log.lua}\n",
    );
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.step_seconds(1.5);
    assert!(runner.take_errors().is_empty());
    let log: Vec<String> = runner
        .script_runtime
        .lua
        .globals()
        .get::<mlua::Table>("trigger_log")
        .unwrap()
        .sequence_values()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        log,
        [
            "portal enter player",
            "player enter portal",
            "portal exit player",
            "player exit portal",
        ]
    );
}
