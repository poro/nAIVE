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
pub fn compile_triangle_shader(
    slang_path: Option<&Path>,
    project_root: &Path,
) -> Result<String, ShaderError> {
    if let Some(path) = slang_path {
        if !path.exists() {
            tracing::warn!("SLANG file not found: {:?}, using fallback WGSL", path);
            return Ok(get_triangle_wgsl());
        }

        match compile_slang_to_wgsl(path, project_root) {
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

/// Load an explicitly referenced project shader. Compilation and artifact
/// failures are errors; only `builtin:` pipeline references use engine WGSL.
pub(crate) fn load_required_shader(
    path: &Path,
    project_root: &Path,
) -> Result<String, ShaderError> {
    let source = if path.extension().is_some_and(|ext| ext == "wgsl") {
        std::fs::read_to_string(path).map_err(ShaderError::IoError)?
    } else if path.extension().is_some_and(|ext| ext == "slang") {
        // An artifact-only project can be repackaged even by a Slang-enabled
        // tool. When source exists, compile it and report any failure.
        if cfg!(feature = "slang") && !path.exists() {
            let artifact = resolve_precompiled_wgsl(path, project_root)
                .map_err(ShaderError::SlangCompilationFailed)?;
            std::fs::read_to_string(artifact).map_err(ShaderError::IoError)?
        } else {
            compile_slang_to_wgsl(path, project_root)?
        }
    } else {
        return Err(ShaderError::SlangCompilationFailed(format!(
            "Expected a .slang or .wgsl shader: {}",
            path.display()
        )));
    };
    let module = wgpu::naga::front::wgsl::parse_str(&source)
        .map_err(|error| ShaderError::SlangCompilationFailed(error.emit_to_string(&source)))?;
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| ShaderError::SlangCompilationFailed(error.emit_to_string(&source)))?;
    for (name, stage) in [
        ("vs_main", wgpu::naga::ShaderStage::Vertex),
        ("fs_main", wgpu::naga::ShaderStage::Fragment),
    ] {
        if !module
            .entry_points
            .iter()
            .any(|entry| entry.name == name && entry.stage == stage)
        {
            return Err(ShaderError::SlangCompilationFailed(format!(
                "Required entry point '{name}' not found in {}",
                path.display()
            )));
        }
    }
    Ok(source)
}

/// Actual SLANG-to-WGSL compilation using shader-slang crate.
/// Compile the Slang shader at `path` to WGSL. Every file it reads (its own,
/// `#include`s and imported modules) must be inside `project_root`; `import`
/// of a relative or absolute path, or a symlink, would otherwise compile files
/// from elsewhere on disk into the game.
#[cfg(feature = "slang")]
fn compile_slang_to_wgsl(path: &Path, project_root: &Path) -> Result<String, ShaderError> {
    use shader_slang as slang;
    use std::ffi::CString;

    let root = project_root.canonicalize().map_err(|error| {
        ShaderError::SlangCompilationFailed(format!(
            "Cannot resolve project {}: {error}",
            project_root.display()
        ))
    })?;
    let inside = |candidate: &Path| {
        candidate
            .canonicalize()
            .is_ok_and(|resolved| resolved.starts_with(&root))
    };

    let global_session = slang::GlobalSession::new().ok_or_else(|| {
        ShaderError::SlangCompilationFailed("Failed to create SLANG global session".to_string())
    })?;

    // Search the shader's directory and, so `import camera;` resolves, the
    // `modules` directory beside it (`shaders/modules` for a shader in
    // `shaders/passes`), each only when it is inside the project.
    let shader_dir = path.parent().unwrap_or(Path::new("."));
    let mut search_paths = Vec::new();
    for directory in [
        Some(shader_dir.to_path_buf()),
        shader_dir.parent().map(|p| p.join("modules")),
    ]
    .into_iter()
    .flatten()
    {
        if inside(&directory) {
            search_paths.push(
                CString::new(directory.to_string_lossy().as_bytes()).map_err(|e| {
                    ShaderError::SlangCompilationFailed(format!("Invalid search path: {:?}", e))
                })?,
            );
        }
    }
    let search_paths_ptrs: Vec<_> = search_paths.iter().map(|path| path.as_ptr()).collect();

    let target_desc = slang::TargetDesc::default().format(slang::CompileTarget::Wgsl);
    let targets = [target_desc];

    let options = slang::CompilerOptions::default().matrix_layout_column(true);

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
    for dependency in module.dependency_file_paths() {
        if !inside(Path::new(dependency)) {
            return Err(ShaderError::SlangCompilationFailed(format!(
                "{} reads {dependency}, which is outside the project",
                path.display()
            )));
        }
    }

    let vertex_entry = module.find_entry_point_by_name("vs_main").ok_or_else(|| {
        ShaderError::SlangCompilationFailed("Vertex entry point 'vs_main' not found".to_string())
    })?;

    let fragment_entry = module.find_entry_point_by_name("fs_main").ok_or_else(|| {
        ShaderError::SlangCompilationFailed("Fragment entry point 'fs_main' not found".to_string())
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

    let wgsl = code.as_str().map(|s| s.to_string()).map_err(|e| {
        ShaderError::SlangCompilationFailed(format!("Invalid UTF-8 in WGSL output: {:?}", e))
    })?;

    Ok(fixup_slang_wgsl(&wgsl))
}

/// Fix known SLANG WGSL backend issues:
/// 1. Depth textures emitted as `texture_2d<f32>` instead of `texture_depth_2d`
/// 2. `textureLoad(depth_tex, ...).x` should be `textureLoad(depth_tex, ...)` (returns f32)
#[cfg(feature = "slang")]
fn fixup_slang_wgsl(wgsl: &str) -> String {
    let mut depth_texture_names: Vec<String> = Vec::new();

    // Detect depth textures: used with textureSampleCompareLevel / textureSampleCompare
    for line in wgsl.lines() {
        for func in &["textureSampleCompareLevel(", "textureSampleCompare("] {
            if let Some(pos) = line.find(func) {
                let after = &line[pos + func.len()..];
                // Pattern: ((texture_name), ...) — extract name between (( and ))
                if let Some(rest) = after.strip_prefix('(') {
                    if let Some(end) = rest.find(')') {
                        let name = rest[..end].trim().to_string();
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
                    if (name.contains("depth") || name.contains("shadow"))
                        && !depth_texture_names.contains(&name)
                    {
                        depth_texture_names.push(name);
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
            for (i, &byte) in bytes.iter().enumerate().skip(call_open) {
                match byte {
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

/// Where `naive build` writes the WGSL compiled from a Slang shader:
/// `bloom.slang` becomes `bloom.slang.wgsl` in the same directory.
pub fn precompiled_wgsl_path(slang_path: &Path) -> std::path::PathBuf {
    let mut name = slang_path.as_os_str().to_owned();
    name.push(".wgsl");
    std::path::PathBuf::from(name)
}

/// The WGSL artifact beside a Slang shader inside `project_root`, held to the
/// same confinement as the source: the source may be absent, and a symlinked
/// artifact must not lead outside the project. Read artifacts only through
/// this.
pub(crate) fn resolve_precompiled_wgsl(
    slang_path: &Path,
    project_root: &Path,
) -> Result<std::path::PathBuf, String> {
    let artifact = precompiled_wgsl_path(slang_path);
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project {}: {error}", project_root.display()))?;
    let relative = artifact
        .strip_prefix(project_root)
        .or_else(|_| artifact.strip_prefix(&root))
        .map_err(|_| {
            format!(
                "shader artifact {} is outside the project",
                artifact.display()
            )
        })?;
    crate::project_path::resolve(project_root, relative)
        .map_err(|error| format!("shader artifact {}: {error}", artifact.display()))
}

/// Without the Slang compiler, use the WGSL that `naive build` compiled from
/// this shader, if there is one (as in a shipped game bundle).
#[cfg(not(feature = "slang"))]
fn compile_slang_to_wgsl(path: &Path, project_root: &Path) -> Result<String, ShaderError> {
    let precompiled = resolve_precompiled_wgsl(path, project_root)
        .map_err(ShaderError::SlangCompilationFailed)?;
    tracing::debug!("Loading precompiled WGSL {:?}", precompiled);
    std::fs::read_to_string(&precompiled).map_err(|_| {
        ShaderError::SlangCompilationFailed(format!(
            "SLANG support not compiled in (feature 'slang' disabled) and no precompiled {}",
            precompiled.display()
        ))
    })
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
    has_texture: f32,
    has_skin: f32,
    emission: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> draw: DrawUniforms;
@group(2) @binding(0) var albedo_texture: texture_2d<f32>;
@group(2) @binding(1) var albedo_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) joint_indices: vec4<u32>,
    @location(5) joint_weights: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
    @location(3) vertex_color: vec4<f32>,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = draw.model_matrix * vec4<f32>(model.position, 1.0);
    out.clip_position = camera.view_projection * world_pos;
    out.world_normal = normalize((draw.normal_matrix * vec4<f32>(model.normal, 0.0)).xyz);
    out.world_pos = world_pos.xyz;
    out.tex_coords = model.tex_coords;
    out.vertex_color = model.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let light_dir = normalize(vec3<f32>(0.3, 1.0, 0.5));
    let view_dir = normalize(camera.position - in.world_pos);
    let half_vec = normalize(light_dir + view_dir);

    let ndotl = max(dot(in.world_normal, light_dir), 0.0);
    let ndoth = max(dot(in.world_normal, half_vec), 0.0);

    var base = draw.base_color.rgb * in.vertex_color.rgb;
    if (draw.has_texture > 0.5) {
        let tex_color = textureSample(albedo_texture, albedo_sampler, in.tex_coords);
        base = base * tex_color.rgb;
    }

    let diffuse_color = base * (1.0 - draw.metallic);
    let F0 = mix(vec3<f32>(0.04), base, draw.metallic);

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

/// A project's own forward shader, which replaces the engine's for scenes
/// drawn without a render pipeline. `naive build` packages its WGSL.
pub const FORWARD_SHADER: &str = "shaders/passes/mesh_forward.slang";

/// The forward shader for scenes drawn without a render pipeline: the
/// project's [`FORWARD_SHADER`] when it has one, otherwise the engine's.
pub fn load_project_forward_shader(project_root: &Path) -> String {
    match crate::project_path::resolve(project_root, FORWARD_SHADER) {
        Ok(path) => compile_mesh_forward_shader(Some(&path), project_root)
            .unwrap_or_else(|_| get_mesh_forward_wgsl()),
        Err(error) => {
            tracing::warn!("Forward shader {FORWARD_SHADER}: {error}; using the engine's");
            get_mesh_forward_wgsl()
        }
    }
}

/// Load the forward shader at `slang_path`, or the WGSL `naive build`
/// compiled from it (all a bundled game has), checked like any required
/// shader. Falls back to the engine's forward shader when there is none or
/// it fails.
pub fn compile_mesh_forward_shader(
    slang_path: Option<&std::path::Path>,
    project_root: &Path,
) -> Result<String, ShaderError> {
    if let Some(path) = slang_path {
        if !path.exists() && !precompiled_wgsl_path(path).exists() {
            tracing::debug!(
                "No project forward shader at {:?}; using the engine's",
                path
            );
            return Ok(get_mesh_forward_wgsl());
        }

        match load_required_shader(path, project_root) {
            Ok(wgsl) => {
                tracing::info!("Forward shader loaded: {:?}", path);
                return Ok(wgsl);
            }
            Err(e) => {
                tracing::warn!(
                    "Forward shader {:?} failed: {}, using the engine's",
                    path,
                    e
                );
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

// One entry per visible entity, 256 bytes each (matches the CPU struct).
struct DrawUniforms {
    model_matrix: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    has_texture: f32,
    has_skin: f32,
    emission: vec4<f32>,
    _padding: array<vec4<f32>, 5>,
};

struct SkinUniforms {
    joint_count: u32,
    has_skin: u32,
    _pad: vec2<u32>,
    matrices: array<mat4x4<f32>, 128>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
// Entities sharing a mesh and texture are drawn in one instanced draw; the
// instance index selects the entity's entry.
@group(1) @binding(0) var<storage, read> draws: array<DrawUniforms>;
@group(2) @binding(0) var albedo_texture: texture_2d<f32>;
@group(2) @binding(1) var albedo_sampler: sampler;
@group(3) @binding(0) var<storage, read> skin: SkinUniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) joint_indices: vec4<u32>,
    @location(5) joint_weights: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
    @location(3) vertex_color: vec4<f32>,
    @location(4) @interpolate(flat) draw_index: u32,
};

struct GBufferOutput {
    @location(0) albedo: vec4<f32>,   // rgb = base_color, a = roughness
    @location(1) normal: vec4<f32>,   // rgb = packed normal, a = metallic
    @location(2) emission: vec4<f32>, // rgb = emission color, a = 0
};

// Only entities with an animator are skinned. Static meshes have joint 0 at
// full weight, so skinning them would move them with the uploaded palette.
fn apply_skinning(entity_has_skin: bool, joints: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    if (skin.has_skin == 0u || !entity_has_skin) {
        return mat4x4<f32>(
            vec4<f32>(1.0, 0.0, 0.0, 0.0),
            vec4<f32>(0.0, 1.0, 0.0, 0.0),
            vec4<f32>(0.0, 0.0, 1.0, 0.0),
            vec4<f32>(0.0, 0.0, 0.0, 1.0),
        );
    }
    let m0 = skin.matrices[joints.x];
    let m1 = skin.matrices[joints.y];
    let m2 = skin.matrices[joints.z];
    let m3 = skin.matrices[joints.w];
    return m0 * weights.x + m1 * weights.y + m2 * weights.z + m3 * weights.w;
}

@vertex
fn vs_main(model: VertexInput, @builtin(instance_index) instance: u32) -> VertexOutput {
    var out: VertexOutput;
    let draw = draws[instance];

    let skin_matrix = apply_skinning(draw.has_skin > 0.5, model.joint_indices, model.joint_weights);
    let skinned_pos = skin_matrix * vec4<f32>(model.position, 1.0);
    let skinned_normal = (skin_matrix * vec4<f32>(model.normal, 0.0)).xyz;

    let world_pos = draw.model_matrix * skinned_pos;
    out.clip_position = camera.view_projection * world_pos;
    out.world_normal = normalize((draw.normal_matrix * vec4<f32>(skinned_normal, 0.0)).xyz);
    out.world_pos = world_pos.xyz;
    out.tex_coords = model.tex_coords;
    out.vertex_color = model.color;
    out.draw_index = instance;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> GBufferOutput {
    var out: GBufferOutput;
    let draw = draws[in.draw_index];
    var albedo = draw.base_color.rgb * in.vertex_color.rgb;
    if (draw.has_texture > 0.5) {
        let tex_color = textureSample(albedo_texture, albedo_sampler, in.tex_coords);
        albedo = albedo * tex_color.rgb;
    }
    out.albedo = vec4<f32>(albedo, draw.roughness);
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
    // Shadow cascades: each one's light view-projection, the view depth where
    // each one ends and its depth bias. Cascade i uses tile (i % 2, i / 2).
    cascade_vp: array<mat4x4<f32>, 4>,
    cascade_splits: vec4<f32>,
    cascade_bias: vec4<f32>,
    // Sky: enabled (0 or 1), intensity, highest mip level, unused; then its
    // diffuse light as nine harmonics coefficients. See sky.rs.
    sky: vec4<f32>,
    sky_irradiance: array<vec4<f32>, 9>,
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
@group(2) @binding(3) var sky_map: texture_2d<f32>;
@group(2) @binding(4) var sky_sampler: sampler;

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
    // The nearest cascade that reaches this point's distance from the camera.
    let view_depth = -(camera.view * vec4<f32>(world_pos, 1.0)).z;
    var cascade = 4u;
    for (var i = 0u; i < 4u; i = i + 1u) {
        if view_depth <= lighting.cascade_splits[i] {
            cascade = i;
            break;
        }
    }
    // Beyond the last cascade shadows end.
    if cascade >= 4u {
        return 1.0;
    }

    let light_clip = lighting.cascade_vp[cascade] * vec4<f32>(world_pos, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;
    let local_uv = vec2<f32>(light_ndc.x * 0.5 + 0.5, -light_ndc.y * 0.5 + 0.5);
    let shadow_depth = light_ndc.z;

    // Nothing outside the cascade, beside it or past its far plane, was
    // drawn into it, so such points are lit.
    if local_uv.x < 0.0 || local_uv.x > 1.0 || local_uv.y < 0.0 || local_uv.y > 1.0
        || shadow_depth > 1.0 {
        return 1.0;
    }

    let texel_size = 1.0 / vec2<f32>(textureDimensions(shadow_map));
    let tile = vec2<f32>(f32(cascade % 2u), f32(cascade / 2u)) * 0.5;
    // Keep the 5x5 filter inside this cascade's tile.
    let shadow_uv = clamp(tile + local_uv * 0.5, tile + texel_size * 2.5,
        tile + vec2<f32>(0.5) - texel_size * 2.5);

    var shadow = 0.0;
    for (var y = -2i; y <= 2i; y = y + 1i) {
        for (var x = -2i; x <= 2i; x = x + 1i) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            shadow = shadow + textureSampleCompareLevel(shadow_map, shadow_sampler, shadow_uv + offset, shadow_depth - lighting.cascade_bias[cascade]);
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

// Diffuse light from the sky for a surface facing `n`, divided by pi.
fn sky_diffuse(n: vec3<f32>) -> vec3<f32> {
    let s = lighting.sky_irradiance;
    let e = s[0].rgb * 0.282095
        + s[1].rgb * (0.488603 * n.y) + s[2].rgb * (0.488603 * n.z) + s[3].rgb * (0.488603 * n.x)
        + s[4].rgb * (1.092548 * n.x * n.y) + s[5].rgb * (1.092548 * n.y * n.z)
        + s[6].rgb * (0.315392 * (3.0 * n.z * n.z - 1.0))
        + s[7].rgb * (1.092548 * n.x * n.z) + s[8].rgb * (0.546274 * (n.x * n.x - n.y * n.y));
    return max(e, vec3<f32>(0.0));
}

// The sky seen in direction `d`, blurred to mip level `lod`.
fn sample_sky(d: vec3<f32>, lod: f32) -> vec3<f32> {
    let uv = vec2<f32>(atan2(d.z, d.x) * 0.15915494 + 0.5, acos(clamp(d.y, -1.0, 1.0)) * 0.31830988);
    return textureSampleLevel(sky_map, sky_sampler, uv, lod).rgb;
}

// Share of reflected sky light for a reflectance, roughness and viewing angle
// (Karis' analytic fit of the split-sum lookup table).
fn env_brdf(F0: vec3<f32>, roughness: f32, NdotV: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * NdotV)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return F0 * ab.x + ab.y;
}

// Light that does not come from the scene's lights: the sky's, when there
// is one, else a faint constant.
fn ambient_light(diffuse_color: vec3<f32>, F0: vec3<f32>, roughness: f32,
                 normal: vec3<f32>, view_dir: vec3<f32>) -> vec3<f32> {
    if lighting.sky.x < 0.5 {
        return diffuse_color * vec3<f32>(0.02, 0.02, 0.025);
    }
    let NdotV = max(dot(normal, view_dir), 0.001);
    let diffuse = diffuse_color * sky_diffuse(normal);
    let reflected = reflect(-view_dir, normal);
    let specular = sample_sky(reflected, roughness * lighting.sky.z) * env_brdf(F0, roughness, NdotV);
    return (diffuse + specular) * lighting.sky.y;
}

// What the camera sees where there is no geometry: the sky and the sun.
fn sky_background(dir: vec3<f32>) -> vec3<f32> {
    var color = sample_sky(dir, 0.0) * lighting.sky.y;
    if lighting.has_directional != 0u {
        let sun = normalize(-lighting.dir_light_direction);
        let c = max(dot(dir, sun), 0.0);
        let disc = smoothstep(0.9995, 0.9998, c) * 8.0 + pow(c, 256.0) * 0.5;
        color = color + lighting.dir_light_color * lighting.dir_light_intensity * disc;
    }
    return color;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_coords = vec2<i32>(in.position.xy);

    let albedo_rough = textureLoad(gbuffer_albedo, tex_coords, 0);
    let normal_metal = textureLoad(gbuffer_normal, tex_coords, 0);
    let emission_val = textureLoad(gbuffer_emission, tex_coords, 0);
    let depth = textureLoad(gbuffer_depth, tex_coords, 0);

    if depth >= 1.0 {
        if lighting.sky.x < 0.5 {
            discard;
        }
        let far_point = reconstruct_world_pos(in.uv, 1.0);
        return vec4<f32>(sky_background(normalize(far_point - camera.position)), 1.0);
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

    // Ambient: the sky's light, or a faint constant without a sky.
    var color = ambient_light(diffuse_color, F0, roughness, normal, view_dir);

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
///
/// Each splat's 3D covariance (its entity's transform, rotation and scale)
/// is projected to a 2D screen-space ellipse, as in 3D Gaussian Splatting
/// (Kerbl et al. 2023, after Zwicker et al.'s EWA splatting), and drawn as a
/// quad along the ellipse's axes, sized to where the splat's alpha falls
/// below 1/255. Splats behind the camera or well outside the view are
/// dropped. The draw order, and each splat's colour for this view (base
/// colour plus spherical harmonics), come from the GPU sort (`splat::sort`).
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
    _pad2: vec4<f32>,
    inv_view_projection: mat4x4<f32>,
};

struct GaussianSplat {
    position: vec3<f32>,
    opacity: f32,
    scale: vec3<f32>,
    _pad0: f32,
    // 3DGS order: w, x, y, z.
    rotation: vec4<f32>,
    sh_dc: vec3<f32>,
    _pad1: f32,
};

struct InstanceUniforms {
    model: mat4x4<f32>,
    view_model: mat4x4<f32>,
    planes: array<vec4<f32>, 5>,
    camera_local: vec3<f32>,
    sh_degree: u32,
    count: u32,
    padded: u32,
    _pad: vec2<u32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

@group(1) @binding(0) var<storage, read> splats: array<GaussianSplat>;
@group(1) @binding(1) var<storage, read> sorted_indices: array<u32>;
@group(1) @binding(2) var<uniform> instance: InstanceUniforms;
// Each splat's colour for this view, written by the sort.
@group(1) @binding(3) var<storage, read> colors: array<vec4<f32>>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // The fragment's offset from the splat's centre, in units where the
    // Gaussian is exp(-dot(offset, offset)).
    @location(0) offset: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) opacity: f32,
};

const MIN_ALPHA: f32 = 0.0039215686; // 1 / 255
const MAX_ALPHA: f32 = 0.99;
// Added to the 2D covariance so no splat is thinner than about a pixel.
const LOW_PASS: f32 = 0.3;
// The largest ellipse axis drawn, in pixels.
const MAX_AXIS: f32 = 4096.0;

// Rotation matrix from a unit quaternion in 3DGS order (w, x, y, z).
fn quat_to_mat3(q: vec4<f32>) -> mat3x3<f32> {
    let w = q.x; let x = q.y; let y = q.z; let z = q.w;
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

fn dropped() -> VertexOutput {
    var out: VertexOutput;
    // Outside the clip volume, so the quad is clipped away.
    out.clip_position = vec4<f32>(0.0, 0.0, 2.0, 1.0);
    out.offset = vec2<f32>(0.0);
    out.color = vec3<f32>(0.0);
    out.opacity = 0.0;
    return out;
}

@vertex
fn vs_main(
    @builtin(instance_index) instance_index: u32,
    @builtin(vertex_index) vertex_index: u32,
) -> VertexOutput {
    let splat_index = sorted_indices[instance_index];
    let splat = splats[splat_index];
    let opacity = min(splat.opacity, MAX_ALPHA);
    if opacity < MIN_ALPHA {
        return dropped();
    }

    let view_position = camera.view * instance.model * vec4<f32>(splat.position, 1.0);
    // View space looks down -z.
    let depth = -view_position.z;
    if depth <= camera.near_plane {
        return dropped();
    }
    let clip = camera.projection * view_position;
    let ndc = clip.xy / clip.w;
    if abs(ndc.x) > 1.3 || abs(ndc.y) > 1.3 {
        return dropped();
    }

    // The splat's axes in view space: T = V M R S, so its covariance is T T^T.
    let model3 = mat3x3<f32>(instance.model[0].xyz, instance.model[1].xyz, instance.model[2].xyz);
    let view3 = mat3x3<f32>(camera.view[0].xyz, camera.view[1].xyz, camera.view[2].xyz);
    let m = view3 * model3 * quat_to_mat3(splat.rotation);
    let axes = mat3x3<f32>(m[0] * splat.scale.x, m[1] * splat.scale.y, m[2] * splat.scale.z);

    // The projection's Jacobian at the centre, in pixels (y up). Like 3DGS,
    // clamp the centre's direction a little outside the view so splats at
    // the edge do not blow up.
    let focal = vec2<f32>(camera.projection[0][0], camera.projection[1][1]) * camera.viewport_size * 0.5;
    let limit = 1.3 / vec2<f32>(camera.projection[0][0], camera.projection[1][1]);
    let slope = clamp(view_position.xy / depth, -limit, limit);
    let j0 = vec3<f32>(focal.x / depth, 0.0, focal.x * slope.x / depth);
    let j1 = vec3<f32>(0.0, focal.y / depth, focal.y * slope.y / depth);
    // Rows of J T; the 2D covariance is (J T)(J T)^T.
    let r0 = vec3<f32>(dot(j0, axes[0]), dot(j0, axes[1]), dot(j0, axes[2]));
    let r1 = vec3<f32>(dot(j1, axes[0]), dot(j1, axes[1]), dot(j1, axes[2]));
    let a = dot(r0, r0) + LOW_PASS;
    let b = dot(r0, r1);
    let c = dot(r1, r1) + LOW_PASS;

    // The ellipse's axes: eigenvectors of the covariance, scaled so the
    // Gaussian is exp(-|q|^2) for q in axis units.
    let mid = 0.5 * (a + c);
    let radius = length(vec2<f32>(0.5 * (a - c), b));
    let lambda1 = mid + radius;
    let lambda2 = max(mid - radius, 0.1);
    var direction = vec2<f32>(b, lambda1 - a);
    if dot(direction, direction) < 1e-12 {
        direction = vec2<f32>(1.0, 0.0);
    }
    direction = normalize(direction);
    let major = min(sqrt(2.0 * lambda1), MAX_AXIS) * direction;
    let minor = min(sqrt(2.0 * lambda2), MAX_AXIS) * vec2<f32>(direction.y, -direction.x);

    // Stop where opacity * exp(-|q|^2) reaches 1/255.
    let extent = sqrt(log(opacity / MIN_ALPHA));
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );
    let corner = corners[vertex_index] * extent;
    let offset_pixels = corner.x * major + corner.y * minor;
    let offset_ndc = offset_pixels / (0.5 * camera.viewport_size);

    var out: VertexOutput;
    out.clip_position = vec4<f32>(clip.xy + offset_ndc * clip.w, clip.z, clip.w);
    out.offset = corner;
    out.color = colors[splat_index].rgb;
    out.opacity = opacity;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = in.opacity * exp(-dot(in.offset, in.offset));
    if alpha < MIN_ALPHA {
        discard;
    }
    // Premultiplied.
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
    // Shadow cascades: each one's light view-projection, the view depth where
    // each one ends and its depth bias. Cascade i uses tile (i % 2, i / 2).
    cascade_vp: array<mat4x4<f32>, 4>,
    cascade_splits: vec4<f32>,
    cascade_bias: vec4<f32>,
    // Sky: enabled (0 or 1), intensity, highest mip level, unused; then its
    // diffuse light as nine harmonics coefficients. See sky.rs.
    sky: vec4<f32>,
    sky_irradiance: array<vec4<f32>, 9>,
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
@group(2) @binding(3) var sky_map: texture_2d<f32>;
@group(2) @binding(4) var sky_sampler: sampler;

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
    // The nearest cascade that reaches this point's distance from the camera.
    let view_depth = -(camera.view * vec4<f32>(world_pos, 1.0)).z;
    var cascade = 4u;
    for (var i = 0u; i < 4u; i = i + 1u) {
        if view_depth <= lighting.cascade_splits[i] {
            cascade = i;
            break;
        }
    }
    // Beyond the last cascade shadows end.
    if cascade >= 4u {
        return 1.0;
    }

    let light_clip = lighting.cascade_vp[cascade] * vec4<f32>(world_pos, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;
    let local_uv = vec2<f32>(light_ndc.x * 0.5 + 0.5, -light_ndc.y * 0.5 + 0.5);
    let shadow_depth = light_ndc.z;

    // Nothing outside the cascade, beside it or past its far plane, was
    // drawn into it, so such points are lit.
    if local_uv.x < 0.0 || local_uv.x > 1.0 || local_uv.y < 0.0 || local_uv.y > 1.0
        || shadow_depth > 1.0 {
        return 1.0;
    }

    let texel_size = 1.0 / vec2<f32>(textureDimensions(shadow_map));
    let tile = vec2<f32>(f32(cascade % 2u), f32(cascade / 2u)) * 0.5;
    // Keep the 5x5 filter inside this cascade's tile.
    let shadow_uv = clamp(tile + local_uv * 0.5, tile + texel_size * 2.5,
        tile + vec2<f32>(0.5) - texel_size * 2.5);

    var shadow = 0.0;
    for (var y = -2i; y <= 2i; y = y + 1i) {
        for (var x = -2i; x <= 2i; x = x + 1i) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            shadow = shadow + textureSampleCompareLevel(shadow_map, shadow_sampler, shadow_uv + offset, shadow_depth - lighting.cascade_bias[cascade]);
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

// Diffuse light from the sky for a surface facing `n`, divided by pi.
fn sky_diffuse(n: vec3<f32>) -> vec3<f32> {
    let s = lighting.sky_irradiance;
    let e = s[0].rgb * 0.282095
        + s[1].rgb * (0.488603 * n.y) + s[2].rgb * (0.488603 * n.z) + s[3].rgb * (0.488603 * n.x)
        + s[4].rgb * (1.092548 * n.x * n.y) + s[5].rgb * (1.092548 * n.y * n.z)
        + s[6].rgb * (0.315392 * (3.0 * n.z * n.z - 1.0))
        + s[7].rgb * (1.092548 * n.x * n.z) + s[8].rgb * (0.546274 * (n.x * n.x - n.y * n.y));
    return max(e, vec3<f32>(0.0));
}

// The sky seen in direction `d`, blurred to mip level `lod`.
fn sample_sky(d: vec3<f32>, lod: f32) -> vec3<f32> {
    let uv = vec2<f32>(atan2(d.z, d.x) * 0.15915494 + 0.5, acos(clamp(d.y, -1.0, 1.0)) * 0.31830988);
    return textureSampleLevel(sky_map, sky_sampler, uv, lod).rgb;
}

// Share of reflected sky light for a reflectance, roughness and viewing angle
// (Karis' analytic fit of the split-sum lookup table).
fn env_brdf(F0: vec3<f32>, roughness: f32, NdotV: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * NdotV)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return F0 * ab.x + ab.y;
}

// Light that does not come from the scene's lights: the sky's, when there
// is one, else a faint constant.
fn ambient_light(diffuse_color: vec3<f32>, F0: vec3<f32>, roughness: f32,
                 normal: vec3<f32>, view_dir: vec3<f32>) -> vec3<f32> {
    if lighting.sky.x < 0.5 {
        return diffuse_color * vec3<f32>(0.02, 0.02, 0.025);
    }
    let NdotV = max(dot(normal, view_dir), 0.001);
    let diffuse = diffuse_color * sky_diffuse(normal);
    let reflected = reflect(-view_dir, normal);
    let specular = sample_sky(reflected, roughness * lighting.sky.z) * env_brdf(F0, roughness, NdotV);
    return (diffuse + specular) * lighting.sky.y;
}

// What the camera sees where there is no geometry: the sky and the sun.
fn sky_background(dir: vec3<f32>) -> vec3<f32> {
    var color = sample_sky(dir, 0.0) * lighting.sky.y;
    if lighting.has_directional != 0u {
        let sun = normalize(-lighting.dir_light_direction);
        let c = max(dot(dir, sun), 0.0);
        let disc = smoothstep(0.9995, 0.9998, c) * 8.0 + pow(c, 256.0) * 0.5;
        color = color + lighting.dir_light_color * lighting.dir_light_intensity * disc;
    }
    return color;
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
    var mesh_color = ambient_light(diffuse_color, F0, roughness, normal, view_dir);

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
    let has_sky = lighting.sky.x >= 0.5;

    // Behind everything: the sky, if the scene has one.
    var background = vec3<f32>(0.0);
    if has_sky && !mesh_valid {
        let far_point = reconstruct_world_pos(in.uv, 1.0);
        background = sky_background(normalize(far_point - camera.position));
    }

    // Composite: prefer closer of mesh vs splat, blend if overlapping
    if splat_valid && (!mesh_valid || splat_depth < mesh_depth) {
        let bg = select(background, mesh_color, mesh_valid);
        let blended = splat_color.rgb + bg * (1.0 - splat_color.a);
        return vec4<f32>(blended, 1.0);
    } else if mesh_valid {
        return vec4<f32>(mesh_color, 1.0);
    } else if has_sky {
        return vec4<f32>(background, 1.0);
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
    has_texture: f32,
    has_skin: f32,
    emission: vec4<f32>,
    _padding: array<vec4<f32>, 5>,
};

struct SkinUniforms {
    joint_count: u32,
    has_skin: u32,
    _pad: vec2<u32>,
    matrices: array<mat4x4<f32>, 128>,
};

@group(0) @binding(0) var<uniform> shadow: ShadowUniforms;
// Indexed by instance, as in the G-buffer shader.
@group(1) @binding(0) var<storage, read> draws: array<DrawUniforms>;
@group(2) @binding(0) var<storage, read> skin: SkinUniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tex_coords: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) joint_indices: vec4<u32>,
    @location(5) joint_weights: vec4<f32>,
};

@vertex
fn vs_main(model: VertexInput, @builtin(instance_index) instance: u32) -> @builtin(position) vec4<f32> {
    let draw = draws[instance];
    var pos = vec4<f32>(model.position, 1.0);
    if (skin.has_skin > 0u && draw.has_skin > 0.5) {
        let m0 = skin.matrices[model.joint_indices.x];
        let m1 = skin.matrices[model.joint_indices.y];
        let m2 = skin.matrices[model.joint_indices.z];
        let m3 = skin.matrices[model.joint_indices.w];
        let skin_matrix = m0 * model.joint_weights.x + m1 * model.joint_weights.y + m2 * model.joint_weights.z + m3 * model.joint_weights.w;
        pos = skin_matrix * pos;
    }
    return shadow.light_view_projection * (draw.model_matrix * pos);
}

@fragment
fn fs_main() {
    // Depth-only pass — no color output needed
}
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse and validate a WGSL source the way wgpu would, without a GPU.
    fn validate(name: &str, source: &str) -> naga::Module {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{name} does not parse:\n{}", e.emit_to_string(source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name} does not validate:\n{}", e.emit_to_string(source)));
        module
    }

    #[test]
    fn built_in_shaders_are_valid_wgsl() {
        let shaders: [(&str, String); 12] = [
            ("triangle", get_triangle_wgsl()),
            ("mesh forward", get_mesh_forward_wgsl()),
            ("gbuffer", get_gbuffer_wgsl()),
            ("deferred light", get_deferred_light_wgsl()),
            ("splat render", get_splat_render_wgsl()),
            (
                "deferred light with splats",
                get_deferred_light_with_splats_wgsl(),
            ),
            ("bloom", get_bloom_wgsl()),
            ("tonemap", get_tonemap_wgsl()),
            ("fxaa", get_fxaa_wgsl()),
            ("shadow depth", get_shadow_depth_wgsl()),
            (
                "swarm compute",
                crate::swarm::SWARM_COMPUTE_WGSL.to_string(),
            ),
            ("splat sort", crate::splat::sort::SORT_WGSL.to_string()),
        ];
        for (name, source) in &shaders {
            validate(name, source);
        }
    }

    /// The instanced shaders and the swarm compute shader index arrays of
    /// `DrawUniforms` written from the CPU struct, so every copy of the struct
    /// must have the CPU struct's size.
    #[test]
    fn draw_uniforms_match_the_cpu_struct() {
        let shaders = [
            ("gbuffer", get_gbuffer_wgsl()),
            ("shadow depth", get_shadow_depth_wgsl()),
            (
                "swarm compute",
                crate::swarm::SWARM_COMPUTE_WGSL.to_string(),
            ),
        ];
        for (name, source) in &shaders {
            let module = validate(name, source);
            let span = module
                .types
                .iter()
                .find_map(|(_, ty)| match (&ty.name, &ty.inner) {
                    (Some(n), naga::TypeInner::Struct { span, .. }) if n == "DrawUniforms" => {
                        Some(*span)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} has no DrawUniforms struct"));
            assert_eq!(
                u64::from(span),
                crate::renderer::DRAW_UNIFORM_SIZE,
                "{name}'s DrawUniforms is {span} bytes"
            );
        }
    }

    /// The splat sort and draw read one entity's `InstanceUniforms` buffer,
    /// written from the CPU struct.
    #[test]
    fn splat_instance_uniforms_match_the_cpu_struct() {
        let shaders = [
            ("splat render", get_splat_render_wgsl()),
            ("splat sort", crate::splat::sort::SORT_WGSL.to_string()),
        ];
        for (name, source) in &shaders {
            let module = validate(name, source);
            let span = module
                .types
                .iter()
                .find_map(|(_, ty)| match (&ty.name, &ty.inner) {
                    (Some(n), naga::TypeInner::Struct { span, .. }) if n == "InstanceUniforms" => {
                        Some(*span)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} has no InstanceUniforms struct"));
            assert_eq!(
                span as usize,
                std::mem::size_of::<crate::splat::sort::InstanceUniforms>(),
                "{name}'s InstanceUniforms is {span} bytes"
            );
        }
    }
}

/// A shader's WGSL artifact is held to the project like its source, with or
/// without the Slang compiler: a symlink cannot make the engine load a file
/// from elsewhere.
#[cfg(all(test, unix))]
mod artifact_confinement_tests {
    use super::*;

    #[test]
    fn a_forward_shader_artifact_symlinked_outside_the_project_is_not_loaded() {
        let base =
            std::env::temp_dir().join(format!("naive-artifact-confine-{}", std::process::id()));
        let project = base.join("project");
        std::fs::create_dir_all(project.join("shaders/passes")).unwrap();
        let outside = base.join("outside.wgsl");
        let leaked = format!("{}\n// outside the project\n", get_mesh_forward_wgsl());
        std::fs::write(&outside, &leaked).unwrap();
        let artifact = project.join(precompiled_wgsl_path(Path::new(FORWARD_SHADER)));
        std::os::unix::fs::symlink(&outside, &artifact).unwrap();
        assert_eq!(
            load_project_forward_shader(&project),
            get_mesh_forward_wgsl()
        );
        let slang = project.join(FORWARD_SHADER);
        let error = resolve_precompiled_wgsl(&slang, &project).unwrap_err();
        assert!(error.contains("outside the project"), "{error}");

        // The same artifact inside the project is the project's shader.
        std::fs::remove_file(&artifact).unwrap();
        std::fs::copy(&outside, &artifact).unwrap();
        assert_eq!(load_project_forward_shader(&project), leaked);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(all(test, not(feature = "slang")))]
mod precompiled_tests {
    use super::*;

    #[test]
    fn without_slang_the_precompiled_wgsl_beside_a_shader_is_used() {
        let dir = std::env::temp_dir().join(format!("naive-precompiled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let slang = dir.join("bloom.slang");
        std::fs::write(&slang, "// slang source").unwrap();
        assert!(compile_slang_to_wgsl(&slang, &dir).is_err());
        std::fs::write(precompiled_wgsl_path(&slang), "// compiled").unwrap();
        assert_eq!(compile_slang_to_wgsl(&slang, &dir).unwrap(), "// compiled");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Slang reads only files inside the project, however a shader names them.
#[cfg(all(test, feature = "slang"))]
mod slang_confinement_tests {
    use super::*;
    use std::path::PathBuf;

    const ENTRY_POINTS: &str = "struct VertexOutput { float4 position : SV_Position; };\n\
        [shader(\"vertex\")] VertexOutput vs_main(uint i : SV_VertexID) {\n\
            VertexOutput o; o.position = float4(0, 0, 0, 1); return o;\n\
        }\n\
        [shader(\"fragment\")] float4 fs_main(VertexOutput input) : SV_Target0 { return color(); }\n";
    const COLOR_MODULE: &str = "float4 color() { return float4(0, 1, 0, 1); }\n";

    struct Dirs(PathBuf);

    impl Dirs {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "naive-slang-confine-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(base.join("project")).unwrap();
            Self(base)
        }
        fn project(&self) -> PathBuf {
            self.0.join("project")
        }
        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn imports_and_includes_inside_the_project_compile() {
        let dirs = Dirs::new("inside");
        dirs.write("project/shaders/modules/palette.slang", COLOR_MODULE);
        dirs.write("project/shaders/passes/common.h", "#define UNUSED 1\n");
        let shader = dirs.write(
            "project/shaders/passes/inside.slang",
            &format!("#include \"common.h\"\nimport palette;\n{ENTRY_POINTS}"),
        );
        compile_slang_to_wgsl(&shader, &dirs.project()).unwrap();
    }

    #[test]
    fn files_outside_the_project_are_never_compiled_in() {
        let dirs = Dirs::new("outside");
        let leak = dirs.write("outside/leak.slang", COLOR_MODULE);
        let relative = dirs.write(
            "project/effects/relative.slang",
            &format!("import \"../../outside/leak.slang\";\n{ENTRY_POINTS}"),
        );
        let absolute = dirs.write(
            "project/effects/absolute.slang",
            &format!("#include \"{}\"\n{ENTRY_POINTS}", leak.display()),
        );
        std::os::unix::fs::symlink(&leak, dirs.project().join("effects/linked.slang")).unwrap();
        let symlinked = dirs.write(
            "project/effects/symlinked.slang",
            &format!("import linked;\n{ENTRY_POINTS}"),
        );
        for shader in [relative, absolute, symlinked] {
            let error = compile_slang_to_wgsl(&shader, &dirs.project()).unwrap_err();
            assert!(
                format!("{error:?}").contains("outside the project"),
                "{}: {error:?}",
                shader.display()
            );
        }

        // A shader at the project root used to search `../modules`.
        dirs.write("modules/palette.slang", COLOR_MODULE);
        let root = dirs.write(
            "project/root.slang",
            &format!("import palette;\n{ENTRY_POINTS}"),
        );
        assert!(compile_slang_to_wgsl(&root, &dirs.project()).is_err());
    }
}
