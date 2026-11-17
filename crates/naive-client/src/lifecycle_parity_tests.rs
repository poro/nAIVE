//! Native asset/audio adapters and the shared runner exercise the same hooks.

use crate::audio::AudioSystem;
use crate::components::{Health, MeshRenderer};
use crate::events::EventBus;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::particles::ParticleSystem;
use crate::physics::{PhysicsWorld, RigidBody};
use crate::scripting::ScriptRuntime;
use crate::splat::SplatCache;
use crate::world::{spawn_entity_native, EntityCommandQueue, EntityPoolManager, SceneWorld};
use mlua::Table;
use naive_sim::sim;
use naive_sim::test_runner::TestRunner;
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "naive-native-lifecycle-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("scene.yaml"),
            r#"
name: Shared lifecycle
settings: {gravity: [0, 0, 0]}
entities:
  - id: actor
    components:
      mesh_renderer: {mesh: 'procedural:cube', material: 'procedural:default'}
      rigid_body: {type: dynamic, mass: 1}
      collider: {shape: sphere, radius: 0.25}
      health: {max: 60}
      script: {source: lifecycle.lua}
  - id: sensor
    components:
      collider: {shape: box, half_extents: [2, 2, 2], is_trigger: true}
      health: {max: 100}
      script: {source: lifecycle.lua}
"#,
        )
        .unwrap();
        std::fs::write(
            path.join("lifecycle.lua"),
            r#"
local function record(value)
    table.insert(game.trace[_entity_string_id], value)
end

function init()
    game.trace = game.trace or {}
    assert(game.trace[_entity_string_id] == nil, 'init repeated')
    game.trace[_entity_string_id] = {}
    record('init')
    assert(entity.exists(_entity_string_id))
    if _entity_string_id == 'actor' then
        physics.set_velocity('actor', 0.25, 0, 0)
        local vx, vy, vz = physics.get_velocity('actor')
        assert(vx == 0.25 and vy == 0 and vz == 0, 'body missing during init')
        physics.set_velocity('actor', 0, 0, 0)
        particles.spawn_burst(1, 2, 3, 2, {
            lifetime_min=10, lifetime_max=10, speed_min=0, speed_max=0,
            size_start=0.2, size_end=0.2, r=0.1, g=0.2, b=0.3, a=1,
            gravity_scale=0
        })
    end
    audio.play_sfx(_entity_string_id, 'missing-sfx.wav', 0.5)
    audio.play_music('missing-music.wav', 0.25, 0)
    events.on('ping', function()
        assert(entity.exists(_entity_string_id), 'dead listener survived')
        record('ping')
        local current, maximum = entity.get_health(_entity_string_id)
        entity.set_health(_entity_string_id, current - 1, maximum)
        if _entity_string_id == 'actor' then
            events.on('nested', function() error('nested dead listener survived') end)
        end
    end)
end

function on_trigger_enter(other)
    assert(_entity_string_id == 'sensor' and other == 'actor')
    assert(entity.exists(other))
    record('enter:' .. other)
end

function on_trigger_exit(other)
    assert(_entity_string_id == 'sensor' and other == 'actor')
    assert(entity.exists(other), 'removed actor ID unavailable during exit')
    assert(entity.get_health(other) == 57, 'exit preceded on_destroy')
    local x, y, z = entity.get_position(other)
    assert(x == 0 and y == 0 and z == 0)
    entity.set_health('sensor', 80, 100)
    record('exit:' .. other)
end

function on_destroy()
    assert(_entity_string_id == 'actor' and entity.exists('actor'))
    record('destroy')
    entity.set_health('actor', 57, 60)
    physics.set_velocity('actor', 0.5, 0, 0)
    assert(physics.get_velocity('actor') == 0.5, 'body removed before on_destroy')
    events.on('ping', function() error('on_destroy listener survived') end)
    audio.stop_sound('actor', 0)
    audio.stop_music(0)
end
"#,
        )
        .unwrap();
        Self(path)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn trace(runtime: &ScriptRuntime) -> serde_json::Value {
    let game: Table = runtime.lua.globals().get("game").unwrap();
    let trace: Table = game.get("trace").unwrap();
    let entries = |id| {
        trace
            .get::<Table>(id)
            .unwrap()
            .sequence_values::<String>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    json!({"actor": entries("actor"), "sensor": entries("sensor")})
}

fn lua(runtime: &ScriptRuntime, source: &str) {
    runtime
        .run_limited(|| runtime.lua.load(source).exec())
        .unwrap();
}

#[test]
#[ignore = "requires native GPU/Vulkan"]
fn native_lifecycle_matches_headless_runner() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("no GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    let project = Project::new();
    let scene = crate::scene::load_scene(&project.0.join("scene.yaml")).unwrap();
    let world = Rc::new(RefCell::new(SceneWorld::new()));
    let physics = Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::ZERO)));
    let mut meshes = MeshCache::new();
    let mut materials = MaterialCache::new();
    let mut splats = SplatCache::new();
    // The native and headless worlds must not rely on equal cache handles.
    meshes
        .get_or_load(&device, &queue, &project.0, "procedural:plane", None)
        .unwrap();
    for definition in &scene.entities {
        spawn_entity_native(
            &mut world.borrow_mut(),
            definition,
            &device,
            &queue,
            &project.0,
            &mut meshes,
            &mut materials,
            &mut splats,
            Some(&mut physics.borrow_mut()),
            None,
            None,
        )
        .unwrap();
    }
    world.borrow_mut().current_scene = Some(scene);
    let bus = Rc::new(RefCell::new(EventBus::new(100)));
    let listeners = Rc::new(RefCell::new(HashMap::new()));
    let listener_ids = Rc::new(RefCell::new(HashMap::new()));
    let particles = Rc::new(RefCell::new(ParticleSystem::new()));
    let mut runtime = ScriptRuntime::new();
    runtime.register_api().unwrap();
    runtime
        .register_entity_api(world.clone(), physics.clone())
        .unwrap();
    runtime
        .register_physics_api(physics.clone(), world.clone())
        .unwrap();
    runtime
        .register_entity_command_api(
            world.clone(),
            Rc::new(RefCell::new(EntityCommandQueue::new())),
            Rc::new(RefCell::new(EntityPoolManager::new())),
        )
        .unwrap();
    runtime
        .register_event_api(
            bus.clone(),
            listeners.clone(),
            Rc::new(RefCell::new(0)),
            listener_ids.clone(),
        )
        .unwrap();
    runtime
        .register_particle_api(world.clone(), particles.clone())
        .unwrap();
    runtime
        .register_audio_api(
            Rc::new(RefCell::new(AudioSystem::silent())),
            project.0.clone(),
        )
        .unwrap();
    sim::initialize_pending_scripts(&world, &mut runtime, &project.0).unwrap();
    sim::initialize_pending_scripts(&world, &mut runtime, &project.0).unwrap();
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    let initial = json!({"actor": ["init"], "sensor": ["init"]});
    assert_eq!(trace(&runtime), initial);
    assert_eq!(trace(&runner.script_runtime), initial);
    let actor = world.borrow().entity_registry["actor"];
    let headless_actor = runner.scene_world.borrow().entity_registry["actor"];
    let native_mesh = world
        .borrow()
        .world
        .get::<&MeshRenderer>(actor)
        .unwrap()
        .mesh_handle;
    let headless_mesh = runner
        .scene_world
        .borrow()
        .world
        .get::<&MeshRenderer>(headless_actor)
        .unwrap()
        .mesh_handle;
    assert_ne!(native_mesh, headless_mesh);
    assert!(meshes.get(native_mesh).vertex_buffer.size() > 0);
    assert!(meshes.get(native_mesh).index_buffer.size() > 0);
    // Compare the native POD adapter before either simulation advances time.
    assert_eq!(particles.borrow().particle_count(), 2);
    assert_eq!(runner.particle_system.borrow().particle_count(), 2);
    let native_vertices = particles
        .borrow()
        .collect_billboard_data(glam::Vec3::X, glam::Vec3::Y);
    let headless_vertices = runner
        .particle_system
        .borrow()
        .collect_billboard_data(glam::Vec3::X, glam::Vec3::Y);
    assert_eq!(native_vertices.len(), 12);
    assert_eq!(native_vertices[0].position, [0.9, 1.9, 3.]);
    assert_eq!(native_vertices[0].color, [0.1, 0.2, 0.3, 1.]);
    assert_eq!(
        native_vertices
            .iter()
            .map(|v| (v.position, v.color))
            .collect::<Vec<_>>(),
        headless_vertices
            .iter()
            .map(|v| (v.position, v.color))
            .collect::<Vec<_>>()
    );
    let actor_body = world
        .borrow()
        .world
        .get::<&RigidBody>(actor)
        .unwrap()
        .handle;
    let headless_body = runner
        .scene_world
        .borrow()
        .world
        .get::<&RigidBody>(headless_actor)
        .unwrap()
        .handle;

    // These are lifecycle entry points, not a duplicate native frame loop.
    sim::run_fixed_steps(&physics, &world, Some(&runtime), runner.delta_time);
    sim::dispatch_physics_events(&world, &physics, &runtime);
    runner.step_frame();
    lua(&runtime, "events.emit('ping', {})");
    sim::dispatch_lua_events(&bus, &listeners, Some(&runtime), 0.0);
    lua(&runner.script_runtime, "events.emit('ping', {})");
    runner.step_frame();
    let before_destroy = json!({
        "actor": ["init", "ping"],
        "sensor": ["init", "enter:actor", "ping"]
    });
    assert_eq!(trace(&runtime), before_destroy);
    assert_eq!(trace(&runner.script_runtime), before_destroy);
    assert!(sim::destroy_entity(
        &world,
        Some(&physics),
        Some(&mut runtime),
        "actor"
    ));
    assert!(!sim::destroy_entity(
        &world,
        Some(&physics),
        Some(&mut runtime),
        "actor"
    ));
    lua(
        &runner.script_runtime,
        "entity.destroy('actor'); entity.destroy('actor')",
    );
    runner.step_frame();
    lua(
        &runtime,
        "events.emit('ping', {}); events.emit('nested', {})",
    );
    sim::dispatch_lua_events(&bus, &listeners, Some(&runtime), 0.0);
    lua(
        &runner.script_runtime,
        "events.emit('ping', {}); events.emit('nested', {})",
    );
    runner.step_frame();
    let expected = json!({
        "actor": ["init", "ping", "destroy"],
        "sensor": ["init", "enter:actor", "ping", "exit:actor", "ping"]
    });
    assert_eq!(trace(&runtime), expected);
    assert_eq!(trace(&runner.script_runtime), expected);
    for (scene, physics, runtime, retired, body) in [
        (&world, &physics, &runtime, actor, actor_body),
        (
            &runner.scene_world,
            &runner.physics_world,
            &runner.script_runtime,
            headless_actor,
            headless_body,
        ),
    ] {
        let scene = scene.borrow();
        assert_eq!(scene.entity_registry.len(), 1);
        assert!(!scene.world.contains(retired));
        let sensor = scene.entity_registry["sensor"];
        assert_eq!(scene.world.get::<&Health>(sensor).unwrap().current, 79.);
        assert_eq!(physics.borrow().rigid_body_set.len(), 1);
        assert_eq!(physics.borrow().collider_set.len(), 1);
        assert!(!physics.borrow().rigid_body_set.contains(body));
        assert!(!runtime.entity_envs.contains_key(&retired));
        assert!(!runtime.script_sources.contains_key(&retired));
        assert_eq!(runtime.entity_envs.len(), 1);
        assert!(runtime.take_errors().is_empty());
    }
    assert_eq!(listener_ids.borrow().len(), 1);
    assert_eq!(listeners.borrow().values().map(Vec::len).sum::<usize>(), 1);
    assert!(runner.take_errors().is_empty());
    let native_snapshot = crate::scene_snapshot::snapshot_scene(
        &world.borrow(),
        &meshes,
        &materials,
        Some(&physics.borrow()),
    )
    .unwrap();
    let headless_snapshot = naive_sim::scene_snapshot::snapshot_scene(
        &runner.scene_world.borrow(),
        &*runner.asset_catalog.borrow(),
        Some(&runner.physics_world.borrow()),
    )
    .unwrap();
    assert_eq!(native_snapshot.entities[0].id, "sensor");
    assert_eq!(
        native_snapshot.entities[0]
            .components
            .health
            .as_ref()
            .unwrap()
            .current,
        Some(79.)
    );
    assert_eq!(
        serde_norway::to_string(&native_snapshot).unwrap(),
        serde_norway::to_string(&headless_snapshot).unwrap()
    );
}
