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
    // Also add the modules directory so `import camera;` etc. resolve
    let modules_path = path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("modules"))
        .unwrap_or_else(|| Path::new("modules").to_path_buf())
        .to_string_lossy()
        .to_string();
    let modules_path_c = CString::new(modules_path.as_str()).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Invalid search path: {:?}", e))
    })?;
    let search_paths_ptrs = [search_path_c.as_ptr(), modules_path_c.as_ptr()];

    let target_desc = slang::TargetDesc::default()
        .format(slang::CompileTarget::Wgsl);
    let targets = [target_desc];

    let options = slang::CompilerOptions::default()
        .matrix_layout_column(true);

    let session_desc = slang::SessionDesc::default()
        .targets(&targets)
        .search_paths(&search_paths_ptrs)
        .options(&options);

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

    let wgsl = code.as_str()
        .map(|s| s.to_string())
        .map_err(|e| {
            ShaderError::SlangCompilationFailed(format!("Invalid UTF-8 in WGSL output: {:?}", e))
        })?;

    Ok(fixup_slang_wgsl(&wgsl))
}

/// Fix known SLANG WGSL backend issues:
/// 1. Depth textures emitted as `texture_2d<f32>` instead of `texture_depth_2d`
/// 2. `textureLoad(depth_tex, ...).x` should be `textureLoad(depth_tex, ...)` (returns f32)
fn fixup_slang_wgsl(wgsl: &str) -> String {
    let mut depth_texture_names: Vec<String> = Vec::new();

    // Detect depth textures: used with textureSampleCompareLevel / textureSampleCompare
    for line in wgsl.lines() {
        for func in &["textureSampleCompareLevel(", "textureSampleCompare("] {
            if let Some(pos) = line.find(func) {
                let after = &line[pos + func.len()..];
                // Pattern: ((texture_name), ...) — extract name between (( and ))
                if after.starts_with('(') {
                    if let Some(end) = after[1..].find(')') {
                        let name = after[1..1 + end].trim().to_string();
                        if !depth_texture_names.contains(&name) {
                            depth_texture_names.push(name);
                        }
                    }
                }
            }
        }
    }

    // Also detect by name: variables containing "depth" declared as texture_2d<f32>
    for line in wgsl.lines() {
        if line.contains("texture_2d<f32>") {
            // Pattern: var NAME : texture_2d<f32>
            if let Some(var_pos) = line.find("var ") {
                let after_var = &line[var_pos + 4..];
                if let Some(colon_pos) = after_var.find(" : ") {
                    let name = after_var[..colon_pos].trim().to_string();
                    if name.contains("depth")
                        || name.contains("shadow")
                    {
                        if !depth_texture_names.contains(&name) {
                            depth_texture_names.push(name);
                        }
                    }
                }
            }
        }
    }

    if depth_texture_names.is_empty() {
        return wgsl.to_string();
    }

    let mut output = wgsl.to_string();

    // Fix declarations: texture_2d<f32> → texture_depth_2d
    for name in &depth_texture_names {
        let old_decl = format!("var {} : texture_2d<f32>", name);
        let new_decl = format!("var {} : texture_depth_2d", name);
        output = output.replace(&old_decl, &new_decl);
    }

    // Fix textureLoad(...).x for depth textures → textureLoad(...)
    // Depth textures return f32 directly, not vec4<f32>
    for name in &depth_texture_names {
        let search = format!("textureLoad(({})", name);
        let mut start = 0;
        loop {
            let remaining = &output[start..];
            let Some(found) = remaining.find(&search) else {
                break;
            };
            let abs_pos = start + found;
            // Find the matching ')' for the textureLoad( call
            let call_open = abs_pos + "textureLoad".len();
            let bytes = output.as_bytes();
            let mut depth = 0i32;
            let mut close_pos = None;
            for i in call_open..bytes.len() {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(cp) = close_pos {
                // Check if followed by ".x"
                if cp + 2 < bytes.len() && bytes[cp + 1] == b'.' && bytes[cp + 2] == b'x' {
                    // Remove ".x"
                    output = format!("{}{}", &output[..cp + 1], &output[cp + 3..]);
                    start = cp + 1;
                } else {
                    start = cp + 1;
                }
            } else {
                break;
            }
        }
    }

    output
}

/// Fallback when SLANG feature is disabled.
#[cfg(not(feature = "slang"))]
fn compile_slang_to_wgsl(_path: &Path) -> Result<String, ShaderError> {
    Err(ShaderError::SlangCompilationFailed(
        "SLANG support not compiled in (feature 'slang' disabled)".to_string(),
    ))
}

/// Hardcoded WGSL fallback for the 3D forward mesh shader.
/// Uses PBR-lite: metallic darkens diffuse, roughness-based specular, emission.
pub fn get_mesh_forward_wgsl() -> String {
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
    let view_dir = normalize(camera.position - in.world_pos);
    let half_vec = normalize(light_dir + view_dir);

    let ndotl = max(dot(in.world_normal, light_dir), 0.0);
    let ndoth = max(dot(in.world_normal, half_vec), 0.0);

    let diffuse_color = draw.base_color.rgb * (1.0 - draw.metallic);
    let F0 = mix(vec3<f32>(0.04), draw.base_color.rgb, draw.metallic);

    let ambient = vec3<f32>(0.03, 0.03, 0.04);
    let diffuse = diffuse_color * (ambient + ndotl * 0.85);

    let spec_power = max(2.0 / (draw.roughness * draw.roughness + 0.001) - 2.0, 1.0);
    let specular = pow(ndoth, spec_power) * ndotl;
    let spec = F0 * specular;

    let color = diffuse + spec + draw.emission.rgb;
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

    let options = slang::CompilerOptions::default()
        .matrix_layout_column(true);

    let session_desc = slang::SessionDesc::default()
        .targets(&targets)
        .search_paths(&search_paths_ptrs)
        .options(&options);

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
/// Outputs 3 MRT: albedo+roughness, normal+metallic, emission.
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
    _pad2: vec4<f32>,
    inv_view_projection: mat4x4<f32>,
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
    @location(0) albedo: vec4<f32>,   // rgb = base_color, a = roughness
    @location(1) normal: vec4<f32>,   // rgb = packed normal, a = metallic
    @location(2) emission: vec4<f32>, // rgb = emission color, a = 0
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
    out.albedo = vec4<f32>(draw.base_color.rgb, draw.roughness);
    out.normal = vec4<f32>(in.world_normal * 0.5 + 0.5, draw.metallic);
    out.emission = vec4<f32>(draw.emission.rgb, 0.0);
    return out;
}
"#
    .to_string()
}

/// Hardcoded WGSL fallback for the deferred lighting pass.
/// PBR-like shading with roughness, metallic, specular, and emission.
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
    has_directional: u32,
    _pad_a: vec2<u32>,
    _pad_b: vec4<u32>,
    dir_light_direction: vec3<f32>,
    dir_light_intensity: f32,
    dir_light_color: vec3<f32>,
    _pad_c: f32,
    light_vp: mat4x4<f32>,
    lights: array<PointLight, 32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

@group(1) @binding(0) var gbuffer_albedo: texture_2d<f32>;
@group(1) @binding(1) var gbuffer_normal: texture_2d<f32>;
@group(1) @binding(2) var gbuffer_depth: texture_depth_2d;
@group(1) @binding(3) var gbuffer_sampler: sampler;
@group(1) @binding(4) var gbuffer_emission: texture_2d<f32>;

@group(2) @binding(0) var<uniform> lighting: LightingUniforms;
@group(2) @binding(1) var shadow_map: texture_depth_2d;
@group(2) @binding(2) var shadow_sampler: sampler_comparison;

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
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth, 1.0);
    let world_h = camera.inv_view_projection * clip;
    return world_h.xyz / world_h.w;
}

fn sample_shadow_pcf(world_pos: vec3<f32>) -> f32 {
    let light_clip = lighting.light_vp * vec4<f32>(world_pos, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;
    let shadow_uv = vec2<f32>(light_ndc.x * 0.5 + 0.5, -light_ndc.y * 0.5 + 0.5);
    let shadow_depth = light_ndc.z;

    if shadow_uv.x < 0.0 || shadow_uv.x > 1.0 || shadow_uv.y < 0.0 || shadow_uv.y > 1.0 {
        return 1.0;
    }

    let shadow_dim = vec2<f32>(textureDimensions(shadow_map));
    let texel_size = 1.0 / shadow_dim;

    var shadow = 0.0;
    for (var y = -2i; y <= 2i; y = y + 1i) {
        for (var x = -2i; x <= 2i; x = x + 1i) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            shadow = shadow + textureSampleCompare(shadow_map, shadow_sampler, shadow_uv + offset, shadow_depth - 0.005);
        }
    }
    return shadow / 25.0;
}

// Cook-Torrance BRDF helper functions
fn distribution_ggx(NdotH: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let NdotH2 = NdotH * NdotH;
    let denom = NdotH2 * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * denom * denom + 0.0001);
}

fn geometry_smith(NdotV: f32, NdotL: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let ggx_v = NdotV / (NdotV * (1.0 - k) + k);
    let ggx_l = NdotL / (NdotL * (1.0 - k) + k);
    return ggx_v * ggx_l;
}

fn fresnel_schlick(cos_theta: f32, F0: vec3<f32>) -> vec3<f32> {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_coords = vec2<i32>(in.position.xy);

    let albedo_rough = textureLoad(gbuffer_albedo, tex_coords, 0);
    let normal_metal = textureLoad(gbuffer_normal, tex_coords, 0);
    let emission_val = textureLoad(gbuffer_emission, tex_coords, 0);
    let depth = textureLoad(gbuffer_depth, tex_coords, 0);

    if depth >= 1.0 {
        discard;
    }

    let albedo    = albedo_rough.rgb;
    let roughness = max(albedo_rough.a, 0.04);
    let normal    = normalize(normal_metal.rgb * 2.0 - 1.0);
    let metallic  = normal_metal.a;
    let emission  = emission_val.rgb;

    // Reconstruct world position
    let world_pos = reconstruct_world_pos(in.uv, depth);
    let view_dir  = normalize(camera.position - world_pos);

    // PBR base reflectance
    let F0 = mix(vec3<f32>(0.04, 0.04, 0.04), albedo, metallic);
    let diffuse_color = albedo * (1.0 - metallic);

    // Ambient
    var color = diffuse_color * vec3<f32>(0.02, 0.02, 0.025);

    let NdotV = max(dot(normal, view_dir), 0.001);

    // Accumulate point lights with Cook-Torrance BRDF
    for (var i = 0u; i < lighting.light_count; i = i + 1u) {
        let light = lighting.lights[i];
        let to_light = light.position - world_pos;
        let dist = length(to_light);

        if dist > light.range {
            continue;
        }

        let light_dir = to_light / dist;
        let half_vec  = normalize(light_dir + view_dir);

        let NdotL = max(dot(normal, light_dir), 0.0);
        let NdotH = max(dot(normal, half_vec), 0.0);
        let HdotV = max(dot(half_vec, view_dir), 0.0);

        // Attenuation: inverse-square with smooth range falloff
        let dist_atten = 1.0 / (1.0 + dist * dist);
        let range_factor = saturate(1.0 - pow(dist / light.range, 4.0));
        let attenuation = light.intensity * dist_atten * range_factor;

        // Cook-Torrance specular BRDF
        let D = distribution_ggx(NdotH, roughness);
        let G = geometry_smith(NdotV, NdotL, roughness);
        let F = fresnel_schlick(HdotV, F0);

        let numerator = D * G * F;
        let denominator = 4.0 * NdotV * NdotL + 0.0001;
        let specular = numerator / denominator;

        // Energy conservation: diffuse reduced by Fresnel
        let kD = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let diffuse = kD * diffuse_color / 3.14159265;

        color = color + (diffuse + specular) * light.color * NdotL * attenuation;
    }

    // Directional light with Cook-Torrance BRDF + shadows
    if lighting.has_directional != 0u {
        let dir_light_dir = normalize(-lighting.dir_light_direction);
        let half_vec_d = normalize(dir_light_dir + view_dir);

        let NdotL_d = max(dot(normal, dir_light_dir), 0.0);
        let NdotH_d = max(dot(normal, half_vec_d), 0.0);
        let HdotV_d = max(dot(half_vec_d, view_dir), 0.0);

        let D_d = distribution_ggx(NdotH_d, roughness);
        let G_d = geometry_smith(NdotV, NdotL_d, roughness);
        let F_d = fresnel_schlick(HdotV_d, F0);

        let spec_d = (D_d * G_d * F_d) / (4.0 * NdotV * NdotL_d + 0.0001);
        let kD_d = (vec3<f32>(1.0) - F_d) * (1.0 - metallic);
        let diff_d = kD_d * diffuse_color / 3.14159265;

        let shadow = sample_shadow_pcf(world_pos);
        color = color + (diff_d + spec_d) * lighting.dir_light_color * NdotL_d * lighting.dir_light_intensity * shadow;
    }

    // Modulate emission by lighting to preserve 3D form (must survive ACES tone mapping)
    let emit_lum = dot(emission, vec3<f32>(0.2126, 0.7152, 0.0722));
    let blend = emit_lum / (emit_lum + 0.5);
    var emit_shade = 0.15;
    if (lighting.has_directional != 0u) {
        let emit_NdotL = max(dot(normal, normalize(-lighting.dir_light_direction)), 0.0);
        emit_shade = 0.1 + 0.9 * emit_NdotL;
    }
    let rim = pow(1.0 - NdotV, 3.0);
    emit_shade = emit_shade + 0.35 * rim;
    color = color * (1.0 - blend) + emission * mix(1.0, emit_shade, blend);

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
/// PBR shading + emission + depth-composited Gaussian splats.
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
    has_directional: u32,
    _pad_a: vec2<u32>,
    _pad_b: vec4<u32>,
    dir_light_direction: vec3<f32>,
    dir_light_intensity: f32,
    dir_light_color: vec3<f32>,
    _pad_c: f32,
    light_vp: mat4x4<f32>,
    lights: array<PointLight, 32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

@group(1) @binding(0) var gbuffer_albedo: texture_2d<f32>;
@group(1) @binding(1) var gbuffer_normal: texture_2d<f32>;
@group(1) @binding(2) var gbuffer_depth: texture_depth_2d;
@group(1) @binding(3) var gbuffer_sampler: sampler;
@group(1) @binding(4) var gbuffer_emission: texture_2d<f32>;

@group(2) @binding(0) var<uniform> lighting: LightingUniforms;
@group(2) @binding(1) var shadow_map: texture_depth_2d;
@group(2) @binding(2) var shadow_sampler: sampler_comparison;

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

fn reconstruct_world_pos(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth, 1.0);
    let world_h = camera.inv_view_projection * clip;
    return world_h.xyz / world_h.w;
}

fn sample_shadow_pcf(world_pos: vec3<f32>) -> f32 {
    let light_clip = lighting.light_vp * vec4<f32>(world_pos, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;
    let shadow_uv = vec2<f32>(light_ndc.x * 0.5 + 0.5, -light_ndc.y * 0.5 + 0.5);
    let shadow_depth = light_ndc.z;

    if shadow_uv.x < 0.0 || shadow_uv.x > 1.0 || shadow_uv.y < 0.0 || shadow_uv.y > 1.0 {
        return 1.0;
    }

    let shadow_dim = vec2<f32>(textureDimensions(shadow_map));
    let texel_size = 1.0 / shadow_dim;

    var shadow = 0.0;
    for (var y = -2i; y <= 2i; y = y + 1i) {
        for (var x = -2i; x <= 2i; x = x + 1i) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            shadow = shadow + textureSampleCompare(shadow_map, shadow_sampler, shadow_uv + offset, shadow_depth - 0.005);
        }
    }
    return shadow / 25.0;
}

// Cook-Torrance BRDF helper functions
fn distribution_ggx(NdotH: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let NdotH2 = NdotH * NdotH;
    let denom = NdotH2 * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * denom * denom + 0.0001);
}

fn geometry_smith(NdotV: f32, NdotL: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let ggx_v = NdotV / (NdotV * (1.0 - k) + k);
    let ggx_l = NdotL / (NdotL * (1.0 - k) + k);
    return ggx_v * ggx_l;
}

fn fresnel_schlick(cos_theta: f32, F0: vec3<f32>) -> vec3<f32> {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_coords = vec2<i32>(in.position.xy);

    let albedo_rough = textureLoad(gbuffer_albedo, tex_coords, 0);
    let normal_metal = textureLoad(gbuffer_normal, tex_coords, 0);
    let emission_val = textureLoad(gbuffer_emission, tex_coords, 0);
    let mesh_depth = textureLoad(gbuffer_depth, tex_coords, 0);

    let albedo    = albedo_rough.rgb;
    let roughness = max(albedo_rough.a, 0.04);
    let normal    = normalize(normal_metal.rgb * 2.0 - 1.0);
    let metallic  = normal_metal.a;
    let emission  = emission_val.rgb;

    // Sample splat targets
    let splat_color = textureLoad(splat_color_tex, tex_coords, 0);
    let splat_depth = textureLoad(splat_depth_tex, tex_coords, 0);

    // PBR base reflectance
    let F0 = mix(vec3<f32>(0.04, 0.04, 0.04), albedo, metallic);
    let diffuse_color = albedo * (1.0 - metallic);

    // Reconstruct world position
    let world_pos = reconstruct_world_pos(in.uv, mesh_depth);
    let view_dir  = normalize(camera.position - world_pos);

    let NdotV = max(dot(normal, view_dir), 0.001);

    // Compute mesh lighting with Cook-Torrance BRDF
    var mesh_color = diffuse_color * vec3<f32>(0.02, 0.02, 0.025);

    for (var i = 0u; i < lighting.light_count; i = i + 1u) {
        let light = lighting.lights[i];
        let to_light = light.position - world_pos;
        let dist = length(to_light);

        if dist > light.range {
            continue;
        }

        let light_dir = to_light / dist;
        let half_vec  = normalize(light_dir + view_dir);

        let NdotL = max(dot(normal, light_dir), 0.0);
        let NdotH = max(dot(normal, half_vec), 0.0);
        let HdotV = max(dot(half_vec, view_dir), 0.0);

        let dist_atten = 1.0 / (1.0 + dist * dist);
        let range_factor = saturate(1.0 - pow(dist / light.range, 4.0));
        let attenuation = light.intensity * dist_atten * range_factor;

        // Cook-Torrance specular BRDF
        let D = distribution_ggx(NdotH, roughness);
        let G = geometry_smith(NdotV, NdotL, roughness);
        let F = fresnel_schlick(HdotV, F0);

        let numerator = D * G * F;
        let denominator = 4.0 * NdotV * NdotL + 0.0001;
        let specular = numerator / denominator;

        let kD = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let diffuse = kD * diffuse_color / 3.14159265;

        mesh_color = mesh_color + (diffuse + specular) * light.color * NdotL * attenuation;
    }

    // Directional light with Cook-Torrance BRDF + shadows
    if lighting.has_directional != 0u {
        let dir_light_dir = normalize(-lighting.dir_light_direction);
        let half_vec_d = normalize(dir_light_dir + view_dir);

        let NdotL_d = max(dot(normal, dir_light_dir), 0.0);
        let NdotH_d = max(dot(normal, half_vec_d), 0.0);
        let HdotV_d = max(dot(half_vec_d, view_dir), 0.0);

        let D_d = distribution_ggx(NdotH_d, roughness);
        let G_d = geometry_smith(NdotV, NdotL_d, roughness);
        let F_d = fresnel_schlick(HdotV_d, F0);

        let spec_d = (D_d * G_d * F_d) / (4.0 * NdotV * NdotL_d + 0.0001);
        let kD_d = (vec3<f32>(1.0) - F_d) * (1.0 - metallic);
        let diff_d = kD_d * diffuse_color / 3.14159265;

        let shadow = sample_shadow_pcf(world_pos);
        mesh_color = mesh_color + (diff_d + spec_d) * lighting.dir_light_color * NdotL_d * lighting.dir_light_intensity * shadow;
    }

    // Modulate emission by lighting to preserve 3D form (must survive ACES tone mapping)
    let emit_lum = dot(emission, vec3<f32>(0.2126, 0.7152, 0.0722));
    let blend = emit_lum / (emit_lum + 0.5);
    var emit_shade = 0.15;
    if (lighting.has_directional != 0u) {
        let emit_NdotL = max(dot(normal, normalize(-lighting.dir_light_direction)), 0.0);
        emit_shade = 0.1 + 0.9 * emit_NdotL;
    }
    let rim = pow(1.0 - NdotV, 3.0);
    emit_shade = emit_shade + 0.35 * rim;
    mesh_color = mesh_color * (1.0 - blend) + emission * mix(1.0, emit_shade, blend);

    let mesh_valid = mesh_depth < 1.0;
    let splat_valid = splat_color.a > 0.004;

    // Composite: prefer closer of mesh vs splat, blend if overlapping
    if splat_valid && (!mesh_valid || splat_depth < mesh_depth) {
        let bg = select(vec3<f32>(0.0), mesh_color, mesh_valid);
        let blended = splat_color.rgb + bg * (1.0 - splat_color.a);
        return vec4<f32>(blended, 1.0);
    } else if mesh_valid {
        return vec4<f32>(mesh_color, 1.0);
    }

    discard;
}
"#
    .to_string()
}

/// Hardcoded WGSL fallback for the tone mapping pass.
/// WGSL fallback for bloom extraction pass (threshold + 13-tap tent downsample).
pub fn get_bloom_wgsl() -> String {
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

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn threshold_color(color: vec3<f32>, threshold: f32, knee: f32) -> vec3<f32> {
    let lum = luminance(color);
    var soft = lum - threshold + knee;
    soft = clamp(soft, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 0.0001);
    let contribution = max(soft, lum - threshold) / max(lum, 0.0001);
    return color * max(contribution, 0.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(hdr_texture));
    let texel = 1.0 / dims;
    let uv = in.uv;

    // 13-tap tent filter (Jimenez 2014)
    let a = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>(-2.0, -2.0) * texel).rgb;
    let b = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 0.0, -2.0) * texel).rgb;
    let c = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 2.0, -2.0) * texel).rgb;
    let d = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>(-1.0, -1.0) * texel).rgb;
    let e = textureSample(hdr_texture, hdr_sampler, uv).rgb;
    let f = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 1.0, -1.0) * texel).rgb;
    let g = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>(-2.0,  0.0) * texel).rgb;
    let h = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 2.0,  0.0) * texel).rgb;
    let i = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>(-1.0,  1.0) * texel).rgb;
    let j = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 1.0,  1.0) * texel).rgb;
    let k = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>(-2.0,  2.0) * texel).rgb;
    let l = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 0.0,  2.0) * texel).rgb;
    let m = textureSample(hdr_texture, hdr_sampler, uv + vec2<f32>( 2.0,  2.0) * texel).rgb;

    // Weighted downsample
    var result = e * 0.125;
    result += (d + f + i + j) * 0.125;
    result += (a + c + k + m) * 0.03125;
    result += (b + g + h + l) * 0.0625;

    // Soft threshold
    result = threshold_color(result, 0.5, 0.3);

    return vec4<f32>(result, 1.0);
}
"#
    .to_string()
}

pub fn get_tonemap_wgsl() -> String {
    r#"
@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;
@group(0) @binding(2) var bloom_texture: texture_2d<f32>;

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
    let uv = in.uv;
    let center_offset = uv - vec2<f32>(0.5, 0.5);
    let dist_from_center = length(center_offset);

    // Chromatic aberration: offset R and B channels outward from center
    let ca_strength = 0.002;
    let ca_offset = center_offset * ca_strength;
    let r = textureSample(hdr_texture, hdr_sampler, uv - ca_offset).r;
    let g = textureSample(hdr_texture, hdr_sampler, uv).g;
    let b = textureSample(hdr_texture, hdr_sampler, uv + ca_offset).b;
    var hdr_color = vec3<f32>(r, g, b);

    // Add bloom (bilinear-filtered from half-res gives natural soft glow)
    let bloom = textureSample(bloom_texture, hdr_sampler, uv).rgb;
    let bloom_strength = 0.7;
    hdr_color += bloom * bloom_strength;

    // ACES tonemap
    let sdr_color_tm = aces_tonemap(hdr_color);

    // Vignette: smooth darkening at edges
    let vignette = 1.0 - smoothstep(0.4, 0.9, dist_from_center);
    let sdr_color = sdr_color_tm * vignette;

    return vec4<f32>(sdr_color, 1.0);
}
"#
    .to_string()
}

pub fn get_fxaa_wgsl() -> String {
    r#"
// FXAA 3.11 — Fast Approximate Anti-Aliasing (Timothy Lottes / NVIDIA)
// Full implementation with edge endpoint search

@group(0) @binding(0) var ldr_texture: texture_2d<f32>;
@group(0) @binding(1) var ldr_sampler: sampler;

const FXAA_EDGE_THRESHOLD: f32 = 0.063;
const FXAA_EDGE_THRESHOLD_MIN: f32 = 0.0312;
const FXAA_SEARCH_STEPS: i32 = 8;
const FXAA_SEARCH_THRESHOLD: f32 = 0.25;
const FXAA_SUBPIX_QUALITY: f32 = 0.75;

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

fn fxaa_luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(ldr_texture));
    let texel = 1.0 / dims;
    let uv = in.uv;

    // Sample center and cardinal neighbors
    let rgbM = textureSample(ldr_texture, ldr_sampler, uv).rgb;
    let rgbN = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(0.0, -texel.y)).rgb;
    let rgbS = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(0.0,  texel.y)).rgb;
    let rgbW = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(-texel.x, 0.0)).rgb;
    let rgbE = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>( texel.x, 0.0)).rgb;

    let lumaM = fxaa_luma(rgbM);
    let lumaN = fxaa_luma(rgbN);
    let lumaS = fxaa_luma(rgbS);
    let lumaW = fxaa_luma(rgbW);
    let lumaE = fxaa_luma(rgbE);

    let lumaMin = min(lumaM, min(min(lumaN, lumaS), min(lumaW, lumaE)));
    let lumaMax = max(lumaM, max(max(lumaN, lumaS), max(lumaW, lumaE)));
    let lumaRange = lumaMax - lumaMin;

    if lumaRange < max(FXAA_EDGE_THRESHOLD_MIN, lumaMax * FXAA_EDGE_THRESHOLD) {
        return vec4<f32>(rgbM, 1.0);
    }

    // Sample diagonal neighbors for edge direction
    let rgbNW = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(-texel.x, -texel.y)).rgb;
    let rgbNE = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>( texel.x, -texel.y)).rgb;
    let rgbSW = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(-texel.x,  texel.y)).rgb;
    let rgbSE = textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>( texel.x,  texel.y)).rgb;

    let lumaNW = fxaa_luma(rgbNW);
    let lumaNE = fxaa_luma(rgbNE);
    let lumaSW = fxaa_luma(rgbSW);
    let lumaSE = fxaa_luma(rgbSE);

    let lumaNS = lumaN + lumaS;
    let lumaWE = lumaW + lumaE;

    // Subpixel aliasing detection
    let lumaL = (lumaNS + lumaWE) * 0.25;
    let rangeL = abs(lumaL - lumaM);
    var subpixBlend = max(0.0, (rangeL / lumaRange) - FXAA_SEARCH_THRESHOLD);
    subpixBlend = min(FXAA_SUBPIX_QUALITY, subpixBlend * subpixBlend * (3.0 - 2.0 * subpixBlend));

    // Edge direction detection
    let lumaNWSW = lumaNW + lumaSW;
    let lumaNESE = lumaNE + lumaSE;
    let lumaNWNE = lumaNW + lumaNE;
    let lumaSWSE = lumaSW + lumaSE;

    let edgeH = abs(lumaNWSW - 2.0 * lumaW) +
                abs(lumaNS - 2.0 * lumaM) * 2.0 +
                abs(lumaNESE - 2.0 * lumaE);
    let edgeV = abs(lumaNWNE - 2.0 * lumaN) +
                abs(lumaWE - 2.0 * lumaM) * 2.0 +
                abs(lumaSWSE - 2.0 * lumaS);

    let isHorizontal = edgeH >= edgeV;

    // Select edge normal direction (perpendicular to edge)
    let luma1 = select(lumaW, lumaN, isHorizontal);
    let luma2 = select(lumaE, lumaS, isHorizontal);
    let grad1 = luma1 - lumaM;
    let grad2 = luma2 - lumaM;
    let is1Steepest = abs(grad1) >= abs(grad2);

    let gradScaled = 0.25 * max(abs(grad1), abs(grad2));

    // Step size along the edge (tangent direction)
    var stepLength = select(texel.x, texel.y, isHorizontal);
    var lumaLocalAvg: f32;

    if is1Steepest {
        stepLength = -stepLength;
        lumaLocalAvg = 0.5 * (luma1 + lumaM);
    } else {
        lumaLocalAvg = 0.5 * (luma2 + lumaM);
    }

    // Shift UV to the edge boundary (half a texel perpendicular)
    var edgeUV = uv;
    if isHorizontal {
        edgeUV.y = edgeUV.y + stepLength * 0.5;
    } else {
        edgeUV.x = edgeUV.x + stepLength * 0.5;
    }

    // Edge tangent direction (along the edge)
    let edgeStep = select(vec2<f32>(0.0, texel.y), vec2<f32>(texel.x, 0.0), isHorizontal);

    // Search along edge in positive direction
    var uvP = edgeUV + edgeStep;
    var lumaEndP = fxaa_luma(textureSample(ldr_texture, ldr_sampler, uvP).rgb) - lumaLocalAvg;
    var doneP = abs(lumaEndP) >= gradScaled;

    for (var i = 1i; i < FXAA_SEARCH_STEPS && !doneP; i = i + 1i) {
        uvP = uvP + edgeStep;
        lumaEndP = fxaa_luma(textureSample(ldr_texture, ldr_sampler, uvP).rgb) - lumaLocalAvg;
        doneP = abs(lumaEndP) >= gradScaled;
    }

    // Search along edge in negative direction
    var uvN = edgeUV - edgeStep;
    var lumaEndN = fxaa_luma(textureSample(ldr_texture, ldr_sampler, uvN).rgb) - lumaLocalAvg;
    var doneN = abs(lumaEndN) >= gradScaled;

    for (var i = 1i; i < FXAA_SEARCH_STEPS && !doneN; i = i + 1i) {
        uvN = uvN - edgeStep;
        lumaEndN = fxaa_luma(textureSample(ldr_texture, ldr_sampler, uvN).rgb) - lumaLocalAvg;
        doneN = abs(lumaEndN) >= gradScaled;
    }

    // Compute distances to edge endpoints
    var distP: f32;
    var distN: f32;
    if isHorizontal {
        distP = uvP.x - uv.x;
        distN = uv.x - uvN.x;
    } else {
        distP = uvP.y - uv.y;
        distN = uv.y - uvN.y;
    }

    let isCloserToP = distP < distN;
    let distClosest = min(distP, distN);
    let edgeLength = distP + distN;

    // Is the closer endpoint's luma delta in the same direction as the local gradient?
    let lumaEndClosest = select(lumaEndN, lumaEndP, isCloserToP);
    let goodSpan = (lumaEndClosest < 0.0) != (lumaM - lumaLocalAvg < 0.0);

    // Edge blend factor: closer to edge endpoint = stronger blend
    var edgeBlend: f32;
    if goodSpan {
        edgeBlend = 0.5 - distClosest / edgeLength;
    } else {
        edgeBlend = 0.0;
    }

    // Use the maximum of edge blend and subpixel blend
    let finalBlend = max(edgeBlend, subpixBlend);

    // Final sample: blend perpendicular to the edge
    var finalUV = uv;
    if isHorizontal {
        finalUV.y = finalUV.y + stepLength * finalBlend;
    } else {
        finalUV.x = finalUV.x + stepLength * finalBlend;
    }

    let result = textureSample(ldr_texture, ldr_sampler, finalUV).rgb;
    return vec4<f32>(result, 1.0);
}
"#
    .to_string()
}

pub fn get_shadow_depth_wgsl() -> String {
    r#"
// Shadow depth pass: renders geometry from light's perspective (depth-only)

struct ShadowUniforms {
    light_view_projection: mat4x4<f32>,
};

struct DrawUniforms {
    model_matrix: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    _pad: vec2<f32>,
    emission: vec4<f32>,
    _padding: array<vec4<f32>, 5>,
};

@group(0) @binding(0) var<uniform> shadow: ShadowUniforms;
@group(1) @binding(0) var<uniform> draw: DrawUniforms;

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) tex_coords: vec2<f32>) -> @builtin(position) vec4<f32> {
    let world_pos = shadow.light_view_projection * draw.model_matrix * vec4<f32>(position, 1.0);
    return world_pos;
}

@fragment
fn fs_main() {
    // Depth-only pass — no color output needed
}
"#
    .to_string()
}
