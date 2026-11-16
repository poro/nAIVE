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

fn cell_centre(region: &Region, x: i32, z: i32) -> Vec2 {
    region.min + (Vec2::new(x as f32, z as f32) + 0.5) * region.field_cell_size()
}

/// A wall along x = 0 from z = -30 up to z = 10; the way round is above it.
fn wall() -> Obstacle {
    Obstacle {
        min: Vec2::new(-1.0, -30.0),
        max: Vec2::new(1.0, 10.0),
    }
}

#[test]
fn flow_field_points_toward_the_target_in_the_open() {
    let region = Region::around(Vec3::ZERO, &def(1));
    let target = Vec2::new(10.0, -5.0);
    let field = solve_flow_field(
        &region,
        &vec![false; (FIELD_CELLS * FIELD_CELLS) as usize],
        target,
    );
    let n = FIELD_CELLS as i32;
    for z in 0..n {
        for x in 0..n {
            let cell = field[(z * n + x) as usize];
            let to_target = target - cell_centre(&region, x, z);
            if to_target.length() < 2.0 * region.field_cell_size() {
                continue;
            }
            let dir = Vec2::new(cell[0], cell[1]);
            assert!(
                dir.dot(to_target.normalize()) > 0.7,
                "cell ({x}, {z}) points {dir}"
            );
            // Paths on the grid are never shorter than the straight line.
            assert!(cell[2] >= to_target.length() - region.field_cell_size());
        }
    }
}

#[test]
fn flow_field_routes_around_a_wall_and_never_into_it() {
    let d = def(1);
    let region = Region::around(Vec3::ZERO, &d);
    let blocked = blocked_cells(&region, &[wall()], d.scale * 0.5);
    let target = Vec2::new(10.0, 0.0);
    let field = solve_flow_field(&region, &blocked, target);
    let n = FIELD_CELLS as i32;

    // Behind the wall, the distance is the way round, not the straight line.
    let (sx, sz) = region.field_cell(Vec2::new(-10.0, 0.0));
    assert!(field[(sz * n + sx) as usize][2] > 30.0);

    // Following the arrows from there reaches the target without touching the wall.
    let (tx, tz) = region.field_cell(target);
    let (mut x, mut z) = (sx, sz);
    for _ in 0..400 {
        if (x, z) == (tx, tz) {
            break;
        }
        let cell = field[(z * n + x) as usize];
        x += cell[0].round() as i32;
        z += cell[1].round() as i32;
        assert!(
            !blocked[(z * n + x) as usize],
            "path entered the wall at ({x}, {z})"
        );
    }
    assert_eq!((x, z), (tx, tz), "the arrows never reached the target");
}

#[test]
fn walled_in_cells_cannot_reach_the_target() {
    let d = def(1);
    let region = Region::around(Vec3::ZERO, &d);
    let ring = [
        Obstacle {
            min: Vec2::new(-6.0, -6.0),
            max: Vec2::new(6.0, -5.0),
        },
        Obstacle {
            min: Vec2::new(-6.0, 5.0),
            max: Vec2::new(6.0, 6.0),
        },
        Obstacle {
            min: Vec2::new(-6.0, -6.0),
            max: Vec2::new(-5.0, 6.0),
        },
        Obstacle {
            min: Vec2::new(5.0, -6.0),
            max: Vec2::new(6.0, 6.0),
        },
    ];
    let blocked = blocked_cells(&region, &ring, 0.0);
    let field = solve_flow_field(&region, &blocked, Vec2::new(20.0, 20.0));
    let n = FIELD_CELLS as i32;
    let (x, z) = region.field_cell(Vec2::ZERO);
    assert_eq!(field[(z * n + x) as usize], [0.0, 0.0, -1.0, 0.0]);
    let (x, z) = region.field_cell(Vec2::new(5.5, 0.0));
    assert_eq!(field[(z * n + x) as usize][3], 1.0);
}

#[test]
fn blocked_cells_cover_the_box_and_its_margin() {
    // 64 m region at the origin: 0.5 m cells, starting at -32.
    let region = Region::around(Vec3::ZERO, &def(1));
    let o = Obstacle {
        min: Vec2::new(0.0, 0.0),
        max: Vec2::new(1.0, 1.0),
    };
    let blocked = blocked_cells(&region, &[o], 0.25);
    // x and z from -0.25 to 1.25: cells 63, 64, 65, 66 on each axis.
    assert_eq!(blocked.iter().filter(|&&b| b).count(), 16);
    let n = FIELD_CELLS as usize;
    assert!(blocked[63 * n + 63] && blocked[66 * n + 66]);
    assert!(!blocked[64 * n + 62] && !blocked[64 * n + 67]);
}

#[test]
fn only_boxes_crossing_the_plane_are_obstacles() {
    let boxes = [
        (Vec3::new(-50.0, -1.0, -50.0), Vec3::new(50.0, 0.0, 50.0)), // floor
        (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 3.0, 1.0)),        // wall
        (Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 6.0, 1.0)),        // overhead beam
    ];
    let obstacles = plane_obstacles(&boxes, 0.25, 0.25);
    assert_eq!(
        obstacles,
        vec![Obstacle {
            min: Vec2::ZERO,
            max: Vec2::ONE
        }]
    );
}

#[test]
fn members_spread_out_instead_of_stacking() {
    let d = GpuSwarmDef {
        spawn_radius: 0.3,
        ..def(150)
    };
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::ZERO);
    let mut members = spawn_members(&d, Vec3::ZERO);
    for _ in 0..600 {
        members = step_swarm(&members, &params, &field).0;
    }
    // Each member's distance to its nearest neighbour.
    let mut nearest: Vec<f32> = members
        .iter()
        .enumerate()
        .map(|(i, a)| {
            assert!(flat(a).length() < 15.0, "a member drifted to {}", flat(a));
            members
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, b)| flat(a).distance(flat(b)))
                .fold(f32::MAX, f32::min)
        })
        .collect();
    nearest.sort_by(f32::total_cmp);
    let (min, p10, median) = (
        nearest[0],
        nearest[nearest.len() / 10],
        nearest[nearest.len() / 2],
    );
    assert!(
        min > 0.4 * d.spacing && median > 0.6 * d.spacing,
        "nearest-neighbour distances: min {min}, 10th percentile {p10}, median {median}"
    );
}

#[test]
fn members_path_around_a_wall_without_entering_it() {
    let d = GpuSwarmDef {
        spawn_radius: 2.0,
        ..def(40)
    };
    let origin = Vec3::new(-12.0, 0.25, 0.0);
    let target = Vec2::new(12.0, 0.0);
    // The region is centred on the origin of the test, not the spawn point.
    let (_, field, params) = setup(&d, Vec3::new(0.0, 0.25, 0.0), &[wall()], target);

    let mut members = spawn_members(&d, origin);
    for _ in 0..60 * 30 {
        members = step_swarm(&members, &params, &field).0;
        for m in &members {
            assert!(
                !blocked_at(&params, &field, flat(m)),
                "a member is inside the wall at {}",
                flat(m)
            );
        }
    }
    let arrived = members
        .iter()
        .filter(|m| flat(m).distance(target) < 6.0)
        .count();
    assert!(arrived >= 36, "only {arrived} of 40 reached the target");
}

#[test]
fn members_never_exceed_their_top_speed_and_stay_on_the_plane() {
    let d = def(60);
    let origin = Vec3::new(0.0, 0.5, 0.0);
    let (_, field, params) = setup(&d, origin, &[wall()], Vec2::new(20.0, 15.0));
    let mut members = spawn_members(&d, origin);
    for _ in 0..600 {
        members = step_swarm(&members, &params, &field).0;
        for m in &members {
            let top = d.speed * m.velocity[3];
            let speed = Vec2::new(m.velocity[0], m.velocity[2]).length();
            assert!(speed <= top + 1e-4, "speed {speed} above {top}");
            assert_eq!(m.position[1], 0.5);
        }
    }
}

#[test]
fn members_face_the_way_they_move() {
    let d = def(1);
    // On a cell centre (0.5 m cells from -32), level with the target, so
    // the field gives a straight line rather than a blend with a diagonal.
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::new(25.0, 0.25));
    let mut members = vec![Member {
        position: [0.0, 0.0, 0.25, 0.0],
        velocity: [0.0, 0.0, 0.0, 1.0],
        life: [10.0, 0.0, 0.0, 0.0],
    }];
    for _ in 0..30 {
        members = step_swarm(&members, &params, &field).0;
    }
    // Moving along +X: heading pi/2 (yaw from +Z toward +X).
    assert!((members[0].position[3] - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
}

#[test]
fn a_zero_time_step_moves_nothing() {
    let d = def(30);
    let (_, field, mut params) = setup(&d, Vec3::ZERO, &[], Vec2::new(10.0, 10.0));
    params.target_pos[3] = 0.0;
    let start = spawn_members(&d, Vec3::ZERO);
    for (a, b) in start.iter().zip(step_swarm(&start, &params, &field).0) {
        assert_eq!(flat(a), flat(&b));
    }
}

#[test]
fn spawn_is_deterministic_and_inside_the_disc() {
    let d = def(1000);
    let origin = Vec3::new(5.0, 1.0, -5.0);
    let a = spawn_members(&d, origin);
    assert_eq!(a, spawn_members(&d, origin));
    assert_ne!(
        a,
        spawn_members(
            &GpuSwarmDef {
                seed: 8,
                ..d.clone()
            },
            origin
        )
    );
    for m in &a {
        assert!(flat(m).distance(Vec2::new(5.0, -5.0)) <= d.spawn_radius + 1e-3);
        assert_eq!(m.position[1], 1.0);
        assert!((0.85..=1.15).contains(&m.velocity[3]));
        assert_eq!(m.life[0], d.health);
    }
    // A spawn disc wider than the region is kept inside it.
    let wide = GpuSwarmDef {
        spawn_radius: 500.0,
        ..def(200)
    };
    for m in spawn_members(&wide, Vec3::ZERO) {
        assert!(flat(&m).length() < wide.field_size * 0.5);
    }
    // Only `initial` members start alive.
    let pool = spawn_members(
        &GpuSwarmDef {
            initial: Some(30),
            ..def(100)
        },
        origin,
    );
    assert_eq!(pool.iter().filter(|m| m.alive()).count(), 30);
}

#[test]
fn a_damage_sphere_kills_what_it_covers_and_nothing_else() {
    let d = GpuSwarmDef {
        spawn_radius: 10.0,
        ..def(200)
    };
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::ZERO);
    let start = spawn_members(&d, Vec3::ZERO);
    let centre = Vec3::new(3.0, 0.0, -2.0);
    let radius = 4.0;
    let params = with_orders(params, &[DamageVolume::sphere(centre, radius, 50.0)], &[]);
    let (after, stats) = step_swarm(&start, &params, &field);

    let reach = radius + d.scale * 0.5;
    let inside = start
        .iter()
        .filter(|m| flat(m).distance(Vec2::new(centre.x, centre.z)) <= reach)
        .count();
    assert!(
        inside > 10,
        "the test needs members inside the sphere, got {inside}"
    );
    for (before, now) in start.iter().zip(&after) {
        let covered = flat(before).distance(Vec2::new(centre.x, centre.z)) <= reach;
        assert_eq!(
            now.alive(),
            !covered,
            "member at {} covered: {covered}",
            flat(before)
        );
    }
    assert_eq!(stats[STAT_KILLS] as usize, inside);
    assert_eq!(stats[STAT_ALIVE] as usize, 200 - inside);
}

#[test]
fn damage_below_a_members_health_wounds_it_and_makes_it_glow() {
    let d = def(1);
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::new(20.0, 0.0));
    let start = vec![Member {
        position: [0.0; 4],
        velocity: [0.0, 0.0, 0.0, 1.0],
        life: [10.0, 0.0, 0.0, 0.0],
    }];
    let hit = with_orders(params, &[DamageVolume::sphere(Vec3::ZERO, 1.0, 6.0)], &[]);

    let (once, stats) = step_swarm(&start, &hit, &field);
    assert_eq!(stats[STAT_KILLS], 0);
    assert_eq!(once[0].life[0], 4.0);
    assert_eq!(once[0].life[2], HIT_FLASH);

    // The glow fades with time, and a second hit kills.
    let (calm, _) = step_swarm(&once, &params, &field);
    assert!(calm[0].life[2] < HIT_FLASH);
    let hit_again = with_orders(
        params,
        &[DamageVolume::sphere(
            Vec3::new(calm[0].position[0], 0.0, calm[0].position[2]),
            1.0,
            6.0,
        )],
        &[],
    );
    let (dead, stats) = step_swarm(&calm, &hit_again, &field);
    assert!(!dead[0].alive());
    assert_eq!(stats[STAT_KILLS], 1);
}

#[test]
fn a_line_hurts_every_member_along_it_and_stops_at_its_end() {
    let d = def(5);
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::new(0.0, 20.0));
    // A row of members along x, one metre apart.
    let start: Vec<Member> = (0..5)
        .map(|k| Member {
            position: [k as f32, 0.0, 0.0, 0.0],
            velocity: [0.0, 0.0, 0.0, 1.0],
            life: [10.0, 0.0, 0.0, 0.0],
        })
        .collect();
    // From behind the first to between the third and fourth, at waist height.
    let line = DamageVolume::line(
        Vec3::new(-2.0, 0.5, 0.0),
        Vec3::new(2.5, 0.5, 0.0),
        0.3,
        20.0,
    );
    let (after, stats) = step_swarm(&start, &with_orders(params, &[line], &[]), &field);
    let alive: Vec<bool> = after.iter().map(Member::alive).collect();
    assert_eq!(alive, [false, false, false, true, true]);
    assert_eq!(stats[STAT_KILLS], 3);
}

#[test]
fn spawning_brings_dead_members_back_inside_the_disc() {
    let d = GpuSwarmDef {
        initial: Some(0),
        ..def(100)
    };
    let (_, field, params) = setup(&d, Vec3::new(0.0, 0.25, 0.0), &[], Vec2::ZERO);
    let start = spawn_members(&d, Vec3::new(0.0, 0.25, 0.0));
    assert!(start.iter().all(|m| !m.alive()));

    let at = Vec3::new(10.0, 0.0, -5.0);
    let order = with_orders(
        params,
        &[],
        &[
            spawn_request(0, 30, at, 4.0, 0),
            spawn_request(30, 20, -at, 2.0, 30),
        ],
    );
    let (after, stats) = step_swarm(&start, &order, &field);
    assert_eq!(stats[STAT_ALIVE], 50);
    let alive: Vec<&Member> = after.iter().filter(|m| m.alive()).collect();
    assert_eq!(alive.len(), 50);
    let near = |m: &&Member, c: Vec3, r: f32| flat(m).distance(Vec2::new(c.x, c.z)) <= r + 1e-4;
    assert_eq!(alive.iter().filter(|m| near(m, at, 4.0)).count(), 30);
    assert_eq!(alive.iter().filter(|m| near(m, -at, 2.0)).count(), 20);
    for m in &alive {
        assert_eq!(
            m.position[1], 0.25,
            "spawned members stand on the swarm's plane"
        );
        assert_eq!(m.life[0], d.health);
    }

    // Asking for more than the pool holds brings back what is left.
    let more = with_orders(params, &[], &[spawn_request(0, 80, at, 4.0, 50)]);
    let (full, stats) = step_swarm(&after, &more, &field);
    assert_eq!(stats[STAT_ALIVE], 100);
    assert!(full.iter().all(Member::alive));

    // A later wave lands somewhere else: its spawn numbers differ.
    let first = with_orders(params, &[], &[spawn_request(0, 10, at, 4.0, 0)]);
    let later = with_orders(params, &[], &[spawn_request(0, 10, at, 4.0, 10)]);
    assert_ne!(
        step_swarm(&start, &first, &field).0,
        step_swarm(&start, &later, &field).0
    );
}

#[test]
fn dead_members_neither_push_nor_move() {
    let d = def(2);
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::new(0.0, 10.0));
    let corpse = Member {
        position: [0.3, 0.0, 0.0, 0.0],
        velocity: [0.0, 0.0, 0.0, 1.0],
        life: [0.0; 4],
    };
    let living = Member {
        position: [0.0, 0.0, 0.0, 0.0],
        velocity: [0.0, 0.0, 0.0, 1.0],
        life: [10.0, 0.0, 0.0, 0.0],
    };
    let (after, stats) = step_swarm(&[living, corpse], &params, &field);
    assert_eq!(after[1], corpse);
    assert_eq!(stats[STAT_ALIVE], 1);
    // The living member moves as if it were alone.
    let (alone, _) = step_swarm(&[living], &params, &field);
    assert_eq!(after[0], alone[0], "pushed by a dead member");
    // A living member in the corpse's place would push it.
    let (pushed, _) = step_swarm(
        &[
            living,
            Member {
                life: living.life,
                ..corpse
            },
        ],
        &params,
        &field,
    );
    assert_ne!(pushed[0], alone[0]);
}

#[test]
fn attackers_stop_short_of_the_target_and_hit_on_their_interval() {
    let d = GpuSwarmDef {
        spawn_radius: 6.0,
        attack_damage: 5.0,
        attack_range: 2.0,
        attack_interval: 0.5,
        ..def(12)
    };
    let target = Vec2::new(0.0, 0.0);
    let (_, field, params) = setup(&d, Vec3::ZERO, &[], target);
    let mut members = spawn_members(&d, Vec3::new(8.0, 0.0, 8.0));
    let mut attacks = 0;
    for _ in 0..60 * 10 {
        let (next, stats) = step_swarm(&members, &params, &field);
        members = next;
        attacks += stats[STAT_ATTACKS];
    }
    // Nobody walks into the target.
    for m in &members {
        assert!(
            flat(m).distance(target) > 0.6 * d.attack_range,
            "a member stands on the target at {}",
            flat(m)
        );
    }
    let in_reach = members
        .iter()
        .filter(|m| flat(m).distance(target) <= d.attack_range)
        .count();
    assert!(in_reach >= 4, "only {in_reach} members reached the target");
    // In reach for most of ten seconds, attacking twice a second each.
    assert!((10..=12 * 20).contains(&attacks), "{attacks} attacks");

    // Over one more second, each member in reach attacks twice.
    let mut last_second = 0;
    for _ in 0..60 {
        let (next, stats) = step_swarm(&members, &params, &field);
        members = next;
        last_second += stats[STAT_ATTACKS];
    }
    let in_reach = members
        .iter()
        .filter(|m| flat(m).distance(target) <= d.attack_range)
        .count() as u32;
    assert!(
        last_second.abs_diff(2 * in_reach) <= 2,
        "{last_second} attacks from {in_reach} members in reach"
    );
}

#[test]
fn spawn_randoms_are_spread_over_the_unit_interval() {
    let values: Vec<f32> = (0..10_000).map(|k| spawn_random(7, k, 0)).collect();
    assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    assert!((mean - 0.5).abs() < 0.02, "mean {mean}");
    assert_ne!(spawn_random(7, 3, 0), spawn_random(7, 3, 1));
}

#[test]
fn swarm_structs_match_the_shader_layout() {
    assert_eq!(std::mem::size_of::<Member>(), 48);
    assert_eq!(std::mem::size_of::<DamageVolume>(), 32);
    assert_eq!(std::mem::size_of::<SpawnRequest>(), 32);
    assert_eq!(
        std::mem::size_of::<SwarmParams>(),
        160 + 32 * MAX_VOLUMES + 32 * MAX_SPAWNS
    );
}

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("no GPU adapter");
    pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap()
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
    device.poll(wgpu::Maintain::Wait);
    let bytes = staging.slice(..).get_mapped_range().to_vec();
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
