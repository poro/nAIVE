use std::collections::HashMap;

use super::def::{PipelineError, ResourceDef};

// ---------------------------------------------------------------------------
// GPU resource types
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
