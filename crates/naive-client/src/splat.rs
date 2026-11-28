//! Gaussian splat loading, caching, and GPU sorting.
//!
//! Loads .ply files in standard 3DGS format (position, scale, rotation,
//! opacity, spherical harmonics) and uploads them to GPU storage buffers.
//! The upload orders each cloud's splats along a Morton curve and bounds
//! every run of [`cluster::CLUSTER`] of them with a sphere (`cluster`). Every
//! frame each splat entity's splats in view are found, culling whole clusters
//! where it can, and sorted back to front on the GPU (`sort`) for correct
//! alpha blending; the same pass works out each splat's view-dependent colour
//! from its spherical harmonics.

pub mod cluster;
#[cfg(test)]
mod projection_tests;
pub mod sort;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::components::SplatHandle;
use crate::gpu_timer::GpuTimer;

pub use naive_sim::assets::AssetError as SplatError;
pub use sort::{SplatCamera, SplatInstance};

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
    /// Base colour, `0.5 + C0 * f_dc`, unclamped.
    pub sh_dc: [f32; 3],
    pub _pad1: f32,
}

/// A loaded GPU splat cloud.
pub struct GpuSplat {
    /// Storage buffer containing all splat data.
    pub splat_buffer: wgpu::Buffer,
    /// Higher-band spherical harmonics as pairs of half floats
    /// ([`sh_words_per_splat`] words per splat); a placeholder at degree 0.
    pub sh_buffer: wgpu::Buffer,
    /// Spherical-harmonics degree, 0 to 3.
    pub sh_degree: u32,
    /// Number of splats in this cloud.
    pub splat_count: u32,
    /// Each cluster's bounding sphere, as `vec4<f32>` (centre, radius).
    pub cluster_buffer: wgpu::Buffer,
    /// Clusters of [`cluster::CLUSTER`] splats, the last one maybe partial.
    pub cluster_count: u32,
}

/// The most splats one cloud may have: the keys pass takes one workgroup
/// per cluster, and a dispatch has at most 65,535.
pub const MAX_SPLATS: usize = 65_535 * cluster::CLUSTER;

/// `u32` words of packed half-float SH coefficients per splat at `degree`.
pub fn sh_words_per_splat(degree: u32) -> usize {
    (3 * naive_sim::assets::sh_rest_coefficients(degree)).div_ceil(2)
}

/// Pack each splat's higher-band coefficients as half floats, two to a word,
/// the first in the low 16 bits, taking the splats in `order`. Values beyond
/// the half-float range are clamped to it.
pub fn pack_sh(sh_rest: &[f32], degree: u32, order: &[u32]) -> Vec<u32> {
    let per_splat = 3 * naive_sim::assets::sh_rest_coefficients(degree);
    let words = sh_words_per_splat(degree);
    let mut packed = vec![0u32; order.len() * words];
    for (splat, &source) in order.iter().enumerate() {
        let start = source as usize * per_splat;
        let Some(coefficients) = sh_rest.get(start..start + per_splat) else {
            continue;
        };
        for (j, value) in coefficients.iter().enumerate() {
            let value = value.clamp(-65504.0, 65504.0);
            let bits = u32::from(half::f16::from_f32(value).to_bits());
            packed[splat * words + j / 2] |= bits << (16 * (j % 2));
        }
    }
    packed
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
        let gpu_splat = upload_splat(device, splat_path, data)?;
        let handle = SplatHandle(self.splats.len());
        tracing::info!(
            "Loaded splat: {} ({} gaussians, spherical harmonics degree {})",
            splat_path,
            gpu_splat.splat_count,
            gpu_splat.sh_degree
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

    /// Find each splat entity's splats in view of `camera` and sort them
    /// back to front, on the GPU. `entities` are this frame's splat entities
    /// with their splat and world matrix; sort state for any other entity is
    /// dropped. An entity whose matrix and camera have not changed keeps its
    /// order.
    pub fn sort_splats(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: SplatCamera,
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
            if instance.update(queue, model, camera) {
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

/// Upload shared, already validated splat data to GPU storage buffers, in
/// Morton order with a bounding sphere per cluster. A cloud too large for
/// this GPU's buffers is an error, not a crash.
fn upload_splat(
    device: &wgpu::Device,
    splat_path: &str,
    data: naive_sim::assets::SplatData,
) -> Result<GpuSplat, SplatError> {
    let count = data.splats.len() as u32;
    let sh_degree = data.sh_degree;
    let limits = device.limits();
    let largest = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size);
    let splat_bytes = data.splats.len() as u64 * std::mem::size_of::<GaussianSplatGpu>() as u64;
    let sh_bytes = data.splats.len() as u64 * sh_words_per_splat(sh_degree) as u64 * 4;
    // The sort's colour buffer: one vec4<f32> per splat.
    let colour_bytes = data.splats.len() as u64 * 16;
    if splat_bytes.max(sh_bytes).max(colour_bytes) > largest {
        return Err(SplatError(format!(
            "Splat '{splat_path}' has {count} splats, more than this GPU's {} MiB buffers hold",
            largest >> 20
        )));
    }
    if data.splats.len() > MAX_SPLATS {
        return Err(SplatError(format!(
            "Splat '{splat_path}' has {count} splats, more than the {MAX_SPLATS} one cloud may have"
        )));
    }
    let positions: Vec<[f32; 3]> = data.splats.iter().map(|point| point.position).collect();
    let order = cluster::morton_order(&positions);
    let mut clusters = cluster::cluster_bounds(&positions, &order);
    drop(positions);
    let cluster_count = clusters.len() as u32;
    if clusters.is_empty() {
        // A binding cannot be empty.
        clusters.push([0.0; 4]);
    }
    let cluster_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat clusters: {splat_path}")),
        contents: bytemuck::cast_slice(&clusters),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let sh_words = if sh_degree == 0 {
        vec![0u32; 4]
    } else {
        pack_sh(&data.sh_rest, sh_degree, &order)
    };
    let sh_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat SH: {splat_path}")),
        contents: bytemuck::cast_slice(&sh_words),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let gpu_data: Vec<_> = order
        .iter()
        .map(|&i| {
            let point = &data.splats[i as usize];
            GaussianSplatGpu {
                position: point.position,
                opacity: point.opacity,
                scale: point.scale,
                _pad0: 0.0,
                rotation: point.rotation,
                sh_dc: point.sh_dc,
                _pad1: 0.0,
            }
        })
        .collect();
    let splat_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Splat Data: {splat_path}")),
        contents: bytemuck::cast_slice(&gpu_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    Ok(GpuSplat {
        splat_buffer,
        sh_buffer,
        sh_degree,
        splat_count: count,
        cluster_buffer,
        cluster_count,
    })
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

    #[test]
    fn sh_coefficients_pack_two_half_floats_to_a_word() {
        assert_eq!(
            [1, 2, 3].map(sh_words_per_splat),
            [5, 12, 23],
            "9, 24 and 45 halves"
        );
        // Two splats at degree 1, swapped: nine coefficients each, the
        // last word half empty.
        let rest: Vec<f32> = (0..18).map(|i| i as f32 * 0.5).collect();
        let packed = pack_sh(&rest, 1, &[1, 0]);
        assert_eq!(packed.len(), 10);
        let half = |bits: u32| half::f16::from_bits(bits as u16).to_f32();
        for (slot, source) in [(0, 1), (1, 0)] {
            for j in 0..9 {
                let word = packed[slot * 5 + j / 2];
                let value = half(word >> (16 * (j % 2)) & 0xFFFF);
                assert_eq!(value, rest[source * 9 + j], "slot {slot} value {j}");
            }
            assert_eq!(packed[slot * 5 + 4] >> 16, 0);
        }
    }
}
