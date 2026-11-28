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
//! index. The size pass reads how many were kept: the length they sort at
//! and the draw's instance count. The pad pass fills the kept splats out to
//! a power of two with keys that sort last, and a bitonic network sorts the
//! (key, index) pairs: every block of [`BLOCK`] pairs is sorted in workgroup
//! memory, then larger merges take one dispatch per step until the step fits
//! a block again. Every pass is dispatched for the whole cloud, and its
//! workgroups past the kept splats, or in merges longer than they need,
//! stop at once, so the sort's work and the draw scale with the splats in
//! view. (Dispatching the network indirectly, sized on the GPU, did the same
//! work but cost about 20 µs more per dispatch on Metal: more than it saved
//! for a cloud mostly in view.)
//!
//! Ties in depth are broken by splat index, so the order is deterministic,
//! whatever order the workgroups packed the splats in, and the same as a
//! stable sort by view-space depth.
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

/// Pairs one workgroup sorts in workgroup memory (two per invocation).
pub const BLOCK: u32 = 512;
const WORKGROUP: u32 = BLOCK / 2;
const _: () = assert!(CLUSTER as u32 == WORKGROUP, "one workgroup per cluster");

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
    pub padded: u32,
    pub _pad: [u32; 2],
}

/// One dispatch of the sort.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SortPass {
    /// Cull clusters and splats; pack each kept splat's depth key and index
    /// at the front, and write its colour.
    Keys,
    /// Size the sort and the draw to the kept splats.
    Size,
    /// Fill the kept splats out to the sort's length with keys that sort
    /// last.
    Pad,
    /// Sort every block, alternating direction as the network needs.
    LocalSort,
    /// A compare-exchange step whose partners are `j` apart (`j >= BLOCK`).
    GlobalStep { k: u32, j: u32 },
    /// The rest of the merge of size `k`, inside each block.
    LocalMerge { k: u32 },
}

impl SortPass {
    /// The shortest sort this pass is part of. A sort of `len` pairs runs the
    /// network's passes up to `len`; the shaders skip the rest.
    pub fn min_len(self) -> u32 {
        match self {
            SortPass::Keys | SortPass::Size | SortPass::Pad => 0,
            SortPass::LocalSort => BLOCK,
            SortPass::GlobalStep { k, .. } | SortPass::LocalMerge { k } => k,
        }
    }
}

/// The sort's length for `count` splats: a power of two, at least a block.
pub fn padded_len(count: u32) -> u32 {
    count.max(1).next_power_of_two().max(BLOCK)
}

/// The dispatches that sort up to `padded` pairs, in order.
pub fn sort_passes(padded: u32) -> Vec<SortPass> {
    debug_assert!(padded.is_power_of_two() && padded >= BLOCK);
    let mut passes = vec![
        SortPass::Keys,
        SortPass::Size,
        SortPass::Pad,
        SortPass::LocalSort,
    ];
    let mut k = BLOCK * 2;
    while k <= padded {
        let mut j = k / 2;
        while j >= BLOCK {
            passes.push(SortPass::GlobalStep { k, j });
            j /= 2;
        }
        passes.push(SortPass::LocalMerge { k });
        k *= 2;
    }
    passes
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

/// The shared compute pipelines, made with the first splat.
pub struct SplatSorter {
    layout: wgpu::BindGroupLayout,
    keys: wgpu::ComputePipeline,
    size: wgpu::ComputePipeline,
    pad: wgpu::ComputePipeline,
    local_sort: wgpu::ComputePipeline,
    global_step: wgpu::ComputePipeline,
    local_merge: wgpu::ComputePipeline,
    /// Bytes between the step parameters of consecutive passes.
    step_stride: u32,
}

impl SplatSorter {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Splat sort"),
            source: wgpu::ShaderSource::Wgsl(SORT_WGSL.into()),
        });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let uniform = |binding, has_dynamic_offset| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splat sort"),
            entries: &[
                storage(0, true),
                storage(1, false),
                storage(2, false),
                storage(5, true),
                storage(6, false),
                storage(7, false),
                storage(8, true),
                uniform(3, false),
                uniform(4, true),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Splat sort"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            keys: pipeline("compute_keys"),
            size: pipeline("size_sort"),
            pad: pipeline("pad"),
            local_sort: pipeline("local_sort"),
            global_step: pipeline("global_step"),
            local_merge: pipeline("local_merge"),
            layout,
            step_stride: device.limits().min_uniform_buffer_offset_alignment.max(16),
        }
    }

    /// Sort state for one entity drawing `splat`.
    pub fn instance(
        &self,
        device: &wgpu::Device,
        splat: &GpuSplat,
        handle: crate::components::SplatHandle,
    ) -> SplatInstance {
        let padded = padded_len(splat.splat_count);
        let passes = sort_passes(padded);
        let stride = self.step_stride as usize;
        let mut steps = vec![0u8; passes.len() * stride];
        for (n, pass) in passes.iter().enumerate() {
            let (k, j) = match *pass {
                SortPass::GlobalStep { k, j } => (k, j),
                SortPass::LocalMerge { k } => (k, 0),
                _ => (0, 0),
            };
            steps[n * stride..n * stride + 8].copy_from_slice(bytemuck::cast_slice(&[k, j]));
        }
        let steps = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Splat sort steps"),
            contents: &steps,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let pairs = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: u64::from(padded) * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let keys = pairs("Splat sort keys");
        let sorted_indices = pairs("Splat sorted indices");
        let colors = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splat colours"),
            size: u64::from(splat.splat_count.max(1)) * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
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
        fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Splat sort"),
            layout: &self.layout,
            entries: &[
                entry(0, &splat.splat_buffer),
                entry(1, &keys),
                entry(2, &sorted_indices),
                entry(3, &uniforms),
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &steps,
                        offset: 0,
                        size: wgpu::BufferSize::new(16),
                    }),
                },
                entry(5, &splat.sh_buffer),
                entry(6, &colors),
                entry(7, &state),
                entry(8, &splat.cluster_buffer),
            ],
        });
        SplatInstance {
            handle,
            count: splat.splat_count,
            sh_degree: splat.sh_degree,
            padded,
            clusters: splat.cluster_count,
            passes,
            _keys: keys,
            sorted_indices,
            colors,
            uniforms,
            state,
            _steps: steps,
            bind_group,
            written: None,
        }
    }

    /// Record the dispatches that cull and sort `instance`.
    pub fn encode(&self, pass: &mut wgpu::ComputePass<'_>, instance: &SplatInstance) {
        for (n, step) in instance.passes.iter().enumerate() {
            let (pipeline, groups) = match step {
                SortPass::Keys => (&self.keys, instance.clusters),
                SortPass::Size => (&self.size, 1),
                // Two pairs per invocation, so a sort of 2^24 pairs still
                // fits in a dispatch.
                SortPass::Pad => (&self.pad, instance.padded / BLOCK),
                SortPass::LocalSort => (&self.local_sort, instance.padded / BLOCK),
                SortPass::GlobalStep { .. } => (&self.global_step, instance.padded / BLOCK),
                SortPass::LocalMerge { .. } => (&self.local_merge, instance.padded / BLOCK),
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &instance.bind_group, &[n as u32 * self.step_stride]);
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
    padded: u32,
    clusters: u32,
    passes: Vec<SortPass>,
    _keys: wgpu::Buffer,
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
    _steps: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
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
            padded: self.padded,
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

/// Culling and keys, sizing, padding, and the bitonic network over
/// (key, index) pairs with its block passes.
pub const SORT_WGSL: &str = r#"
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

struct SortStep {
    k: u32,
    j: u32,
    _pad: vec2<u32>,
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
    // What the size pass read: the splats kept and the length they sort at.
    sorted_count: u32,
    sorted_len: u32,
    _pad: u32,
    // The draw's indirect arguments: vertex count, instance count, first
    // vertex, first instance.
    draw: array<u32, 4>,
};

@group(0) @binding(0) var<storage, read> splats: array<GaussianSplat>;
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

const BLOCK: u32 = 512u;
const HALF: u32 = 256u;
// The draw's limits (the splat render shader's).
const MIN_ALPHA: f32 = 0.0039215686; // 1 / 255
const MAX_ALPHA: f32 = 0.99;
// How generous the cull is (cluster::TOLERANCE).
const TOLERANCE: f32 = 1e-4;

var<workgroup> block_keys: array<u32, 512>;
var<workgroup> block_values: array<u32, 512>;
var<workgroup> scan: array<u32, 256>;
var<workgroup> cluster_in_view: u32;
var<workgroup> packed_base: u32;
var<workgroup> sort_len: u32;

// A float's bits as a u32 that orders the same way.
fn sortable(z: f32) -> u32 {
    let bits = bitcast<u32>(z);
    if (bits & 0x80000000u) != 0u {
        return ~bits;
    }
    return bits | 0x80000000u;
}

// Whether pair a sorts before pair b. Indices are unique, so this is a
// strict total order.
fn before(key_a: u32, value_a: u32, key_b: u32, value_b: u32) -> bool {
    return key_a < key_b || (key_a == key_b && value_a < value_b);
}

// The lower index of the t-th pair whose partners are j apart.
fn lower(t: u32, j: u32) -> u32 {
    return (t / j) * 2u * j + (t % j);
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
    var len = 0u;
    if kept > 0u {
        len = max(BLOCK, 1u << (32u - countLeadingZeros(kept - 1u)));
    }
    state.sorted_count = kept;
    state.sorted_len = len;
    state.draw[0] = 6u;
    state.draw[1] = kept;
    state.draw[2] = 0u;
    state.draw[3] = 0u;
    // Ready for the next sort.
    atomicStore(&state.kept, 0u);
}

@compute @workgroup_size(256)
fn pad(@builtin(global_invocation_id) id: vec3<u32>) {
    for (var i = 2u * id.x; i < 2u * id.x + 2u; i++) {
        if i >= state.sorted_count && i < state.sorted_len {
            keys[i] = 0xFFFFFFFFu;
            values[i] = i;
        }
    }
}

// The length the kept splats sort at, read once for the whole workgroup.
fn workgroup_sort_len(t: u32) -> u32 {
    if t == 0u {
        sort_len = state.sorted_len;
    }
    return workgroupUniformLoad(&sort_len);
}

fn load_block(base: u32, t: u32) {
    block_keys[t] = keys[base + t];
    block_values[t] = values[base + t];
    block_keys[t + HALF] = keys[base + t + HALF];
    block_values[t + HALF] = values[base + t + HALF];
}

fn store_block(base: u32, t: u32) {
    keys[base + t] = block_keys[t];
    values[base + t] = block_values[t];
    keys[base + t + HALF] = block_keys[t + HALF];
    values[base + t + HALF] = block_values[t + HALF];
}

// Compare-exchange steps j_start, j_start / 2, ..., 1 of the merge of size
// k, in workgroup memory.
fn block_steps(base: u32, t: u32, k: u32, j_start: u32) {
    var j = j_start;
    loop {
        if j == 0u {
            break;
        }
        let a = lower(t, j);
        let b = a + j;
        let ascending = ((base + a) & k) == 0u;
        let key_a = block_keys[a];
        let key_b = block_keys[b];
        let value_a = block_values[a];
        let value_b = block_values[b];
        if before(key_b, value_b, key_a, value_a) == ascending {
            block_keys[a] = key_b;
            block_keys[b] = key_a;
            block_values[a] = value_b;
            block_values[b] = value_a;
        }
        workgroupBarrier();
        j = j >> 1u;
    }
}

@compute @workgroup_size(256)
fn local_sort(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    let base = group.x * BLOCK;
    if base >= workgroup_sort_len(t) {
        return;
    }
    load_block(base, t);
    workgroupBarrier();
    var k = 2u;
    loop {
        if k > BLOCK {
            break;
        }
        block_steps(base, t, k, k >> 1u);
        k = k << 1u;
    }
    store_block(base, t);
}

@compute @workgroup_size(256)
fn global_step(@builtin(global_invocation_id) id: vec3<u32>) {
    let t = id.x;
    let len = state.sorted_len;
    if sort_step.k > len || t >= len / 2u {
        return;
    }
    let a = lower(t, sort_step.j);
    let b = a + sort_step.j;
    let ascending = (a & sort_step.k) == 0u;
    let key_a = keys[a];
    let key_b = keys[b];
    let value_a = values[a];
    let value_b = values[b];
    if before(key_b, value_b, key_a, value_a) == ascending {
        keys[a] = key_b;
        keys[b] = key_a;
        values[a] = value_b;
        values[b] = value_a;
    }
}

@compute @workgroup_size(256)
fn local_merge(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    let base = group.x * BLOCK;
    let len = workgroup_sort_len(t);
    if sort_step.k > len || base >= len {
        return;
    }
    load_block(base, t);
    workgroupBarrier();
    block_steps(base, t, sort_step.k, HALF);
    store_block(base, t);
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

    /// Run the passes on the CPU exactly as the kernels index them, over the
    /// whole of `keys`.
    fn emulate(keys: &mut [u32], values: &mut [u32], passes: &[SortPass]) {
        let n = keys.len() as u32;
        let exchange = |keys: &mut [u32], values: &mut [u32], a: u32, b: u32, k: u32| {
            let (a, b) = (a as usize, b as usize);
            let ascending = (a as u32 & k) == 0;
            let b_first = (keys[b], values[b]) < (keys[a], values[a]);
            if b_first == ascending {
                keys.swap(a, b);
                values.swap(a, b);
            }
        };
        let lower = |t: u32, j: u32| (t / j) * 2 * j + (t % j);
        let block_steps = |keys: &mut [u32], values: &mut [u32], k: u32, j_start: u32| {
            for base in (0..n).step_by(BLOCK as usize) {
                let mut j = j_start;
                while j > 0 {
                    for t in 0..WORKGROUP {
                        let a = base + lower(t, j);
                        exchange(keys, values, a, a + j, k);
                    }
                    j /= 2;
                }
            }
        };
        for pass in passes {
            match *pass {
                SortPass::Keys | SortPass::Size | SortPass::Pad => {}
                SortPass::LocalSort => {
                    let mut k = 2;
                    while k <= BLOCK {
                        block_steps(keys, values, k, k / 2);
                        k *= 2;
                    }
                }
                SortPass::GlobalStep { k, j } => {
                    for t in 0..n / 2 {
                        let a = lower(t, j);
                        exchange(keys, values, a, a + j, k);
                    }
                }
                SortPass::LocalMerge { k } => block_steps(keys, values, k, WORKGROUP),
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
    fn passes_cover_every_merge_step() {
        assert_eq!(padded_len(0), BLOCK);
        assert_eq!(padded_len(1), BLOCK);
        assert_eq!(padded_len(513), 1024);
        assert_eq!(
            sort_passes(512),
            [
                SortPass::Keys,
                SortPass::Size,
                SortPass::Pad,
                SortPass::LocalSort
            ]
        );
        assert_eq!(
            sort_passes(2048)[2..],
            [
                SortPass::Pad,
                SortPass::LocalSort,
                SortPass::GlobalStep { k: 1024, j: 512 },
                SortPass::LocalMerge { k: 1024 },
                SortPass::GlobalStep { k: 2048, j: 1024 },
                SortPass::GlobalStep { k: 2048, j: 512 },
                SortPass::LocalMerge { k: 2048 },
            ]
        );
        // 2^21 splats take 94 dispatches.
        assert_eq!(sort_passes(1 << 21).len(), 94);
        // A shorter sort runs a prefix of a longer one's passes.
        let all = sort_passes(1 << 16);
        for len in [512, 1024, 4096, 1 << 16] {
            let run: Vec<_> = all
                .iter()
                .copied()
                .filter(|pass| pass.min_len() <= len)
                .collect();
            assert_eq!(run, sort_passes(len), "len {len}");
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

    /// The passes, run as the kernels index them, give the reference order
    /// for counts below, at and across block and power-of-two boundaries;
    /// and so do the passes a shorter sort runs, over the kept splats packed
    /// at the front of a longer buffer, in any order.
    #[test]
    fn emulated_passes_sort_like_the_reference() {
        let view_model = view_model();
        for count in [1usize, 2, 7, 511, 512, 513, 1000, 1024, 4097] {
            let positions = random_positions(count, count as u64);
            let all = sort_passes(padded_len(count as u32));
            // Every splat, then every third one packed backwards.
            let every: Vec<u32> = (0..count as u32).collect();
            let some: Vec<u32> = (0..count as u32).rev().step_by(3).collect();
            for kept in [every, some] {
                let len = padded_len(kept.len() as u32);
                let key = |i: u32| {
                    sortable_depth(view_model.transform_point3(positions[i as usize].into()).z)
                };
                let mut keys: Vec<u32> = (0..len as usize)
                    .map(|n| kept.get(n).map_or(u32::MAX, |&i| key(i)))
                    .collect();
                let mut values: Vec<u32> = (0..len)
                    .map(|n| kept.get(n as usize).copied().unwrap_or(n))
                    .collect();
                let passes: Vec<_> = all
                    .iter()
                    .copied()
                    .filter(|pass| pass.min_len() <= len)
                    .collect();
                emulate(&mut keys, &mut values, &passes);
                let mut expected = kept.clone();
                expected.sort_by_key(|&i| (key(i), i));
                assert_eq!(
                    values[..kept.len()],
                    expected[..],
                    "count {count}, {} kept",
                    kept.len()
                );
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
