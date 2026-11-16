//! CPU reference simulation and backend used by headless tests.

use super::*;

use std::collections::HashMap;

use crate::components::GpuSwarm;
use crate::physics::PhysicsWorld;

// --- CPU copy of the shaders, for tests and the headless runner ---

fn region_min(p: &SwarmParams) -> Vec2 {
    Vec2::new(p.region[0], p.region[1])
}

fn grid_coords(p: &SwarmParams, pos: Vec2) -> (i32, i32) {
    let n = p.counts[2] as i32;
    let c = ((pos - region_min(p)) / (p.region[2] / n as f32)).floor();
    ((c.x as i32).clamp(0, n - 1), (c.y as i32).clamp(0, n - 1))
}

pub(super) fn blocked_at(p: &SwarmParams, field: &[[f32; 4]], pos: Vec2) -> bool {
    let n = p.counts[1] as i32;
    let c = ((pos - region_min(p)) / (p.region[2] / n as f32)).floor();
    let (x, z) = (c.x as i32, c.y as i32);
    if x < 0 || z < 0 || x >= n || z >= n {
        return false;
    }
    field[(z * n + x) as usize][3] > 0.5
}

fn clamp_to_region(p: &SwarmParams, pos: Vec2) -> Vec2 {
    let lo = region_min(p) + Vec2::splat(0.01);
    let hi = region_min(p) + Vec2::splat(p.region[2] - 0.01);
    pos.clamp(lo, hi)
}

fn flow_direction(p: &SwarmParams, field: &[[f32; 4]], pos: Vec2) -> Vec2 {
    let n = p.counts[1] as i32;
    let g = (pos - region_min(p)) / (p.region[2] / n as f32) - 0.5;
    let base = g.floor();
    let t = g - base;
    let (x0, z0) = (base.x as i32, base.y as i32);
    let mut sum = Vec2::ZERO;
    for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let (x, z) = (x0 + dx, z0 + dz);
        if x < 0 || z < 0 || x >= n || z >= n {
            continue;
        }
        let cell = field[(z * n + x) as usize];
        if cell[2] < 0.0 || cell[3] > 0.5 {
            continue;
        }
        let wx = if dx == 1 { t.x } else { 1.0 - t.x };
        let wz = if dz == 1 { t.y } else { 1.0 - t.y };
        sum += Vec2::new(cell[0], cell[1]) * (wx * wz);
    }
    if sum.length() > 0.0001 {
        sum.normalize()
    } else {
        Vec2::ZERO
    }
}

fn hash32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// A value in [0, 1) for spawn number `index` of a swarm seeded `seed`.
pub(super) fn spawn_random(seed: u32, index: u32, stream: u32) -> f32 {
    let h = hash32(index ^ hash32(seed.wrapping_mul(0x9e37_79b9).wrapping_add(stream)));
    (h >> 8) as f32 / 16_777_216.0
}

fn segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len2 = ab.dot(ab);
    let t = if len2 > 1e-8 {
        ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}

fn heading_toward(from: Vec2, to: Vec2) -> f32 {
    let d = to - from;
    if d.length() < 0.0001 {
        0.0
    } else {
        d.x.atan2(d.y)
    }
}

/// One step of the whole swarm: `bin_main` then `steer_main`. Returns the
/// new members and the step's counters: kills, attacks on the target,
/// living members, spawn claims. Binning and spawn claims go in member
/// order; on the GPU their order varies.
pub fn step_swarm(
    members: &[Member],
    p: &SwarmParams,
    field: &[[f32; 4]],
) -> (Vec<Member>, [u32; 4]) {
    let grid_n = p.counts[2] as usize;
    let cap = p.counts[3] as usize;
    let mut counts = vec![0u32; grid_n * grid_n];
    let mut slots = vec![0u32; grid_n * grid_n * cap];
    for (i, m) in members.iter().enumerate().filter(|(_, m)| m.alive()) {
        let (x, z) = grid_coords(p, Vec2::new(m.position[0], m.position[2]));
        let cell = z as usize * grid_n + x as usize;
        let slot = counts[cell] as usize;
        counts[cell] += 1;
        if slot < cap {
            slots[cell * cap + slot] = i as u32;
        }
    }
    let mut stats = [0u32; 4];
    let mut out = Vec::with_capacity(members.len());
    for (i, m) in members.iter().enumerate() {
        out.push(if m.alive() {
            steer(i as u32, m, members, p, field, &counts, &slots, &mut stats)
        } else {
            dead_or_revive(m, p, &mut stats)
        });
    }
    (out, stats)
}

fn dead_or_revive(m: &Member, p: &SwarmParams, stats: &mut [u32; 4]) -> Member {
    if p.frame[2] == 0 {
        return *m;
    }
    let slot = stats[STAT_CLAIMS];
    stats[STAT_CLAIMS] += 1;
    let Some(req) = p.spawns[..p.frame[1] as usize]
        .iter()
        .find(|r| slot >= r.range[0] && slot < r.range[0] + r.range[1])
    else {
        return *m;
    };
    let n = req.range[2].wrapping_add(slot - req.range[0]);
    let seed = req.range[3];
    let radius = req.center[3] * spawn_random(seed, n, 0).sqrt();
    let angle = TAU * spawn_random(seed, n, 1);
    let centre = Vec2::new(req.center[0], req.center[2]);
    let pos = clamp_to_region(p, centre + radius * Vec2::new(angle.cos(), angle.sin()));
    let height = p.motion[2];
    let heading = heading_toward(pos, Vec2::new(p.target_pos[0], p.target_pos[2]));
    let speed_factor = 0.85 + 0.3 * spawn_random(seed, n, 2);
    stats[STAT_ALIVE] += 1;
    Member {
        position: [pos.x, height, pos.y, heading],
        velocity: [0.0, 0.0, 0.0, speed_factor],
        life: [p.steering[1], p.combat[1], 0.0, 0.0],
    }
}

#[allow(clippy::too_many_arguments)]
fn steer(
    i: u32,
    m: &Member,
    members: &[Member],
    p: &SwarmParams,
    field: &[[f32; 4]],
    counts: &[u32],
    slots: &[u32],
    stats: &mut [u32; 4],
) -> Member {
    let dt = p.target_pos[3];
    let [max_speed, acceleration, height, arrive_radius] = p.motion;
    let separation_radius = p.region[3];
    let pos = Vec2::new(m.position[0], m.position[2]);
    let speed_factor = m.velocity[3];

    let mut health = m.life[0];
    let mut flash = (m.life[2] - dt).max(0.0);
    let here = Vec3::new(m.position[0], m.position[1], m.position[2]);
    for volume in &p.volumes[..p.frame[0] as usize] {
        let (a, b) = (
            Vec3::new(volume.a[0], volume.a[1], volume.a[2]),
            Vec3::new(volume.b[0], volume.b[1], volume.b[2]),
        );
        if segment_distance(here, a, b) <= volume.a[3] + p.steering[2] {
            health -= volume.b[3];
            flash = HIT_FLASH;
        }
    }
    if health <= 0.0 {
        stats[STAT_KILLS] += 1;
        return Member {
            position: m.position,
            velocity: [0.0, 0.0, 0.0, speed_factor],
            life: [0.0; 4],
        };
    }

    let target = Vec2::new(p.target_pos[0], p.target_pos[2]);
    let to_target = target - pos;
    let dist = to_target.length();
    let field_cell = p.region[2] / p.counts[1] as f32;
    let mut dir = Vec2::ZERO;
    if dist > 0.0001 {
        dir = to_target / dist;
        if dist > 2.0 * field_cell {
            let flow = flow_direction(p, field, pos);
            if flow.length_squared() > 0.0 {
                dir = flow;
            }
        }
    }
    let stand_off = p.steering[3];
    let speed = max_speed * speed_factor * ((dist - stand_off) / arrive_radius).clamp(0.0, 1.0);

    let mut cooldown = (m.life[1] - dt).max(0.0);
    if p.combat[0] > 0.0 && dt > 0.0 && dist <= p.combat[0] && cooldown <= 0.0 {
        stats[STAT_ATTACKS] += 1;
        cooldown = p.combat[1];
    }
    stats[STAT_ALIVE] += 1;

    let mut push = Vec2::ZERO;
    let mut pressure = 0.0;
    let n = p.counts[2] as i32;
    let cap = p.counts[3];
    let (hx, hz) = grid_coords(p, pos);
    for dz in -1..=1 {
        for dx in -1..=1 {
            let (x, z) = (hx + dx, hz + dz);
            if x < 0 || z < 0 || x >= n || z >= n {
                continue;
            }
            let cell = (z * n + x) as u32;
            for k in 0..counts[cell as usize].min(cap) {
                let j = slots[(cell * cap + k) as usize];
                if j == i {
                    continue;
                }
                let other = &members[j as usize];
                let d = pos - Vec2::new(other.position[0], other.position[2]);
                let len = d.length();
                if len > 0.00001 && len < separation_radius {
                    let closeness = 1.0 - len / separation_radius;
                    push += d / len * closeness;
                    pressure += closeness;
                }
            }
        }
    }
    if dist > 0.00001 && dist < stand_off {
        let closeness = 1.0 - dist / stand_off;
        push -= to_target / dist * (closeness * TARGET_PUSH);
        pressure += closeness;
    }
    let crowding = f32::min(1.0, pressure);
    let mut desired = dir * speed * (1.0 - crowding) + push * (max_speed * p.steering[0]);
    let top = max_speed * speed_factor;
    let desired_len = desired.length();
    if desired_len > top {
        desired *= top / desired_len;
    }

    let blend = (acceleration * dt).min(1.0);
    let old_v = Vec2::new(m.velocity[0], m.velocity[2]);
    let mut v = old_v + (desired - old_v) * blend;

    let mut next = pos + v * dt;
    if !blocked_at(p, field, pos) {
        if blocked_at(p, field, Vec2::new(next.x, pos.y)) {
            next.x = pos.x;
            v.x = 0.0;
        }
        if blocked_at(p, field, next) {
            next.y = pos.y;
            v.y = 0.0;
        }
    }
    next = clamp_to_region(p, next);

    let mut heading = m.position[3];
    if v.length() > 0.05 {
        heading = v.x.atan2(v.y);
    }

    Member {
        position: [next.x, height, next.y, heading],
        velocity: [v.x, 0.0, v.y, speed_factor],
        life: [health, cooldown, flash, 0.0],
    }
}

/// Initial members: the first `initial_count` alive, spread over a disc of
/// `spawn_radius` around `origin` (kept inside the region), at rest, each
/// with its own speed factor and attack timing. The rest of the pool starts
/// dead, waiting for `swarm.spawn`.
pub fn spawn_members(def: &GpuSwarmDef, origin: Vec3) -> Vec<Member> {
    let radius = def.spawn_radius.min(def.field_size * 0.5 - 0.5).max(0.0);
    let alive = def.initial_count();
    (0..def.count)
        .map(|k| {
            let u = unit_random(def.seed, k, 0);
            let v = unit_random(def.seed, k, 1);
            let w = unit_random(def.seed, k, 2);
            let r = radius * u.sqrt();
            let angle = std::f32::consts::TAU * v;
            let life = if k < alive {
                [
                    def.health.max(f32::MIN_POSITIVE),
                    def.attack_interval * unit_random(def.seed, k, 3),
                    0.0,
                    0.0,
                ]
            } else {
                [0.0; 4]
            };
            Member {
                position: [
                    origin.x + r * angle.cos(),
                    origin.y,
                    origin.z + r * angle.sin(),
                    0.0,
                ],
                velocity: [0.0, 0.0, 0.0, 0.85 + 0.3 * w],
                life,
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

// --- CPU backend for the headless test runner ---

/// Swarms stepped on the CPU with `step_swarm`, for the headless test
/// runner. Same behaviour as the GPU, except that counters are reported on
/// the step that produced them and neighbour order is fixed.
#[derive(Default)]
pub struct CpuSwarms {
    swarms: HashMap<hecs::Entity, (SwarmCore, Vec<Member>)>,
    /// Running kill and attack totals per swarm, as the GPU keeps them.
    totals: HashMap<hecs::Entity, [u32; 2]>,
}

impl CpuSwarms {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.swarms.clear();
        self.totals.clear();
    }

    /// The members of the swarm registered as `id`, for tests.
    pub fn members(&self, id: &str) -> Option<&[Member]> {
        self.swarms
            .values()
            .find(|(core, _)| core.id == id)
            .map(|(_, members)| members.as_slice())
    }

    /// Advance every swarm by `dt` seconds.
    pub fn update(
        &mut self,
        scene_world: &SceneWorld,
        physics: Option<&PhysicsWorld>,
        bus: &mut SwarmBus,
        dt: f32,
    ) {
        let world = &scene_world.world;
        self.swarms
            .retain(|&entity, _| world.get::<&GpuSwarm>(entity).is_ok());
        self.totals
            .retain(|entity, _| world.get::<&GpuSwarm>(*entity).is_ok());
        let boxes = physics.map(static_boxes).unwrap_or_default();

        for (entity, (swarm, transform)) in world
            .query::<(hecs::Entity, (&GpuSwarm, &Transform))>()
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
                .is_some_and(|(core, _)| core.def != swarm.def);
            if stale || !self.swarms.contains_key(&entity) {
                let id = swarm_id(scene_world, entity);
                let core = SwarmCore::new(id, &swarm.def, swarm.def.count, origin, bus);
                self.swarms
                    .insert(entity, (core, spawn_members(&swarm.def, origin)));
                self.totals.insert(entity, [0, 0]);
            }
            let (core, members) = self
                .swarms
                .get_mut(&entity)
                .expect("swarm was just created");
            let plan = core.plan(scene_world, &boxes, bus, origin, dt);
            let (next, stats) = step_swarm(members, &plan.params, &core.field);
            *members = next;
            let totals = self.totals.entry(entity).or_default();
            totals[0] = totals[0].wrapping_add(stats[STAT_KILLS]);
            totals[1] = totals[1].wrapping_add(stats[STAT_ATTACKS]);
            let reported = [totals[0], totals[1], stats[STAT_ALIVE], stats[STAT_CLAIMS]];
            let step = core.step;
            core.finish(bus, step, reported, plan.target_id);
        }
    }
}
