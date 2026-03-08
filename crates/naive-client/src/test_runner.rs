//! Phase 10: Headless test runner for automated gameplay testing.
//!
//! Runs Lua test scripts that inject input, advance game time, and assert
//! that game events occurred. No GPU or window required.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mlua::prelude::*;

use crate::components::{Player, Transform};
use crate::events::EventBus;
use crate::input::InputState;
use crate::physics::{CharacterController, PhysicsWorld};
use crate::physics::{Collider as ColliderComp, RigidBody as RigidBodyComp};
use crate::scripting::{Script, ScriptRuntime};
use crate::tween::TweenSystem;
use crate::world::SceneWorld;

/// Result of a single test function execution.
#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
    pub game_time: f32,
}

/// Headless test runner. Owns all game systems except GPU/rendering.
/// Fields shared with Lua closures use Rc<RefCell<T>> for safe interior mutability.
pub struct TestRunner {
    pub project_root: PathBuf,
    pub scene_world: Rc<RefCell<SceneWorld>>,
    pub input_state: Rc<RefCell<InputState>>,
    pub physics_world: Rc<RefCell<PhysicsWorld>>,
    pub script_runtime: ScriptRuntime,
    pub event_bus: Rc<RefCell<EventBus>>,
    pub tween_system: TweenSystem,
    pub delta_time: f32,
    pub total_time: f32,
    pub frame_count: u64,
    lua_event_listeners: Rc<RefCell<HashMap<String, Vec<mlua::RegistryKey>>>>,
    next_lua_listener_id: Rc<RefCell<u64>>,
    lua_listener_id_map: Rc<RefCell<HashMap<u64, (String, usize)>>>,
}

impl TestRunner {
    pub fn new(project_root: &Path) -> Self {
        let bindings = crate::input::load_bindings(project_root);
        Self {
            project_root: project_root.to_path_buf(),
            scene_world: Rc::new(RefCell::new(SceneWorld::new())),
            input_state: Rc::new(RefCell::new(InputState::new(bindings))),
            physics_world: Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::new(0.0, -9.81, 0.0)))),
            script_runtime: ScriptRuntime::new(),
            event_bus: Rc::new(RefCell::new(EventBus::new(1000))),
            tween_system: TweenSystem::new(),
            delta_time: 1.0 / 60.0,
            total_time: 0.0,
            frame_count: 0,
            lua_event_listeners: Rc::new(RefCell::new(HashMap::new())),
            next_lua_listener_id: Rc::new(RefCell::new(0)),
            lua_listener_id_map: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Load a scene by path (relative to project root).
    pub fn load_scene(&mut self, scene_rel: &str) -> Result<(), String> {
        let scene_path = self.project_root.join(scene_rel);
        let scene = crate::scene::load_scene(&scene_path)
            .map_err(|e| format!("Failed to load scene: {:?}", e))?;

        // Set gravity from scene settings
        let gravity = glam::Vec3::from(scene.settings.gravity);
        *self.physics_world.borrow_mut() = PhysicsWorld::new(gravity);

        // Spawn entities headlessly (no GPU)
        *self.scene_world.borrow_mut() = SceneWorld::new();
        {
            let mut sw = self.scene_world.borrow_mut();
            let mut pw = self.physics_world.borrow_mut();
            crate::world::spawn_all_entities_headless(&mut *sw, &scene, &mut *pw);
        }

        // Initialize scripting
        self.script_runtime = ScriptRuntime::new();
        if let Err(e) = self.script_runtime.register_api() {
            return Err(format!("Failed to register script API: {}", e));
        }

        // Register APIs with shared Rc<RefCell<>> references
        self.script_runtime
            .register_input_api(self.input_state.clone())
            .map_err(|e| format!("Input API: {}", e))?;
        self.script_runtime
            .register_physics_api(self.physics_world.clone(), self.scene_world.clone())
            .map_err(|e| format!("Physics API: {}", e))?;
        self.script_runtime
            .register_entity_api(self.scene_world.clone(), self.physics_world.clone())
            .map_err(|e| format!("Entity API: {}", e))?;
        self.script_runtime
            .register_event_api(
                self.event_bus.clone(),
                self.lua_event_listeners.clone(),
                self.next_lua_listener_id.clone(),
                self.lua_listener_id_map.clone(),
            )
            .map_err(|e| format!("Event API: {}", e))?;

        // Load event schema
        self.event_bus.borrow_mut().load_schema(&self.project_root);

        // Load scripts for entities
        let scene_clone = self.scene_world.borrow().current_scene.clone();
        if let Some(scene_data) = &scene_clone {
            for entity_def in &scene_data.entities {
                if let Some(script_def) = &entity_def.components.script {
                    let sw = self.scene_world.borrow();
                    if let Some(&entity) = sw.entity_registry.get(&entity_def.id) {
                        let source_path = PathBuf::from(&script_def.source);
                        let script_comp = Script {
                            source: source_path.clone(),
                            initialized: false,
                        };
                        drop(sw);
                        let _ = self.scene_world.borrow_mut().world.insert_one(entity, script_comp);

                        if let Err(e) = self.script_runtime.load_script(
                            entity,
                            &self.project_root,
                            &source_path,
                        ) {
                            tracing::error!(
                                "Failed to load script for '{}': {}",
                                entity_def.id,
                                e
                            );
                        } else {
                            let _ = self
                                .script_runtime
                                .set_entity_string_id(entity, &entity_def.id);
                        }
                    }
                }
            }
        }

        // Call init on all scripts (collect first to release world borrow before Lua runs)
        let uninit: Vec<hecs::Entity> = {
            let sw = self.scene_world.borrow();
            let mut query = sw.world.query::<&Script>();
            query.iter()
                .filter(|(_, s)| !s.initialized)
                .map(|(e, _)| e)
                .collect()
        };
        for entity in uninit {
            self.script_runtime.call_init(entity);
        }
        {
            let sw = self.scene_world.borrow_mut();
            for (_entity, script) in sw.world.query::<&mut Script>().iter() {
                script.initialized = true;
            }
        }

        // Emit lifecycle event
        self.event_bus
            .borrow_mut()
            .emit("lifecycle.scene_loaded", std::collections::HashMap::new());
        self.event_bus.borrow_mut().flush();

        tracing::info!("Test runner: scene loaded");
        Ok(())
    }

    /// Advance the simulation by one frame.
    pub fn step_frame(&mut self) {
        let dt = self.delta_time;

        // Apply synthetic inputs
        self.input_state.borrow_mut().begin_frame();

        // Auto-capture cursor for FPS controller
        self.input_state.borrow_mut().cursor_captured = true;

        // FPS controller update
        self.update_fps_controller(dt);

        // Update all scripts (collect first to release world borrow before Lua runs)
        let scripted: Vec<hecs::Entity> = {
            let sw = self.scene_world.borrow();
            let mut query = sw.world.query::<&Script>();
            query.iter()
                .map(|(e, _)| e)
                .collect()
        };
        for entity in scripted {
            self.script_runtime.call_update(entity, dt);
        }

        // Tick event bus and tweens
        self.event_bus.borrow_mut().tick(dt as f64);
        self.event_bus.borrow_mut().flush();
        let _tween_results = self.tween_system.update(dt);

        // Update transforms
        {
            let mut sw = self.scene_world.borrow_mut();
            crate::transform::update_transforms(&mut sw.world);
        }

        self.total_time += dt;
        self.frame_count += 1;
    }

    /// Advance multiple frames.
    pub fn step_frames(&mut self, count: u64) {
        for _ in 0..count {
            self.step_frame();
        }
    }

    /// Advance by a given number of seconds (at fixed timestep).
    pub fn step_seconds(&mut self, seconds: f32) {
        let frames = (seconds / self.delta_time).ceil() as u64;
        self.step_frames(frames);
    }

    /// Check if a specific event occurred in the log.
    pub fn event_occurred(&self, event_type: &str, filter: &std::collections::HashMap<String, String>) -> bool {
        let eb = self.event_bus.borrow();
        for event in eb.get_log() {
            if event.event_type == event_type {
                if filter.is_empty() {
                    return true;
                }
                let mut matches = true;
                for (key, val) in filter {
                    match event.data.get(key) {
                        Some(v) => {
                            if v.as_str().map(|s| s != val.as_str()).unwrap_or(true) {
                                matches = false;
                                break;
                            }
                        }
                        None => {
                            matches = false;
                            break;
                        }
                    }
                }
                if matches {
                    return true;
                }
            }
        }
        false
    }

    /// Get player position.
    pub fn get_entity_position(&self, entity_id: &str) -> Option<glam::Vec3> {
        let sw = self.scene_world.borrow();
        let entity = *sw.entity_registry.get(entity_id)?;
        let transform = sw.world.get::<&Transform>(entity).ok()?;
        Some(transform.position)
    }

    /// FPS controller update (replicated from engine.rs for headless).
    fn update_fps_controller(&mut self, dt: f32) {
        // Collect player data first to avoid borrow conflicts
        let player_data: Vec<_> = {
            let sw = self.scene_world.borrow();
            let mut query = sw.world
                .query::<(&Player, &CharacterController, &RigidBodyComp, &ColliderComp)>();
            query.iter()
                .map(|(entity, (player, cc, rb, col))| {
                    (
                        entity,
                        player.yaw,
                        player.pitch,
                        player.height,
                        cc.move_speed,
                        cc.sprint_multiplier,
                        cc.jump_impulse,
                        cc.grounded,
                        cc.velocity,
                        rb.handle,
                        col.handle,
                    )
                })
                .collect()
        };

        for (
            entity,
            yaw,
            pitch,
            _height,
            move_speed,
            sprint_multiplier,
            jump_impulse,
            grounded,
            velocity,
            rb_handle,
            col_handle,
        ) in player_data
        {
            let mouse_delta = self.input_state.borrow().mouse_delta();
            let sensitivity = 0.002;
            let new_yaw = yaw - mouse_delta.x * sensitivity;
            let new_pitch = (pitch - mouse_delta.y * sensitivity).clamp(
                -std::f32::consts::FRAC_PI_2 + 0.01,
                std::f32::consts::FRAC_PI_2 - 0.01,
            );

            let move_input = self.input_state.borrow().axis_2d(
                "move_forward",
                "move_backward",
                "move_left",
                "move_right",
            );
            let speed = if self.input_state.borrow().pressed("sprint") {
                move_speed * sprint_multiplier
            } else {
                move_speed
            };

            let forward = glam::Vec3::new(-new_yaw.sin(), 0.0, -new_yaw.cos());
            let right = glam::Vec3::new(-forward.z, 0.0, forward.x);
            let move_dir =
                (forward * move_input.y + right * move_input.x).normalize_or_zero();
            let mut desired = move_dir * speed * dt;

            let mut vel_y = velocity.y;
            if grounded {
                vel_y = 0.0;
                if self.input_state.borrow().just_pressed("jump") {
                    vel_y = jump_impulse;
                }
            }
            vel_y += self.physics_world.borrow().gravity.y * dt;
            desired.y = vel_y * dt;

            let (_effective, new_grounded) =
                self.physics_world
                    .borrow_mut()
                    .move_character(rb_handle, col_handle, desired, dt);

            // Update player + character controller
            if let Ok(mut player) = self.scene_world.borrow_mut().world.get::<&mut Player>(entity) {
                player.yaw = new_yaw;
                player.pitch = new_pitch;
            }
            if let Ok(mut cc) = self.scene_world.borrow_mut().world.get::<&mut CharacterController>(entity) {
                cc.grounded = new_grounded;
                cc.velocity.y = vel_y;
            }

            // Sync physics → transform
            {
                let pw = self.physics_world.borrow();
                let mut sw = self.scene_world.borrow_mut();
                pw.sync_to_ecs(&mut sw.world);
            }
        }

        self.physics_world.borrow_mut().step(dt);
        {
            let pw = self.physics_world.borrow();
            let mut sw = self.scene_world.borrow_mut();
            pw.sync_to_ecs(&mut sw.world);
        }
    }
}

// ---------------------------------------------------------------------------
// Test execution: discover and run test functions from a Lua file
// ---------------------------------------------------------------------------

/// Run all test functions in a Lua test file. Returns results for each test.
pub fn run_test_file(project_root: &Path, test_file: &Path) -> Vec<TestResult> {
    let test_source = match std::fs::read_to_string(test_file) {
        Ok(s) => s,
        Err(e) => {
            return vec![TestResult {
                name: test_file.display().to_string(),
                passed: false,
                error: Some(format!("Failed to read test file: {}", e)),
                game_time: 0.0,
            }];
        }
    };

    // Create a temporary Lua VM to discover test functions
    let lua = Lua::new();

    // Load the test file to discover function names
    if let Err(e) = lua.load(&test_source).exec() {
        return vec![TestResult {
            name: test_file.display().to_string(),
            passed: false,
            error: Some(format!("Lua parse error: {}", e)),
            game_time: 0.0,
        }];
    }

    // Find all functions named test_*
    let globals = lua.globals();
    let mut test_names = Vec::new();
    if let Ok(pairs) = globals.pairs::<String, LuaValue>().collect::<Result<Vec<_>, _>>() {
        for (key, value) in pairs {
            if key.starts_with("test_") && matches!(value, LuaValue::Function(_)) {
                test_names.push(key);
            }
        }
    }
    test_names.sort();

    if test_names.is_empty() {
        return vec![TestResult {
            name: test_file.display().to_string(),
            passed: false,
            error: Some("No test_* functions found".into()),
            game_time: 0.0,
        }];
    }

    println!("Running {} tests...", test_names.len());

    let mut results = Vec::new();

    for test_name in &test_names {
        let result = run_single_test(project_root, &test_source, test_name);
        let status = if result.passed { "OK" } else { "FAIL" };
        println!(
            "  {} {} ({:.1}s game time)",
            status, result.name, result.game_time
        );
        if let Some(ref err) = result.error {
            println!("    Error: {}", err);
        }
        results.push(result);
    }

    results
}

/// Run a single test function in an isolated TestRunner.
fn run_single_test(project_root: &Path, test_source: &str, test_name: &str) -> TestResult {
    // Each test gets a fresh TestRunner, wrapped in Rc<RefCell<>> for safe sharing with Lua closures
    let runner = Rc::new(RefCell::new(TestRunner::new(project_root)));
    let start_time = std::time::Instant::now();

    // Create the test Lua VM with the test API
    let test_lua = Lua::new();

    // Register test API functions
    if let Err(e) = register_test_api(&test_lua, runner.clone()) {
        return TestResult {
            name: test_name.to_string(),
            passed: false,
            error: Some(format!("Failed to register test API: {}", e)),
            game_time: 0.0,
        };
    }

    // Load the test source
    if let Err(e) = test_lua.load(test_source).exec() {
        return TestResult {
            name: test_name.to_string(),
            passed: false,
            error: Some(format!("Lua load error: {}", e)),
            game_time: 0.0,
        };
    }

    // Call the test function
    let result = {
        let globals = test_lua.globals();
        match globals.get::<LuaFunction>(test_name) {
            Ok(func) => func.call::<()>(()),
            Err(e) => Err(e),
        }
    };

    let game_time = runner.borrow().total_time;
    let _elapsed = start_time.elapsed();

    match result {
        Ok(()) => TestResult {
            name: test_name.to_string(),
            passed: true,
            error: None,
            game_time,
        },
        Err(e) => TestResult {
            name: test_name.to_string(),
            passed: false,
            error: Some(format!("{}", e)),
            game_time,
        },
    }
}

/// Register the test API into a Lua state.
/// Uses Rc<RefCell<TestRunner>> for safe shared access from Lua closures.
fn register_test_api(lua: &Lua, runner: Rc<RefCell<TestRunner>>) -> Result<(), String> {
    let globals = lua.globals();

    // log.info(msg) — test logging
    let log_table = lua.create_table().map_err(|e| e.to_string())?;
    let log_info = lua
        .create_function(|_, msg: String| {
            println!("    [test] {}", msg);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    log_table
        .set("info", log_info)
        .map_err(|e| e.to_string())?;
    globals.set("log", log_table).map_err(|e| e.to_string())?;

    // scene.load(path) — load a scene
    let scene_table = lua.create_table().map_err(|e| e.to_string())?;
    let r = runner.clone();
    let scene_load = lua
        .create_function(move |_, path: String| {
            r.borrow_mut()
                .load_scene(&path)
                .map_err(|e| LuaError::RuntimeError(e))?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    scene_table
        .set("load", scene_load)
        .map_err(|e| e.to_string())?;

    // scene.find(entity_id) -> table with :get(component) method
    let r = runner.clone();
    let scene_find = lua
        .create_function(move |lua, id: String| {
            let runner = r.borrow();
            let sw = runner.scene_world.borrow();
            match sw.entity_registry.get(&id) {
                Some(&_entity) => {
                    drop(sw);
                    drop(runner);
                    let entity_ref = lua.create_table()?;
                    entity_ref.set("id", id.clone())?;

                    // entity:get("transform") -> { position = { x, y, z } }
                    let id_clone = id.clone();
                    let r2 = r.clone();
                    let get_fn = lua.create_function(move |lua, (_self_tbl, component): (LuaTable, String)| {
                        let runner = r2.borrow();
                        let sw = runner.scene_world.borrow();
                        let entity = match sw.entity_registry.get(&id_clone) {
                            Some(&e) => e,
                            None => return Err(LuaError::RuntimeError(format!("Entity '{}' not found", id_clone))),
                        };
                        match component.as_str() {
                            "transform" => {
                                if let Ok(t) = sw.world.get::<&Transform>(entity) {
                                    let tbl = lua.create_table()?;
                                    let pos = lua.create_table()?;
                                    pos.set("x", t.position.x)?;
                                    pos.set("y", t.position.y)?;
                                    pos.set("z", t.position.z)?;
                                    tbl.set("position", pos)?;
                                    Ok(LuaValue::Table(tbl))
                                } else {
                                    Ok(LuaNil)
                                }
                            }
                            "health" => {
                                let tbl = lua.create_table()?;
                                tbl.set("current", 100)?; // placeholder
                                Ok(LuaValue::Table(tbl))
                            }
                            _ => Ok(LuaNil),
                        }
                    })?;
                    entity_ref.set("get", get_fn)?;
                    Ok(LuaValue::Table(entity_ref))
                }
                None => Ok(LuaNil),
            }
        })
        .map_err(|e| e.to_string())?;
    scene_table
        .set("find", scene_find)
        .map_err(|e| e.to_string())?;
    globals
        .set("scene", scene_table)
        .map_err(|e| e.to_string())?;

    // input.inject(action, type, value) — inject input
    let input_table = lua.create_table().map_err(|e| e.to_string())?;
    let r = runner.clone();
    let input_inject = lua
        .create_function(move |_, (action, kind, value): (String, String, LuaValue)| {
            let runner = r.borrow_mut();
            let mut input = runner.input_state.borrow_mut();
            match kind.as_str() {
                "press" => {
                    let key = action_to_key(&action);
                    input.inject_key_press(&key);
                }
                "release" => {
                    let key = action_to_key(&action);
                    input.inject_key_release(&key);
                }
                "axis" => {
                    if let LuaValue::Table(tbl) = value {
                        let x: f32 = tbl.get(1).unwrap_or(0.0);
                        let y: f32 = tbl.get(2).unwrap_or(0.0);
                        if y > 0.0 {
                            input.inject_key_press("W");
                            input.inject_key_release("S");
                        } else if y < 0.0 {
                            input.inject_key_release("W");
                            input.inject_key_press("S");
                        } else {
                            input.inject_key_release("W");
                            input.inject_key_release("S");
                        }
                        if x > 0.0 {
                            input.inject_key_press("D");
                            input.inject_key_release("A");
                        } else if x < 0.0 {
                            input.inject_key_release("D");
                            input.inject_key_press("A");
                        } else {
                            input.inject_key_release("D");
                            input.inject_key_release("A");
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    input_table
        .set("inject", input_inject)
        .map_err(|e| e.to_string())?;
    globals
        .set("input", input_table)
        .map_err(|e| e.to_string())?;

    // wait_frames(n) — advance N frames
    let r = runner.clone();
    let wait_frames = lua
        .create_function(move |_, n: u64| {
            r.borrow_mut().step_frames(n);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_frames", wait_frames)
        .map_err(|e| e.to_string())?;

    // wait_seconds(n) — advance N seconds of game time
    let r = runner.clone();
    let wait_seconds = lua
        .create_function(move |_, n: f32| {
            r.borrow_mut().step_seconds(n);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_seconds", wait_seconds)
        .map_err(|e| e.to_string())?;

    // wait_for_event(event_type, timeout_seconds) — advance until event occurs
    let r = runner.clone();
    let wait_for_event = lua
        .create_function(move |_, (event_type, timeout): (String, Option<f32>)| {
            let timeout = timeout.unwrap_or(10.0);
            let delta_time = r.borrow().delta_time;
            let max_frames = (timeout / delta_time) as u64;
            let empty_filter = std::collections::HashMap::new();
            for _ in 0..max_frames {
                if r.borrow().event_occurred(&event_type, &empty_filter) {
                    return Ok(true);
                }
                r.borrow_mut().step_frame();
            }
            Ok(r.borrow().event_occurred(&event_type, &empty_filter))
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_for_event", wait_for_event)
        .map_err(|e| e.to_string())?;

    // wait_until(fn, timeout) — advance until condition is true
    let r = runner.clone();
    let wait_until_fn = lua
        .create_function(move |_, (func, timeout): (LuaFunction, Option<f32>)| {
            let timeout = timeout.unwrap_or(10.0);
            let delta_time = r.borrow().delta_time;
            let max_frames = (timeout / delta_time) as u64;
            for _ in 0..max_frames {
                r.borrow_mut().step_frame();
                if let Ok(result) = func.call::<bool>(()) {
                    if result {
                        return Ok(true);
                    }
                }
            }
            Err(LuaError::RuntimeError(format!(
                "wait_until timed out after {:.1}s",
                timeout
            )))
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_until", wait_until_fn)
        .map_err(|e| e.to_string())?;

    // event_occurred(event_type, filter_table) -> bool
    let r = runner.clone();
    let event_occurred_fn = lua
        .create_function(move |_, (event_type, filter): (String, Option<LuaTable>)| {
            let runner = r.borrow();
            let mut filter_map = std::collections::HashMap::new();
            if let Some(tbl) = filter {
                for pair in tbl.pairs::<String, String>() {
                    if let Ok((k, v)) = pair {
                        filter_map.insert(k, v);
                    }
                }
            }
            Ok(runner.event_occurred(&event_type, &filter_map))
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("event_occurred", event_occurred_fn)
        .map_err(|e| e.to_string())?;

    // get_position(entity_id) -> x, y, z
    let r = runner.clone();
    let get_position = lua
        .create_function(move |_, id: String| {
            let runner = r.borrow();
            match runner.get_entity_position(&id) {
                Some(pos) => Ok((pos.x, pos.y, pos.z)),
                None => Ok((0.0f32, 0.0f32, 0.0f32)),
            }
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("get_position", get_position)
        .map_err(|e| e.to_string())?;

    // get_game_value(key) -> value from the script runtime's game table
    let r = runner.clone();
    let get_game_value = lua
        .create_function(move |_, key: String| {
            let runner = r.borrow();
            let globals = runner.script_runtime.lua.globals();
            if let Ok(game_table) = globals.get::<LuaTable>("game") {
                // Try integer first (player_health = 100)
                if let Ok(i) = game_table.get::<i64>(key.clone()) {
                    return Ok(LuaValue::Integer(i));
                }
                // Try number
                if let Ok(n) = game_table.get::<f64>(key.clone()) {
                    return Ok(LuaValue::Number(n));
                }
                // Try boolean (game_over, level_complete)
                if let Ok(b) = game_table.get::<bool>(key.clone()) {
                    return Ok(LuaValue::Boolean(b));
                }
            }
            Ok(LuaValue::Nil)
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("get_game_value", get_game_value)
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Map test action names to key names for synthetic injection.
fn action_to_key(action: &str) -> String {
    match action {
        "interact" => "E".to_string(),
        "move_forward" | "move" | "forward" => "W".to_string(),
        "move_backward" | "backward" => "S".to_string(),
        "move_left" | "left" => "A".to_string(),
        "move_right" | "right" => "D".to_string(),
        "jump" => "Space".to_string(),
        "sprint" => "ShiftLeft".to_string(),
        "fire" | "attack" => "Left".to_string(),
        _ => action.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_to_key_mapping() {
        assert_eq!(action_to_key("interact"), "E");
        assert_eq!(action_to_key("jump"), "Space");
        assert_eq!(action_to_key("move_forward"), "W");
    }
}
