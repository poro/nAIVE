use std::collections::HashMap;
use std::path::Path;

use wgpu::util::DeviceExt;

use crate::camera::CameraState;
use crate::mesh::Vertex3D;
use crate::renderer::DrawUniformPool;

use super::def::{PipelineError, PipelineFile};
use super::resource::{
    allocate_resources, GpuResource, LightingUniforms, PassType,
    ShadowUniforms,
};
use super::{CompiledPass, CompiledPipeline};

// ---------------------------------------------------------------------------
// Pipeline compiler
// ---------------------------------------------------------------------------

/// Compile a pipeline from YAML definition into GPU objects.
#[allow(clippy::too_many_arguments)]
pub fn compile_pipeline(
    device: &wgpu::Device,
    pipeline_file: &PipelineFile,
    project_root: &Path,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    surface_format: wgpu::TextureFormat,
    viewport_width: u32,
    viewport_height: u32,
    texture_bind_group_layout: Option<&wgpu::BindGroupLayout>,
) -> Result<CompiledPipeline, PipelineError> {
    // 1. Build DAG and get execution order
    let pass_order = super::def::build_dag(&pipeline_file.passes)?;

    // 2. Allocate GPU resources
    let resources = allocate_resources(device, &pipeline_file.resources, viewport_width, viewport_height)?;

    // 3. Create light uniform buffer
    let light_uniform = LightingUniforms::default();
    let light_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Lighting Uniform Buffer"),
        contents: bytemuck::cast_slice(&[light_uniform]),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    // Create shadow comparison sampler for lighting pass
    let shadow_cmp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Shadow Comparison Sampler"),
        compare: Some(wgpu::CompareFunction::LessEqual),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // Create a 1x1 dummy depth texture for when shadow_map is not present
    let shadow_dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Dummy Shadow Map"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let shadow_dummy_view = shadow_dummy_tex.create_view(&Default::default());
    let shadow_map_view = resources.get("shadow_map")
        .map(|r| &r.view)
        .unwrap_or(&shadow_dummy_view);

    let light_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Lighting Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });

    let light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Lighting Bind Group"),
        layout: &light_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: light_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(shadow_map_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&shadow_cmp_sampler),
            },
        ],
    });

    // 4. Create shared sampler for G-buffer reads
    let gbuffer_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("GBuffer Sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    // 5. Create skin matrix storage buffer for skeletal animation
    let skin_palette_size = std::mem::size_of::<crate::anim_system::BoneMatrixPalette>() as u64;
    let skin_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Skin Matrix Storage Buffer"),
        size: skin_palette_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let skin_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Skin Bind Group Layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let skin_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Skin Bind Group"),
        layout: &skin_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: skin_buffer.as_entire_binding(),
        }],
    });

    // 6. Compile each pass
    let mut compiled_passes = Vec::new();
    let mut gbuffer_bind_group_layout = None;
    let mut gbuffer_bind_group = None;
    let mut tonemap_bind_group_layout = None;
    let mut tonemap_bind_group = None;
    let mut bloom_bind_group_layout = None;
    let mut bloom_bind_group = None;
    let mut splat_data_bind_group_layout = None;
    let mut splat_composite_bind_group_layout = None;
    let mut splat_composite_bind_group = None;
    let mut fxaa_bind_group_layout = None;
    let mut fxaa_bind_group = None;
    let mut shadow_uniform_buffer = None;
    let mut shadow_bind_group_layout = None;
    let mut shadow_bind_group = None;
    let shadow_sampler = Some(shadow_cmp_sampler);

    for pass_def in &pipeline_file.passes {
        let pass_type = PassType::from_str(&pass_def.pass_type).ok_or_else(|| {
            PipelineError::InvalidFormat(format!("Unknown pass type: '{}'", pass_def.pass_type))
        })?;

        // Determine color and depth targets
        let mut color_targets: Vec<String> = Vec::new();
        let mut depth_target: Option<String> = None;

        for (output_slot, resource_name) in &pass_def.outputs {
            if output_slot == "depth" {
                depth_target = Some(resource_name.clone());
            } else {
                color_targets.push(resource_name.clone());
            }
        }

        // Sort color targets by MRT slot priority to match shader @location indices.
        // The G-buffer shader outputs: @location(0)=color, @location(1)=normal, @location(2)=emission.
        // Alphabetical sort would put emission before normal -- this custom sort prevents that.
        let mut output_pairs: Vec<(&String, &String)> = pass_def
            .outputs
            .iter()
            .filter(|(k, _)| *k != "depth")
            .collect();
        output_pairs.sort_by_key(|(k, _)| match k.as_str() {
            "color" | "albedo" => 0,
            "normal" => 1,
            "emission" => 2,
            other => 3 + other.len() as u32, // unknown slots come last, stable
        });
        color_targets = output_pairs.iter().map(|(_, v)| v.to_string()).collect();

        // Compile the shader (try SLANG first, then fallback)
        let shader_path = project_root.join(&pass_def.shader);
        let wgsl_source = compile_pass_shader(&shader_path, &pass_def.name)?;

        // Create the render pipeline for this pass
        let pipeline = match pass_type {
            PassType::Rasterize => {
                create_rasterize_pipeline(
                    device,
                    &wgsl_source,
                    &color_targets,
                    depth_target.as_deref(),
                    &resources,
                    &camera_state.bind_group_layout,
                    &draw_pool.bind_group_layout,
                    texture_bind_group_layout,
                    Some(&skin_bind_group_layout),
                )
            }
            PassType::Fullscreen => {
                if pass_def.name.contains("tonemap") {
                    // Tonemap pass: outputs to ldr_buffer (rgba8) if it exists, else swapchain
                    let tonemap_output_format = resources
                        .get("ldr_buffer")
                        .map(|r| r.format)
                        .unwrap_or(surface_format);
                    let (layout, bg, pipeline) = create_tonemap_pipeline(
                        device,
                        &wgsl_source,
                        &color_targets,
                        &resources,
                        &gbuffer_sampler,
                        tonemap_output_format,
                    );
                    tonemap_bind_group_layout = Some(layout);
                    tonemap_bind_group = Some(bg);
                    pipeline
                } else if pass_def.name.contains("fxaa") {
                    // FXAA pass: reads LDR buffer, writes to swapchain
                    let (layout, bg, pipeline) = create_fxaa_pipeline(
                        device,
                        &wgsl_source,
                        &resources,
                        surface_format,
                    );
                    fxaa_bind_group_layout = Some(layout);
                    fxaa_bind_group = Some(bg);
                    pipeline
                } else if pass_def.name.contains("bloom") {
                    // Bloom pass: reads HDR buffer, outputs to bloom_buffer
                    let (layout, bg, pipeline) = create_bloom_pipeline(
                        device,
                        &wgsl_source,
                        &color_targets,
                        &resources,
                        &gbuffer_sampler,
                    );
                    bloom_bind_group_layout = Some(layout);
                    bloom_bind_group = Some(bg);
                    pipeline
                } else {
                    // Lighting pass: inputs from G-buffer
                    let has_splat_resources = resources.contains_key("splat_color")
                        && resources.contains_key("splat_depth");

                    // Use splat-compositing shader if splat resources exist
                    let lighting_wgsl = if has_splat_resources {
                        crate::shader::get_deferred_light_with_splats_wgsl()
                    } else {
                        wgsl_source.clone()
                    };

                    if has_splat_resources {
                        let (gb_layout, gb_bg, sc_layout, sc_bg, pipeline) =
                            create_lighting_pipeline_with_splats(
                                device,
                                &lighting_wgsl,
                                &color_targets,
                                &resources,
                                &camera_state.bind_group_layout,
                                &gbuffer_sampler,
                                &light_bind_group_layout,
                            );
                        gbuffer_bind_group_layout = Some(gb_layout);
                        gbuffer_bind_group = Some(gb_bg);
                        splat_composite_bind_group_layout = Some(sc_layout);
                        splat_composite_bind_group = Some(sc_bg);
                        pipeline
                    } else {
                        let (layout, bg, pipeline) = create_lighting_pipeline(
                            device,
                            &wgsl_source,
                            &color_targets,
                            &resources,
                            &camera_state.bind_group_layout,
                            &gbuffer_sampler,
                            &light_bind_group_layout,
                        );
                        gbuffer_bind_group_layout = Some(layout);
                        gbuffer_bind_group = Some(bg);
                        pipeline
                    }
                }
            }
            PassType::Splat => {
                let (layout, pipeline) = create_splat_pipeline(
                    device,
                    &wgsl_source,
                    &color_targets,
                    depth_target.as_deref(),
                    &resources,
                    &camera_state.bind_group_layout,
                );
                splat_data_bind_group_layout = Some(layout);
                pipeline
            }
            PassType::Shadow => {
                // Create shadow pass resources
                let shadow_uniform_data = ShadowUniforms {
                    light_view_projection: [[0.0; 4]; 4],
                };
                let shadow_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Shadow Uniform Buffer"),
                    contents: bytemuck::cast_slice(&[shadow_uniform_data]),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });

                let shadow_bg_layout = device.create_bind_group_layout(
                    &wgpu::BindGroupLayoutDescriptor {
                        label: Some("Shadow Pass Bind Group Layout"),
                        entries: &[wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        }],
                    },
                );

                let shadow_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Shadow Pass Bind Group"),
                    layout: &shadow_bg_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: shadow_buf.as_entire_binding(),
                    }],
                });

                let pipeline = create_shadow_pipeline(
                    device,
                    &wgsl_source,
                    depth_target.as_deref(),
                    &resources,
                    &shadow_bg_layout,
                    &draw_pool.bind_group_layout,
                    &skin_bind_group_layout,
                );

                shadow_uniform_buffer = Some(shadow_buf);
                shadow_bind_group_layout = Some(shadow_bg_layout);
                shadow_bind_group = Some(shadow_bg);

                pipeline
            }
            PassType::Compute => {
                // Compute passes not yet implemented
                return Err(PipelineError::InvalidFormat(
                    "Compute passes not yet supported".to_string(),
                ));
            }
        };

        compiled_passes.push(CompiledPass {
            name: pass_def.name.clone(),
            pass_type,
            pipeline,
            color_targets,
            depth_target,
            wgsl_source,
            shader_path,
        });
    }

    tracing::info!(
        "Pipeline compiled: {} passes, {} resources",
        compiled_passes.len(),
        resources.len()
    );

    Ok(CompiledPipeline {
        resources,
        passes: compiled_passes,
        pass_order,
        light_buffer,
        light_bind_group_layout,
        light_bind_group,
        gbuffer_sampler,
        gbuffer_bind_group_layout,
        gbuffer_bind_group,
        tonemap_bind_group_layout,
        tonemap_bind_group,
        bloom_bind_group_layout,
        bloom_bind_group,
        splat_data_bind_group_layout,
        splat_composite_bind_group_layout,
        splat_composite_bind_group,
        fxaa_bind_group_layout,
        fxaa_bind_group,
        shadow_uniform_buffer,
        shadow_bind_group_layout,
        shadow_bind_group,
        shadow_sampler,
        skin_buffer: Some(skin_buffer),
        skin_bind_group_layout: Some(skin_bind_group_layout),
        skin_bind_group: Some(skin_bind_group),
    })
}

/// Compile a pass shader: try SLANG, fallback to WGSL.
fn compile_pass_shader(shader_path: &Path, pass_name: &str) -> Result<String, PipelineError> {
    // Skip SLANG for geometry pass -- SLANG-compiled WGSL doesn't support dynamic-offset UBOs correctly
    let skip_slang = pass_name.contains("geometry") || pass_name.contains("gbuffer");
    if shader_path.exists() && !skip_slang {
        match crate::shader::compile_slang_to_wgsl_public(shader_path) {
            Ok(wgsl) => {
                tracing::info!("SLANG compiled for pass '{}': {:?}", pass_name, shader_path);
                return Ok(wgsl);
            }
            Err(e) => {
                tracing::warn!(
                    "SLANG compilation failed for pass '{}': {}, using WGSL fallback",
                    pass_name,
                    e
                );
            }
        }
    }

    // Use fallback WGSL
    let wgsl = match pass_name {
        name if name.contains("geometry") || name.contains("gbuffer") => {
            crate::shader::get_gbuffer_wgsl()
        }
        name if name.contains("splat") && !name.contains("light") => {
            crate::shader::get_splat_render_wgsl()
        }
        name if name.contains("light") => crate::shader::get_deferred_light_wgsl(),
        name if name.contains("bloom") => crate::shader::get_bloom_wgsl(),
        name if name.contains("tonemap") => crate::shader::get_tonemap_wgsl(),
        name if name.contains("fxaa") => crate::shader::get_fxaa_wgsl(),
        name if name.contains("shadow") => crate::shader::get_shadow_depth_wgsl(),
        _ => {
            return Err(PipelineError::ShaderError(format!(
                "No fallback WGSL for pass '{}'",
                pass_name
            )));
        }
    };
    Ok(wgsl)
}

/// Create a rasterize (geometry) pipeline with MRT outputs.
fn create_rasterize_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    color_targets: &[String],
    depth_target: Option<&str>,
    resources: &HashMap<String, GpuResource>,
    camera_bind_group_layout: &wgpu::BindGroupLayout,
    draw_bind_group_layout: &wgpu::BindGroupLayout,
    texture_bind_group_layout: Option<&wgpu::BindGroupLayout>,
    skin_bind_group_layout: Option<&wgpu::BindGroupLayout>,
) -> wgpu::RenderPipeline {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("GBuffer Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let mut layouts: Vec<&wgpu::BindGroupLayout> = vec![camera_bind_group_layout, draw_bind_group_layout];
    if let Some(tex_layout) = texture_bind_group_layout {
        layouts.push(tex_layout);
    }
    if let Some(skin_layout) = skin_bind_group_layout {
        layouts.push(skin_layout);
    }

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("GBuffer Pipeline Layout"),
        bind_group_layouts: &layouts,
        push_constant_ranges: &[],
    });

    // Build color targets from resource formats
    let color_target_states: Vec<Option<wgpu::ColorTargetState>> = color_targets
        .iter()
        .map(|name| {
            let format = resources
                .get(name)
                .map(|r| r.format)
                .unwrap_or(wgpu::TextureFormat::Rgba8UnormSrgb);
            Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })
        })
        .collect();

    let depth_format = depth_target.and_then(|name| resources.get(name)).map(|r| r.format);

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("GBuffer Render Pipeline"),
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
            targets: &color_target_states,
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
        depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
            format,
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

/// Create the Gaussian splat rendering pipeline.
/// Returns (splat_data_bind_group_layout, pipeline).
fn create_splat_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    color_targets: &[String],
    depth_target: Option<&str>,
    resources: &HashMap<String, GpuResource>,
    camera_bind_group_layout: &wgpu::BindGroupLayout,
) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline) {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Gaussian Splat Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    // Group 1: splat data + sorted indices (created per-entity at render time)
    let splat_data_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Splat Data Bind Group Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Splat Pipeline Layout"),
        bind_group_layouts: &[camera_bind_group_layout, &splat_data_layout],
        push_constant_ranges: &[],
    });

    // Color targets
    let color_target_states: Vec<Option<wgpu::ColorTargetState>> = color_targets
        .iter()
        .map(|name| {
            let format = resources
                .get(name)
                .map(|r| r.format)
                .unwrap_or(wgpu::TextureFormat::Rgba16Float);
            Some(wgpu::ColorTargetState {
                format,
                // Premultiplied alpha blending for splats
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })
        })
        .collect();

    let depth_format = depth_target.and_then(|name| resources.get(name)).map(|r| r.format);

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Gaussian Splat Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[], // No vertex buffer -- generated in shader
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &color_target_states,
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None, // Billboards are double-sided
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (splat_data_layout, pipeline)
}

/// Create the deferred lighting fullscreen pipeline.
fn create_lighting_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    color_targets: &[String],
    resources: &HashMap<String, GpuResource>,
    camera_bind_group_layout: &wgpu::BindGroupLayout,
    gbuffer_sampler: &wgpu::Sampler,
    light_bind_group_layout: &wgpu::BindGroupLayout,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup, wgpu::RenderPipeline) {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Deferred Lighting Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    // Group 1: G-buffer textures + sampler + emission
    let gbuffer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("GBuffer Input Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });

    // Build the bind group referencing the G-buffer textures
    let albedo_view = resources
        .get("gbuffer_albedo")
        .map(|r| &r.view)
        .expect("gbuffer_albedo resource missing");
    let normal_view = resources
        .get("gbuffer_normal")
        .map(|r| &r.view)
        .expect("gbuffer_normal resource missing");
    let depth_view = resources
        .get("gbuffer_depth")
        .map(|r| &r.view)
        .expect("gbuffer_depth resource missing");
    let emission_view = resources
        .get("gbuffer_emission")
        .map(|r| &r.view)
        .unwrap_or(albedo_view); // fallback to albedo if emission not present

    let gbuffer_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("GBuffer Input Bind Group"),
        layout: &gbuffer_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(albedo_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(normal_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(depth_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(gbuffer_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(emission_view),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Deferred Lighting Pipeline Layout"),
        bind_group_layouts: &[
            camera_bind_group_layout,
            &gbuffer_layout,
            light_bind_group_layout,
        ],
        push_constant_ranges: &[],
    });

    let output_format = color_targets
        .first()
        .and_then(|name| resources.get(name))
        .map(|r| r.format)
        .unwrap_or(wgpu::TextureFormat::Rgba16Float);

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Deferred Lighting Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[], // Fullscreen triangle - no vertex buffer
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (gbuffer_layout, gbuffer_bind_group, pipeline)
}

/// Create the deferred lighting pipeline with splat compositing.
/// Returns (gbuffer_layout, gbuffer_bg, splat_composite_layout, splat_composite_bg, pipeline).
fn create_lighting_pipeline_with_splats(
    device: &wgpu::Device,
    wgsl_source: &str,
    color_targets: &[String],
    resources: &HashMap<String, GpuResource>,
    camera_bind_group_layout: &wgpu::BindGroupLayout,
    gbuffer_sampler: &wgpu::Sampler,
    light_bind_group_layout: &wgpu::BindGroupLayout,
) -> (
    wgpu::BindGroupLayout,
    wgpu::BindGroup,
    wgpu::BindGroupLayout,
    wgpu::BindGroup,
    wgpu::RenderPipeline,
) {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Deferred Lighting + Splat Composite Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    // Group 1: G-buffer textures + sampler + emission (same as non-splat version)
    let gbuffer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("GBuffer Input Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });

    let albedo_view = &resources.get("gbuffer_albedo").expect("gbuffer_albedo missing").view;
    let normal_view = &resources.get("gbuffer_normal").expect("gbuffer_normal missing").view;
    let gbuffer_depth_view = &resources.get("gbuffer_depth").expect("gbuffer_depth missing").view;
    let emission_view = resources
        .get("gbuffer_emission")
        .map(|r| &r.view)
        .unwrap_or(albedo_view);

    let gbuffer_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("GBuffer Input Bind Group"),
        layout: &gbuffer_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(albedo_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(normal_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(gbuffer_depth_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(gbuffer_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(emission_view),
            },
        ],
    });

    // Group 3: splat composite textures (splat_color + splat_depth)
    let splat_composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Splat Composite Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });

    let splat_color_view = &resources.get("splat_color").expect("splat_color missing").view;
    let splat_depth_view = &resources.get("splat_depth").expect("splat_depth missing").view;

    let splat_composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Splat Composite Bind Group"),
        layout: &splat_composite_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(splat_color_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(splat_depth_view),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Deferred Lighting + Splat Pipeline Layout"),
        bind_group_layouts: &[
            camera_bind_group_layout,
            &gbuffer_layout,
            light_bind_group_layout,
            &splat_composite_layout,
        ],
        push_constant_ranges: &[],
    });

    let output_format = color_targets
        .first()
        .and_then(|name| resources.get(name))
        .map(|r| r.format)
        .unwrap_or(wgpu::TextureFormat::Rgba16Float);

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Deferred Lighting + Splat Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (
        gbuffer_layout,
        gbuffer_bind_group,
        splat_composite_layout,
        splat_composite_bind_group,
        pipeline,
    )
}

/// Create a bloom extraction pipeline: reads HDR buffer, outputs to half-res bloom buffer.
fn create_bloom_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    color_targets: &[String],
    resources: &HashMap<String, GpuResource>,
    _gbuffer_sampler: &wgpu::Sampler,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup, wgpu::RenderPipeline) {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Bloom Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let hdr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Bloom HDR Sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Bloom Input Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let hdr_view = resources
        .get("hdr_buffer")
        .map(|r| &r.view)
        .expect("hdr_buffer resource missing for bloom");

    let bloom_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Bloom Input Bind Group"),
        layout: &bloom_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(hdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&hdr_sampler),
            },
        ],
    });

    // Output format from the bloom_buffer resource
    let output_format = color_targets
        .first()
        .and_then(|name| resources.get(name))
        .map(|r| r.format)
        .unwrap_or(wgpu::TextureFormat::Rgba16Float);

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Bloom Pipeline Layout"),
        bind_group_layouts: &[&bloom_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Bloom Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (bloom_layout, bloom_bind_group, pipeline)
}

fn create_tonemap_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    _color_targets: &[String],
    resources: &HashMap<String, GpuResource>,
    _gbuffer_sampler: &wgpu::Sampler,
    surface_format: wgpu::TextureFormat,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup, wgpu::RenderPipeline) {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Tonemap Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    // Create a filtering sampler for tonemap (reads HDR buffer with filtering)
    let hdr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("HDR Sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // Group 0: HDR texture + sampler + bloom texture
    let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Tonemap Input Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });

    let hdr_view = resources
        .get("hdr_buffer")
        .map(|r| &r.view)
        .expect("hdr_buffer resource missing");

    // Bloom buffer (fall back to HDR view if bloom not present yet)
    let bloom_view = resources
        .get("bloom_buffer")
        .map(|r| &r.view)
        .unwrap_or(hdr_view);

    let tonemap_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Tonemap Input Bind Group"),
        layout: &tonemap_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(hdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&hdr_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(bloom_view),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Tonemap Pipeline Layout"),
        bind_group_layouts: &[&tonemap_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Tonemap Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[], // Fullscreen triangle
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (tonemap_layout, tonemap_bind_group, pipeline)
}

/// Create the FXAA post-processing pipeline (reads LDR buffer, writes to swapchain).
fn create_fxaa_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    resources: &HashMap<String, GpuResource>,
    surface_format: wgpu::TextureFormat,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup, wgpu::RenderPipeline) {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("FXAA Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let ldr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("FXAA LDR Sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let fxaa_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("FXAA Input Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let ldr_view = resources
        .get("ldr_buffer")
        .map(|r| &r.view)
        .expect("ldr_buffer resource missing for FXAA pass");

    let fxaa_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("FXAA Input Bind Group"),
        layout: &fxaa_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(ldr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&ldr_sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("FXAA Pipeline Layout"),
        bind_group_layouts: &[&fxaa_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("FXAA Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (fxaa_layout, fxaa_bind_group, pipeline)
}

/// Create a shadow depth rendering pipeline.
fn create_shadow_pipeline(
    device: &wgpu::Device,
    wgsl_source: &str,
    depth_target: Option<&str>,
    resources: &HashMap<String, GpuResource>,
    shadow_bind_group_layout: &wgpu::BindGroupLayout,
    draw_bind_group_layout: &wgpu::BindGroupLayout,
    skin_bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shadow Depth Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Shadow Pipeline Layout"),
        bind_group_layouts: &[shadow_bind_group_layout, draw_bind_group_layout, skin_bind_group_layout],
        push_constant_ranges: &[],
    });

    let depth_format = depth_target
        .and_then(|name| resources.get(name))
        .map(|r| r.format)
        .unwrap_or(wgpu::TextureFormat::Depth32Float);

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Shadow Pipeline"),
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
            targets: &[],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: depth_format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 2.0,
                clamp: 0.0,
            },
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}
