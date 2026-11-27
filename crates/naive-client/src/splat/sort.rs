//! Back-to-front splat sorting on the GPU.
//!
//! Each splat entity gets its own sort state ([`SplatInstance`]): splats are
//! shared by asset path, but the order they draw in depends on the entity's
//! transform as well as the camera. A compute pass writes one key per splat
//! (its view-space depth, as a `u32` that sorts like the float) and sorts
//! (key, index) pairs with a bitonic network: every block of [`BLOCK`] pairs
//! is sorted in workgroup memory, then larger merges take one dispatch per
//! step until the step fits a block again. The draw reads the first `count`
//! sorted indices; padding sorts after every real splat.
//!
//! Ties in depth are broken by splat index, so the order is deterministic and
//! the same as a stable sort by view-space depth.

use glam::Mat4;
use wgpu::util::DeviceExt;

use super::GpuSplat;

/// Pairs one workgroup sorts in workgroup memory (two per invocation).
pub const BLOCK: u32 = 512;
const WORKGROUP: u32 = BLOCK / 2;

/// Per-entity values both the sort and the draw read.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceUniforms {
    pub model: [[f32; 4]; 4],
    pub view_model: [[f32; 4]; 4],
    pub count: u32,
    pub padded: u32,
    pub _pad: [u32; 2],
}

/// One dispatch of the sort.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SortPass {
    /// Write each splat's depth key and its index.
    Keys,
    /// Sort every block, alternating direction as the network needs.
    LocalSort,
    /// A compare-exchange step whose partners are `j` apart (`j >= BLOCK`).
    GlobalStep { k: u32, j: u32 },
    /// The rest of the merge of size `k`, inside each block.
    LocalMerge { k: u32 },
}

/// The sort's length for `count` splats: a power of two, at least a block.
pub fn padded_len(count: u32) -> u32 {
    count.max(1).next_power_of_two().max(BLOCK)
}

/// The dispatches that sort `padded` pairs, in order.
pub fn sort_passes(padded: u32) -> Vec<SortPass> {
    debug_assert!(padded.is_power_of_two() && padded >= BLOCK);
    let mut passes = vec![SortPass::Keys, SortPass::LocalSort];
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
                SortPass::Keys | SortPass::LocalSort => (0, 0),
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
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splat instance"),
            size: std::mem::size_of::<InstanceUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Splat sort"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: splat.splat_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: keys.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: sorted_indices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &steps,
                        offset: 0,
                        size: wgpu::BufferSize::new(16),
                    }),
                },
            ],
        });
        SplatInstance {
            handle,
            count: splat.splat_count,
            padded,
            passes,
            _keys: keys,
            sorted_indices,
            uniforms,
            _steps: steps,
            bind_group,
            written: None,
        }
    }

    /// Record the dispatches that sort `instance`.
    pub fn encode(&self, pass: &mut wgpu::ComputePass<'_>, instance: &SplatInstance) {
        let blocks = instance.padded / BLOCK;
        for (n, step) in instance.passes.iter().enumerate() {
            let (pipeline, groups) = match step {
                SortPass::Keys => (&self.keys, instance.padded / WORKGROUP),
                SortPass::LocalSort => (&self.local_sort, blocks),
                SortPass::GlobalStep { .. } => (&self.global_step, blocks),
                SortPass::LocalMerge { .. } => (&self.local_merge, blocks),
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &instance.bind_group, &[n as u32 * self.step_stride]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
    }
}

/// One entity's sort state and draw uniforms.
pub struct SplatInstance {
    /// The splat asset this state was made for.
    pub handle: crate::components::SplatHandle,
    pub count: u32,
    padded: u32,
    passes: Vec<SortPass>,
    _keys: wgpu::Buffer,
    /// Splat indices, farthest first. The first `count` are real splats.
    pub sorted_indices: wgpu::Buffer,
    /// [`InstanceUniforms`]: the draw reads the model matrix from here.
    pub uniforms: wgpu::Buffer,
    _steps: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// What the uniforms hold, and so what the indices are sorted for.
    written: Option<InstanceUniforms>,
}

impl SplatInstance {
    /// Point the instance at `model` seen through `view`. Returns whether its
    /// order must be sorted again: not when neither has changed.
    pub fn update(&mut self, queue: &wgpu::Queue, model: Mat4, view: Mat4) -> bool {
        let uniforms = InstanceUniforms {
            model: model.to_cols_array_2d(),
            view_model: (view * model).to_cols_array_2d(),
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

/// Keys, the bitonic network over (key, index) pairs, and its block passes.
pub const SORT_WGSL: &str = r#"
struct InstanceUniforms {
    model: mat4x4<f32>,
    view_model: mat4x4<f32>,
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

@group(0) @binding(0) var<storage, read> splats: array<GaussianSplat>;
@group(0) @binding(1) var<storage, read_write> keys: array<u32>;
@group(0) @binding(2) var<storage, read_write> values: array<u32>;
@group(0) @binding(3) var<uniform> instance: InstanceUniforms;
@group(0) @binding(4) var<uniform> sort_step: SortStep;

const BLOCK: u32 = 512u;
const HALF: u32 = 256u;

var<workgroup> block_keys: array<u32, 512>;
var<workgroup> block_values: array<u32, 512>;

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

@compute @workgroup_size(256)
fn compute_keys(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= instance.padded {
        return;
    }
    if i < instance.count {
        // Farthest first: view space looks down -z.
        let view_position = instance.view_model * vec4<f32>(splats[i].position, 1.0);
        keys[i] = sortable(view_position.z);
    } else {
        keys[i] = 0xFFFFFFFFu;
    }
    values[i] = i;
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
    if t >= instance.padded / 2u {
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
    load_block(base, t);
    workgroupBarrier();
    block_steps(base, t, sort_step.k, HALF);
    store_block(base, t);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The CPU order the GPU must match: stable by view-space depth.
    pub(crate) fn reference_order(positions: &[[f32; 3]], view_model: Mat4) -> Vec<u32> {
        let mut order: Vec<u32> = (0..positions.len() as u32).collect();
        order.sort_by_key(|&i| {
            let z = view_model
                .transform_point3(glam::Vec3::from(positions[i as usize]))
                .z;
            (sortable_depth(z), i)
        });
        order
    }

    /// Run the passes on the CPU exactly as the kernels index them.
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
                SortPass::Keys => {}
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
        let view = glam::camera::rh::view::look_at_mat4(
            glam::Vec3::new(3.0, 4.0, 12.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        );
        let model = Mat4::from_scale_rotation_translation(
            glam::Vec3::new(1.0, 2.0, 0.5),
            glam::Quat::from_rotation_y(0.7),
            glam::Vec3::new(-2.0, 0.0, 1.0),
        );
        view * model
    }

    #[test]
    fn passes_cover_every_merge_step() {
        assert_eq!(padded_len(0), BLOCK);
        assert_eq!(padded_len(1), BLOCK);
        assert_eq!(padded_len(513), 1024);
        assert_eq!(sort_passes(512), [SortPass::Keys, SortPass::LocalSort]);
        assert_eq!(
            sort_passes(2048),
            [
                SortPass::Keys,
                SortPass::LocalSort,
                SortPass::GlobalStep { k: 1024, j: 512 },
                SortPass::LocalMerge { k: 1024 },
                SortPass::GlobalStep { k: 2048, j: 1024 },
                SortPass::GlobalStep { k: 2048, j: 512 },
                SortPass::LocalMerge { k: 2048 },
            ]
        );
        // 2^21 splats take 92 dispatches.
        assert_eq!(sort_passes(1 << 21).len(), 92);
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
    /// for counts below, at and across block and power-of-two boundaries.
    #[test]
    fn emulated_passes_sort_like_the_reference() {
        let view_model = view_model();
        for count in [1usize, 2, 7, 511, 512, 513, 1000, 1024, 4097] {
            let positions = random_positions(count, count as u64);
            let padded = padded_len(count as u32);
            let mut keys: Vec<u32> = (0..padded as usize)
                .map(|i| match positions.get(i) {
                    Some(p) => sortable_depth(view_model.transform_point3((*p).into()).z),
                    None => u32::MAX,
                })
                .collect();
            let mut values: Vec<u32> = (0..padded).collect();
            emulate(&mut keys, &mut values, &sort_passes(padded));
            assert_eq!(
                values[..count],
                reference_order(&positions, view_model)[..],
                "count {count}"
            );
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

    fn sort_on_gpu(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sorter: &SplatSorter,
        positions: &[[f32; 3]],
        model: Mat4,
        view: Mat4,
    ) -> Vec<u32> {
        let splat = super::super::upload_splat(
            device,
            "test",
            naive_sim::assets::SplatData {
                splats: positions
                    .iter()
                    .map(|&position| naive_sim::assets::SplatPoint {
                        position,
                        opacity: 1.0,
                        scale: [1.0; 3],
                        rotation: [1.0, 0.0, 0.0, 0.0],
                        sh_dc: [1.0; 3],
                    })
                    .collect(),
            },
        );
        let mut instance = sorter.instance(device, &splat, crate::components::SplatHandle(0));
        assert!(instance.update(queue, model, view));
        assert!(!instance.update(queue, model, view), "unchanged");
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            sorter.encode(&mut pass, &instance);
        }
        queue.submit(std::iter::once(encoder.finish()));
        let mut sorted = read_u32s(device, queue, &instance.sorted_indices);
        sorted.truncate(positions.len());
        sorted
    }

    /// The compute shaders on a real GPU sort like the CPU.
    /// `cargo test -p naive-client --lib -- --ignored gpu_splat_sort`
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_splat_sort_matches_the_cpu_reference() {
        let (device, queue) = gpu();
        let sorter = SplatSorter::new(&device);
        for count in [1usize, 7, 512, 513, 4097, 70_001, 1 << 20] {
            // Exact: integer depths moved by a translation are computed
            // without rounding on both sides, and most depths tie.
            let positions: Vec<[f32; 3]> = random_positions(count, count as u64)
                .iter()
                .map(|p| [p[0], p[1], (p[2] * 10.0).round()])
                .collect();
            let model = Mat4::from_translation(glam::Vec3::new(0.0, 0.0, -50.0));
            let sorted = sort_on_gpu(&device, &queue, &sorter, &positions, model, Mat4::IDENTITY);
            let reference = reference_order(&positions, model);
            let first = sorted.iter().zip(&reference).position(|(a, b)| a != b);
            assert_eq!(first, None, "count {count}: first difference");

            // Any transform: a permutation, farthest first. Depths the GPU
            // rounds differently may swap neighbours that are this close.
            let view = glam::camera::rh::view::look_at_mat4(
                glam::Vec3::new(3.0, 4.0, 12.0),
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            );
            let model = Mat4::from_scale_rotation_translation(
                glam::Vec3::new(1.0, 2.0, 0.5),
                glam::Quat::from_rotation_y(0.7),
                glam::Vec3::new(-2.0, 0.0, 1.0),
            );
            let positions = random_positions(count, 9 + count as u64);
            let sorted = sort_on_gpu(&device, &queue, &sorter, &positions, model, view);
            let mut seen = vec![false; count];
            sorted.iter().for_each(|&i| seen[i as usize] = true);
            assert!(seen.iter().all(|&s| s), "count {count}: not a permutation");
            let depth = |i: u32| {
                (view * model)
                    .transform_point3(glam::Vec3::from(positions[i as usize]))
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
}
