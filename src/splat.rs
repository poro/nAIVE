//! Gaussian splat loading, caching, and CPU sorting.
//!
//! Loads .ply files in standard 3DGS format (position, scale, rotation,
//! opacity, spherical harmonics) and uploads to GPU storage buffers.
//! Provides per-frame CPU depth sorting for correct alpha blending.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use linked_hash_map::LinkedHashMap;

use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::components::SplatHandle;

#[derive(Debug)]
pub enum SplatError {
    IoError(String),
    PlyError(String),
    NoVertices,
    MissingProperty(String),
}

impl std::fmt::Display for SplatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "Splat IO error: {}", msg),
            Self::PlyError(msg) => write!(f, "PLY parse error: {}", msg),
            Self::NoVertices => write!(f, "PLY file contains no vertices"),
            Self::MissingProperty(name) => write!(f, "PLY missing property: {}", name),
        }
    }
}

/// GPU-side splat data. Packed to 64 bytes for efficient storage buffer access.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GaussianSplatGpu {
    pub position: [f32; 3],
    pub opacity: f32,
    pub scale: [f32; 3],
    pub _pad0: f32,
    pub rotation: [f32; 4],
    pub sh_dc: [f32; 3],
    pub _pad1: f32,
}

/// A loaded GPU splat cloud.
pub struct GpuSplat {
    /// Storage buffer containing all splat data.
    pub splat_buffer: wgpu::Buffer,
    /// Buffer of sorted u32 indices (updated each frame).
    pub sorted_index_buffer: wgpu::Buffer,
    /// Number of splats in this cloud.
    pub splat_count: u32,
    /// CPU-side positions for depth sorting.
    pub cpu_positions: Vec<[f32; 3]>,
}

/// Cache of loaded splat clouds, keyed by file path.
pub struct SplatCache {
    splats: Vec<GpuSplat>,
    path_to_handle: HashMap<PathBuf, SplatHandle>,
}

impl SplatCache {
    pub fn new() -> Self {
        Self {
            splats: Vec::new(),
            path_to_handle: HashMap::new(),
        }
    }

    pub fn get_or_load(
        &mut self,
        device: &wgpu::Device,
        project_root: &Path,
        splat_path: &str,
    ) -> Result<SplatHandle, SplatError> {
        let key = PathBuf::from(splat_path);
        if let Some(&handle) = self.path_to_handle.get(&key) {
            return Ok(handle);
        }

        let gpu_splat = load_ply(device, project_root, splat_path)?;
        let handle = SplatHandle(self.splats.len());
        tracing::info!(
            "Loaded splat: {} ({} gaussians)",
            splat_path,
            gpu_splat.splat_count
        );
        self.splats.push(gpu_splat);
        self.path_to_handle.insert(key, handle);
        Ok(handle)
    }

    pub fn get(&self, handle: SplatHandle) -> &GpuSplat {
        &self.splats[handle.0]
    }

    /// Sort splats back-to-front for correct alpha blending.
    /// Updates the sorted_index_buffer on GPU.
    pub fn sort_splats(
        &self,
        handle: SplatHandle,
        view_matrix: &Mat4,
        queue: &wgpu::Queue,
    ) {
        let gpu_splat = &self.splats[handle.0];
        let count = gpu_splat.splat_count as usize;
        if count == 0 {
            return;
        }

        // Compute camera-space Z for each splat
        let mut indexed_depths: Vec<(u32, f32)> = gpu_splat
            .cpu_positions
            .iter()
            .enumerate()
            .map(|(i, pos)| {
                let world_pos = Vec3::from(*pos);
                let view_pos = view_matrix.transform_point3(world_pos);
                (i as u32, view_pos.z)
            })
            .collect();

        // Sort back-to-front (most negative Z = farthest in right-handed view space)
        indexed_depths.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Upload sorted indices to GPU
        let sorted_indices: Vec<u32> = indexed_depths.iter().map(|(i, _)| *i).collect();
        queue.write_buffer(
            &gpu_splat.sorted_index_buffer,
            0,
            bytemuck::cast_slice(&sorted_indices),
        );
    }

    /// Invalidate a cached splat (for hot-reload).
    pub fn invalidate(&mut self, splat_path: &str) {
        let key = PathBuf::from(splat_path);
        if let Some(handle) = self.path_to_handle.remove(&key) {
            tracing::info!("Invalidated splat cache: {} (handle {})", splat_path, handle.0);
            // The GpuSplat remains in the vec but the path mapping is removed,
            // so next get_or_load will reload it (with a new handle).
        }
    }

    /// Check if any splats are loaded.
    pub fn has_splats(&self) -> bool {
        !self.splats.is_empty()
    }
}

/// Sigmoid activation: 1 / (1 + exp(-x))
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// SH DC coefficient to linear color: c * C0 + 0.5
/// where C0 = 0.28209479 (Y_0^0 normalization constant)
fn sh_dc_to_color(c: f32) -> f32 {
    (c * 0.28209479 + 0.5).clamp(0.0, 1.0)
}

/// Load a PLY file in standard 3DGS format and upload to GPU.
fn load_ply(
    device: &wgpu::Device,
    project_root: &Path,
    splat_path: &str,
) -> Result<GpuSplat, SplatError> {
    let full_path = project_root.join(splat_path);

    if !full_path.exists() {
        // Generate a small procedural splat cloud as fallback
        tracing::warn!(
            "Splat file not found: {:?}, using procedural splat cloud",
            full_path
        );
        return Ok(create_procedural_splats(device));
    }

    let file =
        std::fs::File::open(&full_path).map_err(|e| SplatError::IoError(e.to_string()))?;
    let mut reader = std::io::BufReader::new(file);

    let parser = ply_rs::parser::Parser::<ply_rs::ply::DefaultElement>::new();
    let ply = parser
        .read_ply(&mut reader)
        .map_err(|e| SplatError::PlyError(format!("{:?}", e)))?;

    let vertices = ply
        .payload
        .get("vertex")
        .ok_or(SplatError::NoVertices)?;

    if vertices.is_empty() {
        return Err(SplatError::NoVertices);
    }

    let count = vertices.len();
    let mut gpu_data = Vec::with_capacity(count);
    let mut cpu_positions = Vec::with_capacity(count);

    for vertex in vertices {
        let x = get_float_property(vertex, "x")?;
        let y = get_float_property(vertex, "y")?;
        let z = get_float_property(vertex, "z")?;

        let scale_0 = get_float_property(vertex, "scale_0").unwrap_or(0.0);
        let scale_1 = get_float_property(vertex, "scale_1").unwrap_or(0.0);
        let scale_2 = get_float_property(vertex, "scale_2").unwrap_or(0.0);

        let rot_0 = get_float_property(vertex, "rot_0").unwrap_or(1.0);
        let rot_1 = get_float_property(vertex, "rot_1").unwrap_or(0.0);
        let rot_2 = get_float_property(vertex, "rot_2").unwrap_or(0.0);
        let rot_3 = get_float_property(vertex, "rot_3").unwrap_or(0.0);

        let opacity_raw = get_float_property(vertex, "opacity").unwrap_or(0.0);

        let f_dc_0 = get_float_property(vertex, "f_dc_0").unwrap_or(0.5);
        let f_dc_1 = get_float_property(vertex, "f_dc_1").unwrap_or(0.5);
        let f_dc_2 = get_float_property(vertex, "f_dc_2").unwrap_or(0.5);

        // Apply activations: exp for scale, sigmoid for opacity, SH DC → color
        let scale = [scale_0.exp(), scale_1.exp(), scale_2.exp()];
        let opacity = sigmoid(opacity_raw);
        let sh_dc = [
            sh_dc_to_color(f_dc_0),
            sh_dc_to_color(f_dc_1),
            sh_dc_to_color(f_dc_2),
        ];

        // Normalize quaternion
        let q_len = (rot_0 * rot_0 + rot_1 * rot_1 + rot_2 * rot_2 + rot_3 * rot_3).sqrt();
        let rotation = if q_len > 0.0001 {
            [rot_0 / q_len, rot_1 / q_len, rot_2 / q_len, rot_3 / q_len]
        } else {
            [1.0, 0.0, 0.0, 0.0]
        };

        cpu_positions.push([x, y, z]);

        gpu_data.push(GaussianSplatGpu {
            position: [x, y, z],
            opacity,
            scale,
            _pad0: 0.0,
            rotation,
            sh_dc,
            _pad1: 0.0,
        });
    }

    let splat_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat Data: {}", splat_path)),
        contents: bytemuck::cast_slice(&gpu_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });

    // Create sorted index buffer (initially sequential)
    let initial_indices: Vec<u32> = (0..count as u32).collect();
    let sorted_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat Sorted Indices: {}", splat_path)),
        contents: bytemuck::cast_slice(&initial_indices),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });

    tracing::info!(
        "Parsed PLY: {} gaussians from {:?}",
        count,
        full_path.file_name().unwrap_or_default()
    );

    Ok(GpuSplat {
        splat_buffer,
        sorted_index_buffer,
        splat_count: count as u32,
        cpu_positions,
    })
}

/// Extract a float property from a PLY element, handling both Float and Double types.
fn get_float_property(
    element: &LinkedHashMap<String, ply_rs::ply::Property>,
    name: &str,
) -> Result<f32, SplatError> {
    match element.get(name) {
        Some(ply_rs::ply::Property::Float(v)) => Ok(*v),
        Some(ply_rs::ply::Property::Double(v)) => Ok(*v as f32),
        Some(_) => Err(SplatError::MissingProperty(format!(
            "{} has wrong type",
            name
        ))),
        None => Err(SplatError::MissingProperty(name.to_string())),
    }
}

/// Create a galaxy/nebula spiral procedural splat cloud.
fn create_procedural_splats(device: &wgpu::Device) -> GpuSplat {
    use std::f32::consts::PI;

    let mut gpu_data = Vec::new();
    let mut cpu_positions = Vec::new();

    // Simple LCG pseudo-random for deterministic results without rand crate
    let mut seed: u32 = 42;
    let mut next_rand = || -> f32 {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        ((seed >> 16) & 0x7FFF) as f32 / 32767.0
    };

    let num_arms = 3;
    let splats_per_arm = 400;
    let core_splats = 200;
    let total = num_arms * splats_per_arm + core_splats;

    // Galaxy spiral arms
    for arm in 0..num_arms {
        let arm_offset = (arm as f32 / num_arms as f32) * 2.0 * PI;
        for i in 0..splats_per_arm {
            let t = i as f32 / splats_per_arm as f32;
            let radius = t * 3.0 + 0.3;
            let angle = arm_offset + t * 4.0 * PI;

            // Add some spread perpendicular to the arm
            let spread = 0.15 + t * 0.3;
            let dx = (next_rand() - 0.5) * spread;
            let dy = (next_rand() - 0.5) * 0.15; // Thin disk
            let dz = (next_rand() - 0.5) * spread;

            let x = radius * angle.cos() + dx;
            let y = dy;
            let z = radius * angle.sin() + dz;

            let pos = [x, y, z];
            cpu_positions.push(pos);

            // Color: warm core fading to cool blue/purple at edges
            let core_mix = (1.0 - t).powf(1.5);
            let r = 0.9 * core_mix + 0.15 * (1.0 - core_mix) + next_rand() * 0.1;
            let g = 0.6 * core_mix + 0.1 * (1.0 - core_mix) + next_rand() * 0.05;
            let b = 0.3 * core_mix + 0.7 * (1.0 - core_mix) + next_rand() * 0.15;

            let opacity = (0.6 - t * 0.3).max(0.15) + next_rand() * 0.1;
            let scale_val = 0.04 + t * 0.06 + next_rand() * 0.02;

            gpu_data.push(GaussianSplatGpu {
                position: pos,
                opacity,
                scale: [scale_val, scale_val * 0.5, scale_val],
                _pad0: 0.0,
                rotation: [1.0, 0.0, 0.0, 0.0],
                sh_dc: [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)],
                _pad1: 0.0,
            });
        }
    }

    // Dense bright core
    for _ in 0..core_splats {
        let r_dist = next_rand().powf(2.0) * 0.5;
        let theta = next_rand() * 2.0 * PI;
        let phi = (next_rand() - 0.5) * PI * 0.3;

        let x = r_dist * theta.cos() * phi.cos();
        let y = r_dist * phi.sin() * 0.4;
        let z = r_dist * theta.sin() * phi.cos();

        let pos = [x, y, z];
        cpu_positions.push(pos);

        // Hot white/yellow core
        let r = 1.0;
        let g = 0.85 + next_rand() * 0.15;
        let b = 0.5 + next_rand() * 0.3;

        gpu_data.push(GaussianSplatGpu {
            position: pos,
            opacity: 0.5 + next_rand() * 0.3,
            scale: [0.06, 0.03, 0.06],
            _pad0: 0.0,
            rotation: [1.0, 0.0, 0.0, 0.0],
            sh_dc: [r, g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)],
            _pad1: 0.0,
        });
    }

    let count = gpu_data.len();
    assert_eq!(count, total);

    let splat_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Procedural Galaxy Splat Data"),
        contents: bytemuck::cast_slice(&gpu_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });

    let initial_indices: Vec<u32> = (0..count as u32).collect();
    let sorted_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Procedural Galaxy Splat Sorted Indices"),
        contents: bytemuck::cast_slice(&initial_indices),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });

    GpuSplat {
        splat_buffer,
        sorted_index_buffer,
        splat_count: count as u32,
        cpu_positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid() {
        assert!((sigmoid(0.0) - 0.5).abs() < 0.001);
        assert!(sigmoid(10.0) > 0.999);
        assert!(sigmoid(-10.0) < 0.001);
    }

    #[test]
    fn test_sh_dc_to_color() {
        // DC = 0 → 0.5 (grey)
        let c = sh_dc_to_color(0.0);
        assert!((c - 0.5).abs() < 0.001);
        // DC clamped to [0, 1]
        assert!(sh_dc_to_color(-100.0) >= 0.0);
        assert!(sh_dc_to_color(100.0) <= 1.0);
    }

    #[test]
    fn test_splat_cache_new() {
        let cache = SplatCache::new();
        assert!(!cache.has_splats());
    }

    #[test]
    fn test_gpu_splat_size() {
        // Verify the struct is 64 bytes as expected
        assert_eq!(std::mem::size_of::<GaussianSplatGpu>(), 64);
    }
}
