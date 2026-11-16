//! GPU swarms (Tier 4): many members simulated by a compute shader and drawn
//! with the deferred pipeline's instanced shaders.
//!
//! A swarm is one ECS entity with a `GpuSwarm` component. Its members exist
//! only in GPU buffers, owned here and keyed by that entity. Every frame the
//! compute shader moves each member toward its slot around the target and
//! writes the member's draw data (the same 256-byte `DrawUniforms` the
//! G-buffer and shadow shaders read), so drawing a swarm is one instanced draw
//! per pass. Nothing is read back.
//!
//! `step_member` is a CPU copy of the shader's maths, for tests.

use std::collections::HashMap;

use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::components::{GpuSwarm, Hidden, MaterialOverride, MeshHandle, Transform};
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::renderer::{DrawUniformPool, GpuState, DRAW_UNIFORM_SIZE};
use crate::scene::GpuSwarmDef;
use crate::texture_cache::TextureHandle;
use crate::world::SceneWorld;

const WORKGROUP_SIZE: u32 = 64;

/// One member, as the compute shader stores it.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Member {
    /// xyz: position. w: heading, the yaw in radians it faces.
    pub position: [f32; 4],
    /// xyz: velocity. w: unused.
    pub velocity: [f32; 4],
    /// xyz: offset of this member's slot from the target. w: its speed factor.
    pub slot: [f32; 4],
}

/// Per-swarm values for one step. Matches `SwarmParams` in the shader.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SwarmParams {
    /// xyz: target position. w: time step in seconds.
    pub target_pos: [f32; 4],
    /// Top speed, acceleration, height of the plane members move on, arrive radius.
    pub motion: [f32; 4],
    pub base_color: [f32; 4],
    /// Roughness, metallic, has_texture, member scale.
    pub material: [f32; 4],
    pub emission: [f32; 4],
    /// x: member count.
    pub count: [u32; 4],
}

/// The compute shader. One invocation per member.
pub const SWARM_COMPUTE_WGSL: &str = r#"
struct Member {
    position: vec4<f32>,
    velocity: vec4<f32>,
    slot: vec4<f32>,
};

struct SwarmParams {
    target_pos: vec4<f32>,
    motion: vec4<f32>,
    base_color: vec4<f32>,
    material: vec4<f32>,
    emission: vec4<f32>,
    count: vec4<u32>,
};

// Same layout as the CPU DrawUniforms and the G-buffer and shadow shaders.
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

@group(0) @binding(0) var<uniform> params: SwarmParams;
@group(0) @binding(1) var<storage, read_write> members: array<Member>;
@group(0) @binding(2) var<storage, read_write> draws: array<DrawUniforms>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.count.x) {
        return;
    }
    let m = members[i];
    let dt = params.target_pos.w;
    let max_speed = params.motion.x;
    let acceleration = params.motion.y;
    let height = params.motion.z;
    let arrive_radius = params.motion.w;

    // Steer toward this member's slot around the target, slowing on arrival.
    let goal = params.target_pos.xyz + m.slot.xyz;
    let to_goal = vec3<f32>(goal.x - m.position.x, 0.0, goal.z - m.position.z);
    let dist = length(to_goal);
    var desired = vec3<f32>(0.0, 0.0, 0.0);
    if (dist > 0.0001) {
        let speed = max_speed * m.slot.w * min(1.0, dist / arrive_radius);
        desired = to_goal * (speed / dist);
    }
    let blend = min(1.0, acceleration * dt);
    let velocity = m.velocity.xyz + (desired - m.velocity.xyz) * blend;
    let position = vec3<f32>(m.position.x + velocity.x * dt, height, m.position.z + velocity.z * dt);

    // Face the direction of travel; keep the last heading when nearly still.
    var heading = m.position.w;
    if (length(velocity) > 0.05) {
        heading = atan2(velocity.x, velocity.z);
    }

    members[i] = Member(vec4<f32>(position, heading), vec4<f32>(velocity, 0.0), m.slot);

    // Model matrix: translate * rotate about Y by heading * uniform scale.
    let scale = params.material.w;
    let c = cos(heading);
    let s = sin(heading);
    let x_axis = vec4<f32>(c, 0.0, -s, 0.0);
    let y_axis = vec4<f32>(0.0, 1.0, 0.0, 0.0);
    let z_axis = vec4<f32>(s, 0.0, c, 0.0);
    draws[i].model_matrix = mat4x4<f32>(x_axis * scale, y_axis * scale, z_axis * scale, vec4<f32>(position, 1.0));
    // Normals only need the rotation; the shaders normalize them.
    draws[i].normal_matrix = mat4x4<f32>(x_axis, y_axis, z_axis, vec4<f32>(0.0, 0.0, 0.0, 1.0));
    draws[i].base_color = params.base_color;
    draws[i].roughness = params.material.x;
    draws[i].metallic = params.material.y;
    draws[i].has_texture = params.material.z;
    draws[i].has_skin = 0.0;
    draws[i].emission = params.emission;
}
"#;

/// CPU copy of one step of `cs_main`'s movement, for tests.
pub fn step_member(m: &Member, p: &SwarmParams) -> Member {
    let dt = p.target_pos[3];
    let [max_speed, acceleration, height, arrive_radius] = p.motion;

    let goal = Vec3::from_slice(&p.target_pos[..3]) + Vec3::from_slice(&m.slot[..3]);
    let to_goal = Vec3::new(goal.x - m.position[0], 0.0, goal.z - m.position[2]);
    let dist = to_goal.length();
    let mut desired = Vec3::ZERO;
    if dist > 0.0001 {
        let speed = max_speed * m.slot[3] * (dist / arrive_radius).min(1.0);
        desired = to_goal * (speed / dist);
    }
    let blend = (acceleration * dt).min(1.0);
    let old_velocity = Vec3::from_slice(&m.velocity[..3]);
    let velocity = old_velocity + (desired - old_velocity) * blend;
    let position = Vec3::new(m.position[0] + velocity.x * dt, height, m.position[2] + velocity.z * dt);

    let mut heading = m.position[3];
    if velocity.length() > 0.05 {
        heading = velocity.x.atan2(velocity.z);
    }

    Member {
        position: [position.x, position.y, position.z, heading],
        velocity: [velocity.x, velocity.y, velocity.z, 0.0],
        slot: m.slot,
    }
}

/// Initial members: spread over a disc of `spawn_radius` around `origin`,
/// each assigned a slot on an even spiral around the target.
pub fn spawn_members(def: &GpuSwarmDef, origin: Vec3) -> Vec<Member> {
    // Golden-angle spiral: point k at radius c * sqrt(k) covers area pi * c^2
    // per point, so c = spacing / sqrt(pi) puts neighbours about `spacing` apart.
    const GOLDEN_ANGLE: f32 = 2.399_963_2;
    let c = def.spacing / std::f32::consts::PI.sqrt();

    (0..def.count)
        .map(|k| {
            let u = unit_random(def.seed, k, 0);
            let v = unit_random(def.seed, k, 1);
            let w = unit_random(def.seed, k, 2);
            let r = def.spawn_radius * u.sqrt();
            let angle = std::f32::consts::TAU * v;
            let slot_r = c * (k as f32 + 0.5).sqrt();
            let slot_angle = k as f32 * GOLDEN_ANGLE;
            Member {
                position: [origin.x + r * angle.cos(), origin.y, origin.z + r * angle.sin(), 0.0],
                velocity: [0.0; 4],
                slot: [
                    slot_r * slot_angle.cos(),
                    0.0,
                    slot_r * slot_angle.sin(),
                    0.85 + 0.3 * w,
                ],
            }
        })
        .collect()
}

/// A deterministic value in [0, 1) from a seed, an index and a stream.
fn unit_random(seed: u32, index: u32, stream: u32) -> f32 {
    // SplitMix64 finalizer over the three inputs.
    let mut z = (u64::from(seed) << 40) ^ (u64::from(stream) << 32) ^ u64::from(index);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
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

/// GPU state of one swarm.
struct SwarmGpu {
    def: GpuSwarmDef,
    mesh: MeshHandle,
    params_buffer: wgpu::Buffer,
    compute_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
    count: u32,
    albedo_texture: Option<TextureHandle>,
    hidden: bool,
    // Kept alive for the bind groups.
    _members: wgpu::Buffer,
    _draws: wgpu::Buffer,
}

/// All swarms in the scene.
#[derive(Default)]
pub struct SwarmSystem {
    pipeline: Option<(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    swarms: HashMap<hecs::Entity, SwarmGpu>,
}

impl SwarmSystem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of members across all swarms.
    pub fn member_count(&self) -> u32 {
        self.swarms.values().map(|s| s.count).sum()
    }

    /// Bring the GPU state in line with the world's `GpuSwarm` entities,
    /// then advance every swarm by `dt` seconds and rewrite its draw data.
    /// A `dt` of zero (while paused) only rewrites the draw data.
    pub fn update(
        &mut self,
        gpu: &GpuState,
        scene_world: &SceneWorld,
        mesh_cache: &MeshCache,
        material_cache: &MaterialCache,
        draw_pool: &DrawUniformPool,
        dt: f32,
    ) {
        let world = &scene_world.world;

        // Drop swarms whose entity is gone or no longer has the component.
        self.swarms.retain(|&entity, _| world.get::<&GpuSwarm>(entity).is_ok());

        let mut steps: Vec<(hecs::Entity, SwarmParams)> = Vec::new();
        for (entity, (swarm, transform, mat_override, hidden)) in world
            .query::<(&GpuSwarm, &Transform, Option<&MaterialOverride>, Option<&Hidden>)>()
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
                .is_some_and(|s| s.def != swarm.def || s.mesh != swarm.mesh_handle);
            if stale || !self.swarms.contains_key(&entity) {
                let state = self.create_swarm(gpu, swarm, origin, draw_pool);
                self.swarms.insert(entity, state);
            }

            let target = swarm
                .def
                .target_entity
                .as_ref()
                .and_then(|id| scene_world.entity_registry.get(id))
                .and_then(|&target| world.get::<&Transform>(target).ok())
                .map(|t| t.world_matrix.w_axis.truncate())
                .unwrap_or(origin);

            let material = material_cache.get(swarm.material_handle);
            let gpu_mesh = mesh_cache.get(swarm.mesh_handle);
            let has_texture = material.albedo_texture.is_some() || gpu_mesh.texture_bind_group.is_some();
            let base_color = mat_override
                .and_then(|o| o.base_color)
                .map(|c| [c[0], c[1], c[2], material.uniform.base_color[3]])
                .unwrap_or(material.uniform.base_color);
            let emission = mat_override
                .and_then(|o| o.emission)
                .map(|e| [e[0], e[1], e[2], 0.0])
                .unwrap_or(material.uniform.emission);
            let roughness = mat_override.and_then(|o| o.roughness).unwrap_or(material.uniform.roughness);
            let metallic = mat_override.and_then(|o| o.metallic).unwrap_or(material.uniform.metallic);

            let state = self.swarms.get_mut(&entity).expect("swarm state was just created");
            state.albedo_texture = material.albedo_texture;
            state.hidden = hidden.is_some();

            let def = &swarm.def;
            steps.push((
                entity,
                SwarmParams {
                    target_pos: [target.x, target.y, target.z, dt],
                    motion: [def.speed, def.acceleration, origin.y, def.arrive_radius.max(0.001)],
                    base_color,
                    material: [roughness, metallic, if has_texture { 1.0 } else { 0.0 }, def.scale],
                    emission,
                    count: [state.count, 0, 0, 0],
                },
            ));
        }

        if steps.is_empty() {
            return;
        }
        let Some((_, pipeline)) = &self.pipeline else {
            return;
        };

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Swarm step"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Swarm step"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            for (entity, params) in &steps {
                let state = &self.swarms[entity];
                gpu.queue.write_buffer(&state.params_buffer, 0, bytemuck::bytes_of(params));
                pass.set_bind_group(0, &state.compute_bind_group, &[]);
                pass.dispatch_workgroups(state.count.div_ceil(WORKGROUP_SIZE), 1, 1);
            }
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
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
                count: s.count,
            })
            .collect()
    }

    fn create_swarm(
        &mut self,
        gpu: &GpuState,
        swarm: &GpuSwarm,
        origin: Vec3,
        draw_pool: &DrawUniformPool,
    ) -> SwarmGpu {
        let device = &gpu.device;
        let (layout, _) = self.pipeline.get_or_insert_with(|| create_pipeline(device));

        // Each member's draw data is one 256-byte entry of a single storage
        // binding, so the device's binding size limits the swarm.
        let limit = u64::from(device.limits().max_storage_buffer_binding_size) / DRAW_UNIFORM_SIZE;
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
        let count = members.len() as u32;
        let member_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Swarm members"),
            contents: bytemuck::cast_slice(&members),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let draw_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Swarm draw data"),
            size: DRAW_UNIFORM_SIZE * u64::from(count),
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Swarm params"),
            size: std::mem::size_of::<SwarmParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let compute_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Swarm step bind group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: member_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: draw_buffer.as_entire_binding(),
                },
            ],
        });
        let draw_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Swarm draw bind group"),
            layout: &draw_pool.storage_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: draw_buffer.as_entire_binding(),
            }],
        });

        tracing::info!("Swarm created: {} members", count);
        SwarmGpu {
            def: swarm.def.clone(),
            mesh: swarm.mesh_handle,
            params_buffer,
            compute_bind_group,
            draw_bind_group,
            count,
            albedo_texture: None,
            hidden: false,
            _members: member_buffer,
            _draws: draw_buffer,
        }
    }
}

fn create_pipeline(device: &wgpu::Device) -> (wgpu::BindGroupLayout, wgpu::ComputePipeline) {
    let storage = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Swarm step layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<SwarmParams>() as u64),
                },
                count: None,
            },
            storage(1),
            storage(2),
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Swarm step shader"),
        source: wgpu::ShaderSource::Wgsl(SWARM_COMPUTE_WGSL.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Swarm step pipeline layout"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Swarm step pipeline"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("cs_main"),
        compilation_options: Default::default(),
        cache: None,
    });
    (layout, pipeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(count: u32) -> GpuSwarmDef {
        GpuSwarmDef {
            count,
            mesh: "procedural:cube".into(),
            material: "procedural:default".into(),
            scale: 0.5,
            speed: 4.0,
            acceleration: 3.0,
            spawn_radius: 20.0,
            spacing: 1.0,
            arrive_radius: 2.0,
            target_entity: None,
            seed: 7,
        }
    }

    fn params(target: Vec3, dt: f32) -> SwarmParams {
        SwarmParams {
            target_pos: [target.x, target.y, target.z, dt],
            motion: [4.0, 3.0, 0.5, 2.0],
            base_color: [1.0; 4],
            material: [0.5, 0.0, 0.0, 0.5],
            emission: [0.0; 4],
            count: [1, 0, 0, 0],
        }
    }

    fn goal(m: &Member, target: Vec3) -> Vec3 {
        target + Vec3::from_slice(&m.slot[..3])
    }

    fn flat_distance(m: &Member, to: Vec3) -> f32 {
        Vec3::new(m.position[0] - to.x, 0.0, m.position[2] - to.z).length()
    }

    #[test]
    fn members_reach_their_slots_around_the_target() {
        let target = Vec3::new(30.0, 0.0, -10.0);
        for start in spawn_members(&def(50), Vec3::ZERO) {
            let mut m = start;
            for _ in 0..60 * 30 {
                m = step_member(&m, &params(target, 1.0 / 60.0));
            }
            assert!(
                flat_distance(&m, goal(&m, target)) < 0.05,
                "member ended {} from its slot",
                flat_distance(&m, goal(&m, target))
            );
        }
    }

    #[test]
    fn members_never_exceed_their_top_speed_and_stay_on_the_plane() {
        let target = Vec3::new(-40.0, 0.0, 25.0);
        for start in spawn_members(&def(50), Vec3::new(0.0, 0.5, 0.0)) {
            let mut m = start;
            let top_speed = 4.0 * m.slot[3];
            for _ in 0..600 {
                m = step_member(&m, &params(target, 1.0 / 60.0));
                let speed = Vec3::from_slice(&m.velocity[..3]).length();
                assert!(speed <= top_speed + 1e-4, "speed {speed} above {top_speed}");
                assert_eq!(m.position[1], 0.5);
            }
        }
    }

    #[test]
    fn members_face_the_way_they_move() {
        let mut m = Member {
            position: [0.0, 0.5, 0.0, 0.0],
            velocity: [0.0; 4],
            slot: [0.0, 0.0, 0.0, 1.0],
        };
        // Target straight along +X: heading pi/2 (yaw from +Z toward +X).
        for _ in 0..30 {
            m = step_member(&m, &params(Vec3::new(50.0, 0.0, 0.0), 1.0 / 60.0));
        }
        assert!((m.position[3] - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
    }

    #[test]
    fn a_zero_time_step_moves_nothing() {
        // Spawned at the height `params` gives the plane.
        for start in spawn_members(&def(20), Vec3::new(0.0, 0.5, 0.0)) {
            let m = step_member(&start, &params(Vec3::new(10.0, 0.0, 10.0), 0.0));
            assert_eq!(&m.position[..3], &start.position[..3]);
        }
    }

    #[test]
    fn spawn_is_deterministic_and_inside_the_disc() {
        let d = def(1000);
        let origin = Vec3::new(5.0, 1.0, -5.0);
        let a = spawn_members(&d, origin);
        assert_eq!(a, spawn_members(&d, origin));
        assert_ne!(a, spawn_members(&GpuSwarmDef { seed: 8, ..d.clone() }, origin));
        for m in &a {
            assert!(flat_distance(m, origin) <= d.spawn_radius + 1e-3);
            assert_eq!(m.position[1], 1.0);
            assert!((0.85..=1.15).contains(&m.slot[3]));
        }
    }

    #[test]
    fn slots_are_spread_about_spacing_apart() {
        let d = def(500);
        let members = spawn_members(&d, Vec3::ZERO);
        let slots: Vec<Vec3> = members.iter().map(|m| Vec3::from_slice(&m.slot[..3])).collect();
        // Nearest-neighbour distance of every slot: no stacking, no big holes.
        for (i, a) in slots.iter().enumerate() {
            let nearest = slots
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, b)| a.distance(*b))
                .fold(f32::MAX, f32::min);
            assert!(
                (0.4 * d.spacing..1.5 * d.spacing).contains(&nearest),
                "slot {i} nearest neighbour at {nearest}"
            );
        }
    }

    /// Run the compute shader on a real GPU and compare it with `step_member`.
    /// CI has no GPU, so run it by hand:
    /// `cargo test -p naive-client --lib -- --ignored gpu_step`
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_step_matches_the_cpu_reference() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("no GPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
        let (layout, pipeline) = create_pipeline(&device);

        let origin = Vec3::new(3.0, 0.5, -2.0);
        let start = spawn_members(&def(300), origin);
        let count = start.len() as u32;
        let members = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&start),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let draws = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: DRAW_UNIFORM_SIZE * u64::from(count),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<SwarmParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: members.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: draws.as_entire_binding() },
            ],
        });

        let target = Vec3::new(-20.0, 0.0, 15.0);
        let mut p = params(target, 1.0 / 60.0);
        p.count[0] = count;
        let steps = 90;
        for _ in 0..steps {
            queue.write_buffer(&params_buffer, 0, bytemuck::bytes_of(&p));
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups(count.div_ceil(WORKGROUP_SIZE), 1, 1);
            }
            queue.submit(std::iter::once(encoder.finish()));
        }

        let read = |buffer: &wgpu::Buffer| -> Vec<u8> {
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: buffer.size(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
            queue.submit(std::iter::once(encoder.finish()));
            staging.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
            device.poll(wgpu::Maintain::Wait);
            let bytes = staging.slice(..).get_mapped_range().to_vec();
            bytes
        };
        let gpu_members: Vec<Member> = bytemuck::cast_slice(&read(&members)).to_vec();
        let gpu_draws: Vec<crate::renderer::DrawUniforms> = bytemuck::cast_slice(&read(&draws)).to_vec();

        for (i, first) in start.iter().enumerate() {
            let mut cpu = *first;
            for _ in 0..steps {
                cpu = step_member(&cpu, &p);
            }
            let gpu = gpu_members[i];
            for k in 0..4 {
                assert!((gpu.position[k] - cpu.position[k]).abs() < 1e-3, "member {i} position {gpu:?} vs {cpu:?}");
                assert!((gpu.velocity[k] - cpu.velocity[k]).abs() < 1e-3, "member {i} velocity {gpu:?} vs {cpu:?}");
            }

            // The draw matrix places a scaled, turned member where it is.
            let model = glam::Mat4::from_cols_array_2d(&gpu_draws[i].model_matrix);
            let expected = glam::Mat4::from_scale_rotation_translation(
                Vec3::splat(p.material[3]),
                glam::Quat::from_rotation_y(gpu.position[3]),
                Vec3::from_slice(&gpu.position[..3]),
            );
            assert!(model.abs_diff_eq(expected, 1e-4), "member {i} model matrix\n{model}\nvs\n{expected}");
            assert_eq!(gpu_draws[i].base_color, p.base_color);
        }
    }

    #[test]
    fn swarm_params_match_the_shader_layout() {
        assert_eq!(std::mem::size_of::<Member>(), 48);
        assert_eq!(std::mem::size_of::<SwarmParams>(), 96);
    }
}
