use std::path::Path;

#[derive(Debug)]
pub enum ShaderError {
    SlangCompilationFailed(String),
    #[allow(dead_code)]
    FileNotFound(String),
    #[allow(dead_code)]
    IoError(std::io::Error),
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SlangCompilationFailed(msg) => write!(f, "SLANG compilation failed: {}", msg),
            Self::FileNotFound(path) => write!(f, "Shader file not found: {}", path),
            Self::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

/// Hardcoded WGSL fallback for the triangle shader.
pub fn get_triangle_wgsl() -> String {
    r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(model.position, 0.0, 1.0);
    out.color = model.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
"#
    .to_string()
}

/// Attempt to compile a SLANG file to WGSL.
/// Falls back to hardcoded WGSL on any failure.
pub fn compile_triangle_shader(slang_path: Option<&Path>) -> Result<String, ShaderError> {
    if let Some(path) = slang_path {
        if !path.exists() {
            tracing::warn!("SLANG file not found: {:?}, using fallback WGSL", path);
            return Ok(get_triangle_wgsl());
        }

        match compile_slang_to_wgsl(path) {
            Ok(wgsl) => {
                tracing::info!("SLANG compiled successfully: {:?}", path);
                return Ok(wgsl);
            }
            Err(e) => {
                tracing::warn!("SLANG compilation failed: {}, using fallback WGSL", e);
                return Ok(get_triangle_wgsl());
            }
        }
    }

    Ok(get_triangle_wgsl())
}

/// Actual SLANG-to-WGSL compilation using shader-slang crate.
#[cfg(feature = "slang")]
fn compile_slang_to_wgsl(path: &Path) -> Result<String, ShaderError> {
    use shader_slang as slang;
    use std::ffi::CString;

    let global_session = slang::GlobalSession::new().ok_or_else(|| {
        ShaderError::SlangCompilationFailed("Failed to create SLANG global session".to_string())
    })?;

    let search_path = path
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();
    let search_path_c = CString::new(search_path.as_str()).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Invalid search path: {:?}", e))
    })?;
    let search_paths_ptrs = [search_path_c.as_ptr()];

    let target_desc = slang::TargetDesc::default()
        .format(slang::CompileTarget::Wgsl);
    let targets = [target_desc];

    let session_desc = slang::SessionDesc::default()
        .targets(&targets)
        .search_paths(&search_paths_ptrs);

    let session = global_session
        .create_session(&session_desc)
        .ok_or_else(|| {
            ShaderError::SlangCompilationFailed("Failed to create SLANG session".to_string())
        })?;

    let file_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| ShaderError::SlangCompilationFailed("Invalid file name".to_string()))?;

    let module = session.load_module(file_name).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!(
            "Failed to load module '{}': {:?}",
            file_name, e
        ))
    })?;

    let vertex_entry = module.find_entry_point_by_name("vs_main").ok_or_else(|| {
        ShaderError::SlangCompilationFailed("Vertex entry point 'vs_main' not found".to_string())
    })?;

    let fragment_entry =
        module
            .find_entry_point_by_name("fs_main")
            .ok_or_else(|| {
                ShaderError::SlangCompilationFailed(
                    "Fragment entry point 'fs_main' not found".to_string(),
                )
            })?;

    use slang::Downcast;

    let program = session
        .create_composite_component_type(&[
            module.downcast().clone(),
            vertex_entry.downcast().clone(),
            fragment_entry.downcast().clone(),
        ])
        .map_err(|e| {
            ShaderError::SlangCompilationFailed(format!("Failed to compose program: {:?}", e))
        })?;

    let linked = program.link().map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Failed to link program: {:?}", e))
    })?;

    // Get the compiled WGSL for the whole target (target index 0)
    let code = linked.target_code(0).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Failed to get compiled code: {:?}", e))
    })?;

    code.as_str()
        .map(|s| s.to_string())
        .map_err(|e| {
            ShaderError::SlangCompilationFailed(format!("Invalid UTF-8 in WGSL output: {:?}", e))
        })
}

/// Fallback when SLANG feature is disabled.
#[cfg(not(feature = "slang"))]
fn compile_slang_to_wgsl(_path: &Path) -> Result<String, ShaderError> {
    Err(ShaderError::SlangCompilationFailed(
        "SLANG support not compiled in (feature 'slang' disabled)".to_string(),
    ))
}

/// Hardcoded WGSL fallback for the 3D forward mesh shader.
pub fn get_mesh_forward_wgsl() -> String {
    r#"
struct CameraUniform {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    view_projection: mat4x4<f32>,
    position: vec3<f32>,
    near_plane: f32,
    far_plane: f32,
    viewport_size: vec2<f32>,
    _padding: f32,
};

struct DrawUniforms {
    model_matrix: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    _pad: vec2<f32>,
    emission: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> draw: DrawUniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = draw.model_matrix * vec4<f32>(model.position, 1.0);
    out.clip_position = camera.view_projection * world_pos;
    out.world_normal = normalize((draw.normal_matrix * vec4<f32>(model.normal, 0.0)).xyz);
    out.world_pos = world_pos.xyz;
    out.tex_coords = model.tex_coords;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let light_dir = normalize(vec3<f32>(0.3, 1.0, 0.5));
    let ndotl = max(dot(in.world_normal, light_dir), 0.0);
    let ambient = vec3<f32>(0.15, 0.15, 0.18);
    let diffuse = draw.base_color.rgb * (ambient + ndotl * 0.85);
    let color = diffuse + draw.emission.rgb;
    return vec4<f32>(color, 1.0);
}
"#
    .to_string()
}

/// Attempt to compile the mesh forward shader, falling back to WGSL.
pub fn compile_mesh_forward_shader(slang_path: Option<&std::path::Path>) -> Result<String, ShaderError> {
    if let Some(path) = slang_path {
        if !path.exists() {
            tracing::warn!("Forward SLANG file not found: {:?}, using fallback WGSL", path);
            return Ok(get_mesh_forward_wgsl());
        }

        match compile_slang_to_wgsl(path) {
            Ok(wgsl) => {
                tracing::info!("Forward SLANG compiled successfully: {:?}", path);
                return Ok(wgsl);
            }
            Err(e) => {
                tracing::warn!("Forward SLANG compilation failed: {}, using fallback WGSL", e);
                return Ok(get_mesh_forward_wgsl());
            }
        }
    }

    Ok(get_mesh_forward_wgsl())
}
