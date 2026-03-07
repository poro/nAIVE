use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
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
use crate::editor_camera::EditorCamera;
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
    pub scene_world: Option<Rc<RefCell<SceneWorld>>>,
    pub mesh_cache: MeshCache,
    pub material_cache: MaterialCache,
    pub splat_cache: SplatCache,
    pub camera_state: Option<Rc<RefCell<CameraState>>>,
    pub draw_pool: Option<DrawUniformPool>,
    pub forward_pipeline: Option<wgpu::RenderPipeline>,
    scene_path: Option<PathBuf>,

    // Phase 3: compiled render pipeline
    pub compiled_pipeline: Option<CompiledPipeline>,
    pipeline_path: Option<PathBuf>,

    // Phase 5: input + physics
    pub input_state: Option<Rc<RefCell<InputState>>>,
    pub physics_world: Option<Rc<RefCell<PhysicsWorld>>>,
    last_frame_time: Option<instant::Instant>,
    delta_time: f32,

    // Phase 6: scripting
    pub script_runtime: Option<ScriptRuntime>,

    // Phase 7: events + audio + tweens
    pub event_bus: Rc<RefCell<EventBus>>,
    pub audio_system: Rc<RefCell<AudioSystem>>,
    pub tween_system: TweenSystem,

    // Phase 8: command socket
    pub command_server: Option<CommandServer>,
    pub paused: bool,

    // UI overlay
    pub bitmap_font: Option<Rc<RefCell<BitmapFont>>>,
    pub ui_renderer: Option<Rc<RefCell<UiRenderer>>>,

    // Entity command queue (deferred Lua commands)
    pub entity_commands: Rc<RefCell<crate::world::EntityCommandQueue>>,

    // Tier 2: Entity pool manager
    pub pool_manager: Rc<RefCell<crate::world::EntityPoolManager>>,

    // Tier 2: Particle system
    pub particle_system: Rc<RefCell<crate::particles::ParticleSystem>>,

    // Tier 2: Lua event listeners
    pub lua_event_listeners: Rc<RefCell<HashMap<String, Vec<mlua::RegistryKey>>>>,
    pub next_lua_listener_id: Rc<RefCell<u64>>,
    pub lua_listener_id_map: Rc<RefCell<HashMap<u64, (String, usize)>>>,

    // Render debug: interactive pass toggles (number keys)
    pub render_debug: crate::pipeline::RenderDebugState,

    // Camera shake state
    pub camera_shake: Rc<RefCell<CameraShakeState>>,

    // Editor mode
    pub editor_camera: Option<EditorCamera>,
    pub editor_command_log: Vec<(String, instant::Instant)>,
    pub editor_scene_path: Option<PathBuf>,

    // Surface config shared with scripting API
    pub shared_surface_config: Option<Rc<RefCell<wgpu::SurfaceConfiguration>>>,

    // Texture resources for GLB albedo textures
    pub texture_resources: Option<crate::mesh::TextureResources>,

    // Skeletal animation system
    pub animation_system: crate::anim_system::AnimationSystem,
    /// Per-entity bone matrix palettes computed this frame (entity -> palette).
    pub bone_palettes: HashMap<hecs::Entity, crate::anim_system::BoneMatrixPalette>,

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
            event_bus: Rc::new(RefCell::new(EventBus::new(1000))),
            audio_system: Rc::new(RefCell::new(AudioSystem::new())),
            tween_system: TweenSystem::new(),
            command_server: None,
            paused: false,
            bitmap_font: None,
            ui_renderer: None,
            entity_commands: Rc::new(RefCell::new(crate::world::EntityCommandQueue::new())),
            pool_manager: Rc::new(RefCell::new(crate::world::EntityPoolManager::new())),
            particle_system: Rc::new(RefCell::new(crate::particles::ParticleSystem::new())),
            lua_event_listeners: Rc::new(RefCell::new(HashMap::new())),
            next_lua_listener_id: Rc::new(RefCell::new(0)),
            lua_listener_id_map: Rc::new(RefCell::new(HashMap::new())),
            render_debug: crate::pipeline::RenderDebugState {
                show_hud,
                ..Default::default()
            },
            camera_shake: Rc::new(RefCell::new(CameraShakeState::new())),
            editor_camera: None,
            editor_command_log: Vec::new(),
            editor_scene_path: None,
            shared_surface_config: None,
            texture_resources: None,
            animation_system: crate::anim_system::AnimationSystem::new(),
            bone_palettes: HashMap::new(),
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

        // Initialize texture resources for GLB albedo textures
        let tex_res = crate::mesh::TextureResources::new(&gpu.device, &gpu.queue);

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
            Some(&tex_res.bind_group_layout),
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
            &gpu.queue,
            &self.project_root,
            &mut self.mesh_cache,
            &mut self.material_cache,
            &mut self.splat_cache,
            None,
            Some(&tex_res),
        );

        self.texture_resources = Some(tex_res);

        self.scene_world = Some(Rc::new(RefCell::new(scene_world)));
        self.camera_state = Some(Rc::new(RefCell::new(camera_state)));
        self.draw_pool = Some(draw_pool);
        self.forward_pipeline = Some(forward_pipeline);
        self.scene_path = Some(scene_path);

        tracing::info!("Scene loaded and forward pipeline created");

        // UI overlay: bitmap font atlas + 2D renderer
        let font = crate::font::create_bitmap_font(&gpu.device, &gpu.queue);
        let ui = UiRenderer::new(&gpu.device, gpu.config.format, &font);
        self.bitmap_font = Some(Rc::new(RefCell::new(font)));
        self.ui_renderer = Some(Rc::new(RefCell::new(ui)));

        // Register skeletal animation data from loaded meshes
        self.register_skeletons();

        // Phase 5: Initialize input system
        let bindings = crate::input::load_bindings(&self.project_root);
        self.input_state = Some(Rc::new(RefCell::new(InputState::new(bindings))));

        // Phase 5: Initialize physics world
        let gravity = if let Some(sw) = &self.scene_world {
            let sw = sw.borrow();
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
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
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
        self.physics_world = Some(Rc::new(RefCell::new(physics_world)));
        self.last_frame_time = Some(instant::Instant::now());
        tracing::info!("Physics world initialized");

        // Phase 6: Initialize scripting runtime
        let mut script_runtime = ScriptRuntime::new();
        if let Err(e) = script_runtime.register_api() {
            tracing::error!("Failed to register script API: {}", e);
        }

        // Register input API
        if let Some(input) = &self.input_state {
            if let Err(e) = script_runtime.register_input_api(input.clone()) {
                tracing::error!("Failed to register input API: {}", e);
            }
        }

        // Register physics API
        if let (Some(pw), Some(sw)) = (&self.physics_world, &self.scene_world) {
            if let Err(e) = script_runtime.register_physics_api(pw.clone(), sw.clone()) {
                tracing::error!("Failed to register physics API: {}", e);
            }
        }

        // Register entity manipulation API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_entity_api(sw.clone()) {
                tracing::error!("Failed to register entity API: {}", e);
            }
            // Entity command API (spawn, destroy, scale, visibility, pooling)
            if let Err(e) = script_runtime.register_entity_command_api(sw.clone(), self.entity_commands.clone(), self.pool_manager.clone()) {
                tracing::error!("Failed to register entity command API: {}", e);
            }
        }

        // Register UI overlay API
        if let (Some(ui), Some(font), Some(gpu)) = (
            &self.ui_renderer,
            &self.bitmap_font,
            &self.gpu,
        ) {
            let surface_config = Rc::new(RefCell::new(gpu.config.clone()));
            self.shared_surface_config = Some(surface_config.clone());
            if let Err(e) = script_runtime.register_ui_api(ui.clone(), font.clone(), surface_config) {
                tracing::error!("Failed to register UI API: {}", e);
            }
        }

        // Register camera API (world_to_screen)
        if let (Some(cs), Some(sc)) = (&self.camera_state, &self.shared_surface_config) {
            if let Err(e) = script_runtime.register_camera_api(cs.clone(), sc.clone()) {
                tracing::error!("Failed to register camera API: {}", e);
            }
        }

        // Register camera shake API
        {
            if let Err(e) = script_runtime.register_camera_shake_api(self.camera_shake.clone()) {
                tracing::error!("Failed to register camera shake API: {}", e);
            }
        }

        // Register event bus API (with Lua listener support)
        {
            if let Err(e) = script_runtime.register_event_api(self.event_bus.clone(), self.lua_event_listeners.clone(), self.next_lua_listener_id.clone(), self.lua_listener_id_map.clone()) {
                tracing::error!("Failed to register event API: {}", e);
            }
        }

        // Register audio API
        {
            if let Err(e) = script_runtime.register_audio_api(self.audio_system.clone(), self.project_root.clone()) {
                tracing::error!("Failed to register audio API: {}", e);
            }
        }

        // Register particle API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_particle_api(sw.clone(), self.particle_system.clone()) {
                tracing::error!("Failed to register particle API: {}", e);
            }
        }

        // Register animation API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_animation_api(sw.clone()) {
                tracing::error!("Failed to register animation API: {}", e);
            }
        }

        // Load scripts for entities that have them
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
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
        let uninit_entities: Vec<hecs::Entity> = if let Some(sw) = &self.scene_world {
            let sw = sw.borrow();
            let mut query = sw.world.query::<&Script>();
            query.iter()
                .filter(|(_, script)| !script.initialized)
                .map(|(entity, _)| entity)
                .collect()
        } else {
            vec![]
        };
        for entity in uninit_entities {
            script_runtime.call_init(entity);
        }
        // Mark all as initialized
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
            for (_entity, script) in sw.world.query::<&mut Script>().iter() {
                script.initialized = true;
            }
        }

        self.script_runtime = Some(script_runtime);
        tracing::info!("Script runtime initialized");

        // Phase 7: Initialize event bus schema and audio
        self.event_bus.borrow_mut().load_schema(&self.project_root);

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

    /// Initialize editor mode: load or create scene, init free camera, start command socket.
    fn init_editor_mode(&mut self) {
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        // Set window title
        let title = if let Some(scene) = &self.args.scene {
            format!("nAIVE Editor - {}", scene)
        } else {
            "nAIVE Editor".to_string()
        };
        gpu.window.set_title(&title);

        // Generate default audio files
        crate::audio_gen::generate_default_sounds(&self.project_root);

        // Create camera state and draw uniform pool
        let camera_state = CameraState::new(&gpu.device);
        let draw_pool = DrawUniformPool::new(&gpu.device);

        // Initialize texture resources for GLB albedo textures
        let tex_res = crate::mesh::TextureResources::new(&gpu.device, &gpu.queue);

        // Compile forward shader
        let forward_slang = self.project_root.join("shaders/passes/mesh_forward.slang");
        let forward_wgsl = match crate::shader::compile_mesh_forward_shader(Some(&forward_slang)) {
            Ok(wgsl) => wgsl,
            Err(e) => {
                tracing::error!("Forward shader compilation failed: {}", e);
                crate::shader::get_mesh_forward_wgsl()
            }
        };
        let forward_pipeline = crate::renderer::create_forward_pipeline(
            &gpu.device,
            &forward_wgsl,
            gpu.config.format,
            &camera_state.bind_group_layout,
            &draw_pool.bind_group_layout,
            Some(&tex_res.bind_group_layout),
        );

        // Load existing scene or create a default editor scene
        let (scene, scene_path) = if let Some(scene_arg) = &self.args.scene {
            let path = self.project_root.join(scene_arg);
            if path.exists() {
                match crate::scene::load_scene(&path) {
                    Ok(s) => (s, Some(path)),
                    Err(e) => {
                        tracing::error!("Failed to load scene: {}", e);
                        (Self::default_editor_scene(), None)
                    }
                }
            } else {
                tracing::warn!("Scene file not found: {:?}, creating default", path);
                (Self::default_editor_scene(), None)
            }
        } else {
            (Self::default_editor_scene(), None)
        };

        // Spawn entities into ECS
        let mut scene_world = SceneWorld::new();
        crate::world::spawn_all_entities(
            &mut scene_world,
            &scene,
            &gpu.device,
            &gpu.queue,
            &self.project_root,
            &mut self.mesh_cache,
            &mut self.material_cache,
            &mut self.splat_cache,
            None,
            Some(&tex_res),
        );

        self.texture_resources = Some(tex_res);

        // Store the scene for physics init
        scene_world.current_scene = Some(scene.clone());
        self.scene_world = Some(Rc::new(RefCell::new(scene_world)));
        self.camera_state = Some(Rc::new(RefCell::new(camera_state)));
        self.draw_pool = Some(draw_pool);
        self.forward_pipeline = Some(forward_pipeline);
        self.scene_path = scene_path.clone();
        self.editor_scene_path = scene_path;

        // Initialize physics world (same as normal game mode)
        let gravity = glam::Vec3::from(scene.settings.gravity);
        let mut physics_world = PhysicsWorld::new(gravity);
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
            for entity_def in &scene.entities {
                if let Some(&entity) = sw.entity_registry.get(&entity_def.id) {
                    let pos = entity_def.components.transform.as_ref()
                        .map(|t| glam::Vec3::from(t.position))
                        .unwrap_or(glam::Vec3::ZERO);
                    let rot = entity_def.components.transform.as_ref()
                        .map(|t| crate::world::euler_degrees_to_quat(t.rotation))
                        .unwrap_or(glam::Quat::IDENTITY);

                    if let Some(col_def) = &entity_def.components.collider {
                        let shape = crate::world::parse_collider_shape(col_def);
                        let is_trigger = col_def.is_trigger;
                        let restitution = col_def.restitution;
                        let friction = col_def.friction;
                        let body_type = entity_def.components.rigid_body.as_ref()
                            .map(|rb| rb.body_type.as_str())
                            .unwrap_or("static");

                        match body_type {
                            "dynamic" => {
                                let mass = entity_def.components.rigid_body.as_ref()
                                    .map(|rb| rb.mass).unwrap_or(1.0);
                                let ccd = entity_def.components.rigid_body.as_ref()
                                    .map(|rb| rb.ccd).unwrap_or(false);
                                let (rb_handle, col_handle) = physics_world
                                    .add_dynamic_body(entity, pos, rot, shape.clone(), mass, restitution, friction, ccd);
                                let _ = sw.world.insert(entity, (
                                    crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Dynamic },
                                    crate::physics::Collider { handle: col_handle, shape, is_trigger },
                                ));
                            }
                            _ => {
                                let (rb_handle, col_handle) = physics_world
                                    .add_static_body(entity, pos, rot, shape.clone(), is_trigger, restitution, friction);
                                let _ = sw.world.insert(entity, (
                                    crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Static },
                                    crate::physics::Collider { handle: col_handle, shape, is_trigger },
                                ));
                            }
                        }
                    }
                }
            }
        }
        self.physics_world = Some(Rc::new(RefCell::new(physics_world)));
        tracing::info!("Physics world initialized");

        // UI overlay (must be initialized before Lua API registration)
        let font = crate::font::create_bitmap_font(&gpu.device, &gpu.queue);
        let ui = UiRenderer::new(&gpu.device, gpu.config.format, &font);
        self.bitmap_font = Some(Rc::new(RefCell::new(font)));
        self.ui_renderer = Some(Rc::new(RefCell::new(ui)));

        // Register skeletal animation data from loaded meshes
        self.register_skeletons();

        // Input system (must be initialized before Lua API registration)
        let bindings = crate::input::load_bindings(&self.project_root);
        self.input_state = Some(Rc::new(RefCell::new(InputState::new(bindings))));

        // Initialize scripting runtime with full API suite (same as load_scene)
        let script_runtime = ScriptRuntime::new();
        if let Err(e) = script_runtime.register_api() {
            tracing::error!("Failed to register script API: {}", e);
        }

        // Register input API
        if let Some(input) = &self.input_state {
            if let Err(e) = script_runtime.register_input_api(input.clone()) {
                tracing::error!("Failed to register input API: {}", e);
            }
        }

        // Register physics API
        if let (Some(pw), Some(sw)) = (&self.physics_world, &self.scene_world) {
            if let Err(e) = script_runtime.register_physics_api(pw.clone(), sw.clone()) {
                tracing::error!("Failed to register physics API: {}", e);
            }
        }

        // Register entity manipulation API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_entity_api(sw.clone()) {
                tracing::error!("Failed to register entity API: {}", e);
            }
            // Entity command API (spawn, destroy, scale, visibility, pooling)
            if let Err(e) = script_runtime.register_entity_command_api(sw.clone(), self.entity_commands.clone(), self.pool_manager.clone()) {
                tracing::error!("Failed to register entity command API: {}", e);
            }
        }

        // Register UI overlay API
        if let (Some(ui), Some(font), Some(gpu)) = (
            &self.ui_renderer,
            &self.bitmap_font,
            &self.gpu,
        ) {
            let surface_config = Rc::new(RefCell::new(gpu.config.clone()));
            self.shared_surface_config = Some(surface_config.clone());
            if let Err(e) = script_runtime.register_ui_api(ui.clone(), font.clone(), surface_config) {
                tracing::error!("Failed to register UI API: {}", e);
            }
        }

        // Register camera API
        if let (Some(cs), Some(sc)) = (&self.camera_state, &self.shared_surface_config) {
            if let Err(e) = script_runtime.register_camera_api(cs.clone(), sc.clone()) {
                tracing::error!("Failed to register camera API: {}", e);
            }
        }

        // Register camera shake API
        {
            if let Err(e) = script_runtime.register_camera_shake_api(self.camera_shake.clone()) {
                tracing::error!("Failed to register camera shake API: {}", e);
            }
        }

        // Register event bus API
        {
            if let Err(e) = script_runtime.register_event_api(self.event_bus.clone(), self.lua_event_listeners.clone(), self.next_lua_listener_id.clone(), self.lua_listener_id_map.clone()) {
                tracing::error!("Failed to register event API: {}", e);
            }
        }

        // Register audio API
        {
            if let Err(e) = script_runtime.register_audio_api(self.audio_system.clone(), self.project_root.clone()) {
                tracing::error!("Failed to register audio API: {}", e);
            }
        }

        // Register particle API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_particle_api(sw.clone(), self.particle_system.clone()) {
                tracing::error!("Failed to register particle API: {}", e);
            }
        }

        // Register animation API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_animation_api(sw.clone()) {
                tracing::error!("Failed to register animation API: {}", e);
            }
        }

        self.script_runtime = Some(script_runtime);

        self.last_frame_time = Some(instant::Instant::now());

        // Initialize editor camera: try to extract position from scene's main camera
        let cam_pos = self.scene_world.as_ref().and_then(|sw| {
            let sw = sw.borrow();
            for (_entity, (transform, camera)) in sw.world.query::<(&Transform, &Camera)>().iter() {
                if camera.role == CameraRole::Main {
                    return Some(transform.position);
                }
            }
            None
        }).unwrap_or(glam::Vec3::new(0.0, 5.0, 10.0));

        self.editor_camera = Some(EditorCamera::new(cam_pos, 0.0, -0.3));

        // Try to load render pipeline
        self.try_load_pipeline();

        // Start command socket
        match CommandServer::start(&self.args.socket) {
            Ok(server) => {
                tracing::info!("Editor command socket: {}", server.socket_path);
                self.command_server = Some(server);
            }
            Err(e) => {
                tracing::warn!("Failed to start command server: {}", e);
            }
        }

        tracing::info!("Editor mode initialized");
    }

    /// Create a default editor scene with ground plane, lights, marker cube, and camera.
    fn default_editor_scene() -> crate::scene::SceneFile {
        use crate::scene::*;
        SceneFile {
            name: "Editor Scene".to_string(),
            settings: SceneSettings {
                ambient_light: [0.15, 0.15, 0.2],
                fog: None,
                gravity: [0.0, -9.81, 0.0],
            },
            entities: vec![
                // Ground plane (with static collider so things bounce off it)
                EntityDef {
                    id: "editor_ground".to_string(),
                    tags: vec!["ground".to_string()],
                    extends: None,
                    components: ComponentMap {
                        transform: Some(TransformDef {
                            position: [0.0, -0.5, 0.0],
                            rotation: [0.0, 0.0, 0.0],
                            scale: [20.0, 1.0, 20.0],
                        }),
                        mesh_renderer: Some(MeshRendererDef {
                            mesh: "procedural:cube".to_string(),
                            material: "procedural:default".to_string(),
                            cast_shadows: true,
                            receive_shadows: true,
                        }),
                        collider: Some(ColliderDef {
                            shape: "box".to_string(),
                            half_extents: Some([10.0, 0.5, 10.0]),
                            radius: None,
                            half_height: None,
                            is_trigger: false,
                            restitution: 0.5,
                            friction: 0.5,
                        }),
                        ..Default::default()
                    },
                },
                // Origin marker cube (so you can always see something)
                EntityDef {
                    id: "editor_marker".to_string(),
                    tags: vec!["marker".to_string()],
                    extends: None,
                    components: ComponentMap {
                        transform: Some(TransformDef {
                            position: [0.0, 0.5, 0.0],
                            rotation: [0.0, 0.0, 0.0],
                            scale: [1.0, 1.0, 1.0],
                        }),
                        mesh_renderer: Some(MeshRendererDef {
                            mesh: "procedural:cube".to_string(),
                            material: "procedural:default".to_string(),
                            cast_shadows: true,
                            receive_shadows: true,
                        }),
                        ..Default::default()
                    },
                },
                // Directional light (sun)
                EntityDef {
                    id: "editor_sun".to_string(),
                    tags: vec!["light".to_string()],
                    extends: None,
                    components: ComponentMap {
                        transform: Some(TransformDef {
                            position: [0.0, 10.0, 0.0],
                            rotation: [0.0, 0.0, 0.0],
                            scale: [1.0, 1.0, 1.0],
                        }),
                        directional_light: Some(DirectionalLightDef {
                            direction: [0.3, -1.0, 0.5],
                            color: [1.0, 0.95, 0.9],
                            intensity: 3.0,
                            shadow_extent: 30.0,
                        }),
                        ..Default::default()
                    },
                },
                // Key light (point)
                EntityDef {
                    id: "editor_key_light".to_string(),
                    tags: vec!["light".to_string()],
                    extends: None,
                    components: ComponentMap {
                        transform: Some(TransformDef {
                            position: [5.0, 8.0, 5.0],
                            rotation: [0.0, 0.0, 0.0],
                            scale: [1.0, 1.0, 1.0],
                        }),
                        point_light: Some(PointLightDef {
                            color: [1.0, 0.9, 0.8],
                            intensity: 50.0,
                            range: 30.0,
                        }),
                        ..Default::default()
                    },
                },
                // Fill light (point)
                EntityDef {
                    id: "editor_fill_light".to_string(),
                    tags: vec!["light".to_string()],
                    extends: None,
                    components: ComponentMap {
                        transform: Some(TransformDef {
                            position: [-5.0, 6.0, -3.0],
                            rotation: [0.0, 0.0, 0.0],
                            scale: [1.0, 1.0, 1.0],
                        }),
                        point_light: Some(PointLightDef {
                            color: [0.6, 0.7, 1.0],
                            intensity: 30.0,
                            range: 25.0,
                        }),
                        ..Default::default()
                    },
                },
                // Camera
                EntityDef {
                    id: "editor_camera".to_string(),
                    tags: vec![],
                    extends: None,
                    components: ComponentMap {
                        transform: Some(TransformDef {
                            position: [0.0, 3.0, 8.0],
                            rotation: [0.0, 0.0, 0.0],
                            scale: [1.0, 1.0, 1.0],
                        }),
                        camera: Some(CameraDef {
                            fov: 75.0,
                            near: 0.1,
                            far: 500.0,
                            role: "main".to_string(),
                            mode: "first_person".to_string(),
                            distance: 4.0,
                            height_offset: 1.5,
                            pitch_limits: None,
                        }),
                        ..Default::default()
                    },
                },
            ],
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
        let camera_state_rc = match &self.camera_state {
            Some(cs) => cs,
            None => return,
        };
        let camera_state = camera_state_rc.borrow();
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
                let tex_layout = self.texture_resources.as_ref().map(|tr| &tr.bind_group_layout);
                match crate::pipeline::compile_pipeline(
                    &gpu.device,
                    &pipeline_file,
                    &self.project_root,
                    &*camera_state,
                    draw_pool,
                    gpu.config.format,
                    gpu.config.width,
                    gpu.config.height,
                    tex_layout,
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

            let camera_state = self.camera_state.as_ref().unwrap().borrow();
            let draw_pool = self.draw_pool.as_ref().unwrap();

            let tex_layout = self.texture_resources.as_ref().map(|tr| &tr.bind_group_layout);
            let new_pipeline = crate::renderer::create_forward_pipeline(
                &gpu.device,
                &wgsl,
                gpu.config.format,
                &camera_state.bind_group_layout,
                &draw_pool.bind_group_layout,
                tex_layout,
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

        let scene_world = match &self.scene_world {
    Some(scene_world) => scene_world,
    None => return,
};
let mut scene_world = scene_world.borrow_mut();

        tracing::info!("Hot-reloading scene: {:?}", changed_path);

        let new_scene = match crate::scene::load_scene(changed_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Scene reload failed: {}, keeping old scene", e);
                return;
            }
        };

        crate::world::reconcile_scene(
            &mut *scene_world,
            &new_scene,
            &gpu.device,
            &gpu.queue,
            &self.project_root,
            &mut self.mesh_cache,
            &mut self.material_cache,
            &mut self.splat_cache,
            None,
            self.texture_resources.as_ref(),
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
                let scene_world_guard = scene_world.borrow();
                let reload_candidates: Vec<(hecs::Entity, std::path::PathBuf)> = scene_world_guard.world
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
    Some(input) => input,
    None => return,
};
let input = input.borrow();
        let scene_world = match &self.scene_world {
    Some(scene_world) => scene_world,
    None => return,
};
let mut scene_world = scene_world.borrow_mut();
        let physics_world = match &self.physics_world {
    Some(physics_world) => physics_world,
    None => return,
};
let mut physics_world = physics_world.borrow_mut();

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
        let scene_world = match &self.scene_world {
            Some(sw) => sw,
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
        let mut sw = scene_world.borrow_mut();
        let physics_world = physics_world.borrow();

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
            self.entity_commands.borrow_mut().destroys.push(id);
        }
    }

    /// Update projectile ages and destroy expired ones.
    fn update_projectiles(&mut self) {
        let scene_world_rc = match &self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let mut scene_world = scene_world_rc.borrow_mut();
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
                self.entity_commands.borrow_mut().destroys.push(id.clone());
            }
        }
    }

    /// Register skeletons from newly loaded meshes and attach Animator components
    /// to entities whose meshes have skin data.
    fn register_skeletons(&mut self) {
        // Take skin data from mesh cache and register with animation system
        let registered = self.animation_system.register_from_mesh_cache(&mut self.mesh_cache);
        if registered.is_empty() {
            return;
        }

        // Build mesh_index -> skeleton_handle map
        let mesh_to_skeleton: std::collections::HashMap<usize, crate::components::SkeletonHandle> =
            registered.into_iter().collect();

        let scene_world = match &self.scene_world {
    Some(scene_world) => scene_world,
    None => return,
};
let mut scene_world = scene_world.borrow_mut();

        // Find entities with MeshRenderer whose mesh has a registered skeleton
        let to_animate: Vec<(hecs::Entity, crate::components::SkeletonHandle)> = scene_world
            .world
            .query::<&crate::components::MeshRenderer>()
            .iter()
            .filter_map(|(entity, mr)| {
                mesh_to_skeleton.get(&mr.mesh_handle.0).map(|sh| (entity, *sh))
            })
            .collect();

        for (entity, skeleton_handle) in to_animate {
            // Skip if already has an Animator
            if scene_world.world.get::<&crate::components::Animator>(entity).is_ok() {
                continue;
            }
            let animator = crate::components::Animator {
                skeleton_handle,
                controller: naive_core::animation::AnimationController {
                    current_state: naive_core::animation::AnimState::Idle,
                    ..Default::default()
                },
            };
            let _ = scene_world.world.insert_one(entity, animator);
            let _ = scene_world.world.insert_one(entity, skeleton_handle);
            tracing::info!("Attached Animator to entity {:?} with skeleton {:?}", entity, skeleton_handle);
        }
    }

    /// Tick skeletal animations: advance time, compute bone matrix palettes.
    fn tick_animations(&mut self) {
        let scene_world_rc = match &self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let mut scene_world = scene_world_rc.borrow_mut();
        let dt = self.delta_time;

        // Collect entities with animators
        let animated: Vec<hecs::Entity> = scene_world
            .world
            .query::<&crate::components::Animator>()
            .iter()
            .map(|(e, _)| e)
            .collect();

        self.bone_palettes.clear();

        for entity in animated {
            if let Ok(mut animator) = scene_world.world.get::<&mut crate::components::Animator>(entity) {
                let palette = self.animation_system.tick_entity(&mut animator, dt);
                self.bone_palettes.insert(entity, palette);
            }
        }
    }

    /// Process the health system: detect deaths and fire on_death callbacks.
    fn process_health_system(&mut self) {
        let scene_world_rc = match &self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let mut scene_world = scene_world_rc.borrow_mut();
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
        let destroys: Vec<_> = self.entity_commands.borrow_mut().destroys.drain(..).collect();
        for id in destroys {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
                // Call on_destroy hook before despawning
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if let Some(script_runtime) = &self.script_runtime {
                        script_runtime.call_on_destroy(entity);
                    }
                    // Clean up physics body if entity has one
                    if let Ok(rb) = scene_world.world.get::<&crate::physics::RigidBody>(entity) {
                        let rb_handle = rb.handle;
                        drop(rb);
                        if let Some(physics_world) = &self.physics_world {
                            let mut physics_world = physics_world.borrow_mut();
                            physics_world.remove_body(rb_handle);
                        }
                    }
                }
                crate::world::destroy_runtime_entity(&mut *scene_world, &id);
            }
        }

        // Process spawns (after destroys, so destroy+spawn same ID works)
        let spawns: Vec<_> = self.entity_commands.borrow_mut().spawns.drain(..).collect();
        for cmd in spawns {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
                crate::world::spawn_runtime_entity(
                    &mut *scene_world,
                    &cmd.id,
                    &cmd.mesh,
                    &cmd.material,
                    cmd.position,
                    cmd.scale,
                    &gpu.device,
                    &gpu.queue,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                    self.texture_resources.as_ref(),
                );
            }
        }

        // Process projectile spawns
        let proj_spawns: Vec<_> = self.entity_commands.borrow_mut().projectile_spawns.drain(..).collect();
        for cmd in &proj_spawns {
            if let (Some(scene_world), Some(physics_world)) = (&self.scene_world, &self.physics_world) {
                let mut scene_world = scene_world.borrow_mut();
                let mut physics_world = physics_world.borrow_mut();
                crate::world::spawn_projectile_entity(
                    &mut *scene_world,
                    cmd,
                    &gpu.device,
                    &gpu.queue,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                    &mut *physics_world,
                    self.texture_resources.as_ref(),
                );
            }
        }

        // Process dynamic spawns (bouncing physics objects)
        let dyn_spawns: Vec<_> = self.entity_commands.borrow_mut().dynamic_spawns.drain(..).collect();
        for cmd in &dyn_spawns {
            if let (Some(scene_world), Some(physics_world)) = (&self.scene_world, &self.physics_world) {
                let mut scene_world = scene_world.borrow_mut();
                let mut physics_world = physics_world.borrow_mut();
                crate::world::spawn_dynamic_entity(
                    &mut *scene_world,
                    cmd,
                    &gpu.device,
                    &gpu.queue,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                    &mut *physics_world,
                    self.texture_resources.as_ref(),
                );
            }
        }

        // Process pool operations
        let pool_ops: Vec<_> = self.entity_commands.borrow_mut().pool_ops.drain(..).collect();
        for op in pool_ops {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
                match op {
                    crate::world::PoolOp::Release(id) => {
                        if let Some(&entity) = scene_world.entity_registry.get(&id) {
                            // Hide the entity and mark as inactive
                            let _ = scene_world.world.insert_one(entity, crate::components::Hidden);
                            if let Ok(mut pooled) = scene_world.world.get::<&mut crate::components::Pooled>(entity) {
                                pooled.active = false;
                                let pool_name = pooled.pool_name.clone();
                                drop(pooled);
                                self.pool_manager.borrow_mut().release(&pool_name, &id);
                            }
                        }
                    }
                }
            }
        }

        // Process scale updates
        let scale_updates: Vec<_> = self.entity_commands.borrow_mut().scale_updates.drain(..).collect();
        for (id, scale) in scale_updates {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                        transform.scale = glam::Vec3::from(scale);
                        transform.dirty = true;
                    }
                }
            }
        }

        // Process visibility updates
        let vis_updates: Vec<_> = self.entity_commands.borrow_mut().visibility_updates.drain(..).collect();
        for (id, visible) in vis_updates {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
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
        let scene_rel = match self.entity_commands.borrow_mut().pending_scene_load.take() {
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
            let sw = sw.borrow();
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
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
            sw.world.clear();
            sw.entity_registry.clear();
            sw.current_scene = None;
        }

        // 4. Replace physics world in-place
        let gravity = glam::Vec3::from(scene.settings.gravity);
        if let Some(pw) = &self.physics_world {
            let mut pw = pw.borrow_mut();
            *pw = PhysicsWorld::new(gravity);
        }

        // 5. Clear pool manager, particle system, lua event listeners, camera shake
        *self.pool_manager.borrow_mut() = crate::world::EntityPoolManager::new();
        *self.particle_system.borrow_mut() = crate::particles::ParticleSystem::new();
        self.lua_event_listeners.borrow_mut().clear();
        *self.next_lua_listener_id.borrow_mut() = 0;
        self.lua_listener_id_map.borrow_mut().clear();
        *self.camera_shake.borrow_mut() = CameraShakeState::new();

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        // 6. Re-spawn entities
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
            crate::world::spawn_all_entities(
                &mut *sw,
                &scene,
                &gpu.device,
                &gpu.queue,
                &self.project_root,
                &mut self.mesh_cache,
                &mut self.material_cache,
                &mut self.splat_cache,
                None,
                self.texture_resources.as_ref(),
            );
        }

        // 6b. Register skeletal animation data from loaded meshes
        self.register_skeletons();

        // 7. Spawn physics components for entities
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
            if let (Some(scene_data), Some(pw)) = (&sw.current_scene.clone(), &self.physics_world) {
                let mut pw = pw.borrow_mut();
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
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
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

        // Call init on all scripts (collect first to release world borrow before Lua runs)
        let uninit: Vec<hecs::Entity> = if let Some(sw) = &self.scene_world {
            let sw = sw.borrow();
            let mut query = sw.world.query::<&Script>();
            query.iter()
                .filter(|(_, s)| !s.initialized)
                .map(|(e, _)| e)
                .collect()
        } else {
            vec![]
        };
        if let Some(sr) = &self.script_runtime {
            for entity in uninit {
                sr.call_init(entity);
            }
        }
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
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
        let mut shake = self.camera_shake.borrow_mut();
        if shake.timer <= 0.0 {
            return glam::Vec3::ZERO;
        }
        shake.timer -= dt;
        if shake.timer < 0.0 {
            shake.timer = 0.0;
        }
        let t = if shake.duration > 0.0 {
            shake.timer / shake.duration
        } else {
            0.0
        };
        let scale = shake.intensity * t;
        // Simple pseudo-random using seed and timer
        let s = shake.seed as f32;
        let phase = shake.timer * 37.0 + s;
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
        let scene_world = scene_world.borrow();
        let camera_state_rc = match &self.camera_state {
            Some(cs) => cs,
            None => return,
        };
        let mut camera_state = camera_state_rc.borrow_mut();

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
                        let mut physics_world = physics_world.borrow_mut();
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
    /// Editor-enhanced commands (spawn with mesh, save_scene, etc.) are handled here
    /// at Engine level where we have access to GPU resources and caches.
    fn process_commands(&mut self) {
        let commands = match &self.command_server {
            Some(s) => s.poll(),
            None => return,
        };

        for pending in commands {
            let cmd = pending.request.cmd.as_str();

            // Log command for editor overlay (with detail)
            if self.args.editor_mode {
                let detail = match cmd {
                    "spawn_entity" => {
                        let eid = pending.request.params.get("entity_id")
                            .and_then(|v| v.as_str()).unwrap_or("?");
                        let mesh = pending.request.params.get("components")
                            .and_then(|c| c.get("mesh_renderer"))
                            .and_then(|m| m.get("mesh"))
                            .and_then(|v| v.as_str()).unwrap_or("");
                        if mesh.is_empty() {
                            format!("spawn {}", eid)
                        } else {
                            format!("spawn {} [{}]", eid, mesh)
                        }
                    }
                    "destroy_entity" => {
                        let eid = pending.request.params.get("entity_id")
                            .and_then(|v| v.as_str()).unwrap_or("?");
                        format!("destroy {}", eid)
                    }
                    "modify_entity" => {
                        let eid = pending.request.params.get("entity_id")
                            .and_then(|v| v.as_str()).unwrap_or("?");
                        format!("modify {}", eid)
                    }
                    "set_camera" => "set_camera".to_string(),
                    "save_scene" => {
                        let path = pending.request.params.get("path")
                            .and_then(|v| v.as_str()).unwrap_or("scenes/editor_scene.yaml");
                        format!("save -> {}", path)
                    }
                    "run_lua" => {
                        let code = pending.request.params.get("code")
                            .and_then(|v| v.as_str()).unwrap_or("");
                        let preview: String = code.chars().take(40).collect();
                        format!("lua: {}", preview)
                    }
                    other => other.to_string(),
                };
                self.editor_command_log.push((detail, instant::Instant::now()));
                // Keep only last 10
                if self.editor_command_log.len() > 10 {
                    self.editor_command_log.remove(0);
                }
            }

            let response = match cmd {
                // Enhanced spawn_entity: if it has mesh_renderer, handle at Engine level
                "spawn_entity" => {
                    let has_mesh = pending.request.params.get("components")
                        .and_then(|c| c.get("mesh_renderer"))
                        .is_some();
                    if has_mesh {
                        self.handle_spawn_with_mesh(&pending.request)
                    } else {
                        {
                            let mut sw_opt = self.scene_world.as_ref().map(|rc| rc.borrow_mut());
                            let mut eb = self.event_bus.borrow_mut();
                            let mut is_opt = self.input_state.as_ref().map(|rc| rc.borrow_mut());
                            crate::command::handle_command_rc(
                                &pending.request,
                                sw_opt.as_deref_mut(),
                                &mut *eb,
                                is_opt.as_deref_mut(),
                                &mut self.paused,
                            )
                        }
                    }
                }
                "save_scene" => self.handle_save_scene(&pending.request),
                "get_scene_yaml" => self.handle_get_scene_yaml(),
                "set_camera" => self.handle_set_camera(&pending.request),
                "editor_status" => self.handle_editor_status(),
                "run_lua" => self.handle_run_lua(&pending.request),
                _ => {
                        let mut sw_opt = self.scene_world.as_ref().map(|rc| rc.borrow_mut());
                        let mut eb = self.event_bus.borrow_mut();
                        let mut is_opt = self.input_state.as_ref().map(|rc| rc.borrow_mut());
                        crate::command::handle_command_rc(
                            &pending.request,
                            sw_opt.as_deref_mut(),
                            &mut *eb,
                            is_opt.as_deref_mut(),
                            &mut self.paused,
                        )
                    },
            };
            let _ = pending.responder.send(response);
        }
    }

    /// Handle spawn_entity with mesh_renderer component (needs GPU resources).
    fn handle_spawn_with_mesh(&mut self, req: &crate::command::CommandRequest) -> crate::command::CommandResponse {
        use crate::command::{CommandResponse};
        use serde_json::json;

        let entity_id = match req.params.get("entity_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return CommandResponse::error("Missing 'entity_id' parameter"),
        };

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return CommandResponse::error("GPU not initialized"),
        };
        let device = &gpu.device;

        let components = match req.params.get("components") {
            Some(c) => c,
            None => return CommandResponse::error("Missing 'components' parameter"),
        };

        let mr = match components.get("mesh_renderer") {
            Some(mr) => mr,
            None => return CommandResponse::error("Missing 'mesh_renderer' in components"),
        };

        let mesh = mr.get("mesh").and_then(|v| v.as_str()).unwrap_or("procedural:cube");
        let material = mr.get("material").and_then(|v| v.as_str()).unwrap_or("procedural:default");

        // Parse transform
        let mut position = [0.0f32; 3];
        let mut scale = [1.0f32; 3];
        let mut rotation = [0.0f32; 3];
        if let Some(t) = components.get("transform") {
            if let Some(arr) = t.get("position").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    position = [
                        arr[0].as_f64().unwrap_or(0.0) as f32,
                        arr[1].as_f64().unwrap_or(0.0) as f32,
                        arr[2].as_f64().unwrap_or(0.0) as f32,
                    ];
                }
            }
            if let Some(arr) = t.get("scale").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    scale = [
                        arr[0].as_f64().unwrap_or(1.0) as f32,
                        arr[1].as_f64().unwrap_or(1.0) as f32,
                        arr[2].as_f64().unwrap_or(1.0) as f32,
                    ];
                }
            }
            if let Some(arr) = t.get("rotation").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    rotation = [
                        arr[0].as_f64().unwrap_or(0.0) as f32,
                        arr[1].as_f64().unwrap_or(0.0) as f32,
                        arr[2].as_f64().unwrap_or(0.0) as f32,
                    ];
                }
            }
        }

        // Parse tags
        let tags = req.params.get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
            .unwrap_or_default();

        // Spawn entity, apply rotation/tags (scoped to release scene_world borrow)
        {
            let scene_world = match &self.scene_world {
                Some(sw) => sw,
                None => return CommandResponse::error("No scene loaded"),
            };
            let mut scene_world = scene_world.borrow_mut();

            if scene_world.entity_registry.contains_key(&entity_id) {
                return CommandResponse::error(format!("Entity '{}' already exists", entity_id));
            }

            // Use spawn_runtime_entity for mesh entities
            let ok = crate::world::spawn_runtime_entity(
                &mut *scene_world,
                &entity_id,
                mesh,
                material,
                position,
                scale,
                device,
                &gpu.queue,
                &self.project_root,
                &mut self.mesh_cache,
                &mut self.material_cache,
                self.texture_resources.as_ref(),
            );

            if !ok {
                return CommandResponse::error(format!("Failed to spawn entity '{}'", entity_id));
            }

            // Apply rotation if non-zero
            if rotation != [0.0, 0.0, 0.0] {
                if let Some(&entity) = scene_world.entity_registry.get(&entity_id) {
                    if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                        transform.rotation = crate::world::euler_degrees_to_quat(rotation);
                        transform.dirty = true;
                    }
                }
            }

            // Apply tags
            if !tags.is_empty() {
                if let Some(&entity) = scene_world.entity_registry.get(&entity_id) {
                    if let Ok(mut entity_tags) = scene_world.world.get::<&mut crate::components::Tags>(entity) {
                        entity_tags.0 = tags;
                    }
                }
            }
        } // scene_world borrow released here

        // Add physics body if rigid_body/collider components are specified
        if let Some(collider_json) = components.get("collider") {
            // Parse collider shape from JSON
            let shape_str = collider_json.get("shape").and_then(|v| v.as_str()).unwrap_or("box");
            let shape = match shape_str {
                "sphere" => crate::physics::PhysicsShape::Sphere {
                    radius: collider_json.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                },
                "capsule" => crate::physics::PhysicsShape::Capsule {
                    half_height: collider_json.get("half_height").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                    radius: collider_json.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32,
                },
                _ => {
                    let he = if let Some(arr) = collider_json.get("half_extents").and_then(|v| v.as_array()) {
                        if arr.len() == 3 {
                            glam::Vec3::new(
                                arr[0].as_f64().unwrap_or(0.5) as f32,
                                arr[1].as_f64().unwrap_or(0.5) as f32,
                                arr[2].as_f64().unwrap_or(0.5) as f32,
                            )
                        } else {
                            glam::Vec3::splat(0.5)
                        }
                    } else {
                        glam::Vec3::new(scale[0] * 0.5, scale[1] * 0.5, scale[2] * 0.5)
                    };
                    crate::physics::PhysicsShape::Box { half_extents: he }
                }
            };

            let is_trigger = collider_json.get("is_trigger").and_then(|v| v.as_bool()).unwrap_or(false);
            let restitution = collider_json.get("restitution").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let friction = collider_json.get("friction").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;

            let rb_json = components.get("rigid_body");
            let body_type = rb_json.and_then(|rb| rb.get("type")).and_then(|v| v.as_str()).unwrap_or("static");

            let pos = glam::Vec3::from(position);
            let rot = crate::world::euler_degrees_to_quat(rotation);

            if let (Some(pw), Some(sw)) = (&self.physics_world, &self.scene_world) {
                let mut pw = pw.borrow_mut();
                let mut sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&entity_id) {
                    match body_type {
                        "dynamic" => {
                            let mass = rb_json.and_then(|rb| rb.get("mass")).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                            let ccd = rb_json.and_then(|rb| rb.get("ccd")).and_then(|v| v.as_bool()).unwrap_or(false);
                            let (rb_handle, col_handle) = pw
                                .add_dynamic_body(entity, pos, rot, shape.clone(), mass, restitution, friction, ccd);
                            let _ = sw.world.insert(entity, (
                                crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Dynamic },
                                crate::physics::Collider { handle: col_handle, shape, is_trigger },
                            ));
                        }
                        _ => {
                            let (rb_handle, col_handle) = pw
                                .add_static_body(entity, pos, rot, shape.clone(), is_trigger, restitution, friction);
                            let _ = sw.world.insert(entity, (
                                crate::physics::RigidBody { handle: rb_handle, body_type: crate::physics::PhysicsBodyType::Static },
                                crate::physics::Collider { handle: col_handle, shape, is_trigger },
                            ));
                        }
                    }
                }
            }
        }

        CommandResponse::ok(json!({"entity_id": entity_id}))
    }

    /// Handle save_scene: serialize current ECS state to YAML file.
    fn handle_save_scene(&self, req: &crate::command::CommandRequest) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let path = req.params.get("path").and_then(|v| v.as_str())
            .unwrap_or("scenes/editor_scene.yaml");
        let full_path = self.project_root.join(path);

        let yaml = match self.serialize_scene_to_yaml() {
            Some(y) => y,
            None => return CommandResponse::error("No scene to save"),
        };

        // Ensure parent directory exists
        if let Some(parent) = full_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match std::fs::write(&full_path, &yaml) {
            Ok(()) => {
                tracing::info!("Scene saved to {:?}", full_path);
                CommandResponse::ok(json!({"path": full_path.display().to_string(), "bytes": yaml.len()}))
            }
            Err(e) => CommandResponse::error(format!("Failed to write scene: {}", e)),
        }
    }

    /// Handle get_scene_yaml: return current scene as YAML string.
    fn handle_get_scene_yaml(&self) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        match self.serialize_scene_to_yaml() {
            Some(yaml) => CommandResponse::ok(json!({"yaml": yaml})),
            None => CommandResponse::error("No scene loaded"),
        }
    }

    /// Handle set_camera: update editor camera position/rotation.
    fn handle_set_camera(&mut self, req: &crate::command::CommandRequest) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let editor_cam = match &mut self.editor_camera {
            Some(c) => c,
            None => return CommandResponse::error("Not in editor mode"),
        };

        if let Some(arr) = req.params.get("position").and_then(|v| v.as_array()) {
            if arr.len() == 3 {
                editor_cam.position = glam::Vec3::new(
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[1].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                );
            }
        }
        if let Some(yaw) = req.params.get("yaw").and_then(|v| v.as_f64()) {
            editor_cam.yaw = (yaw as f32).to_radians();
        }
        if let Some(pitch) = req.params.get("pitch").and_then(|v| v.as_f64()) {
            editor_cam.pitch = (pitch as f32).to_radians();
        }
        if let Some(arr) = req.params.get("look_at").and_then(|v| v.as_array()) {
            if arr.len() == 3 {
                let target = glam::Vec3::new(
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[1].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                );
                let dir = (target - editor_cam.position).normalize_or_zero();
                editor_cam.yaw = (-dir.x).atan2(-dir.z);
                editor_cam.pitch = dir.y.asin();
            }
        }

        CommandResponse::ok(json!({
            "position": [editor_cam.position.x, editor_cam.position.y, editor_cam.position.z],
            "yaw": editor_cam.yaw.to_degrees(),
            "pitch": editor_cam.pitch.to_degrees(),
        }))
    }

    /// Handle editor_status: return editor mode info.
    fn handle_editor_status(&self) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let entity_count = self.scene_world.as_ref()
            .map(|sw| sw.borrow().entity_registry.len())
            .unwrap_or(0);

        let camera_info = self.editor_camera.as_ref().map(|c| {
            json!({
                "position": [c.position.x, c.position.y, c.position.z],
                "yaw": c.yaw.to_degrees(),
                "pitch": c.pitch.to_degrees(),
                "speed": c.speed,
            })
        });

        let scene_path = self.editor_scene_path.as_ref()
            .map(|p| p.display().to_string());

        CommandResponse::ok(json!({
            "editor_mode": self.args.editor_mode,
            "entity_count": entity_count,
            "scene_path": scene_path,
            "camera": camera_info,
        }))
    }

    /// Handle run_lua: execute arbitrary Lua code with access to all registered APIs.
    fn handle_run_lua(&mut self, req: &crate::command::CommandRequest) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let code = match req.params.get("code").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c,
            _ => return CommandResponse::error("Missing or empty 'code' parameter"),
        };

        let script_runtime = match &self.script_runtime {
            Some(sr) => sr,
            None => return CommandResponse::error("Script runtime not initialized"),
        };

        let lua = &script_runtime.lua;

        // Create a sandboxed environment with __index fallback to globals
        // This gives access to all registered API tables (entity, physics, particles, etc.)
        let result: Result<mlua::Value, mlua::Error> = (|| {
            let env = lua.create_table()?;
            let meta = lua.create_table()?;
            meta.set("__index", lua.globals())?;
            env.set_metatable(Some(meta));

            // Capture print output
            let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let output_clone = output.clone();
            let print_fn = lua.create_function(move |_, args: mlua::MultiValue| {
                let parts: Vec<String> = args.iter().map(|v| format!("{:?}", v)).collect();
                let line = parts.join("\t");
                if let Ok(mut out) = output_clone.lock() {
                    out.push(line);
                }
                Ok(())
            })?;
            env.set("print", print_fn)?;

            let value = lua.load(code)
                .set_name("run_lua")
                .set_environment(env)
                .eval::<mlua::Value>()?;

            // Format return value
            let result_str = match &value {
                mlua::Value::Nil => "nil".to_string(),
                mlua::Value::Boolean(b) => b.to_string(),
                mlua::Value::Integer(i) => i.to_string(),
                mlua::Value::Number(n) => n.to_string(),
                mlua::Value::String(s) => s.to_string_lossy(),
                other => format!("{:?}", other),
            };

            let print_output = output.lock().map(|o| o.clone()).unwrap_or_default();

            Ok(mlua::Value::String(lua.create_string(&json!({
                "result": result_str,
                "print_output": print_output,
            }).to_string())?))
        })();

        match result {
            Ok(val) => {
                // Parse back the JSON we stuffed in
                if let mlua::Value::String(s) = val {
                    let s = s.to_string_lossy();
                    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap_or(json!({"result": s}));
                    CommandResponse::ok(parsed)
                } else {
                    CommandResponse::ok(json!({"result": "ok"}))
                }
            }
            Err(e) => CommandResponse::error(format!("Lua error: {}", e)),
        }
    }

    /// Serialize the current ECS scene state to YAML.
    fn serialize_scene_to_yaml(&self) -> Option<String> {
        use crate::components::*;
        use crate::scene::*;

        let scene_world = self.scene_world.as_ref()?;
        let scene_world = scene_world.borrow();

        let scene_name = scene_world.current_scene.as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Editor Scene".to_string());

        let settings = scene_world.current_scene.as_ref()
            .map(|s| s.settings.clone())
            .unwrap_or_default();

        let mut entities = Vec::new();

        for (id, &entity) in &scene_world.entity_registry {
            let mut components = ComponentMap::default();

            // Transform
            if let Ok(t) = scene_world.world.get::<&Transform>(entity) {
                let (yaw, pitch, roll) = t.rotation.to_euler(glam::EulerRot::YXZ);
                components.transform = Some(TransformDef {
                    position: t.position.to_array(),
                    rotation: [pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees()],
                    scale: t.scale.to_array(),
                });
            }

            // MeshRenderer
            if let Ok(mr) = scene_world.world.get::<&MeshRenderer>(entity) {
                let mesh_name = self.mesh_cache.name_for_handle(mr.mesh_handle)
                    .unwrap_or_else(|| format!("mesh:{}", mr.mesh_handle.0));
                let material_name = self.material_cache.name_for_handle(mr.material_handle)
                    .unwrap_or_else(|| format!("material:{}", mr.material_handle.0));
                components.mesh_renderer = Some(MeshRendererDef {
                    mesh: mesh_name,
                    material: material_name,
                    cast_shadows: true,
                    receive_shadows: true,
                });
            }

            // Camera
            if let Ok(c) = scene_world.world.get::<&Camera>(entity) {
                let role_str = match &c.role {
                    CameraRole::Main => "main".to_string(),
                    CameraRole::Other(s) => s.clone(),
                };
                components.camera = Some(CameraDef {
                    fov: c.fov_degrees,
                    near: c.near,
                    far: c.far,
                    role: role_str,
                    mode: "first_person".to_string(),
                    distance: 4.0,
                    height_offset: 1.5,
                    pitch_limits: None,
                });
            }

            // PointLight
            if let Ok(pl) = scene_world.world.get::<&PointLight>(entity) {
                components.point_light = Some(PointLightDef {
                    color: pl.color.to_array(),
                    intensity: pl.intensity,
                    range: pl.range,
                });
            }

            // DirectionalLight
            if let Ok(dl) = scene_world.world.get::<&DirectionalLight>(entity) {
                components.directional_light = Some(DirectionalLightDef {
                    direction: dl.direction.to_array(),
                    color: dl.color.to_array(),
                    intensity: dl.intensity,
                    shadow_extent: dl.shadow_extent,
                });
            }

            // Tags
            let tags = scene_world.world.get::<&Tags>(entity)
                .map(|t| t.0.clone())
                .unwrap_or_default();

            entities.push(EntityDef {
                id: id.clone(),
                tags,
                extends: None,
                components,
            });
        }

        let scene_file = SceneFile {
            name: scene_name,
            settings,
            entities,
        };

        serde_yaml::to_string(&scene_file).ok()
    }

    /// Update the editor camera and apply to CameraState.
    fn update_editor_camera(&mut self) {
        // Compute mouse delta from cursor position snapshot (event-order independent)
        if let Some(input) = &self.input_state {
            let mut input = input.borrow_mut();
            input.compute_cursor_delta();
        }

        let input = match &self.input_state {
    Some(input) => input,
    None => return,
};
let input = input.borrow();

        if let Some(editor_cam) = &mut self.editor_camera {
            editor_cam.update(&*input, self.delta_time);
        }

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        if let (Some(editor_cam), Some(camera_state)) = (&self.editor_camera, &self.camera_state) {
            let mut camera_state = camera_state.borrow_mut();
            editor_cam.apply_to_camera_state(&mut *camera_state, &gpu.queue, gpu.config.width, gpu.config.height);
        }
    }

    /// Render a full editor frame: 3D scene + overlay.
    /// Draw editor status overlay.
    fn draw_editor_overlay(&mut self) {
        let (Some(ui_rc), Some(font_rc), Some(gpu)) = (&self.ui_renderer, &self.bitmap_font, &self.gpu) else {
            return;
        };
        let mut ui = ui_rc.borrow_mut();
        let font_ref = font_rc.borrow();
        let font: &BitmapFont = &*font_ref;

        let sz = 16.0;
        let x = 10.0;
        let green = [0.3, 1.0, 0.3, 1.0];
        let white = [1.0, 1.0, 1.0, 0.9];
        let dim = [0.7, 0.7, 0.7, 0.7];

        // Background strip
        ui.draw_rect(0.0, 0.0, gpu.config.width as f32, 30.0, [0.0, 0.0, 0.0, 0.6]);
        ui.draw_text(x, 7.0, "EDITOR MODE", sz, green, font);

        // Entity count
        let entity_count = self.scene_world.as_ref()
            .map(|sw| sw.borrow().entity_registry.len())
            .unwrap_or(0);
        ui.draw_text(180.0, 7.0, &format!("Entities: {}", entity_count), sz, white, font);

        // Camera position
        if let Some(cam) = &self.editor_camera {
            let pos_text = format!("Cam: ({:.1}, {:.1}, {:.1})  Speed: {:.1}",
                cam.position.x, cam.position.y, cam.position.z, cam.speed);
            ui.draw_text(380.0, 7.0, &pos_text, sz, dim, font);
        }

        // Recent commands (bottom-left, with background panel)
        let now = instant::Instant::now();
        let h = gpu.config.height as f32;
        let visible_cmds: Vec<_> = self.editor_command_log.iter().rev()
            .filter(|(_, ts)| now.duration_since(*ts).as_secs_f32() < 10.0)
            .take(8)
            .collect();
        if !visible_cmds.is_empty() {
            let panel_h = visible_cmds.len() as f32 * 18.0 + 28.0;
            let panel_y = h - panel_h - 4.0;
            ui.draw_rect(4.0, panel_y, 450.0, panel_h, [0.0, 0.0, 0.0, 0.5]);
            ui.draw_text(x, panel_y + 4.0, "Commands:", 14.0, dim, font);
            let mut y = panel_y + 22.0;
            for &(cmd_text, timestamp) in visible_cmds.iter().rev() {
                let age = now.duration_since(*timestamp).as_secs_f32();
                let alpha = if age > 7.0 { 1.0 - (age - 7.0) / 3.0 } else { 1.0 };
                let color = [0.5, 0.9, 1.0, alpha.clamp(0.0, 1.0)];
                ui.draw_text(x + 4.0, y, &format!("> {}", cmd_text), 14.0, color, font);
                y += 18.0;
            }
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

        if self.args.editor_mode {
            self.init_editor_mode();
        } else {
            // Phase 2: load scene if --scene was provided
            self.load_scene();
        }

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
        if let Some(input) = &self.input_state {
            let mut input = input.borrow_mut();
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

                // ── Editor mode: update free camera (runs full loop below) ─
                if self.args.editor_mode {
                    self.update_editor_camera();
                }

                // Handle Escape to toggle cursor capture (skip in editor mode)
                if !self.args.editor_mode {
                    if let Some(input) = &self.input_state {
                        let mut input = input.borrow_mut();
                        if input.key_held(KeyCode::Escape) {
                            if let Some(gpu) = &self.gpu {
                                let _ = gpu.window.set_cursor_grab(winit::window::CursorGrabMode::None);
                                gpu.window.set_cursor_visible(true);
                            }
                            if let Some(input) = &self.input_state {
                                let mut input = input.borrow_mut();
                                input.cursor_captured = false;
                            }
                        }
                    }

                    // Handle mouse click or any movement key to capture cursor
                    if let Some(input) = &self.input_state {
                        let mut input = input.borrow_mut();
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
                                if let Some(input) = &self.input_state {
                                    let mut input = input.borrow_mut();
                                    input.cursor_captured = true;
                                }
                            }
                        }
                    }
                }

                // Render debug toggles: 0 always toggles HUD, 1-6 only when HUD is visible
                if let Some(input) = &self.input_state {
                    let mut input = input.borrow_mut();
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
                        // Phase 5: FPS controller update (skip in editor mode — uses free camera)
                        if !self.args.editor_mode {
                            if self.input_state.as_ref().map(|i| i.borrow().cursor_captured).unwrap_or(false) {
                                self.update_fps_controller();
                            }
                        }

                        // Always step physics (gravity, collisions, etc.)
                        if let (Some(scene_world), Some(physics_world)) =
                            (&self.scene_world, &self.physics_world)
                        {
                            let mut pw = physics_world.borrow_mut();
                            pw.step(self.delta_time);
                            let mut sw = scene_world.borrow_mut();
                            pw.sync_to_ecs(&mut sw.world);
                        }

                        // Dispatch collision events to scripts
                        if let (Some(scene_world), Some(physics_world), Some(script_runtime)) =
                            (&self.scene_world, &self.physics_world, &self.script_runtime)
                        {
                            let sw = scene_world.borrow();
                            let pw = physics_world.borrow();
                            // Build reverse lookup: entity -> string id
                            let entity_to_id: HashMap<hecs::Entity, &str> = sw
                                .entity_registry
                                .iter()
                                .map(|(id, &e)| (e, id.as_str()))
                                .collect();

                            for event in &pw.collision_events {
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
                            let scripted: Vec<hecs::Entity> = {
                                let sw = scene_world.borrow();
                                let mut query = sw.world.query::<&Script>();
                                query.iter().map(|(e, _)| e).collect()
                            };
                            for entity in scripted {
                                script_runtime.call_update(entity, dt);
                            }
                        }

                        // Tick skeletal animations
                        self.tick_animations();

                        // Process deferred entity commands from Lua
                        self.process_entity_commands();

                        // Process deferred scene load (must be after entity commands)
                        self.process_pending_scene_load();

                        // Tier 2: Dispatch Lua event listeners
                        self.event_bus.borrow_mut().tick(dt as f64);
                        let flushed_events = self.event_bus.borrow_mut().flush();
                        if let Some(script_runtime) = &self.script_runtime {
                            for event in &flushed_events {
                                if let Some(listeners) = self.lua_event_listeners.borrow().get(&event.event_type) {
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
                        self.audio_system.borrow_mut().cleanup();

                        // Tier 2: Update particle system
                        if let Some(scene_world) = &self.scene_world {
                            let scene_world = scene_world.borrow();
                            self.particle_system.borrow_mut().update(dt, &*scene_world);
                        }

                        // Update listener position for spatial audio
                        if let Some(scene_world) = &self.scene_world {
                            let scene_world = scene_world.borrow();
                            for (_entity, (transform, _player)) in
                                scene_world.world.query::<(&Transform, &Player)>().iter()
                            {
                                self.audio_system.borrow_mut().set_listener_position(transform.position);
                                break;
                            }
                        }
                    }

                    // Update transforms and camera
                    {
                        let mut sw = self.scene_world.as_ref().unwrap().borrow_mut();
                        crate::transform::update_transforms(&mut sw.world);
                    }
                    // Editor mode: camera already updated above via update_editor_camera()
                    if !self.args.editor_mode {
                        self.update_camera();
                    }

                    // Tier 2: Grow GPU draw buffer if needed
                    if let (Some(gpu), Some(scene_world), Some(draw_pool)) =
                        (&self.gpu, &self.scene_world, &mut self.draw_pool)
                    {
                        let sw = scene_world.borrow();
                        let mut visible_count = 0u32;
                        for (entity, _) in sw.world.query::<&crate::components::MeshRenderer>().iter() {
                            if sw.world.get::<&crate::components::Hidden>(entity).is_ok() {
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
                        let cs = camera_state.borrow();
                        let view_matrix = cs.view_matrix();
                        let sw = scene_world.borrow();
                        for (_entity, splat) in
                            sw.world.query::<&GaussianSplat>().iter()
                        {
                            self.splat_cache.sort_splats(
                                splat.splat_handle,
                                &view_matrix,
                                &gpu.queue,
                            );
                        }
                    }

                    // Queue editor overlay draw commands (before gpu borrow)
                    if self.args.editor_mode {
                        self.draw_editor_overlay();
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
                                if let Some(input) = &self.input_state {
                                    let mut input = input.borrow_mut();
                                    input.begin_frame();
                                }
                                return;
                            }
                            Err(e) => {
                                tracing::error!("Surface error: {:?}", e);
                                if let Some(gpu) = &self.gpu {
                                    gpu.window.request_redraw();
                                }
                                if let Some(input) = &self.input_state {
                                    let mut input = input.borrow_mut();
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
                                let sw = scene_world.borrow();
                                let cs = camera_state.borrow();
                                let encoder = crate::pipeline::execute_pipeline_to_view(
                                    gpu,
                                    compiled,
                                    &*sw,
                                    &*cs,
                                    draw_pool,
                                    &self.mesh_cache,
                                    &self.material_cache,
                                    &self.splat_cache,
                                    &swapchain_view,
                                    &self.render_debug,
                                    self.texture_resources.as_ref(),
                                    &self.bone_palettes,
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
                            let sw = scene_world.borrow();
                            let cs = camera_state.borrow();
                            crate::renderer::render_scene_to_view(
                                gpu,
                                &*sw,
                                &*cs,
                                draw_pool,
                                &self.mesh_cache,
                                &self.material_cache,
                                forward_pipeline,
                                &swapchain_view,
                                &mut encoder,
                                self.texture_resources.as_ref(),
                            );
                            gpu.queue.submit(std::iter::once(encoder.finish()));
                        }

                        // UI overlay pass (drawn on top of 3D scene)
                        if let (Some(ui_rc), Some(font_rc)) = (
                            &self.ui_renderer,
                            &self.bitmap_font,
                        ) {
                            let mut ui = ui_rc.borrow_mut();
                            let font_guard = font_rc.borrow();
                            let font: &BitmapFont = &*font_guard;
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
                if let Some(input) = &self.input_state {
                    let mut input = input.borrow_mut();
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
        if let Some(input) = &self.input_state {
            let mut input = input.borrow_mut();
            input.handle_device_event(&event);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }
}
