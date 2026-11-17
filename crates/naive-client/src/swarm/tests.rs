use super::*;
use crate::scene::GpuSwarmDef;
use bytemuck::Zeroable;
use glam::{Vec2, Vec3};

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
        separation: 3.0,
        arrive_radius: 2.0,
        field_size: 64.0,
        target_entity: None,
        seed: 7,
        initial: None,
        health: 10.0,
        attack_damage: 0.0,
        attack_range: 1.5,
        attack_interval: 1.0,
    }
}

/// Step parameters for `d`, its region centred on `origin`, heading for `target`.
fn params_for(d: &GpuSwarmDef, region: &Region, origin: Vec3, target: Vec2) -> SwarmParams {
    let attacks = d.attack_damage > 0.0;
    let mut params = SwarmParams::zeroed();
    params.target_pos = [target.x, origin.y, target.y, 1.0 / 60.0];
    params.motion = [d.speed, d.acceleration, origin.y, d.arrive_radius];
    params.base_color = [0.2, 0.6, 0.2, 1.0];
    params.material = [0.5, 0.0, 0.0, d.scale];
    params.region = [region.min.x, region.min.y, region.size, d.spacing];
    params.counts = [d.count, FIELD_CELLS, region.grid_cells, CELL_CAPACITY];
    params.steering = [
        d.separation,
        d.health,
        d.scale * 0.5,
        if attacks {
            d.attack_range * ATTACK_STOP
        } else {
            0.0
        },
    ];
    params.combat = [
        if attacks { d.attack_range } else { 0.0 },
        d.attack_interval,
        0.0,
        0.0,
    ];
    params
}

/// Region, solved flow field and step parameters for `def` around `origin`.
fn setup(
    d: &GpuSwarmDef,
    origin: Vec3,
    obstacles: &[Obstacle],
    target: Vec2,
) -> (Region, Vec<[f32; 4]>, SwarmParams) {
    let region = Region::around(origin, d);
    let blocked = blocked_cells(&region, obstacles, d.scale * 0.5);
    let field = solve_flow_field(&region, &blocked, target);
    let params = params_for(d, &region, origin, target);
    (region, field, params)
}

/// Put `volumes` and `spawns` into `params` for one step.
fn with_orders(
    mut params: SwarmParams,
    volumes: &[DamageVolume],
    spawns: &[SpawnRequest],
) -> SwarmParams {
    params.volumes[..volumes.len()].copy_from_slice(volumes);
    params.spawns[..spawns.len()].copy_from_slice(spawns);
    let requested = spawns.iter().map(|s| s.range[1]).sum();
    params.frame = [volumes.len() as u32, spawns.len() as u32, requested, 0];
    params
}

fn spawn_request(first: u32, count: u32, at: Vec3, radius: f32, sequence: u32) -> SpawnRequest {
    SpawnRequest {
        center: [at.x, at.y, at.z, radius],
        range: [first, count, sequence, 7],
    }
}

fn flat(m: &Member) -> Vec2 {
    Vec2::new(m.position[0], m.position[2])
}

/// A wall along x = 0 from z = -30 up to z = 10; the way round is above it.
fn wall() -> Obstacle {
    Obstacle {
        min: Vec2::new(-1.0, -30.0),
        max: Vec2::new(1.0, 10.0),
    }
}

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("no GPU adapter");
    pollster::block_on(adapter.request_device(&Default::default())).unwrap()
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer) -> Vec<u8> {
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
    bytes
}

/// Run the compute shaders on a real GPU and compare them with
/// `step_swarm`, damage and attacks included. CI has no GPU, so run it
/// by hand: `cargo test -p naive-client --lib -- --ignored gpu_step`
#[test]
#[ignore = "needs a GPU"]
fn gpu_step_matches_the_cpu_reference() {
    let (device, queue) = gpu();
    let pipelines = SwarmPipelines::new(&device);

    // Sparse enough that no grid cell overflows, so both sides see the
    // same neighbours; only the order inside a cell differs.
    let d = GpuSwarmDef {
        spawn_radius: 10.0,
        attack_damage: 3.0,
        attack_range: 4.0,
        attack_interval: 0.1,
        ..def(300)
    };
    let origin = Vec3::new(0.0, 0.25, 0.0);
    let target = Vec2::new(-10.0, 3.0);
    let (region, field, params) = setup(&d, origin, &[wall()], target);
    let start: Vec<Member> = spawn_members(&d, origin)
        .into_iter()
        .map(|mut m| {
            m.position[0] -= 14.0;
            m
        })
        .collect();
    let buffers = SwarmBuffers::new(
        &device,
        &pipelines,
        &start,
        region.grid_cells,
        wgpu::BufferUsages::COPY_SRC,
    );
    queue.write_buffer(&buffers.field, 0, bytemuck::cast_slice(&field));

    // A sphere and a line every few steps; members near the target attack it.
    let steps = 20;
    let mut parity = 0;
    let mut cpu = start.clone();
    let mut cpu_totals = [0u32; 2];
    let mut cpu_alive = 0;
    for step in 0..steps {
        let volumes = if step % 4 == 0 {
            vec![
                DamageVolume::sphere(Vec3::new(-14.0 + step as f32 * 0.3, 0.25, 2.0), 3.0, 4.0),
                DamageVolume::line(
                    Vec3::new(-25.0, 0.5, -4.0),
                    Vec3::new(-5.0, 0.5, 6.0),
                    0.5,
                    6.0,
                ),
            ]
        } else {
            Vec::new()
        };
        let p = with_orders(params, &volumes, &[]);
        queue.write_buffer(&buffers.params, 0, bytemuck::bytes_of(&p));
        let mut encoder = device.create_command_encoder(&Default::default());
        encode_steps(&mut encoder, &pipelines, &[(&buffers, parity)], None);
        queue.submit(std::iter::once(encoder.finish()));
        parity = 1 - parity;
        let (next, stats) = step_swarm(&cpu, &p, &field);
        cpu = next;
        cpu_totals[0] += stats[STAT_KILLS];
        cpu_totals[1] += stats[STAT_ATTACKS];
        cpu_alive = stats[STAT_ALIVE];
    }

    let gpu_members: Vec<Member> =
        bytemuck::cast_slice(&read_back(&device, &queue, &buffers.members[parity])).to_vec();
    let gpu_draws: Vec<crate::renderer::DrawUniforms> =
        bytemuck::cast_slice(&read_back(&device, &queue, &buffers.draws)).to_vec();
    let gpu_stats: Vec<u32> =
        bytemuck::cast_slice(&read_back(&device, &queue, &buffers.stats)).to_vec();

    let mut moved = 0;
    for (i, (gpu, cpu)) in gpu_members.iter().zip(&cpu).enumerate() {
        for k in 0..4 {
            assert!(
                (gpu.position[k] - cpu.position[k]).abs() < 1e-3,
                "member {i} position {gpu:?} vs {cpu:?}"
            );
            assert!(
                (gpu.velocity[k] - cpu.velocity[k]).abs() < 1e-3,
                "member {i} velocity {gpu:?} vs {cpu:?}"
            );
            assert!(
                (gpu.life[k] - cpu.life[k]).abs() < 1e-3,
                "member {i} life {gpu:?} vs {cpu:?}"
            );
        }
        // From rest, a third of a second is a few tens of centimetres.
        if flat(gpu).distance(flat(&start[i])) > 0.2 {
            moved += 1;
        }
        let model = glam::Mat4::from_cols_array_2d(&gpu_draws[i].model_matrix);
        if !gpu.alive() {
            assert_eq!(
                model.x_axis,
                glam::Vec4::ZERO,
                "dead member {i} is still drawn"
            );
            continue;
        }
        let expected = glam::Mat4::from_scale_rotation_translation(
            Vec3::splat(d.scale),
            glam::Quat::from_rotation_y(gpu.position[3]),
            Vec3::new(gpu.position[0], gpu.position[1], gpu.position[2]),
        );
        assert!(
            model.abs_diff_eq(expected, 1e-4),
            "member {i} model matrix\n{model}\nvs\n{expected}"
        );
    }
    assert!(moved > 150, "only {moved} members moved");
    assert!(
        cpu_totals[0] > 20 && cpu_totals[1] > 0,
        "the test should kill and attack: {cpu_totals:?}"
    );
    assert_eq!(
        [gpu_stats[0], gpu_stats[1], gpu_stats[2]],
        [cpu_totals[0], cpu_totals[1], cpu_alive]
    );
}

/// Spawning on the GPU: which dead slot takes which spawn number varies
/// from run to run, so compare the positions as sets.
#[test]
#[ignore = "needs a GPU"]
fn gpu_spawn_matches_the_cpu_reference() {
    let (device, queue) = gpu();
    let pipelines = SwarmPipelines::new(&device);
    let d = GpuSwarmDef {
        initial: Some(20),
        ..def(500)
    };
    let origin = Vec3::new(0.0, 0.25, 0.0);
    let (region, field, params) = setup(&d, origin, &[], Vec2::new(20.0, 0.0));
    let start = spawn_members(&d, origin);
    let buffers = SwarmBuffers::new(
        &device,
        &pipelines,
        &start,
        region.grid_cells,
        wgpu::BufferUsages::COPY_SRC,
    );
    queue.write_buffer(&buffers.field, 0, bytemuck::cast_slice(&field));
    let p = with_orders(
        params,
        &[],
        &[
            spawn_request(0, 200, Vec3::new(-10.0, 0.0, 5.0), 6.0, 0),
            spawn_request(200, 100, Vec3::new(12.0, 0.0, -8.0), 3.0, 200),
        ],
    );
    queue.write_buffer(&buffers.params, 0, bytemuck::bytes_of(&p));
    let mut encoder = device.create_command_encoder(&Default::default());
    encode_steps(&mut encoder, &pipelines, &[(&buffers, 0)], None);
    queue.submit(std::iter::once(encoder.finish()));

    let gpu_members: Vec<Member> =
        bytemuck::cast_slice(&read_back(&device, &queue, &buffers.members[1])).to_vec();
    let gpu_stats: Vec<u32> =
        bytemuck::cast_slice(&read_back(&device, &queue, &buffers.stats)).to_vec();
    let (cpu, cpu_stats) = step_swarm(&start, &p, &field);
    assert_eq!(gpu_stats[STAT_ALIVE], 320);
    assert_eq!(gpu_stats[STAT_ALIVE], cpu_stats[STAT_ALIVE]);
    assert_eq!(gpu_stats[STAT_CLAIMS], cpu_stats[STAT_CLAIMS]);

    let sorted = |members: &[Member]| {
        let mut v: Vec<[f32; 4]> = members
            .iter()
            .filter(|m| m.alive())
            .map(|m| m.position)
            .collect();
        v.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[2].total_cmp(&b[2])));
        v
    };
    let (g, c) = (sorted(&gpu_members), sorted(&cpu));
    assert_eq!(g.len(), c.len());
    for (a, b) in g.iter().zip(&c) {
        for k in 0..4 {
            assert!((a[k] - b[k]).abs() < 1e-3, "spawned at {a:?} vs {b:?}");
        }
    }
}
