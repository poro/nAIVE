use super::*;
use crate::physics::FIXED_TIMESTEP;
use crate::scripting::{Script, SharedEntityCommandQueue, SharedPhysicsWorld, SharedSceneWorld};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "naive-frame-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.0.join(path), contents).unwrap();
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Execute the engine's relevant shared phases explicitly, independently of
/// TestRunner::step_frame and its deferred-command processing. Fixtures omit
/// controllers, swarms and presentation so those phases have no work to do.
struct EnginePhases {
    world: SharedSceneWorld,
    physics: SharedPhysicsWorld,
    runtime: ScriptRuntime,
    commands: SharedEntityCommandQueue,
    pools: Rc<RefCell<EntityPoolManager>>,
    events: Rc<RefCell<EventBus>>,
    listeners: Rc<RefCell<HashMap<String, Vec<LuaRegistryKey>>>>,
    listener_ids: Rc<RefCell<HashMap<u64, (String, usize)>>>,
    next_listener: Rc<RefCell<u64>>,
    assets: crate::assets::AssetCatalog,
}

impl EnginePhases {
    fn new(project: &Project, scene: &str) -> Self {
        let mut phases = Self {
            world: Rc::new(RefCell::new(SceneWorld::new())),
            physics: Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::ZERO))),
            runtime: ScriptRuntime::new(),
            commands: Rc::new(RefCell::new(EntityCommandQueue::new())),
            pools: Rc::new(RefCell::new(EntityPoolManager::new())),
            events: Rc::new(RefCell::new(EventBus::new(1000))),
            listeners: Rc::new(RefCell::new(HashMap::new())),
            listener_ids: Rc::new(RefCell::new(HashMap::new())),
            next_listener: Rc::new(RefCell::new(0)),
            assets: crate::assets::AssetCatalog::new(&project.0),
        };
        phases.replace_scene(project, scene);
        phases
    }

    fn replace_scene(&mut self, project: &Project, scene: &str) {
        let scene = crate::scene::load_scene(&project.0.join(scene)).unwrap();
        crate::world::preflight_scene(
            &scene,
            &mut self.assets,
            Some(&mut self.physics.borrow_mut()),
        )
        .unwrap();
        let ids: Vec<_> = self
            .world
            .borrow()
            .entity_registry
            .keys()
            .cloned()
            .collect();
        for id in ids {
            assert!(crate::sim::destroy_entity(
                &self.world,
                Some(&self.physics),
                Some(&mut self.runtime),
                &id,
            ));
        }
        assert!(self.runtime.take_errors().is_empty());
        *self.world.borrow_mut() = SceneWorld::new();
        self.physics
            .borrow_mut()
            .reset(scene.settings.gravity.into());
        *self.events.borrow_mut() = EventBus::new(1000);
        *self.commands.borrow_mut() = EntityCommandQueue::new();
        *self.pools.borrow_mut() = EntityPoolManager::new();
        self.listeners.borrow_mut().clear();
        self.listener_ids.borrow_mut().clear();
        *self.next_listener.borrow_mut() = 0;
        for definition in &scene.entities {
            crate::world::spawn_entity_headless_with_assets(
                &mut self.world.borrow_mut(),
                definition,
                &mut self.physics.borrow_mut(),
                &mut self.assets,
            )
            .unwrap();
        }
        self.world.borrow_mut().current_scene = Some(scene);
        self.runtime = ScriptRuntime::new();
        self.runtime.register_api().unwrap();
        self.runtime
            .register_entity_api(self.world.clone(), self.physics.clone())
            .unwrap();
        self.runtime
            .register_physics_api(self.physics.clone(), self.world.clone())
            .unwrap();
        self.runtime
            .register_entity_command_api(
                self.world.clone(),
                self.commands.clone(),
                self.pools.clone(),
            )
            .unwrap();
        self.runtime
            .register_event_api(
                self.events.clone(),
                self.listeners.clone(),
                self.next_listener.clone(),
                self.listener_ids.clone(),
            )
            .unwrap();
        crate::sim::initialize_pending_scripts(&self.world, &mut self.runtime, &project.0).unwrap();
        self.events
            .borrow_mut()
            .record("lifecycle.scene_loaded", HashMap::new());
    }

    fn frame(&mut self, project: &Project, dt: f32) -> u32 {
        let steps =
            crate::sim::run_fixed_steps(&self.physics, &self.world, Some(&self.runtime), dt);
        crate::sim::dispatch_physics_events(&self.world, &self.physics, &self.runtime);
        crate::sim::process_collision_damage(
            &self.world,
            &self.physics,
            &self.runtime,
            &self.commands,
        );
        crate::sim::update_projectiles(&self.world, &self.commands, dt);
        crate::sim::process_health(&self.world, &self.runtime);
        crate::sim::update_scripts(&self.world, &self.runtime, dt);
        crate::sim::process_destroys(
            &self.world,
            Some(&self.physics),
            Some(&mut self.runtime),
            &self.commands,
        );
        let spawns = std::mem::take(&mut self.commands.borrow_mut().spawns);
        for command in spawns {
            crate::world::spawn_entity_headless_with_assets(
                &mut self.world.borrow_mut(),
                &command.entity_def(),
                &mut self.physics.borrow_mut(),
                &mut self.assets,
            )
            .unwrap();
        }
        crate::sim::initialize_pending_scripts(&self.world, &mut self.runtime, &project.0).unwrap();
        crate::sim::dispatch_lua_events(&self.events, &self.listeners, Some(&self.runtime), dt);
        crate::transform::update_transforms(&mut self.world.borrow_mut().world);
        assert!(self.runtime.take_errors().is_empty());
        steps
    }
}

fn trace(runtime: &ScriptRuntime) -> LuaTable {
    let game: LuaTable = runtime.lua.globals().get("game").unwrap();
    game.get("trace").unwrap()
}

fn trace_values(table: &LuaTable) -> Vec<String> {
    table
        .clone()
        .sequence_values()
        .collect::<LuaResult<_>>()
        .unwrap()
}

fn assert_same_snapshot(engine: &EnginePhases, runner: &TestRunner) {
    let engine = crate::scene_snapshot::snapshot_scene(
        &engine.world.borrow(),
        &engine.assets,
        Some(&engine.physics.borrow()),
    )
    .unwrap();
    let runner = crate::scene_snapshot::snapshot_scene(
        &runner.scene_world.borrow(),
        &*runner.asset_catalog.borrow(),
        Some(&runner.physics_world.borrow()),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(engine).unwrap(),
        serde_json::to_value(runner).unwrap()
    );
}

#[test]
fn runner_and_engine_phases_preserve_fixed_update_event_order_and_reentrant_state() {
    let project = Project::new();
    project.write(
        "scene.yaml",
        r#"
name: Frame ordering
settings: {gravity: [0, 0, 0]}
entities:
- id: actor
  components:
    rigid_body: {type: dynamic}
    collider: {shape: sphere, radius: 0.5}
    script: {source: actor.lua}
- id: marker
"#,
    );
    project.write(
        "actor.lua",
        r#"
function init()
    game.trace = {}
    game.inits = (game.inits or 0) + 1
    game.fixed = 0
    game.updates = 0
    table.insert(game.trace, 'init')
    assert(entity.exists(_entity_string_id))
    entity.set_position('marker', 1, 0, 0)
    physics.set_velocity(_entity_string_id, 0, 0, 0)
    events.on('pulse', function(e)
        local x, y, z = entity.get_position('marker')
        entity.set_position('marker', x + 10, y, z)
        table.insert(game.trace, string.format('e%d', e.data.n))
    end)
end
function fixed_update(dt)
    assert(math.abs(dt - 1 / 120) < 0.000001)
    game.fixed = game.fixed + 1
    table.insert(game.trace, 'f' .. game.fixed)
    local x, y, z = entity.get_position('marker')
    entity.set_position('marker', x + 1, y, z)
    physics.set_velocity(_entity_string_id, 3, 0, 0)
end
function update(dt)
    game.updates = game.updates + 1
    local x, y, z = entity.get_position('marker')
    entity.set_position('marker', x, y + 1, z)
    table.insert(game.trace, 'u' .. game.updates .. ':f' .. game.fixed)
    events.emit('pulse', {n=game.updates})
end
"#,
    );
    let mut engine = EnginePhases::new(&project, "scene.yaml");
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    // A repeated initializer pass must not rerun any hook or duplicate listeners.
    crate::sim::initialize_pending_scripts(&engine.world, &mut engine.runtime, &project.0).unwrap();
    crate::sim::initialize_pending_scripts(
        &runner.scene_world,
        &mut runner.script_runtime,
        &project.0,
    )
    .unwrap();
    // PhysicsWorld starts with half a fixed step accumulated to absorb frame
    // jitter, so the first half-step frame advances and the next one waits.
    for (dt, steps) in [
        (0., 0),
        (FIXED_TIMESTEP * 0.5, 1),
        (FIXED_TIMESTEP * 0.5, 0),
        (FIXED_TIMESTEP * 2., 2),
    ] {
        assert_eq!(engine.frame(&project, dt), steps);
        runner.delta_time = dt;
        runner.step_frame();
        assert_eq!(
            trace_values(&trace(&engine.runtime)),
            trace_values(&trace(&runner.script_runtime))
        );
        assert_same_snapshot(&engine, &runner);
        assert!(runner.take_errors().is_empty());
    }
    assert_eq!(
        trace_values(&trace(&engine.runtime)),
        ["init", "u1:f0", "e1", "f1", "u2:f1", "e2", "u3:f1", "e3", "f2", "f3", "u4:f3", "e4",]
    );
    let game: LuaTable = runner.script_runtime.lua.globals().get("game").unwrap();
    assert_eq!(game.get::<u32>("inits").unwrap(), 1);
    assert_eq!(
        runner.get_entity_position("marker").unwrap(),
        glam::Vec3::new(44., 4., 0.)
    );
    assert!(
        (runner.get_entity_position("actor").unwrap().x - 3. * FIXED_TIMESTEP * 3.).abs() < 0.00001
    );
}

#[test]
fn deferred_spawn_destroy_and_scene_recreation_clean_up_owned_listeners() {
    let project = Project::new();
    project.write("scene.yaml", "name: Deferred lifecycle\nsettings: {gravity: [0,0,0]}\nentities:\n- id: actor\n  components:\n    script: {source: actor.lua}\n- id: marker\n");
    project.write("actor.lua", r#"
function init()
    game.trace = {'actor:init'}
    self.updates = 0
    events.on('pulse', function(e)
        table.insert(game.trace, string.format('actor:e%d', e.data.n))
    end)
end
function update()
    self.updates = self.updates + 1
    table.insert(game.trace, 'actor:u' .. self.updates)
    if self.updates == 1 then
        entity.spawn_ex({id='child', script='child.lua', rigid_body='dynamic', collider={shape='sphere', radius=0.2}})
        assert(not entity.exists('child'))
    elseif self.updates == 2 then
        entity.destroy('child')
        entity.destroy('child')
        assert(entity.exists('child'))
    end
    events.emit('pulse', {n=self.updates})
end
function on_destroy()
    assert(entity.exists(_entity_string_id))
    table.insert(game.trace, 'actor:destroy')
end
"#);
    project.write(
        "child.lua",
        r#"
function init()
    assert(entity.exists(_entity_string_id))
    table.insert(game.trace, 'child:init')
    events.on('pulse', function(e)
        table.insert(game.trace, string.format('child:e%d', e.data.n))
    end)
end
function on_destroy()
    assert(entity.exists(_entity_string_id))
    entity.set_position('marker', 0, 9, 0)
    physics.set_velocity(_entity_string_id, 0, 0, 0)
    table.insert(game.trace, 'child:destroy')
    events.on('pulse', function() error('destroy-time listener escaped') end)
end
"#,
    );
    project.write("replacement.yaml", "name: Replacement\nsettings: {gravity: [0,0,0]}\nentities:\n- id: actor\n  components:\n    script: {source: replacement.lua}\n");
    project.write(
        "replacement.lua",
        r#"
function init()
    game.trace = {'replacement:init'}
    events.on('pulse', function() table.insert(game.trace, 'replacement:event') end)
end
function update() events.emit('pulse', {}) end
"#,
    );
    let mut engine = EnginePhases::new(&project, "scene.yaml");
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    let runner_world = runner.scene_world.clone();
    let runner_physics = runner.physics_world.clone();
    let mut child_handles = None;
    for frame in 1..=3 {
        engine.frame(&project, FIXED_TIMESTEP);
        runner.delta_time = FIXED_TIMESTEP;
        runner.step_frame();
        assert_same_snapshot(&engine, &runner);
        assert_eq!(
            trace_values(&trace(&engine.runtime)),
            trace_values(&trace(&runner.script_runtime))
        );
        if frame == 1 {
            child_handles = Some((
                engine.world.borrow().entity_registry["child"],
                runner.scene_world.borrow().entity_registry["child"],
            ));
            assert_eq!(runner.physics_world.borrow().rigid_body_set.len(), 1);
        } else {
            let (engine_child, runner_child) = child_handles.unwrap();
            assert!(!engine.runtime.entity_envs.contains_key(&engine_child));
            assert!(!runner
                .script_runtime
                .entity_envs
                .contains_key(&runner_child));
            assert_eq!(runner.physics_world.borrow().rigid_body_set.len(), 0);
            assert_eq!(runner.lua_listener_id_map.borrow().len(), 1);
            assert_eq!(engine.listener_ids.borrow().len(), 1);
        }
        assert!(runner.take_errors().is_empty());
    }
    // Tables keep weak VM references in mlua. Hold the old VMs alive while
    // checking that retired listeners cannot mutate their traces after reload.
    let _old_vms = (
        engine.runtime.lua.clone(),
        runner.script_runtime.lua.clone(),
    );
    let engine_old_trace = trace(&engine.runtime);
    let runner_old_trace = trace(&runner.script_runtime);
    let mut expected = vec![
        "actor:init",
        "actor:u1",
        "child:init",
        "actor:e1",
        "child:e1",
        "actor:u2",
        "child:destroy",
        "actor:e2",
        "actor:u3",
        "actor:e3",
    ];
    assert_eq!(trace_values(&runner_old_trace), expected);
    assert_eq!(runner.get_entity_position("marker").unwrap().y, 9.);
    engine.events.borrow_mut().emit_simple("pulse");
    runner.event_bus.borrow_mut().emit_simple("pulse");
    engine.replace_scene(&project, "replacement.yaml");
    runner.load_scene("replacement.yaml").unwrap();
    expected.push("actor:destroy");
    assert_eq!(trace_values(&engine_old_trace), expected);
    assert_eq!(trace_values(&runner_old_trace), expected);
    assert!(Rc::ptr_eq(&runner_world, &runner.scene_world));
    assert!(Rc::ptr_eq(&runner_physics, &runner.physics_world));
    engine.frame(&project, FIXED_TIMESTEP);
    runner.step_frame();
    assert_same_snapshot(&engine, &runner);
    for runtime in [&engine.runtime, &runner.script_runtime] {
        assert_eq!(
            trace_values(&trace(runtime)),
            ["replacement:init", "replacement:event"]
        );
        assert_eq!(runtime.entity_envs.len(), 1);
    }
    assert_eq!(trace_values(&runner_old_trace), expected);
    assert_eq!(runner.lua_listener_id_map.borrow().len(), 1);
    assert!(runner.take_errors().is_empty());
    let world = runner.scene_world.borrow();
    assert_eq!(world.entity_registry.len(), 1);
    assert!(
        world
            .world
            .get::<&Script>(world.entity_registry["actor"])
            .unwrap()
            .initialized
    );
}
