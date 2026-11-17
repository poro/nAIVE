use super::*;
use std::path::PathBuf;

#[test]
fn script_loads_and_reloads_stay_inside_the_project() {
    let scratch = script_dir("confinement");
    let project = scratch.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let outside = scratch.join("outside.lua");
    std::fs::write(&outside, "error('outside script was executed')").unwrap();
    std::fs::write(project.join("inside.lua"), "self.loaded = true").unwrap();
    let mut runtime = ScriptRuntime::new();
    let world = hecs::World::new();
    let entity = world.reserve_entity();

    for escape in [Path::new("../outside.lua"), outside.as_path()] {
        assert!(runtime.load_script(entity, &project, escape).is_err());
        assert!(!runtime.entity_envs.contains_key(&entity));
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, project.join("link.lua")).unwrap();
        assert!(runtime
            .load_script(entity, &project, Path::new("link.lua"))
            .is_err());
    }

    runtime
        .load_script(entity, &project, Path::new("inside.lua"))
        .unwrap();
    assert!(runtime
        .hot_reload_script(entity, &project, Path::new("../outside.lua"))
        .is_err());
    let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
    assert!(state.get::<bool>("loaded").unwrap());
    std::fs::remove_dir_all(scratch).unwrap();
}

#[test]
fn test_script_runtime_creation() {
    let runtime = ScriptRuntime::new();
    assert!(runtime.entity_envs.is_empty());
}

#[test]
fn test_register_api() {
    let runtime = ScriptRuntime::new();
    runtime.register_api().unwrap();

    // Verify log function exists
    let globals = runtime.lua.globals();
    let log_fn: LuaFunction = globals.get("log").unwrap();
    log_fn.call::<()>("test message").unwrap();
}

#[test]
fn test_load_and_call_script() {
    let mut runtime = ScriptRuntime::new();
    runtime.register_api().unwrap();

    // Create a temp script file
    let dir = script_dir("load_and_call_script");
    let script_path = dir.join("test.lua");
    std::fs::write(
        &script_path,
        r#"
        self.count = 0
        function init()
            self.count = 1
        end
        function update(dt)
            self.count = self.count + 1
        end
    "#,
    )
    .unwrap();

    let world = hecs::World::new();
    let entity = world.reserve_entity();

    runtime
        .load_script(entity, &dir, Path::new("test.lua"))
        .unwrap();
    runtime.call_init(entity);
    runtime.call_update(entity, 0.016);

    // Verify state
    let key = runtime.entity_envs.get(&entity).unwrap();
    let env: LuaTable = runtime.lua.registry_value(key).unwrap();
    let self_table: LuaTable = env.get("self").unwrap();
    let count: i32 = self_table.get("count").unwrap();
    assert_eq!(count, 2); // init sets to 1, update increments to 2

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scratch directory for one test's script files.
fn script_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "naive_test_scripting_{}_{test}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The script environment of `entity`.
fn env_of(runtime: &ScriptRuntime, entity: hecs::Entity) -> LuaTable {
    runtime
        .lua
        .registry_value(runtime.entity_envs.get(&entity).unwrap())
        .unwrap()
}

#[test]
fn sandbox_has_no_filesystem_shell_or_module_access() {
    let runtime = ScriptRuntime::new();
    let blocked = [
        "os",
        "io",
        "package",
        "require",
        "coroutine",
        "debug",
        "loadfile",
        "dofile",
    ];
    for name in blocked {
        let present: bool = runtime
            .lua
            .load(format!("return {name} ~= nil"))
            .eval()
            .unwrap();
        assert!(!present, "`{name}` should not be available to scripts");
    }

    // The libraries scripts rely on are still there.
    let usable: bool = runtime
        .lua
        .load(
            r#"return type(math.sin) == "function"
                and type(string.format) == "function"
                and type(table.insert) == "function""#,
        )
        .eval()
        .unwrap();
    assert!(usable);
}

/// Limits small enough to trip quickly in a test.
fn tight_limits() -> ScriptLimits {
    ScriptLimits {
        instructions_per_call: 200_000,
        memory_bytes: 16 * 1024 * 1024,
    }
}

/// Write `code` to a script file and load it for `entity`.
fn load(
    runtime: &mut ScriptRuntime,
    entity: hecs::Entity,
    test: &str,
    code: &str,
) -> Result<(), String> {
    let dir = script_dir(test);
    std::fs::write(dir.join("script.lua"), code).unwrap();
    let result = runtime.load_script(entity, &dir, Path::new("script.lua"));
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[test]
fn runaway_hook_is_stopped_and_stays_stopped() {
    let mut runtime = ScriptRuntime::with_limits(tight_limits());
    let entity = hecs::World::new().reserve_entity();
    load(
        &mut runtime,
        entity,
        "runaway_hook",
        r#"
        function update(dt)
            while true do end
        end
    "#,
    )
    .unwrap();
    runtime.set_entity_string_id(entity, "spinner").unwrap();

    // Returns instead of hanging.
    runtime.call_update(entity, 0.016);

    let reports = runtime.take_limit_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(
        reports[0].contains("spinner") && reports[0].contains("CPU"),
        "{reports:?}"
    );

    // The script's code is gone, so the next frame does not run it again.
    let env = env_of(&runtime, entity);
    assert!(env.raw_get::<LuaValue>("update").unwrap().is_nil());
    runtime.call_update(entity, 0.016);
    assert!(runtime.take_limit_reports().is_empty());

    // It still knows which entity it belongs to, for a reload.
    assert_eq!(
        env.raw_get::<String>("_entity_string_id").unwrap(),
        "spinner"
    );
}

#[test]
fn cpu_limit_cannot_be_swallowed_with_pcall() {
    let mut runtime = ScriptRuntime::with_limits(tight_limits());
    let entity = hecs::World::new().reserve_entity();
    load(
        &mut runtime,
        entity,
        "pcall_retry",
        r#"
        function update(dt)
            -- A retry loop around code that never finishes.
            while true do
                pcall(function() while true do end end)
            end
        end
    "#,
    )
    .unwrap();

    runtime.call_update(entity, 0.016);

    assert_eq!(runtime.take_limit_reports().len(), 1);
}

#[test]
fn cpu_limit_applies_to_each_call_separately() {
    let mut runtime = ScriptRuntime::with_limits(tight_limits());
    let entity = hecs::World::new().reserve_entity();
    load(
        &mut runtime,
        entity,
        "per_call_budget",
        r#"
        count = 0
        function update(dt)
            for i = 1, 10000 do count = count + 1 end
        end
    "#,
    )
    .unwrap();

    // Fifty calls use far more than one call's budget in total.
    for _ in 0..50 {
        runtime.call_update(entity, 0.016);
    }

    assert!(runtime.take_limit_reports().is_empty());
    let count: i64 = env_of(&runtime, entity).raw_get("count").unwrap();
    assert_eq!(count, 500_000);
}

#[test]
fn memory_hog_is_stopped_and_its_memory_reclaimed() {
    let mut runtime = ScriptRuntime::with_limits(ScriptLimits {
        memory_bytes: 16 * 1024 * 1024,
        ..ScriptLimits::default()
    });
    let world = hecs::World::new();
    let hog = world.reserve_entity();
    let bystander = world.reserve_entity();
    load(
        &mut runtime,
        hog,
        "memory_hog",
        r#"
        hoard = {}
        function update(dt)
            while true do
                hoard[#hoard + 1] = string.rep("x", 1024 * 1024)
            end
        end
    "#,
    )
    .unwrap();
    load(
        &mut runtime,
        bystander,
        "memory_bystander",
        r#"
        ticks = 0
        function update(dt) ticks = ticks + 1 end
    "#,
    )
    .unwrap();

    runtime.call_update(hog, 0.016);

    let reports = runtime.take_limit_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(reports[0].contains("memory"), "{reports:?}");
    let used = runtime.lua.used_memory();
    assert!(
        used < 4 * 1024 * 1024,
        "hoard not reclaimed: {used} bytes in use"
    );

    // Other scripts carry on.
    runtime.call_update(bystander, 0.016);
    let ticks: i64 = env_of(&runtime, bystander).raw_get("ticks").unwrap();
    assert_eq!(ticks, 1);
}

#[test]
fn script_that_never_finishes_loading_is_rejected() {
    let mut runtime = ScriptRuntime::with_limits(tight_limits());
    let entity = hecs::World::new().reserve_entity();

    let err = load(&mut runtime, entity, "runaway_load", "while true do end").unwrap_err();
    assert!(err.contains("CPU limit"), "{err}");
    assert!(runtime.entity_envs.is_empty());

    // A well-behaved script loads and runs normally afterwards.
    load(
        &mut runtime,
        entity,
        "after_runaway_load",
        "count = 0  function update(dt) count = count + 1 end",
    )
    .unwrap();
    runtime.call_update(entity, 0.016);
    assert!(runtime.take_limit_reports().is_empty());
    let count: i64 = env_of(&runtime, entity).raw_get("count").unwrap();
    assert_eq!(count, 1);
}

#[test]
fn hot_reload_reaches_every_entity_sharing_a_script() {
    let mut runtime = ScriptRuntime::new();
    let dir = script_dir("shared_script_reload");
    let world = hecs::World::new();
    let first = world.reserve_entity();
    let second = world.reserve_entity();

    std::fs::write(dir.join("shared.lua"), "version = 1").unwrap();
    runtime
        .load_script(first, &dir, Path::new("shared.lua"))
        .unwrap();
    runtime
        .load_script(second, &dir, Path::new("shared.lua"))
        .unwrap();

    std::fs::write(dir.join("shared.lua"), "version = 2").unwrap();
    for entity in [first, second] {
        assert!(runtime
            .hot_reload_script(entity, &dir, Path::new("shared.lua"))
            .unwrap());
        let version: i32 = env_of(&runtime, entity).raw_get("version").unwrap();
        assert_eq!(version, 2);
    }

    // Nothing changed since, so a second pass reloads nothing.
    assert!(!runtime
        .hot_reload_script(first, &dir, Path::new("shared.lua"))
        .unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stopped_script_runs_again_after_a_reload() {
    let mut runtime = ScriptRuntime::with_limits(tight_limits());
    let dir = script_dir("stopped_then_reloaded");
    let entity = hecs::World::new().reserve_entity();

    std::fs::write(
        dir.join("fixme.lua"),
        "function update(dt) while true do end end",
    )
    .unwrap();
    runtime
        .load_script(entity, &dir, Path::new("fixme.lua"))
        .unwrap();
    runtime.set_entity_string_id(entity, "fixme").unwrap();
    runtime.call_update(entity, 0.016);
    assert_eq!(runtime.take_limit_reports().len(), 1);

    // Fixing the file and saving it brings the script back.
    std::fs::write(
        dir.join("fixme.lua"),
        "count = 0  function update(dt) count = count + 1 end",
    )
    .unwrap();
    assert!(runtime
        .hot_reload_script(entity, &dir, Path::new("fixme.lua"))
        .unwrap());
    runtime.call_update(entity, 0.016);

    let env = env_of(&runtime, entity);
    assert_eq!(env.raw_get::<i64>("count").unwrap(), 1);
    assert_eq!(env.raw_get::<String>("_entity_string_id").unwrap(), "fixme");
    assert!(runtime.take_limit_reports().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hot_reload_keeps_the_entity_string_id() {
    let mut runtime = ScriptRuntime::new();
    let dir = script_dir("hot_reload_id");
    let world = hecs::World::new();
    let entity = world.reserve_entity();

    std::fs::write(dir.join("hero.lua"), "version = 1").unwrap();
    runtime
        .load_script(entity, &dir, Path::new("hero.lua"))
        .unwrap();
    runtime.set_entity_string_id(entity, "hero").unwrap();

    std::fs::write(dir.join("hero.lua"), "version = 2").unwrap();
    assert!(runtime
        .hot_reload_script(entity, &dir, Path::new("hero.lua"))
        .unwrap());

    let env = env_of(&runtime, entity);
    assert_eq!(env.get::<i32>("version").unwrap(), 2);
    assert_eq!(
        env.get::<Option<String>>("_entity_string_id")
            .unwrap()
            .as_deref(),
        Some("hero")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_reload_keeps_live_state_code_and_subscriptions() {
    let mut runtime = ScriptRuntime::new();
    let entity = hecs::World::new().reserve_entity();
    let events = Rc::new(RefCell::new(EventBus::new(100)));
    let listeners = Rc::new(RefCell::new(HashMap::new()));
    runtime
        .register_event_api(
            events.clone(),
            listeners.clone(),
            Rc::new(RefCell::new(0)),
            Rc::new(RefCell::new(HashMap::new())),
        )
        .unwrap();
    let dir = script_dir("transactional_reload");
    let source = dir.join("script.lua");
    let good = r#"
        self.count = 1
        self.nested = { value = 7 }
        self.cycle = self
        self.listener = events.on("ping", function() self.count = self.count + 1 end)
        function update() self.count = self.count + 1 end
    "#;
    std::fs::write(&source, good).unwrap();
    runtime
        .load_script(entity, &dir, Path::new("script.lua"))
        .unwrap();
    runtime.set_entity_string_id(entity, "hero").unwrap();

    for broken in [
        "function update( syntax error",
        "events.on('ping', function() error('leaked listener') end); error('top-level failed')",
        r#"
            function on_reload()
                self.count = 999
                self.nested.value = 999
                events.off(self.listener)
                events.on('ping', function() error('leaked listener') end)
                error('reload failed')
            end
        "#,
    ] {
        std::fs::write(&source, broken).unwrap();
        assert!(runtime
            .hot_reload_script(entity, &dir, Path::new("script.lua"))
            .is_err());
        assert_eq!(runtime.script_sources.get(&entity).unwrap(), good);
        let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
        assert_eq!(state.get::<i64>("count").unwrap(), 1);
        assert_eq!(
            state
                .get::<LuaTable>("nested")
                .unwrap()
                .get::<i64>("value")
                .unwrap(),
            7
        );
        assert_eq!(listeners.borrow()["ping"].len(), 1);
    }
    events.borrow_mut().emit("ping", HashMap::new());
    crate::sim::dispatch_lua_events(&events, &listeners, Some(&runtime), 0.0);
    runtime.call_update(entity, 0.0);
    let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
    assert_eq!(state.get::<i64>("count").unwrap(), 3);
    assert!(runtime.take_errors().is_empty());

    std::fs::write(
        &source,
        r#"
        events.on('ping', function() self.count = self.count + 10 end)
        function on_reload() self.nested.value = 8 end
    "#,
    )
    .unwrap();
    assert!(runtime
        .hot_reload_script(entity, &dir, Path::new("script.lua"))
        .unwrap());
    assert_eq!(listeners.borrow()["ping"].len(), 1);
    events.borrow_mut().emit("ping", HashMap::new());
    crate::sim::dispatch_lua_events(&events, &listeners, Some(&runtime), 0.0);
    let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
    assert_eq!(state.get::<i64>("count").unwrap(), 13);
    assert_eq!(
        state
            .get::<LuaTable>("nested")
            .unwrap()
            .get::<i64>("value")
            .unwrap(),
        8
    );
    assert_eq!(
        state.get::<LuaTable>("cycle").unwrap().to_pointer(),
        state.to_pointer()
    );
    assert!(runtime.take_errors().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn same_frame_pool_expansion_reserves_unique_ids() {
    let runtime = ScriptRuntime::new();
    let world = Rc::new(RefCell::new(SceneWorld::new()));
    let physics = Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::ZERO)));
    let commands = Rc::new(RefCell::new(EntityCommandQueue::default()));
    let pools = Rc::new(RefCell::new(EntityPoolManager::new()));
    runtime.register_entity_api(world.clone(), physics).unwrap();
    runtime
        .register_entity_command_api(world.clone(), commands.clone(), pools.clone())
        .unwrap();
    runtime
        .lua
        .load(
            r#"
        entity.pool_create('effects_with_underscores', 'procedural:cube', 'procedural:default', 2)
        first = entity.pool_acquire('effects_with_underscores')
        second = entity.pool_acquire('effects_with_underscores')
        assert(first ~= second)
    "#,
        )
        .exec()
        .unwrap();
    let commands = commands.borrow();
    let unique: std::collections::HashSet<_> =
        commands.spawns.iter().map(|spawn| &spawn.id).collect();
    assert_eq!(unique.len(), 4);
    for command in &commands.spawns {
        let entity = world
            .borrow_mut()
            .world
            .spawn((crate::components::Transform::default(),));
        world
            .borrow_mut()
            .entity_registry
            .insert(command.id.clone(), entity);
        crate::sim::finish_pool_spawn(&world, &pools, command);
    }
    assert_eq!(pools.borrow().pool_size("effects_with_underscores"), (4, 2));
}

#[test]
fn init_only_subscriptions_survive_reloads_with_shared_current_state() {
    let mut runtime = ScriptRuntime::new();
    let entity = hecs::World::new().reserve_entity();
    let events = Rc::new(RefCell::new(EventBus::new(100)));
    let listeners = Rc::new(RefCell::new(HashMap::new()));
    runtime
        .register_event_api(
            events.clone(),
            listeners.clone(),
            Rc::new(RefCell::new(0)),
            Rc::new(RefCell::new(HashMap::new())),
        )
        .unwrap();
    runtime
        .lua
        .load("counts = {initializations = 0}")
        .exec()
        .unwrap();
    let dir = script_dir("init_only_subscriptions");
    std::fs::write(
        dir.join("script.lua"),
        r#"
        function init()
            counts.initializations = counts.initializations + 1
            self.count = 0
            events.on('ping', function() self.count = self.count + 1 end)
        end
    "#,
    )
    .unwrap();
    runtime
        .load_script(entity, &dir, Path::new("script.lua"))
        .unwrap();
    runtime.call_init(entity);
    for revision in 1..=3 {
        std::fs::write(
            dir.join("script.lua"),
            format!("function update() self.count = self.count + 10 end -- revision {revision}"),
        )
        .unwrap();
        runtime
            .hot_reload_script(entity, &dir, Path::new("script.lua"))
            .unwrap();
        runtime.call_update(entity, 0.0);
        events.borrow_mut().emit("ping", HashMap::new());
        crate::sim::dispatch_lua_events(&events, &listeners, Some(&runtime), 0.0);
        let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
        assert_eq!(state.get::<i64>("count").unwrap(), revision * 11);
        assert_eq!(listeners.borrow()["ping"].len(), 1);
    }
    let counts: LuaTable = runtime.lua.globals().get("counts").unwrap();
    assert_eq!(counts.get::<i64>("initializations").unwrap(), 1);
    std::fs::write(
        dir.join("script.lua"),
        "function on_reload() events.on('ping', function() self.count = self.count + 100 end) end",
    )
    .unwrap();
    runtime
        .hot_reload_script(entity, &dir, Path::new("script.lua"))
        .unwrap();
    events.borrow_mut().emit("ping", HashMap::new());
    crate::sim::dispatch_lua_events(&events, &listeners, Some(&runtime), 0.0);
    let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
    assert_eq!(state.get::<i64>("count").unwrap(), 133);
    assert_eq!(listeners.borrow()["ping"].len(), 1);
    runtime.remove_entity(entity);
    assert!(listeners.borrow().values().all(Vec::is_empty));
    assert!(runtime.listener_environments.borrow().is_empty());
    assert!(runtime.take_errors().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deeply_nested_state_rejects_reload_without_losing_live_environment() {
    let mut runtime = ScriptRuntime::new();
    let entity = hecs::World::new().reserve_entity();
    let dir = script_dir("deep_reload_state");
    let old_code = "self.chain = {}; local cursor = self.chain; for i=1,100000 do cursor.next = {}; cursor = cursor.next end; function update() self.alive = true end";
    std::fs::write(dir.join("script.lua"), old_code).unwrap();
    runtime
        .load_script(entity, &dir, Path::new("script.lua"))
        .unwrap();
    std::fs::write(
        dir.join("script.lua"),
        "function update() error('replacement should not run') end",
    )
    .unwrap();
    let error = runtime
        .hot_reload_script(entity, &dir, Path::new("script.lua"))
        .unwrap_err();
    assert!(error.contains("depth limit"), "{error}");
    assert_eq!(runtime.script_sources.get(&entity).unwrap(), old_code);
    runtime.call_update(entity, 0.0);
    let state: LuaTable = env_of(&runtime, entity).get("self").unwrap();
    assert!(state.get::<bool>("alive").unwrap());
    assert!(state.get::<LuaTable>("chain").is_ok());
    assert!(runtime.take_errors().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
