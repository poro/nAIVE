use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use notify::RecommendedWatcher;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::cli::CliArgs;
use crate::renderer::GpuState;
use crate::watcher::WatchEvent;

/// Main engine struct implementing winit's ApplicationHandler.
pub struct Engine {
    #[allow(dead_code)]
    pub args: CliArgs,
    pub gpu: Option<GpuState>,
    pub project_root: PathBuf,
    _watcher: Option<RecommendedWatcher>,
    shader_rx: Option<mpsc::Receiver<WatchEvent>>,
}

impl Engine {
    pub fn new(args: CliArgs) -> Self {
        let project_root = PathBuf::from(&args.project);
        Self {
            args,
            gpu: None,
            project_root,
            _watcher: None,
            shader_rx: None,
        }
    }

    /// Get the initial WGSL shader source, trying SLANG first.
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

    /// Start the file watcher on the shaders directory.
    fn start_shader_watcher(&mut self) {
        let shaders_dir = self.project_root.join("shaders");
        if !shaders_dir.exists() {
            tracing::warn!("Shaders directory not found: {:?}", shaders_dir);
            return;
        }

        match crate::watcher::start_watching(&shaders_dir) {
            Ok((watcher, rx)) => {
                self._watcher = Some(watcher);
                self.shader_rx = Some(rx);
                tracing::info!("Shader hot-reload enabled");
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

        let wgsl = match crate::shader::compile_triangle_shader(Some(changed_path)) {
            Ok(wgsl) => wgsl,
            Err(e) => {
                tracing::error!("Shader reload failed: {}, keeping old pipeline", e);
                return;
            }
        };

        // Try to recreate the pipeline. On WGSL validation failure, wgpu may panic.
        // Use push_error_scope to catch validation errors gracefully.
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
        tracing::info!("Shader hot-reload complete");
    }

    /// Poll for shader change events (non-blocking).
    fn poll_shader_changes(&mut self) {
        let paths: Vec<PathBuf> = if let Some(rx) = &self.shader_rx {
            let mut paths = Vec::new();
            while let Ok(event) = rx.try_recv() {
                match event {
                    WatchEvent::ShaderChanged(path) => {
                        paths.push(path);
                    }
                }
            }
            paths
        } else {
            return;
        };

        // Deduplicate: only reload each unique path once per frame
        let mut seen = std::collections::HashSet::new();
        for path in paths {
            if seen.insert(path.clone()) {
                self.handle_shader_reload(&path);
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

        self.start_shader_watcher();
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
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.poll_shader_changes();

                if let Some(gpu) = &self.gpu {
                    crate::renderer::render(gpu);
                    gpu.window.request_redraw();
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
