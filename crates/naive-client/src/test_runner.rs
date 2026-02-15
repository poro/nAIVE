//! Phase 10: Headless test runner for automated gameplay testing.
//!
//! Runs Lua test scripts that inject input, advance game time, and assert
//! that game events occurred. No GPU or window required.

use std::path::{Path, PathBuf};

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
pub struct TestRunner {
    pub project_root: PathBuf,
    pub scene_world: SceneWorld,
    pub input_state: InputState,
    pub physics_world: PhysicsWorld,
    pub script_runtime: ScriptRuntime,
    pub event_bus: EventBus,
    pub tween_system: TweenSystem,
    pub delta_time: f32,
    pub total_time: f32,
    pub frame_count: u64,
}

impl TestRunner {
    pub fn new(project_root: &Path) -> Self {
        let bindings = crate::input::load_bindings(project_root);
        Self {
            project_root: project_root.to_path_buf(),
            scene_world: SceneWorld::new(),
            input_state: InputState::new(bindings),
            physics_world: PhysicsWorld::new(glam::Vec3::new(0.0, -9.81, 0.0)),
            script_runtime: ScriptRuntime::new(),
            event_bus: EventBus::new(1000),
            tween_system: TweenSystem::new(),
            delta_time: 1.0 / 60.0,
            total_time: 0.0,
            frame_count: 0,
        }
    }

    /// Load a scene by path (relative to project root).
    pub fn load_scene(&mut self, scene_rel: &str) -> Result<(), String> {
        let scene_path = self.project_root.join(scene_rel);
        let scene = crate::scene::load_scene(&scene_path)
            .map_err(|e| format!("Failed to load scene: {:?}", e))?;

        // Set gravity from scene settings
        let gravity = glam::Vec3::from(scene.settings.gravity);
        self.physics_world = PhysicsWorld::new(gravity);

        // Spawn entities headlessly (no GPU)
        self.scene_world = SceneWorld::new();
        crate::world::spawn_all_entities_headless(
            &mut self.scene_world,
            &scene,
            &mut self.physics_world,
        );

        // Initialize scripting
        self.script_runtime = ScriptRuntime::new();
        if let Err(e) = self.script_runtime.register_api() {
            return Err(format!("Failed to register script API: {}", e));
        }

        // Register input API
        let input_ptr = &self.input_state as *const InputState;
        self.script_runtime
            .register_input_api(input_ptr)
            .map_err(|e| format!("Input API: {}", e))?;

        // Register physics API
        let physics_ptr = &self.physics_world as *const PhysicsWorld;
        let sw_const_ptr = &self.scene_world as *const SceneWorld;
        self.script_runtime
            .register_physics_api(physics_ptr, sw_const_ptr)
            .map_err(|e| format!("Physics API: {}", e))?;

        // Register entity API
        let sw_ptr = &mut self.scene_world as *mut SceneWorld;
        self.script_runtime
            .register_entity_api(sw_ptr)
            .map_err(|e| format!("Entity API: {}", e))?;

        // Register event bus API
        let bus_ptr = &mut self.event_bus as *mut EventBus;
        self.script_runtime
            .register_event_api(bus_ptr)
            .map_err(|e| format!("Event API: {}", e))?;

        // Load event schema
        self.event_bus.load_schema(&self.project_root);

        // Load scripts for entities
        if let Some(scene_data) = &self.scene_world.current_scene {
            let scene_clone = scene_data.clone();
            for entity_def in &scene_clone.entities {
                if let Some(script_def) = &entity_def.components.script {
                    if let Some(&entity) = self.scene_world.entity_registry.get(&entity_def.id) {
                        let source_path = PathBuf::from(&script_def.source);
                        let script_comp = Script {
                            source: source_path.clone(),
                            initialized: false,
                        };
                        let _ = self.scene_world.world.insert_one(entity, script_comp);

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
        let uninit: Vec<hecs::Entity> = self.scene_world.world.query::<&Script>()
            .iter()
            .filter(|(_, s)| !s.initialized)
            .map(|(e, _)| e)
            .collect();
        for entity in uninit {
            self.script_runtime.call_init(entity);
        }
        for (_entity, script) in self.scene_world.world.query::<&mut Script>().iter() {
            script.initialized = true;
        }

        // Emit lifecycle event
        self.event_bus
            .emit("lifecycle.scene_loaded", std::collections::HashMap::new());
        self.event_bus.flush();

        tracing::info!("Test runner: scene loaded");
        Ok(())
    }

    /// Advance the simulation by one frame.
    pub fn step_frame(&mut self) {
        let dt = self.delta_time;

        // Apply synthetic inputs
        self.input_state.begin_frame();

        // Auto-capture cursor for FPS controller
        self.input_state.cursor_captured = true;

        // FPS controller update
        self.update_fps_controller(dt);

        // Update all scripts (collect first to release world borrow before Lua runs)
        let scripted: Vec<hecs::Entity> = self.scene_world.world.query::<&Script>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        for entity in scripted {
            self.script_runtime.call_update(entity, dt);
        }

        // Tick event bus and tweens
        self.event_bus.tick(dt as f64);
        self.event_bus.flush();
        let _tween_results = self.tween_system.update(dt);

        // Update transforms
        crate::transform::update_transforms(&mut self.scene_world.world);

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
        for event in self.event_bus.get_log() {
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
        let entity = *self.scene_world.entity_registry.get(entity_id)?;
        let transform = self.scene_world.world.get::<&Transform>(entity).ok()?;
        Some(transform.position)
    }

    /// FPS controller update (replicated from engine.rs for headless).
    fn update_fps_controller(&mut self, dt: f32) {
        // Collect player data first to avoid borrow conflicts
        let player_data: Vec<_> = self
            .scene_world
            .world
            .query::<(&Player, &CharacterController, &RigidBodyComp, &ColliderComp)>()
            .iter()
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
            .collect();

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
            let mouse_delta = self.input_state.mouse_delta();
            let sensitivity = 0.002;
            let new_yaw = yaw - mouse_delta.x * sensitivity;
            let new_pitch = (pitch - mouse_delta.y * sensitivity).clamp(
                -std::f32::consts::FRAC_PI_2 + 0.01,
                std::f32::consts::FRAC_PI_2 - 0.01,
            );

            let move_input = self.input_state.axis_2d(
                "move_forward",
                "move_backward",
                "move_left",
                "move_right",
            );
            let speed = if self.input_state.pressed("sprint") {
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
                if self.input_state.just_pressed("jump") {
                    vel_y = jump_impulse;
                }
            }
            vel_y += self.physics_world.gravity.y * dt;
            desired.y = vel_y * dt;

            let (_effective, new_grounded) =
                self.physics_world
                    .move_character(rb_handle, col_handle, desired, dt);

            // Update player + character controller
            if let Ok(mut player) = self.scene_world.world.get::<&mut Player>(entity) {
                player.yaw = new_yaw;
                player.pitch = new_pitch;
            }
            if let Ok(mut cc) = self
                .scene_world
                .world
                .get::<&mut CharacterController>(entity)
            {
                cc.grounded = new_grounded;
                cc.velocity.y = vel_y;
            }

            // Sync physics → transform
            self.physics_world.sync_to_ecs(&mut self.scene_world.world);
        }

        self.physics_world.step(dt);
        self.physics_world
            .sync_to_ecs(&mut self.scene_world.world);
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
    // Each test gets a fresh TestRunner
    let mut runner = TestRunner::new(project_root);
    let start_time = std::time::Instant::now();

    // Create the test Lua VM with the test API
    let test_lua = Lua::new();

    // Register test API functions
    if let Err(e) = register_test_api(&test_lua, &mut runner) {
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

    let game_time = runner.total_time;
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
/// Uses raw pointers to the TestRunner for the API functions.
fn register_test_api(lua: &Lua, runner: &mut TestRunner) -> Result<(), String> {
    let globals = lua.globals();
    let runner_ptr = runner as *mut TestRunner;

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
    let scene_load = lua
        .create_function(move |_, path: String| {
            let runner = unsafe { &mut *runner_ptr };
            runner
                .load_scene(&path)
                .map_err(|e| LuaError::RuntimeError(e))?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    scene_table
        .set("load", scene_load)
        .map_err(|e| e.to_string())?;

    // scene.find(entity_id) -> table with :get(component) method
    let scene_find = lua
        .create_function(move |lua, id: String| {
            let runner = unsafe { &*runner_ptr };
            match runner.scene_world.entity_registry.get(&id) {
                Some(&_entity) => {
                    let entity_ref = lua.create_table()?;
                    entity_ref.set("id", id.clone())?;

                    // entity:get("transform") -> { position = { x, y, z } }
                    let id_clone = id.clone();
                    let get_fn = lua.create_function(move |lua, (_self_tbl, component): (LuaTable, String)| {
                        let runner = unsafe { &*runner_ptr };
                        let entity = match runner.scene_world.entity_registry.get(&id_clone) {
                            Some(&e) => e,
                            None => return Err(LuaError::RuntimeError(format!("Entity '{}' not found", id_clone))),
                        };
                        match component.as_str() {
                            "transform" => {
                                if let Ok(t) = runner.scene_world.world.get::<&Transform>(entity) {
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
                                // Read from game table in script runtime
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
    let input_inject = lua
        .create_function(move |_, (action, kind, value): (String, String, LuaValue)| {
            let runner = unsafe { &mut *runner_ptr };
            match kind.as_str() {
                "press" => {
                    // Map action name to key name for injection
                    let key = action_to_key(&action);
                    runner.input_state.inject_key_press(&key);
                }
                "release" => {
                    let key = action_to_key(&action);
                    runner.input_state.inject_key_release(&key);
                }
                "axis" => {
                    // value is a table {x, y} for movement
                    if let LuaValue::Table(tbl) = value {
                        let x: f32 = tbl.get(1).unwrap_or(0.0);
                        let y: f32 = tbl.get(2).unwrap_or(0.0);
                        // Only release keys we're NOT pressing (avoid same-frame cancel)
                        if y > 0.0 {
                            runner.input_state.inject_key_press("W");
                            runner.input_state.inject_key_release("S");
                        } else if y < 0.0 {
                            runner.input_state.inject_key_release("W");
                            runner.input_state.inject_key_press("S");
                        } else {
                            runner.input_state.inject_key_release("W");
                            runner.input_state.inject_key_release("S");
                        }
                        if x > 0.0 {
                            runner.input_state.inject_key_press("D");
                            runner.input_state.inject_key_release("A");
                        } else if x < 0.0 {
                            runner.input_state.inject_key_release("D");
                            runner.input_state.inject_key_press("A");
                        } else {
                            runner.input_state.inject_key_release("D");
                            runner.input_state.inject_key_release("A");
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
    let wait_frames = lua
        .create_function(move |_, n: u64| {
            let runner = unsafe { &mut *runner_ptr };
            runner.step_frames(n);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_frames", wait_frames)
        .map_err(|e| e.to_string())?;

    // wait_seconds(n) — advance N seconds of game time
    let wait_seconds = lua
        .create_function(move |_, n: f32| {
            let runner = unsafe { &mut *runner_ptr };
            runner.step_seconds(n);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_seconds", wait_seconds)
        .map_err(|e| e.to_string())?;

    // wait_for_event(event_type, timeout_seconds) — advance until event occurs
    let wait_for_event = lua
        .create_function(move |_, (event_type, timeout): (String, Option<f32>)| {
            let runner = unsafe { &mut *runner_ptr };
            let timeout = timeout.unwrap_or(10.0);
            let max_frames = (timeout / runner.delta_time) as u64;
            let empty_filter = std::collections::HashMap::new();
            for _ in 0..max_frames {
                if runner.event_occurred(&event_type, &empty_filter) {
                    return Ok(true);
                }
                runner.step_frame();
            }
            Ok(runner.event_occurred(&event_type, &empty_filter))
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_for_event", wait_for_event)
        .map_err(|e| e.to_string())?;

    // wait_until(fn, timeout) — advance until condition is true
    let wait_until_fn = lua
        .create_function(move |_, (func, timeout): (LuaFunction, Option<f32>)| {
            let runner = unsafe { &mut *runner_ptr };
            let timeout = timeout.unwrap_or(10.0);
            let max_frames = (timeout / runner.delta_time) as u64;
            for _ in 0..max_frames {
                runner.step_frame();
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
    let event_occurred_fn = lua
        .create_function(move |_, (event_type, filter): (String, Option<LuaTable>)| {
            let runner = unsafe { &*runner_ptr };
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
    let get_position = lua
        .create_function(move |_, id: String| {
            let runner = unsafe { &*runner_ptr };
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
    // Uses typed getters to extract primitives safely across Lua VMs.
    let get_game_value = lua
        .create_function(move |_, key: String| {
            let runner = unsafe { &*runner_ptr };
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
        "fire" | "attack" => "Left".to_string(), // mouse button - handled specially
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
