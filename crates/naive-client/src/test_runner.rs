//! Phase 10: Headless test runner for automated gameplay testing.
//!
//! Runs Lua test scripts that inject input, advance game time, and assert
//! that game events occurred. No GPU or window required.

mod limits;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mlua::prelude::*;

use self::limits::{create_lua, TestBudget, TestLimits};
use crate::components::Transform;
use crate::events::EventBus;
use crate::input::InputState;
use crate::physics::PhysicsWorld;
use crate::scripting::ScriptRuntime;
use crate::swarm::CpuSwarms;
use crate::swarm_api::{register_swarm_api, SharedSwarmBus, SwarmBus};
use crate::tween::TweenSystem;
use crate::world::{EntityCommandQueue, EntityPoolManager, SceneWorld};

/// Result of a single test function execution.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
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
    entity_commands: Rc<RefCell<EntityCommandQueue>>,
    pool_manager: Rc<RefCell<EntityPoolManager>>,
    /// Swarms run on the CPU here, with the same maths as the GPU shaders.
    pub swarm_bus: SharedSwarmBus,
    pub cpu_swarms: CpuSwarms,
    /// Failures remain sticky across scene changes and caught Lua errors.
    errors: RefCell<Vec<String>>,
    execution_budget: Rc<TestBudget>,
}

impl TestRunner {
    pub fn new(project_root: &Path) -> Self {
        Self::with_limits(project_root, TestLimits::default())
    }

    fn with_limits(project_root: &Path, limits: TestLimits) -> Self {
        let bindings = crate::input::load_bindings(project_root);
        Self {
            project_root: project_root.to_path_buf(),
            scene_world: Rc::new(RefCell::new(SceneWorld::new())),
            input_state: Rc::new(RefCell::new(InputState::new(bindings))),
            physics_world: Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::new(
                0.0, -9.81, 0.0,
            )))),
            script_runtime: ScriptRuntime::new(),
            event_bus: Rc::new(RefCell::new(EventBus::new(1000))),
            tween_system: TweenSystem::new(),
            delta_time: 1.0 / 60.0,
            total_time: 0.0,
            frame_count: 0,
            lua_event_listeners: Rc::new(RefCell::new(HashMap::new())),
            next_lua_listener_id: Rc::new(RefCell::new(0)),
            lua_listener_id_map: Rc::new(RefCell::new(HashMap::new())),
            entity_commands: Rc::new(RefCell::new(EntityCommandQueue::new())),
            pool_manager: Rc::new(RefCell::new(EntityPoolManager::new())),
            swarm_bus: Rc::new(RefCell::new(SwarmBus::new())),
            cpu_swarms: CpuSwarms::new(),
            errors: RefCell::new(Vec::new()),
            execution_budget: TestBudget::new(limits),
        }
    }

    /// Load a scene by path (relative to project root).
    pub fn load_scene(&mut self, scene_rel: &str) -> Result<(), String> {
        let result = self
            .execution_budget
            .check()
            .map_err(|error| error.to_string())
            .and_then(|()| self.load_scene_inner(scene_rel))
            .and_then(|()| {
                self.execution_budget
                    .check()
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = &result {
            self.errors.borrow_mut().push(error.clone());
        }
        result
    }

    fn load_scene_inner(&mut self, scene_rel: &str) -> Result<(), String> {
        let scene_path = crate::project_path::resolve(&self.project_root, scene_rel)?;
        let scene = crate::scene::load_scene(&scene_path)
            .map_err(|e| format!("Failed to load scene: {:?}", e))?;
        for definition in &scene.entities {
            validate_headless_entity(definition)?;
        }

        // Use the same teardown as live destruction, without holding a world
        // borrow while callbacks run. Preserve failures before replacing the VM.
        let ids: Vec<_> = self
            .scene_world
            .borrow()
            .entity_registry
            .keys()
            .cloned()
            .collect();
        for id in ids {
            crate::sim::destroy_entity(
                &self.scene_world,
                Some(&self.physics_world),
                Some(&mut self.script_runtime),
                &id,
            );
        }
        let previous_errors = self.take_errors();
        self.errors.borrow_mut().extend(previous_errors);
        self.lua_event_listeners.borrow_mut().clear();
        self.lua_listener_id_map.borrow_mut().clear();
        *self.next_lua_listener_id.borrow_mut() = 0;
        *self.pool_manager.borrow_mut() = EntityPoolManager::new();
        *self.event_bus.borrow_mut() = EventBus::new(1000);

        // Set gravity from scene settings
        let gravity = glam::Vec3::from(scene.settings.gravity);
        *self.physics_world.borrow_mut() = PhysicsWorld::new(gravity);

        // Spawn entities headlessly (no GPU)
        *self.scene_world.borrow_mut() = SceneWorld::new();
        {
            let mut sw = self.scene_world.borrow_mut();
            let mut pw = self.physics_world.borrow_mut();
            crate::world::spawn_all_entities_headless(&mut sw, &scene, &mut pw);
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
        *self.entity_commands.borrow_mut() = EntityCommandQueue::new();
        self.script_runtime
            .register_entity_command_api(
                self.scene_world.clone(),
                self.entity_commands.clone(),
                self.pool_manager.clone(),
            )
            .map_err(|e| format!("Entity command API: {}", e))?;
        self.script_runtime
            .register_event_api(
                self.event_bus.clone(),
                self.lua_event_listeners.clone(),
                self.next_lua_listener_id.clone(),
                self.lua_listener_id_map.clone(),
            )
            .map_err(|e| format!("Event API: {}", e))?;
        self.script_runtime
            .register_audio_api(
                Rc::new(RefCell::new(crate::audio::AudioSystem::silent())),
                self.project_root.clone(),
            )
            .map_err(|e| format!("Audio API: {e}"))?;
        self.script_runtime
            .register_mesh_api(self.entity_commands.clone())
            .map_err(|e| format!("Mesh API: {e}"))?;
        *self.swarm_bus.borrow_mut() = SwarmBus::new();
        self.cpu_swarms.clear();
        register_swarm_api(
            &self.script_runtime.lua,
            self.swarm_bus.clone(),
            self.scene_world.clone(),
        )
        .map_err(|e| format!("Swarm API: {}", e))?;

        // Load event schema
        self.event_bus.borrow_mut().load_schema(&self.project_root);

        crate::sim::initialize_pending_scripts(
            &self.scene_world,
            &mut self.script_runtime,
            &self.project_root,
        )?;
        let errors = self.script_runtime.take_errors();
        if !errors.is_empty() {
            return Err(errors.join("\n"));
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
        if self.execution_budget.check().is_err() {
            return;
        }
        let dt = self.delta_time;

        // Apply synthetic inputs
        self.input_state.borrow_mut().begin_frame();

        // Auto-capture cursor for FPS controller
        self.input_state.borrow_mut().cursor_captured = true;

        // The same steps the engine runs each frame, minus the ones that need a GPU.
        crate::sim::update_fps_controller(
            &self.input_state,
            &self.scene_world,
            &self.physics_world,
            dt,
        );
        crate::sim::run_fixed_steps(
            &self.physics_world,
            &self.scene_world,
            Some(&self.script_runtime),
            dt,
        );
        crate::sim::dispatch_physics_events(
            &self.scene_world,
            &self.physics_world,
            &self.script_runtime,
        );
        crate::sim::process_collision_damage(
            &self.scene_world,
            &self.physics_world,
            &self.script_runtime,
            &self.entity_commands,
        );
        crate::sim::update_projectiles(&self.scene_world, &self.entity_commands, dt);
        crate::sim::process_swarm_reports(
            &self.swarm_bus,
            &self.scene_world,
            &self.script_runtime,
            &self.event_bus,
        );
        crate::sim::process_health(&self.scene_world, &self.script_runtime);
        crate::sim::update_scripts(&self.scene_world, &self.script_runtime, dt);
        self.process_entity_commands();
        crate::sim::dispatch_lua_events(
            &self.event_bus,
            &self.lua_event_listeners,
            Some(&self.script_runtime),
            dt,
        );
        let _tween_results = self.tween_system.update(dt);

        // Update transforms
        {
            let mut sw = self.scene_world.borrow_mut();
            crate::transform::update_transforms(&mut sw.world);
        }

        // Swarms step after the transforms, where the engine steps them on the GPU.
        self.cpu_swarms.update(
            &self.scene_world.borrow(),
            Some(&self.physics_world.borrow()),
            &mut self.swarm_bus.borrow_mut(),
            dt,
        );

        self.total_time += dt;
        self.frame_count += 1;
    }

    /// Apply gameplay operations through the same constructors/lifecycle as
    /// the engine. GPU-only operations fail the test explicitly.
    fn process_entity_commands(&mut self) {
        crate::sim::process_destroys(
            &self.scene_world,
            Some(&self.physics_world),
            Some(&mut self.script_runtime),
            &self.entity_commands,
        );
        let (spawns, projectiles, dynamics, scales, visibility, pools, scene_load, gpu_ops) = {
            let mut queue = self.entity_commands.borrow_mut();
            let gpu_ops = queue.texture_swaps.len() + queue.mesh_creates.len();
            queue.texture_swaps.clear();
            queue.mesh_creates.clear();
            (
                std::mem::take(&mut queue.spawns),
                std::mem::take(&mut queue.projectile_spawns),
                std::mem::take(&mut queue.dynamic_spawns),
                std::mem::take(&mut queue.scale_updates),
                std::mem::take(&mut queue.visibility_updates),
                std::mem::take(&mut queue.pool_ops),
                queue.pending_scene_load.take(),
                gpu_ops,
            )
        };
        if gpu_ops > 0 {
            self.errors.borrow_mut().push("Headless tests cannot execute GPU mesh/texture commands; use the native release smoke test".into());
        }

        for command in spawns {
            if self.spawn_definition(&command.entity_def()).is_some() {
                crate::sim::finish_pool_spawn(&self.scene_world, &self.pool_manager, &command);
            }
        }
        for command in projectiles {
            use crate::scene::*;
            let definition = EntityDef {
                id: command.id.clone(),
                tags: vec!["projectile".into()],
                extends: None,
                components: ComponentMap {
                    transform: Some(TransformDef {
                        position: command.position,
                        rotation: [0.0; 3],
                        scale: command.scale,
                    }),
                    mesh_renderer: Some(MeshRendererDef {
                        mesh: command.mesh.clone(),
                        material: command.material.clone(),
                        cast_shadows: true,
                        receive_shadows: true,
                    }),
                    rigid_body: Some(RigidBodyDef {
                        body_type: "dynamic".into(),
                        mass: 0.1,
                        ccd: true,
                    }),
                    collider: Some(ColliderDef {
                        shape: "sphere".into(),
                        half_extents: None,
                        radius: Some(0.1),
                        half_height: None,
                        is_trigger: false,
                        restitution: 0.0,
                        friction: 0.5,
                    }),
                    collision_damage: Some(CollisionDamageDef {
                        damage: command.damage,
                        destroy_on_hit: true,
                    }),
                    ..Default::default()
                },
            };
            if let Some(entity) = self.spawn_definition(&definition) {
                let mut world = self.scene_world.borrow_mut();
                let _ = world.world.insert_one(
                    entity,
                    crate::components::Projectile {
                        damage: command.damage,
                        lifetime: command.lifetime,
                        age: 0.0,
                        owner_id: command.owner_id,
                    },
                );
                if let Ok(body) = world.world.get::<&crate::physics::RigidBody>(entity) {
                    self.physics_world.borrow_mut().set_linvel(
                        body.handle,
                        glam::Vec3::from(command.direction).normalize_or_zero() * command.speed,
                        !command.gravity,
                    );
                };
            }
        }
        for command in dynamics {
            use crate::scene::*;
            let definition = EntityDef {
                id: command.id.clone(),
                tags: vec!["dynamic".into()],
                extends: None,
                components: ComponentMap {
                    transform: Some(TransformDef {
                        position: command.position,
                        rotation: [0.0; 3],
                        scale: [command.radius * 2.0; 3],
                    }),
                    mesh_renderer: Some(MeshRendererDef {
                        mesh: command.mesh,
                        material: command.material,
                        cast_shadows: true,
                        receive_shadows: true,
                    }),
                    rigid_body: Some(RigidBodyDef {
                        body_type: "dynamic".into(),
                        mass: command.mass,
                        ccd: false,
                    }),
                    collider: Some(ColliderDef {
                        shape: "sphere".into(),
                        half_extents: None,
                        radius: Some(command.radius),
                        half_height: None,
                        is_trigger: false,
                        restitution: command.restitution,
                        friction: command.friction,
                    }),
                    ..Default::default()
                },
            };
            if let Some(entity) = self.spawn_definition(&definition) {
                let mut world = self.scene_world.borrow_mut();
                let _ = world.world.insert_one(
                    entity,
                    crate::components::Projectile {
                        damage: 0.0,
                        lifetime: command.lifetime,
                        age: 0.0,
                        owner_id: String::new(),
                    },
                );
                if let Ok(body) = world.world.get::<&crate::physics::RigidBody>(entity) {
                    let mut physics = self.physics_world.borrow_mut();
                    physics.set_linvel(body.handle, glam::Vec3::from(command.velocity), false);
                    if let Some(body) = physics.rigid_body_set.get_mut(body.handle) {
                        body.activation_mut().normalized_linear_threshold = -1.0;
                    }
                };
            }
        }
        for (id, scale) in scales {
            let world = self.scene_world.borrow();
            if let Some(&entity) = world.entity_registry.get(&id) {
                if let Ok(mut transform) = world.world.get::<&mut Transform>(entity) {
                    transform.scale = glam::Vec3::from(scale);
                    transform.dirty = true;
                }
            }
        }
        for (id, visible) in visibility {
            let mut world = self.scene_world.borrow_mut();
            if let Some(&entity) = world.entity_registry.get(&id) {
                if visible {
                    let _ = world.world.remove_one::<crate::components::Hidden>(entity);
                } else {
                    let _ = world.world.insert_one(entity, crate::components::Hidden);
                }
            }
        }
        for operation in pools {
            match operation {
                crate::world::PoolOp::Release(id) => {
                    let mut world = self.scene_world.borrow_mut();
                    if let Some(&entity) = world.entity_registry.get(&id) {
                        let _ = world.world.insert_one(entity, crate::components::Hidden);
                        if let Ok(mut pooled) =
                            world.world.get::<&mut crate::components::Pooled>(entity)
                        {
                            pooled.active = false;
                            self.pool_manager
                                .borrow_mut()
                                .release(&pooled.pool_name, &id);
                        }
                    }
                }
            }
        }
        if let Err(error) = crate::sim::initialize_pending_scripts(
            &self.scene_world,
            &mut self.script_runtime,
            &self.project_root,
        ) {
            self.errors.borrow_mut().push(error);
        }
        if let Some(path) = scene_load {
            let _ = self.load_scene(&path); // load_scene records the failure for the test result.
        }
    }

    fn spawn_definition(&self, definition: &crate::scene::EntityDef) -> Option<hecs::Entity> {
        if let Err(error) = validate_headless_entity(definition) {
            self.errors.borrow_mut().push(error);
            return None;
        }
        let mut world = self.scene_world.borrow_mut();
        if world.entity_registry.contains_key(&definition.id) {
            self.errors
                .borrow_mut()
                .push(format!("Cannot spawn duplicate entity '{}'", definition.id));
            return None;
        }
        crate::world::spawn_entity_headless(
            &mut world,
            definition,
            &mut self.physics_world.borrow_mut(),
        );
        world.entity_registry.get(&definition.id).copied()
    }

    pub fn take_errors(&self) -> Vec<String> {
        let mut errors = std::mem::take(&mut *self.errors.borrow_mut());
        errors.extend(self.script_runtime.take_errors());
        let pending = self.entity_commands.borrow();
        if !pending.mesh_creates.is_empty() || !pending.texture_swaps.is_empty() {
            errors.push("Headless tests cannot execute GPU mesh/texture commands; use the native release smoke test".into());
        }
        errors
    }

    /// Advance multiple frames.
    pub fn step_frames(&mut self, count: u64) {
        for _ in 0..count {
            if self.execution_budget.check().is_err() {
                break;
            }
            self.step_frame();
        }
    }

    /// Advance by a given number of seconds (at fixed timestep).
    pub fn step_seconds(&mut self, seconds: f32) {
        let frames = (seconds / self.delta_time).ceil() as u64;
        self.step_frames(frames);
    }

    /// Check if a specific event occurred in the log.
    pub fn event_occurred(
        &self,
        event_type: &str,
        filter: &std::collections::HashMap<String, String>,
    ) -> bool {
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
}

fn validate_headless_entity(definition: &crate::scene::EntityDef) -> Result<(), String> {
    if definition
        .components
        .collider
        .as_ref()
        .is_some_and(|collider| collider.shape == "trimesh")
    {
        return Err(format!("Headless tests cannot construct trimesh collider for '{}'; use the native release smoke test", definition.id));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Test execution: discover and run test functions from a Lua file
// ---------------------------------------------------------------------------

/// Run all test functions in a Lua test file. Returns results for each test.
/// Runtime code generation (`load`) and `__gc` finalizers are unavailable in
/// test code because they can suppress allocation errors or bypass VM hooks.
pub fn run_test_file(project_root: &Path, test_file: &Path) -> Vec<TestResult> {
    run_test_file_with_limits(project_root, test_file, TestLimits::default())
}

/// Run without printing statuses when a parent process reports the results.
/// Test-authored stdout, including log.info, remains available to the caller.
pub(crate) fn run_test_file_quiet(project_root: &Path, test_file: &Path) -> Vec<TestResult> {
    run_test_file_impl(project_root, test_file, TestLimits::default(), false)
}

fn run_test_file_with_limits(
    project_root: &Path,
    test_file: &Path,
    limits: TestLimits,
) -> Vec<TestResult> {
    run_test_file_impl(project_root, test_file, limits, true)
}

fn run_test_file_impl(
    project_root: &Path,
    test_file: &Path,
    limits: TestLimits,
    print_results: bool,
) -> Vec<TestResult> {
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
    let budget = TestBudget::new(limits);
    let lua = match budget.finish(create_lua(budget.clone())) {
        Ok(lua) => lua,
        Err(error) => {
            return vec![TestResult {
                name: test_file.display().to_string(),
                passed: false,
                error: Some(format!("Failed to create test discovery VM: {error}")),
                game_time: 0.0,
            }];
        }
    };

    // Load the test file to discover function names
    if let Err(e) = budget.finish(lua.load(&test_source).exec()) {
        return vec![TestResult {
            name: test_file.display().to_string(),
            passed: false,
            error: Some(format!("Lua test discovery error: {}", e)),
            game_time: 0.0,
        }];
    }

    // Find all functions named test_*
    let globals = lua.globals();
    let mut test_names = Vec::new();
    if let Ok(pairs) = globals
        .pairs::<String, LuaValue>()
        .collect::<Result<Vec<_>, _>>()
    {
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

    if print_results {
        println!("Running {} tests...", test_names.len());
    }

    let mut results = Vec::new();

    for test_name in &test_names {
        let result = run_single_test_with_limits(project_root, &test_source, test_name, limits);
        if print_results {
            let status = if result.passed { "OK" } else { "FAIL" };
            println!(
                "  {} {} ({:.1}s game time)",
                status, result.name, result.game_time
            );
            if let Some(ref err) = result.error {
                println!("    Error: {}", err);
            }
        }
        results.push(result);
    }

    results
}

/// Run a single test function in an isolated TestRunner.
#[cfg(test)]
fn run_single_test(project_root: &Path, test_source: &str, test_name: &str) -> TestResult {
    run_single_test_with_limits(project_root, test_source, test_name, TestLimits::default())
}

fn run_single_test_with_limits(
    project_root: &Path,
    test_source: &str,
    test_name: &str,
    limits: TestLimits,
) -> TestResult {
    // Each test gets a fresh TestRunner, wrapped in Rc<RefCell<>> for safe sharing with Lua closures
    let runner = Rc::new(RefCell::new(TestRunner::with_limits(project_root, limits)));
    let budget = runner.borrow().execution_budget.clone();

    // Create the test Lua VM with the test API
    let test_lua = match budget.finish(create_lua(budget.clone())) {
        Ok(lua) => lua,
        Err(error) => {
            return TestResult {
                name: test_name.to_string(),
                passed: false,
                error: Some(format!("Failed to create test VM: {error}")),
                game_time: 0.0,
            };
        }
    };

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
    if let Err(e) = budget.finish(test_lua.load(test_source).exec()) {
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
    let mut errors = runner.borrow().take_errors();
    if let Err(error) = budget.finish(result) {
        errors.insert(0, error.to_string());
    }
    TestResult {
        name: test_name.to_string(),
        passed: errors.is_empty(),
        error: (!errors.is_empty()).then(|| errors.join("\n")),
        game_time,
    }
}

/// Register the test API into a Lua state.
/// Uses Rc<RefCell<TestRunner>> for safe shared access from Lua closures.
fn register_test_api(lua: &Lua, runner: Rc<RefCell<TestRunner>>) -> Result<(), String> {
    let globals = lua.globals();

    // The same `swarm` table scene scripts get, so tests can fire at swarms
    // and read their counts.
    {
        let r = runner.borrow();
        register_swarm_api(lua, r.swarm_bus.clone(), r.scene_world.clone())?;
    }

    // log.info(msg) — test logging
    let log_table = lua.create_table().map_err(|e| e.to_string())?;
    let log_info = lua
        .create_function(|_, msg: String| {
            println!("    [test] {}", msg);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    log_table.set("info", log_info).map_err(|e| e.to_string())?;
    globals.set("log", log_table).map_err(|e| e.to_string())?;

    // scene.load(path) — load a scene
    let scene_table = lua.create_table().map_err(|e| e.to_string())?;
    let r = runner.clone();
    let scene_load = lua
        .create_function(move |_, path: String| {
            r.borrow_mut()
                .load_scene(&path)
                .map_err(LuaError::RuntimeError)?;
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
                    let get_fn = lua.create_function(
                        move |lua, (_self_tbl, component): (LuaTable, String)| {
                            let runner = r2.borrow();
                            let sw = runner.scene_world.borrow();
                            let entity = match sw.entity_registry.get(&id_clone) {
                                Some(&e) => e,
                                None => {
                                    return Err(LuaError::RuntimeError(format!(
                                        "Entity '{}' not found",
                                        id_clone
                                    )))
                                }
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
                                    if let Ok(health) =
                                        sw.world.get::<&crate::components::Health>(entity)
                                    {
                                        let tbl = lua.create_table()?;
                                        tbl.set("current", health.current)?;
                                        tbl.set("max", health.max)?;
                                        tbl.set("dead", health.dead)?;
                                        Ok(LuaValue::Table(tbl))
                                    } else {
                                        Ok(LuaNil)
                                    }
                                }
                                _ => Ok(LuaNil),
                            }
                        },
                    )?;
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
        .create_function(
            move |_, (action, kind, value): (String, String, LuaValue)| {
                let runner = r.borrow_mut();
                let mut input = runner.input_state.borrow_mut();
                match kind.as_str() {
                    "press" => {
                        let key = action_to_key(&action);
                        match crate::input::MouseButton::from_name(&key) {
                            Some(button) => input.inject_mouse_press(button),
                            None => input.inject_key_press(&key),
                        }
                    }
                    "release" => {
                        let key = action_to_key(&action);
                        match crate::input::MouseButton::from_name(&key) {
                            Some(button) => input.inject_mouse_release(button),
                            None => input.inject_key_release(&key),
                        }
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
            },
        )
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
            let mut runner = r.borrow_mut();
            runner.execution_budget.check()?;
            runner.step_frames(n);
            runner.execution_budget.check()
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("wait_frames", wait_frames)
        .map_err(|e| e.to_string())?;

    // wait_seconds(n) — advance N seconds of game time
    let r = runner.clone();
    let wait_seconds = lua
        .create_function(move |_, n: f32| {
            let mut runner = r.borrow_mut();
            runner.execution_budget.check()?;
            runner.step_seconds(n);
            runner.execution_budget.check()
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
            let budget = r.borrow().execution_budget.clone();
            budget.check()?;
            for _ in 0..max_frames {
                budget.check()?;
                if r.borrow().event_occurred(&event_type, &empty_filter) {
                    return Ok(true);
                }
                r.borrow_mut().step_frame();
            }
            budget.check()?;
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
            let budget = r.borrow().execution_budget.clone();
            budget.check()?;
            for _ in 0..max_frames {
                budget.check()?;
                r.borrow_mut().step_frame();
                if budget.finish(func.call::<bool>(()))? {
                    return Ok(true);
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
                for (k, v) in tbl.pairs::<String, String>().flatten() {
                    filter_map.insert(k, v);
                }
            }
            Ok(runner.event_occurred(&event_type, &filter_map))
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("event_occurred", event_occurred_fn)
        .map_err(|e| e.to_string())?;

    // get_position(entity_id) -> x, y, z
    // Raises an error for an unknown entity. Returning zeros instead would let
    // a test that names a missing entity pass by accident.
    let r = runner.clone();
    let get_position = lua
        .create_function(move |_, id: String| {
            let runner = r.borrow();
            match runner.get_entity_position(&id) {
                Some(pos) => Ok((pos.x, pos.y, pos.z)),
                None => Err(LuaError::RuntimeError(format!(
                    "get_position: no entity with id '{}' and a transform in the loaded scene",
                    id
                ))),
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

    fn tight_limits() -> TestLimits {
        TestLimits {
            instructions: 50_000,
            memory_bytes: 2 * 1024 * 1024,
            wall_time: std::time::Duration::from_secs(2),
        }
    }

    struct Project(PathBuf);

    impl Project {
        fn new(script: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("naive-runner-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let project = Self(root);
            project.write("game.lua", script);
            project.write("scene.yaml", "name: Regression\nentities:\n  - id: actor\n    components:\n      transform: {}\n      script: { source: game.lua }\n");
            project
        }

        fn write(&self, path: &str, source: &str) {
            std::fs::write(self.0.join(path), source).unwrap();
        }

        fn run(&self, body: &str) -> TestResult {
            run_single_test(
                &self.0,
                &format!("function test_regression()\n{body}\nend"),
                "test_regression",
            )
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn trigger_hooks_cover_dynamic_sensors_destruction_and_scene_reload() {
        let project = Project::new(
            r#"
            entered = {}
            exited = {}
            function on_trigger_enter(other)
                entered[#entered + 1] = other
                local x = entity.get_position(_entity_string_id)
                assert(x ~= nil)
                physics.set_velocity(_entity_string_id, 0, 0, 0)
            end
            function on_trigger_exit(other)
                exited[#exited + 1] = other
                local x = entity.get_position(_entity_string_id)
                assert(x ~= nil)
                physics.set_velocity(_entity_string_id, 0, 0, 0)
            end
        "#,
        );
        project.write(
            "scene.yaml",
            r#"
name: Triggers
settings: {gravity: [0, 0, 0]}
entities:
  - id: dynamic_sensor
    components:
      rigid_body: {type: dynamic}
      collider: {shape: sphere, radius: 1, is_trigger: true}
      script: {source: game.lua}
  - id: static_sensor
    components:
      collider: {shape: sphere, radius: 1, is_trigger: true}
      script: {source: game.lua}
  - id: actor
    components:
      rigid_body: {type: dynamic}
      collider: {shape: sphere, radius: 0.5}
"#,
        );
        let mut runner = TestRunner::new(&project.0);
        runner.load_scene("scene.yaml").unwrap();
        let environment = |runner: &TestRunner, id: &str| -> mlua::Table {
            let entity = runner.scene_world.borrow().entity_registry[id];
            runner
                .script_runtime
                .lua
                .registry_value(&runner.script_runtime.entity_envs[&entity])
                .unwrap()
        };
        let dynamic = environment(&runner, "dynamic_sensor");
        let fixed = environment(&runner, "static_sensor");
        let count =
            |env: &mlua::Table, hook| -> usize { env.get::<mlua::Table>(hook).unwrap().raw_len() };
        runner.step_frame(); // Two fixed steps: overlapping sensors each get two enters.
        assert_eq!(count(&dynamic, "entered"), 2);
        assert_eq!(count(&fixed, "entered"), 2);
        assert!(runner.physics_world.borrow().collision_events.is_empty());
        assert!(runner.physics_world.borrow().trigger_events.is_empty());
        runner.step_frame();
        runner.delta_time = 0.0;
        runner.step_frame();
        assert_eq!(count(&dynamic, "entered"), 2);
        assert_eq!(count(&fixed, "entered"), 2);

        runner
            .entity_commands
            .borrow_mut()
            .destroys
            .push("actor".into());
        runner.step_frame(); // Destruction emits exits even when no physics step is due.
        assert_eq!(count(&dynamic, "exited"), 1);
        assert_eq!(count(&fixed, "exited"), 1);
        assert_eq!(
            dynamic
                .get::<mlua::Table>("exited")
                .unwrap()
                .get::<String>(1)
                .unwrap(),
            "actor"
        );
        runner
            .entity_commands
            .borrow_mut()
            .destroys
            .push("dynamic_sensor".into());
        runner.step_frame();
        assert_eq!(
            count(&dynamic, "exited"),
            1,
            "destroyed scripts must receive no further hooks"
        );
        assert_eq!(count(&fixed, "exited"), 2);
        assert_eq!(
            fixed
                .get::<mlua::Table>("exited")
                .unwrap()
                .get::<String>(2)
                .unwrap(),
            "dynamic_sensor"
        );
        runner.delta_time = 1.0 / 60.0;
        runner.step_frame();
        assert_eq!(
            count(&fixed, "exited"),
            2,
            "removal exit must not be replayed"
        );

        // Keep the old VM alive long enough to inspect its detached tables.
        let previous_vm = runner.script_runtime.lua.clone();
        runner.load_scene("scene.yaml").unwrap();
        let replacement = environment(&runner, "static_sensor");
        runner.step_frame();
        assert_eq!(count(&replacement, "entered"), 2);
        assert_eq!(count(&replacement, "exited"), 0);
        assert_eq!(
            count(&fixed, "entered"),
            2,
            "old scene callbacks must stay detached"
        );
        assert!(runner.take_errors().is_empty());
        drop(previous_vm);
    }

    #[test]
    fn a_scene_without_settings_uses_gravity_in_the_headless_runner() {
        let project = Project::new("");
        project.write("scene.yaml", "name: Defaults\nentities:\n  - id: falling\n    components:\n      transform: {position: [0, 10, 0]}\n      rigid_body: {type: dynamic}\n      collider: {shape: sphere}\n");
        let mut runner = TestRunner::new(&project.0);
        runner.load_scene("scene.yaml").unwrap();
        runner.step_frame();
        assert_eq!(
            runner.physics_world.borrow().gravity,
            glam::Vec3::new(0.0, -9.81, 0.0)
        );
        let world = runner.scene_world.borrow();
        let falling = world.entity_registry["falling"];
        assert!(
            world
                .world
                .get::<&crate::components::Transform>(falling)
                .unwrap()
                .position
                .y
                < 10.0
        );
    }

    #[test]
    fn discovery_rejects_unbounded_top_level_code_and_caught_limits() {
        let project = Project::new("");
        for (source, expected) in [
            ("while true do end", "CPU limit"),
            (
                "while true do pcall(function() while true do end end) end",
                "CPU limit",
            ),
            (
                "pcall(function() string.rep('x', 8000000) end)\nfunction test_ok() end",
                "memory limit",
            ),
        ] {
            project.write("test.lua", source);
            let results =
                run_test_file_with_limits(&project.0, &project.0.join("test.lua"), tight_limits());
            assert_eq!(results.len(), 1);
            assert!(!results[0].passed, "discovery accepted {source}");
            assert!(results[0].error.as_ref().unwrap().contains(expected));
        }
    }

    #[test]
    fn test_source_loading_is_limited_before_calling_the_body() {
        let project = Project::new("");
        let result = run_single_test_with_limits(
            &project.0,
            "while true do end\nfunction test_regression() end",
            "test_regression",
            tight_limits(),
        );
        assert!(!result.passed);
        assert!(result.error.unwrap().contains("CPU limit"));
    }

    #[test]
    fn test_body_cpu_and_memory_limits_cannot_be_caught() {
        let project = Project::new("");
        for (body, expected) in [
            ("while true do end", "CPU limit"),
            ("pcall(function() while true do end end)", "CPU limit"),
            (
                "while true do pcall(function() while true do end end) end",
                "CPU limit",
            ),
            ("string.rep('x', 8000000)", "memory limit"),
            (
                "pcall(function() string.rep('x', 8000000) end)",
                "memory limit",
            ),
            (
                "xpcall(function() string.rep('x', 8000000) end, function() return 'hidden' end)",
                "memory limit",
            ),
        ] {
            let result = run_single_test_with_limits(
                &project.0,
                &format!("function test_regression()\n{body}\nend"),
                "test_regression",
                tight_limits(),
            );
            assert!(!result.passed, "test accepted {body}");
            assert!(
                result.error.unwrap().contains(expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn monotonic_deadline_interrupts_lua_and_giant_rust_waits() {
        let project = Project::new("");
        for body in [
            "while true do end",
            "wait_frames(1000000000000)",
            "wait_seconds(1e30)",
            "wait_for_event('never', 1e30)",
            "wait_until(function() return false end, 1e30)",
            "pcall(wait_frames, 1000000000000)\npcall(scene.load, 'scene.yaml')",
        ] {
            let started = std::time::Instant::now();
            let result = run_single_test_with_limits(
                &project.0,
                &format!("function test_regression()\n{body}\nend"),
                "test_regression",
                TestLimits {
                    instructions: u64::MAX,
                    wall_time: std::time::Duration::from_millis(20),
                    ..tight_limits()
                },
            );
            assert!(!result.passed, "test accepted {body}");
            assert!(result.error.unwrap().contains("wall-clock deadline"));
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
        }
    }

    #[test]
    fn protected_calls_keep_normal_results_and_errors() {
        let project = Project::new("");
        let result = project.run(
            r#"
            local ok, first, second, third = pcall(function() return 7, nil, 9 end)
            assert(ok and first == 7 and second == nil and third == 9)
            local ok, message = pcall(error, 'expected')
            assert(not ok and message == 'expected')
            local ok, message = xpcall(function() error('expected', 0) end,
                function(err) return 'handled ' .. err end)
            assert(not ok and message == 'handled expected')
            assert(coroutine == nil and debug == nil and os == nil and io == nil)
            assert(load == nil and loadfile == nil and dofile == nil)
            "#,
        );
        assert!(result.passed, "{:?}", result.error);
    }

    #[test]
    fn finalizers_cannot_escape_cpu_or_memory_limits() {
        let project = Project::new("");
        for finalizer in ["while true do end", "string.rep('x', 8000000)"] {
            let started = std::time::Instant::now();
            let result = run_single_test_with_limits(
                &project.0,
                &format!(
                    "function test_regression()\nsetmetatable({{}}, {{__gc = function() {finalizer} end}})\ncollectgarbage('collect')\nend"
                ),
                "test_regression",
                tight_limits(),
            );
            assert!(!result.passed);
            assert!(result.error.unwrap().contains("__gc finalizers"));
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
        }
    }

    #[test]
    fn ordinary_metatables_work_and_later_finalizer_assignment_is_rejected() {
        let project = Project::new("");
        let result = project.run(
            r#"
            local mt = {__index = {answer = 42}}
            local value = setmetatable({}, mt)
            assert(value.answer == 42 and getmetatable(value) == mt)
            assert(setmetatable(value, nil) == value and getmetatable(value) == nil)
            setmetatable(value, mt)
            mt.__gc = function() error('finalizer must not run') end
            local ok, message = pcall(setmetatable, value, mt)
            assert(not ok and string.find(message, '__gc finalizers', 1, true))
            mt.__gc = nil
            mt.__metatable = 'protected'
            assert(getmetatable(value) == 'protected')
            assert(not pcall(setmetatable, value, nil))
            "#,
        );
        assert!(result.passed, "{:?}", result.error);
    }

    #[test]
    fn caught_scene_load_failures_and_game_callback_errors_fail_the_test() {
        for (script, expected) in [
            ("function broken(", "Failed to load script"),
            ("error('load regression')", "load regression"),
            ("function init() error('init regression') end", "init regression"),
            ("function update(dt) error('update regression') end", "update regression"),
            ("function init() events.on('regression', function() error('listener regression') end) end\nfunction update(dt) events.emit('regression', {}) end", "listener regression"),
        ] {
            let project = Project::new(script);
            let result = project.run("pcall(scene.load, 'scene.yaml')\nwait_frames(3)");
            assert!(!result.passed, "game failure was ignored: {expected}");
            assert!(result.error.unwrap().contains(expected), "missing game error: {expected}");
        }
    }

    #[test]
    fn collision_callback_errors_fail_the_test() {
        let project =
            Project::new("function on_collision(other) error('collision regression') end");
        project.write(
            "scene.yaml",
            r#"
name: Collision regression
settings: { gravity: [0, -9.81, 0] }
entities:
  - id: actor
    components:
      transform: { position: [0, 1, 0] }
      rigid_body: { body_type: dynamic, mass: 1 }
      collider: { shape: sphere, radius: 0.5 }
      script: { source: game.lua }
  - id: floor
    components:
      transform: { position: [0, -0.5, 0] }
      collider: { shape: box, half_extents: [10, 0.5, 10] }
"#,
        );
        let result = project.run("scene.load('scene.yaml')\nwait_frames(90)");
        assert!(!result.passed);
        assert!(result.error.unwrap().contains("collision regression"));
    }

    #[test]
    fn unsupported_gpu_commands_fail_even_without_advancing_a_frame() {
        for script in [
            "function init() entity.set_texture('actor', 'albedo', 'image.png') end",
            "function init() mesh.create('triangle', {{0,0,0},{1,0,0},{0,1,0}}, {0,1,2}, {}) end",
        ] {
            let project = Project::new(script);
            let result = project.run("scene.load('scene.yaml')");
            assert!(!result.passed);
            assert!(result.error.unwrap().contains("GPU mesh/texture"));
        }
    }

    #[test]
    fn unsupported_trimesh_colliders_fail_instead_of_using_a_different_shape() {
        let project = Project::new("");
        project.write("scene.yaml", "name: Trimesh\nentities:\n  - id: actor\n    components:\n      collider: {shape: trimesh}\n");
        let result = project.run("pcall(scene.load, 'scene.yaml')");
        assert!(!result.passed);
        assert!(result.error.unwrap().contains("trimesh collider"));
        project.write("scene.yaml", "name: Spawn\nentities:\n  - id: actor\n    components:\n      script: {source: game.lua}\n");
        project.write(
            "game.lua",
            "function init() entity.spawn_ex({id='mesh', collider={shape='trimesh'}}) end",
        );
        let result = project.run("scene.load('scene.yaml')\nwait_frames(1)");
        assert!(!result.passed);
        assert!(result.error.unwrap().contains("trimesh collider"));
    }

    #[test]
    fn spawned_scripts_initialize_once_and_headless_visibility_is_applied() {
        let project = Project::new(
            r#"
function init()
    entity.spawn_ex({id='child', position={1,2,3}, script='child.lua'})
end
function update(dt)
    entity.set_visible('child', false)
    entity.set_scale('child', 2, 3, 4)
end
"#,
        );
        project.write("child.lua", "function init() game.child_inits = (game.child_inits or 0) + 1 end\nfunction update(dt) game.child_updates = (game.child_updates or 0) + 1 end");
        let mut runner = TestRunner::new(&project.0);
        runner.load_scene("scene.yaml").unwrap();
        runner.step_frames(3);
        let game: LuaTable = runner.script_runtime.lua.globals().get("game").unwrap();
        assert_eq!(game.get::<u32>("child_inits").unwrap(), 1);
        assert_eq!(game.get::<u32>("child_updates").unwrap(), 2);
        let world = runner.scene_world.borrow();
        let child = world.entity_registry["child"];
        assert!(world.world.get::<&crate::components::Hidden>(child).is_ok());
        assert_eq!(
            world.world.get::<&Transform>(child).unwrap().scale,
            glam::Vec3::new(2.0, 3.0, 4.0)
        );
        assert!(runner.take_errors().is_empty());
    }

    #[test]
    fn missing_spawned_script_fails_the_test() {
        let project =
            Project::new("function init() entity.spawn_ex({id='child', script='missing.lua'}) end");
        let result = project.run("scene.load('scene.yaml')\nwait_frames(1)");
        assert!(!result.passed);
        assert!(result.error.unwrap().contains("missing.lua"));
    }

    #[test]
    fn scene_changes_preserve_previous_runtime_and_destroy_errors() {
        let project = Project::new("function update(dt) error('previous scene update') end\nfunction on_destroy() error('previous scene destroy') end");
        project.write("next.yaml", "name: Next\nentities: []\n");
        let result =
            project.run("scene.load('scene.yaml')\nwait_frames(1)\nscene.load('next.yaml')");
        assert!(!result.passed);
        let error = result.error.unwrap();
        assert!(error.contains("previous scene update"));
        assert!(error.contains("previous scene destroy"));
    }

    #[test]
    fn scene_changes_preserve_pending_unsupported_commands() {
        let project =
            Project::new("function init() entity.set_texture('actor', 'albedo', 'image.png') end");
        project.write("next.yaml", "name: Next\nentities: []\n");
        let result = project.run("scene.load('scene.yaml')\nscene.load('next.yaml')");
        assert!(!result.passed);
        assert!(result.error.unwrap().contains("GPU mesh/texture"));
    }

    #[test]
    fn headless_audio_api_and_scene_transitions_support_gameplay_scripts() {
        let project = Project::new(
            r#"
function update(dt)
    audio.play_sfx('hit', 'unused.wav', 0.5)
    scene.load('next.yaml')
end
function on_destroy() entity.set_position('actor', 1, 2, 3) end
"#,
        );
        project.write(
            "next.yaml",
            "name: Next\nentities:\n  - id: next\n    components:\n      transform: {}\n",
        );
        let result = project.run("scene.load('scene.yaml')\nwait_frames(2)\nassert(scene.find('next') ~= nil)\nassert(scene.find('actor') == nil)");
        assert!(result.passed, "{:?}", result.error);
    }

    #[test]
    fn wait_until_preserves_the_predicate_error() {
        let project = Project::new("");
        let result = project.run("wait_until(function() error('predicate regression') end, 0.1)");
        assert!(!result.passed);
        let error = result.error.unwrap();
        assert!(error.contains("predicate regression"));
        assert!(!error.contains("timed out"));
    }

    #[test]
    fn test_action_to_key_mapping() {
        assert_eq!(action_to_key("interact"), "E");
        assert_eq!(action_to_key("jump"), "Space");
        assert_eq!(action_to_key("move_forward"), "W");
    }

    #[test]
    fn get_position_rejects_an_unknown_entity() {
        let runner = Rc::new(RefCell::new(TestRunner::new(Path::new("."))));
        let lua = Lua::new();
        register_test_api(&lua, runner).unwrap();

        let err = lua
            .load(r#"return get_position("no_such_entity")"#)
            .exec()
            .unwrap_err();
        assert!(
            err.to_string().contains("no_such_entity"),
            "unexpected error: {err}"
        );
    }
}
