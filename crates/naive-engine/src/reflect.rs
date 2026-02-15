//! SLANG reflection -> wgpu bind group layout mapping.
//!
//! This module extracts reflection data from compiled SLANG programs and
//! maps them to wgpu bind group layout entries. Used by the pipeline
//! compiler to auto-generate bind group layouts from shader source.

use crate::shader::ShaderError;

/// Reflection data extracted from a compiled shader.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ShaderReflection {
    pub bind_group_layouts: Vec<BindGroupLayoutInfo>,
    pub entry_points: Vec<EntryPointInfo>,
}

/// Info about one bind group (one @group(N) in WGSL).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BindGroupLayoutInfo {
    pub group: u32,
    pub entries: Vec<wgpu::BindGroupLayoutEntry>,
}

/// Info about one shader entry point.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EntryPointInfo {
    pub name: String,
    pub stage: wgpu::ShaderStages,
    pub workgroup_size: Option<[u64; 3]>,
}

/// Extract reflection data from a linked SLANG program.
///
/// Uses `linked.layout(0)` to get `&reflection::Shader`, then iterates
/// parameters to build bind group layout entries.
#[allow(dead_code)]
#[cfg(feature = "slang")]
pub fn reflect_shader(
    linked: &shader_slang::ComponentType,
) -> Result<ShaderReflection, ShaderError> {
    use std::collections::HashMap;

    let layout = linked.layout(0).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!(
            "Failed to get program layout for reflection: {:?}",
            e
        ))
    })?;

    // Collect bind group entries grouped by binding space (= @group)
    let mut groups: HashMap<u32, Vec<wgpu::BindGroupLayoutEntry>> = HashMap::new();

    let param_count = layout.parameter_count();
    for i in 0..param_count {
        if let Some(param) = layout.parameter_by_index(i) {
            let binding_index = param.binding_index();
            let binding_space = param.binding_space();
            let type_layout = param.type_layout();
            let binding_type = slang_type_to_wgpu_binding(type_layout);

            // Default visibility: VERTEX | FRAGMENT (refined later if needed)
            let visibility = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;

            let entry = wgpu::BindGroupLayoutEntry {
                binding: binding_index,
                visibility,
                ty: binding_type,
                count: None,
            };

            groups.entry(binding_space).or_default().push(entry);
        }
    }

    // Sort entries within each group by binding index
    let mut bind_group_layouts: Vec<BindGroupLayoutInfo> = groups
        .into_iter()
        .map(|(group, mut entries)| {
            entries.sort_by_key(|e| e.binding);
            BindGroupLayoutInfo { group, entries }
        })
        .collect();
    bind_group_layouts.sort_by_key(|bg| bg.group);

    // Extract entry points
    let mut entry_points = Vec::new();
    let ep_count = layout.entry_point_count();
    for i in 0..ep_count {
        if let Some(ep) = layout.entry_point_by_index(i) {
            let name = ep.name().to_string();
            let stage = match ep.stage() {
                shader_slang::Stage::Vertex => wgpu::ShaderStages::VERTEX,
                shader_slang::Stage::Fragment => wgpu::ShaderStages::FRAGMENT,
                shader_slang::Stage::Compute => wgpu::ShaderStages::COMPUTE,
                _ => wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            };
            let workgroup_size = if stage == wgpu::ShaderStages::COMPUTE {
                Some(ep.compute_thread_group_size())
            } else {
                None
            };
            entry_points.push(EntryPointInfo {
                name,
                stage,
                workgroup_size,
            });
        }
    }

    Ok(ShaderReflection {
        bind_group_layouts,
        entry_points,
    })
}

/// Map Slang type layout kind to wgpu::BindingType.
#[cfg(feature = "slang")]
fn slang_type_to_wgpu_binding(
    type_layout: &shader_slang::reflection::TypeLayout,
) -> wgpu::BindingType {
    use shader_slang::TypeKind;

    match type_layout.kind() {
        TypeKind::ConstantBuffer => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        TypeKind::Resource => {
            // Check resource shape to distinguish textures from buffers
            match type_layout.resource_shape() {
                Some(shader_slang::ResourceShape::SlangTexture2d) => {
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    }
                }
                Some(shader_slang::ResourceShape::SlangTextureCube) => {
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::Cube,
                        multisampled: false,
                    }
                }
                Some(shader_slang::ResourceShape::SlangTexture3d) => {
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    }
                }
                Some(shader_slang::ResourceShape::SlangStructuredBuffer) => {
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    }
                }
                _ => {
                    // Default to storage buffer for other resource types
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    }
                }
            }
        }
        TypeKind::SamplerState => {
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
        }
        TypeKind::ParameterBlock => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        _ => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    }
}

/// Fallback when `slang` feature is disabled: returns empty reflection.
#[cfg(not(feature = "slang"))]
pub fn reflect_shader(
    _linked: &(),
) -> Result<ShaderReflection, ShaderError> {
    Ok(ShaderReflection {
        bind_group_layouts: Vec::new(),
        entry_points: Vec::new(),
    })
}
