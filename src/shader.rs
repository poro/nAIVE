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

/// Public SLANG-to-WGSL compilation for use by the pipeline module.
pub fn compile_slang_to_wgsl_public(path: &Path) -> Result<String, ShaderError> {
    compile_slang_to_wgsl(path)
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

/// Compile a SLANG file with flexible entry points and return both WGSL and reflection data.
#[allow(dead_code)]
#[cfg(feature = "slang")]
pub fn compile_shader(
    path: &Path,
    entry_points: &[(&str, shader_slang::Stage)],
) -> Result<(String, crate::reflect::ShaderReflection), ShaderError> {
    use shader_slang as slang;
    use std::ffi::CString;

    let global_session = slang::GlobalSession::new().ok_or_else(|| {
        ShaderError::SlangCompilationFailed("Failed to create SLANG global session".to_string())
    })?;

    // Add both the shader's parent directory and the project-relative modules directory
    let shader_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();

    // Also add the shaders/modules directory for shared imports
    let modules_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .parent()
        .map(|p| p.join("modules"))
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let shader_dir_c = CString::new(shader_dir.as_str()).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Invalid search path: {:?}", e))
    })?;
    let modules_dir_c = CString::new(modules_dir.as_str()).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Invalid search path: {:?}", e))
    })?;
    let search_paths_ptrs = [shader_dir_c.as_ptr(), modules_dir_c.as_ptr()];

    let target_desc = slang::TargetDesc::default().format(slang::CompileTarget::Wgsl);
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

    // Find all requested entry points
    use slang::Downcast;
    let mut components: Vec<slang::ComponentType> = vec![module.downcast().clone()];

    for (ep_name, _stage) in entry_points {
        let entry = module
            .find_entry_point_by_name(ep_name)
            .ok_or_else(|| {
                ShaderError::SlangCompilationFailed(format!(
                    "Entry point '{}' not found in {}",
                    ep_name, file_name
                ))
            })?;
        components.push(entry.downcast().clone());
    }

    let program = session
        .create_composite_component_type(&components)
        .map_err(|e| {
            ShaderError::SlangCompilationFailed(format!("Failed to compose program: {:?}", e))
        })?;

    let linked = program.link().map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Failed to link program: {:?}", e))
    })?;

    // Extract reflection data
    let reflection = crate::reflect::reflect_shader(&linked)?;

    // Get the compiled WGSL
    let code = linked.target_code(0).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Failed to get compiled code: {:?}", e))
    })?;

    let wgsl = code
        .as_str()
        .map(|s| s.to_string())
        .map_err(|e| {
            ShaderError::SlangCompilationFailed(format!("Invalid UTF-8 in WGSL output: {:?}", e))
        })?;

    Ok((wgsl, reflection))
}

/// Fallback when SLANG feature is disabled.
#[allow(dead_code)]
#[cfg(not(feature = "slang"))]
pub fn compile_shader(
    _path: &Path,
    _entry_points: &[(&str, ())],
) -> Result<(String, crate::reflect::ShaderReflection), ShaderError> {
    Err(ShaderError::SlangCompilationFailed(
        "SLANG support not compiled in (feature 'slang' disabled)".to_string(),
    ))
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

/// Hardcoded WGSL fallback for the G-buffer pass.
pub fn get_gbuffer_wgsl() -> String {
    r#"
struct CameraUniform {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    view_projection: mat4x4<f32>,
    position: vec3<f32>,
    near_plane: f32,
    far_plane: f32,
    _pad1: f32,
    viewport_size: vec2<f32>,
    _padding: f32,
    _pad2: vec3<f32>,
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

struct GBufferOutput {
    @location(0) albedo: vec4<f32>,
    @location(1) normal: vec4<f32>,
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
fn fs_main(in: VertexOutput) -> GBufferOutput {
    var out: GBufferOutput;
    out.albedo = draw.base_color;
    out.normal = vec4<f32>(in.world_normal * 0.5 + 0.5, 1.0);
    return out;
}
"#
    .to_string()
}

/// Hardcoded WGSL fallback for the deferred lighting pass.
pub fn get_deferred_light_wgsl() -> String {
    r#"
struct CameraUniform {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    view_projection: mat4x4<f32>,
    position: vec3<f32>,
    near_plane: f32,
    far_plane: f32,
    _pad1: f32,
    viewport_size: vec2<f32>,
    _pad2: vec4<f32>,
    inv_view_projection: mat4x4<f32>,
};

struct PointLight {
    position: vec3<f32>,
    range: f32,
    color: vec3<f32>,
    intensity: f32,
};

struct LightingUniforms {
    light_count: u32,
    _pad: vec3<u32>,
    lights: array<PointLight, 32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

@group(1) @binding(0) var gbuffer_albedo: texture_2d<f32>;
@group(1) @binding(1) var gbuffer_normal: texture_2d<f32>;
@group(1) @binding(2) var gbuffer_depth: texture_depth_2d;
@group(1) @binding(3) var gbuffer_sampler: sampler;

@group(2) @binding(0) var<uniform> lighting: LightingUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let uv = vec2<f32>(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    out.position = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

fn reconstruct_world_pos(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    // Convert UV [0,1] to clip space [-1,1], flip Y for NDC
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth, 1.0);
    let world_h = camera.inv_view_projection * clip;
    return world_h.xyz / world_h.w;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_coords = vec2<i32>(in.position.xy);
    let albedo = textureLoad(gbuffer_albedo, tex_coords, 0).rgb;
    let normal_raw = textureLoad(gbuffer_normal, tex_coords, 0).rgb;
    let normal = normalize(normal_raw * 2.0 - 1.0);
    let depth = textureLoad(gbuffer_depth, tex_coords, 0);

    // Skip empty pixels (depth at 1.0 = far plane)
    if depth >= 1.0 {
        discard;
    }

    // Reconstruct world position from depth
    let uv = in.uv;
    let world_pos = reconstruct_world_pos(uv, depth);

    // Ambient light
    var color = albedo * vec3<f32>(0.08, 0.08, 0.1);

    // Accumulate point lights with proper attenuation
    for (var i = 0u; i < lighting.light_count; i = i + 1u) {
        let light = lighting.lights[i];
        let to_light = light.position - world_pos;
        let dist = length(to_light);

        // Range-based cutoff
        if dist > light.range {
            continue;
        }

        let light_dir = to_light / dist;
        let ndotl = max(dot(normal, light_dir), 0.0);

        // Inverse-square falloff with range attenuation
        let dist_atten = 1.0 / (1.0 + dist * dist);
        let range_factor = saturate(1.0 - pow(dist / light.range, 4.0));
        let attenuation = light.intensity * dist_atten * range_factor;

        color = color + albedo * light.color * ndotl * attenuation;
    }

    // If no lights, use a default directional light
    if lighting.light_count == 0u {
        let light_dir = normalize(vec3<f32>(0.3, 1.0, 0.5));
        let ndotl = max(dot(normal, light_dir), 0.0);
        color = albedo * (vec3<f32>(0.15, 0.15, 0.18) + ndotl * 0.85);
    }

    return vec4<f32>(color, 1.0);
}
"#
    .to_string()
}

/// Hardcoded WGSL for the Gaussian splat rendering pass.
/// Renders each splat as a camera-facing billboard quad with 2D Gaussian falloff.
pub fn get_splat_render_wgsl() -> String {
    r#"
struct CameraUniform {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    view_projection: mat4x4<f32>,
    position: vec3<f32>,
    near_plane: f32,
    far_plane: f32,
    _pad1: f32,
    viewport_size: vec2<f32>,
    _padding: f32,
    _pad2: vec3<f32>,
};

struct GaussianSplat {
    position: vec3<f32>,
    opacity: f32,
    scale: vec3<f32>,
    _pad0: f32,
    rotation: vec4<f32>,
    sh_dc: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

@group(1) @binding(0) var<storage, read> splats: array<GaussianSplat>;
@group(1) @binding(1) var<storage, read> sorted_indices: array<u32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) opacity: f32,
};

// Build a rotation matrix from a quaternion
fn quat_to_mat3(q: vec4<f32>) -> mat3x3<f32> {
    let x = q.x; let y = q.y; let z = q.z; let w = q.w;
    let x2 = x + x; let y2 = y + y; let z2 = z + z;
    let xx = x * x2; let xy = x * y2; let xz = x * z2;
    let yy = y * y2; let yz = y * z2; let zz = z * z2;
    let wx = w * x2; let wy = w * y2; let wz = w * z2;
    return mat3x3<f32>(
        vec3<f32>(1.0 - (yy + zz), xy + wz, xz - wy),
        vec3<f32>(xy - wz, 1.0 - (xx + zz), yz + wx),
        vec3<f32>(xz + wy, yz - wx, 1.0 - (xx + yy)),
    );
}

@vertex
fn vs_main(
    @builtin(instance_index) instance_index: u32,
    @builtin(vertex_index) vertex_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    // Look up the sorted splat index
    let splat_idx = sorted_indices[instance_index];
    let splat = splats[splat_idx];

    // Quad vertices: two triangles forming a quad [-1,1] x [-1,1]
    // Triangle 1: (0,1,2) = (-1,-1), (1,-1), (1,1)
    // Triangle 2: (3,4,5) = (-1,-1), (1,1), (-1,1)
    var quad_pos: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );

    let uv = quad_pos[vertex_index];
    out.uv = uv;
    out.color = splat.sh_dc;
    out.opacity = splat.opacity;

    // Compute the 3D covariance axes from rotation and scale
    let rot_mat = quat_to_mat3(splat.rotation);
    let scaled_x = rot_mat[0] * splat.scale.x;
    let scaled_y = rot_mat[1] * splat.scale.y;

    // Billboard: offset the splat center in world space along the covariance axes
    // Use 2x scale for the quad extent (covers ~95% of Gaussian at 2 sigma)
    let world_offset = scaled_x * uv.x * 2.0 + scaled_y * uv.y * 2.0;
    let world_pos = splat.position + world_offset;

    out.clip_position = camera.view_projection * vec4<f32>(world_pos, 1.0);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // 2D Gaussian falloff: exp(-0.5 * |uv|^2)
    let d = dot(in.uv, in.uv);
    if d > 4.0 {
        discard;
    }
    let alpha = in.opacity * exp(-0.5 * d);

    // Threshold very transparent fragments
    if alpha < 0.004 {
        discard;
    }

    return vec4<f32>(in.color * alpha, alpha);
}
"#
    .to_string()
}

/// Hardcoded WGSL for the deferred lighting pass with splat compositing.
/// Composites splat_color over mesh lighting using depth comparison.
pub fn get_deferred_light_with_splats_wgsl() -> String {
    r#"
struct CameraUniform {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    view_projection: mat4x4<f32>,
    position: vec3<f32>,
    near_plane: f32,
    far_plane: f32,
    _pad1: f32,
    viewport_size: vec2<f32>,
    _padding: f32,
    _pad2: vec3<f32>,
};

struct PointLight {
    position: vec3<f32>,
    range: f32,
    color: vec3<f32>,
    intensity: f32,
};

struct LightingUniforms {
    light_count: u32,
    _pad: vec3<u32>,
    lights: array<PointLight, 32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

@group(1) @binding(0) var gbuffer_albedo: texture_2d<f32>;
@group(1) @binding(1) var gbuffer_normal: texture_2d<f32>;
@group(1) @binding(2) var gbuffer_depth: texture_depth_2d;
@group(1) @binding(3) var gbuffer_sampler: sampler;

@group(2) @binding(0) var<uniform> lighting: LightingUniforms;

@group(3) @binding(0) var splat_color_tex: texture_2d<f32>;
@group(3) @binding(1) var splat_depth_tex: texture_depth_2d;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let uv = vec2<f32>(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    out.position = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_coords = vec2<i32>(in.position.xy);
    let albedo = textureLoad(gbuffer_albedo, tex_coords, 0).rgb;
    let normal_raw = textureLoad(gbuffer_normal, tex_coords, 0).rgb;
    let normal = normalize(normal_raw * 2.0 - 1.0);
    let mesh_depth = textureLoad(gbuffer_depth, tex_coords, 0);

    // Sample splat targets
    let splat_color = textureLoad(splat_color_tex, tex_coords, 0);
    let splat_depth = textureLoad(splat_depth_tex, tex_coords, 0);

    // Compute mesh lighting
    var mesh_color = albedo * vec3<f32>(0.08, 0.08, 0.1);

    for (var i = 0u; i < lighting.light_count; i = i + 1u) {
        let light = lighting.lights[i];
        let light_dir = normalize(light.position);
        let ndotl = max(dot(normal, light_dir), 0.0);
        let attenuation = light.intensity;
        mesh_color = mesh_color + albedo * light.color * ndotl * attenuation;
    }

    if lighting.light_count == 0u {
        let light_dir = normalize(vec3<f32>(0.3, 1.0, 0.5));
        let ndotl = max(dot(normal, light_dir), 0.0);
        mesh_color = albedo * (vec3<f32>(0.15, 0.15, 0.18) + ndotl * 0.85);
    }

    let mesh_valid = mesh_depth < 1.0;
    let splat_valid = splat_color.a > 0.004;

    // Depth compositing
    if splat_valid && (!mesh_valid || splat_depth < mesh_depth) {
        // Splat is closer or mesh is empty: blend splat over background
        let bg = select(vec3<f32>(0.0), mesh_color, mesh_valid);
        let blended = splat_color.rgb + bg * (1.0 - splat_color.a);
        return vec4<f32>(blended, 1.0);
    } else if mesh_valid {
        // Mesh is closer: use mesh lighting, blend splat behind
        if splat_valid {
            let blended = mesh_color * (1.0 - splat_color.a * 0.0) + splat_color.rgb * 0.0;
            return vec4<f32>(mesh_color, 1.0);
        }
        return vec4<f32>(mesh_color, 1.0);
    }

    // Nothing at this pixel
    discard;
}
"#
    .to_string()
}

/// Hardcoded WGSL fallback for the tone mapping pass.
pub fn get_tonemap_wgsl() -> String {
    r#"
@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let uv = vec2<f32>(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    out.position = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

// ACES tone mapping curve
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let hdr_color = textureSample(hdr_texture, hdr_sampler, in.uv).rgb;
    let sdr_color = aces_tonemap(hdr_color);
    return vec4<f32>(sdr_color, 1.0);
}
"#
    .to_string()
}
