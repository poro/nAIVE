use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use wgpu::util::DeviceExt;

use crate::camera::CameraState;
use crate::components::{DirectionalLight, GaussianSplat, Hidden, MaterialOverride, MeshRenderer, PointLight, Transform};
use crate::material::MaterialCache;
use crate::mesh::{MeshCache, Vertex3D};
use crate::renderer::{DrawUniformPool, DrawUniforms, GpuState, DRAW_UNIFORM_SIZE};
use crate::splat::SplatCache;
use crate::world::SceneWorld;

// ---------------------------------------------------------------------------
// Pipeline YAML serde types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PipelineFile {
    pub version: u32,
    #[serde(default)]
    pub settings: PipelineSettings,
    #[serde(default)]
    pub resources: Vec<ResourceDef>,
    pub passes: Vec<PassDef>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PipelineSettings {
    #[serde(default = "default_resolution")]
    pub resolution: [u32; 2],
    #[serde(default = "default_true")]
    pub vsync: bool,
    #[serde(default = "default_60")]
    pub max_fps: u32,
    #[serde(default)]
    pub hdr: bool,
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            resolution: default_resolution(),
            vsync: true,
            max_fps: 60,
            hdr: false,
        }
    }
}

fn default_resolution() -> [u32; 2] {
    [1280, 720]
}
fn default_true() -> bool {
    true
}
fn default_60() -> u32 {
    60
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ResourceDef {
    pub name: String,
    #[serde(rename = "type")]
    pub resource_type: String,
    pub format: String,
    #[serde(default = "default_viewport")]
    pub size: String,
}

fn default_viewport() -> String {
    "viewport".to_string()
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PassDef {
    pub name: String,
    #[serde(rename = "type")]
    pub pass_type: String,
    pub shader: String,
    #[serde(default)]
    pub inputs: HashMap<String, String>,
    #[serde(default)]
    pub outputs: HashMap<String, String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub cull: Option<String>,
    #[serde(default)]
    pub dispatch: Option<String>,
}

// ---------------------------------------------------------------------------
// Pipeline YAML loading
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PipelineError {
    IoError(std::io::Error),
    ParseError(serde_yaml::Error),
    DagCycle(String),
    InvalidFormat(String),
    ShaderError(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "Pipeline IO error: {}", e),
            Self::ParseError(e) => write!(f, "Pipeline parse error: {}", e),
            Self::DagCycle(msg) => write!(f, "Pipeline DAG cycle: {}", msg),
            Self::InvalidFormat(msg) => write!(f, "Invalid format: {}", msg),
            Self::ShaderError(msg) => write!(f, "Shader error: {}", msg),
        }
    }
}

pub fn load_pipeline(path: &Path) -> Result<PipelineFile, PipelineError> {
    let contents = std::fs::read_to_string(path).map_err(PipelineError::IoError)?;
    let pipeline: PipelineFile =
        serde_yaml::from_str(&contents).map_err(PipelineError::ParseError)?;
    tracing::info!(
        "Loaded pipeline v{} with {} passes and {} resources",
        pipeline.version,
        pipeline.passes.len(),
        pipeline.resources.len()
    );
    Ok(pipeline)
}

// ---------------------------------------------------------------------------
// DAG builder — topological sort via Kahn's algorithm
// ---------------------------------------------------------------------------

/// Build an execution order for the passes using topological sort.
///
/// Dependencies are inferred from outputs → inputs: if pass B lists an input
/// whose value matches the name of a resource that pass A writes to, then
/// A must execute before B. Special values "auto" and "swapchain" are not
/// considered resources produced by other passes.
pub fn build_dag(passes: &[PassDef]) -> Result<Vec<usize>, PipelineError> {
    let n = passes.len();

    // Map: resource_name -> index of the pass that produces it
    let mut producer: HashMap<&str, usize> = HashMap::new();
    for (i, pass) in passes.iter().enumerate() {
        for resource_name in pass.outputs.values() {
            if resource_name != "swapchain" {
                producer.insert(resource_name.as_str(), i);
            }
        }
    }

    // Build adjacency list and in-degree counts
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, pass) in passes.iter().enumerate() {
        for input_resource in pass.inputs.values() {
            if input_resource == "auto" {
                continue;
            }
            if let Some(&producer_idx) = producer.get(input_resource.as_str()) {
                if producer_idx != i {
                    adj[producer_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }
    }

    // Kahn's algorithm
    let mut queue: Vec<usize> = Vec::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push(i);
        }
    }

    let mut order: Vec<usize> = Vec::with_capacity(n);
    while let Some(node) = queue.pop() {
        order.push(node);
        for &neighbor in &adj[node] {
            in_degree[neighbor] -= 1;
            if in_degree[neighbor] == 0 {
                queue.push(neighbor);
            }
        }
    }

    if order.len() != n {
        return Err(PipelineError::DagCycle(
            "Cycle detected in render pass dependencies".to_string(),
        ));
    }

    Ok(order)
}

// ---------------------------------------------------------------------------
// GPU resource types (used in later steps)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PassType {
    Rasterize,
    Fullscreen,
    Compute,
    Splat,
    Shadow,
}

impl PassType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "rasterize" => Some(Self::Rasterize),
            "fullscreen" => Some(Self::Fullscreen),
            "compute" => Some(Self::Compute),
            "splat" => Some(Self::Splat),
            "shadow" => Some(Self::Shadow),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ResourceSize {
    Viewport,
    /// Viewport divided by N (e.g., ViewportDiv(2) = half resolution)
    ViewportDiv(u32),
    Fixed(u32, u32),
}

/// Map format string from YAML to wgpu::TextureFormat.
pub fn format_from_string(s: &str) -> Result<wgpu::TextureFormat, PipelineError> {
    match s {
        "rgba8" => Ok(wgpu::TextureFormat::Rgba8Unorm),
        "rgba8unorm" => Ok(wgpu::TextureFormat::Rgba8Unorm),
        "rgb16f" | "rgba16f" => Ok(wgpu::TextureFormat::Rgba16Float),
        "rg16f" => Ok(wgpu::TextureFormat::Rg16Float),
        "r16f" => Ok(wgpu::TextureFormat::R16Float),
        "rgba32f" => Ok(wgpu::TextureFormat::Rgba32Float),
        "depth32f" => Ok(wgpu::TextureFormat::Depth32Float),
        "depth24plus" => Ok(wgpu::TextureFormat::Depth24Plus),
        _ => Err(PipelineError::InvalidFormat(format!(
            "Unknown texture format: '{}'",
            s
        ))),
    }
}

/// Parse a size string: "viewport", "viewport/2", or "[width, height]".
pub fn parse_resource_size(s: &str) -> ResourceSize {
    if s == "viewport" {
        return ResourceSize::Viewport;
    }
    // Support "viewport/N" for fractional viewport sizes
    if let Some(divisor_str) = s.strip_prefix("viewport/") {
        if let Ok(divisor) = divisor_str.trim().parse::<u32>() {
            if divisor > 0 {
                return ResourceSize::ViewportDiv(divisor);
            }
        }
    }
    // Try parsing "[w, h]" or "w,h"
    let trimmed = s.trim_matches(|c| c == '[' || c == ']');
    let parts: Vec<&str> = trimmed.split(',').map(|p| p.trim()).collect();
    if parts.len() == 2 {
        if let (Ok(w), Ok(h)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return ResourceSize::Fixed(w, h);
        }
    }
    ResourceSize::Viewport
}

// ---------------------------------------------------------------------------
// GPU resource allocation
// ---------------------------------------------------------------------------

/// A GPU texture resource allocated by the pipeline.
pub struct GpuResource {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub size: ResourceSize,
    pub name: String,
}

/// Allocate all pipeline resources as GPU textures.
pub fn allocate_resources(
    device: &wgpu::Device,
    resource_defs: &[ResourceDef],
    viewport_width: u32,
    viewport_height: u32,
) -> Result<HashMap<String, GpuResource>, PipelineError> {
    let mut resources = HashMap::new();

    for def in resource_defs {
        let format = format_from_string(&def.format)?;
        let size = parse_resource_size(&def.size);
        let (width, height) = match size {
            ResourceSize::Viewport => (viewport_width, viewport_height),
            ResourceSize::ViewportDiv(d) => (viewport_width / d, viewport_height / d),
            ResourceSize::Fixed(w, h) => (w, h),
        };

        let usage =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&def.name),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        tracing::debug!("Allocated pipeline resource '{}': {:?} {}x{}", def.name, format, width, height);

        resources.insert(
            def.name.clone(),
            GpuResource {
                texture,
                view,
                format,
                size,
                name: def.name.clone(),
            },
        );
    }

    Ok(resources)
}

/// Recreate all viewport-sized resources after a window resize.
pub fn resize_resources(
    device: &wgpu::Device,
    resources: &mut HashMap<String, GpuResource>,
    new_width: u32,
    new_height: u32,
) {
    for resource in resources.values_mut() {
        let (w, h) = match resource.size {
            ResourceSize::Viewport => (new_width, new_height),
            ResourceSize::ViewportDiv(d) => (new_width / d, new_height / d),
            ResourceSize::Fixed(_, _) => continue,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&resource.name),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: resource.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        resource.texture = texture;
        resource.view = view;
        tracing::debug!(
            "Resized pipeline resource '{}': {}x{}",
            resource.name,
            w,
            h
        );
    }
}

// ---------------------------------------------------------------------------
// Light uniforms for deferred lighting
// ---------------------------------------------------------------------------

/// Per-point-light data sent to the GPU.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PointLightUniform {
    pub position: [f32; 3],
    pub range: f32,
    pub color: [f32; 3],
    pub intensity: f32,
}

/// Shadow pass uniforms (light view-projection matrix).
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowUniforms {
    pub light_view_projection: [[f32; 4]; 4],
}

/// Light data buffer header + array.
pub const MAX_LIGHTS: usize = 32;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightingUniforms {
    pub light_count: u32,
    pub has_directional: u32,
    pub _pad: [u32; 6],
    // Directional light fields (offset 32)
    pub dir_light_direction: [f32; 3],
    pub dir_light_intensity: f32,
    pub dir_light_color: [f32; 3],
    pub _pad2: f32,
    // Shadow light VP matrix (offset 64)
    pub light_vp: [[f32; 4]; 4],
    // Point lights (offset 128)
    pub lights: [PointLightUniform; MAX_LIGHTS],
}

impl Default for LightingUniforms {
    fn default() -> Self {
        Self {
            light_count: 0,
            has_directional: 0,
            _pad: [0; 6],
            dir_light_direction: [0.0; 3],
            dir_light_intensity: 0.0,
            dir_light_color: [1.0, 1.0, 1.0],
            _pad2: 0.0,
            light_vp: [[0.0; 4]; 4],
            lights: [PointLightUniform {
                position: [0.0; 3],
                range: 0.0,
                color: [0.0; 3],
                intensity: 0.0,
            }; MAX_LIGHTS],
        }
    }
}

// ---------------------------------------------------------------------------
// Compiled pipeline types
// ---------------------------------------------------------------------------

/// A compiled render pipeline ready for execution.
#[allow(dead_code)]
pub struct CompiledPipeline {
    pub resources: HashMap<String, GpuResource>,
    pub passes: Vec<CompiledPass>,
    pub pass_order: Vec<usize>,
    pub light_buffer: wgpu::Buffer,
    pub light_bind_group_layout: wgpu::BindGroupLayout,
    pub light_bind_group: wgpu::BindGroup,
    pub gbuffer_sampler: wgpu::Sampler,
    pub gbuffer_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub gbuffer_bind_group: Option<wgpu::BindGroup>,
    pub tonemap_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub tonemap_bind_group: Option<wgpu::BindGroup>,
    /// Bloom pass bind group (reads HDR buffer).
    pub bloom_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub bloom_bind_group: Option<wgpu::BindGroup>,
    /// Bind group layout for splat data (storage buffers).
    pub splat_data_bind_group_layout: Option<wgpu::BindGroupLayout>,
    /// Bind group layout + bind group for splat compositing in lighting pass.
    pub splat_composite_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub splat_composite_bind_group: Option<wgpu::BindGroup>,
    /// FXAA pass bind group (reads LDR buffer).
    pub fxaa_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub fxaa_bind_group: Option<wgpu::BindGroup>,
    /// Shadow map resources.
    pub shadow_uniform_buffer: Option<wgpu::Buffer>,
    pub shadow_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub shadow_bind_group: Option<wgpu::BindGroup>,
    /// Shadow map sampler (comparison) for lighting pass.
    pub shadow_sampler: Option<wgpu::Sampler>,
}

/// A single compiled render pass.
#[allow(dead_code)]
pub struct CompiledPass {
    pub name: String,
    pub pass_type: PassType,
    pub pipeline: wgpu::RenderPipeline,
    pub color_targets: Vec<String>,
    pub depth_target: Option<String>,
    pub wgsl_source: String,
    pub shader_path: PathBuf,
}

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
) -> Result<CompiledPipeline, PipelineError> {
    // 1. Build DAG and get execution order
    let pass_order = build_dag(&pipeline_file.passes)?;

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

    // 5. Compile each pass
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
        // Alphabetical sort would put emission before normal — this custom sort prevents that.
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
    })
}

/// Compile a pass shader: try SLANG, fallback to WGSL.
fn compile_pass_shader(shader_path: &Path, pass_name: &str) -> Result<String, PipelineError> {
    if shader_path.exists() {
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
) -> wgpu::RenderPipeline {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("GBuffer Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("GBuffer Pipeline Layout"),
        bind_group_layouts: &[camera_bind_group_layout, draw_bind_group_layout],
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
            buffers: &[], // No vertex buffer — generated in shader
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

/// Create the tonemap fullscreen pipeline.
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
) -> wgpu::RenderPipeline {
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shadow Depth Shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Shadow Pipeline Layout"),
        bind_group_layouts: &[shadow_bind_group_layout, draw_bind_group_layout],
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

// ---------------------------------------------------------------------------
// Pipeline executor
// ---------------------------------------------------------------------------

/// Execute the compiled pipeline for one frame.
#[allow(clippy::too_many_arguments)]
pub fn execute_pipeline(
    gpu: &GpuState,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    material_cache: &MaterialCache,
    splat_cache: &SplatCache,
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

    let swapchain_view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = execute_pipeline_to_view(
        gpu, compiled, scene_world, camera_state, draw_pool,
        mesh_cache, material_cache, splat_cache, &swapchain_view,
    );

    gpu.queue.submit(std::iter::once(encoder.finish()));
    output.present();
}

/// Execute the compiled multi-pass pipeline, returning the encoder for further passes.
pub fn execute_pipeline_to_view(
    gpu: &GpuState,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    material_cache: &MaterialCache,
    splat_cache: &SplatCache,
    swapchain_view: &wgpu::TextureView,
) -> wgpu::CommandEncoder {

    // DEBUG: dump camera VP matrix and first entities' transforms (frame 0 only)
    {
        static DEBUG_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !DEBUG_ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let cam = &camera_state.uniform;
            let vp = cam.view_projection;
            tracing::warn!(
                "Camera pos=({:.2},{:.2},{:.2}) near={} far={}",
                cam.position[0], cam.position[1], cam.position[2],
                cam.near_plane, cam.far_plane,
            );
            tracing::warn!("VP col0=[{:.4},{:.4},{:.4},{:.4}]", vp[0][0], vp[0][1], vp[0][2], vp[0][3]);
            tracing::warn!("VP col1=[{:.4},{:.4},{:.4},{:.4}]", vp[1][0], vp[1][1], vp[1][2], vp[1][3]);
            tracing::warn!("VP col2=[{:.4},{:.4},{:.4},{:.4}]", vp[2][0], vp[2][1], vp[2][2], vp[2][3]);
            tracing::warn!("VP col3=[{:.4},{:.4},{:.4},{:.4}]", vp[3][0], vp[3][1], vp[3][2], vp[3][3]);
            let entity_count = scene_world.world.query::<(&Transform, &MeshRenderer)>().iter().count();
            tracing::warn!("Entities with MeshRenderer: {}", entity_count);
            for (i, (_e, (t, _mr))) in (0u32..).zip(
                scene_world.world.query::<(&Transform, &MeshRenderer)>().iter()
            ) {
                if i < 3 {
                    tracing::warn!(
                        "  Entity {}: pos=({:.2},{:.2},{:.2}) scale=({:.2},{:.2},{:.2})",
                        i, t.position.x, t.position.y, t.position.z,
                        t.scale.x, t.scale.y, t.scale.z,
                    );
                }
            }
        }
    }

    // Upload per-entity draw uniforms
    for (draw_index, (entity, (transform, mesh_renderer))) in
        (0_u32..).zip(scene_world.world.query::<(&Transform, &MeshRenderer)>().iter())
    {
        if scene_world.world.get::<&Hidden>(entity).is_ok() {
            continue;
        }
        let material = material_cache.get(mesh_renderer.material_handle);
        let model_matrix = transform.world_matrix;
        let normal_matrix = model_matrix.inverse().transpose();

        // Apply runtime material overrides from Lua scripts
        let mat_override = scene_world.world.get::<&MaterialOverride>(entity).ok();
        let roughness = mat_override
            .as_ref()
            .and_then(|o| o.roughness)
            .unwrap_or(material.uniform.roughness);
        let metallic = mat_override
            .as_ref()
            .and_then(|o| o.metallic)
            .unwrap_or(material.uniform.metallic);
        let emission = mat_override
            .as_ref()
            .and_then(|o| o.emission)
            .map(|e| [e[0], e[1], e[2], 0.0])
            .unwrap_or(material.uniform.emission);

        let draw_uniform = DrawUniforms {
            model_matrix: model_matrix.to_cols_array_2d(),
            normal_matrix: normal_matrix.to_cols_array_2d(),
            base_color: material.uniform.base_color,
            roughness,
            metallic,
            _pad: [0.0; 2],
            emission,
            _padding: [0.0; 20],
        };

        gpu.queue.write_buffer(
            &draw_pool.buffer,
            draw_index as u64 * DRAW_UNIFORM_SIZE,
            bytemuck::cast_slice(&[draw_uniform]),
        );
    }

    // Upload light uniforms (point lights + directional light)
    let mut light_data = LightingUniforms::default();
    for (_entity, (transform, light)) in
        scene_world.world.query::<(&Transform, &PointLight)>().iter()
    {
        if (light_data.light_count as usize) < MAX_LIGHTS {
            let idx = light_data.light_count as usize;
            light_data.lights[idx] = PointLightUniform {
                position: transform.position.to_array(),
                range: light.range,
                color: light.color.to_array(),
                intensity: light.intensity,
            };
            light_data.light_count += 1;
        }
    }

    // Query directional light and compute shadow VP matrix
    let mut light_vp = glam::Mat4::IDENTITY;
    for (_entity, dir_light) in
        scene_world.world.query::<&DirectionalLight>().iter()
    {
        light_data.has_directional = 1;
        light_data.dir_light_direction = dir_light.direction.to_array();
        light_data.dir_light_intensity = dir_light.intensity;
        light_data.dir_light_color = dir_light.color.to_array();

        // Compute orthographic VP from light direction
        let extent = dir_light.shadow_extent;
        let light_pos = -dir_light.direction.normalize() * 30.0;
        let light_view = glam::Mat4::look_at_rh(light_pos, glam::Vec3::ZERO, glam::Vec3::Y);
        let light_proj = glam::Mat4::orthographic_rh(
            -extent, extent, -extent, extent, 0.1, 60.0,
        );
        light_vp = light_proj * light_view;
        light_data.light_vp = light_vp.to_cols_array_2d();
        break; // Only one directional light supported
    }

    gpu.queue.write_buffer(
        &compiled.light_buffer,
        0,
        bytemuck::cast_slice(&[light_data]),
    );

    // Upload shadow uniform buffer (light VP matrix for shadow pass)
    if let Some(shadow_buf) = &compiled.shadow_uniform_buffer {
        let shadow_data = ShadowUniforms {
            light_view_projection: light_vp.to_cols_array_2d(),
        };
        gpu.queue.write_buffer(
            shadow_buf,
            0,
            bytemuck::cast_slice(&[shadow_data]),
        );
    }

    // Create command encoder
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Pipeline Render Encoder"),
        });

    // Execute passes in topological order
    for &pass_idx in &compiled.pass_order {
        let pass = &compiled.passes[pass_idx];

        match pass.pass_type {
            PassType::Rasterize => {
                execute_rasterize_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    scene_world,
                    camera_state,
                    draw_pool,
                    mesh_cache,
                );
            }
            PassType::Fullscreen => {
                execute_fullscreen_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    camera_state,
                    &swapchain_view,
                );
            }
            PassType::Splat => {
                execute_splat_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    &gpu.device,
                    scene_world,
                    camera_state,
                    splat_cache,
                );
            }
            PassType::Shadow => {
                execute_shadow_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    scene_world,
                    draw_pool,
                    mesh_cache,
                );
            }
            PassType::Compute => {
                // Not implemented yet
            }
        }
    }

    encoder
}

/// Execute a shadow depth pass (renders all geometry from light's perspective).
fn execute_shadow_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
) {
    let depth_view = pass
        .depth_target
        .as_ref()
        .and_then(|name| compiled.resources.get(name))
        .map(|r| &r.view)
        .expect("Shadow pass has no depth target");

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);

        // Group 0: shadow uniforms (light VP matrix)
        if let Some(bg) = &compiled.shadow_bind_group {
            render_pass.set_bind_group(0, bg, &[]);
        }

        // Draw all mesh entities
        for (draw_index, (entity, (_, mesh_renderer))) in
            (0_u32..).zip(scene_world.world.query::<(&Transform, &MeshRenderer)>().iter())
        {
            if scene_world.world.get::<&Hidden>(entity).is_ok() {
                continue;
            }
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
}

/// Execute a rasterize pass (G-buffer geometry pass).
fn execute_rasterize_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
) {
    // Build color attachments from pass targets
    let color_views: Vec<&wgpu::TextureView> = pass
        .color_targets
        .iter()
        .filter_map(|name| compiled.resources.get(name).map(|r| &r.view))
        .collect();

    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = color_views
        .iter()
        .map(|view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();

    let depth_view = pass
        .depth_target
        .as_ref()
        .and_then(|name| compiled.resources.get(name))
        .map(|r| &r.view);

    let depth_attachment = depth_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    });

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);

        let mut draw_count = 0u32;
        for (draw_index, (entity, (_, mesh_renderer))) in
            (0_u32..).zip(scene_world.world.query::<(&Transform, &MeshRenderer)>().iter())
        {
            if scene_world.world.get::<&Hidden>(entity).is_ok() {
                continue;
            }
            let gpu_mesh = mesh_cache.get(mesh_renderer.mesh_handle);
            let dynamic_offset = draw_index * DRAW_UNIFORM_SIZE as u32;

            render_pass.set_bind_group(1, &draw_pool.bind_group, &[dynamic_offset]);
            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                gpu_mesh.index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
            draw_count += 1;
        }
        if draw_count == 0 {
            tracing::warn!("Rasterize pass '{}': ZERO entities drawn!", pass.name);
        } else {
            tracing::debug!("Rasterize pass '{}': {} entities drawn", pass.name, draw_count);
        }
    }
}

/// Execute a Gaussian splat rendering pass.
fn execute_splat_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    device: &wgpu::Device,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    splat_cache: &SplatCache,
) {
    // Build color attachments
    let color_views: Vec<&wgpu::TextureView> = pass
        .color_targets
        .iter()
        .filter_map(|name| compiled.resources.get(name).map(|r| &r.view))
        .collect();

    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = color_views
        .iter()
        .map(|view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();

    let depth_view = pass
        .depth_target
        .as_ref()
        .and_then(|name| compiled.resources.get(name))
        .map(|r| &r.view);

    let depth_attachment = depth_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    });

    // Get the splat data bind group layout
    let splat_layout = match &compiled.splat_data_bind_group_layout {
        Some(layout) => layout,
        None => return,
    };

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);

        // For each entity with a GaussianSplat component, create a bind group and draw
        for (_entity, splat) in scene_world.world.query::<&GaussianSplat>().iter() {
            let gpu_splat = splat_cache.get(splat.splat_handle);
            if gpu_splat.splat_count == 0 {
                continue;
            }

            // Create bind group for this splat's data
            let splat_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Splat Data Bind Group"),
                layout: splat_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: gpu_splat.splat_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: gpu_splat.sorted_index_buffer.as_entire_binding(),
                    },
                ],
            });

            render_pass.set_bind_group(1, &splat_bind_group, &[]);
            // 6 vertices per quad, N instances (one per splat)
            render_pass.draw(0..6, 0..gpu_splat.splat_count);
        }
    }
}

/// Execute a fullscreen pass (lighting or tonemap).
fn execute_fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    camera_state: &CameraState,
    swapchain_view: &wgpu::TextureView,
) {
    let is_tonemap = pass.name.contains("tonemap");
    let is_bloom = pass.name.contains("bloom");
    let is_fxaa = pass.name.contains("fxaa");
    let writes_to_swapchain = pass
        .color_targets
        .iter()
        .any(|t| t == "swapchain");

    // Determine the output view
    let output_view = if writes_to_swapchain {
        swapchain_view
    } else {
        pass.color_targets
            .first()
            .and_then(|name| compiled.resources.get(name))
            .map(|r| &r.view)
            .expect("Fullscreen pass has no output target")
    };

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);

        if is_fxaa {
            // FXAA: group 0 = LDR texture + sampler
            if let Some(bg) = &compiled.fxaa_bind_group {
                render_pass.set_bind_group(0, bg, &[]);
            }
        } else if is_tonemap {
            // Tonemap: group 0 = HDR texture + sampler + bloom texture
            if let Some(bg) = &compiled.tonemap_bind_group {
                render_pass.set_bind_group(0, bg, &[]);
            }
        } else if is_bloom {
            // Bloom: group 0 = HDR texture + sampler
            if let Some(bg) = &compiled.bloom_bind_group {
                render_pass.set_bind_group(0, bg, &[]);
            }
        } else {
            // Lighting: group 0 = camera, group 1 = G-buffer textures, group 2 = lights
            render_pass.set_bind_group(0, &camera_state.bind_group, &[]);
            if let Some(bg) = &compiled.gbuffer_bind_group {
                render_pass.set_bind_group(1, bg, &[]);
            }
            render_pass.set_bind_group(2, &compiled.light_bind_group, &[]);
            // Group 3: splat composite textures (if available)
            if let Some(bg) = &compiled.splat_composite_bind_group {
                render_pass.set_bind_group(3, bg, &[]);
            }
        }

        // Draw fullscreen triangle (3 vertices, no vertex buffer)
        render_pass.draw(0..3, 0..1);
    }
}

/// Rebuild bind groups after resources are resized.
/// Call this after `resize_resources()` to update texture view references.
pub fn rebuild_bind_groups(
    device: &wgpu::Device,
    compiled: &mut CompiledPipeline,
) {
    // Rebuild G-buffer bind group
    if let Some(layout) = &compiled.gbuffer_bind_group_layout {
        let albedo_view = compiled
            .resources
            .get("gbuffer_albedo")
            .map(|r| &r.view);
        let normal_view = compiled
            .resources
            .get("gbuffer_normal")
            .map(|r| &r.view);
        let depth_view = compiled
            .resources
            .get("gbuffer_depth")
            .map(|r| &r.view);
        let emission_view = compiled
            .resources
            .get("gbuffer_emission")
            .map(|r| &r.view);

        if let (Some(albedo), Some(normal), Some(depth)) = (albedo_view, normal_view, depth_view) {
            let emission = emission_view.unwrap_or(albedo);
            compiled.gbuffer_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
                    label: Some("GBuffer Input Bind Group (resized)"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(albedo),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(normal),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(depth),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&compiled.gbuffer_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(emission),
                        },
                    ],
                },
            ));
        }
    }

    // Rebuild bloom bind group
    if let Some(layout) = &compiled.bloom_bind_group_layout {
        if let Some(hdr_view) = compiled.resources.get("hdr_buffer").map(|r| &r.view) {
            let hdr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Bloom HDR Sampler (resized)"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            compiled.bloom_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
                    label: Some("Bloom Input Bind Group (resized)"),
                    layout,
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
                },
            ));
        }
    }

    // Rebuild tonemap bind group (HDR + bloom)
    if let Some(layout) = &compiled.tonemap_bind_group_layout {
        if let Some(hdr_view) = compiled.resources.get("hdr_buffer").map(|r| &r.view) {
            let hdr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("HDR Sampler (resized)"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            let bloom_view = compiled.resources.get("bloom_buffer")
                .map(|r| &r.view)
                .unwrap_or(hdr_view);

            compiled.tonemap_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
                    label: Some("Tonemap Input Bind Group (resized)"),
                    layout,
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
                },
            ));
        }
    }

    // Rebuild FXAA bind group
    if let Some(layout) = &compiled.fxaa_bind_group_layout {
        if let Some(ldr_view) = compiled.resources.get("ldr_buffer").map(|r| &r.view) {
            let ldr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("FXAA LDR Sampler (resized)"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            compiled.fxaa_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
                    label: Some("FXAA Input Bind Group (resized)"),
                    layout,
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
                },
            ));
        }
    }

    // Rebuild splat composite bind group
    if let Some(layout) = &compiled.splat_composite_bind_group_layout {
        let splat_color = compiled.resources.get("splat_color").map(|r| &r.view);
        let splat_depth = compiled.resources.get("splat_depth").map(|r| &r.view);

        if let (Some(color_view), Some(depth_view)) = (splat_color, splat_depth) {
            compiled.splat_composite_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
                    label: Some("Splat Composite Bind Group (resized)"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(color_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(depth_view),
                        },
                    ],
                },
            ));
        }
    }

    // Rebuild lighting bind group (shadow map may have been resized)
    if let Some(sampler) = &compiled.shadow_sampler {
        // Create dummy shadow map fallback
        let shadow_dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Dummy Shadow Map (resized)"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_dummy_view = shadow_dummy_tex.create_view(&Default::default());
        let shadow_map_view = compiled.resources.get("shadow_map")
            .map(|r| &r.view)
            .unwrap_or(&shadow_dummy_view);

        compiled.light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Lighting Bind Group (resized)"),
            layout: &compiled.light_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: compiled.light_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(shadow_map_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pipeline_yaml() {
        let yaml = r#"
version: 1
settings:
  vsync: true
  hdr: true

resources:
  - name: gbuffer_albedo
    type: texture_2d
    format: rgba8
    size: viewport
  - name: gbuffer_normal
    type: texture_2d
    format: rgba16f
    size: viewport
  - name: gbuffer_depth
    type: texture_2d
    format: depth32f
    size: viewport
  - name: hdr_buffer
    type: texture_2d
    format: rgba16f
    size: viewport

passes:
  - name: geometry_pass
    type: rasterize
    shader: shaders/passes/gbuffer.slang
    inputs:
      scene_meshes: auto
      scene_materials: auto
    outputs:
      color: gbuffer_albedo
      normal: gbuffer_normal
      depth: gbuffer_depth

  - name: lighting_pass
    type: fullscreen
    shader: shaders/passes/deferred_light.slang
    inputs:
      gbuffer_albedo: gbuffer_albedo
      gbuffer_normal: gbuffer_normal
      gbuffer_depth: gbuffer_depth
      scene_lights: auto
    outputs:
      color: hdr_buffer

  - name: tonemap_pass
    type: fullscreen
    shader: shaders/passes/tonemap.slang
    inputs:
      hdr: hdr_buffer
    outputs:
      color: swapchain
"#;

        let pipeline: PipelineFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pipeline.version, 1);
        assert!(pipeline.settings.hdr);
        assert!(pipeline.settings.vsync);
        assert_eq!(pipeline.resources.len(), 4);
        assert_eq!(pipeline.passes.len(), 3);

        assert_eq!(pipeline.passes[0].name, "geometry_pass");
        assert_eq!(pipeline.passes[0].pass_type, "rasterize");
        assert_eq!(pipeline.passes[1].name, "lighting_pass");
        assert_eq!(pipeline.passes[1].pass_type, "fullscreen");
        assert_eq!(pipeline.passes[2].name, "tonemap_pass");
    }

    #[test]
    fn test_dag_order() {
        let yaml = r#"
version: 1
passes:
  - name: geometry_pass
    type: rasterize
    shader: gbuffer.slang
    inputs:
      scene_meshes: auto
    outputs:
      color: gbuffer_albedo
      normal: gbuffer_normal
      depth: gbuffer_depth

  - name: lighting_pass
    type: fullscreen
    shader: deferred_light.slang
    inputs:
      gbuffer_albedo: gbuffer_albedo
      gbuffer_normal: gbuffer_normal
      gbuffer_depth: gbuffer_depth
    outputs:
      color: hdr_buffer

  - name: tonemap_pass
    type: fullscreen
    shader: tonemap.slang
    inputs:
      hdr: hdr_buffer
    outputs:
      color: swapchain
"#;

        let pipeline: PipelineFile = serde_yaml::from_str(yaml).unwrap();
        let order = build_dag(&pipeline.passes).unwrap();

        // geometry_pass (0) must come before lighting_pass (1)
        // lighting_pass (1) must come before tonemap_pass (2)
        let pos_geom = order.iter().position(|&x| x == 0).unwrap();
        let pos_light = order.iter().position(|&x| x == 1).unwrap();
        let pos_tone = order.iter().position(|&x| x == 2).unwrap();

        assert!(pos_geom < pos_light, "geometry must precede lighting");
        assert!(pos_light < pos_tone, "lighting must precede tonemap");
    }

    #[test]
    fn test_dag_cycle_detection() {
        // Create a cycle: A outputs x, B reads x and outputs y, A reads y
        let passes = vec![
            PassDef {
                name: "pass_a".to_string(),
                pass_type: "fullscreen".to_string(),
                shader: "a.slang".to_string(),
                inputs: [("in1".to_string(), "resource_y".to_string())]
                    .into_iter()
                    .collect(),
                outputs: [("color".to_string(), "resource_x".to_string())]
                    .into_iter()
                    .collect(),
                sort: None,
                cull: None,
                dispatch: None,
            },
            PassDef {
                name: "pass_b".to_string(),
                pass_type: "fullscreen".to_string(),
                shader: "b.slang".to_string(),
                inputs: [("in1".to_string(), "resource_x".to_string())]
                    .into_iter()
                    .collect(),
                outputs: [("color".to_string(), "resource_y".to_string())]
                    .into_iter()
                    .collect(),
                sort: None,
                cull: None,
                dispatch: None,
            },
        ];

        let result = build_dag(&passes);
        assert!(result.is_err());
        match result {
            Err(PipelineError::DagCycle(_)) => {} // expected
            other => panic!("Expected DagCycle error, got: {:?}", other),
        }
    }

    #[test]
    fn test_format_from_string() {
        assert_eq!(
            format_from_string("rgba8").unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert_eq!(
            format_from_string("rgba16f").unwrap(),
            wgpu::TextureFormat::Rgba16Float
        );
        assert_eq!(
            format_from_string("depth32f").unwrap(),
            wgpu::TextureFormat::Depth32Float
        );
        assert!(format_from_string("unknown").is_err());
    }

    #[test]
    fn test_parse_resource_size() {
        match parse_resource_size("viewport") {
            ResourceSize::Viewport => {}
            other => panic!("Expected Viewport, got: {:?}", other),
        }
        match parse_resource_size("viewport/2") {
            ResourceSize::ViewportDiv(2) => {}
            other => panic!("Expected ViewportDiv(2), got: {:?}", other),
        }
        match parse_resource_size("viewport/4") {
            ResourceSize::ViewportDiv(4) => {}
            other => panic!("Expected ViewportDiv(4), got: {:?}", other),
        }
        match parse_resource_size("[1920, 1080]") {
            ResourceSize::Fixed(1920, 1080) => {}
            other => panic!("Expected Fixed(1920, 1080), got: {:?}", other),
        }
    }

    #[test]
    fn test_pass_type_from_str() {
        assert_eq!(PassType::from_str("rasterize"), Some(PassType::Rasterize));
        assert_eq!(PassType::from_str("fullscreen"), Some(PassType::Fullscreen));
        assert_eq!(PassType::from_str("compute"), Some(PassType::Compute));
        assert_eq!(PassType::from_str("splat"), Some(PassType::Splat));
        assert_eq!(PassType::from_str("shadow"), Some(PassType::Shadow));
        assert_eq!(PassType::from_str("invalid"), None);
    }
}
