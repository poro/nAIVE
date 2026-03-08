pub mod def;
pub mod resource;
pub mod compiler;
pub mod executor;

use std::collections::HashMap;
use std::path::PathBuf;

// Re-export all public items so external code using `crate::pipeline::*` continues to work.
pub use def::*;
pub use resource::*;
pub use compiler::compile_pipeline;
pub use executor::{execute_pipeline, execute_pipeline_to_view, rebuild_bind_groups};

// ---------------------------------------------------------------------------
// Runtime render debug state (toggled interactively via number keys)
// ---------------------------------------------------------------------------

/// Runtime debug state for toggling render passes interactively.
#[derive(Debug, Clone)]
pub struct RenderDebugState {
    pub bloom_enabled: bool,
    pub point_lights_enabled: bool,
    pub emission_enabled: bool,
    pub torch_flicker_enabled: bool,
    pub show_hud: bool,
    /// Show physics collider wireframes (toggle with H key).
    pub show_colliders: bool,
    /// Multiplier for all light intensities (1.0 = normal, 10.0 = boosted)
    pub light_intensity_mult: f32,
    /// Override ambient light level (0.0 = use scene default)
    pub ambient_override: f32,
}

impl Default for RenderDebugState {
    fn default() -> Self {
        Self {
            bloom_enabled: true,
            point_lights_enabled: true,
            emission_enabled: true,
            torch_flicker_enabled: true,
            show_hud: false,
            show_colliders: false,
            light_intensity_mult: 1.0,
            ambient_override: 0.0,
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
    /// Skin matrix storage buffer for skeletal animation (shared, updated per-entity).
    pub skin_buffer: Option<wgpu::Buffer>,
    pub skin_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub skin_bind_group: Option<wgpu::BindGroup>,
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
