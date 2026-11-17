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
use crate::components::{
    Camera, CameraMode, CameraRole, GaussianSplat, MeshRenderer, Player, Transform,
};
use crate::editor_camera::EditorCamera;
use crate::events::EventBus;
use crate::font::BitmapFont;
use crate::input::{InputState, Key};
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::{CharacterController, PhysicsWorld};
use crate::pipeline::CompiledPipeline;
use crate::renderer::{DrawUniformPool, GpuState};
use crate::scripting::{CameraShakeState, Script, ScriptRuntime};
use crate::splat::SplatCache;
use crate::tween::TweenSystem;
use crate::ui::UiRenderer;
use crate::watcher::WatchEvent;
use crate::world::SceneWorld;

mod commands;
mod winit_input;

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
    last_frame_time: Option<std::time::Instant>,
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

    // Tier 4: GPU swarms (members simulated and drawn on the GPU), and the
    // requests and reports passing between them and scripts
    pub swarm_system: crate::swarm::SwarmSystem,
    pub swarm_bus: crate::swarm_api::SharedSwarmBus,

    // GPU time per part of the frame, where the device has timestamp queries
    pub gpu_timer: Option<crate::gpu_timer::GpuTimer>,

    // Tier 2: Lua event listeners
    pub lua_event_listeners: Rc<RefCell<HashMap<String, Vec<mlua::RegistryKey>>>>,
    pub next_lua_listener_id: Rc<RefCell<u64>>,
    pub lua_listener_id_map: Rc<RefCell<HashMap<u64, (String, usize)>>>,

    // Render debug: interactive pass toggles (number keys)
    pub render_debug: crate::pipeline::RenderDebugState,

    // Debug wireframe renderer for collider visualization
    pub debug_draw: Option<crate::debug_draw::DebugDrawRenderer>,

    // Hot-reload notifications (message, timestamp, color)
    pub reload_notifications: Vec<(String, std::time::Instant, [f32; 4])>,

    // Camera shake state
    pub camera_shake: Rc<RefCell<CameraShakeState>>,

    // Editor mode
    pub editor_camera: Option<EditorCamera>,
    pub editor_command_log: Vec<(String, std::time::Instant)>,
    /// Scene snapshots taken before each undoable command, newest last.
    pub(crate) undo_stack: Vec<commands::UndoEntry>,
    pub(crate) test_jobs: crate::test_execution::TestJobs,
    pub editor_scene_path: Option<PathBuf>,

    // Surface config shared with scripting API
    pub shared_surface_config: Option<Rc<RefCell<wgpu::SurfaceConfiguration>>>,

    // Texture resources for GLB albedo textures
    pub texture_resources: Option<crate::mesh::TextureResources>,

    // Texture cache for material-level textures (albedo_map, normal_map)
    pub texture_cache: crate::texture_cache::TextureCache,

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
            swarm_system: crate::swarm::SwarmSystem::new(),
            swarm_bus: Rc::new(RefCell::new(crate::swarm_api::SwarmBus::new())),
            gpu_timer: None,
            lua_event_listeners: Rc::new(RefCell::new(HashMap::new())),
            next_lua_listener_id: Rc::new(RefCell::new(0)),
            lua_listener_id_map: Rc::new(RefCell::new(HashMap::new())),
            render_debug: crate::pipeline::RenderDebugState {
                show_hud,
                ..Default::default()
            },
            debug_draw: None,
            reload_notifications: Vec::new(),
            camera_shake: Rc::new(RefCell::new(CameraShakeState::new())),
            editor_camera: None,
            editor_command_log: Vec::new(),
            undo_stack: Vec::new(),
            test_jobs: crate::test_execution::TestJobs::default(),
            editor_scene_path: None,
            shared_surface_config: None,
            texture_resources: None,
            texture_cache: crate::texture_cache::TextureCache::new(),
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

    /// Register every Lua API table on `script_runtime`. `load_scene` and
    /// `init_editor_mode` both call this, so game and editor scripts see the
    /// same API.
    fn register_script_apis(&mut self, script_runtime: &ScriptRuntime) {
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
        if let (Some(sw), Some(pw)) = (&self.scene_world, &self.physics_world) {
            if let Err(e) = script_runtime.register_entity_api(sw.clone(), pw.clone()) {
                tracing::error!("Failed to register entity API: {}", e);
            }
            // Entity command API (spawn, destroy, scale, visibility, pooling)
            if let Err(e) = script_runtime.register_entity_command_api(
                sw.clone(),
                self.entity_commands.clone(),
                self.pool_manager.clone(),
            ) {
                tracing::error!("Failed to register entity command API: {}", e);
            }
        }

        // Register UI overlay API
        if let (Some(ui), Some(font), Some(gpu)) = (&self.ui_renderer, &self.bitmap_font, &self.gpu)
        {
            let surface_config = Rc::new(RefCell::new(gpu.config.clone()));
            self.shared_surface_config = Some(surface_config.clone());
            if let Err(e) = script_runtime.register_ui_api(ui.clone(), font.clone(), surface_config)
            {
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
            if let Err(e) = script_runtime.register_event_api(
                self.event_bus.clone(),
                self.lua_event_listeners.clone(),
                self.next_lua_listener_id.clone(),
                self.lua_listener_id_map.clone(),
            ) {
                tracing::error!("Failed to register event API: {}", e);
            }
        }

        // Register audio API
        {
            if let Err(e) = script_runtime
                .register_audio_api(self.audio_system.clone(), self.project_root.clone())
            {
                tracing::error!("Failed to register audio API: {}", e);
            }
        }

        // Register particle API
        if let Some(sw) = &self.scene_world {
            if let Err(e) =
                script_runtime.register_particle_api(sw.clone(), self.particle_system.clone())
            {
                tracing::error!("Failed to register particle API: {}", e);
            }
        }

        // Register animation API
        if let Some(sw) = &self.scene_world {
            if let Err(e) = script_runtime.register_animation_api(sw.clone()) {
                tracing::error!("Failed to register animation API: {}", e);
            }
            if let Err(e) = script_runtime.register_mesh_api(self.entity_commands.clone()) {
                tracing::error!("Failed to register mesh API: {}", e);
            }
        }

        // Register swarm API. A new script runtime means a new scene, so
        // swarms start over.
        *self.swarm_bus.borrow_mut() = crate::swarm_api::SwarmBus::new();
        self.swarm_system.clear();
        if let Some(sw) = &self.scene_world {
            if let Err(e) = crate::swarm_api::register_swarm_api(
                &script_runtime.lua,
                self.swarm_bus.clone(),
                sw.clone(),
            ) {
                tracing::error!("Failed to register swarm API: {}", e);
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
        let scene_path = match crate::project_path::resolve(&self.project_root, &scene_arg) {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("Invalid scene path: {}", e);
                return;
            }
        };
        if !scene_path.exists() {
            tracing::error!("Scene file not found: {:?}", scene_path);
            return;
        }

        // Create camera state and draw uniform pool
        let camera_state = CameraState::new(&gpu.device);
        let draw_pool = DrawUniformPool::new(&gpu.device);

        // Debug wireframe renderer for collider visualization
        self.debug_draw = Some(crate::debug_draw::DebugDrawRenderer::new(
            &gpu.device,
            &camera_state.bind_group_layout,
            gpu.config.format,
        ));

        // Initialize texture resources for GLB albedo textures
        let tex_res = crate::mesh::TextureResources::new(&gpu.device, &gpu.queue);

        // Compile the forward shader (Phase 2 fallback pipeline)
        let forward_slang = self.project_root.join("shaders/passes/mesh_forward.slang");
        let forward_wgsl = match crate::shader::compile_mesh_forward_shader(Some(&forward_slang)) {
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
            Some(&mut self.texture_cache),
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
                            let _ = sw
                                .world
                                .insert(entity, (rb_comp, col_comp, cc_comp, player));
                        } else if let Some(col_def) = &entity_def.components.collider {
                            let mut shape = crate::world::parse_collider_shape(col_def);
                            let is_trigger = col_def.is_trigger;
                            let restitution = col_def.restitution;
                            let friction = col_def.friction;
                            let body_type = entity_def
                                .components
                                .rigid_body
                                .as_ref()
                                .map(|rb| rb.body_type.as_str())
                                .unwrap_or("static");
                            // Resolve trimesh: look up mesh data from cache
                            if matches!(shape, crate::physics::PhysicsShape::Trimesh { .. }) {
                                let scale = entity_def
                                    .components
                                    .transform
                                    .as_ref()
                                    .map(|t| glam::Vec3::from(t.scale))
                                    .unwrap_or(glam::Vec3::ONE);
                                if let Ok(mr) =
                                    sw.world.get::<&crate::components::MeshRenderer>(entity)
                                {
                                    if let Some((verts, idxs)) =
                                        self.mesh_cache.get_physics_trimesh(mr.mesh_handle, scale)
                                    {
                                        shape = crate::physics::PhysicsShape::Trimesh {
                                            vertices: verts,
                                            indices: idxs,
                                        };
                                    } else {
                                        tracing::warn!("Trimesh collider for '{}': no mesh data, falling back to box", entity_def.id);
                                        let he = col_def.half_extents.unwrap_or([0.5, 0.5, 0.5]);
                                        shape = crate::physics::PhysicsShape::Box {
                                            half_extents: glam::Vec3::from(he),
                                        };
                                    }
                                }
                            }

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
                                        .add_dynamic_body_with_sensor(
                                            entity,
                                            pos,
                                            rot,
                                            shape.clone(),
                                            mass,
                                            restitution,
                                            friction,
                                            ccd,
                                            is_trigger,
                                        );
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
                                "kinematic" => {
                                    let (rb_handle, col_handle) = physics_world.add_kinematic_body(
                                        entity,
                                        pos,
                                        rot,
                                        shape.clone(),
                                        is_trigger,
                                        restitution,
                                        friction,
                                    );
                                    let rb_comp = crate::physics::RigidBody {
                                        handle: rb_handle,
                                        body_type: crate::physics::PhysicsBodyType::Kinematic,
                                    };
                                    let col_comp = crate::physics::Collider {
                                        handle: col_handle,
                                        shape,
                                        is_trigger,
                                    };
                                    let _ = sw.world.insert(entity, (rb_comp, col_comp));
                                }
                                _ => {
                                    let (rb_handle, col_handle) = physics_world.add_static_body(
                                        entity,
                                        pos,
                                        rot,
                                        shape.clone(),
                                        is_trigger,
                                        restitution,
                                        friction,
                                    );
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
        self.last_frame_time = Some(std::time::Instant::now());
        tracing::info!("Physics world initialized");

        // Phase 6: Initialize scripting runtime
        let mut script_runtime = ScriptRuntime::new();
        self.register_script_apis(&script_runtime);

        if let Some(world) = &self.scene_world {
            if let Err(error) = crate::sim::initialize_pending_scripts(
                world,
                &mut script_runtime,
                &self.project_root,
            ) {
                tracing::error!("{error}");
            }
        }

        self.script_runtime = Some(script_runtime);
        tracing::info!("Script runtime initialized");

        // Phase 7: Initialize event bus schema and audio
        self.event_bus.borrow_mut().load_schema(&self.project_root);

        // Phase 3: try to compile the render pipeline if --pipeline was given
        self.try_load_pipeline();

        // Phase 8: Start command socket server
        self.start_command_server();
    }

    /// Start the command socket, unless this run has none: a shipped game
    /// started without --socket passes an empty path.
    fn start_command_server(&mut self) {
        if self.args.socket.is_empty() {
            tracing::info!("Command socket disabled");
            return;
        }
        match CommandServer::start(&self.args.socket) {
            Ok(server) => {
                tracing::info!("Command socket: {}", server.socket_path);
                self.command_server = Some(server);
            }
            Err(e) => {
                tracing::error!(
                    "Command socket unavailable, MCP and socket commands will not work: {}",
                    e
                );
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
            let path = match crate::project_path::resolve(&self.project_root, scene_arg) {
                Ok(path) => path,
                Err(e) => {
                    tracing::error!("Invalid editor scene path: {}", e);
                    return;
                }
            };
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
            Some(&mut self.texture_cache),
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
                    let pos = entity_def
                        .components
                        .transform
                        .as_ref()
                        .map(|t| glam::Vec3::from(t.position))
                        .unwrap_or(glam::Vec3::ZERO);
                    let rot = entity_def
                        .components
                        .transform
                        .as_ref()
                        .map(|t| crate::world::euler_degrees_to_quat(t.rotation))
                        .unwrap_or(glam::Quat::IDENTITY);

                    if let Some(col_def) = &entity_def.components.collider {
                        let mut shape = crate::world::parse_collider_shape(col_def);
                        let is_trigger = col_def.is_trigger;
                        let restitution = col_def.restitution;
                        let friction = col_def.friction;
                        let body_type = entity_def
                            .components
                            .rigid_body
                            .as_ref()
                            .map(|rb| rb.body_type.as_str())
                            .unwrap_or("static");

                        // Resolve trimesh: look up mesh data from cache
                        if matches!(shape, crate::physics::PhysicsShape::Trimesh { .. }) {
                            let scale = entity_def
                                .components
                                .transform
                                .as_ref()
                                .map(|t| glam::Vec3::from(t.scale))
                                .unwrap_or(glam::Vec3::ONE);
                            if let Ok(mr) = sw.world.get::<&crate::components::MeshRenderer>(entity)
                            {
                                if let Some((verts, idxs)) =
                                    self.mesh_cache.get_physics_trimesh(mr.mesh_handle, scale)
                                {
                                    shape = crate::physics::PhysicsShape::Trimesh {
                                        vertices: verts,
                                        indices: idxs,
                                    };
                                } else {
                                    tracing::warn!("Trimesh collider for '{}': no mesh data, falling back to box", entity_def.id);
                                    let he = col_def.half_extents.unwrap_or([0.5, 0.5, 0.5]);
                                    shape = crate::physics::PhysicsShape::Box {
                                        half_extents: glam::Vec3::from(he),
                                    };
                                }
                            }
                        }

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
                                    .add_dynamic_body_with_sensor(
                                        entity,
                                        pos,
                                        rot,
                                        shape.clone(),
                                        mass,
                                        restitution,
                                        friction,
                                        ccd,
                                        is_trigger,
                                    );
                                let _ = sw.world.insert(
                                    entity,
                                    (
                                        crate::physics::RigidBody {
                                            handle: rb_handle,
                                            body_type: crate::physics::PhysicsBodyType::Dynamic,
                                        },
                                        crate::physics::Collider {
                                            handle: col_handle,
                                            shape,
                                            is_trigger,
                                        },
                                    ),
                                );
                            }
                            "kinematic" => {
                                let (rb_handle, col_handle) = physics_world.add_kinematic_body(
                                    entity,
                                    pos,
                                    rot,
                                    shape.clone(),
                                    is_trigger,
                                    restitution,
                                    friction,
                                );
                                let _ = sw.world.insert(
                                    entity,
                                    (
                                        crate::physics::RigidBody {
                                            handle: rb_handle,
                                            body_type: crate::physics::PhysicsBodyType::Kinematic,
                                        },
                                        crate::physics::Collider {
                                            handle: col_handle,
                                            shape,
                                            is_trigger,
                                        },
                                    ),
                                );
                            }
                            _ => {
                                let (rb_handle, col_handle) = physics_world.add_static_body(
                                    entity,
                                    pos,
                                    rot,
                                    shape.clone(),
                                    is_trigger,
                                    restitution,
                                    friction,
                                );
                                let _ = sw.world.insert(
                                    entity,
                                    (
                                        crate::physics::RigidBody {
                                            handle: rb_handle,
                                            body_type: crate::physics::PhysicsBodyType::Static,
                                        },
                                        crate::physics::Collider {
                                            handle: col_handle,
                                            shape,
                                            is_trigger,
                                        },
                                    ),
                                );
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
        self.register_script_apis(&script_runtime);

        let mut script_runtime = script_runtime;
        if let Some(world) = &self.scene_world {
            if let Err(error) = crate::sim::initialize_pending_scripts(
                world,
                &mut script_runtime,
                &self.project_root,
            ) {
                tracing::error!("{error}");
            }
        }
        self.script_runtime = Some(script_runtime);

        self.last_frame_time = Some(std::time::Instant::now());

        // Initialize editor camera: try to extract position from scene's main camera
        let cam_pos = self
            .scene_world
            .as_ref()
            .and_then(|sw| {
                let sw = sw.borrow();
                for (_entity, (transform, camera)) in sw
                    .world
                    .query::<(hecs::Entity, (&Transform, &Camera))>()
                    .iter()
                {
                    if camera.role == CameraRole::Main {
                        return Some(transform.position);
                    }
                }
                None
            })
            .unwrap_or(glam::Vec3::new(0.0, 5.0, 10.0));

        self.editor_camera = Some(EditorCamera::new(cam_pos, 0.0, -0.3));

        // Try to load render pipeline
        self.try_load_pipeline();

        // Start command socket
        self.start_command_server();

        tracing::info!("Editor mode initialized");
    }

    /// Create a default editor scene with ground plane, lights, marker cube, and camera.
    fn default_editor_scene() -> crate::scene::SceneFile {
        use crate::scene::*;
        SceneFile {
            version: SCENE_FORMAT_VERSION,
            name: "Editor Scene".to_string(),
            settings: SceneSettings {
                ambient_light: [0.15, 0.15, 0.2],
                fog: None,
                gravity: [0.0, -9.81, 0.0],
                sky: None,
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

        let pipeline_path = match crate::project_path::resolve(&self.project_root, &pipeline_arg) {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("Invalid pipeline path: {}", e);
                return;
            }
        };
        if !pipeline_path.exists() {
            tracing::error!("Pipeline file not found: {:?}", pipeline_path);
            return;
        }

        let mut vsync = None;
        match crate::pipeline::load_pipeline(&pipeline_path) {
            Ok(pipeline_file) => {
                vsync = Some(pipeline_file.settings.vsync);
                let tex_layout = self
                    .texture_resources
                    .as_ref()
                    .map(|tr| &tr.bind_group_layout);
                match crate::pipeline::compile_pipeline(
                    &gpu.device,
                    &pipeline_file,
                    &self.project_root,
                    &camera_state,
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
        drop(camera_state);
        if let Some(vsync) = vsync {
            self.set_vsync(vsync);
        }
    }

    /// Wait for the display's refresh (`vsync: true` in the pipeline's
    /// settings, the default) or present as fast as frames are ready.
    fn set_vsync(&mut self, vsync: bool) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let mode = if vsync {
            wgpu::PresentMode::Fifo
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        if gpu.config.present_mode != mode {
            gpu.config.present_mode = mode;
            gpu.surface.configure(&gpu.device, &gpu.config);
            tracing::info!("Present mode: {:?}", mode);
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

            let error_scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

            let camera_state = self.camera_state.as_ref().unwrap().borrow();
            let draw_pool = self.draw_pool.as_ref().unwrap();

            let tex_layout = self
                .texture_resources
                .as_ref()
                .map(|tr| &tr.bind_group_layout);
            let new_pipeline = crate::renderer::create_forward_pipeline(
                &gpu.device,
                &wgsl,
                gpu.config.format,
                &camera_state.bind_group_layout,
                &draw_pool.bind_group_layout,
                tex_layout,
            );

            let error = pollster::block_on(error_scope.pop());
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

            let error_scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

            let new_pipeline =
                crate::renderer::create_render_pipeline(&gpu.device, &wgsl, gpu.config.format);

            let error = pollster::block_on(error_scope.pop());
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
        tracing::info!("Hot-reloading scene: {:?}", changed_path);

        let new_scene = match crate::scene::load_scene(changed_path) {
            Ok(s) => s,
            Err(e) => {
                let file_name = changed_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.reload_notifications.push((
                    format!("Scene failed: {} - {}", file_name, e),
                    std::time::Instant::now(),
                    [1.0, 0.3, 0.3, 1.0],
                ));
                tracing::error!("Scene reload failed: {}, keeping old scene", e);
                return;
            }
        };

        // Reconciliation removes old handles before callbacks can run; keep
        // their names until surviving sensors have received their exit hooks.
        let previous_ids = scene_world
            .borrow()
            .entity_registry
            .iter()
            .map(|(id, &entity)| (entity, id.clone()))
            .collect();

        // Run destroy hooks while removed/replaced entities can still access
        // their components, and before reconciliation borrows either world.
        let retiring: Vec<_> = {
            let world = scene_world.borrow();
            world
                .current_scene
                .as_ref()
                .map(|old| {
                    old.entities
                        .iter()
                        .filter_map(|definition| {
                            let replacement = new_scene
                                .entities
                                .iter()
                                .find(|new| new.id == definition.id);
                            let retire = replacement.is_none_or(|new| {
                                crate::world::entity_needs_respawn(definition, new)
                                    || definition
                                        .components
                                        .script
                                        .as_ref()
                                        .map(|script| &script.source)
                                        != new
                                            .components
                                            .script
                                            .as_ref()
                                            .map(|script| &script.source)
                            });
                            retire
                                .then(|| world.entity_registry.get(&definition.id).copied())
                                .flatten()
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        if let Some(runtime) = self.script_runtime.as_mut() {
            for entity in retiring {
                runtime.remove_entity(entity);
            }
        }
        let mut scene_world = scene_world.borrow_mut();

        // Borrow physics_world for reconcile so new entities get physics bodies
        let mut pw_borrow = self.physics_world.as_ref().map(|pw| pw.borrow_mut());
        let pw_ref = pw_borrow.as_deref_mut();

        crate::world::reconcile_scene(
            &mut scene_world,
            &new_scene,
            &gpu.device,
            &gpu.queue,
            &self.project_root,
            &mut self.mesh_cache,
            &mut self.material_cache,
            &mut self.splat_cache,
            pw_ref,
            self.texture_resources.as_ref(),
        );

        // Resolve trimesh colliders for newly spawned entities
        if let Some(ref mut pw) = pw_borrow {
            for entity_def in &new_scene.entities {
                if let Some(col_def) = &entity_def.components.collider {
                    if col_def.shape == "trimesh" {
                        if let Some(&entity) = scene_world.entity_registry.get(&entity_def.id) {
                            let needs_resolve = if let Ok(col) =
                                scene_world.world.get::<&crate::physics::Collider>(entity)
                            {
                                matches!(&col.shape, crate::physics::PhysicsShape::Trimesh { vertices, .. } if vertices.is_empty())
                            } else {
                                false
                            };

                            if needs_resolve {
                                let scale = entity_def
                                    .components
                                    .transform
                                    .as_ref()
                                    .map(|t| glam::Vec3::from(t.scale))
                                    .unwrap_or(glam::Vec3::ONE);
                                let resolved_shape = if let Ok(mr) =
                                    scene_world
                                        .world
                                        .get::<&crate::components::MeshRenderer>(entity)
                                {
                                    if let Some((verts, idxs)) =
                                        self.mesh_cache.get_physics_trimesh(mr.mesh_handle, scale)
                                    {
                                        Some(crate::physics::PhysicsShape::Trimesh {
                                            vertices: verts,
                                            indices: idxs,
                                        })
                                    } else {
                                        let he = col_def.half_extents.unwrap_or([0.5, 0.5, 0.5]);
                                        Some(crate::physics::PhysicsShape::Box {
                                            half_extents: glam::Vec3::from(he),
                                        })
                                    }
                                } else {
                                    None
                                };

                                if let Some(new_shape) = resolved_shape {
                                    let rb_info = scene_world
                                        .world
                                        .get::<&crate::physics::RigidBody>(entity)
                                        .ok()
                                        .map(|rb| (rb.handle, rb.body_type.clone()));
                                    if let Some((old_handle, body_type_enum)) = rb_info {
                                        pw.remove_body(old_handle);
                                        let pos = entity_def
                                            .components
                                            .transform
                                            .as_ref()
                                            .map(|t| glam::Vec3::from(t.position))
                                            .unwrap_or(glam::Vec3::ZERO);
                                        let rot = entity_def
                                            .components
                                            .transform
                                            .as_ref()
                                            .map(|t| {
                                                crate::world::euler_degrees_to_quat(t.rotation)
                                            })
                                            .unwrap_or(glam::Quat::IDENTITY);
                                        let is_trigger = col_def.is_trigger;
                                        let restitution = col_def.restitution;
                                        let friction = col_def.friction;

                                        let (rb_handle, col_handle) = match body_type_enum {
                                            crate::physics::PhysicsBodyType::Dynamic => {
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
                                                pw.add_dynamic_body_with_sensor(
                                                    entity,
                                                    pos,
                                                    rot,
                                                    new_shape.clone(),
                                                    mass,
                                                    restitution,
                                                    friction,
                                                    ccd,
                                                    is_trigger,
                                                )
                                            }
                                            crate::physics::PhysicsBodyType::Kinematic => pw
                                                .add_kinematic_body(
                                                    entity,
                                                    pos,
                                                    rot,
                                                    new_shape.clone(),
                                                    is_trigger,
                                                    restitution,
                                                    friction,
                                                ),
                                            crate::physics::PhysicsBodyType::Static => pw
                                                .add_static_body(
                                                    entity,
                                                    pos,
                                                    rot,
                                                    new_shape.clone(),
                                                    is_trigger,
                                                    restitution,
                                                    friction,
                                                ),
                                        };
                                        let _ = scene_world.world.insert(
                                            entity,
                                            (
                                                crate::physics::RigidBody {
                                                    handle: rb_handle,
                                                    body_type: body_type_enum,
                                                },
                                                crate::physics::Collider {
                                                    handle: col_handle,
                                                    shape: new_shape,
                                                    is_trigger,
                                                },
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        drop(pw_borrow);
        drop(scene_world);
        if let (Some(world), Some(physics), Some(runtime)) =
            (&self.scene_world, &self.physics_world, &self.script_runtime)
        {
            crate::sim::dispatch_trigger_events_with_retired_ids(
                world,
                physics,
                runtime,
                &previous_ids,
            );
        }
        self.register_skeletons();
        if let (Some(world), Some(runtime)) = (&self.scene_world, self.script_runtime.as_mut()) {
            let stale: Vec<_> = runtime
                .entity_envs
                .keys()
                .copied()
                .filter(|&entity| !world.borrow().world.contains(entity))
                .collect();
            for entity in stale {
                runtime.forget_entity(entity);
            }
            if let Err(error) =
                crate::sim::initialize_pending_scripts(world, runtime, &self.project_root)
            {
                tracing::error!("scene reload: {error}");
            }
        }

        let file_name = changed_path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        self.reload_notifications.push((
            format!("Scene reloaded: {}", file_name),
            std::time::Instant::now(),
            [0.3, 1.0, 0.3, 1.0],
        ));
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
            if crate::watcher::is_active_scene(self.scene_path.as_deref(), &path) {
                self.handle_scene_reload(&path);
            }
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
                let reload_candidates: Vec<(hecs::Entity, std::path::PathBuf)> = scene_world_guard
                    .world
                    .query::<(hecs::Entity, &Script)>()
                    .iter()
                    .filter_map(|(entity, script)| {
                        let script_source_str = script.source.to_string_lossy();
                        for changed_path in &script_paths {
                            let changed_str = changed_path.to_string_lossy();
                            if changed_str.ends_with(&*script_source_str)
                                || changed_str.ends_with(
                                    script
                                        .source
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .as_ref(),
                                )
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
    fn update_fps_controller(&self) {
        if let (Some(input), Some(scene_world), Some(physics_world)) =
            (&self.input_state, &self.scene_world, &self.physics_world)
        {
            crate::sim::update_fps_controller(input, scene_world, physics_world, self.delta_time);
        }
    }

    /// Process collision damage: auto-damage on physics contact.
    fn process_collision_damage(&mut self) {
        if let (Some(scene_world), Some(physics_world), Some(script_runtime)) =
            (&self.scene_world, &self.physics_world, &self.script_runtime)
        {
            crate::sim::process_collision_damage(
                scene_world,
                physics_world,
                script_runtime,
                &self.entity_commands,
            );
        }
    }

    /// Update projectile ages and destroy expired ones.
    fn update_projectiles(&mut self) {
        if let Some(scene_world) = &self.scene_world {
            crate::sim::update_projectiles(scene_world, &self.entity_commands, self.delta_time);
        }
    }

    /// Register skeletons from newly loaded meshes and attach Animator components
    /// to entities whose meshes have skin data.
    fn register_skeletons(&mut self) {
        self.animation_system
            .register_from_mesh_cache(&mut self.mesh_cache);
        if let Some(world) = &self.scene_world {
            self.animation_system
                .attach_animators(&mut world.borrow_mut());
        }
    }

    /// Tick skeletal animations: advance time, compute bone matrix palettes.
    fn tick_animations(&mut self) {
        let scene_world_rc = match &self.scene_world {
            Some(sw) => sw,
            None => return,
        };
        let scene_world = scene_world_rc.borrow_mut();
        let dt = self.delta_time;

        // Collect entities with animators
        let animated: Vec<hecs::Entity> = scene_world
            .world
            .query::<(hecs::Entity, &crate::components::Animator)>()
            .iter()
            .map(|(e, _)| e)
            .collect();

        self.bone_palettes.clear();

        for entity in animated {
            if let Ok(mut animator) = scene_world
                .world
                .get::<&mut crate::components::Animator>(entity)
            {
                let palette = self.animation_system.tick_entity(&mut animator, dt);
                self.bone_palettes.insert(entity, palette);
            }
        }
    }

    /// Process the health system: detect deaths and fire on_death callbacks.
    fn process_health_system(&mut self) {
        if let (Some(scene_world), Some(script_runtime)) = (&self.scene_world, &self.script_runtime)
        {
            crate::sim::process_health(scene_world, script_runtime);
        }
    }

    /// Process deferred entity commands (spawn/destroy/scale/visibility).
    fn process_entity_commands(&mut self) {
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        // Tier 2: Process destroys FIRST (fixes destroy+spawn same-frame bug)
        if let Some(scene_world) = &self.scene_world {
            crate::sim::process_destroys(
                scene_world,
                self.physics_world.as_ref(),
                self.script_runtime.as_mut(),
                &self.entity_commands,
            );
        }

        // Use the same component constructor as scene loading and headless tests.
        let spawns: Vec<_> = std::mem::take(&mut self.entity_commands.borrow_mut().spawns);
        for cmd in &spawns {
            if let Some(scene_world) = &self.scene_world {
                {
                    let mut world = scene_world.borrow_mut();
                    let mut physics = self.physics_world.as_ref().map(|world| world.borrow_mut());
                    crate::world::spawn_entity(
                        &mut world,
                        &cmd.entity_def(),
                        &gpu.device,
                        &gpu.queue,
                        &self.project_root,
                        &mut self.mesh_cache,
                        &mut self.material_cache,
                        &mut self.splat_cache,
                        physics.as_deref_mut(),
                        self.texture_resources.as_ref(),
                        Some(&mut self.texture_cache),
                    );
                }
                crate::sim::finish_pool_spawn(scene_world, &self.pool_manager, cmd);
            }
        }

        // Process projectile spawns
        let proj_spawns: Vec<_> =
            std::mem::take(&mut self.entity_commands.borrow_mut().projectile_spawns);
        for cmd in &proj_spawns {
            if let (Some(scene_world), Some(physics_world)) =
                (&self.scene_world, &self.physics_world)
            {
                let mut scene_world = scene_world.borrow_mut();
                let mut physics_world = physics_world.borrow_mut();
                crate::world::spawn_projectile_entity(
                    &mut scene_world,
                    cmd,
                    &gpu.device,
                    &gpu.queue,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                    &mut physics_world,
                    self.texture_resources.as_ref(),
                );
            }
        }

        // Process dynamic spawns (bouncing physics objects)
        let dyn_spawns: Vec<_> =
            std::mem::take(&mut self.entity_commands.borrow_mut().dynamic_spawns);
        for cmd in &dyn_spawns {
            if let (Some(scene_world), Some(physics_world)) =
                (&self.scene_world, &self.physics_world)
            {
                let mut scene_world = scene_world.borrow_mut();
                let mut physics_world = physics_world.borrow_mut();
                crate::world::spawn_dynamic_entity(
                    &mut scene_world,
                    cmd,
                    &gpu.device,
                    &gpu.queue,
                    &self.project_root,
                    &mut self.mesh_cache,
                    &mut self.material_cache,
                    &mut physics_world,
                    self.texture_resources.as_ref(),
                );
            }
        }

        // Process pool operations
        let pool_ops: Vec<_> = std::mem::take(&mut self.entity_commands.borrow_mut().pool_ops);
        for op in pool_ops {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
                match op {
                    crate::world::PoolOp::Release(id) => {
                        if let Some(&entity) = scene_world.entity_registry.get(&id) {
                            // Hide the entity and mark as inactive
                            let _ = scene_world
                                .world
                                .insert_one(entity, crate::components::Hidden);
                            if let Ok(mut pooled) = scene_world
                                .world
                                .get::<&mut crate::components::Pooled>(entity)
                            {
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
        let scale_updates: Vec<_> =
            std::mem::take(&mut self.entity_commands.borrow_mut().scale_updates);
        for (id, scale) in scale_updates {
            if let Some(scene_world) = &self.scene_world {
                let scene_world = scene_world.borrow_mut();
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                        transform.scale = glam::Vec3::from(scale);
                        transform.dirty = true;
                    }
                }
            }
        }

        // Process visibility updates
        let vis_updates: Vec<_> =
            std::mem::take(&mut self.entity_commands.borrow_mut().visibility_updates);
        for (id, visible) in vis_updates {
            if let Some(scene_world) = &self.scene_world {
                let mut scene_world = scene_world.borrow_mut();
                if let Some(&entity) = scene_world.entity_registry.get(&id) {
                    if visible {
                        let _ = scene_world
                            .world
                            .remove_one::<crate::components::Hidden>(entity);
                    } else {
                        let _ = scene_world
                            .world
                            .insert_one(entity, crate::components::Hidden);
                    }
                }
            }
        }

        // Process texture swaps
        let tex_swaps: Vec<_> =
            std::mem::take(&mut self.entity_commands.borrow_mut().texture_swaps);
        if !tex_swaps.is_empty() {
            if let Some(tex_res) = &self.texture_resources {
                let layout = &tex_res.bind_group_layout;
                for swap in tex_swaps {
                    match self.texture_cache.get_or_load(
                        &gpu.device,
                        &gpu.queue,
                        layout,
                        &self.project_root,
                        &swap.texture_path,
                    ) {
                        Ok(tex_handle) => {
                            if let Some(scene_world) = &self.scene_world {
                                let mut sw = scene_world.borrow_mut();
                                if let Some(&entity) = sw.entity_registry.get(&swap.entity_id) {
                                    let mut changed = false;
                                    if let Ok(mr) = sw.world.get::<&MeshRenderer>(entity) {
                                        let mat = self.material_cache.get_mut(mr.material_handle);
                                        match swap.slot.as_str() {
                                            "albedo" => {
                                                mat.albedo_texture = Some(tex_handle);
                                                changed = true;
                                            }
                                            "normal" => {
                                                mat.normal_texture = Some(tex_handle);
                                                changed = true;
                                            }
                                            _ => tracing::warn!(
                                                "entity.set_texture: unknown slot '{}'",
                                                swap.slot
                                            ),
                                        }
                                    }
                                    if changed {
                                        let _ = sw.world.insert_one(
                                            entity,
                                            crate::world::UnsavedRuntimeState(
                                                "runtime texture changes cannot be saved"
                                                    .to_string(),
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => tracing::warn!(
                            "entity.set_texture: failed to load '{}': {}",
                            swap.texture_path,
                            e
                        ),
                    }
                }
            }
        }

        // Process runtime mesh creates
        let mesh_creates: Vec<_> =
            std::mem::take(&mut self.entity_commands.borrow_mut().mesh_creates);
        for cmd in mesh_creates {
            self.mesh_cache.insert_runtime_mesh(
                &gpu.device,
                &cmd.name,
                &cmd.positions,
                &cmd.normals,
                &cmd.uvs,
                &cmd.indices,
            );
        }

        if !spawns.is_empty() || !proj_spawns.is_empty() || !dyn_spawns.is_empty() {
            self.register_skeletons();
        }
        if let (Some(world), Some(runtime)) = (&self.scene_world, self.script_runtime.as_mut()) {
            if let Err(error) =
                crate::sim::initialize_pending_scripts(world, runtime, &self.project_root)
            {
                tracing::error!("{error}");
            }
        }
    }

    /// Process a pending scene load (deferred from Lua `scene.load(path)`).
    fn process_pending_scene_load(&mut self) {
        let scene_rel = match self.entity_commands.borrow_mut().pending_scene_load.take() {
            Some(p) => p,
            None => return,
        };

        let scene_path = match crate::project_path::resolve(&self.project_root, &scene_rel) {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("scene.load: {}", e);
                return;
            }
        };
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

        // Undo steps belong to the scene being replaced.
        self.undo_stack.clear();
        if !self.replace_scene(scene) {
            return;
        }
        // 9. Update scene_path for hot-reload
        self.scene_path = Some(scene_path);

        tracing::info!("Scene loaded via scene.load(\"{}\")", scene_rel);
    }

    /// Replace the running scene with `scene`: destroy every entity through
    /// the usual path (so `on_destroy` hooks run), reset per-scene state, then
    /// build and initialize the new entities. Shared by `scene.load` and the
    /// `undo` command. Returns false when there is no GPU to build with.
    pub(crate) fn replace_scene(&mut self, scene: crate::scene::SceneFile) -> bool {
        // Destroy through the same path as Lua and MCP. Never hold world borrows
        // while invoking user callbacks.
        if let Some(world) = &self.scene_world {
            let ids: Vec<_> = world.borrow().entity_registry.keys().cloned().collect();
            for id in ids {
                crate::sim::destroy_entity(
                    world,
                    self.physics_world.as_ref(),
                    self.script_runtime.as_mut(),
                    &id,
                );
            }
        }
        if let Some(runtime) = self.script_runtime.as_mut() {
            let stale: Vec<_> = runtime.entity_envs.keys().copied().collect();
            for entity in stale {
                runtime.forget_entity(entity);
            }
        }
        self.entity_commands.borrow_mut().clear();
        // Events from the previous scene, including teardown hooks, must not
        // reach subscriptions registered by the replacement scene.
        self.event_bus.borrow_mut().discard_pending();

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
        *self.swarm_bus.borrow_mut() = crate::swarm_api::SwarmBus::new();
        self.swarm_system.clear();

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return false,
        };

        // 6. Re-spawn entities
        if let Some(sw) = &self.scene_world {
            let mut sw = sw.borrow_mut();
            crate::world::spawn_all_entities(
                &mut sw,
                &scene,
                &gpu.device,
                &gpu.queue,
                &self.project_root,
                &mut self.mesh_cache,
                &mut self.material_cache,
                &mut self.splat_cache,
                None,
                self.texture_resources.as_ref(),
                Some(&mut self.texture_cache),
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
                        let pos = entity_def
                            .components
                            .transform
                            .as_ref()
                            .map(|t| glam::Vec3::from(t.position))
                            .unwrap_or(glam::Vec3::ZERO);
                        let rot = entity_def
                            .components
                            .transform
                            .as_ref()
                            .map(|t| crate::world::euler_degrees_to_quat(t.rotation))
                            .unwrap_or(glam::Quat::IDENTITY);

                        if let Some(cc_def) = &entity_def.components.character_controller {
                            let half_height = cc_def.height / 2.0 - cc_def.radius;
                            let (rb_handle, col_handle) = pw.add_character_body(
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
                            let _ = sw
                                .world
                                .insert(entity, (rb_comp, col_comp, cc_comp, player));
                        } else if let Some(col_def) = &entity_def.components.collider {
                            let mut shape = crate::world::parse_collider_shape(col_def);
                            let is_trigger = col_def.is_trigger;
                            let restitution = col_def.restitution;
                            let friction = col_def.friction;
                            let body_type = entity_def
                                .components
                                .rigid_body
                                .as_ref()
                                .map(|rb| rb.body_type.as_str())
                                .unwrap_or("static");

                            // Resolve trimesh
                            if matches!(shape, crate::physics::PhysicsShape::Trimesh { .. }) {
                                let scale = entity_def
                                    .components
                                    .transform
                                    .as_ref()
                                    .map(|t| glam::Vec3::from(t.scale))
                                    .unwrap_or(glam::Vec3::ONE);
                                if let Ok(mr) =
                                    sw.world.get::<&crate::components::MeshRenderer>(entity)
                                {
                                    if let Some((verts, idxs)) =
                                        self.mesh_cache.get_physics_trimesh(mr.mesh_handle, scale)
                                    {
                                        shape = crate::physics::PhysicsShape::Trimesh {
                                            vertices: verts,
                                            indices: idxs,
                                        };
                                    } else {
                                        let he = col_def.half_extents.unwrap_or([0.5, 0.5, 0.5]);
                                        shape = crate::physics::PhysicsShape::Box {
                                            half_extents: glam::Vec3::from(he),
                                        };
                                    }
                                }
                            }

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
                                    let (rb_handle, col_handle) = pw.add_dynamic_body_with_sensor(
                                        entity,
                                        pos,
                                        rot,
                                        shape.clone(),
                                        mass,
                                        restitution,
                                        friction,
                                        ccd,
                                        is_trigger,
                                    );
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
                                "kinematic" => {
                                    let (rb_handle, col_handle) = pw.add_kinematic_body(
                                        entity,
                                        pos,
                                        rot,
                                        shape.clone(),
                                        is_trigger,
                                        restitution,
                                        friction,
                                    );
                                    let rb_comp = crate::physics::RigidBody {
                                        handle: rb_handle,
                                        body_type: crate::physics::PhysicsBodyType::Kinematic,
                                    };
                                    let col_comp = crate::physics::Collider {
                                        handle: col_handle,
                                        shape,
                                        is_trigger,
                                    };
                                    let _ = sw.world.insert(entity, (rb_comp, col_comp));
                                }
                                _ => {
                                    let (rb_handle, col_handle) = pw.add_static_body(
                                        entity,
                                        pos,
                                        rot,
                                        shape.clone(),
                                        is_trigger,
                                        restitution,
                                        friction,
                                    );
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

        // Load and initialize scripts only after construction releases its borrows.
        if let (Some(world), Some(runtime)) = (&self.scene_world, self.script_runtime.as_mut()) {
            if let Err(error) =
                crate::sim::initialize_pending_scripts(world, runtime, &self.project_root)
            {
                tracing::error!("scene.load: {error}");
            }
        }
        true
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
        for (entity, (transform, player, camera)) in scene_world
            .world
            .query::<(hecs::Entity, (&Transform, &Player, &Camera))>()
            .iter()
        {
            if camera.role == CameraRole::Main {
                // Check for CameraMode component
                let camera_mode = scene_world.world.get::<&CameraMode>(entity).ok();

                let is_third_person = camera_mode
                    .as_ref()
                    .map(|cm| matches!(**cm, CameraMode::ThirdPerson { .. }))
                    .unwrap_or(false);

                if is_third_person {
                    let (distance, height_offset) = if let Some(ref cm) = camera_mode {
                        match **cm {
                            CameraMode::ThirdPerson {
                                distance,
                                height_offset,
                                ..
                            } => (distance, height_offset),
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
                        let physics_world = physics_world.borrow_mut();
                        let ray_dir = (desired_pos - target).normalize_or_zero();
                        let ray_dist = (desired_pos - target).length();
                        if let Some((_entity, toi, _hit, _normal)) =
                            physics_world.raycast_detailed(target, ray_dir, ray_dist, None)
                        {
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
                        position: transform.position
                            + glam::Vec3::new(0.0, player.height * 0.4, 0.0)
                            + shake_offset,
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
            for (_entity, (transform, camera)) in scene_world
                .world
                .query::<(hecs::Entity, (&Transform, &Camera))>()
                .iter()
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
            editor_cam.update(&input, self.delta_time);
        }

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return,
        };

        if let (Some(editor_cam), Some(camera_state)) = (&self.editor_camera, &self.camera_state) {
            let mut camera_state = camera_state.borrow_mut();
            editor_cam.apply_to_camera_state(
                &mut camera_state,
                &gpu.queue,
                gpu.config.width,
                gpu.config.height,
            );
        }
    }

    /// Render a full editor frame: 3D scene + overlay.
    /// Draw editor status overlay.
    fn draw_editor_overlay(&mut self) {
        let (Some(ui_rc), Some(font_rc), Some(gpu)) =
            (&self.ui_renderer, &self.bitmap_font, &self.gpu)
        else {
            return;
        };
        let mut ui = ui_rc.borrow_mut();
        let font_ref = font_rc.borrow();
        let font: &BitmapFont = &font_ref;

        let sz = 16.0;
        let x = 10.0;
        let green = [0.3, 1.0, 0.3, 1.0];
        let white = [1.0, 1.0, 1.0, 0.9];
        let dim = [0.7, 0.7, 0.7, 0.7];

        // Background strip
        ui.draw_rect(
            0.0,
            0.0,
            gpu.config.width as f32,
            30.0,
            [0.0, 0.0, 0.0, 0.6],
        );
        ui.draw_text(x, 7.0, "EDITOR MODE", sz, green, font);

        // Entity count
        let entity_count = self
            .scene_world
            .as_ref()
            .map(|sw| sw.borrow().entity_registry.len())
            .unwrap_or(0);
        ui.draw_text(
            180.0,
            7.0,
            &format!("Entities: {}", entity_count),
            sz,
            white,
            font,
        );

        // Camera position
        if let Some(cam) = &self.editor_camera {
            let pos_text = format!(
                "Cam: ({:.1}, {:.1}, {:.1})  Speed: {:.1}",
                cam.position.x, cam.position.y, cam.position.z, cam.speed
            );
            ui.draw_text(380.0, 7.0, &pos_text, sz, dim, font);
        }

        // Recent commands (bottom-left, with background panel)
        let now = std::time::Instant::now();
        let h = gpu.config.height as f32;
        let visible_cmds: Vec<_> = self
            .editor_command_log
            .iter()
            .rev()
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
                let alpha = if age > 7.0 {
                    1.0 - (age - 7.0) / 3.0
                } else {
                    1.0
                };
                let color = [0.5, 0.9, 1.0, alpha.clamp(0.0, 1.0)];
                ui.draw_text(x + 4.0, y, &format!("> {}", cmd_text), 14.0, color, font);
                y += 18.0;
            }
        }
    }

    // ── Frame loop ──────────────────────────────────────────────────────

    /// Run one frame: gather input, advance the simulation, then render.
    /// Called for every `RedrawRequested` event.
    fn run_frame(&mut self) {
        self.update_frame_timing();

        // Phase 8: Process command socket before input
        self.process_commands();

        // Editor mode: update the free camera
        if self.args.editor_mode {
            self.update_editor_camera();
        }

        self.update_cursor_capture();
        self.handle_render_debug_keys();

        // Poll for file changes (shader + scene + pipeline)
        self.poll_changes();

        if self.scene_world.is_some() {
            if !self.paused {
                self.tick_simulation();
            }
            self.prepare_render();
            self.render_frame();

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

    /// Measure the time since the previous frame, capped at 0.1 s.
    fn update_frame_timing(&mut self) {
        // Calculate delta time
        let now = std::time::Instant::now();
        if let Some(last) = self.last_frame_time {
            self.delta_time = now.duration_since(last).as_secs_f32().min(0.1);
        }
        self.last_frame_time = Some(now);
    }

    /// Release the cursor on Escape and capture it again on the next game input.
    fn update_cursor_capture(&mut self) {
        // Handle Escape to toggle cursor capture (skip in editor mode)
        if !self.args.editor_mode {
            if let Some(input_rc) = &self.input_state {
                let mut input = input_rc.borrow_mut();
                if input.key_held(Key::Escape) {
                    if let Some(gpu) = &self.gpu {
                        let _ = gpu
                            .window
                            .set_cursor_grab(winit::window::CursorGrabMode::None);
                        gpu.window.set_cursor_visible(true);
                    }
                    input.cursor_captured = false;
                }

                // Handle mouse click or any movement key to capture cursor
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
                            let _ = gpu
                                .window
                                .set_cursor_grab(winit::window::CursorGrabMode::Locked)
                                .or_else(|_| {
                                    gpu.window
                                        .set_cursor_grab(winit::window::CursorGrabMode::Confined)
                                });
                            gpu.window.set_cursor_visible(false);
                        }
                        input.cursor_captured = true;
                    }
                }
            }
        }
    }

    /// Apply the render-debug key toggles (0, H and, with the HUD open, 1-6).
    fn handle_render_debug_keys(&mut self) {
        // Render debug toggles: 0 always toggles HUD, 1-6 only when HUD is visible
        if let Some(input) = &self.input_state {
            let input = input.borrow_mut();
            if input.just_pressed_key(Key::Digit0) {
                self.render_debug.show_hud = !self.render_debug.show_hud;
            }
            if input.just_pressed_key(Key::H) {
                self.render_debug.show_colliders = !self.render_debug.show_colliders;
                tracing::info!(
                    "Collider wireframes: {}",
                    if self.render_debug.show_colliders {
                        "ON"
                    } else {
                        "OFF"
                    }
                );
            }
            if self.render_debug.show_hud {
                if input.just_pressed_key(Key::Digit1) {
                    self.render_debug.bloom_enabled = !self.render_debug.bloom_enabled;
                    tracing::info!(
                        "Bloom: {}",
                        if self.render_debug.bloom_enabled {
                            "ON"
                        } else {
                            "OFF"
                        }
                    );
                }
                if input.just_pressed_key(Key::Digit2) {
                    // Cycle light intensity: 1x → 5x → 10x → 20x → 1x
                    self.render_debug.light_intensity_mult =
                        match self.render_debug.light_intensity_mult as u32 {
                            0..=1 => 5.0,
                            2..=5 => 10.0,
                            6..=10 => 20.0,
                            _ => 1.0,
                        };
                    tracing::info!(
                        "Light intensity: {}x",
                        self.render_debug.light_intensity_mult
                    );
                }
                if input.just_pressed_key(Key::Digit3) {
                    self.render_debug.point_lights_enabled =
                        !self.render_debug.point_lights_enabled;
                    tracing::info!(
                        "Point Lights: {}",
                        if self.render_debug.point_lights_enabled {
                            "ON"
                        } else {
                            "OFF"
                        }
                    );
                }
                if input.just_pressed_key(Key::Digit4) {
                    self.render_debug.emission_enabled = !self.render_debug.emission_enabled;
                    tracing::info!(
                        "Emission: {}",
                        if self.render_debug.emission_enabled {
                            "ON"
                        } else {
                            "OFF"
                        }
                    );
                }
                if input.just_pressed_key(Key::Digit5) {
                    self.render_debug.torch_flicker_enabled =
                        !self.render_debug.torch_flicker_enabled;
                    tracing::info!(
                        "Torch Flicker: {}",
                        if self.render_debug.torch_flicker_enabled {
                            "ON"
                        } else {
                            "OFF"
                        }
                    );
                }
                if input.just_pressed_key(Key::Digit6) {
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
    }

    /// Advance the game by one frame: player and physics, gameplay systems,
    /// scripts, deferred commands, events, then audio and particles. Not
    /// called while the engine is paused.
    fn tick_simulation(&mut self) {
        let dt = self.delta_time;

        // Phase 5: FPS controller update (skip in editor mode — uses free camera)
        if !self.args.editor_mode
            && self
                .input_state
                .as_ref()
                .map(|i| i.borrow().cursor_captured)
                .unwrap_or(false)
        {
            self.update_fps_controller();
        }

        // Always advance physics (gravity, collisions, etc.), in
        // fixed steps so the simulation does not depend on the
        // frame rate. Scripts get `fixed_update` before each step.
        if let (Some(scene_world), Some(physics_world)) = (&self.scene_world, &self.physics_world) {
            crate::sim::run_fixed_steps(
                physics_world,
                scene_world,
                self.script_runtime.as_ref(),
                self.delta_time,
            );
        }

        self.dispatch_physics_events();

        // Tier 1: Process collision damage (auto-damage + projectile hits)
        self.process_collision_damage();

        // Tier 1: Update projectiles (age tracking, lifetime expiry)
        self.update_projectiles();

        // Tier 4: swarm attacks hurt their targets; kill and hit events
        if let (Some(scene_world), Some(script_runtime)) = (&self.scene_world, &self.script_runtime)
        {
            crate::sim::process_swarm_reports(
                &self.swarm_bus,
                scene_world,
                script_runtime,
                &self.event_bus,
            );
        }

        // Tier 1: Process health system (on_death callbacks)
        self.process_health_system();

        // Phase 6: Update scripts
        self.update_scripts(dt);

        // Tick skeletal animations
        self.tick_animations();

        // Process deferred entity commands from Lua
        self.process_entity_commands();

        // Process deferred scene load (must be after entity commands)
        self.process_pending_scene_load();

        // Tier 2: Dispatch Lua event listeners
        self.dispatch_lua_events(dt);

        // Show any script or listener that was stopped for exceeding a limit
        if let Some(script_runtime) = &self.script_runtime {
            for message in script_runtime.take_limit_reports() {
                self.reload_notifications.push((
                    message,
                    std::time::Instant::now(),
                    [1.0, 0.3, 0.3, 1.0],
                ));
            }
        }

        let _tween_results = self.tween_system.update(dt);
        self.audio_system.borrow_mut().cleanup();

        // Tier 2: Update particle system
        if let Some(scene_world) = &self.scene_world {
            let scene_world = scene_world.borrow();
            self.particle_system.borrow_mut().update(dt, &scene_world);
        }

        // Update listener position for spatial audio
        if let Some(scene_world) = &self.scene_world {
            let scene_world = scene_world.borrow();
            // Only the first player entity drives the listener.
            let listener_pos = scene_world
                .world
                .query::<(hecs::Entity, (&Transform, &Player))>()
                .iter()
                .next()
                .map(|(_entity, (transform, _player))| transform.position);
            if let Some(pos) = listener_pos {
                self.audio_system.borrow_mut().set_listener_position(pos);
            }
        }
    }

    /// Deliver contact and trigger hooks accumulated by the fixed steps.
    fn dispatch_physics_events(&self) {
        if let (Some(scene_world), Some(physics_world), Some(script_runtime)) =
            (&self.scene_world, &self.physics_world, &self.script_runtime)
        {
            crate::sim::dispatch_physics_events(scene_world, physics_world, script_runtime);
        }
    }

    /// Call `update(dt)` on every scripted entity.
    fn update_scripts(&self, dt: f32) {
        if let (Some(scene_world), Some(script_runtime)) = (&self.scene_world, &self.script_runtime)
        {
            crate::sim::update_scripts(scene_world, script_runtime, dt);
        }
    }

    /// Advance the event bus and hand this frame's events to Lua listeners.
    fn dispatch_lua_events(&self, dt: f32) {
        crate::sim::dispatch_lua_events(
            &self.event_bus,
            &self.lua_event_listeners,
            self.script_runtime.as_ref(),
            dt,
        );
    }

    /// Get the scene ready to draw: world transforms, camera, GPU draw
    /// buffer size, splat order and the editor overlay. Requires a loaded scene.
    fn prepare_render(&mut self) {
        // Update transforms and camera
        {
            let mut sw = self.scene_world.as_ref().unwrap().borrow_mut();
            crate::transform::update_transforms(&mut sw.world);
        }
        // Editor mode: camera already updated above via update_editor_camera()
        if !self.args.editor_mode {
            self.update_camera();
        }

        // Build the sky when the scene's changes (including scene loads).
        if let (Some(gpu), Some(scene_world), Some(compiled)) =
            (&self.gpu, &self.scene_world, &mut self.compiled_pipeline)
        {
            let sky = scene_world
                .borrow()
                .current_scene
                .as_ref()
                .and_then(|scene| scene.settings.sky.clone());
            crate::pipeline::sync_environment(
                &gpu.device,
                &gpu.queue,
                compiled,
                sky.as_ref(),
                &self.project_root,
            );
        }

        // Tier 2: Grow GPU draw buffer if needed
        if let (Some(gpu), Some(scene_world), Some(draw_pool)) =
            (&self.gpu, &self.scene_world, &mut self.draw_pool)
        {
            let sw = scene_world.borrow();
            let mut visible_count = 0u32;
            for (entity, _) in sw
                .world
                .query::<(hecs::Entity, &crate::components::MeshRenderer)>()
                .iter()
            {
                if sw.world.get::<&crate::components::Hidden>(entity).is_ok() {
                    continue;
                }
                visible_count += 1;
            }
            draw_pool.ensure_capacity(&gpu.device, visible_count);
        }

        // Tier 4: step GPU swarms once per frame (not again for captures).
        // While paused they only rewrite their draw data.
        if let (Some(gpu), Some(scene_world), Some(draw_pool)) =
            (&self.gpu, &self.scene_world, &self.draw_pool)
        {
            let dt = if self.paused { 0.0 } else { self.delta_time };
            let physics = self.physics_world.as_ref().map(|p| p.borrow());
            self.swarm_system.update(
                gpu,
                &scene_world.borrow(),
                physics.as_deref(),
                &self.mesh_cache,
                &self.material_cache,
                draw_pool,
                &mut self.swarm_bus.borrow_mut(),
                self.gpu_timer.as_ref(),
                dt,
            );
        }

        // Sort splats for correct alpha blending (CPU back-to-front)
        if let (Some(gpu), Some(scene_world), Some(camera_state)) =
            (&self.gpu, &self.scene_world, &self.camera_state)
        {
            let cs = camera_state.borrow();
            let view_matrix = cs.view_matrix();
            let sw = scene_world.borrow();
            for (_entity, splat) in sw.world.query::<(hecs::Entity, &GaussianSplat)>().iter() {
                self.splat_cache
                    .sort_splats(splat.splat_handle, &view_matrix, &gpu.queue);
            }
        }

        // Queue editor overlay draw commands (before gpu borrow)
        if self.args.editor_mode {
            self.draw_editor_overlay();
        }
    }

    /// Render the 3D scene into `target`, without collider wireframes or
    /// the UI overlay. Used for the window and for frame captures.
    fn render_scene_to(
        &self,
        gpu: &GpuState,
        target: &wgpu::TextureView,
        timer: Option<&crate::gpu_timer::GpuTimer>,
    ) {
        if self.compiled_pipeline.is_some() {
            if let (Some(scene_world), Some(camera_state), Some(draw_pool), Some(compiled)) = (
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
                    &sw,
                    &cs,
                    draw_pool,
                    &self.mesh_cache,
                    &self.material_cache,
                    &self.splat_cache,
                    target,
                    &self.render_debug,
                    self.texture_resources.as_ref(),
                    &self.bone_palettes,
                    Some(&self.texture_cache),
                    &self.swarm_system.draws(),
                    timer,
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
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Forward Render Encoder"),
                });
            let sw = scene_world.borrow();
            let cs = camera_state.borrow();
            crate::renderer::render_scene_to_view(
                gpu,
                &sw,
                &cs,
                draw_pool,
                &self.mesh_cache,
                &self.material_cache,
                forward_pipeline,
                target,
                &mut encoder,
                self.texture_resources.as_ref(),
            );
            gpu.queue.submit(std::iter::once(encoder.finish()));
        }
    }

    /// GPU timings as JSON: average milliseconds per frame over the last 60
    /// frames, for the frame and per pass (the swarm step is "swarms").
    fn render_frame(&mut self) {
        // Acquire swapchain and render 3D scene + UI overlay
        if let Some(gpu) = &self.gpu {
            let output = match gpu.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t)
                | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    return;
                }
                // Minimized, hidden or slow to present: skip this frame.
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    return
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    tracing::error!("Surface validation error while acquiring a frame");
                    return;
                }
            };

            let swapchain_view = output
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            // Render 3D scene
            self.render_scene_to(gpu, &swapchain_view, self.gpu_timer.as_ref());

            // Debug wireframe pass: draw collider shapes over 3D scene
            if self.render_debug.show_colliders {
                if let (Some(debug_draw), Some(physics_world), Some(camera_state)) =
                    (&self.debug_draw, &self.physics_world, &self.camera_state)
                {
                    let pw = physics_world.borrow();
                    let cs = camera_state.borrow();
                    let mut encoder =
                        gpu.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("Debug Wireframe Encoder"),
                            });
                    debug_draw.render(
                        &gpu.device,
                        &mut encoder,
                        &swapchain_view,
                        &gpu.depth_view,
                        &cs,
                        &pw,
                    );
                    gpu.queue.submit(std::iter::once(encoder.finish()));
                }
            }

            // UI overlay pass (drawn on top of 3D scene)
            if let (Some(ui_rc), Some(font_rc)) = (&self.ui_renderer, &self.bitmap_font) {
                let mut ui = ui_rc.borrow_mut();
                let font_guard = font_rc.borrow();
                let font: &BitmapFont = &font_guard;
                // Queue render debug HUD if enabled
                if self.render_debug.show_hud {
                    let on = [0.3, 1.0, 0.3, 1.0];
                    let off = [1.0, 0.3, 0.3, 1.0];
                    let val = [1.0, 0.9, 0.3, 1.0];
                    let hdr = [0.7, 0.7, 0.7, 1.0];
                    let sz = 16.0;
                    let x = 10.0;
                    let mut y = 10.0;

                    ui.draw_text(x, y, "RENDER DEBUG", sz, hdr, font);
                    y += sz + 4.0;
                    let c = if self.render_debug.bloom_enabled {
                        on
                    } else {
                        off
                    };
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[1] Bloom: {}",
                            if self.render_debug.bloom_enabled {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ),
                        sz,
                        c,
                        font,
                    );
                    y += sz + 2.0;
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[2] Light Boost: {}x",
                            self.render_debug.light_intensity_mult
                        ),
                        sz,
                        val,
                        font,
                    );
                    y += sz + 2.0;
                    let c = if self.render_debug.point_lights_enabled {
                        on
                    } else {
                        off
                    };
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[3] Point Lights: {}",
                            if self.render_debug.point_lights_enabled {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ),
                        sz,
                        c,
                        font,
                    );
                    y += sz + 2.0;
                    let c = if self.render_debug.emission_enabled {
                        on
                    } else {
                        off
                    };
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[4] Emission: {}",
                            if self.render_debug.emission_enabled {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ),
                        sz,
                        c,
                        font,
                    );
                    y += sz + 2.0;
                    let c = if self.render_debug.torch_flicker_enabled {
                        on
                    } else {
                        off
                    };
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[5] Torch Flicker: {}",
                            if self.render_debug.torch_flicker_enabled {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ),
                        sz,
                        c,
                        font,
                    );
                    y += sz + 2.0;
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[6] Ambient: {}",
                            if self.render_debug.ambient_override < 0.1 {
                                "scene".to_string()
                            } else {
                                format!("{:.1}", self.render_debug.ambient_override)
                            }
                        ),
                        sz,
                        val,
                        font,
                    );
                    y += sz + 2.0;
                    ui.draw_text(x, y, "[0] Toggle this HUD", sz, hdr, font);
                    y += sz + 2.0;
                    let c = if self.render_debug.show_colliders {
                        on
                    } else {
                        off
                    };
                    ui.draw_text(
                        x,
                        y,
                        &format!(
                            "[H] Colliders: {}",
                            if self.render_debug.show_colliders {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ),
                        sz,
                        c,
                        font,
                    );
                    y += sz + 2.0;
                    if let Some(timer) = &self.gpu_timer {
                        let t = timer.timings();
                        let swarms = t
                            .pass_ms(crate::gpu_timer::SWARMS)
                            .map_or(String::new(), |ms| format!(" (swarms {ms:.2})"));
                        ui.draw_text(
                            x,
                            y,
                            &format!("GPU: {:.2} ms{}", t.frame_ms, swarms),
                            sz,
                            val,
                            font,
                        );
                    }
                }

                // Always show collider indicator when active
                if self.render_debug.show_colliders {
                    ui.draw_text(
                        10.0,
                        (gpu.config.height as f32) - 30.0,
                        "[H] Collider wireframes ON",
                        14.0,
                        [0.0, 1.0, 1.0, 1.0],
                        font,
                    );
                }

                // Reload notifications (always visible, auto-fade)
                self.reload_notifications
                    .retain(|(_, t, _)| t.elapsed().as_secs_f32() < 4.0);
                let screen_w = gpu.config.width as f32;
                let screen_h = gpu.config.height as f32;
                for (i, (msg, t, color)) in self.reload_notifications.iter().rev().enumerate() {
                    let age = t.elapsed().as_secs_f32();
                    let alpha = if age > 3.0 { 1.0 - (age - 3.0) } else { 1.0 };
                    let c = [color[0], color[1], color[2], color[3] * alpha];
                    let text_w = msg.len() as f32 * 8.0; // approximate monospace width
                    let x = screen_w - text_w - 20.0;
                    let y = screen_h - 50.0 - (i as f32 * 20.0);
                    ui.draw_text(x.max(10.0), y, msg, 14.0, c, font);
                }

                let mut ui_encoder =
                    gpu.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("UI Encoder"),
                        });
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

            if let Some(timer) = self.gpu_timer.as_mut() {
                timer.finish_frame(&gpu.device, &gpu.queue);
            }
            gpu.queue.present(output);
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

        let gpu_state = pollster::block_on(crate::renderer::init_gpu(
            Arc::clone(&window),
            &initial_wgsl,
        ));

        self.gpu_timer = crate::gpu_timer::GpuTimer::new(&gpu_state.device, &gpu_state.queue);
        if self.gpu_timer.is_none() {
            tracing::info!("GPU timings unavailable: the adapter has no timestamp queries");
        }
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
        if let (Some(input), Some(input_event)) =
            (&self.input_state, winit_input::window_input_event(&event))
        {
            input.borrow_mut().handle_event(input_event);
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
                        let (depth_texture, depth_view) = crate::renderer::create_depth_texture(
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
            WindowEvent::RedrawRequested => self.run_frame(),
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let (Some(input), Some(input_event)) =
            (&self.input_state, winit_input::device_input_event(&event))
        {
            input.borrow_mut().handle_event(input_event);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }
}
