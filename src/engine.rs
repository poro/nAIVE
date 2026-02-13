use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use notify::RecommendedWatcher;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::camera::CameraState;
use crate::cli::CliArgs;
use crate::components::{Camera, CameraRole, Transform};
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::renderer::{DrawUniformPool, GpuState};
use crate::watcher::WatchEvent;
use crate::world::SceneWorld;

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
    pub camera_state: Option<CameraState>,
    pub draw_pool: Option<DrawUniformPool>,
    pub forward_pipeline: Option<wgpu::RenderPipeline>,
    scene_path: Option<PathBuf>,
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
            camera_state: None,
            draw_pool: None,
            forward_pipeline: None,
            scene_path: None,
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

        // Compile the forward shader
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
        );

        self.scene_world = Some(scene_world);
        self.camera_state = Some(camera_state);
        self.draw_pool = Some(draw_pool);
        self.forward_pipeline = Some(forward_pipeline);
        self.scene_path = Some(scene_path);

        tracing::info!("Scene loaded and forward pipeline created");
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

        // Determine if this is a forward shader or the triangle shader
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
        );

        tracing::info!("Scene hot-reload complete");
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

        for event in events {
            match event {
                WatchEvent::ShaderChanged(path) => {
                    shader_paths.insert(path);
                }
                WatchEvent::SceneChanged(path) => {
                    scene_paths.insert(path);
                }
                WatchEvent::MaterialChanged(_path) => {
                    // Material hot-reload: for Phase 2, just log it
                    tracing::info!("Material changed (reload not yet implemented)");
                }
            }
        }

        for path in shader_paths {
            self.handle_shader_reload(&path);
        }

        for path in scene_paths {
            self.handle_scene_reload(&path);
        }
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

        // Find the main camera entity
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

        // Start watchers (unified for shaders, scenes, materials)
        self.start_watcher();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
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
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Poll for file changes (shader + scene)
                self.poll_changes();

                if self.scene_world.is_some() {
                    // Phase 2: scene rendering path
                    crate::transform::update_transforms(
                        &mut self.scene_world.as_mut().unwrap().world,
                    );
                    self.update_camera();

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
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }
}
