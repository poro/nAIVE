use super::*;
use crate::physics::{
    from_rapier_rot, from_rapier_vec, CharacterController, Collider, PhysicsShape, RigidBody,
};
use crate::scripting::{Script, ScriptRuntime};
use glam::Vec3;
use mlua::Table;
use naive_sim::assets::AssetCatalog;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("naive-world-parity-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, path: &str, source: &str) {
        std::fs::write(self.0.join(path), source).unwrap();
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// GPU caches can have unrelated handle numbering and can fail during upload.
/// This adapter exercises those properties while retaining real CPU validation.
struct NativeLikeResolver {
    catalog: AssetCatalog,
    calls: Vec<&'static str>,
    fail_on: Option<&'static str>,
    corrupt_geometry: Option<&'static str>,
}

impl NativeLikeResolver {
    fn new(root: &Path) -> Self {
        Self {
            catalog: AssetCatalog::new(root),
            calls: Vec::new(),
            fail_on: None,
            corrupt_geometry: None,
        }
    }

    fn request(&mut self, kind: &'static str) -> Result<(), String> {
        self.calls.push(kind);
        if self.fail_on == Some(kind) {
            Err(format!("native-like {kind} upload failed"))
        } else {
            Ok(())
        }
    }
}

impl SpawnAssetResolver for NativeLikeResolver {
    fn mesh(&mut self, path: &str) -> Result<MeshHandle, String> {
        self.request("mesh")?;
        self.catalog
            .mesh(path)
            .map(|handle| MeshHandle(handle.0 + 100))
            .map_err(|error| error.to_string())
    }

    fn material(&mut self, path: &str) -> Result<MaterialHandle, String> {
        self.request("material")?;
        self.catalog
            .material(path)
            .map(|handle| MaterialHandle(handle.0 + 200))
            .map_err(|error| error.to_string())
    }

    fn splat(&mut self, path: &str) -> Result<SplatHandle, String> {
        self.request("splat")?;
        self.catalog
            .validate_splat(path)
            .map(|handle| SplatHandle(handle.0 + 300))
            .map_err(|error| error.to_string())
    }

    fn trimesh(
        &mut self,
        mesh: MeshHandle,
        scale: Vec3,
    ) -> Result<crate::mesh::PhysicsTrimesh, String> {
        self.request("trimesh")?;
        let index = mesh.0.checked_sub(100).ok_or("foreign mesh handle")?;
        let mut geometry = self
            .catalog
            .mesh_data(MeshHandle(index))
            .ok_or("missing native-like CPU mesh")?
            .physics_trimesh(scale);
        match self.corrupt_geometry {
            Some("empty") => geometry.0.clear(),
            Some("indices") => geometry.1[0][0] = geometry.0.len() as u32,
            Some("nonfinite") => geometry.0[0].x = f32::NAN,
            _ => {}
        }
        Ok(geometry)
    }
}

fn actor() -> EntityDef {
    serde_norway::from_str(r#"
id: actor
tags: [hero, scripted]
components:
  transform: {position: [2, 4, 6], rotation: [15, 25, 5], scale: [2, 3, 4]}
  mesh_renderer: {mesh: 'procedural:cube', material: 'procedural:default'}
  camera: {fov: 62, near: 0.2, far: 250, mode: third_person, distance: 6, height_offset: 2, pitch_limits: [-35, 65]}
  point_light: {color: [0.2, 0.4, 0.6], intensity: 8, range: 30}
  directional_light: {direction: [0, -2, -1], color: [0.6, 0.5, 0.4], intensity: 3, shadow_extent: 40}
  script: {source: actor.lua}
  health: {max: 120, current: 75}
  collision_damage: {damage: 17, destroy_on_hit: true}
  particle_emitter: {max_particles: 30, spawn_rate: 12, lifetime: [0.3, 0.9], enabled: false}
  rigid_body: {body_type: dynamic, mass: 2.5, ccd: true}
  collider: {shape: box, half_extents: [0.2, 0.3, 0.4], is_trigger: true, restitution: 0.6, friction: 0.2}
"#).unwrap()
}

fn plane_actor() -> EntityDef {
    let mut definition = actor();
    let components = &mut definition.components;
    let transform = components.transform.as_mut().unwrap();
    transform.position = [0.0; 3];
    transform.rotation = [0.0; 3];
    transform.scale = [4.0, 1.0, 6.0];
    components.mesh_renderer.as_mut().unwrap().mesh = "procedural:plane".into();
    components.rigid_body = None;
    components.collider.as_mut().unwrap().shape = "trimesh".into();
    components.collider.as_mut().unwrap().is_trigger = false;
    definition
}

fn shape_snapshot(shape: &PhysicsShape) -> Value {
    match shape {
        PhysicsShape::Box { half_extents } => json!({"box": half_extents.to_array()}),
        PhysicsShape::Sphere { radius } => json!({"sphere": radius}),
        PhysicsShape::Capsule {
            half_height,
            radius,
        } => json!({"capsule": [half_height, radius]}),
        PhysicsShape::Trimesh { vertices, indices } => json!({
            "vertices": vertices.iter().map(|v| [v.x, v.y, v.z]).collect::<Vec<_>>(),
            "indices": indices,
        }),
    }
}

/// Compare gameplay state without comparing backend-specific asset handles.
fn snapshot(scene: &SceneWorld, physics: &PhysicsWorld, id: &str) -> Value {
    let entity = scene.entity_registry[id];
    let transform = scene.world.get::<&Transform>(entity).unwrap();
    let mut value = json!({
        "id": scene.world.get::<&EntityId>(entity).unwrap().0,
        "tags": scene.world.get::<&Tags>(entity).unwrap().0,
        "position": transform.position.to_array(),
        "rotation": transform.rotation.to_array(),
        "scale": transform.scale.to_array(),
        "world_matrix": transform.world_matrix.to_cols_array_2d(),
        "dirty": transform.dirty,
        "authored": scene.world.get::<&SceneEntityDefinition>(entity).unwrap().0,
        "mesh": scene.world.get::<&MeshRenderer>(entity).is_ok(),
        "splat": scene.world.get::<&GaussianSplat>(entity).is_ok(),
    });
    if let Ok(script) = scene.world.get::<&Script>(entity) {
        value["script"] = json!([script.source.to_string_lossy(), json!(script.initialized)]);
    }
    if let Ok(camera) = scene.world.get::<&Camera>(entity) {
        value["camera"] = json!({
            "fov": camera.fov_degrees, "near": camera.near, "far": camera.far,
            "role": format!("{:?}", camera.role), "aspect": camera.aspect_ratio,
        });
    }
    if let Ok(mode) = scene.world.get::<&CameraMode>(entity) {
        value["camera_mode"] = json!(format!("{:?}", *mode));
    }
    if let Ok(light) = scene.world.get::<&PointLight>(entity) {
        value["point_light"] = json!([
            json!(light.color.to_array()),
            json!(light.intensity),
            json!(light.range)
        ]);
    }
    if let Ok(light) = scene.world.get::<&DirectionalLight>(entity) {
        value["directional_light"] = json!({
            "direction": light.direction.to_array(), "color": light.color.to_array(),
            "intensity": light.intensity, "shadow_extent": light.shadow_extent,
        });
    }
    if let Ok(health) = scene.world.get::<&Health>(entity) {
        value["health"] =
            json!({"current": health.current, "max": health.max, "dead": health.dead});
    }
    if let Ok(damage) = scene.world.get::<&CollisionDamage>(entity) {
        value["collision_damage"] =
            json!({"damage": damage.damage, "destroy": damage.destroy_on_hit});
    }
    if let Ok(emitter) = scene.world.get::<&ParticleEmitter>(entity) {
        let config = &emitter.config;
        value["emitter"] = json!({
            "enabled": emitter.enabled, "max_particles": config.max_particles,
            "rate": config.spawn_rate, "lifetime": config.lifetime,
            "speed": config.initial_speed, "direction": config.direction.to_array(),
            "spread": config.spread, "size": config.size, "start": config.color_start,
            "end": config.color_end, "gravity": config.gravity_scale,
        });
    }
    if let Ok(swarm) = scene.world.get::<&GpuSwarm>(entity) {
        value["swarm"] = json!(swarm.def);
    }
    if let Ok(controller) = scene.world.get::<&CharacterController>(entity) {
        value["controller"] = json!({
            "speed": controller.move_speed, "sprint": controller.sprint_multiplier,
            "jump": controller.jump_impulse, "step": controller.step_height,
            "grounded": controller.grounded, "velocity": controller.velocity.to_array(),
        });
    }
    if let Ok(player) = scene.world.get::<&Player>(entity) {
        value["player"] = json!({"height": player.height, "radius": player.radius, "yaw": player.yaw, "pitch": player.pitch});
    }
    if let Ok(projectile) = scene.world.get::<&Projectile>(entity) {
        value["projectile"] = json!({
            "damage": projectile.damage, "lifetime": projectile.lifetime,
            "age": projectile.age, "owner": projectile.owner_id,
        });
    }
    if let Ok(component) = scene.world.get::<&RigidBody>(entity) {
        let body = &physics.rigid_body_set[component.handle];
        value["body"] = json!({
            "type": format!("{:?}", component.body_type), "ccd": body.is_ccd_enabled(),
            "position": from_rapier_vec(body.translation()).to_array(),
            "rotation": from_rapier_rot(body.position().rotation).to_array(),
            "velocity": from_rapier_vec(body.linvel()).to_array(),
            "gravity": body.gravity_scale(),
        });
    }
    if let Ok(component) = scene.world.get::<&Collider>(entity) {
        let collider = &physics.collider_set[component.handle];
        value["collider"] = json!({
            "shape": shape_snapshot(&component.shape), "trigger": component.is_trigger,
            "sensor": collider.is_sensor(), "mass": collider.mass(),
            "friction": collider.friction(), "restitution": collider.restitution(),
            "collision_types": collider.active_collision_types().bits(),
        });
    }
    value
}

struct Pair {
    native: SceneWorld,
    headless: SceneWorld,
    native_physics: PhysicsWorld,
    headless_physics: PhysicsWorld,
    native_assets: NativeLikeResolver,
    headless_assets: AssetCatalog,
}

impl Pair {
    fn new(root: &Path) -> Self {
        Self {
            native: SceneWorld::new(),
            headless: SceneWorld::new(),
            native_physics: PhysicsWorld::new(Vec3::ZERO),
            headless_physics: PhysicsWorld::new(Vec3::ZERO),
            native_assets: NativeLikeResolver::new(root),
            headless_assets: AssetCatalog::new(root),
        }
    }

    fn spawn(&mut self, definition: &EntityDef) -> (hecs::Entity, hecs::Entity) {
        let native = spawn_entity_with_resolver(
            &mut self.native,
            definition,
            Some(&mut self.native_physics),
            &mut self.native_assets,
        )
        .unwrap();
        spawn_entity_headless_with_assets(
            &mut self.headless,
            definition,
            &mut self.headless_physics,
            &mut self.headless_assets,
        )
        .unwrap();
        let headless = self.headless.entity_registry[&definition.id];
        self.assert_parity(&definition.id);
        (native, headless)
    }

    fn assert_parity(&self, id: &str) {
        assert_eq!(
            snapshot(&self.native, &self.native_physics, id),
            snapshot(&self.headless, &self.headless_physics, id)
        );
    }
}

#[test]
fn renderer_handles_do_not_change_gameplay_components_or_physics_options() {
    let project = Project::new();
    for kind in ["static", "kinematic", "dynamic"] {
        let mut pair = Pair::new(&project.0);
        let mut definition = actor();
        definition.components.rigid_body.as_mut().unwrap().body_type = kind.into();
        let (native, headless) = pair.spawn(&definition);
        let state = snapshot(&pair.native, &pair.native_physics, "actor");
        assert_eq!(state["id"], "actor");
        assert_eq!(state["tags"], json!(["hero", "scripted"]));
        assert_eq!(state["position"], json!([2.0, 4.0, 6.0]));
        assert_eq!(state["scale"], json!([2.0, 3.0, 4.0]));
        assert_eq!(state["authored"], json!(definition));
        assert_eq!(state["script"], json!(["actor.lua", false]));
        assert_eq!(
            state["health"],
            json!({"current":75.0,"max":120.0,"dead":false})
        );
        assert_eq!(state["camera"]["fov"], 62.0);
        assert_eq!(state["point_light"][1], 8.0);
        assert_eq!(state["directional_light"]["shadow_extent"], 40.0);
        assert_eq!(state["collision_damage"]["damage"], 17.0);
        assert_eq!(state["emitter"]["enabled"], false);
        assert_eq!(state["collider"]["sensor"], true);
        assert_eq!(
            state["collider"]["collision_types"],
            json!(rapier3d::prelude::ActiveCollisionTypes::all().bits())
        );
        if kind == "dynamic" {
            assert_eq!(state["collider"]["mass"], 2.5);
            assert_eq!(state["body"]["ccd"], true);
        }
        assert_eq!(
            pair.native
                .world
                .get::<&MeshRenderer>(native)
                .unwrap()
                .mesh_handle,
            MeshHandle(100)
        );
        assert_eq!(
            pair.headless
                .world
                .get::<&MeshRenderer>(headless)
                .unwrap()
                .mesh_handle,
            MeshHandle(0)
        );
        assert_eq!(pair.native_physics.rigid_body_set.len(), 1);
        assert_eq!(pair.native_physics.collider_to_entity.len(), 1);
    }
}

#[test]
fn splats_and_swarms_keep_independent_script_health_camera_and_light_components() {
    let project = Project::new();
    let mut splat = actor();
    splat.components.gaussian_splat = Some(crate::scene::GaussianSplatDef {
        source: "procedural".into(),
    });
    splat.components.mesh_renderer.as_mut().unwrap().mesh = "missing-unused.glb".into();
    let mut pair = Pair::new(&project.0);
    let (native, headless) = pair.spawn(&splat);
    assert_eq!(pair.native_assets.calls, ["splat"]);
    assert!(pair.native.world.get::<&MeshRenderer>(native).is_err());
    assert_eq!(
        pair.native
            .world
            .get::<&GaussianSplat>(native)
            .unwrap()
            .splat_handle,
        SplatHandle(300)
    );
    assert_eq!(
        pair.headless
            .world
            .get::<&GaussianSplat>(headless)
            .unwrap()
            .splat_handle,
        SplatHandle(0)
    );
    for state in [
        snapshot(&pair.native, &pair.native_physics, "actor"),
        snapshot(&pair.headless, &pair.headless_physics, "actor"),
    ] {
        assert_eq!(state["splat"], true);
        assert_eq!(state["script"], json!(["actor.lua", false]));
        assert_eq!(state["health"]["current"], 75.0);
        assert_eq!(state["point_light"][1], 8.0);
        assert_eq!(state["camera"]["fov"], 62.0);
    }
    let mut swarm = actor();
    swarm.components.mesh_renderer = None;
    swarm.components.gpu_swarm = Some(
        serde_norway::from_str("count: 4\ninitial: 2\nmesh: procedural:sphere\nseed: 7\n").unwrap(),
    );
    let mut pair = Pair::new(&project.0);
    let (native, _) = pair.spawn(&swarm);
    assert_eq!(
        pair.native
            .world
            .get::<&GpuSwarm>(native)
            .unwrap()
            .def
            .count,
        4
    );
    assert!(pair.native.world.get::<&Script>(native).is_ok());
    assert!(pair.native.world.get::<&Health>(native).is_ok());
    assert!(pair.native.world.get::<&DirectionalLight>(native).is_ok());
}

#[test]
fn character_controller_precedence_and_absent_physics_are_consistent() {
    let project = Project::new();
    let mut definition = actor();
    definition.components.character_controller = Some(serde_norway::from_str(
        "height: 2.2\nradius: 0.4\nmove_speed: 7\nsprint_multiplier: 2\njump_impulse: 9\nstep_height: 0.6\n"
    ).unwrap());
    // This collider has no usable mesh, but the controller owns physics shape.
    definition.components.mesh_renderer = None;
    definition.components.collider.as_mut().unwrap().shape = "trimesh".into();
    let mut pair = Pair::new(&project.0);
    pair.spawn(&definition);
    assert!(pair.native_assets.calls.is_empty());
    let state = snapshot(&pair.native, &pair.native_physics, "actor");
    assert_eq!(state["body"]["type"], "Kinematic");
    assert_eq!(state["body"]["ccd"], false);
    assert_eq!(state["collider"]["sensor"], false);
    assert_eq!(state["controller"]["speed"], 7.0);
    assert_eq!(state["player"]["height"], json!(2.2_f32));
    assert_eq!(state["player"]["radius"], json!(0.4_f32));
    assert_eq!(pair.native_physics.collider_cooking_stats().mesh_cooks, 0);

    let mut native = SceneWorld::new();
    let mut headless = SceneWorld::new();
    spawn_entity_with_resolver(&mut native, &definition, None, &mut pair.native_assets).unwrap();
    spawn_entity_with_resolver(&mut headless, &definition, None, &mut pair.headless_assets)
        .unwrap();
    assert_eq!(
        snapshot(&native, &pair.native_physics, "actor"),
        snapshot(&headless, &pair.headless_physics, "actor")
    );
    for scene in [&native, &headless] {
        let entity = scene.entity_registry["actor"];
        assert!(scene.world.get::<&RigidBody>(entity).is_err());
        assert!(scene.world.get::<&CharacterController>(entity).is_err());
        assert!(scene.world.get::<&Health>(entity).is_ok());
        assert!(scene.world.get::<&Script>(entity).is_ok());
    }
}

#[test]
fn scaled_trimeshes_use_resolver_geometry_and_share_cooking_for_repeated_instances() {
    let project = Project::new();
    let mut pair = Pair::new(&project.0);
    let mut definition = plane_actor();
    pair.spawn(&definition);
    for physics in [&pair.native_physics, &pair.headless_physics] {
        assert_eq!(physics.collider_cooking_stats().mesh_cooks, 1);
    }
    let shape = pair
        .native
        .world
        .get::<&Collider>(pair.native.entity_registry["actor"])
        .unwrap();
    let PhysicsShape::Trimesh { vertices, indices } = &shape.shape else {
        panic!("expected mesh triangles")
    };
    assert!(!indices.is_empty());
    assert!(vertices.iter().any(|v| (v.x.abs() - 2.0).abs() < 1e-5));
    assert!(vertices.iter().any(|v| (v.z.abs() - 3.0).abs() < 1e-5));
    drop(shape);
    definition.id = "second".into();
    definition.components.transform.as_mut().unwrap().position = [10.0, 0.0, 0.0];
    pair.spawn(&definition);
    for physics in [&mut pair.native_physics, &mut pair.headless_physics] {
        assert_eq!(physics.collider_cooking_stats().mesh_cooks, 1);
        // Preparation and construction each find the shape cooked for the
        // first instance.
        assert_eq!(physics.collider_cooking_stats().cache_hits, 3);
        physics.step_fixed();
        let (_, distance, _) = physics
            .raycast(Vec3::new(11.5, 2.0, 0.0), Vec3::NEG_Y, 4.0)
            .unwrap();
        assert!((distance - 2.0).abs() < 1e-4);
    }
}

#[test]
fn duplicate_and_asset_failures_leave_existing_entities_and_physics_untouched() {
    let project = Project::new();
    let mut pair = Pair::new(&project.0);
    let definition = actor();
    pair.spawn(&definition);
    let before = snapshot(&pair.native, &pair.native_physics, "actor");
    let calls = pair.native_assets.calls.len();
    let mut duplicate = definition.clone();
    duplicate.components.transform.as_mut().unwrap().position = [99.0; 3];
    assert!(spawn_entity_with_resolver(
        &mut pair.native,
        &duplicate,
        Some(&mut pair.native_physics),
        &mut pair.native_assets
    )
    .is_err());
    assert!(spawn_entity_headless_with_assets(
        &mut pair.headless,
        &duplicate,
        &mut pair.headless_physics,
        &mut pair.headless_assets
    )
    .is_err());
    assert_eq!(pair.native_assets.calls.len(), calls);
    assert_eq!(
        snapshot(&pair.native, &pair.native_physics, "actor"),
        before
    );
    pair.assert_parity("actor");

    for failure in ["mesh", "material", "splat", "trimesh"] {
        let mut incoming = if failure == "trimesh" {
            plane_actor()
        } else {
            actor()
        };
        incoming.id = format!("bad_{failure}");
        if failure == "splat" {
            incoming.components.gaussian_splat = Some(crate::scene::GaussianSplatDef {
                source: "procedural".into(),
            });
        }
        pair.native_assets.fail_on = Some(failure);
        let stats = pair.native_physics.collider_cooking_stats();
        let error = spawn_entity_with_resolver(
            &mut pair.native,
            &incoming,
            Some(&mut pair.native_physics),
            &mut pair.native_assets,
        )
        .unwrap_err();
        assert!(
            error.contains(&incoming.id) && error.contains(failure),
            "{error}"
        );
        pair.native_assets.fail_on = None;
        match failure {
            "mesh" | "trimesh" => {
                incoming.components.mesh_renderer.as_mut().unwrap().mesh = "missing.glb".into()
            }
            "material" => {
                incoming.components.mesh_renderer.as_mut().unwrap().material = "missing.yaml".into()
            }
            "splat" => {
                incoming.components.gaussian_splat.as_mut().unwrap().source = "missing.ply".into()
            }
            _ => unreachable!(),
        }
        assert!(spawn_entity_headless_with_assets(
            &mut pair.headless,
            &incoming,
            &mut pair.headless_physics,
            &mut pair.headless_assets
        )
        .is_err());
        assert_eq!(pair.native.entity_registry.len(), 1);
        assert_eq!(pair.headless.entity_registry.len(), 1);
        assert_eq!(pair.native.world.len(), 1);
        assert_eq!(pair.headless.world.len(), 1);
        for physics in [&pair.native_physics, &pair.headless_physics] {
            assert_eq!(physics.rigid_body_set.len(), 1);
            assert_eq!(physics.collider_set.len(), 1);
            assert_eq!(physics.body_to_entity.len(), 1);
            assert_eq!(physics.collider_to_entity.len(), 1);
        }
        assert_eq!(pair.native_physics.collider_cooking_stats(), stats);
        pair.assert_parity("actor");
    }
}

#[test]
fn invalid_geometry_from_a_resolver_is_rejected_before_entity_or_body_allocation() {
    let project = Project::new();
    for fault in ["empty", "indices", "nonfinite"] {
        let mut resolver = NativeLikeResolver::new(&project.0);
        resolver.corrupt_geometry = Some(fault);
        let mut scene = SceneWorld::new();
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        assert!(spawn_entity_with_resolver(
            &mut scene,
            &plane_actor(),
            Some(&mut physics),
            &mut resolver
        )
        .is_err());
        assert!(scene.entity_registry.is_empty());
        assert_eq!(scene.world.len(), 0);
        assert_eq!(physics.rigid_body_set.len(), 0);
        assert_eq!(physics.collider_set.len(), 0);
        assert_eq!(physics.collider_cooking_stats().mesh_cooks, 0);
    }
}

#[test]
fn runner_projectile_and_dynamic_commands_match_native_like_recipes_and_finishers() {
    let project = Project::new();
    project.write("actor.lua", r#"
function init()
    entity.spawn_projectile('owner', 'procedural:sphere', 'procedural:default', 1,2,3, 0,0,-2, 10,7,4,false)
    entity.spawn_dynamic('procedural:sphere', 'procedural:default', 5,6,7, 2,3,4, 0.75,3,0.6,0.2,9)
end
"#);
    project.write("scene.yaml", "name: Recipes\nentities:\n  - id: owner\n    components:\n      script: {source: actor.lua}\n");
    let mut runner = crate::test_runner::TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.step_frame();

    let projectile = ProjectileSpawnCommand {
        id: "proj_1".into(),
        owner_id: "owner".into(),
        mesh: "procedural:sphere".into(),
        material: "procedural:default".into(),
        position: [1.0, 2.0, 3.0],
        direction: [0.0, 0.0, -2.0],
        speed: 10.0,
        damage: 7.0,
        lifetime: 4.0,
        gravity: false,
        scale: [1.0; 3],
    };
    let dynamic = DynamicSpawnCommand {
        id: "dyn_1".into(),
        mesh: "procedural:sphere".into(),
        material: "procedural:default".into(),
        position: [5.0, 6.0, 7.0],
        velocity: [2.0, 3.0, 4.0],
        scale: [99.0; 3],
        radius: 0.75,
        mass: 3.0,
        restitution: 0.6,
        friction: 0.2,
        lifetime: 9.0,
    };
    let mut scene = SceneWorld::new();
    let mut physics = PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0));
    let mut assets = NativeLikeResolver::new(&project.0);
    let shot = spawn_entity_with_resolver(
        &mut scene,
        &projectile.entity_def(),
        Some(&mut physics),
        &mut assets,
    )
    .unwrap();
    finish_projectile_spawn(&mut scene, &mut physics, shot, &projectile);
    let bouncing = spawn_entity_with_resolver(
        &mut scene,
        &dynamic.entity_def(),
        Some(&mut physics),
        &mut assets,
    )
    .unwrap();
    finish_dynamic_spawn(&mut scene, &mut physics, bouncing, &dynamic);
    // Native rendering and the runner both propagate transforms after the tick.
    crate::transform::update_transforms(&mut scene.world);
    for id in ["proj_1", "dyn_1"] {
        assert_eq!(
            snapshot(&scene, &physics, id),
            snapshot(
                &runner.scene_world.borrow(),
                &runner.physics_world.borrow(),
                id
            )
        );
    }
    let shot = snapshot(&scene, &physics, "proj_1");
    assert_eq!(shot["body"]["velocity"], json!([0.0, 0.0, -10.0]));
    assert_eq!(shot["body"]["gravity"], 0.0);
    assert_eq!(shot["body"]["ccd"], true);
    assert_eq!(shot["projectile"]["owner"], "owner");
    assert_eq!(shot["projectile"]["lifetime"], 4.0);
    let bouncing = snapshot(&scene, &physics, "dyn_1");
    assert_eq!(bouncing["scale"], json!([1.5, 1.5, 1.5]));
    assert_eq!(bouncing["body"]["velocity"], json!([2.0, 3.0, 4.0]));
    assert_eq!(bouncing["collider"]["mass"], 3.0);
    assert_eq!(bouncing["projectile"]["lifetime"], 9.0);
    let body = scene
        .world
        .get::<&RigidBody>(scene.entity_registry["dyn_1"])
        .unwrap()
        .handle;
    assert_eq!(
        physics.rigid_body_set[body]
            .activation()
            .normalized_linear_threshold,
        -1.0
    );
    assert!(runner.take_errors().is_empty());
}

fn lifecycle_trace(mut assets: impl SpawnAssetResolver, root: &Path) -> Value {
    let scene = Rc::new(RefCell::new(SceneWorld::new()));
    let physics = Rc::new(RefCell::new(PhysicsWorld::new(Vec3::ZERO)));
    let mut runtime = ScriptRuntime::new();
    runtime.register_api().unwrap();
    runtime
        .register_entity_api(scene.clone(), physics.clone())
        .unwrap();
    let mut definition = plane_actor();
    let mut trace = Vec::new();
    for generation in 0..3 {
        if generation == 2 {
            *scene.borrow_mut() = SceneWorld::new();
            *physics.borrow_mut() = PhysicsWorld::new(Vec3::ZERO);
        }
        definition.components.transform.as_mut().unwrap().position =
            [generation as f32 * 7.0, 0.0, 0.0];
        definition.components.health.as_mut().unwrap().current = Some(75.0 - generation as f32);
        definition
            .components
            .point_light
            .as_mut()
            .unwrap()
            .intensity = 8.0 + generation as f32;
        spawn_entity_with_resolver(
            &mut scene.borrow_mut(),
            &definition,
            Some(&mut physics.borrow_mut()),
            &mut assets,
        )
        .unwrap();
        crate::sim::initialize_pending_scripts(&scene, &mut runtime, root).unwrap();
        crate::sim::initialize_pending_scripts(&scene, &mut runtime, root).unwrap();
        let game: Table = runtime.lua.globals().get("game").unwrap();
        assert_eq!(game.get::<u32>("inits").unwrap(), generation + 1);
        assert_eq!(game.get::<f32>("seen_x").unwrap(), generation as f32 * 7.0);
        let state = snapshot(&scene.borrow(), &physics.borrow(), "actor");
        assert_eq!(state["health"]["current"], json!(75.0 - generation as f32));
        assert_eq!(state["point_light"][1], json!(8.0 + generation as f32));
        let stats = physics.borrow().collider_cooking_stats();
        assert_eq!(stats.mesh_cooks, 1);
        // Preparation cooks (or finds) the shape and construction finds it; a
        // new physics world starts with an empty cache.
        assert_eq!(stats.cache_hits, if generation == 1 { 3 } else { 1 });
        trace.push(json!({"state": state, "cooks": stats.mesh_cooks, "hits": stats.cache_hits}));
        assert!(crate::sim::destroy_entity(
            &scene,
            Some(&physics),
            Some(&mut runtime),
            "actor"
        ));
        assert!(!crate::sim::destroy_entity(
            &scene,
            Some(&physics),
            Some(&mut runtime),
            "actor"
        ));
        assert_eq!(game.get::<u32>("destroys").unwrap(), generation + 1);
        assert_eq!(
            game.get::<f32>("destroy_x").unwrap(),
            generation as f32 * 7.0
        );
        assert!(scene.borrow().entity_registry.is_empty());
        assert_eq!(physics.borrow().rigid_body_set.len(), 0);
        assert_eq!(physics.borrow().collider_set.len(), 0);
        assert!(runtime.entity_envs.is_empty());
    }
    assert!(runtime.take_errors().is_empty());
    json!(trace)
}

#[test]
fn structural_recreation_and_scene_reset_preserve_lifecycle_and_cache_behavior() {
    let project = Project::new();
    project.write(
        "actor.lua",
        r#"
function init()
    game.inits = (game.inits or 0) + 1
    game.seen_x = entity.get_position('actor')
end
function on_destroy()
    game.destroys = (game.destroys or 0) + 1
    game.destroy_x = entity.get_position('actor')
end
"#,
    );
    assert_eq!(
        lifecycle_trace(NativeLikeResolver::new(&project.0), &project.0),
        lifecycle_trace(AssetCatalog::new(&project.0), &project.0),
    );
}

fn replacement_scene(entities: Vec<EntityDef>) -> SceneFile {
    let mut scene: SceneFile =
        serde_norway::from_str("name: Replacement\nsettings: {gravity: [0, 0, 0]}\nentities: []\n")
            .unwrap();
    scene.entities = entities;
    scene
}

#[test]
fn whole_scene_preflight_checks_duplicates_before_assets_and_rejects_late_asset_failures() {
    let project = Project::new();
    let mut pair = Pair::new(&project.0);
    pair.spawn(&actor());
    let original = snapshot(&pair.native, &pair.native_physics, "actor");
    let mut first = actor();
    first.id = "replacement".into();
    let mut second = actor();
    second.id = "broken".into();
    second.components.mesh_renderer.as_mut().unwrap().material = "missing.yaml".into();
    let invalid_asset = replacement_scene(vec![first.clone(), second]);
    let native_error = preflight_scene(&invalid_asset, &mut pair.native_assets, None)
        .err()
        .unwrap();
    let headless_error = preflight_scene(&invalid_asset, &mut pair.headless_assets, None)
        .err()
        .unwrap();
    assert!(native_error.contains("broken"), "{native_error}");
    assert!(headless_error.contains("broken"), "{headless_error}");

    // Even a bad asset earlier in the file must not hide a duplicate ID.
    let mut duplicate_first = first.clone();
    duplicate_first
        .components
        .mesh_renderer
        .as_mut()
        .unwrap()
        .mesh = "missing.glb".into();
    let duplicate_scene = replacement_scene(vec![duplicate_first, first]);
    let calls = pair.native_assets.calls.len();
    for error in [
        preflight_scene(&duplicate_scene, &mut pair.native_assets, None)
            .err()
            .unwrap(),
        preflight_scene(&duplicate_scene, &mut pair.headless_assets, None)
            .err()
            .unwrap(),
    ] {
        assert!(
            error.contains("duplicate") && error.contains("replacement"),
            "{error}"
        );
    }
    assert_eq!(pair.native_assets.calls.len(), calls);
    assert_eq!(
        snapshot(&pair.native, &pair.native_physics, "actor"),
        original
    );
    pair.assert_parity("actor");
    for scene in [&pair.native, &pair.headless] {
        assert_eq!(scene.entity_registry.len(), 1);
        assert_eq!(scene.world.len(), 1);
    }
    for physics in [&pair.native_physics, &pair.headless_physics] {
        assert_eq!(physics.rigid_body_set.len(), 1);
        assert_eq!(physics.collider_set.len(), 1);
        assert_eq!(physics.body_to_entity.len(), 1);
        assert_eq!(physics.collider_to_entity.len(), 1);
    }
}

#[test]
fn runner_failed_scene_preflight_keeps_previous_entities_physics_cache_and_live_script_vm() {
    let project = Project::new();
    project.write(
        "actor.lua",
        r#"
function init() game.inits = (game.inits or 0) + 1 end
function update(dt) game.updates = (game.updates or 0) + 1 end
function on_destroy() game.destroys = (game.destroys or 0) + 1 end
"#,
    );
    let scene = replacement_scene(vec![plane_actor()]);
    project.write("scene.yaml", &serde_norway::to_string(&scene).unwrap());
    let mut first = actor();
    first.id = "replacement".into();
    let mut broken = first.clone();
    broken.id = "broken".into();
    broken.components.mesh_renderer.as_mut().unwrap().material = "missing.yaml".into();
    project.write(
        "bad_asset.yaml",
        &serde_norway::to_string(&replacement_scene(vec![first.clone(), broken])).unwrap(),
    );
    project.write(
        "duplicate.yaml",
        &serde_norway::to_string(&replacement_scene(vec![first.clone(), first])).unwrap(),
    );

    let mut runner = crate::test_runner::TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.step_frame();
    let entity = runner.scene_world.borrow().entity_registry["actor"];
    let body = runner
        .scene_world
        .borrow()
        .world
        .get::<&RigidBody>(entity)
        .unwrap()
        .handle;
    let collider = runner
        .scene_world
        .borrow()
        .world
        .get::<&Collider>(entity)
        .unwrap()
        .handle;
    let original = snapshot(
        &runner.scene_world.borrow(),
        &runner.physics_world.borrow(),
        "actor",
    );
    let cooking = runner.physics_world.borrow().collider_cooking_stats();
    let game: Table = runner.script_runtime.lua.globals().get("game").unwrap();
    assert_eq!(game.get::<u32>("inits").unwrap(), 1);

    for (path, expected) in [
        ("bad_asset.yaml", "broken"),
        ("duplicate.yaml", "duplicate"),
    ] {
        let updates = game.get::<u32>("updates").unwrap();
        let error = runner.load_scene(path).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(runner.scene_world.borrow().entity_registry["actor"], entity);
        assert_eq!(runner.scene_world.borrow().entity_registry.len(), 1);
        assert_eq!(
            snapshot(
                &runner.scene_world.borrow(),
                &runner.physics_world.borrow(),
                "actor"
            ),
            original
        );
        assert!(runner.physics_world.borrow().rigid_body_set.contains(body));
        assert!(runner
            .physics_world
            .borrow()
            .collider_set
            .contains(collider));
        assert_eq!(
            runner.physics_world.borrow().collider_cooking_stats(),
            cooking
        );
        assert!(runner.script_runtime.entity_envs.contains_key(&entity));
        assert_eq!(game.get::<Option<u32>>("destroys").unwrap(), None);
        let errors = runner.take_errors();
        assert!(errors.iter().any(|recorded| recorded.contains(expected)));
        runner.step_frame();
        // Inspect the original VM's table, proving it is still receiving updates.
        assert_eq!(game.get::<u32>("updates").unwrap(), updates + 1);
        assert_eq!(game.get::<u32>("inits").unwrap(), 1);
        assert!(runner.take_errors().is_empty());
    }
}
