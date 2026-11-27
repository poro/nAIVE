//! Gaussian splat loading, caching, and GPU sorting.
//!
//! Loads .ply files in standard 3DGS format (position, scale, rotation,
//! opacity, spherical harmonics) and uploads them to GPU storage buffers.
//! Every frame each splat entity's splats are sorted back to front on the GPU
//! (`sort`) for correct alpha blending.

pub mod sort;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::components::SplatHandle;
use crate::gpu_timer::GpuTimer;

pub use naive_sim::assets::AssetError as SplatError;
pub use sort::SplatInstance;

/// GPU-side splat data. Packed to 64 bytes for efficient storage buffer access.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GaussianSplatGpu {
    pub position: [f32; 3],
    pub opacity: f32,
    pub scale: [f32; 3],
    pub _pad0: f32,
    /// Quaternion in 3DGS order: w, x, y, z.
    pub rotation: [f32; 4],
    pub sh_dc: [f32; 3],
    pub _pad1: f32,
}

/// A loaded GPU splat cloud.
pub struct GpuSplat {
    /// Storage buffer containing all splat data.
    pub splat_buffer: wgpu::Buffer,
    /// Number of splats in this cloud.
    pub splat_count: u32,
}

/// Cache of loaded splat clouds, keyed by file path, and each splat entity's
/// sort state.
pub struct SplatCache {
    splats: Vec<GpuSplat>,
    path_to_handle: HashMap<PathBuf, SplatHandle>,
    /// The sort pipelines, made with the first sort.
    sorter: Option<sort::SplatSorter>,
    /// Sort state per entity: entities sharing a splat asset can stand in
    /// different places, so each draws in its own order.
    instances: HashMap<hecs::Entity, SplatInstance>,
}

impl SplatCache {
    pub fn new() -> Self {
        Self {
            splats: Vec::new(),
            path_to_handle: HashMap::new(),
            sorter: None,
            instances: HashMap::new(),
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

    /// The sort state `entity` draws with, once a sort has made it.
    pub fn instance(&self, entity: hecs::Entity) -> Option<&SplatInstance> {
        self.instances.get(&entity)
    }

    /// Sort each splat entity's splats back to front, as `view` sees them,
    /// on the GPU. `entities` are this frame's splat entities with their
    /// splat and world matrix; sort state for any other entity is dropped.
    /// An entity whose matrix and view have not changed keeps its order.
    pub fn sort_splats(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: Mat4,
        entities: &[(hecs::Entity, SplatHandle, Mat4)],
        timer: Option<&GpuTimer>,
    ) {
        let Self {
            splats,
            sorter,
            instances,
            ..
        } = self;
        let live: HashSet<hecs::Entity> = entities.iter().map(|(entity, ..)| *entity).collect();
        instances.retain(|entity, _| live.contains(entity));
        if entities.is_empty() {
            return;
        }
        let sorter = sorter.get_or_insert_with(|| sort::SplatSorter::new(device));

        let mut stale = Vec::new();
        for &(entity, handle, model) in entities {
            let splat = &splats[handle.0];
            if splat.splat_count == 0 {
                instances.remove(&entity);
                continue;
            }
            let instance = match instances.get_mut(&entity) {
                // Hot reload gives the entity a new splat.
                Some(instance) if instance.handle == handle => instance,
                _ => {
                    instances.insert(entity, sorter.instance(device, splat, handle));
                    instances.get_mut(&entity).expect("just inserted")
                }
            };
            if instance.update(queue, model, view) {
                stale.push(entity);
            }
        }
        if stale.is_empty() {
            return;
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Splat sort"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Splat sort"),
                timestamp_writes: timer.and_then(|t| t.compute_pass(crate::gpu_timer::SPLAT_SORT)),
            });
            for entity in &stale {
                sorter.encode(&mut pass, &instances[entity]);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
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

/// Upload shared, already validated splat data to a GPU storage buffer.
fn upload_splat(
    device: &wgpu::Device,
    splat_path: &str,
    data: naive_sim::assets::SplatData,
) -> GpuSplat {
    let count = data.splats.len() as u32;
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
    GpuSplat {
        splat_buffer,
        splat_count: count,
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
