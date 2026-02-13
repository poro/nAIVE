use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use notify::RecommendedWatcher;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::audio::AudioSystem;
use crate::camera::CameraState;
use crate::cli::CliArgs;
use crate::command::CommandServer;
use crate::components::{Camera, CameraRole, GaussianSplat, Player, Transform};
use crate::events::EventBus;
use crate::tween::TweenSystem;
use crate::input::InputState;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::{CharacterController, Collider as ColliderComp, PhysicsWorld, RigidBody as RigidBodyComp};
use crate::scripting::{Script, ScriptRuntime};
use crate::pipeline::CompiledPipeline;
use crate::splat::SplatCache;
use crate::renderer::{DrawUniformPool, GpuState};
use crate::watcher::WatchEvent;
use crate::world::SceneWorld;

use winit::keyboard::KeyCode;

/// Main engine struct implementing winit's ApplicationHandler.
pub struct Engine {
    #[allow(dead_code)]
    pub args: CliArgs,
    pub gpu: Option<GpuState>,
    pub project_root: PathBuf,
    _watcher: Option<RecommendedWatcher>,
    watch_rx: Option<mpsc::Receiver<WatchEvent>>,

    // Phase 2: scene rendering state
    pub scene_world: Option<SceneWorld>,
    pub mesh_cache: MeshCache,
    pub material_cache: MaterialCache,
    pub splat_cache: SplatCache,
    pub camera_state: Option<CameraState>,
    pub draw_pool: Option<DrawUniformPool>,
    pub forward_pipeline: Option<wgpu::RenderPipeline>,
    scene_path: Option<PathBuf>,

    // Phase 3: compiled render pipeline
    pub compiled_pipeline: Option<CompiledPipeline>,
    pipeline_path: Option<PathBuf>,

    // Phase 5: input + physics
    pub input_state: Option<InputState>,
    pub physics_world: Option<PhysicsWorld>,
    last_frame_time: Option<instant::Instant>,
    delta_time: f32,

    // Phase 6: scripting
    pub script_runtime: Option<ScriptRuntime>,

    // Phase 7: events + audio + tweens
    pub event_bus: EventBus,
    pub audio_system: AudioSystem,
    pub tween_system: TweenSystem,

    // Phase 8: command socket
    pub command_server: Option<CommandServer>,
    pub paused: bool,
}

impl Engine {
    pub fn new(args: CliArgs) -> Self {
        let project_root = PathBuf::from(&args.project);
        Self {
            args,
            gpu: None,
            project_root,
            _watcher: None,
            watch_rx: None,
            scene_world: None,
            mesh_cache: MeshCache::new(),
            material_cache: MaterialCache::new(),
            splat_cache: SplatCache::new(),
            camera_state: None,
            draw_pool: None,
            forward_pipeline: None,
            scene_path: None,
            compiled_pipeline: None,
            pipeline_path: None,
            input_state: None,
            physics_world: None,
            last_frame_time: None,
            delta_time: 1.0 / 60.0,
            script_runtime: None,
            event_bus: EventBus::new(1000),
            audio_system: AudioSystem::new(),
            tween_system: TweenSystem::new(),
            command_server: None,
            paused: false,
        }
    }

    /// Get the initial WGSL shader source for the triangle, trying SLANG first.
    fn get_initial_shader(&self) -> String {
        let triangle_slang = self.project_root.join("shaders/passes/triangle.slang");
        match crate::shader::compile_triangle_shader(Some(&triangle_slang)) {
            Ok(wgsl) => wgsl,
            Err(e) => {
                tracing::error!("Initial shader compilation failed: {}", e);
                crate::shader::get_triangle_wgsl()
            }
        }
    }

    /// Initialize the forward pipeline and load the scene.
    fn load_scene(&mut self) {
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        let scene_arg = match &self.args.scene {
            Some(s) => s.clone(),
            None => return,
        };

        // Resolve scene path relative to project root
        let scene_path = self.project_root.join(&scene_arg);
        if !scene_path.exists() {
            tracing::error!("Scene file not found: {:?}", scene_path);
            return;
        }

        // Create camera state and draw uniform pool
        let camera_state = CameraState::new(&gpu.device);
        let draw_pool = DrawUniformPool::new(&gpu.device);

        // Compile the forward shader (Phase 2 fallback pipeline)
        let forward_slang = self.project_root.join("shaders/passes/mesh_forward.slang");
        let forward_wgsl =
            match crate::shader::compile_mesh_forward_shader(Some(&forward_slang)) {
                Ok(wgsl) => wgsl,
                Err(e) => {
                    tracing::error!("Forward shader compilation failed: {}", e);
                    crate::shader::get_mesh_forward_wgsl()
                }
            };

        // Create the forward render pipeline
        let forward_pipeline = crate::renderer::create_forward_pipeline(
            &gpu.device,
            &forward_wgsl,
            gpu.config.format,
            &camera_state.bind_group_layout,
            &draw_pool.bind_group_layout,
        );

        // Load the scene YAML
        let scene = match crate::scene::load_scene(&scene_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to load scene: {}", e);
                return;
            }
        };

        // Spawn entities into ECS
        let mut scene_world = SceneWorld::new();
        crate::world::spawn_all_entities(
            &mut scene_world,
            &scene,
            &gpu.device,
            &self.project_root,
            &mut self.mesh_cache,
            &mut self.material_cache,
            &mut self.splat_cache,
            None,
        );

        self.scene_world = Some(scene_world);
        self.camera_state = Some(camera_state);
        self.draw_pool = Some(draw_pool);
        self.forward_pipeline = Some(forward_pipeline);
        self.scene_path = Some(scene_path);

        tracing::info!("Scene loaded and forward pipeline created");

        // Phase 5: Initialize input system
        let bindings = crate::input::load_bindings(&self.project_root);
        self.input_state = Some(InputState::new(bindings));

        // Phase 5: Initialize physics world
        let gravity = if let Some(sw) = &self.scene_world {
            if let Some(scene) = &sw.current_scene {
                glam::Vec3::from(scene.settings.gravity)
            } else {
                glam::Vec3::new(0.0, -9.81, 0.0)
            }
        } else {
            glam::Vec3::new(0.0, -9.81, 0.0)
        };
        let mut physics_world = PhysicsWorld::new(gravity);

        // Spawn physics components for entities that have them
        if let Some(sw) = &mut self.scene_world {
            if let Some(scene) = &sw.current_scene {
                let scene_clone = scene.clone();
                for entity_def in &scene_clone.entities {
                    if let Some(&entity) = sw.entity_registry.get(&entity_def.id) {
                        let pos = if let Some(t) = &entity_def.components.transform {
                            glam::Vec3::from(t.position)
                        } else {
                            glam::Vec3::ZERO
                        };
                        let rot = if let Some(t) = &entity_def.components.transform {
                            crate::world::euler_degrees_to_quat(t.rotation)
                        } else {
                            glam::Quat::IDENTITY
                        };

                        if let Some(cc_def) = &entity_def.components.character_controller {
                            let half_height = cc_def.height / 2.0 - cc_def.radius;
                            let (rb_handle, col_handle) = physics_world.add_character_body(
                                entity,
                                pos,
                                half_height.max(0.1),
                                cc_def.radius,
                            );
                            let rb_comp = crate::physics::RigidBody {
                                handle: rb_handle,
                                body_type: crate::physics::PhysicsBodyType::Kinematic,
                            };
                            let col_comp = crate::physics::Collider {
                                handle: col_handle,
                                shape: crate::physics::PhysicsShape::Capsule {
                                    half_height: half_height.max(0.1),
                                    radius: cc_def.radius,
                                },
                                is_trigger: false,
                            };
                            let cc_comp = CharacterController {
                                move_speed: cc_def.move_speed,
                                sprint_multiplier: cc_def.sprint_multiplier,
                                jump_impulse: cc_def.jump_impulse,
                                step_height: cc_def.step_height,
                                ..Default::default()
                            };
                            let player = crate::components::Player {
                                height: cc_def.height,
                                radius: cc_def.radius,
                                ..Default::default()
                            };
                            let _ = sw.world.insert(entity, (rb_comp, col_comp, cc_comp, player));
                        } else if let Some(col_def) = &entity_def.components.collider {
                            let shape = crate::world::parse_collider_shape(col_def);
                            let is_trigger = col_def.is_trigger;
                            let body_type = entity_def
                                .components
                                .rigid_body
                                .as_ref()
                                .map(|rb| rb.body_type.as_str())
                                .unwrap_or("static");

                            match body_type {
                                "dynamic" => {
                                    let mass = entity_def
                                        .components
                                        .rigid_body
                                        .as_ref()
                                        .map(|rb| rb.mass)
                                        .unwrap_or(1.0);
                                    let (rb_handle, col_handle) = physics_world
                                        .add_dynamic_body(entity, pos, rot, shape.clone(), mass);
                                    let rb_comp = crate::physics::RigidBody {
                                        handle: rb_handle,
                                        body_type: crate::physics::PhysicsBodyType::Dynamic,
                                    };
                                    let col_comp = crate::physics::Collider {
                                        handle: col_handle,
                                        shape,
                                        is_trigger,
                                    };
                                    let _ = sw.world.insert(entity, (rb_comp, col_comp));
                                }
                                _ => {
                                    let (rb_handle, col_handle) = physics_world
                                        .add_static_body(entity, pos, rot, shape.clone(), is_trigger);
                                    let rb_comp = crate::physics::RigidBody {
                                        handle: rb_handle,
                                        body_type: crate::physics::PhysicsBodyType::Static,
                                    };
                                    let col_comp = crate::physics::Collider {
                                        handle: col_handle,
                                        shape,
                                        is_trigger,
                                    };
                                    let _ = sw.world.insert(entity, (rb_comp, col_comp));
                                }
                            }
                        }
                    }
                }
            }
        }
        self.physics_world = Some(physics_world);
        self.last_frame_time = Some(instant::Instant::now());
        tracing::info!("Physics world initialized");

        // Phase 6: Initialize scripting runtime
        let mut script_runtime = ScriptRuntime::new();
        if let Err(e) = script_runtime.register_api() {
            tracing::error!("Failed to register script API: {}", e);
        }

        // Register input API
        if let Some(input) = &self.input_state {
            let input_ptr = input as *const InputState;
            if let Err(e) = script_runtime.register_input_api(input_ptr) {
                tracing::error!("Failed to register input API: {}", e);
            }
        }

        // Register physics API
        if let Some(pw) = &self.physics_world {
            let physics_ptr = pw as *const PhysicsWorld;
            if let Err(e) = script_runtime.register_physics_api(physics_ptr) {
                tracing::error!("Failed to register physics API: {}", e);
            }
        }

        // Register entity manipulation API
        if let Some(sw) = &mut self.scene_world {
            let sw_ptr = sw as *mut SceneWorld;
            if let Err(e) = script_runtime.register_entity_api(sw_ptr) {
                tracing::error!("Failed to register entity API: {}", e);
            }
        }

        // Register event bus API
        {
            let bus_ptr = &mut self.event_bus as *mut crate::events::EventBus;
            if let Err(e) = script_runtime.register_event_api(bus_ptr) {
                tracing::error!("Failed to register event API: {}", e);
            }
        }

        // Load scripts for entities that have them
        if let Some(sw) = &mut self.scene_world {
            if let Some(scene) = &sw.current_scene {
                let scene_clone = scene.clone();
                for entity_def in &scene_clone.entities {
                    if let Some(script_def) = &entity_def.components.script {
                        if let Some(&entity) = sw.entity_registry.get(&entity_def.id) {
                            let source_path = std::path::PathBuf::from(&script_def.source);
                            let script_comp = Script {
                                source: source_path.clone(),
                                initialized: false,
                            };
                            let _ = sw.world.insert_one(entity, script_comp);

                            if let Err(e) = script_runtime.load_script(
                                entity,
                                &self.project_root,
                                &source_path,
                            ) {
                                tracing::error!("Failed to load script for '{}': {}", entity_def.id, e);
                            } else {
                                // Set the entity's YAML string ID in its script environment
                                let _ = script_runtime.set_entity_string_id(entity, &entity_def.id);
                            }
                        }
                    }
                }
            }
        }

        // Call init on all scripts
        if let Some(sw) = &self.scene_world {
            for (entity, script) in sw.world.query::<&Script>().iter() {
                if !script.initialized {
                    script_runtime.call_init(entity);
                }
            }
            // Mark all as initialized
        }
        if let Some(sw) = &mut self.scene_world {
            for (_entity, script) in sw.world.query::<&mut Script>().iter() {
                script.initialized = true;
            }
        }

        self.script_runtime = Some(script_runtime);
        tracing::info!("Script runtime initialized");

        // Phase 7: Initialize event bus schema and audio
        self.event_bus.load_schema(&self.project_root);

        // Phase 3: try to compile the render pipeline if --pipeline was given
        self.try_load_pipeline();

        // Phase 8: Start command socket server
        match CommandServer::start(&self.args.socket) {
            Ok(server) => {
                tracing::info!("Command socket: {}", server.socket_path);
                self.command_server = Some(server);
            }
            Err(e) => {
                tracing::warn!("Failed to start command server: {}", e);
            }
        }
    }

    /// Attempt to load and compile the render pipeline from YAML.
    fn try_load_pipeline(&mut self) {
        let pipeline_arg = match &self.args.pipeline {
            Some(p) => p.clone(),
            None => return,
        };

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };
        let camera_state = match &self.camera_state {
            Some(cs) => cs,
            None => return,
        };
        let draw_pool = match &self.draw_pool {
            Some(dp) => dp,
            None => return,
        };

        let pipeline_path = self.project_root.join(&pipeline_arg);
        if !pipeline_path.exists() {
            tracing::error!("Pipeline file not found: {:?}", pipeline_path);
            return;
        }

        match crate::pipeline::load_pipeline(&pipeline_path) {
            Ok(pipeline_file) => {
                match crate::pipeline::compile_pipeline(
                    &gpu.device,
                    &pipeline_file,
                    &self.project_root,
                    camera_state,
                    draw_pool,
                    gpu.config.format,
                    gpu.config.width,
                    gpu.config.height,
                ) {
                    Ok(compiled) => {
                        self.compiled_pipeline = Some(compiled);
                        self.pipeline_path = Some(pipeline_path);
                        tracing::info!("Render pipeline compiled successfully");
                    }
                    Err(e) => {
                        tracing::error!("Pipeline compilation failed: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to load pipeline: {}", e);
            }
        }
    }

    /// Start the file watcher on the project directory.
    fn start_watcher(&mut self) {
        match crate::watcher::start_watching_all(&self.project_root) {
            Ok((watcher, rx)) => {
                self._watcher = Some(watcher);
                self.watch_rx = Some(rx);
                tracing::info!("File watching enabled");
            }
            Err(e) => {
                tracing::warn!("Failed to start file watcher: {:?}", e);
            }
        }
    }

    /// Handle a shader file change by recompiling and recreating the pipeline.
    fn handle_shader_reload(&mut self, changed_path: &Path) {
        let gpu = match &mut self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        tracing::info!("Hot-reloading shader: {:?}", changed_path);

        // If we have a compiled pipeline and the shader belongs to it, recompile the pipeline
        if self.compiled_pipeline.is_some() {
            tracing::info!("Recompiling render pipeline after shader change");
            // Drop the old pipeline first
            self.compiled_pipeline = None;
            // Recompile
            self.try_load_pipeline();
            return;
        }

        // Phase 2 shader reload path
        let is_forward = self.forward_pipeline.is_some();

        if is_forward {
            let wgsl = match crate::shader::compile_mesh_forward_shader(Some(changed_path)) {
                Ok(wgsl) => wgsl,
                Err(e) => {
                    tracing::error!("Shader reload failed: {}, keeping old pipeline", e);
                    return;
                }
            };

            gpu.device
                .push_error_scope(wgpu::ErrorFilter::Validation);

            let camera_state = self.camera_state.as_ref().unwrap();
            let draw_pool = self.draw_pool.as_ref().unwrap();

            let new_pipeline = crate::renderer::create_forward_pipeline(
                &gpu.device,
                &wgsl,
                gpu.config.format,
                &camera_state.bind_group_layout,
                &draw_pool.bind_group_layout,
            );

            let error = pollster::block_on(gpu.device.pop_error_scope());
            if let Some(err) = error {
                tracing::error!("Shader validation error: {:?}, keeping old pipeline", err);
                return;
            }

            self.forward_pipeline = Some(new_pipeline);
            tracing::info!("Forward shader hot-reload complete");
        } else {
            let wgsl = match crate::shader::compile_triangle_shader(Some(changed_path)) {
                Ok(wgsl) => wgsl,
                Err(e) => {
                    tracing::error!("Shader reload failed: {}, keeping old pipeline", e);
                    return;
                }
            };

            gpu.device
                .push_error_scope(wgpu::ErrorFilter::Validation);

            let new_pipeline =
                crate::renderer::create_render_pipeline(&gpu.device, &wgsl, gpu.config.format);

            let error = pollster::block_on(gpu.device.pop_error_scope());
            if let Some(err) = error {
                tracing::error!("Shader validation error: {:?}, keeping old pipeline", err);
                return;
            }

            gpu.render_pipeline = Some(new_pipeline);
            tracing::info!("Triangle shader hot-reload complete");
        }
    }

    /// Handle a scene file change.
    fn handle_scene_reload(&mut self, changed_path: &Path) {
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        let scene_world = match &mut self.scene_world {
            Some(sw) => sw,
            None => return,
        };

        tracing::info!("Hot-reloading scene: {:?}", changed_path);

        let new_scene = match crate::scene::load_scene(changed_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Scene reload failed: {}, keeping old scene", e);
                return;
            }
        };

        crate::world::reconcile_scene(
            scene_world,
            &new_scene,
            &gpu.device,
            &self.project_root,
            &mut self.mesh_cache,
            &mut self.material_cache,
            &mut self.splat_cache,
            None,
        );

        tracing::info!("Scene hot-reload complete");
    }

    /// Handle a splat (.ply) file change by invalidating the cache and reloading.
    fn handle_splat_reload(&mut self, changed_path: &Path) {
        tracing::info!("Hot-reloading splat: {:?}", changed_path);

        // Try to extract relative path from project root
        if let Ok(relative) = changed_path.strip_prefix(&self.project_root) {
            let relative_str = relative.to_string_lossy().to_string();
            self.splat_cache.invalidate(&relative_str);
        }

        // Also invalidate by filename in case of different path resolution
        if let Some(file_name) = changed_path.file_name() {
            let name_str = file_name.to_string_lossy().to_string();
            self.splat_cache.invalidate(&name_str);
        }

        // Reloading will happen automatically on next frame when get_or_load is called
        // Force pipeline recompilation to pick up new splat data
        if self.compiled_pipeline.is_some() {
            self.compiled_pipeline = None;
            self.try_load_pipeline();
        }
    }

    /// Handle a pipeline YAML file change.
    fn handle_pipeline_reload(&mut self, _changed_path: &Path) {
        tracing::info!("Hot-reloading render pipeline");
        self.compiled_pipeline = None;
        self.try_load_pipeline();
    }

    /// Poll for file change events (non-blocking).
    fn poll_changes(&mut self) {
        let events: Vec<WatchEvent> = if let Some(rx) = &self.watch_rx {
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            events
        } else {
            return;
        };

        let mut shader_paths = std::collections::HashSet::new();
        let mut scene_paths = std::collections::HashSet::new();
        let mut splat_paths = std::collections::HashSet::new();
        let mut script_paths = std::collections::HashSet::new();
        let mut pipeline_changed = false;

        for event in events {
            match event {
                WatchEvent::ShaderChanged(path) => {
                    shader_paths.insert(path);
                }
                WatchEvent::SceneChanged(path) => {
                    scene_paths.insert(path);
                }
                WatchEvent::MaterialChanged(_path) => {
                    tracing::info!("Material changed (reload not yet implemented)");
                }
                WatchEvent::PipelineChanged(path) => {
                    tracing::info!("Pipeline file changed: {:?}", path);
                    pipeline_changed = true;
                }
                WatchEvent::SplatChanged(path) => {
                    splat_paths.insert(path);
                }
                WatchEvent::ScriptChanged(path) => {
                    script_paths.insert(path);
                }
            }
        }

        for path in shader_paths {
            self.handle_shader_reload(&path);
        }

        for path in scene_paths {
            self.handle_scene_reload(&path);
        }

        for path in &splat_paths {
            self.handle_splat_reload(path);
        }

        if pipeline_changed {
            if let Some(path) = self.pipeline_path.clone() {
                self.handle_pipeline_reload(&path);
            }
        }

        // Hot-reload scripts
        if !script_paths.is_empty() {
            if let (Some(scene_world), Some(script_runtime)) =
                (&self.scene_world, &mut self.script_runtime)
            {
                for (entity, script) in scene_world.world.query::<&Script>().iter() {
                    for changed_path in &script_paths {
                        let script_source_str = script.source.to_string_lossy();
                        let changed_str = changed_path.to_string_lossy();
                        if changed_str.ends_with(&*script_source_str)
                            || changed_str.ends_with(script.source.file_name().unwrap_or_default().to_string_lossy().as_ref())
                        {
                            match script_runtime.hot_reload_script(entity, &self.project_root, &script.source) {
                                Ok(true) => tracing::info!("Script hot-reloaded: {:?}", script.source),
                                Ok(false) => {}
                                Err(e) => tracing::error!("Script hot-reload failed: {}", e),
                            }
                        }
                    }
                }
            }
        }
    }

    /// Update the FPS camera controller: mouse look + WASD movement + physics.
    fn update_fps_controller(&mut self) {
        let input = match &self.input_state {
            Some(i) => i,
            None => return,
        };
        let scene_world = match &mut self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let physics_world = match &mut self.physics_world {
            Some(pw) => pw,
            None => return,
        };

        let dt = self.delta_time;

        // Collect player entity data
        let mut player_updates: Vec<(hecs::Entity, glam::Vec3, f32, f32, rapier3d::prelude::RigidBodyHandle, rapier3d::prelude::ColliderHandle, f32, f32, f32, bool, glam::Vec3)> = Vec::new();

        for (entity, (player, cc, rb, col)) in scene_world
            .world
            .query::<(&Player, &CharacterController, &RigidBodyComp, &ColliderComp)>()
            .iter()
        {
            // Mouse look
            let mouse_delta = input.mouse_delta();
            let sensitivity = 0.002;
            let new_yaw = player.yaw - mouse_delta.x * sensitivity;
            let new_pitch = (player.pitch - mouse_delta.y * sensitivity)
                .clamp(-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01);

            // Movement
            let move_input = input.axis_2d("move_forward", "move_backward", "move_left", "move_right");
            let speed = if input.pressed("sprint") {
                cc.move_speed * cc.sprint_multiplier
            } else {
                cc.move_speed
            };

            // Calculate movement direction relative to yaw
            let forward = glam::Vec3::new(-new_yaw.sin(), 0.0, -new_yaw.cos());
            let right = glam::Vec3::new(-forward.z, 0.0, forward.x);
            let move_dir = (forward * move_input.y + right * move_input.x).normalize_or_zero();
            let mut desired = move_dir * speed * dt;

            // Vertical velocity for gravity + jump
            let mut vel_y = cc.velocity.y;
            if cc.grounded {
                vel_y = 0.0;
                if input.just_pressed("jump") {
                    vel_y = cc.jump_impulse;
                }
            }
            vel_y += physics_world.gravity.y * dt;
            desired.y = vel_y * dt;

            player_updates.push((
                entity,
                desired,
                new_yaw,
                new_pitch,
                rb.handle,
                col.handle,
                vel_y,
                player.height,
                player.radius,
                cc.grounded,
                cc.velocity,
            ));
        }

        // Apply updates
        for (entity, desired, new_yaw, new_pitch, rb_handle, col_handle, vel_y, _height, _radius, _was_grounded, _old_vel) in player_updates {
            let (_effective, grounded) = physics_world.move_character(rb_handle, col_handle, desired, dt);

            // Update ECS components
            if let Ok(mut player) = scene_world.world.get::<&mut Player>(entity) {
                player.yaw = new_yaw;
                player.pitch = new_pitch;
            }
            if let Ok(mut cc) = scene_world.world.get::<&mut CharacterController>(entity) {
                cc.grounded = grounded;
                cc.velocity.y = if grounded && vel_y < 0.0 { 0.0 } else { vel_y };
            }

            // The physics body position is already updated by move_character
            // Sync it back to the transform
            if let Some(body) = physics_world.rigid_body_set.get(rb_handle) {
                let pos = body.position().translation;
                if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                    transform.position = glam::Vec3::new(pos.x, pos.y, pos.z);
                    transform.rotation = glam::Quat::from_rotation_y(new_yaw);
                    transform.dirty = true;
                }
            }
        }

        // Step physics for non-character bodies
        physics_world.step(dt);
        physics_world.sync_to_ecs(&mut scene_world.world);
    }

    /// Update the camera uniform from the main camera entity.
    fn update_camera(&mut self) {
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };
        let scene_world = match &self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let camera_state = match &mut self.camera_state {
            Some(cs) => cs,
            None => return,
        };

        // Check if there's a player entity controlling the camera
        let mut player_camera_applied = false;
        for (_entity, (transform, player, camera)) in
            scene_world.world.query::<(&Transform, &Player, &Camera)>().iter()
        {
            if camera.role == CameraRole::Main {
                // Override camera transform with player look direction
                let look_rotation = glam::Quat::from_rotation_y(player.yaw)
                    * glam::Quat::from_rotation_x(player.pitch);
                let cam_transform = Transform {
                    position: transform.position + glam::Vec3::new(0.0, player.height * 0.4, 0.0),
                    rotation: look_rotation,
                    ..transform.clone()
                };
                camera_state.update(
                    &gpu.queue,
                    camera,
                    &cam_transform,
                    gpu.config.width,
                    gpu.config.height,
                );
                player_camera_applied = true;
                break;
            }
        }

        if !player_camera_applied {
            // Find the main camera entity (non-player)
            for (_entity, (transform, camera)) in
                scene_world.world.query::<(&Transform, &Camera)>().iter()
            {
                if camera.role == CameraRole::Main {
                    camera_state.update(
                        &gpu.queue,
                        camera,
                        transform,
                        gpu.config.width,
                        gpu.config.height,
                    );
                    break;
                }
            }
        }
    }

    /// Process commands from the command socket.
    fn process_commands(&mut self) {
        let commands = match &self.command_server {
            Some(s) => s.poll(),
            None => return,
        };

        for pending in commands {
            let response = crate::command::handle_command(
                &pending.request,
                &mut self.scene_world,
                &mut self.event_bus,
                &mut self.input_state,
                &mut self.paused,
            );
            let _ = pending.responder.send(response);
        }
    }
}

impl ApplicationHandler for Engine {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        tracing::info!("Application resumed, initializing GPU");

        let window_attrs = Window::default_attributes()
            .with_title("nAIVE Engine")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));

        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("Failed to create window"),
        );

        let initial_wgsl = self.get_initial_shader();

        let gpu_state =
            pollster::block_on(crate::renderer::init_gpu(Arc::clone(&window), &initial_wgsl));

        self.gpu = Some(gpu_state);
        tracing::info!("GPU initialized successfully");

        // Phase 2: load scene if --scene was provided
        self.load_scene();

        // Start watchers (unified for shaders, scenes, materials, pipelines)
        self.start_watcher();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Feed all events to input system
        if let Some(input) = &mut self.input_state {
            input.handle_window_event(&event);
        }

        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("Close requested, exiting");
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let Some(gpu) = &mut self.gpu {
                    if new_size.width > 0 && new_size.height > 0 {
                        gpu.config.width = new_size.width;
                        gpu.config.height = new_size.height;
                        gpu.surface.configure(&gpu.device, &gpu.config);

                        // Recreate depth texture on resize
                        let (depth_texture, depth_view) =
                            crate::renderer::create_depth_texture(
                                &gpu.device,
                                new_size.width,
                                new_size.height,
                            );
                        gpu.depth_texture = depth_texture;
                        gpu.depth_view = depth_view;

                        // Phase 3: resize pipeline resources
                        if let Some(compiled) = &mut self.compiled_pipeline {
                            crate::pipeline::resize_resources(
                                &gpu.device,
                                &mut compiled.resources,
                                new_size.width,
                                new_size.height,
                            );
                            crate::pipeline::rebuild_bind_groups(&gpu.device, compiled);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Calculate delta time
                let now = instant::Instant::now();
                if let Some(last) = self.last_frame_time {
                    self.delta_time = now.duration_since(last).as_secs_f32().min(0.1);
                }
                self.last_frame_time = Some(now);

                // Phase 8: Process command socket before input
                self.process_commands();

                // Handle Escape to toggle cursor capture
                if let Some(input) = &self.input_state {
                    if input.key_held(KeyCode::Escape) {
                        if let Some(gpu) = &self.gpu {
                            let _ = gpu.window.set_cursor_grab(winit::window::CursorGrabMode::None);
                            gpu.window.set_cursor_visible(true);
                        }
                        if let Some(input) = &mut self.input_state {
                            input.cursor_captured = false;
                        }
                    }
                }

                // Handle mouse click or any movement key to capture cursor
                if let Some(input) = &self.input_state {
                    if !input.cursor_captured {
                        let should_capture = input.just_pressed("attack")
                            || input.just_pressed("move_forward")
                            || input.just_pressed("move_backward")
                            || input.just_pressed("move_left")
                            || input.just_pressed("move_right")
                            || input.just_pressed("jump")
                            || input.just_pressed("interact");
                        if should_capture {
                            tracing::info!("Capturing cursor for FPS mode");
                            if let Some(gpu) = &self.gpu {
                                let _ = gpu.window.set_cursor_grab(winit::window::CursorGrabMode::Locked)
                                    .or_else(|_| gpu.window.set_cursor_grab(winit::window::CursorGrabMode::Confined));
                                gpu.window.set_cursor_visible(false);
                            }
                            if let Some(input) = &mut self.input_state {
                                input.cursor_captured = true;
                            }
                        }
                    }
                }

                // Poll for file changes (shader + scene + pipeline)
                self.poll_changes();

                if self.scene_world.is_some() {
                    if !self.paused {
                        // Phase 5: FPS controller update (physics + input)
                        if self.input_state.as_ref().map(|i| i.cursor_captured).unwrap_or(false) {
                            self.update_fps_controller();
                        }

                        // Phase 6: Update scripts
                        let dt = self.delta_time;
                        if let (Some(scene_world), Some(script_runtime)) =
                            (&self.scene_world, &self.script_runtime)
                        {
                            for (entity, _script) in scene_world.world.query::<&Script>().iter() {
                                script_runtime.call_update(entity, dt);
                            }
                        }

                        // Phase 7: Tick event bus and tweens
                        self.event_bus.tick(dt as f64);
                        self.event_bus.flush();
                        let _tween_results = self.tween_system.update(dt);
                        self.audio_system.cleanup();

                        // Update listener position for spatial audio
                        if let Some(scene_world) = &self.scene_world {
                            for (_entity, (transform, _player)) in
                                scene_world.world.query::<(&Transform, &Player)>().iter()
                            {
                                self.audio_system.set_listener_position(transform.position);
                                break;
                            }
                        }
                    }

                    // Update transforms and camera
                    crate::transform::update_transforms(
                        &mut self.scene_world.as_mut().unwrap().world,
                    );
                    self.update_camera();

                    // Sort splats for correct alpha blending (CPU back-to-front)
                    if let (Some(gpu), Some(scene_world), Some(camera_state)) =
                        (&self.gpu, &self.scene_world, &self.camera_state)
                    {
                        let view_matrix = camera_state.view_matrix();
                        for (_entity, splat) in
                            scene_world.world.query::<&GaussianSplat>().iter()
                        {
                            self.splat_cache.sort_splats(
                                splat.splat_handle,
                                &view_matrix,
                                &gpu.queue,
                            );
                        }
                    }

                    // Phase 3: use compiled pipeline if available
                    if self.compiled_pipeline.is_some() {
                        if let (
                            Some(gpu),
                            Some(scene_world),
                            Some(camera_state),
                            Some(draw_pool),
                            Some(compiled),
                        ) = (
                            &self.gpu,
                            &self.scene_world,
                            &self.camera_state,
                            &self.draw_pool,
                            &self.compiled_pipeline,
                        ) {
                            crate::pipeline::execute_pipeline(
                                gpu,
                                compiled,
                                scene_world,
                                camera_state,
                                draw_pool,
                                &self.mesh_cache,
                                &self.material_cache,
                                &self.splat_cache,
                            );
                        }
                    } else {
                        // Phase 2 fallback: forward rendering
                        if let (
                            Some(gpu),
                            Some(scene_world),
                            Some(camera_state),
                            Some(draw_pool),
                            Some(forward_pipeline),
                        ) = (
                            &self.gpu,
                            &self.scene_world,
                            &self.camera_state,
                            &self.draw_pool,
                            &self.forward_pipeline,
                        ) {
                            crate::renderer::render_scene(
                                gpu,
                                scene_world,
                                camera_state,
                                draw_pool,
                                &self.mesh_cache,
                                &self.material_cache,
                                forward_pipeline,
                            );
                        }
                    }

                    if let Some(gpu) = &self.gpu {
                        gpu.window.request_redraw();
                    }
                } else {
                    // Phase 1: triangle fallback
                    if let Some(gpu) = &self.gpu {
                        crate::renderer::render(gpu);
                        gpu.window.request_redraw();
                    }
                }

                // End of frame: clear transient input state for next frame
                if let Some(input) = &mut self.input_state {
                    input.begin_frame();
                }
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let Some(input) = &mut self.input_state {
            input.handle_device_event(&event);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }
}
