//! GPU compute pipelines, buffers, rendering, and asynchronous statistics.

use super::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::components::{GpuSwarm, Hidden, MaterialOverride, MeshHandle, Transform};
use crate::gpu_timer::GpuTimer;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::PhysicsWorld;
use crate::renderer::{DrawUniformPool, GpuState, DRAW_UNIFORM_SIZE};
use crate::swarm_api::SwarmBus;
use crate::texture_cache::TextureHandle;
use crate::world::SceneWorld;

// --- GPU side ---

/// The compute pipelines and the bind group layout they share.
pub struct SwarmPipelines {
    pub layout: wgpu::BindGroupLayout,
    bin: wgpu::ComputePipeline,
    steer: wgpu::ComputePipeline,
}

impl SwarmPipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let write = wgpu::BufferBindingType::Storage { read_only: false };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Swarm step layout"),
            entries: &[
                buffer(0, wgpu::BufferBindingType::Uniform),
                buffer(1, read),
                buffer(2, write),
                buffer(3, write),
                buffer(4, read),
                buffer(5, write),
                buffer(6, write),
                buffer(7, write),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Swarm step shader"),
            source: wgpu::ShaderSource::Wgsl(SWARM_COMPUTE_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Swarm step pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point, label| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            bin: pipeline("bin_main", "Swarm bin pipeline"),
            steer: pipeline("steer_main", "Swarm steer pipeline"),
            layout,
        }
    }
}

/// The GPU buffers of one swarm.
pub struct SwarmBuffers {
    pub params: wgpu::Buffer,
    /// Double-buffered members: a step reads one and writes the other.
    pub members: [wgpu::Buffer; 2],
    pub draws: wgpu::Buffer,
    pub field: wgpu::Buffer,
    /// The step's four counters (see `STAT_KILLS` and the rest).
    pub stats: wgpu::Buffer,
    cell_counts: wgpu::Buffer,
    _cell_members: wgpu::Buffer,
    /// `bind_groups[k]` reads `members[k]` and writes `members[1 - k]`.
    bind_groups: [wgpu::BindGroup; 2],
    count: u32,
}

impl SwarmBuffers {
    /// `extra` usage is added to the member and draw buffers (tests read them back).
    pub fn new(
        device: &wgpu::Device,
        pipelines: &SwarmPipelines,
        members: &[Member],
        grid_cells: u32,
        extra: wgpu::BufferUsages,
    ) -> Self {
        let count = members.len() as u32;
        let storage = wgpu::BufferUsages::STORAGE | extra;
        let initial = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Swarm members A"),
            contents: bytemuck::cast_slice(members),
            usage: storage,
        });
        let other = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Swarm members B"),
            size: initial.size(),
            usage: storage,
            mapped_at_creation: false,
        });
        let empty = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let draws = empty(
            "Swarm draw data",
            DRAW_UNIFORM_SIZE * u64::from(count),
            storage,
        );
        let params = empty(
            "Swarm params",
            std::mem::size_of::<SwarmParams>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let field = empty(
            "Swarm flow field",
            16 * u64::from(FIELD_CELLS * FIELD_CELLS),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let stats = empty(
            "Swarm stats",
            16,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        );
        let grid = u64::from(grid_cells * grid_cells);
        let cell_counts = empty(
            "Swarm grid counts",
            4 * grid,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let cell_members = empty(
            "Swarm grid members",
            4 * grid * u64::from(CELL_CAPACITY),
            wgpu::BufferUsages::STORAGE,
        );

        let members = [initial, other];
        fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        let bind_group = |k: usize| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Swarm step bind group"),
                layout: &pipelines.layout,
                entries: &[
                    entry(0, &params),
                    entry(1, &members[k]),
                    entry(2, &members[1 - k]),
                    entry(3, &draws),
                    entry(4, &field),
                    entry(5, &cell_counts),
                    entry(6, &cell_members),
                    entry(7, &stats),
                ],
            })
        };
        let bind_groups = [bind_group(0), bind_group(1)];
        Self {
            params,
            members,
            draws,
            field,
            stats,
            cell_counts,
            _cell_members: cell_members,
            bind_groups,
            count,
        }
    }
}

/// Record one step for each `(swarm, parity)`: clear its grid and this
/// step's counters, bin, steer. All in one compute pass, timed with
/// `timestamps` if given.
pub fn encode_steps(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &SwarmPipelines,
    steps: &[(&SwarmBuffers, usize)],
    timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
) {
    for (buffers, _) in steps {
        encoder.clear_buffer(&buffers.cell_counts, 0, None);
        // Living members and spawn claims are per step; kills and attacks
        // are running totals.
        encoder.clear_buffer(&buffers.stats, 8, Some(8));
    }
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("Swarm step"),
        timestamp_writes: timestamps,
    });
    for (buffers, parity) in steps {
        let groups = buffers.count.div_ceil(WORKGROUP_SIZE);
        pass.set_bind_group(0, &buffers.bind_groups[*parity], &[]);
        pass.set_pipeline(&pipelines.bin);
        pass.dispatch_workgroups(groups, 1, 1);
        pass.set_pipeline(&pipelines.steer);
        pass.dispatch_workgroups(groups, 1, 1);
    }
}

/// What the render passes need to draw one swarm.
pub struct SwarmDraw<'a> {
    pub mesh: MeshHandle,
    pub albedo_texture: Option<TextureHandle>,
    /// The swarm's draw data, bound where the G-buffer and shadow shaders
    /// expect the per-entity array.
    pub draws: &'a wgpu::BindGroup,
    pub count: u32,
}

const READBACK_FREE: u8 = 0;
const READBACK_PENDING: u8 = 1;
const READBACK_READY: u8 = 2;
/// Copies of the counters in flight per swarm. A frame with none free skips
/// its copy; the running totals in the next one cover it.
const READBACK_SLOTS: usize = 3;

/// A mappable copy of a swarm's counters from one step.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    step: u64,
    target_id: Option<String>,
}

/// GPU state of one swarm.
struct SwarmGpu {
    core: SwarmCore,
    mesh: MeshHandle,
    buffers: SwarmBuffers,
    /// Which member buffer the next step reads.
    parity: usize,
    draw_bind_group: wgpu::BindGroup,
    readbacks: Vec<Readback>,
    albedo_texture: Option<TextureHandle>,
    hidden: bool,
}

impl SwarmGpu {
    /// Report every copy of the counters that has arrived, oldest first.
    fn collect_readbacks(&mut self, bus: &mut SwarmBus) {
        let mut ready: Vec<usize> = (0..self.readbacks.len())
            .filter(|&k| self.readbacks[k].state.load(Ordering::Acquire) == READBACK_READY)
            .collect();
        ready.sort_by_key(|&k| self.readbacks[k].step);
        for k in ready {
            let readback = &mut self.readbacks[k];
            let stats: [u32; 4] = {
                let Ok(view) = readback.buffer.slice(..).get_mapped_range() else {
                    tracing::error!("Swarm readback buffer was not mapped");
                    continue;
                };
                let words: &[u32] = bytemuck::cast_slice(&view);
                [words[0], words[1], words[2], words[3]]
            };
            readback.buffer.unmap();
            readback.state.store(READBACK_FREE, Ordering::Release);
            let target_id = readback.target_id.take();
            self.core.finish(bus, readback.step, stats, target_id);
        }
    }
}

/// All swarms in the scene.
#[derive(Default)]
pub struct SwarmSystem {
    pipelines: Option<SwarmPipelines>,
    swarms: HashMap<hecs::Entity, SwarmGpu>,
}

impl SwarmSystem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every swarm, for a new scene.
    pub fn clear(&mut self) {
        self.swarms.clear();
    }

    /// Number of members across all swarms.
    pub fn member_count(&self) -> u32 {
        self.swarms.values().map(|s| s.buffers.count).sum()
    }

    /// Bring the GPU state in line with the world's `GpuSwarm` entities,
    /// report counters that have come back, then advance every swarm by
    /// `dt` seconds and rewrite its draw data. A `dt` of zero (while paused)
    /// moves nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        gpu: &GpuState,
        scene_world: &SceneWorld,
        physics: Option<&PhysicsWorld>,
        mesh_cache: &MeshCache,
        material_cache: &MaterialCache,
        draw_pool: &DrawUniformPool,
        bus: &mut SwarmBus,
        timer: Option<&GpuTimer>,
        dt: f32,
    ) {
        let world = &scene_world.world;

        // Drop swarms whose entity is gone or no longer has the component.
        self.swarms
            .retain(|&entity, _| world.get::<&GpuSwarm>(entity).is_ok());

        let _ = gpu.device.poll(wgpu::PollType::Poll);
        for state in self.swarms.values_mut() {
            state.collect_readbacks(bus);
        }

        let boxes = physics.map(static_boxes).unwrap_or_default();
        let mut active: Vec<(hecs::Entity, Option<String>)> = Vec::new();
        for (entity, (swarm, transform, mat_override, hidden)) in world
            .query::<(
                hecs::Entity,
                (
                    &GpuSwarm,
                    &Transform,
                    Option<&MaterialOverride>,
                    Option<&Hidden>,
                ),
            )>()
            .iter()
        {
            if swarm.def.count == 0 {
                self.swarms.remove(&entity);
                continue;
            }
            let origin = transform.world_matrix.w_axis.truncate();
            let stale = self
                .swarms
                .get(&entity)
                .is_some_and(|s| *s.core.definition() != swarm.def || s.mesh != swarm.mesh_handle);
            if stale || !self.swarms.contains_key(&entity) {
                let id = swarm_id(scene_world, entity);
                let state = self.create_swarm(gpu, id, swarm, origin, draw_pool, bus);
                self.swarms.insert(entity, state);
            }

            let material = material_cache.get(swarm.material_handle);
            let gpu_mesh = mesh_cache.get(swarm.mesh_handle);
            let has_texture =
                material.albedo_texture.is_some() || gpu_mesh.texture_bind_group.is_some();
            let base_color = mat_override
                .and_then(|o| o.base_color)
                .map(|c| [c[0], c[1], c[2], material.uniform.base_color[3]])
                .unwrap_or(material.uniform.base_color);
            let emission = mat_override
                .and_then(|o| o.emission)
                .map(|e| [e[0], e[1], e[2], 0.0])
                .unwrap_or(material.uniform.emission);
            let roughness = mat_override
                .and_then(|o| o.roughness)
                .unwrap_or(material.uniform.roughness);
            let metallic = mat_override
                .and_then(|o| o.metallic)
                .unwrap_or(material.uniform.metallic);

            let state = self
                .swarms
                .get_mut(&entity)
                .expect("swarm state was just created");
            state.albedo_texture = material.albedo_texture;
            state.hidden = hidden.is_some();

            let mut plan = state.core.plan(scene_world, &boxes, bus, origin, dt);
            if plan.field_changed {
                gpu.queue.write_buffer(
                    &state.buffers.field,
                    0,
                    bytemuck::cast_slice(state.core.field()),
                );
            }
            plan.params.base_color = base_color;
            plan.params.material = [
                roughness,
                metallic,
                if has_texture { 1.0 } else { 0.0 },
                swarm.def.scale,
            ];
            plan.params.emission = emission;
            gpu.queue
                .write_buffer(&state.buffers.params, 0, bytemuck::bytes_of(&plan.params));
            active.push((entity, plan.target_id));
        }

        let Some(pipelines) = &self.pipelines else {
            return;
        };
        if active.is_empty() {
            return;
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Swarm step"),
            });
        {
            let steps: Vec<(&SwarmBuffers, usize)> = active
                .iter()
                .map(|(e, _)| {
                    let state = &self.swarms[e];
                    (&state.buffers, state.parity)
                })
                .collect();
            let timestamps = timer.and_then(|t| t.compute_pass(crate::gpu_timer::SWARMS));
            encode_steps(&mut encoder, pipelines, &steps, timestamps);
        }
        // Copy each swarm's counters out, where a slot is free.
        let mut copied: Vec<(hecs::Entity, usize)> = Vec::new();
        for (entity, target_id) in active.iter_mut() {
            let state = self.swarms.get_mut(entity).expect("active swarm");
            state.parity = 1 - state.parity;
            let step = state.core.step();
            if let Some(k) = state
                .readbacks
                .iter()
                .position(|r| r.state.load(Ordering::Acquire) == READBACK_FREE)
            {
                let readback = &mut state.readbacks[k];
                encoder.copy_buffer_to_buffer(&state.buffers.stats, 0, &readback.buffer, 0, 16);
                readback.state.store(READBACK_PENDING, Ordering::Release);
                readback.step = step;
                readback.target_id = target_id.take();
                copied.push((*entity, k));
            }
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        for (entity, k) in copied {
            let readback = &self.swarms[&entity].readbacks[k];
            let state = readback.state.clone();
            readback
                .buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let next = if result.is_ok() {
                        READBACK_READY
                    } else {
                        READBACK_FREE
                    };
                    state.store(next, Ordering::Release);
                });
        }
    }

    /// The swarms to draw this frame.
    pub fn draws(&self) -> Vec<SwarmDraw<'_>> {
        self.swarms
            .values()
            .filter(|s| !s.hidden)
            .map(|s| SwarmDraw {
                mesh: s.mesh,
                albedo_texture: s.albedo_texture,
                draws: &s.draw_bind_group,
                count: s.buffers.count,
            })
            .collect()
    }

    fn create_swarm(
        &mut self,
        gpu: &GpuState,
        id: String,
        swarm: &GpuSwarm,
        origin: Vec3,
        draw_pool: &DrawUniformPool,
        bus: &mut SwarmBus,
    ) -> SwarmGpu {
        let device = &gpu.device;
        let pipelines = self
            .pipelines
            .get_or_insert_with(|| SwarmPipelines::new(device));

        // Each member's draw data is one 256-byte entry of a single storage
        // binding, so the device's binding size limits the swarm.
        let limit = device.limits().max_storage_buffer_binding_size / DRAW_UNIFORM_SIZE;
        let limit = u32::try_from(limit).unwrap_or(u32::MAX);
        let mut def = swarm.def.clone();
        if def.count > limit {
            tracing::warn!(
                "Swarm of {} members is over this GPU's limit of {}; using {}",
                def.count,
                limit,
                limit
            );
            def.count = limit;
        }
        let members = spawn_members(&def, origin);
        let core = SwarmCore::new(id, &swarm.def, def.count, origin, bus);
        let buffers = SwarmBuffers::new(
            device,
            pipelines,
            &members,
            core.region().grid_cells,
            wgpu::BufferUsages::empty(),
        );
        let draw_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Swarm draw bind group"),
            layout: &draw_pool.storage_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffers.draws.as_entire_binding(),
            }],
        });
        let readbacks = (0..READBACK_SLOTS)
            .map(|_| Readback {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Swarm stats readback"),
                    size: 16,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(READBACK_FREE)),
                step: 0,
                target_id: None,
            })
            .collect();

        tracing::info!(
            "Swarm '{}' created: {} members, {}x{} neighbour grid",
            core.id(),
            buffers.count,
            core.region().grid_cells,
            core.region().grid_cells
        );
        SwarmGpu {
            core,
            mesh: swarm.mesh_handle,
            buffers,
            parity: 0,
            draw_bind_group,
            readbacks,
            albedo_texture: None,
            hidden: false,
        }
    }
}
