//! Back-to-front splat sorting on the GPU, of only the splats in view.
//!
//! Each splat entity gets its own sort state ([`SplatInstance`]): splats are
//! shared by asset path, but which are in view and the order they draw in
//! depend on the entity's transform as well as the camera.
//!
//! The keys pass culls first. One workgroup takes each cluster of
//! [`CLUSTER`] splats (`cluster`) and stops at once when the cluster's
//! bounding sphere is wholly outside the view volume the draw keeps splat
//! centres in. Otherwise it tests each of its splats the same way, drops the
//! ones too faint to draw, and packs the rest at the front of the sort: each
//! one's view-space depth, as a `u32` that sorts like the float, and its
//! index. It also records where each cluster's kept splats went and how many
//! there are. The size pass reads how many were kept: the length they sort
//! at and the draw's instance count.
//!
//! A least-significant-digit radix sort then orders the (key, index) pairs,
//! [`DIGIT_BITS`] bits of the key at a time, between two pairs of buffers.
//! Each digit takes three passes over tiles of [`TILE`] pairs: count each
//! tile's digits, prefix-sum each digit's counts over the tiles, and move
//! every pair to its place, the tile sorted by digit in workgroup memory
//! first so that the pass is stable and its writes run together. Every pass
//! is dispatched for the whole cloud, and its workgroups past the kept
//! splats stop at once, so the sort's work and the draw scale with the
//! splats in view. (Dispatching indirectly, sized on the GPU, costs about
//! 20 µs more per dispatch on Metal: more than it saves for a cloud mostly
//! in view.)
//!
//! Ties in depth are broken by splat index, so the order is deterministic
//! and the same as a stable sort by view-space depth. Workgroups pack
//! clusters in whatever order they finish, so the first pass does not read
//! the packed pairs in packed order: each of its tiles takes a run of
//! clusters and reads each cluster's kept splats where the keys pass put
//! them, in splat order. Every pass is stable, so splats with equal depths
//! stay in splat order to the end. (Sorting the index as a further 16 or 24
//! bits of key would cost two or three more digits.)
//!
//! The keys pass also writes each kept splat's colour for this view: its
//! base colour plus its higher spherical-harmonics bands, evaluated as in 3D
//! Gaussian Splatting for the direction from the camera to the splat in the
//! splat asset's own space, then clamped to [0, 1]. The colour only changes
//! when the camera or entity moves, which is when the sort runs.

use glam::Mat4;
use wgpu::util::DeviceExt;

use super::cluster::{self, CLUSTER};
use super::GpuSplat;

/// Bits of the key each radix pass sorts by.
pub const DIGIT_BITS: u32 = 8;
const RADIX: u32 = 1 << DIGIT_BITS;
/// Radix passes: the whole 32-bit key.
const DIGITS: u32 = 32 / DIGIT_BITS;
const WORKGROUP: u32 = 256;
/// Pairs one workgroup counts and moves (eight per invocation).
pub const TILE: u32 = 2048;
/// Clusters one tile of the first pass reads (the shader's constant).
#[cfg(test)]
const TILE_CLUSTERS: u32 = TILE / WORKGROUP;
const _: () = assert!(CLUSTER as u32 == WORKGROUP, "one workgroup per cluster");
const _: () = assert!(RADIX == WORKGROUP, "one invocation per digit");
const _: () = assert!(
    super::MAX_SPLATS.div_ceil(TILE as usize) <= 65_535,
    "a tile pass fits in a dispatch"
);

/// What the sort needs of the camera the draw uses.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SplatCamera {
    pub view: Mat4,
    pub projection: Mat4,
    /// The draw drops splats whose depth is not beyond this.
    pub near: f32,
}

impl From<&crate::camera::CameraUniform> for SplatCamera {
    fn from(camera: &crate::camera::CameraUniform) -> Self {
        Self {
            view: Mat4::from_cols_array_2d(&camera.view),
            projection: Mat4::from_cols_array_2d(&camera.projection),
            near: camera.near_plane,
        }
    }
}

/// Per-entity values both the sort and the draw read.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceUniforms {
    pub model: [[f32; 4]; 4],
    pub view_model: [[f32; 4]; 4],
    /// The view volume the draw keeps splat centres in, in the splat
    /// asset's space ([`cluster::view_planes`]).
    pub planes: [[f32; 4]; 5],
    /// The camera's position in the splat asset's space.
    pub camera_local: [f32; 3],
    pub sh_degree: u32,
    pub count: u32,
    /// Tiles the whole cloud makes: the radix passes' dispatch size.
    pub tiles: u32,
    pub _pad: [u32; 2],
}

/// One dispatch of the sort.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SortPass {
    /// Cull clusters and splats; pack each kept splat's depth key and index
    /// at the front, record each cluster's kept splats, and write their
    /// colours.
    Keys,
    /// Size the sort and the draw to the kept splats.
    Size,
    /// Count each tile's keys by their digit at `shift`.
    Count { shift: u32 },
    /// Turn each digit's counts into where each tile's pairs with that
    /// digit start, among that digit's pairs; and total each digit.
    Scan { shift: u32 },
    /// Move every pair to its place by its digit at `shift`.
    Scatter { shift: u32 },
}

/// The dispatches that cull and sort, in order.
pub fn sort_passes() -> Vec<SortPass> {
    let mut passes = vec![SortPass::Keys, SortPass::Size];
    for digit in 0..DIGITS {
        let shift = digit * DIGIT_BITS;
        passes.extend([
            SortPass::Count { shift },
            SortPass::Scan { shift },
            SortPass::Scatter { shift },
        ]);
    }
    passes
}

/// The tiles `count` pairs make.
pub fn tiles(count: u32) -> u32 {
    count.div_ceil(TILE)
}

/// A float's bits as a `u32` that orders the same way.
pub fn sortable_depth(z: f32) -> u32 {
    let bits = z.to_bits();
    if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits | 0x8000_0000
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, has_dynamic_offset: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset,
            min_binding_size: None,
        },
        count: None,
    }
}

fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

/// The shared compute pipelines, made with the first splat.
pub struct SplatSorter {
    /// The keys and size passes' bindings.
    cull_layout: wgpu::BindGroupLayout,
    /// The radix passes' bindings: they read one pair of key and index
    /// buffers and write the other.
    radix_layout: wgpu::BindGroupLayout,
    keys: wgpu::ComputePipeline,
    size: wgpu::ComputePipeline,
    count: wgpu::ComputePipeline,
    scan: wgpu::ComputePipeline,
    scatter: wgpu::ComputePipeline,
    passes: Vec<SortPass>,
    /// Each pass's digit shift, `step_stride` bytes apart.
    steps: wgpu::Buffer,
    step_stride: u32,
}

impl SplatSorter {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Splat sort"),
            source: wgpu::ShaderSource::Wgsl(SORT_WGSL.into()),
        });
        // At most eight storage buffers each, the default limit.
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splat cull"),
            entries: &[
                storage_entry(0, true),
                storage_entry(1, false),
                storage_entry(2, false),
                uniform_entry(3, false),
                storage_entry(5, true),
                storage_entry(6, false),
                storage_entry(7, false),
                storage_entry(8, true),
                storage_entry(13, false),
            ],
        });
        let radix_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splat radix sort"),
            entries: &[
                storage_entry(1, false),
                storage_entry(2, false),
                uniform_entry(3, false),
                uniform_entry(4, true),
                storage_entry(7, false),
                storage_entry(9, false),
                storage_entry(10, false),
                storage_entry(11, false),
                storage_entry(13, false),
            ],
        });
        let pipeline = |layout: &wgpu::BindGroupLayout, entry_point: &str| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry_point),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let passes = sort_passes();
        let step_stride = device.limits().min_uniform_buffer_offset_alignment.max(16);
        let stride = step_stride as usize;
        let mut steps = vec![0u8; passes.len() * stride];
        for (n, pass) in passes.iter().enumerate() {
            let shift = match *pass {
                SortPass::Count { shift }
                | SortPass::Scan { shift }
                | SortPass::Scatter { shift } => shift,
                SortPass::Keys | SortPass::Size => 0,
            };
            steps[n * stride..n * stride + 4].copy_from_slice(&shift.to_le_bytes());
        }
        let steps = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Splat sort steps"),
            contents: &steps,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        Self {
            keys: pipeline(&cull_layout, "compute_keys"),
            size: pipeline(&cull_layout, "size_sort"),
            count: pipeline(&radix_layout, "count_digits"),
            scan: pipeline(&radix_layout, "scan_counts"),
            scatter: pipeline(&radix_layout, "scatter"),
            cull_layout,
            radix_layout,
            passes,
            steps,
            step_stride,
        }
    }

    /// Sort state for one entity drawing `splat`.
    pub fn instance(
        &self,
        device: &wgpu::Device,
        splat: &GpuSplat,
        handle: crate::components::SplatHandle,
    ) -> SplatInstance {
        let count = splat.splat_count.max(1);
        let tiles = tiles(count);
        let storage = |label, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let pairs = u64::from(count) * 4;
        let keys = storage("Splat sort keys", pairs);
        let sorted_indices = storage("Splat sorted indices", pairs);
        let other_keys = storage("Splat sort keys (other)", pairs);
        let other_indices = storage("Splat sort indices (other)", pairs);
        // Each digit's total, then each digit's count per tile.
        let counts = storage(
            "Splat sort digit counts",
            u64::from(RADIX + RADIX * tiles) * 4,
        );
        // Each cluster's kept splats: where they start, and how many.
        let runs = storage(
            "Splat cluster runs",
            u64::from(splat.cluster_count.max(1)) * 8,
        );
        let colors = storage("Splat colours", u64::from(count) * 16);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splat instance"),
            size: std::mem::size_of::<InstanceUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // The shader's CullState, ending with the draw's indirect arguments.
        // Buffers start zeroed: no splats kept, nothing to draw.
        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splat cull state"),
            size: DRAW_ARGS_OFFSET + 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let cull = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Splat cull"),
            layout: &self.cull_layout,
            entries: &[
                entry(0, &splat.splat_buffer),
                entry(1, &keys),
                entry(2, &sorted_indices),
                entry(3, &uniforms),
                entry(5, &splat.sh_buffer),
                entry(6, &colors),
                entry(7, &state),
                entry(8, &splat.cluster_buffer),
                entry(13, &runs),
            ],
        });
        let radix = |from: (&wgpu::Buffer, &wgpu::Buffer), to: (&wgpu::Buffer, &wgpu::Buffer)| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Splat radix sort"),
                layout: &self.radix_layout,
                entries: &[
                    entry(1, from.0),
                    entry(2, from.1),
                    entry(3, &uniforms),
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.steps,
                            offset: 0,
                            size: wgpu::BufferSize::new(16),
                        }),
                    },
                    entry(7, &state),
                    entry(9, to.0),
                    entry(10, to.1),
                    entry(11, &counts),
                    entry(13, &runs),
                ],
            })
        };
        // An even number of digits ends where the keys pass started: in the
        // buffers the draw reads.
        const _: () = assert!(DIGITS.is_multiple_of(2));
        let radix = [
            radix((&keys, &sorted_indices), (&other_keys, &other_indices)),
            radix((&other_keys, &other_indices), (&keys, &sorted_indices)),
        ];
        SplatInstance {
            handle,
            count: splat.splat_count,
            sh_degree: splat.sh_degree,
            tiles,
            clusters: splat.cluster_count,
            _buffers: [keys, other_keys, other_indices, counts, runs],
            sorted_indices,
            colors,
            uniforms,
            state,
            cull,
            radix,
            written: None,
        }
    }

    /// Record the dispatches that cull and sort `instance`.
    pub fn encode(&self, pass: &mut wgpu::ComputePass<'_>, instance: &SplatInstance) {
        for (n, step) in self.passes.iter().enumerate() {
            let (pipeline, groups, shift) = match *step {
                SortPass::Keys => (&self.keys, instance.clusters, None),
                SortPass::Size => (&self.size, 1, None),
                SortPass::Count { shift } => (&self.count, instance.tiles, Some(shift)),
                SortPass::Scan { shift } => (&self.scan, RADIX, Some(shift)),
                SortPass::Scatter { shift } => (&self.scatter, instance.tiles, Some(shift)),
            };
            pass.set_pipeline(pipeline);
            match shift {
                None => pass.set_bind_group(0, &instance.cull, &[]),
                // Digits alternate between the two pairs of buffers.
                Some(shift) => pass.set_bind_group(
                    0,
                    &instance.radix[(shift / DIGIT_BITS % 2) as usize],
                    &[n as u32 * self.step_stride],
                ),
            }
            pass.dispatch_workgroups(groups, 1, 1);
        }
    }
}

/// Where the draw's indirect arguments start in an instance's cull state.
const DRAW_ARGS_OFFSET: u64 = 16;

/// One entity's sort state and draw uniforms.
pub struct SplatInstance {
    /// The splat asset this state was made for.
    pub handle: crate::components::SplatHandle,
    pub count: u32,
    sh_degree: u32,
    tiles: u32,
    clusters: u32,
    /// The keys beside `sorted_indices`, the other pair of key and index
    /// buffers, the digit counts and the cluster runs.
    _buffers: [wgpu::Buffer; 5],
    /// Indices of the splats in view, farthest first; the draw's arguments
    /// say how many.
    pub sorted_indices: wgpu::Buffer,
    /// Each splat's colour for the current view, as `vec4<f32>`, in splat
    /// order. Only splats in view are written.
    pub colors: wgpu::Buffer,
    /// [`InstanceUniforms`]: the draw reads the model matrix from here.
    pub uniforms: wgpu::Buffer,
    /// The cull's count of splats kept, and the draw's indirect arguments:
    /// 6 vertices, one instance per splat in view.
    state: wgpu::Buffer,
    /// The keys and size passes' bind group.
    cull: wgpu::BindGroup,
    /// The radix passes' bind groups: from the draw's buffers to the others,
    /// and back.
    radix: [wgpu::BindGroup; 2],
    /// What the uniforms hold, and so what the indices are sorted for.
    written: Option<InstanceUniforms>,
}

impl SplatInstance {
    /// The splat pass's per-entity bind group (`layout` is the splat
    /// pipeline's group 1): `splat`'s data, the sorted indices, the instance
    /// uniforms and this view's colours.
    pub fn draw_bind_group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        splat: &GpuSplat,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Splat Data Bind Group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: splat.splat_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.sorted_indices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.colors.as_entire_binding(),
                },
            ],
        })
    }

    /// Draw the splats in view, as many as the last sort kept.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.draw_indirect(&self.state, DRAW_ARGS_OFFSET);
    }

    /// Point the instance at `model` seen by `camera`. Returns whether it
    /// must be culled and sorted again: not when neither has changed.
    pub fn update(&mut self, queue: &wgpu::Queue, model: Mat4, camera: SplatCamera) -> bool {
        let position = camera.view.inverse().w_axis.truncate();
        let camera_local = model.inverse().transform_point3(position);
        let planes = cluster::view_planes(model, camera.view, camera.projection, camera.near);
        let uniforms = InstanceUniforms {
            model: model.to_cols_array_2d(),
            view_model: (camera.view * model).to_cols_array_2d(),
            planes: planes.map(|plane| plane.to_array()),
            // A degenerate world matrix has no inverse; any direction will do.
            camera_local: if camera_local.is_finite() {
                camera_local
            } else {
                position
            }
            .to_array(),
            sh_degree: self.sh_degree,
            count: self.count,
            tiles: self.tiles,
            _pad: [0; 2],
        };
        if self.written == Some(uniforms) {
            return false;
        }
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
        self.written = Some(uniforms);
        true
    }
}

/// Culling and keys, sizing, and the radix sort's count, scan and scatter
/// passes over (key, index) pairs.
pub const SORT_WGSL: &str = r#"
struct InstanceUniforms {
    model: mat4x4<f32>,
    view_model: mat4x4<f32>,
    planes: array<vec4<f32>, 5>,
    camera_local: vec3<f32>,
    sh_degree: u32,
    count: u32,
    tiles: u32,
    _pad: vec2<u32>,
};

struct SortStep {
    shift: u32,
    _pad0: u32,
    _pad1: vec2<u32>,
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

struct CullState {
    // Splats the keys pass has packed at the front so far.
    kept: atomic<u32>,
    // What the size pass read: the splats kept and the tiles they make.
    sorted_count: u32,
    sorted_tiles: u32,
    _pad: u32,
    // The draw's indirect arguments: vertex count, instance count, first
    // vertex, first instance.
    draw: array<u32, 4>,
};

@group(0) @binding(0) var<storage, read> splats: array<GaussianSplat>;
// The pairs: where the keys pass packs them and the draw reads the indices,
// or where a radix pass reads them.
@group(0) @binding(1) var<storage, read_write> keys: array<u32>;
@group(0) @binding(2) var<storage, read_write> values: array<u32>;
@group(0) @binding(3) var<uniform> instance: InstanceUniforms;
@group(0) @binding(4) var<uniform> sort_step: SortStep;
// Higher-band SH coefficients, two half floats to a word.
@group(0) @binding(5) var<storage, read> sh: array<u32>;
@group(0) @binding(6) var<storage, read_write> colors: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> state: CullState;
// Each cluster's bounding sphere: centre and radius.
@group(0) @binding(8) var<storage, read> clusters: array<vec4<f32>>;
// Where a radix pass writes the pairs.
@group(0) @binding(9) var<storage, read_write> keys_out: array<u32>;
@group(0) @binding(10) var<storage, read_write> values_out: array<u32>;
// Each digit's total (RADIX words), then its count in each tile (a row of
// instance.tiles words per digit), which the scan turns into where each
// tile's pairs with that digit start among them.
@group(0) @binding(11) var<storage, read_write> counts: array<u32>;
// Each cluster's kept splats: where the keys pass packed them, and how many.
@group(0) @binding(13) var<storage, read_write> runs: array<vec2<u32>>;

const HALF: u32 = 256u;
const DIGIT_BITS: u32 = 8u;
const RADIX: u32 = 256u;
const TILE: u32 = 2048u;
const TILE_CLUSTERS: u32 = 8u;
// Bits of a local sort entry that hold the pair's place in its tile.
const PLACE_BITS: u32 = 11u;
// The draw's limits (the splat render shader's).
const MIN_ALPHA: f32 = 0.0039215686; // 1 / 255
const MAX_ALPHA: f32 = 0.99;
// How generous the cull is (cluster::TOLERANCE).
const TOLERANCE: f32 = 1e-4;

var<workgroup> scan: array<u32, 256>;
var<workgroup> cluster_in_view: u32;
var<workgroup> packed_base: u32;
// A tile's size, and for the first pass where each of its clusters' kept
// splats were packed and where they start in the tile.
var<workgroup> tile_size: u32;
// The tiles a scan covers.
var<workgroup> scan_rows: u32;
var<workgroup> tile_runs: array<vec2<u32>, 8>;
var<workgroup> digit_counts: array<atomic<u32>, 256>;
// Each entry a pair's digit and its place in the tile.
var<workgroup> local_sort: array<u32, 2048>;
// Where each digit's pairs from this tile go in the output, less where they
// start in the sorted tile.
var<workgroup> digit_place: array<u32, 256>;
var<workgroup> run_start: array<u32, 256>;

// A float's bits as a u32 that orders the same way.
fn sortable(z: f32) -> u32 {
    let bits = bitcast<u32>(z);
    if (bits & 0x80000000u) != 0u {
        return ~bits;
    }
    return bits | 0x80000000u;
}

// The lower index of the t-th pair whose partners are j apart (j a power
// of two): (t / j) * 2j + t % j, without dividing.
fn lower(t: u32, j: u32) -> u32 {
    return ((t & ~(j - 1u)) << 1u) | (t & (j - 1u));
}

// Whether a sphere may hold a splat centre the draw keeps, erring on
// keeping (cluster::may_be_visible).
fn may_be_visible(centre: vec3<f32>, radius: f32) -> bool {
    for (var p = 0u; p < 5u; p++) {
        let plane = instance.planes[p];
        let along = dot(plane.xyz, centre);
        if along + plane.w < -radius - TOLERANCE * (abs(along) + abs(plane.w) + radius) {
            return false;
        }
    }
    return true;
}

const SH_C1: f32 = 0.4886025119029199;
const SH_C2_0: f32 = 1.0925484305920792;
const SH_C2_1: f32 = -1.0925484305920792;
const SH_C2_2: f32 = 0.31539156525252005;
const SH_C2_3: f32 = -1.0925484305920792;
const SH_C2_4: f32 = 0.5462742152960396;
const SH_C3_0: f32 = -0.5900435899266435;
const SH_C3_1: f32 = 2.890611442640554;
const SH_C3_2: f32 = -0.4570457994644658;
const SH_C3_3: f32 = 0.3731763325901154;
const SH_C3_4: f32 = -0.4570457994644658;
const SH_C3_5: f32 = 1.445305721320277;
const SH_C3_6: f32 = -0.5900435899266435;

// Words of packed coefficients per splat at a degree.
fn sh_words(degree: u32) -> u32 {
    let coefficients = (degree + 1u) * (degree + 1u) - 1u;
    return (3u * coefficients + 1u) / 2u;
}

// A half float's bits as an f32, exactly. Done by hand: unpack2x16float
// needs a capability not every backend has.
fn half_to_float(bits: u32) -> f32 {
    let sign = (bits & 0x8000u) << 16u;
    let exponent = (bits >> 10u) & 0x1Fu;
    let mantissa = bits & 0x3FFu;
    if exponent == 0u {
        // Zero or subnormal: mantissa * 2^-24.
        return bitcast<f32>(sign | bitcast<u32>(f32(mantissa) * 5.9604645e-8));
    }
    return bitcast<f32>(sign | ((exponent + 112u) << 23u) | (mantissa << 13u));
}

// Packed value j of the splat whose words start at base: the even one in
// the low 16 bits.
fn sh_value(base: u32, j: u32) -> f32 {
    let word = sh[base + j / 2u];
    return half_to_float((word >> (16u * (j & 1u))) & 0xFFFFu);
}

// Higher-band coefficient k (band 1 starts at 0) as (r, g, b).
fn sh_coefficient(base: u32, k: u32) -> vec3<f32> {
    return vec3<f32>(sh_value(base, 3u * k), sh_value(base, 3u * k + 1u), sh_value(base, 3u * k + 2u));
}

// The higher bands' colour for splat i seen along unit direction d, as in
// 3D Gaussian Splatting's computeColorFromSH.
fn sh_rest_color(i: u32, d: vec3<f32>, degree: u32) -> vec3<f32> {
    let base = i * sh_words(degree);
    let x = d.x;
    let y = d.y;
    let z = d.z;
    var color = -SH_C1 * y * sh_coefficient(base, 0u)
        + SH_C1 * z * sh_coefficient(base, 1u)
        - SH_C1 * x * sh_coefficient(base, 2u);
    if degree > 1u {
        let xx = x * x;
        let yy = y * y;
        let zz = z * z;
        let xy = x * y;
        let yz = y * z;
        let xz = x * z;
        color += SH_C2_0 * xy * sh_coefficient(base, 3u)
            + SH_C2_1 * yz * sh_coefficient(base, 4u)
            + SH_C2_2 * (2.0 * zz - xx - yy) * sh_coefficient(base, 5u)
            + SH_C2_3 * xz * sh_coefficient(base, 6u)
            + SH_C2_4 * (xx - yy) * sh_coefficient(base, 7u);
        if degree > 2u {
            color += SH_C3_0 * y * (3.0 * xx - yy) * sh_coefficient(base, 8u)
                + SH_C3_1 * xy * z * sh_coefficient(base, 9u)
                + SH_C3_2 * y * (4.0 * zz - xx - yy) * sh_coefficient(base, 10u)
                + SH_C3_3 * z * (2.0 * zz - 3.0 * xx - 3.0 * yy) * sh_coefficient(base, 11u)
                + SH_C3_4 * x * (4.0 * zz - xx - yy) * sh_coefficient(base, 12u)
                + SH_C3_5 * z * (xx - yy) * sh_coefficient(base, 13u)
                + SH_C3_6 * x * (xx - 3.0 * yy) * sh_coefficient(base, 14u);
        }
    }
    return color;
}

// One workgroup per cluster of HALF splats.
@compute @workgroup_size(256)
fn compute_keys(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    if t == 0u {
        let sphere = clusters[group.x];
        cluster_in_view = u32(may_be_visible(sphere.xyz, sphere.w));
    }
    if workgroupUniformLoad(&cluster_in_view) == 0u {
        if t == 0u {
            runs[group.x] = vec2<u32>(0u, 0u);
        }
        return;
    }

    let i = group.x * HALF + t;
    var kept = false;
    if i < instance.count {
        let splat = splats[i];
        kept = min(splat.opacity, MAX_ALPHA) >= MIN_ALPHA && may_be_visible(splat.position, 0.0);
    }

    // Each kept splat's place among the workgroup's: an inclusive scan.
    scan[t] = u32(kept);
    for (var offset = 1u; offset < HALF; offset = offset << 1u) {
        workgroupBarrier();
        var below = 0u;
        if t >= offset {
            below = scan[t - offset];
        }
        workgroupBarrier();
        scan[t] += below;
    }
    if t == HALF - 1u {
        packed_base = atomicAdd(&state.kept, scan[t]);
        runs[group.x] = vec2<u32>(packed_base, scan[t]);
    }
    let base = workgroupUniformLoad(&packed_base);
    if !kept {
        return;
    }

    let splat = splats[i];
    let slot = base + scan[t] - 1u;
    // Farthest first: view space looks down -z.
    let view_position = instance.view_model * vec4<f32>(splat.position, 1.0);
    keys[slot] = sortable(view_position.z);
    values[slot] = i;

    var color = splat.sh_dc;
    let to_splat = splat.position - instance.camera_local;
    if instance.sh_degree > 0u && dot(to_splat, to_splat) > 1e-12 {
        color += sh_rest_color(i, normalize(to_splat), instance.sh_degree);
    }
    colors[i] = vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

@compute @workgroup_size(1)
fn size_sort() {
    let kept = atomicLoad(&state.kept);
    state.sorted_count = kept;
    state.sorted_tiles = (kept + TILE - 1u) / TILE;
    state.draw[0] = 6u;
    state.draw[1] = kept;
    state.draw[2] = 0u;
    state.draw[3] = 0u;
    // Ready for the next sort.
    atomicStore(&state.kept, 0u);
}

// The number of pairs in tile g, the same for the whole workgroup. The
// first pass's tile g is the kept splats of clusters TILE_CLUSTERS * g on,
// in splat order; a later pass's is the pairs TILE * g on.
fn load_tile(g: u32, t: u32) -> u32 {
    if sort_step.shift == 0u {
        if t == 0u {
            var start = 0u;
            for (var k = 0u; k < TILE_CLUSTERS; k++) {
                let c = g * TILE_CLUSTERS + k;
                var run = vec2<u32>(0u, 0u);
                if c < (instance.count + HALF - 1u) / HALF {
                    run = runs[c];
                }
                tile_runs[k] = vec2<u32>(run.x, start);
                start += run.y;
            }
            tile_size = start;
        }
    } else if t == 0u {
        let count = state.sorted_count;
        let start = g * TILE;
        tile_size = select(0u, min(TILE, count - start), start < count);
    }
    return workgroupUniformLoad(&tile_size);
}

// Where pair n of tile g (n < its size) is in the pairs a pass reads.
fn tile_position(g: u32, n: u32) -> u32 {
    if sort_step.shift == 0u {
        // The last cluster starting at or before n: the one holding it, as
        // a cluster with no kept splats starts where the next one does.
        var k = 0u;
        for (var m = 1u; m < TILE_CLUSTERS; m++) {
            if tile_runs[m].y <= n {
                k = m;
            }
        }
        return tile_runs[k].x + n - tile_runs[k].y;
    }
    return g * TILE + n;
}

fn digit(key: u32) -> u32 {
    return (key >> sort_step.shift) & (RADIX - 1u);
}

// One workgroup per tile: count its pairs' digits.
@compute @workgroup_size(256)
fn count_digits(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    atomicStore(&digit_counts[t], 0u);
    let size = load_tile(group.x, t);
    // The first pass's scan reads every tile's counts; a later pass's only
    // the tiles of kept splats.
    if size == 0u && sort_step.shift != 0u {
        return;
    }
    for (var m = 0u; m < TILE / HALF; m++) {
        let n = m * HALF + t;
        if n < size {
            atomicAdd(&digit_counts[digit(keys[tile_position(group.x, n)])], 1u);
        }
    }
    workgroupBarrier();
    counts[RADIX + t * instance.tiles + group.x] = atomicLoad(&digit_counts[t]);
}

// An inclusive scan of each invocation's value over the workgroup; the last
// invocation's is the total.
fn workgroup_scan(t: u32, value: u32) -> u32 {
    workgroupBarrier();
    scan[t] = value;
    for (var offset = 1u; offset < HALF; offset = offset << 1u) {
        workgroupBarrier();
        var below = 0u;
        if t >= offset {
            below = scan[t - offset];
        }
        workgroupBarrier();
        scan[t] += below;
    }
    workgroupBarrier();
    return scan[t];
}

// One workgroup per digit: an exclusive scan of its counts over the tiles,
// four at a time per invocation, and its total.
@compute @workgroup_size(256)
fn scan_counts(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    if t == 0u {
        scan_rows = select(state.sorted_tiles, instance.tiles, sort_step.shift == 0u);
    }
    let rows = workgroupUniformLoad(&scan_rows);
    let row = RADIX + group.x * instance.tiles;
    var carry = 0u;
    for (var base = 0u; base < rows; base += 4u * HALF) {
        var tile_counts = array<u32, 4>(0u, 0u, 0u, 0u);
        var sum = 0u;
        for (var m = 0u; m < 4u; m++) {
            let n = base + 4u * t + m;
            if n < rows {
                tile_counts[m] = counts[row + n];
            }
            sum += tile_counts[m];
        }
        var running = carry + workgroup_scan(t, sum) - sum;
        for (var m = 0u; m < 4u; m++) {
            let n = base + 4u * t + m;
            if n < rows {
                counts[row + n] = running;
            }
            running += tile_counts[m];
        }
        carry += scan[HALF - 1u];
    }
    if t == 0u {
        counts[group.x] = carry;
    }
}

// An invocation's eight consecutive entries of a tile's local sort.
struct Eight {
    lo: vec4<u32>,
    hi: vec4<u32>,
};

// Whether the pairs whose lower entries are at these places sort upward in
// a merge of size k.
fn upward(places: vec4<u32>, k: u32) -> vec4<bool> {
    return (places & vec4<u32>(k)) == vec4<u32>(0u);
}

// The bitonic steps of a merge of size k whose partners are fewer than
// eight apart, from j_start apart down, over eight entries whose first is at
// place base. Swizzles only, so they stay in registers.
fn register_steps(entries: Eight, base: u32, k: u32, j_start: u32) -> Eight {
    var lo = entries.lo;
    var hi = entries.hi;
    if j_start >= 4u {
        // (0, 4), (1, 5), (2, 6), (3, 7)
        let up = upward(vec4<u32>(base) + vec4<u32>(0u, 1u, 2u, 3u), k);
        let first = select(max(lo, hi), min(lo, hi), up);
        let second = select(min(lo, hi), max(lo, hi), up);
        lo = first;
        hi = second;
    }
    if j_start >= 2u {
        // (0, 2), (1, 3), (4, 6), (5, 7)
        let a = vec4<u32>(lo.xy, hi.xy);
        let b = vec4<u32>(lo.zw, hi.zw);
        let up = upward(vec4<u32>(base) + vec4<u32>(0u, 1u, 4u, 5u), k);
        let first = select(max(a, b), min(a, b), up);
        let second = select(min(a, b), max(a, b), up);
        lo = vec4<u32>(first.xy, second.xy);
        hi = vec4<u32>(first.zw, second.zw);
    }
    // (0, 1), (2, 3), (4, 5), (6, 7)
    let a = vec4<u32>(lo.xz, hi.xz);
    let b = vec4<u32>(lo.yw, hi.yw);
    let up = upward(vec4<u32>(base) + vec4<u32>(0u, 2u, 4u, 6u), k);
    let first = select(max(a, b), min(a, b), up);
    let second = select(min(a, b), max(a, b), up);
    return Eight(
        vec4<u32>(first.x, second.x, first.y, second.y),
        vec4<u32>(first.z, second.z, first.w, second.w),
    );
}

fn store_eight(at: u32, entries: Eight) {
    local_sort[at] = entries.lo.x;
    local_sort[at + 1u] = entries.lo.y;
    local_sort[at + 2u] = entries.lo.z;
    local_sort[at + 3u] = entries.lo.w;
    local_sort[at + 4u] = entries.hi.x;
    local_sort[at + 5u] = entries.hi.y;
    local_sort[at + 6u] = entries.hi.z;
    local_sort[at + 7u] = entries.hi.w;
}

fn load_eight(at: u32) -> Eight {
    return Eight(
        vec4<u32>(local_sort[at], local_sort[at + 1u], local_sort[at + 2u], local_sort[at + 3u]),
        vec4<u32>(local_sort[at + 4u], local_sort[at + 5u], local_sort[at + 6u], local_sort[at + 7u]),
    );
}

// Entry n of tile g's local sort: the digit of its pair n and its place.
fn local_entry(g: u32, size: u32, n: u32) -> u32 {
    if n < size {
        return (digit(keys[tile_position(g, n)]) << PLACE_BITS) | n;
    }
    return 0xFFFFFFFFu;
}

// One workgroup per tile: sort it by digit in workgroup memory, keeping
// order within a digit, then write each digit's run where it goes.
@compute @workgroup_size(256)
fn scatter(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    let g = group.x;
    let size = load_tile(g, t);
    if size == 0u {
        return;
    }

    // Each digit's pairs go after every smaller digit's, and after its own
    // from earlier tiles.
    let total = counts[t];
    let below = workgroup_scan(t, total) - total;
    digit_place[t] = below + counts[RADIX + t * instance.tiles + g];

    // A bitonic network over (digit, place): places are unique, so the
    // order is total and the sort stable by digit. Each invocation keeps
    // eight consecutive entries in registers, where the steps whose partners
    // are fewer than eight apart run; the rest run in workgroup memory.
    // Read coalesced, a pair per invocation at a time.
    for (var m = 0u; m < TILE / HALF; m++) {
        let n = m * HALF + t;
        local_sort[n] = local_entry(g, size, n);
    }
    workgroupBarrier();
    let base = 8u * t;
    var entries = load_eight(base);
    entries = register_steps(entries, base, 2u, 1u);
    entries = register_steps(entries, base, 4u, 2u);
    entries = register_steps(entries, base, 8u, 4u);
    for (var k = 16u; k <= TILE; k = k << 1u) {
        store_eight(base, entries);
        for (var j = k >> 1u; j >= 8u; j = j >> 1u) {
            workgroupBarrier();
            for (var m = 0u; m < TILE / (2u * HALF); m++) {
                let a = lower(m * HALF + t, j);
                let b = a + j;
                let first = min(local_sort[a], local_sort[b]);
                let second = max(local_sort[a], local_sort[b]);
                let ascending = (a & k) == 0u;
                local_sort[a] = select(second, first, ascending);
                local_sort[b] = select(first, second, ascending);
            }
        }
        workgroupBarrier();
        entries = register_steps(load_eight(base), base, k, 4u);
    }
    store_eight(base, entries);
    workgroupBarrier();

    for (var m = 0u; m < TILE / HALF; m++) {
        let p = m * HALF + t;
        if p < size {
            let d = local_sort[p] >> PLACE_BITS;
            if p == 0u || (local_sort[p - 1u] >> PLACE_BITS) != d {
                run_start[d] = p;
            }
        }
    }
    workgroupBarrier();

    // Consecutive invocations write consecutive places of a digit's run.
    for (var m = 0u; m < TILE / HALF; m++) {
        let p = m * HALF + t;
        if p < size {
            let entry = local_sort[p];
            let d = entry >> PLACE_BITS;
            let source = tile_position(g, entry & ((1u << PLACE_BITS) - 1u));
            let dest = digit_place[d] + p - run_start[d];
            values_out[dest] = values[source];
            // The last pass's keys are not read again.
            if sort_step.shift + DIGIT_BITS < 32u {
                keys_out[dest] = keys[source];
            }
        }
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    /// The CPU order the GPU must match: stable by view-space depth.
    pub(crate) fn reference_order(positions: &[[f32; 3]], view_model: Mat4) -> Vec<u32> {
        let mut order: Vec<u32> = (0..positions.len() as u32).collect();
        order.sort_by_key(|&i| {
            let z = view_model
                .transform_point3(Vec3::from(positions[i as usize]))
                .z;
            (sortable_depth(z), i)
        });
        order
    }

    /// What the keys pass leaves for the radix passes: the pairs of the
    /// splats `kept`, packed a cluster at a time with the clusters in
    /// `cluster_order`, and each cluster's run (where its kept splats start,
    /// and how many).
    fn pack(
        keys_of: &[u32],
        kept: &[bool],
        cluster_order: &[u32],
    ) -> (Vec<u32>, Vec<u32>, Vec<[u32; 2]>) {
        let count = keys_of.len();
        let mut keys = vec![0; count.max(1)];
        let mut values = vec![0; count.max(1)];
        let mut runs = vec![[0, 0]; count.div_ceil(CLUSTER)];
        let mut next = 0u32;
        for &c in cluster_order {
            let c = c as usize;
            let base = next;
            for i in c * CLUSTER..((c + 1) * CLUSTER).min(count) {
                if kept[i] {
                    keys[next as usize] = keys_of[i];
                    values[next as usize] = i as u32;
                    next += 1;
                }
            }
            runs[c] = [base, next - base];
        }
        (keys, values, runs)
    }

    /// Run the radix passes on the CPU exactly as the kernels index them,
    /// from what the keys pass leaves ([`pack`]) for a cloud of `count`.
    fn emulate(keys: &mut Vec<u32>, values: &mut Vec<u32>, runs: &[[u32; 2]], count: u32) {
        let tiles = tiles(count.max(1));
        let kept: u32 = runs.iter().map(|run| run[1]).sum();
        let mut other_keys = vec![0; keys.len()];
        let mut other_values = vec![0; values.len()];
        let mut counts = vec![0u32; (RADIX + RADIX * tiles) as usize];
        let row = |d: u32| (RADIX + d * tiles) as usize;
        // load_tile and tile_position: tile g's size, and where each of its
        // pairs is.
        let tile = |shift: u32, g: u32| -> Vec<u32> {
            if shift != 0 {
                let start = g * TILE;
                return (start..kept.min(start + TILE)).collect();
            }
            let mut starts = [[0u32; 2]; TILE_CLUSTERS as usize];
            let mut start = 0;
            for (k, entry) in starts.iter_mut().enumerate() {
                let run = runs
                    .get((g * TILE_CLUSTERS) as usize + k)
                    .copied()
                    .unwrap_or([0, 0]);
                *entry = [run[0], start];
                start += run[1];
            }
            (0..start)
                .map(|n| {
                    let k = (1..starts.len()).rfind(|&m| starts[m][1] <= n).unwrap_or(0);
                    starts[k][0] + n - starts[k][1]
                })
                .collect()
        };
        for pass in sort_passes() {
            let (SortPass::Count { shift }
            | SortPass::Scan { shift }
            | SortPass::Scatter { shift }) = pass
            else {
                continue;
            };
            let digit = |key: u32| (key >> shift) & (RADIX - 1);
            match pass {
                SortPass::Count { .. } => {
                    for g in 0..tiles {
                        let positions = tile(shift, g);
                        if positions.is_empty() && shift != 0 {
                            continue;
                        }
                        let mut digits = [0u32; RADIX as usize];
                        for &p in &positions {
                            digits[digit(keys[p as usize]) as usize] += 1;
                        }
                        for d in 0..RADIX {
                            counts[row(d) + g as usize] = digits[d as usize];
                        }
                    }
                }
                SortPass::Scan { .. } => {
                    let rows = if shift == 0 {
                        tiles
                    } else {
                        kept.div_ceil(TILE)
                    };
                    for d in 0..RADIX {
                        let mut running = 0;
                        for n in 0..rows as usize {
                            let tile_count = counts[row(d) + n];
                            counts[row(d) + n] = running;
                            running += tile_count;
                        }
                        counts[d as usize] = running;
                    }
                }
                _ => {
                    for g in 0..tiles {
                        let positions = tile(shift, g);
                        let size = positions.len() as u32;
                        if size == 0 {
                            continue;
                        }
                        let mut place = [0u32; RADIX as usize];
                        let mut below = 0;
                        for d in 0..RADIX {
                            place[d as usize] = below + counts[row(d) + g as usize];
                            below += counts[d as usize];
                        }
                        let place_bits = TILE.trailing_zeros();
                        let mut local: Vec<u32> = (0..TILE)
                            .map(|n| match positions.get(n as usize) {
                                Some(&p) => digit(keys[p as usize]) << place_bits | n,
                                None => u32::MAX,
                            })
                            .collect();
                        // The workgroup's bitonic network: steps whose
                        // partners are fewer than eight apart on each
                        // invocation's eight consecutive entries, paired as
                        // register_steps pairs them; the rest a comparator
                        // per invocation per step.
                        let exchange = |local: &mut [u32], a: usize, b: usize, up: bool| {
                            let (x, y) = (local[a], local[b]);
                            local[a] = if up { x.min(y) } else { x.max(y) };
                            local[b] = if up { x.max(y) } else { x.min(y) };
                        };
                        let register_steps = |local: &mut [u32], k: u32, j_start: u32| {
                            let mut steps = Vec::new();
                            if j_start >= 4 {
                                steps.push([(0, 4), (1, 5), (2, 6), (3, 7)]);
                            }
                            if j_start >= 2 {
                                steps.push([(0, 2), (1, 3), (4, 6), (5, 7)]);
                            }
                            steps.push([(0, 1), (2, 3), (4, 5), (6, 7)]);
                            for base in (0..TILE).step_by(8) {
                                for pairs in &steps {
                                    for &(i, j) in pairs {
                                        let up = (base + i) & k == 0;
                                        exchange(
                                            local,
                                            (base + i) as usize,
                                            (base + j) as usize,
                                            up,
                                        );
                                    }
                                }
                            }
                        };
                        register_steps(&mut local, 2, 1);
                        register_steps(&mut local, 4, 2);
                        register_steps(&mut local, 8, 4);
                        let mut k = 16;
                        while k <= TILE {
                            let mut j = k / 2;
                            while j >= 8 {
                                for c in 0..TILE / 2 {
                                    let a = ((c & !(j - 1)) << 1) | (c & (j - 1));
                                    exchange(&mut local, a as usize, (a + j) as usize, a & k == 0);
                                }
                                j /= 2;
                            }
                            register_steps(&mut local, k, 4);
                            k *= 2;
                        }
                        let mut run_start = [0u32; RADIX as usize];
                        for p in 0..size as usize {
                            let d = local[p] >> place_bits;
                            if p == 0 || local[p - 1] >> place_bits != d {
                                run_start[d as usize] = p as u32;
                            }
                        }
                        for p in 0..size {
                            let entry = local[p as usize];
                            let d = (entry >> place_bits) as usize;
                            let source = positions[(entry & (TILE - 1)) as usize] as usize;
                            let dest = (place[d] + p - run_start[d]) as usize;
                            other_values[dest] = values[source];
                            if shift + DIGIT_BITS < 32 {
                                other_keys[dest] = keys[source];
                            }
                        }
                    }
                    std::mem::swap(keys, &mut other_keys);
                    std::mem::swap(values, &mut other_values);
                }
            }
        }
    }

    fn random_positions(count: usize, seed: u64) -> Vec<[f32; 3]> {
        let mut state = seed;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f32 / (1u64 << 24) as f32) * 40.0 - 20.0
        };
        (0..count)
            .map(|i| {
                // Some exact ties, so the index tie-break is exercised.
                if i % 7 == 3 {
                    [1.0, 2.0, 3.0]
                } else {
                    [next(), next(), next()]
                }
            })
            .collect()
    }

    fn view_model() -> Mat4 {
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(3.0, 4.0, 12.0), Vec3::ZERO, Vec3::Y);
        let model = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 2.0, 0.5),
            glam::Quat::from_rotation_y(0.7),
            Vec3::new(-2.0, 0.0, 1.0),
        );
        view * model
    }

    #[test]
    fn passes_sort_every_digit() {
        let passes = sort_passes();
        assert_eq!(passes.len(), 2 + 3 * DIGITS as usize);
        assert_eq!(passes[..2], [SortPass::Keys, SortPass::Size]);
        for (digit, three) in passes[2..].chunks(3).enumerate() {
            let shift = digit as u32 * DIGIT_BITS;
            assert_eq!(
                three,
                [
                    SortPass::Count { shift },
                    SortPass::Scan { shift },
                    SortPass::Scatter { shift }
                ]
            );
        }
        assert_eq!((tiles(1), tiles(TILE), tiles(TILE + 1)), (1, 1, 2));
        // The first pass's tiles of clusters are the later passes' tiles.
        let max = super::super::MAX_SPLATS as u32;
        assert_eq!(
            tiles(max),
            max.div_ceil(CLUSTER as u32).div_ceil(TILE_CLUSTERS)
        );
        assert!(tiles(max) <= 65_535);
        // The shader's sizes are these.
        for constant in [
            format!("const DIGIT_BITS: u32 = {DIGIT_BITS}u;"),
            format!("const RADIX: u32 = {RADIX}u;"),
            format!("const TILE: u32 = {TILE}u;"),
            format!("const TILE_CLUSTERS: u32 = {TILE_CLUSTERS}u;"),
            format!("const PLACE_BITS: u32 = {}u;", TILE.trailing_zeros()),
        ] {
            assert!(SORT_WGSL.contains(&constant), "{constant}");
        }
    }

    #[test]
    fn sortable_depth_orders_like_the_float() {
        let values = [-1e30, -3.5, -1.0, -1e-30, -0.0, 0.0, 1e-30, 1.0, 2.5, 1e30];
        for pair in values.windows(2) {
            assert!(
                sortable_depth(pair[0]) <= sortable_depth(pair[1]),
                "{pair:?}"
            );
        }
    }

    /// The radix passes, run as the kernels index them, give the reference
    /// order for counts below, at and across cluster and tile boundaries:
    /// with every splat kept or only some, and the clusters packed in any
    /// order.
    #[test]
    fn emulated_passes_sort_like_the_reference() {
        let view_model = view_model();
        for count in [1usize, 7, 255, 256, 2047, 2048, 2049, 5000, 20_001] {
            let keys_of: Vec<u32> = random_positions(count, count as u64)
                .iter()
                .map(|&p| sortable_depth(view_model.transform_point3(p.into()).z))
                .collect();
            let clusters = count.div_ceil(CLUSTER) as u32;
            let every = vec![true; count];
            // Every third splat dropped, and every fifth cluster.
            let some: Vec<bool> = (0..count)
                .map(|i| i % 3 != 1 && (i / CLUSTER) % 5 != 2)
                .collect();
            let none = vec![false; count];
            let forward: Vec<u32> = (0..clusters).collect();
            let backward: Vec<u32> = (0..clusters).rev().collect();
            let odd_first: Vec<u32> = (0..clusters)
                .filter(|c| c % 2 == 1)
                .chain((0..clusters).filter(|c| c % 2 == 0))
                .collect();
            for kept in [every, some, none] {
                let mut expected: Vec<u32> =
                    (0..count as u32).filter(|&i| kept[i as usize]).collect();
                expected.sort_by_key(|&i| (keys_of[i as usize], i));
                for order in [&forward, &backward, &odd_first] {
                    let (mut keys, mut values, runs) = pack(&keys_of, &kept, order);
                    emulate(&mut keys, &mut values, &runs, count as u32);
                    assert_eq!(
                        values[..expected.len()],
                        expected[..],
                        "count {count}, {} kept",
                        expected.len()
                    );
                }
            }
        }
    }

    fn gpu() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("no GPU adapter");
        pollster::block_on(adapter.request_device(&Default::default())).unwrap()
    }

    fn read_u32s(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer) -> Vec<u32> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: buffer.size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
        queue.submit(std::iter::once(encoder.finish()));
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll");
        let bytes = staging
            .slice(..)
            .get_mapped_range()
            .expect("mapped staging buffer")
            .to_vec();
        bytemuck::cast_slice(&bytes).to_vec()
    }

    /// A 60° camera at `eye` looking at `target`, as the engine projects.
    fn camera(eye: Vec3, target: Vec3) -> SplatCamera {
        SplatCamera {
            view: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
            projection: glam::camera::rh::proj::directx::perspective(
                60f32.to_radians(),
                16.0 / 9.0,
                0.1,
                1000.0,
            ),
            near: 0.1,
        }
    }

    fn splat_points(positions: &[[f32; 3]]) -> Vec<naive_sim::assets::SplatPoint> {
        positions
            .iter()
            .map(|&position| naive_sim::assets::SplatPoint {
                position,
                opacity: 1.0,
                scale: [1.0; 3],
                rotation: [1.0, 0.0, 0.0, 0.0],
                sh_dc: [1.0; 3],
            })
            .collect()
    }

    fn upload(device: &wgpu::Device, splats: Vec<naive_sim::assets::SplatPoint>) -> GpuSplat {
        super::super::upload_splat(
            device,
            "test",
            naive_sim::assets::SplatData {
                splats,
                sh_degree: 0,
                sh_rest: Vec::new(),
            },
        )
        .unwrap()
    }

    /// Cull and sort `instance` once; the sorted indices of the splats kept
    /// (in upload order) and the draw's arguments.
    fn run(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sorter: &SplatSorter,
        instance: &mut SplatInstance,
        model: Mat4,
        camera: SplatCamera,
    ) -> (Vec<u32>, Vec<u32>) {
        instance.update(queue, model, camera);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            sorter.encode(&mut pass, instance);
        }
        queue.submit(std::iter::once(encoder.finish()));
        let draw = read_u32s(device, queue, &instance.state)[4..].to_vec();
        let mut sorted = read_u32s(device, queue, &instance.sorted_indices);
        sorted.truncate(draw[1] as usize);
        (sorted, draw)
    }

    /// The compute shaders on a real GPU sort like the CPU, when every
    /// splat is in view.
    /// `cargo test -p naive-client --lib -- --ignored gpu_splat_sort`
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_splat_sort_matches_the_cpu_reference() {
        let (device, queue) = gpu();
        let sorter = SplatSorter::new(&device);
        for count in [1usize, 7, 512, 513, 4097, 70_001, 1 << 20] {
            // Exact: integer depths moved by a translation are computed
            // without rounding on both sides, and most depths tie. The
            // upload reorders the splats, so the tie-break follows its order.
            let positions: Vec<[f32; 3]> = random_positions(count, count as u64)
                .iter()
                .map(|p| [p[0], p[1], (p[2] * 10.0).round()])
                .collect();
            let order = cluster::morton_order(&positions);
            let uploaded: Vec<[f32; 3]> = order.iter().map(|&i| positions[i as usize]).collect();
            let splat = upload(&device, splat_points(&positions));
            let mut instance = sorter.instance(&device, &splat, crate::components::SplatHandle(0));
            let model = Mat4::from_translation(Vec3::new(0.0, 0.0, -500.0));
            let straight_ahead = SplatCamera {
                view: Mat4::IDENTITY,
                ..camera(Vec3::ZERO, Vec3::NEG_Z)
            };
            let (sorted, draw) = run(
                &device,
                &queue,
                &sorter,
                &mut instance,
                model,
                straight_ahead,
            );
            assert_eq!(draw, [6, count as u32, 0, 0], "count {count}: all in view");
            assert!(!instance.update(&queue, model, straight_ahead), "unchanged");
            let reference = reference_order(&uploaded, model);
            let first = sorted.iter().zip(&reference).position(|(a, b)| a != b);
            assert_eq!(first, None, "count {count}: first difference");

            // Partly behind the camera and beside the view, still exact:
            // the splats kept, by depth and then by index.
            let partly = Mat4::from_translation(Vec3::new(0.0, 0.0, -100.0));
            let (kept, _) = run(
                &device,
                &queue,
                &sorter,
                &mut instance,
                partly,
                straight_ahead,
            );
            let key = |s: u32| {
                sortable_depth(partly.transform_point3(Vec3::from(uploaded[s as usize])).z)
            };
            let mut expected = kept.clone();
            expected.sort_by_key(|&s| (key(s), s));
            let first = kept.iter().zip(&expected).position(|(a, b)| a != b);
            assert_eq!(
                first, None,
                "count {count}: partly culled, first difference"
            );
            if count > 1000 {
                assert!(
                    kept.len() > count / 4 && kept.len() < count * 9 / 10,
                    "count {count}: {} kept",
                    kept.len()
                );
            }

            // Any transform: a permutation, farthest first. Depths the GPU
            // rounds differently may swap neighbours that are this close.
            let seen_from = camera(Vec3::new(30.0, 40.0, 150.0), Vec3::ZERO);
            let model = Mat4::from_scale_rotation_translation(
                Vec3::new(1.0, 2.0, 0.5),
                glam::Quat::from_rotation_y(0.7),
                Vec3::new(-2.0, 0.0, 1.0),
            );
            let positions = random_positions(count, 9 + count as u64);
            let order = cluster::morton_order(&positions);
            let splat = upload(&device, splat_points(&positions));
            let mut instance = sorter.instance(&device, &splat, crate::components::SplatHandle(0));
            let (sorted, draw) = run(&device, &queue, &sorter, &mut instance, model, seen_from);
            assert_eq!(draw[1], count as u32, "count {count}: all in view");
            let mut seen = vec![false; count];
            sorted.iter().for_each(|&s| seen[s as usize] = true);
            assert!(seen.iter().all(|&s| s), "count {count}: not a permutation");
            let depth = |s: u32| {
                (seen_from.view * model)
                    .transform_point3(Vec3::from(positions[order[s as usize] as usize]))
                    .z
            };
            for pair in sorted.windows(2) {
                let (a, b) = (depth(pair[0]), depth(pair[1]));
                assert!(
                    a <= b + 1e-5 * b.abs().max(1.0),
                    "count {count}: {a} before {b}"
                );
            }
        }
    }

    /// The cull on a real GPU keeps every splat the draw would draw, and
    /// little else; the sort orders what it keeps; and the draw's count
    /// follows, back to none and up again.
    /// `cargo test -p naive-client --lib -- --ignored gpu_splat_cull`
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_splat_cull_keeps_what_the_draw_would() {
        let (device, queue) = gpu();
        let sorter = SplatSorter::new(&device);
        let count = 200_000;
        // A cloud around the camera, some splats too faint to draw.
        let positions = random_positions(count, 17);
        let mut splats = splat_points(&positions);
        for (i, splat) in splats.iter_mut().enumerate() {
            if i % 11 == 0 {
                splat.opacity = 0.003;
            }
        }
        let order = cluster::morton_order(&positions);
        let splat = upload(&device, splats);
        let mut instance = sorter.instance(&device, &splat, crate::components::SplatHandle(0));
        let model = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 1.5, 0.8),
            glam::Quat::from_rotation_y(0.4),
            Vec3::new(1.0, -2.0, 0.5),
        );
        let looking = camera(Vec3::new(1.0, 2.0, 3.0), Vec3::new(6.0, 0.0, -10.0));

        let (sorted, draw) = run(&device, &queue, &sorter, &mut instance, model, looking);
        let check = |sorted: &[u32], draw: &[u32]| {
            let kept = draw[1] as usize;
            assert_eq!(draw, [6, kept as u32, 0, 0]);
            assert_eq!(sorted.len(), kept);
            let mut is_kept = vec![false; count];
            sorted.iter().for_each(|&s| is_kept[s as usize] = true);
            let mut drawn = 0;
            for (s, &original) in order.iter().enumerate() {
                let p = Vec3::from(positions[original as usize]);
                let margin =
                    cluster::draw_margin(model, looking.view, looking.projection, looking.near, p);
                let faint = original % 11 == 0;
                if margin > 1e-4 && !faint {
                    drawn += 1;
                    assert!(is_kept[s], "splat {original} at {p} ({margin}) culled");
                }
                if is_kept[s] {
                    assert!(!faint, "kept faint splat {original}");
                    assert!(
                        margin > -1e-3,
                        "kept splat {original}, {margin} out of view"
                    );
                }
            }
            assert!(
                drawn > 5_000 && kept < count / 4,
                "{drawn} drawn, {kept} kept"
            );
            let depth = |s: u32| {
                (looking.view * model)
                    .transform_point3(Vec3::from(positions[order[s as usize] as usize]))
                    .z
            };
            for pair in sorted.windows(2) {
                let (a, b) = (depth(pair[0]), depth(pair[1]));
                assert!(a <= b + 1e-5 * b.abs().max(1.0), "{a} before {b}");
            }
        };
        check(&sorted, &draw);

        // Turned around, nothing is in view.
        let away = camera(Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 2.0, 3.0) - Vec3::Z);
        let far_off = Mat4::from_translation(Vec3::new(0.0, 0.0, 500.0));
        let (none, draw) = run(&device, &queue, &sorter, &mut instance, far_off, away);
        assert_eq!((none.len(), draw), (0, vec![6, 0, 0, 0]));

        // And back: the count starts from zero each sort.
        let (again, draw) = run(&device, &queue, &sorter, &mut instance, model, looking);
        assert_eq!(again, sorted);
        check(&again, &draw);
    }

    /// 3D Gaussian Splatting's computeColorFromSH, on the CPU: the base
    /// colour plus the higher bands for unit direction `d`, clamped.
    fn reference_color(dc: [f32; 3], rest: &[f32], degree: u32, d: Vec3) -> [f32; 3] {
        const C1: f32 = 0.488_602_5;
        const C2: [f32; 5] = [
            1.092_548_4,
            -1.092_548_4,
            0.315_391_57,
            -1.092_548_4,
            0.546_274_2,
        ];
        const C3: [f32; 7] = [
            -0.590_043_6,
            2.890_611_4,
            -0.457_045_8,
            0.373_176_33,
            -0.457_045_8,
            1.445_305_7,
            -0.590_043_6,
        ];
        let (x, y, z) = (d.x, d.y, d.z);
        let (xx, yy, zz, xy, yz, xz) = (x * x, y * y, z * z, x * y, y * z, x * z);
        let basis = [
            -C1 * y,
            C1 * z,
            -C1 * x,
            C2[0] * xy,
            C2[1] * yz,
            C2[2] * (2.0 * zz - xx - yy),
            C2[3] * xz,
            C2[4] * (xx - yy),
            C3[0] * y * (3.0 * xx - yy),
            C3[1] * xy * z,
            C3[2] * y * (4.0 * zz - xx - yy),
            C3[3] * z * (2.0 * zz - 3.0 * xx - 3.0 * yy),
            C3[4] * x * (4.0 * zz - xx - yy),
            C3[5] * z * (xx - yy),
            C3[6] * x * (xx - 3.0 * yy),
        ];
        let n = naive_sim::assets::sh_rest_coefficients(degree);
        std::array::from_fn(|c| {
            let higher: f32 = (0..n).map(|k| basis[k] * rest[3 * k + c]).sum();
            (dc[c] + higher).clamp(0.0, 1.0)
        })
    }

    /// The sort pass's colours on a real GPU match the CPU formula for
    /// every degree, through an entity transform.
    /// `cargo test -p naive-client --lib -- --ignored gpu_splat_colors`
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_splat_colors_match_the_cpu_spherical_harmonics() {
        let (device, queue) = gpu();
        let sorter = SplatSorter::new(&device);
        // Far enough back to see every splat, so every colour is written.
        let seen_from = camera(Vec3::new(60.0, 80.0, 240.0), Vec3::ZERO);
        let model = Mat4::from_scale_rotation_translation(
            Vec3::splat(1.5),
            glam::Quat::from_rotation_y(0.7) * glam::Quat::from_rotation_x(-0.3),
            Vec3::new(-2.0, 0.5, 1.0),
        );
        let count = 3000;
        let positions = random_positions(count, 5);
        let order = cluster::morton_order(&positions);
        let mut state = 77u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 40) as f32 / (1u64 << 24) as f32
        };
        for degree in 0..=naive_sim::assets::MAX_SH_DEGREE {
            let per_splat = 3 * naive_sim::assets::sh_rest_coefficients(degree);
            let splats: Vec<_> = positions
                .iter()
                .map(|&position| naive_sim::assets::SplatPoint {
                    position,
                    opacity: 1.0,
                    scale: [1.0; 3],
                    rotation: [1.0, 0.0, 0.0, 0.0],
                    // Some outside [0, 1], so the clamp is exercised.
                    sh_dc: [next() * 1.4 - 0.2, next() * 1.4 - 0.2, next() * 1.4 - 0.2],
                })
                .collect();
            // Rounded to half floats here, as the upload stores them.
            let sh_rest: Vec<f32> = (0..count * per_splat)
                .map(|_| half::f16::from_f32(next() * 1.2 - 0.6).to_f32())
                .collect();
            let splat = super::super::upload_splat(
                &device,
                "test",
                naive_sim::assets::SplatData {
                    splats: splats.clone(),
                    sh_degree: degree,
                    sh_rest: sh_rest.clone(),
                },
            )
            .unwrap();
            let mut instance = sorter.instance(&device, &splat, crate::components::SplatHandle(0));
            let (_, draw) = run(&device, &queue, &sorter, &mut instance, model, seen_from);
            assert_eq!(draw[1], count as u32, "all in view");
            let colors: Vec<f32> = read_u32s(&device, &queue, &instance.colors)
                .into_iter()
                .map(f32::from_bits)
                .collect();

            let camera_position = seen_from.view.inverse().w_axis.truncate();
            let camera_local = model.inverse().transform_point3(camera_position);
            // The colours are in upload order.
            for (s, &i) in order.iter().enumerate() {
                let i = i as usize;
                let point = &splats[i];
                let d = (Vec3::from(point.position) - camera_local).normalize();
                let rest = &sh_rest[i * per_splat..(i + 1) * per_splat];
                let expected = reference_color(point.sh_dc, rest, degree, d);
                let got = &colors[s * 4..s * 4 + 3];
                for c in 0..3 {
                    assert!(
                        (got[c] - expected[c]).abs() < 1e-4,
                        "degree {degree} splat {i} channel {c}: {} vs {}",
                        got[c],
                        expected[c]
                    );
                }
            }
        }
    }
}
