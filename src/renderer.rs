use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::camera::CameraState;
use crate::components::{MeshRenderer, Transform};
use crate::material::MaterialCache;
use crate::mesh::{MeshCache, Vertex3D};
use crate::world::SceneWorld;

// --- Phase 1 triangle types (kept for fallback) ---

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub color: [f32; 3],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x3];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

pub const TRIANGLE_VERTICES: &[Vertex] = &[
    Vertex {
        position: [0.0, 0.5],
        color: [1.0, 0.0, 0.0],
    },
    Vertex {
        position: [-0.5, -0.5],
        color: [0.0, 1.0, 0.0],
    },
    Vertex {
        position: [0.5, -0.5],
        color: [0.0, 0.0, 1.0],
    },
];

// --- Phase 2: Draw uniform for per-entity data ---

/// Per-entity draw uniform data, padded to 256-byte alignment.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DrawUniforms {
    pub model_matrix: [[f32; 4]; 4],
    pub normal_matrix: [[f32; 4]; 4],
    pub base_color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
    pub _pad: [f32; 2],
    pub emission: [f32; 4],
    // Pad to 256 bytes total: 64+64+16+8+8+16 = 176, need 80 more bytes = 20 floats
    pub _padding: [f32; 20],
}

const DRAW_UNIFORM_SIZE: u64 = 256;
const MAX_ENTITIES: usize = 256;

/// Manages per-entity draw uniforms with dynamic offsets.
pub struct DrawUniformPool {
    pub buffer: wgpu::Buffer,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub bind_group: wgpu::BindGroup,
}

impl DrawUniformPool {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Draw Bind Group Layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(DRAW_UNIFORM_SIZE),
                    },
                    count: None,
                }],
            });

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Draw Uniform Buffer"),
            size: DRAW_UNIFORM_SIZE * MAX_ENTITIES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Draw Bind Group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(DRAW_UNIFORM_SIZE),
                }),
            }],
        });

        DrawUniformPool {
            buffer,
            bind_group_layout,
            bind_group,
        }
    }
}

// --- GPU State ---

/// GPU state created after the window is available.
pub struct GpuState {
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub render_pipeline: Option<wgpu::RenderPipeline>,
    pub vertex_buffer: wgpu::Buffer,
    // Phase 2: depth buffer
    pub depth_texture: wgpu::Texture,
    pub depth_view: wgpu::TextureView,
}

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Create a depth texture for the given dimensions.
pub fn create_depth_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Depth Texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Initialize the wgpu device, surface, and create the initial render pipeline.
pub async fn init_gpu(window: Arc<Window>, initial_wgsl: &str) -> GpuState {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let surface = instance
        .create_surface(Arc::clone(&window))
        .expect("Failed to create surface");

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await
        .expect("Failed to find suitable GPU adapter");

    let adapter_info = adapter.get_info();
    tracing::info!(
        "GPU adapter: {} ({:?})",
        adapter_info.name,
        adapter_info.backend
    );

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("nAIVE Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        )
        .await
        .expect("Failed to create device");

    let size = window.inner_size();
    let surface_caps = surface.get_capabilities(&adapter);
    let surface_format = surface_caps
        .formats
        .iter()
        .find(|f| f.is_srgb())
        .copied()
        .unwrap_or(surface_caps.formats[0]);

    tracing::info!("Surface format: {:?}", surface_format);

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: surface_caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Triangle Vertex Buffer"),
        contents: bytemuck::cast_slice(TRIANGLE_VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let render_pipeline = create_render_pipeline(&device, initial_wgsl, surface_format);

    let (depth_texture, depth_view) =
        create_depth_texture(&device, config.width, config.height);

    GpuState {
        window,
        surface,
        device,
        queue,
        config,
        render_pipeline: Some(render_pipeline),
        vertex_buffer,
        depth_texture,
        depth_view,
    }
}

/// Create the Phase 1 triangle render pipeline from WGSL source.
pub fn create_render_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Triangle Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Triangle Pipeline Layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Triangle Render Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[Vertex::desc()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// Create the Phase 2 forward render pipeline for 3D meshes.
pub fn create_forward_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    format: wgpu::TextureFormat,
    camera_bind_group_layout: &wgpu::BindGroupLayout,
    draw_bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Forward Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Forward Pipeline Layout"),
        bind_group_layouts: &[camera_bind_group_layout, draw_bind_group_layout],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Forward Render Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[Vertex3D::desc()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// Render one frame with the Phase 1 triangle (fallback when no scene loaded).
pub fn render(gpu: &GpuState) {
    let output = match gpu.surface.get_current_texture() {
        Ok(t) => t,
        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
            gpu.surface.configure(&gpu.device, &gpu.config);
            return;
        }
        Err(e) => {
            tracing::error!("Surface error: {:?}", e);
            return;
        }
    };

    let view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Render Encoder"),
        });

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Triangle Render Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.1,
                        g: 0.1,
                        b: 0.15,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        if let Some(pipeline) = &gpu.render_pipeline {
            render_pass.set_pipeline(pipeline);
            render_pass.set_vertex_buffer(0, gpu.vertex_buffer.slice(..));
            render_pass.draw(0..3, 0..1);
        }
    }

    gpu.queue.submit(std::iter::once(encoder.finish()));
    output.present();
}

/// Render one frame with 3D scene content.
pub fn render_scene(
    gpu: &GpuState,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    material_cache: &MaterialCache,
    forward_pipeline: &wgpu::RenderPipeline,
) {
    let output = match gpu.surface.get_current_texture() {
        Ok(t) => t,
        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
            gpu.surface.configure(&gpu.device, &gpu.config);
            return;
        }
        Err(e) => {
            tracing::error!("Surface error: {:?}", e);
            return;
        }
    };

    let view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    // Write per-draw uniforms
    for (draw_index, (_entity, (transform, mesh_renderer))) in
        (0_u32..).zip(scene_world.world.query::<(&Transform, &MeshRenderer)>().iter())
    {
        let material = material_cache.get(mesh_renderer.material_handle);
        let model_matrix = transform.world_matrix;
        let normal_matrix = model_matrix.inverse().transpose();

        let draw_uniform = DrawUniforms {
            model_matrix: model_matrix.to_cols_array_2d(),
            normal_matrix: normal_matrix.to_cols_array_2d(),
            base_color: material.uniform.base_color,
            roughness: material.uniform.roughness,
            metallic: material.uniform.metallic,
            _pad: [0.0; 2],
            emission: material.uniform.emission,
            _padding: [0.0; 20],
        };

        gpu.queue.write_buffer(
            &draw_pool.buffer,
            draw_index as u64 * DRAW_UNIFORM_SIZE,
            bytemuck::cast_slice(&[draw_uniform]),
        );
    }

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Scene Render Encoder"),
        });

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Forward Render Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.1,
                        g: 0.1,
                        b: 0.15,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &gpu.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(forward_pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);

        for (draw_index, (_entity, (_, mesh_renderer))) in
            (0_u32..).zip(scene_world.world.query::<(&Transform, &MeshRenderer)>().iter())
        {
            let gpu_mesh = mesh_cache.get(mesh_renderer.mesh_handle);
            let dynamic_offset = draw_index * DRAW_UNIFORM_SIZE as u32;

            render_pass.set_bind_group(1, &draw_pool.bind_group, &[dynamic_offset]);
            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                gpu_mesh.index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
        }
    }

    gpu.queue.submit(std::iter::once(encoder.finish()));
    output.present();
}
