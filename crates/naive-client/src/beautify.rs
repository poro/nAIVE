//! Scene-to-Splat Beautification Pipeline.
//!
//! Exports scene geometry to GLB, sends to a beautification backend
//! (World Labs, Marble, local GPU), receives Gaussian Splat PLY,
//! and loads it into the scene as a visual overlay.

use std::path::{Path, PathBuf};

/// Beautification backend type.
#[derive(Debug, Clone)]
pub enum BeautifyBackend {
    /// Local GPU server (Hunyuan3D via GATEWAY_URL).
    LocalGpu {
        gateway_url: String,
        gateway_key: String,
    },
    /// World Labs / Marble cloud API.
    CloudApi {
        api_url: String,
        api_key: String,
    },
    /// Mock backend for testing: copies a pre-existing PLY.
    Mock {
        ply_path: PathBuf,
    },
}

/// Configuration for the beautification pipeline.
#[derive(Debug, Clone)]
pub struct BeautifyConfig {
    pub backend: BeautifyBackend,
    /// Style prompt for the beautification (e.g., "photorealistic", "stylized anime").
    pub style_prompt: Option<String>,
    /// Output PLY path (relative to project root).
    pub output_ply: String,
}

impl Default for BeautifyConfig {
    fn default() -> Self {
        Self {
            backend: BeautifyBackend::LocalGpu {
                gateway_url: String::new(),
                gateway_key: String::new(),
            },
            style_prompt: None,
            output_ply: "assets/splats/beautified.ply".to_string(),
        }
    }
}

/// Result of the beautification pipeline.
#[derive(Debug)]
pub struct BeautifyResult {
    /// Path to the generated PLY file (absolute).
    pub ply_path: PathBuf,
    /// Number of gaussians in the result.
    pub splat_count: Option<u32>,
    /// Backend that was used.
    pub backend_name: String,
}

/// Export scene geometry as a minimal GLB binary.
///
/// Combines all mesh entities' positions into a single GLB file.
/// For procedural meshes (cube, sphere), generates the geometry inline.
/// For file-based meshes (GLBs), re-reads from disk and merges.
pub fn export_scene_to_glb(
    project_root: &Path,
    scene: &naive_core::scene::SceneFile,
) -> Result<Vec<u8>, String> {
    let mut all_positions: Vec<[f32; 3]> = Vec::new();
    let mut all_normals: Vec<[f32; 3]> = Vec::new();
    let mut all_indices: Vec<u32> = Vec::new();

    for entity_def in &scene.entities {
        let mr = match &entity_def.components.mesh_renderer {
            Some(mr) => mr,
            None => continue,
        };

        let transform = entity_def.components.transform.as_ref();
        let model_matrix = transform
            .map(|t| {
                let pos = glam::Vec3::from(t.position);
                let rot = crate::world::euler_degrees_to_quat(t.rotation);
                let scale = glam::Vec3::from(t.scale);
                glam::Mat4::from_scale_rotation_translation(scale, rot, pos)
            })
            .unwrap_or(glam::Mat4::IDENTITY);

        let base_index = all_positions.len() as u32;

        if mr.mesh.starts_with("procedural:") {
            let (verts, norms, idxs) = generate_procedural_geometry(&mr.mesh);
            for (v, n) in verts.iter().zip(norms.iter()) {
                let p = model_matrix.transform_point3(glam::Vec3::from(*v));
                let nm = model_matrix
                    .inverse()
                    .transpose()
                    .transform_vector3(glam::Vec3::from(*n))
                    .normalize();
                all_positions.push(p.to_array());
                all_normals.push(nm.to_array());
            }
            for idx in idxs {
                all_indices.push(base_index + idx);
            }
        } else {
            // File-based mesh: try to read from project
            let mesh_path = project_root.join(&mr.mesh);
            if mesh_path.exists() {
                match read_glb_geometry(&mesh_path) {
                    Ok((verts, norms, idxs)) => {
                        for (v, n) in verts.iter().zip(norms.iter()) {
                            let p = model_matrix.transform_point3(glam::Vec3::from(*v));
                            let nm = model_matrix
                                .inverse()
                                .transpose()
                                .transform_vector3(glam::Vec3::from(*n))
                                .normalize();
                            all_positions.push(p.to_array());
                            all_normals.push(nm.to_array());
                        }
                        for idx in idxs {
                            all_indices.push(base_index + idx);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Skipping mesh '{}' for entity '{}': {}",
                            mr.mesh,
                            entity_def.id,
                            e
                        );
                    }
                }
            } else {
                tracing::warn!(
                    "Mesh file not found: {:?}, skipping entity '{}'",
                    mesh_path,
                    entity_def.id
                );
            }
        }
    }

    if all_positions.is_empty() {
        return Err("No geometry to export".to_string());
    }

    encode_glb(&all_positions, &all_normals, &all_indices)
}

/// Send GLB to beautification backend and receive PLY.
pub fn beautify(
    config: &BeautifyConfig,
    glb_data: &[u8],
    project_root: &Path,
) -> Result<BeautifyResult, String> {
    let output_path = project_root.join(&config.output_ply);

    // Ensure output directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
    }

    match &config.backend {
        BeautifyBackend::Mock { ply_path } => {
            std::fs::copy(ply_path, &output_path)
                .map_err(|e| format!("Failed to copy mock PLY: {}", e))?;
            Ok(BeautifyResult {
                ply_path: output_path,
                splat_count: None,
                backend_name: "mock".to_string(),
            })
        }
        BeautifyBackend::LocalGpu {
            gateway_url,
            gateway_key,
        } => {
            send_to_gateway(gateway_url, gateway_key, glb_data, config, &output_path)
        }
        BeautifyBackend::CloudApi { api_url, api_key } => {
            send_to_cloud_api(api_url, api_key, glb_data, config, &output_path)
        }
    }
}

/// Full pipeline: export scene → send to backend → return result path.
pub fn beautify_scene(
    project_root: &Path,
    scene: &naive_core::scene::SceneFile,
    config: &BeautifyConfig,
) -> Result<BeautifyResult, String> {
    tracing::info!("Beautify: exporting scene geometry to GLB...");
    let glb_data = export_scene_to_glb(project_root, scene)?;
    tracing::info!("Beautify: exported {} bytes of GLB", glb_data.len());

    // Optionally save the intermediate GLB for debugging
    let glb_path = project_root.join("assets/splats/beautify_export.glb");
    if let Some(parent) = glb_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&glb_path, &glb_data);
    tracing::info!("Beautify: saved intermediate GLB to {:?}", glb_path);

    tracing::info!("Beautify: sending to backend...");
    let result = beautify(config, &glb_data, project_root)?;
    tracing::info!(
        "Beautify: received PLY at {:?} (backend: {})",
        result.ply_path,
        result.backend_name
    );

    Ok(result)
}

/// Build a BeautifyConfig from environment variables.
pub fn config_from_env() -> BeautifyConfig {
    let gateway_url = std::env::var("GATEWAY_URL").unwrap_or_default();
    let gateway_key = std::env::var("GATEWAY_KEY").unwrap_or_default();

    let backend = if !gateway_url.is_empty() {
        BeautifyBackend::LocalGpu {
            gateway_url,
            gateway_key,
        }
    } else {
        let api_url = std::env::var("WORLDLABS_API_URL")
            .or_else(|_| std::env::var("MARBLE_API_URL"))
            .unwrap_or_default();
        let api_key = std::env::var("WORLDLABS_API_KEY")
            .or_else(|_| std::env::var("MARBLE_API_KEY"))
            .unwrap_or_default();

        if !api_url.is_empty() {
            BeautifyBackend::CloudApi { api_url, api_key }
        } else {
            // Fallback to local GPU with empty URL (will error on use)
            BeautifyBackend::LocalGpu {
                gateway_url: String::new(),
                gateway_key: String::new(),
            }
        }
    };

    BeautifyConfig {
        backend,
        style_prompt: std::env::var("BEAUTIFY_STYLE").ok(),
        output_ply: "assets/splats/beautified.ply".to_string(),
    }
}

// --- Internal helpers ---

fn generate_procedural_geometry(mesh_type: &str) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    match mesh_type {
        "procedural:cube" => generate_cube(),
        "procedural:sphere" => generate_sphere(16, 16),
        _ => {
            tracing::warn!("Unknown procedural mesh type: {}, using cube", mesh_type);
            generate_cube()
        }
    }
}

fn generate_cube() -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    // Unit cube centered at origin
    let positions = vec![
        // Front face
        [-0.5, -0.5, 0.5], [0.5, -0.5, 0.5], [0.5, 0.5, 0.5], [-0.5, 0.5, 0.5],
        // Back face
        [0.5, -0.5, -0.5], [-0.5, -0.5, -0.5], [-0.5, 0.5, -0.5], [0.5, 0.5, -0.5],
        // Top face
        [-0.5, 0.5, 0.5], [0.5, 0.5, 0.5], [0.5, 0.5, -0.5], [-0.5, 0.5, -0.5],
        // Bottom face
        [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, -0.5, 0.5], [-0.5, -0.5, 0.5],
        // Right face
        [0.5, -0.5, 0.5], [0.5, -0.5, -0.5], [0.5, 0.5, -0.5], [0.5, 0.5, 0.5],
        // Left face
        [-0.5, -0.5, -0.5], [-0.5, -0.5, 0.5], [-0.5, 0.5, 0.5], [-0.5, 0.5, -0.5],
    ];
    let normals = vec![
        [0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0], [0.0, 0.0, -1.0], [0.0, 0.0, -1.0], [0.0, 0.0, -1.0],
        [0.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, -1.0, 0.0],
        [1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [-1.0, 0.0, 0.0],
    ];
    let indices = vec![
        0, 1, 2, 0, 2, 3,       // front
        4, 5, 6, 4, 6, 7,       // back
        8, 9, 10, 8, 10, 11,    // top
        12, 13, 14, 12, 14, 15, // bottom
        16, 17, 18, 16, 18, 19, // right
        20, 21, 22, 20, 22, 23, // left
    ];
    (positions, normals, indices)
}

fn generate_sphere(stacks: u32, slices: u32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    use std::f32::consts::PI;

    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();

    for i in 0..=stacks {
        let phi = PI * i as f32 / stacks as f32;
        for j in 0..=slices {
            let theta = 2.0 * PI * j as f32 / slices as f32;
            let x = phi.sin() * theta.cos();
            let y = phi.cos();
            let z = phi.sin() * theta.sin();
            positions.push([x * 0.5, y * 0.5, z * 0.5]);
            normals.push([x, y, z]);
        }
    }

    for i in 0..stacks {
        for j in 0..slices {
            let a = i * (slices + 1) + j;
            let b = a + slices + 1;
            indices.extend_from_slice(&[a, b, a + 1, b, b + 1, a + 1]);
        }
    }

    (positions, normals, indices)
}

/// Read geometry from an existing GLB/glTF file.
fn read_glb_geometry(
    path: &Path,
) -> Result<(Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>), String> {
    let (document, buffers, _) =
        gltf::import(path).map_err(|e| format!("Failed to import GLB: {}", e))?;

    let mut all_positions = Vec::new();
    let mut all_normals = Vec::new();
    let mut all_indices = Vec::new();

    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
            let base = all_positions.len() as u32;

            if let Some(positions) = reader.read_positions() {
                all_positions.extend(positions);
            }
            if let Some(normals) = reader.read_normals() {
                all_normals.extend(normals);
            }
            if let Some(indices) = reader.read_indices() {
                all_indices.extend(indices.into_u32().map(|i| i + base));
            }
        }
    }

    // Fill normals if missing
    while all_normals.len() < all_positions.len() {
        all_normals.push([0.0, 1.0, 0.0]);
    }

    Ok((all_positions, all_normals, all_indices))
}

/// Encode geometry as a minimal GLB (glTF Binary) file.
fn encode_glb(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
) -> Result<Vec<u8>, String> {
    let vertex_count = positions.len();
    let index_count = indices.len();

    // Binary buffer: positions + normals + indices
    let pos_bytes = vertex_count * 12; // 3 * f32
    let norm_bytes = vertex_count * 12;
    let idx_bytes = index_count * 4; // u32
    let buffer_size = pos_bytes + norm_bytes + idx_bytes;

    let mut bin_data = Vec::with_capacity(buffer_size);

    // Write positions
    for p in positions {
        for v in p {
            bin_data.extend_from_slice(&v.to_le_bytes());
        }
    }
    // Write normals
    for n in normals {
        for v in n {
            bin_data.extend_from_slice(&v.to_le_bytes());
        }
    }
    // Write indices
    for i in indices {
        bin_data.extend_from_slice(&i.to_le_bytes());
    }

    // Compute AABB for positions
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for p in positions {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }

    // Build glTF JSON
    let json = serde_json::json!({
        "asset": { "version": "2.0", "generator": "nAIVE Engine" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{
            "primitives": [{
                "attributes": {
                    "POSITION": 0,
                    "NORMAL": 1
                },
                "indices": 2,
                "mode": 4
            }]
        }],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126,
                "count": vertex_count,
                "type": "VEC3",
                "min": [min[0], min[1], min[2]],
                "max": [max[0], max[1], max[2]]
            },
            {
                "bufferView": 1,
                "componentType": 5126,
                "count": vertex_count,
                "type": "VEC3"
            },
            {
                "bufferView": 2,
                "componentType": 5125,
                "count": index_count,
                "type": "SCALAR"
            }
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": pos_bytes, "target": 34962 },
            { "buffer": 0, "byteOffset": pos_bytes, "byteLength": norm_bytes, "target": 34962 },
            { "buffer": 0, "byteOffset": pos_bytes + norm_bytes, "byteLength": idx_bytes, "target": 34963 }
        ],
        "buffers": [{ "byteLength": buffer_size }]
    });

    let json_str = serde_json::to_string(&json)
        .map_err(|e| format!("Failed to serialize glTF JSON: {}", e))?;
    let json_bytes = json_str.as_bytes();

    // Pad JSON to 4-byte alignment
    let json_padded_len = (json_bytes.len() + 3) & !3;
    let mut json_padded = json_bytes.to_vec();
    json_padded.resize(json_padded_len, b' ');

    // Pad binary to 4-byte alignment
    let bin_padded_len = (bin_data.len() + 3) & !3;
    bin_data.resize(bin_padded_len, 0);

    // GLB header: magic + version + total length
    let total_len = 12 + 8 + json_padded_len + 8 + bin_padded_len;
    let mut glb = Vec::with_capacity(total_len);

    // Header
    glb.extend_from_slice(b"glTF"); // magic
    glb.extend_from_slice(&2u32.to_le_bytes()); // version
    glb.extend_from_slice(&(total_len as u32).to_le_bytes()); // total length

    // JSON chunk
    glb.extend_from_slice(&(json_padded_len as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F534Au32.to_le_bytes()); // "JSON"
    glb.extend_from_slice(&json_padded);

    // BIN chunk
    glb.extend_from_slice(&(bin_padded_len as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E4942u32.to_le_bytes()); // "BIN\0"
    glb.extend_from_slice(&bin_data);

    Ok(glb)
}

/// Send GLB to local GPU gateway for beautification.
fn send_to_gateway(
    gateway_url: &str,
    gateway_key: &str,
    glb_data: &[u8],
    config: &BeautifyConfig,
    output_path: &Path,
) -> Result<BeautifyResult, String> {
    if gateway_url.is_empty() {
        return Err("GATEWAY_URL not set. Configure it in .env for local GPU beautification.".to_string());
    }

    let url = format!("{}/beautify", gateway_url.trim_end_matches('/'));
    let style = config.style_prompt.as_deref().unwrap_or("photorealistic");

    // Encode GLB as base64 for JSON transport
    use base64::Engine as _;
    let glb_b64 = base64::engine::general_purpose::STANDARD.encode(glb_data);

    let body = serde_json::json!({
        "glb_data": glb_b64,
        "style": style,
        "output_format": "ply"
    });

    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", gateway_key))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("Gateway request failed: {}", e))?;

    if response.status() != 200 {
        return Err(format!(
            "Gateway returned status {}: {}",
            response.status(),
            response.into_string().unwrap_or_default()
        ));
    }

    // Response should contain the PLY data (base64 encoded or raw)
    let response_body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse gateway response: {}", e))?;

    let ply_b64 = response_body["ply_data"]
        .as_str()
        .ok_or("Gateway response missing 'ply_data' field")?;

    let ply_bytes = base64::engine::general_purpose::STANDARD
        .decode(ply_b64)
        .map_err(|e| format!("Failed to decode PLY base64: {}", e))?;

    std::fs::write(output_path, &ply_bytes)
        .map_err(|e| format!("Failed to write PLY: {}", e))?;

    let splat_count = response_body["splat_count"].as_u64().map(|c| c as u32);

    Ok(BeautifyResult {
        ply_path: output_path.to_path_buf(),
        splat_count,
        backend_name: "local_gpu".to_string(),
    })
}

/// Send GLB to cloud API (World Labs / Marble).
fn send_to_cloud_api(
    api_url: &str,
    api_key: &str,
    glb_data: &[u8],
    config: &BeautifyConfig,
    output_path: &Path,
) -> Result<BeautifyResult, String> {
    if api_url.is_empty() {
        return Err(
            "Cloud API URL not set. Set WORLDLABS_API_URL or MARBLE_API_URL in .env.".to_string(),
        );
    }

    let style = config.style_prompt.as_deref().unwrap_or("photorealistic");

    use base64::Engine as _;
    let glb_b64 = base64::engine::general_purpose::STANDARD.encode(glb_data);

    let body = serde_json::json!({
        "input_glb": glb_b64,
        "style_prompt": style,
        "output_format": "gaussian_splat_ply",
    });

    let response = ureq::post(api_url)
        .set("Authorization", &format!("Bearer {}", api_key))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("Cloud API request failed: {}", e))?;

    if response.status() != 200 {
        return Err(format!(
            "Cloud API returned status {}: {}",
            response.status(),
            response.into_string().unwrap_or_default()
        ));
    }

    let response_body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse cloud API response: {}", e))?;

    // Try common response formats
    let ply_data = if let Some(b64) = response_body["ply_data"].as_str() {
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("Failed to decode PLY: {}", e))?
    } else if let Some(url) = response_body["download_url"].as_str() {
        // Download from provided URL
        let dl_response = ureq::get(url)
            .call()
            .map_err(|e| format!("Failed to download PLY: {}", e))?;
        let mut bytes = Vec::new();
        dl_response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Failed to read PLY download: {}", e))?;
        bytes
    } else {
        return Err("Cloud API response missing 'ply_data' or 'download_url'".to_string());
    };

    std::fs::write(output_path, &ply_data)
        .map_err(|e| format!("Failed to write PLY: {}", e))?;

    let splat_count = response_body["splat_count"].as_u64().map(|c| c as u32);

    Ok(BeautifyResult {
        ply_path: output_path.to_path_buf(),
        splat_count,
        backend_name: "cloud_api".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_cube() {
        let (positions, normals, indices) = generate_cube();
        assert_eq!(positions.len(), 24);
        assert_eq!(normals.len(), 24);
        assert_eq!(indices.len(), 36);
    }

    #[test]
    fn test_generate_sphere() {
        let (positions, normals, indices) = generate_sphere(8, 8);
        assert_eq!(positions.len(), normals.len());
        assert!(!indices.is_empty());
    }

    #[test]
    fn test_encode_glb() {
        let positions = vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let normals = vec![[0.0, 0.0, 1.0]; 3];
        let indices = vec![0, 1, 2];

        let glb = encode_glb(&positions, &normals, &indices).unwrap();

        // Check GLB magic number
        assert_eq!(&glb[0..4], b"glTF");
        // Check version
        assert_eq!(u32::from_le_bytes([glb[4], glb[5], glb[6], glb[7]]), 2);
    }

    #[test]
    fn test_config_from_env_defaults() {
        // With no env vars set, should get empty local GPU backend
        let config = config_from_env();
        assert_eq!(config.output_ply, "assets/splats/beautified.ply");
    }
}
