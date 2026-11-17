//! Gaussian splat loading, caching, and CPU sorting.
//!
//! Loads .ply files in standard 3DGS format (position, scale, rotation,
//! opacity, spherical harmonics) and uploads to GPU storage buffers.
//! Provides per-frame CPU depth sorting for correct alpha blending.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::components::SplatHandle;

pub use naive_sim::assets::AssetError as SplatError;

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

        let data = naive_sim::assets::load_splat(project_root, splat_path)?;
        let gpu_splat = upload_splat(device, splat_path, data);
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
    pub fn sort_splats(&self, handle: SplatHandle, view_matrix: &Mat4, queue: &wgpu::Queue) {
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
            tracing::info!(
                "Invalidated splat cache: {} (handle {})",
                splat_path,
                handle.0
            );
            // The GpuSplat remains in the vec but the path mapping is removed,
            // so next get_or_load will reload it (with a new handle).
        }
    }

    /// Check if any splats are loaded.
    pub fn has_splats(&self) -> bool {
        !self.splats.is_empty()
    }
}

impl Default for SplatCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Upload shared, already validated splat data to GPU storage buffers.
fn upload_splat(
    device: &wgpu::Device,
    splat_path: &str,
    data: naive_sim::assets::SplatData,
) -> GpuSplat {
    let count = data.splats.len() as u32;
    let cpu_positions = data.splats.iter().map(|point| point.position).collect();
    let gpu_data: Vec<_> = data
        .splats
        .into_iter()
        .map(|point| GaussianSplatGpu {
            position: point.position,
            opacity: point.opacity,
            scale: point.scale,
            _pad0: 0.0,
            rotation: point.rotation,
            sh_dc: point.sh_dc,
            _pad1: 0.0,
        })
        .collect();
    let splat_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat Data: {splat_path}")),
        contents: bytemuck::cast_slice(&gpu_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let initial_indices: Vec<u32> = (0..count).collect();
    let sorted_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat Sorted Indices: {splat_path}")),
        contents: bytemuck::cast_slice(&initial_indices),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    GpuSplat {
        splat_buffer,
        sorted_index_buffer,
        splat_count: count,
        cpu_positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_splat_cache_new() {
        assert!(!SplatCache::new().has_splats());
    }

    #[test]
    fn test_gpu_splat_size() {
        assert_eq!(std::mem::size_of::<GaussianSplatGpu>(), 64);
    }
}
