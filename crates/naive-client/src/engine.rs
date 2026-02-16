use std::collections::HashMap;
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
use crate::components::{Camera, CameraMode, CameraRole, CollisionDamage, GaussianSplat, Health, Player, Projectile, Transform};
use crate::events::EventBus;
use crate::font::BitmapFont;
use crate::tween::TweenSystem;
use crate::ui::UiRenderer;
use crate::input::InputState;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::{CharacterController, Collider as ColliderComp, PhysicsWorld, RigidBody as RigidBodyComp};
use crate::scripting::{CameraShakeState, Script, ScriptRuntime};
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

    // UI overlay
    pub bitmap_font: Option<BitmapFont>,
    pub ui_renderer: Option<UiRenderer>,

    // Entity command queue (deferred Lua commands)
    pub entity_commands: crate::world::EntityCommandQueue,

    // Tier 2: Entity pool manager
    pub pool_manager: crate::world::EntityPoolManager,

    // Tier 2: Particle system
    pub particle_system: crate::particles::ParticleSystem,

    // Tier 2: Lua event listeners
    pub lua_event_listeners: HashMap<String, Vec<mlua::RegistryKey>>,
    pub next_lua_listener_id: u64,
    pub lua_listener_id_map: HashMap<u64, (String, usize)>,

    // Render debug: interactive pass toggles (number keys)
    pub render_debug: crate::pipeline::RenderDebugState,

    // Camera shake state
    pub camera_shake: CameraShakeState,
}

impl Engine {
    pub fn new(args: CliArgs) -> Self {
        let project_root = PathBuf::from(&args.project);
        let show_hud = args.hud;
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
            bitmap_font: None,
            ui_renderer: None,
            entity_commands: crate::world::EntityCommandQueue::new(),
            pool_manager: crate::world::EntityPoolManager::new(),
            particle_system: crate::particles::ParticleSystem::new(),
            lua_event_listeners: HashMap::new(),
            next_lua_listener_id: 0,
            lua_listener_id_map: HashMap::new(),
            render_debug: crate::pipeline::RenderDebugState {
                show_hud,
                ..Default::default()
            },
            camera_shake: CameraShakeState::new(),
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

        // Generate default audio files if they don't exist
        crate::audio_gen::generate_default_sounds(&self.project_root);

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

        // UI overlay: bitmap font atlas + 2D renderer
        let font = crate::font::create_bitmap_font(&gpu.device, &gpu.queue);
        let ui = UiRenderer::new(&gpu.device, gpu.config.format, &font);
        self.bitmap_font = Some(font);
        self.ui_renderer = Some(ui);

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
                            let restitution = col_def.restitution;
                            let friction = col_def.friction;
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
                                    let ccd = entity_def
                                        .components
                                        .rigid_body
                                        .as_ref()
                                        .map(|rb| rb.ccd)
                                        .unwrap_or(false);
                                    let (rb_handle, col_handle) = physics_world
                                        .add_dynamic_body(entity, pos, rot, shape.clone(), mass, restitution, friction, ccd);
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
                                        .add_static_body(entity, pos, rot, shape.clone(), is_trigger, restitution, friction);
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
        if let (Some(pw), Some(sw)) = (&mut self.physics_world, &self.scene_world) {
            let physics_ptr = pw as *mut PhysicsWorld;
            let sw_ptr = sw as *const SceneWorld;
            if let Err(e) = script_runtime.register_physics_api(physics_ptr, sw_ptr) {
                tracing::error!("Failed to register physics API: {}", e);
            }
        }

        // Register entity manipulation API
        if let Some(sw) = &mut self.scene_world {
            let sw_ptr = sw as *mut SceneWorld;
            if let Err(e) = script_runtime.register_entity_api(sw_ptr) {
                tracing::error!("Failed to register entity API: {}", e);
            }
            // Entity command API (spawn, destroy, scale, visibility, pooling)
            let cmd_ptr = &mut self.entity_commands as *mut crate::world::EntityCommandQueue;
            let pool_mgr_ptr = &mut self.pool_manager as *mut crate::world::EntityPoolManager;
            if let Err(e) = script_runtime.register_entity_command_api(sw_ptr, cmd_ptr, pool_mgr_ptr) {
                tracing::error!("Failed to register entity command API: {}", e);
            }
        }

        // Register UI overlay API
        if let (Some(ui), Some(font), Some(gpu)) = (
            &mut self.ui_renderer,
            &self.bitmap_font,
            &self.gpu,
        ) {
            let ui_ptr = ui as *mut UiRenderer;
            let font_ptr = font as *const BitmapFont;
            let config_ptr = &gpu.config as *const wgpu::SurfaceConfiguration;
            if let Err(e) = script_runtime.register_ui_api(ui_ptr, font_ptr, config_ptr) {
                tracing::error!("Failed to register UI API: {}", e);
            }
        }

        // Register camera API (world_to_screen)
        if let (Some(cs), Some(gpu)) = (&self.camera_state, &self.gpu) {
            let cs_ptr = cs as *const crate::camera::CameraState;
            let config_ptr = &gpu.config as *const wgpu::SurfaceConfiguration;
            if let Err(e) = script_runtime.register_camera_api(cs_ptr, config_ptr) {
                tracing::error!("Failed to register camera API: {}", e);
            }
        }

        // Register camera shake API
        {
            let shake_ptr = &mut self.camera_shake as *mut CameraShakeState;
            if let Err(e) = script_runtime.register_camera_shake_api(shake_ptr) {
                tracing::error!("Failed to register camera shake API: {}", e);
            }
        }

        // Register event bus API (with Lua listener support)
        {
            let bus_ptr = &mut self.event_bus as *mut crate::events::EventBus;
            let listeners_ptr = &mut self.lua_event_listeners as *mut HashMap<String, Vec<mlua::RegistryKey>>;
            let next_id_ptr = &mut self.next_lua_listener_id as *mut u64;
            let id_map_ptr = &mut self.lua_listener_id_map as *mut HashMap<u64, (String, usize)>;
            if let Err(e) = script_runtime.register_event_api(bus_ptr, listeners_ptr, next_id_ptr, id_map_ptr) {
                tracing::error!("Failed to register event API: {}", e);
            }
        }

        // Register audio API
        {
            let audio_ptr = &mut self.audio_system as *mut AudioSystem;
            if let Err(e) = script_runtime.register_audio_api(audio_ptr, self.project_root.clone()) {
                tracing::error!("Failed to register audio API: {}", e);
            }
        }

        // Register particle API
        if let Some(sw) = &mut self.scene_world {
            let sw_ptr = sw as *mut SceneWorld;
            let ps_ptr = &mut self.particle_system as *mut crate::particles::ParticleSystem;
            if let Err(e) = script_runtime.register_particle_api(sw_ptr, ps_ptr) {
                tracing::error!("Failed to register particle API: {}", e);
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

        // Call init on all scripts (collect first to release world borrow before Lua runs)
        if let Some(sw) = &self.scene_world {
            let uninit_entities: Vec<hecs::Entity> = sw.world.query::<&Script>()
                .iter()
                .filter(|(_, script)| !script.initialized)
                .map(|(entity, _)| entity)
                .collect();
            for entity in uninit_entities {
                script_runtime.call_init(entity);
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
            None => {
                // Auto-detect: use pipelines/render.yaml if it exists
                let default_path = self.project_root.join("pipelines/render.yaml");
                if default_path.exists() {
                    "pipelines/render.yaml".to_string()
                } else {
                    return;
                }
            }
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
                let reload_candidates: Vec<(hecs::Entity, std::path::PathBuf)> = scene_world.world
                    .query::<&Script>().iter()
                    .filter_map(|(entity, script)| {
                        let script_source_str = script.source.to_string_lossy();
                        for changed_path in &script_paths {
                            let changed_str = changed_path.to_string_lossy();
                            if changed_str.ends_with(&*script_source_str)
                                || changed_str.ends_with(script.source.file_name().unwrap_or_default().to_string_lossy().as_ref())
                            {
                                return Some((entity, script.source.clone()));
                            }
                        }
                        None
                    })
                    .collect();
                for (entity, source) in reload_candidates {
                    match script_runtime.hot_reload_script(entity, &self.project_root, &source) {
                        Ok(true) => tracing::info!("Script hot-reloaded: {:?}", source),
                        Ok(false) => {}
                        Err(e) => tracing::error!("Script hot-reload failed: {}", e),
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

            // Get pitch limits from CameraMode if available
            let (pitch_min, pitch_max) = if let Ok(camera_mode) = scene_world.world.get::<&CameraMode>(entity) {
                match &*camera_mode {
                    CameraMode::ThirdPerson { pitch_min, pitch_max, .. } => (*pitch_min, *pitch_max),
                    CameraMode::FirstPerson => (-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01),
                }
            } else {
                (-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01)
            };
            let new_pitch = (player.pitch - mouse_delta.y * sensitivity).clamp(pitch_min, pitch_max);

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

    }

    /// Process collision damage: auto-damage on physics contact.
    fn process_collision_damage(&mut self) {
        let scene_world = match &mut self.scene_world {
            Some(sw) => sw as *mut SceneWorld,
            None => return,
        };
        let physics_world = match &self.physics_world {
            Some(pw) => pw,
            None => return,
        };
        let script_runtime = match &self.script_runtime {
            Some(sr) => sr,
            None => return,
        };
        let sw = unsafe { &mut *scene_world };

        // Build reverse lookup: entity -> string ID
        let entity_to_id: HashMap<hecs::Entity, String> = sw
            .entity_registry
            .iter()
            .map(|(id, &e)| (e, id.clone()))
            .collect();

        let mut destroys = Vec::new();

        for event in &physics_world.collision_events {
            if !event.started {
                continue;
            }

            // Check both directions
            for &(dmg_entity, target_entity) in &[
                (event.entity_a, event.entity_b),
                (event.entity_b, event.entity_a),
            ] {
                let cd = match sw.world.get::<&CollisionDamage>(dmg_entity) {
                    Ok(cd) => (cd.damage, cd.destroy_on_hit),
                    Err(_) => continue,
                };
                let (damage, destroy_on_hit) = cd;

                // Skip if target has no health
                if sw.world.get::<&Health>(target_entity).is_err() {
                    continue;
                }

                // Owner skip: if damage entity is a projectile, skip if target is the owner
                if let Ok(proj) = sw.world.get::<&Projectile>(dmg_entity) {
                    let target_id = entity_to_id.get(&target_entity).map(|s| s.as_str()).unwrap_or("");
                    if target_id == proj.owner_id {
                        continue;
                    }
                }

                // Apply damage
                if let Ok(mut health) = sw.world.get::<&mut Health>(target_entity) {
                    health.current = (health.current - damage).max(0.0);
                }

                // Call on_damage on the target
                let source_id = entity_to_id.get(&dmg_entity).cloned().unwrap_or_default();
                script_runtime.call_on_damage(target_entity, damage, source_id);

                // Queue destruction of damage entity if destroy_on_hit
                if destroy_on_hit {
                    if let Some(id) = entity_to_id.get(&dmg_entity) {
                        if !destroys.contains(id) {
                            destroys.push(id.clone());
                        }
                    }
                }
            }
        }

        // Queue destroys
        for id in destroys {
            self.entity_commands.destroys.push(id);
        }
    }

    /// Update projectile ages and destroy expired ones.
    fn update_projectiles(&mut self) {
        let scene_world = match &mut self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let dt = self.delta_time;

        let mut expired = Vec::new();

        for (entity, projectile) in scene_world.world.query::<&mut Projectile>().iter() {
            projectile.age += dt;
            if projectile.age >= projectile.lifetime {
                expired.push(entity);
            }
        }

        // Build entity->id lookup for expired projectiles
        let entity_to_id: HashMap<hecs::Entity, String> = scene_world
            .entity_registry
            .iter()
            .map(|(id, &e)| (e, id.clone()))
            .collect();

        for entity in expired {
            if let Some(id) = entity_to_id.get(&entity) {
                self.entity_commands.destroys.push(id.clone());
            }
        }
    }

    /// Process the health system: detect deaths and fire on_death callbacks.
    fn process_health_system(&mut self) {
        let scene_world = match &mut self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let script_runtime = match &self.script_runtime {
            Some(sr) => sr,
            None => return,
        };

        let mut newly_dead = Vec::new();

        for (entity, health) in scene_world.world.query::<&mut Health>().iter() {
            if health.current <= 0.0 && !health.dead {
                health.dead = true;
                newly_dead.push(entity);
            }
        }

        for entity in newly_dead {
            script_runtime.call_on_death(entity);
        }
    }

    /// Process deferred entity commands (spawn/destroy/scale/visibility).
    fn process_entity_commands(&mut self) {
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        // Tier 2: Process destroys FIRST (fixes destroy+spawn same-frame bug)
        let destroys: Vec<_> = self.entity_commands.destroys.drain(..).collect();
        for id in destroys {
            if let Some(scene_world) = &mut self.scene_world {
                // Call on_destroy hook before despawning
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if let Some(script_runtime) = &self.script_runtime {
                        script_runtime.call_on_destroy(entity);
                    }
                    // Clean up physics body if entity has one
                    if let Ok(rb) = scene_world.world.get::<&crate::physics::RigidBody>(entity) {
                        let rb_handle = rb.handle;
                        drop(rb);
                        if let Some(physics_world) = &mut self.physics_world {
                            physics_world.remove_body(rb_handle);
                        }
                    }
                }
                crate::world::destroy_runtime_entity(scene_world, &id);
            }
        }

        // Process spawns (after destroys, so destroy+spawn same ID works)
        let spawns: Vec<_> = self.entity_commands.spawns.drain(..).collect();
        for cmd in spawns {
            if let Some(scene_world) = &mut self.scene_world {
                crate::world::spawn_runtime_entity(
                    scene_world,
                    &cmd.id,
                    &cmd.mesh,
                    &cmd.material,
                    cmd.position,
                    cmd.scale,
                    &gpu.device,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                );
            }
        }

        // Process projectile spawns
        let proj_spawns: Vec<_> = self.entity_commands.projectile_spawns.drain(..).collect();
        for cmd in &proj_spawns {
            if let (Some(scene_world), Some(physics_world)) = (&mut self.scene_world, &mut self.physics_world) {
                crate::world::spawn_projectile_entity(
                    scene_world,
                    cmd,
                    &gpu.device,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                    physics_world,
                );
            }
        }

        // Process pool operations
        let pool_ops: Vec<_> = self.entity_commands.pool_ops.drain(..).collect();
        for op in pool_ops {
            if let Some(scene_world) = &mut self.scene_world {
                match op {
                    crate::world::PoolOp::Release(id) => {
                        if let Some(&entity) = scene_world.entity_registry.get(&id) {
                            // Hide the entity and mark as inactive
                            let _ = scene_world.world.insert_one(entity, crate::components::Hidden);
                            if let Ok(mut pooled) = scene_world.world.get::<&mut crate::components::Pooled>(entity) {
                                pooled.active = false;
                                let pool_name = pooled.pool_name.clone();
                                drop(pooled);
                                self.pool_manager.release(&pool_name, &id);
                            }
                        }
                    }
                }
            }
        }

        // Process scale updates
        let scale_updates: Vec<_> = self.entity_commands.scale_updates.drain(..).collect();
        for (id, scale) in scale_updates {
            if let Some(scene_world) = &mut self.scene_world {
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                        transform.scale = glam::Vec3::from(scale);
                        transform.dirty = true;
                    }
                }
            }
        }

        // Process visibility updates
        let vis_updates: Vec<_> = self.entity_commands.visibility_updates.drain(..).collect();
        for (id, visible) in vis_updates {
            if let Some(scene_world) = &mut self.scene_world {
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if visible {
                        let _ = scene_world.world.remove_one::<crate::components::Hidden>(entity);
                    } else {
                        let _ = scene_world.world.insert_one(entity, crate::components::Hidden);
                    }
                }
            }
        }
    }

    /// Process a pending scene load (deferred from Lua `scene.load(path)`).
    fn process_pending_scene_load(&mut self) {
        let scene_rel = match self.entity_commands.pending_scene_load.take() {
            Some(p) => p,
            None => return,
        };

        let scene_path = self.project_root.join(&scene_rel);
        if !scene_path.exists() {
            tracing::error!("scene.load: file not found: {:?}", scene_path);
            return;
        }

        let scene = match crate::scene::load_scene(&scene_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("scene.load: failed to parse: {}", e);
                return;
            }
        };

        // 1. Call on_destroy on all scripted entities
        if let (Some(sw), Some(sr)) = (&self.scene_world, &self.script_runtime) {
            let scripted: Vec<hecs::Entity> = sw.world.query::<&Script>()
                .iter()
                .map(|(e, _)| e)
                .collect();
            for entity in scripted {
                sr.call_on_destroy(entity);
            }
        }

        // 2. Clear script environments
        if let Some(sr) = &mut self.script_runtime {
            sr.entity_envs.clear();
            sr.script_sources.clear();
        }

        // 3. Clear ECS world and entity registry in-place
        if let Some(sw) = &mut self.scene_world {
            sw.world.clear();
            sw.entity_registry.clear();
            sw.current_scene = None;
        }

        // 4. Replace physics world in-place
        let gravity = glam::Vec3::from(scene.settings.gravity);
        if let Some(pw) = &mut self.physics_world {
            *pw = PhysicsWorld::new(gravity);
        }

        // 5. Clear pool manager, particle system, lua event listeners, camera shake
        self.pool_manager = crate::world::EntityPoolManager::new();
        self.particle_system = crate::particles::ParticleSystem::new();
        self.lua_event_listeners.clear();
        self.next_lua_listener_id = 0;
        self.lua_listener_id_map.clear();
        self.camera_shake = CameraShakeState::new();

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        // 6. Re-spawn entities
        if let Some(sw) = &mut self.scene_world {
            crate::world::spawn_all_entities(
                sw,
                &scene,
                &gpu.device,
                &self.project_root,
                &mut self.mesh_cache,
                &mut self.material_cache,
                &mut self.splat_cache,
                None,
            );
        }

        // 7. Spawn physics components for entities
        if let Some(sw) = &mut self.scene_world {
            if let (Some(scene_data), Some(pw)) = (&sw.current_scene.clone(), &mut self.physics_world) {
                for entity_def in &scene_data.entities {
                    if let Some(&entity) = sw.entity_registry.get(&entity_def.id) {
                        let pos = entity_def.components.transform.as_ref()
                            .map(|t| glam::Vec3::from(t.position))
                            .unwrap_or(glam::Vec3::ZERO);
                        let rot = entity_def.components.transform.as_ref()
                            .map(|t| crate::world::euler_degrees_to_quat(t.rotation))
                            .unwrap_or(glam::Quat::IDENTITY);

                        if let Some(cc_def) = &entity_def.components.character_controller {
                            let half_height = cc_def.height / 2.0 - cc_def.radius;
                            let (rb_handle, col_handle) = pw.add_character_body(entity, pos, half_height.max(0.1), cc_def.radius);
                            let rb_comp = crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Kinematic };
                            let col_comp = crate::physics::Collider { handle: col_handle, shape: crate::physics::PhysicsShape::Capsule { half_height: half_height.max(0.1), radius: cc_def.radius }, is_trigger: false };
                            let cc_comp = CharacterController { move_speed: cc_def.move_speed, sprint_multiplier: cc_def.sprint_multiplier, jump_impulse: cc_def.jump_impulse, step_height: cc_def.step_height, ..Default::default() };
                            let player = crate::components::Player { height: cc_def.height, radius: cc_def.radius, ..Default::default() };
                            let _ = sw.world.insert(entity, (rb_comp, col_comp, cc_comp, player));
                        } else if let Some(col_def) = &entity_def.components.collider {
                            let shape = crate::world::parse_collider_shape(col_def);
                            let is_trigger = col_def.is_trigger;
                            let restitution = col_def.restitution;
                            let friction = col_def.friction;
                            let body_type = entity_def.components.rigid_body.as_ref().map(|rb| rb.body_type.as_str()).unwrap_or("static");
                            match body_type {
                                "dynamic" => {
                                    let mass = entity_def.components.rigid_body.as_ref().map(|rb| rb.mass).unwrap_or(1.0);
                                    let ccd = entity_def.components.rigid_body.as_ref().map(|rb| rb.ccd).unwrap_or(false);
                                    let (rb_handle, col_handle) = pw.add_dynamic_body(entity, pos, rot, shape.clone(), mass, restitution, friction, ccd);
                                    let rb_comp = crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Dynamic };
                                    let col_comp = crate::physics::Collider { handle: col_handle, shape, is_trigger };
                                    let _ = sw.world.insert(entity, (rb_comp, col_comp));
                                }
                                _ => {
                                    let (rb_handle, col_handle) = pw.add_static_body(entity, pos, rot, shape.clone(), is_trigger, restitution, friction);
                                    let rb_comp = crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Static };
                                    let col_comp = crate::physics::Collider { handle: col_handle, shape, is_trigger };
                                    let _ = sw.world.insert(entity, (rb_comp, col_comp));
                                }
                            }
                        }
                    }
                }
            }
        }

        // 8. Re-load scripts for the new scene
        if let Some(sw) = &mut self.scene_world {
            if let Some(sr) = &mut self.script_runtime {
                if let Some(scene_data) = &sw.current_scene.clone() {
                    for entity_def in &scene_data.entities {
                        if let Some(script_def) = &entity_def.components.script {
                            if let Some(&entity) = sw.entity_registry.get(&entity_def.id) {
                                let source_path = std::path::PathBuf::from(&script_def.source);
                                let script_comp = Script { source: source_path.clone(), initialized: false };
                                let _ = sw.world.insert_one(entity, script_comp);
                                if let Err(e) = sr.load_script(entity, &self.project_root, &source_path) {
                                    tracing::error!("scene.load: script for '{}' failed: {}", entity_def.id, e);
                                } else {
                                    let _ = sr.set_entity_string_id(entity, &entity_def.id);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Call init on all scripts
        if let (Some(sw), Some(sr)) = (&self.scene_world, &self.script_runtime) {
            let uninit: Vec<hecs::Entity> = sw.world.query::<&Script>()
                .iter()
                .filter(|(_, s)| !s.initialized)
                .map(|(e, _)| e)
                .collect();
            for entity in uninit {
                sr.call_init(entity);
            }
        }
        if let Some(sw) = &mut self.scene_world {
            for (_entity, script) in sw.world.query::<&mut Script>().iter() {
                script.initialized = true;
            }
        }

        // 9. Update scene_path for hot-reload
        self.scene_path = Some(scene_path);

        tracing::info!("Scene loaded via scene.load(\"{}\")", scene_rel);
    }

    /// Compute camera shake offset, decaying the timer.
    fn compute_camera_shake(&mut self, dt: f32) -> glam::Vec3 {
        if self.camera_shake.timer <= 0.0 {
            return glam::Vec3::ZERO;
        }
        self.camera_shake.timer -= dt;
        if self.camera_shake.timer < 0.0 {
            self.camera_shake.timer = 0.0;
        }
        let t = if self.camera_shake.duration > 0.0 {
            self.camera_shake.timer / self.camera_shake.duration
        } else {
            0.0
        };
        let scale = self.camera_shake.intensity * t;
        // Simple pseudo-random using seed and timer
        let s = self.camera_shake.seed as f32;
        let phase = self.camera_shake.timer * 37.0 + s;
        let x = (phase * 7.31).sin();
        let y = (phase * 13.17).sin();
        let z = (phase * 23.41).sin();
        glam::Vec3::new(x * scale, y * scale, z * scale)
    }

    /// Update the camera uniform from the main camera entity.
    fn update_camera(&mut self) {
        // Compute camera shake offset before borrowing other fields
        let shake_offset = self.compute_camera_shake(self.delta_time);

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
        for (entity, (transform, player, camera)) in
            scene_world.world.query::<(&Transform, &Player, &Camera)>().iter()
        {
            if camera.role == CameraRole::Main {
                // Check for CameraMode component
                let camera_mode = scene_world.world.get::<&CameraMode>(entity).ok();

                let is_third_person = camera_mode.as_ref().map(|cm| matches!(**cm, CameraMode::ThirdPerson { .. })).unwrap_or(false);

                if is_third_person {
                    let (distance, height_offset) = if let Some(ref cm) = camera_mode {
                        match **cm {
                            CameraMode::ThirdPerson { distance, height_offset, .. } => (distance, height_offset),
                            _ => (4.0, 1.5),
                        }
                    } else {
                        (4.0, 1.5)
                    };
                    // Third-person camera: orbit behind player
                    let target = transform.position + glam::Vec3::new(0.0, height_offset, 0.0);

                    // Camera position orbits around target based on yaw and pitch
                    let cam_offset = glam::Vec3::new(
                        player.yaw.sin() * player.pitch.cos() * distance,
                        player.pitch.sin() * distance,
                        player.yaw.cos() * player.pitch.cos() * distance,
                    );
                    let mut desired_pos = target + cam_offset;

                    // Wall collision: raycast from target to desired camera position
                    if let Some(physics_world) = &self.physics_world {
                        let ray_dir = (desired_pos - target).normalize_or_zero();
                        let ray_dist = (desired_pos - target).length();
                        if let Some((_entity, toi, _hit, _normal)) = physics_world.raycast_detailed(
                            target, ray_dir, ray_dist, None,
                        ) {
                            // Pull camera closer to avoid clipping through walls
                            desired_pos = target + ray_dir * (toi - 0.2).max(0.5);
                        }
                    }

                    // Look rotation: camera looks from desired_pos toward target
                    let forward = (target - desired_pos).normalize_or_zero();
                    let look_rotation = if forward.length_squared() > 0.001 {
                        glam::Quat::from_rotation_arc(-glam::Vec3::Z, forward)
                    } else {
                        glam::Quat::IDENTITY
                    };

                    let cam_transform = Transform {
                        position: desired_pos + shake_offset,
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
                } else {
                    // First-person camera (existing behavior)
                    let look_rotation = glam::Quat::from_rotation_y(player.yaw)
                        * glam::Quat::from_rotation_x(player.pitch);
                    let cam_transform = Transform {
                        position: transform.position + glam::Vec3::new(0.0, player.height * 0.4, 0.0) + shake_offset,
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
                }
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
                    let cam_transform = Transform {
                        position: transform.position + shake_offset,
                        ..transform.clone()
                    };
                    camera_state.update(
                        &gpu.queue,
                        camera,
                        &cam_transform,
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

                // Render debug toggles: 0 always toggles HUD, 1-6 only when HUD is visible
                if let Some(input) = &self.input_state {
                    if input.just_pressed_key(KeyCode::Digit0) {
                        self.render_debug.show_hud = !self.render_debug.show_hud;
                    }
                    if self.render_debug.show_hud {
                        if input.just_pressed_key(KeyCode::Digit1) {
                            self.render_debug.bloom_enabled = !self.render_debug.bloom_enabled;
                            tracing::info!("Bloom: {}", if self.render_debug.bloom_enabled { "ON" } else { "OFF" });
                        }
                        if input.just_pressed_key(KeyCode::Digit2) {
                            // Cycle light intensity: 1x → 5x → 10x → 20x → 1x
                            self.render_debug.light_intensity_mult = match self.render_debug.light_intensity_mult as u32 {
                                0..=1 => 5.0,
                                2..=5 => 10.0,
                                6..=10 => 20.0,
                                _ => 1.0,
                            };
                            tracing::info!("Light intensity: {}x", self.render_debug.light_intensity_mult);
                        }
                        if input.just_pressed_key(KeyCode::Digit3) {
                            self.render_debug.point_lights_enabled = !self.render_debug.point_lights_enabled;
                            tracing::info!("Point Lights: {}", if self.render_debug.point_lights_enabled { "ON" } else { "OFF" });
                        }
                        if input.just_pressed_key(KeyCode::Digit4) {
                            self.render_debug.emission_enabled = !self.render_debug.emission_enabled;
                            tracing::info!("Emission: {}", if self.render_debug.emission_enabled { "ON" } else { "OFF" });
                        }
                        if input.just_pressed_key(KeyCode::Digit5) {
                            self.render_debug.torch_flicker_enabled = !self.render_debug.torch_flicker_enabled;
                            tracing::info!("Torch Flicker: {}", if self.render_debug.torch_flicker_enabled { "ON" } else { "OFF" });
                        }
                        if input.just_pressed_key(KeyCode::Digit6) {
                            // Cycle ambient override: 0 → 0.3 → 1.0 → 3.0 → 0
                            self.render_debug.ambient_override = match self.render_debug.ambient_override {
                                x if x < 0.1 => 0.3,
                                x if x < 0.5 => 1.0,
                                x if x < 2.0 => 3.0,
                                _ => 0.0,
                            };
                            tracing::info!("Ambient override: {}", self.render_debug.ambient_override);
                        }
                    }
                }

                // Poll for file changes (shader + scene + pipeline)
                self.poll_changes();

                if self.scene_world.is_some() {
                    if !self.paused {
                        // Phase 5: FPS controller update (player input only when cursor captured)
                        if self.input_state.as_ref().map(|i| i.cursor_captured).unwrap_or(false) {
                            self.update_fps_controller();
                        }

                        // Always step physics (gravity, collisions, etc.)
                        if let (Some(scene_world), Some(physics_world)) =
                            (&mut self.scene_world, &mut self.physics_world)
                        {
                            physics_world.step(self.delta_time);
                            physics_world.sync_to_ecs(&mut scene_world.world);
                        }

                        // Dispatch collision events to scripts
                        if let (Some(scene_world), Some(physics_world), Some(script_runtime)) =
                            (&self.scene_world, &self.physics_world, &self.script_runtime)
                        {
                            // Build reverse lookup: entity -> string id
                            let entity_to_id: HashMap<hecs::Entity, &str> = scene_world
                                .entity_registry
                                .iter()
                                .map(|(id, &e)| (e, id.as_str()))
                                .collect();

                            for event in &physics_world.collision_events {
                                let id_a = entity_to_id.get(&event.entity_a).copied().unwrap_or("unknown");
                                let id_b = entity_to_id.get(&event.entity_b).copied().unwrap_or("unknown");
                                // Dispatch both directions
                                script_runtime.call_on_collision(event.entity_a, id_b);
                                script_runtime.call_on_collision(event.entity_b, id_a);
                            }
                        }

                        // Tier 1: Process collision damage (auto-damage + projectile hits)
                        self.process_collision_damage();

                        // Tier 1: Update projectiles (age tracking, lifetime expiry)
                        self.update_projectiles();

                        // Tier 1: Process health system (on_death callbacks)
                        self.process_health_system();

                        // Phase 6: Update scripts
                        let dt = self.delta_time;
                        if let (Some(scene_world), Some(script_runtime)) =
                            (&self.scene_world, &self.script_runtime)
                        {
                            let scripted: Vec<hecs::Entity> = scene_world.world
                                .query::<&Script>().iter()
                                .map(|(e, _)| e)
                                .collect();
                            for entity in scripted {
                                script_runtime.call_update(entity, dt);
                            }
                        }

                        // Process deferred entity commands from Lua
                        self.process_entity_commands();

                        // Process deferred scene load (must be after entity commands)
                        self.process_pending_scene_load();

                        // Tier 2: Dispatch Lua event listeners
                        self.event_bus.tick(dt as f64);
                        let flushed_events = self.event_bus.flush();
                        if let Some(script_runtime) = &self.script_runtime {
                            for event in &flushed_events {
                                if let Some(listeners) = self.lua_event_listeners.get(&event.event_type) {
                                    for key in listeners {
                                        if let Ok(func) = script_runtime.lua.registry_value::<mlua::Function>(key) {
                                            let lua = &script_runtime.lua;
                                            if let Ok(tbl) = lua.create_table() {
                                                let _ = tbl.set("type", event.event_type.clone());
                                                if let Ok(data_tbl) = lua.create_table() {
                                                    for (k, v) in &event.data {
                                                        match v {
                                                            serde_json::Value::Number(n) => {
                                                                if let Some(f) = n.as_f64() {
                                                                    let _ = data_tbl.set(k.as_str(), f);
                                                                }
                                                            }
                                                            serde_json::Value::String(s) => {
                                                                let _ = data_tbl.set(k.as_str(), s.as_str());
                                                            }
                                                            serde_json::Value::Bool(b) => {
                                                                let _ = data_tbl.set(k.as_str(), *b);
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                    let _ = tbl.set("data", data_tbl);
                                                }
                                                if let Err(e) = func.call::<()>(tbl) {
                                                    tracing::error!("Lua event listener error: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let _tween_results = self.tween_system.update(dt);
                        self.audio_system.cleanup();

                        // Tier 2: Update particle system
                        if let Some(scene_world) = &self.scene_world {
                            self.particle_system.update(dt, scene_world);
                        }

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

                    // Tier 2: Grow GPU draw buffer if needed
                    if let (Some(gpu), Some(scene_world), Some(draw_pool)) =
                        (&self.gpu, &self.scene_world, &mut self.draw_pool)
                    {
                        let mut visible_count = 0u32;
                        for (entity, _) in scene_world.world.query::<&crate::components::MeshRenderer>().iter() {
                            if scene_world.world.get::<&crate::components::Hidden>(entity).is_ok() {
                                continue;
                            }
                            visible_count += 1;
                        }
                        draw_pool.ensure_capacity(&gpu.device, visible_count);
                    }

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

                    // Acquire swapchain and render 3D scene + UI overlay
                    if let Some(gpu) = &self.gpu {
                        let output = match gpu.surface.get_current_texture() {
                            Ok(t) => t,
                            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                                gpu.surface.configure(&gpu.device, &gpu.config);
                                if let Some(gpu) = &self.gpu {
                                    gpu.window.request_redraw();
                                }
                                if let Some(input) = &mut self.input_state {
                                    input.begin_frame();
                                }
                                return;
                            }
                            Err(e) => {
                                tracing::error!("Surface error: {:?}", e);
                                if let Some(gpu) = &self.gpu {
                                    gpu.window.request_redraw();
                                }
                                if let Some(input) = &mut self.input_state {
                                    input.begin_frame();
                                }
                                return;
                            }
                        };

                        let swapchain_view = output
                            .texture
                            .create_view(&wgpu::TextureViewDescriptor::default());

                        // Render 3D scene
                        if self.compiled_pipeline.is_some() {
                            if let (
                                Some(scene_world),
                                Some(camera_state),
                                Some(draw_pool),
                                Some(compiled),
                            ) = (
                                &self.scene_world,
                                &self.camera_state,
                                &self.draw_pool,
                                &self.compiled_pipeline,
                            ) {
                                let encoder = crate::pipeline::execute_pipeline_to_view(
                                    gpu,
                                    compiled,
                                    scene_world,
                                    camera_state,
                                    draw_pool,
                                    &self.mesh_cache,
                                    &self.material_cache,
                                    &self.splat_cache,
                                    &swapchain_view,
                                    &self.render_debug,
                                );
                                gpu.queue.submit(std::iter::once(encoder.finish()));
                            }
                        } else if let (
                            Some(scene_world),
                            Some(camera_state),
                            Some(draw_pool),
                            Some(forward_pipeline),
                        ) = (
                            &self.scene_world,
                            &self.camera_state,
                            &self.draw_pool,
                            &self.forward_pipeline,
                        ) {
                            let mut encoder = gpu.device.create_command_encoder(
                                &wgpu::CommandEncoderDescriptor {
                                    label: Some("Forward Render Encoder"),
                                },
                            );
                            crate::renderer::render_scene_to_view(
                                gpu,
                                scene_world,
                                camera_state,
                                draw_pool,
                                &self.mesh_cache,
                                &self.material_cache,
                                forward_pipeline,
                                &swapchain_view,
                                &mut encoder,
                            );
                            gpu.queue.submit(std::iter::once(encoder.finish()));
                        }

                        // UI overlay pass (drawn on top of 3D scene)
                        if let (Some(ui), Some(font)) = (
                            &mut self.ui_renderer,
                            &self.bitmap_font,
                        ) {
                            // Queue render debug HUD if enabled
                            if self.render_debug.show_hud {
                                let on = [0.3, 1.0, 0.3, 1.0];
                                let off = [1.0, 0.3, 0.3, 1.0];
                                let val = [1.0, 0.9, 0.3, 1.0];
                                let hdr = [0.7, 0.7, 0.7, 1.0];
                                let sz = 16.0;
                                let x = 10.0;
                                let mut y = 10.0;

                                ui.draw_text(x, y, "RENDER DEBUG", sz, hdr, font); y += sz + 4.0;
                                let c = if self.render_debug.bloom_enabled { on } else { off };
                                ui.draw_text(x, y, &format!("[1] Bloom: {}", if self.render_debug.bloom_enabled { "ON" } else { "OFF" }), sz, c, font); y += sz + 2.0;
                                ui.draw_text(x, y, &format!("[2] Light Boost: {}x", self.render_debug.light_intensity_mult), sz, val, font); y += sz + 2.0;
                                let c = if self.render_debug.point_lights_enabled { on } else { off };
                                ui.draw_text(x, y, &format!("[3] Point Lights: {}", if self.render_debug.point_lights_enabled { "ON" } else { "OFF" }), sz, c, font); y += sz + 2.0;
                                let c = if self.render_debug.emission_enabled { on } else { off };
                                ui.draw_text(x, y, &format!("[4] Emission: {}", if self.render_debug.emission_enabled { "ON" } else { "OFF" }), sz, c, font); y += sz + 2.0;
                                let c = if self.render_debug.torch_flicker_enabled { on } else { off };
                                ui.draw_text(x, y, &format!("[5] Torch Flicker: {}", if self.render_debug.torch_flicker_enabled { "ON" } else { "OFF" }), sz, c, font); y += sz + 2.0;
                                ui.draw_text(x, y, &format!("[6] Ambient: {}", if self.render_debug.ambient_override < 0.1 { "scene".to_string() } else { format!("{:.1}", self.render_debug.ambient_override) }), sz, val, font); y += sz + 2.0;
                                ui.draw_text(x, y, "[0] Toggle this HUD", sz, hdr, font);
                            }

                            let mut ui_encoder = gpu.device.create_command_encoder(
                                &wgpu::CommandEncoderDescriptor {
                                    label: Some("UI Encoder"),
                                },
                            );
                            ui.render(
                                &gpu.device,
                                &gpu.queue,
                                &mut ui_encoder,
                                &swapchain_view,
                                font,
                                gpu.config.width,
                                gpu.config.height,
                                self.delta_time,
                            );
                            gpu.queue.submit(std::iter::once(ui_encoder.finish()));
                        }

                        output.present();
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
