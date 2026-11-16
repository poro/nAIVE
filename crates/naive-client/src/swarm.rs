//! GPU swarms (Tier 4): many members simulated by compute shaders and drawn
//! with the deferred pipeline's instanced shaders.
//!
//! A swarm is one ECS entity with a `GpuSwarm` component. Its members exist
//! only in GPU buffers, owned here and keyed by that entity. `count` is the
//! size of the pool: members die, and `swarm.spawn` brings dead ones back.
//! Each frame, per swarm:
//!
//! 1. `bin_main` files every living member into a cell of a neighbour grid.
//!    Cells hold up to `CELL_CAPACITY` members; the rest go unseen that frame.
//! 2. `steer_main` brings dead members back to fill this frame's spawn
//!    requests, applies this frame's damage volumes, counts kills and attacks
//!    on the target, moves living members along a flow field toward the
//!    target and away from neighbours, slides them along walls, and writes
//!    their draw data (the 256-byte `DrawUniforms` the G-buffer and shadow
//!    shaders read). Dead members get a zero-size draw.
//!
//! Members are double-buffered, so every member reads the same frame's
//! positions. The flow field is solved on the CPU around static colliders
//! and uploaded when the target changes cell or the obstacles change.
//!
//! Scripts talk to swarms through `crate::swarm_api`: requests come in
//! through the `SwarmBus`, and four counters per swarm (kills, attacks on the
//! target, living members, spawn claims) go back out. The GPU copies them to
//! a small buffer that is read back asynchronously, a frame or two late, so
//! nothing waits on the GPU.
//!
//! `step_swarm` is a CPU copy of the shaders' maths, for tests, and
//! `CpuSwarms` runs it in place of the GPU in the headless test runner.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use bytemuck::Zeroable;
use glam::{Vec2, Vec3};
use wgpu::util::DeviceExt;

use crate::components::{GpuSwarm, Hidden, MaterialOverride, MeshHandle, Transform};
use crate::gpu_timer::GpuTimer;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::PhysicsWorld;
use crate::renderer::{DrawUniformPool, GpuState, DRAW_UNIFORM_SIZE};
use crate::scene::GpuSwarmDef;
use crate::swarm_api::{SwarmBus, SwarmTarget};
use crate::texture_cache::TextureHandle;
use crate::world::SceneWorld;

const WORKGROUP_SIZE: u32 = 64;
/// The flow field is this many cells on a side.
pub const FIELD_CELLS: u32 = 128;
/// Members one neighbour-grid cell can list.
pub const CELL_CAPACITY: u32 = 8;
/// Upper bound on neighbour-grid cells per side, to bound memory.
const MAX_GRID_CELLS: u32 = 512;
/// Damage volumes one step applies; more wait for the next frame.
pub const MAX_VOLUMES: usize = 64;
/// Spawn requests one step serves; more wait for the next frame.
pub const MAX_SPAWNS: usize = 8;
/// Seconds a member glows after it is hurt.
pub const HIT_FLASH: f32 = 0.12;
/// Attacking members stop at this fraction of their attack range from the target.
pub const ATTACK_STOP: f32 = 0.8;
/// Full turn, written out so the shader and the CPU copy use the same value.
const TAU: f32 = 6.283_185_5;
/// How much harder than a neighbour the target pushes members out of its space.
const TARGET_PUSH: f32 = 3.0;

/// One member, as the compute shaders store it.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Member {
    /// xyz: position. w: heading, the yaw in radians it faces.
    pub position: [f32; 4],
    /// xyz: velocity. w: this member's speed factor.
    pub velocity: [f32; 4],
    /// Health (zero or less: dead, its slot free), seconds until it can
    /// attack again, seconds left of its hit glow, unused.
    pub life: [f32; 4],
}

impl Member {
    pub fn alive(&self) -> bool {
        self.life[0] > 0.0
    }
}

/// Hurts every living member within `a.w` (plus the member's own half size)
/// of the segment from `a.xyz` to `b.xyz` by `b.w`. A sphere is a segment
/// whose ends meet.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DamageVolume {
    pub a: [f32; 4],
    pub b: [f32; 4],
}

impl DamageVolume {
    pub fn sphere(center: Vec3, radius: f32, damage: f32) -> Self {
        Self::line(center, center, radius, damage)
    }

    pub fn line(from: Vec3, to: Vec3, radius: f32, damage: f32) -> Self {
        Self {
            a: [from.x, from.y, from.z, radius.max(0.0)],
            b: [to.x, to.y, to.z, damage],
        }
    }
}

/// Bring back `range.y` dead members in a disc of `center.w` around
/// `center.xyz`. They take claim slots `range.x ..` of this step; spawn
/// number `range.z + k` of a swarm seeded `range.w` decides where the k-th
/// lands, so every wave lands somewhere new.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpawnRequest {
    pub center: [f32; 4],
    pub range: [u32; 4],
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
    /// Region's min x, min z, side length; separation radius.
    pub region: [f32; 4],
    /// Members, flow-field cells per side, neighbour-grid cells per side, cell capacity.
    pub counts: [u32; 4],
    /// Separation strength, health on spawning, a member's half size (for
    /// damage volumes), distance kept from the target.
    pub steering: [f32; 4],
    /// Attack range (zero: members never attack), seconds between attacks.
    pub combat: [f32; 4],
    /// Damage volumes, spawn requests, members requested in all.
    pub frame: [u32; 4],
    pub volumes: [DamageVolume; MAX_VOLUMES],
    pub spawns: [SpawnRequest; MAX_SPAWNS],
}

/// The four counters a step leaves in a swarm's stats buffer.
pub const STAT_KILLS: usize = 0;
pub const STAT_ATTACKS: usize = 1;
pub const STAT_ALIVE: usize = 2;
pub const STAT_CLAIMS: usize = 3;

/// The square a swarm lives in, on the horizontal plane. `Vec2` here is
/// world (x, z).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub min: Vec2,
    pub size: f32,
    /// Neighbour-grid cells per side; each cell is at least the separation radius.
    pub grid_cells: u32,
}

impl Region {
    pub fn around(center: Vec3, def: &GpuSwarmDef) -> Self {
        let size = def.field_size.max(1.0);
        let radius = def.spacing.max(0.01);
        let grid_cells = ((size / radius).floor() as u32).clamp(1, MAX_GRID_CELLS);
        Self {
            min: Vec2::new(center.x, center.z) - size * 0.5,
            size,
            grid_cells,
        }
    }

    pub fn field_cell_size(&self) -> f32 {
        self.size / FIELD_CELLS as f32
    }

    /// The flow-field cell holding `p`, clamped into the region.
    pub fn field_cell(&self, p: Vec2) -> (i32, i32) {
        let n = FIELD_CELLS as i32;
        let c = ((p - self.min) / self.field_cell_size()).floor();
        ((c.x as i32).clamp(0, n - 1), (c.y as i32).clamp(0, n - 1))
    }
}

/// What a static obstacle covers on a swarm's plane, in world (x, z).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obstacle {
    pub min: Vec2,
    pub max: Vec2,
}

/// The world-space boxes of every fixed, solid collider.
pub fn static_boxes(physics: &PhysicsWorld) -> Vec<(Vec3, Vec3)> {
    physics
        .collider_set
        .iter()
        .filter(|(_, c)| !c.is_sensor())
        .filter(|(_, c)| {
            c.parent()
                .and_then(|h| physics.rigid_body_set.get(h))
                .is_none_or(|body| body.is_fixed())
        })
        .map(|(_, c)| {
            let aabb = c.compute_aabb();
            (
                Vec3::new(aabb.mins.x, aabb.mins.y, aabb.mins.z),
                Vec3::new(aabb.maxs.x, aabb.maxs.y, aabb.maxs.z),
            )
        })
        .collect()
}

/// The boxes that cross the plane members of half-height `half` move on at
/// `height`. A floor whose top is at the members' feet is not an obstacle.
pub fn plane_obstacles(boxes: &[(Vec3, Vec3)], height: f32, half: f32) -> Vec<Obstacle> {
    const EPSILON: f32 = 0.01;
    boxes
        .iter()
        .filter(|(min, max)| min.y < height + half - EPSILON && max.y > height - half + EPSILON)
        .map(|(min, max)| Obstacle {
            min: Vec2::new(min.x, min.z),
            max: Vec2::new(max.x, max.z),
        })
        .collect()
}

/// Flow-field cells that obstacles, grown by `margin`, overlap.
pub fn blocked_cells(region: &Region, obstacles: &[Obstacle], margin: f32) -> Vec<bool> {
    let n = FIELD_CELLS as i32;
    let cell = region.field_cell_size();
    let mut blocked = vec![false; (n * n) as usize];
    for o in obstacles {
        let lo = ((o.min - margin - region.min) / cell).floor();
        let hi = ((o.max + margin - region.min) / cell).ceil();
        let (x0, z0) = ((lo.x as i32).max(0), (lo.y as i32).max(0));
        let (x1, z1) = ((hi.x as i32).min(n), (hi.y as i32).min(n));
        for z in z0..z1 {
            for x in x0..x1 {
                blocked[(z * n + x) as usize] = true;
            }
        }
    }
    blocked
}

const NEIGHBOURS: [(i32, i32, f32); 8] = [
    (1, 0, 1.0),
    (-1, 0, 1.0),
    (0, 1, 1.0),
    (0, -1, 1.0),
    (1, 1, std::f32::consts::SQRT_2),
    (1, -1, std::f32::consts::SQRT_2),
    (-1, 1, std::f32::consts::SQRT_2),
    (-1, -1, std::f32::consts::SQRT_2),
];

/// A move from cell (x, z) by (dx, dz) lands on an open cell, and a diagonal
/// move does not cut the corner of a blocked one.
fn step_allowed(x: i32, z: i32, dx: i32, dz: i32, open: &impl Fn(i32, i32) -> bool) -> bool {
    open(x + dx, z + dz) && (dx == 0 || dz == 0 || (open(x + dx, z) && open(x, z + dz)))
}

/// Solve the flow field toward `target`: for every cell, `[dir x, dir z,
/// distance to the target, blocked]`. Distance is -1 where the target cannot
/// be reached. Each reachable cell points at its neighbour nearest the target.
pub fn solve_flow_field(region: &Region, blocked: &[bool], target: Vec2) -> Vec<[f32; 4]> {
    let n = FIELD_CELLS as i32;
    let cell = region.field_cell_size();
    let idx = |x: i32, z: i32| (z * n + x) as usize;
    let open = |x: i32, z: i32| x >= 0 && z >= 0 && x < n && z < n && !blocked[idx(x, z)];
    let (tx, tz) = region.field_cell(target);

    // Dijkstra from the target's cell. Non-negative f32 bits sort like the floats.
    let mut dist = vec![f32::INFINITY; (n * n) as usize];
    let mut heap = BinaryHeap::new();
    dist[idx(tx, tz)] = 0.0;
    heap.push(Reverse((0f32.to_bits(), idx(tx, tz))));
    while let Some(Reverse((bits, i))) = heap.pop() {
        let d = f32::from_bits(bits);
        if d > dist[i] {
            continue;
        }
        let (x, z) = (i as i32 % n, i as i32 / n);
        for (dx, dz, cost) in NEIGHBOURS {
            if !step_allowed(x, z, dx, dz, &open) {
                continue;
            }
            let j = idx(x + dx, z + dz);
            let nd = d + cost * cell;
            if nd < dist[j] {
                dist[j] = nd;
                heap.push(Reverse((nd.to_bits(), j)));
            }
        }
    }

    (0..n * n)
        .map(|i| {
            let (x, z) = (i % n, i / n);
            let i = i as usize;
            if blocked[i] {
                return [0.0, 0.0, -1.0, 1.0];
            }
            if !dist[i].is_finite() {
                return [0.0, 0.0, -1.0, 0.0];
            }
            let mut best = (dist[i], Vec2::ZERO);
            for (dx, dz, _) in NEIGHBOURS {
                if !step_allowed(x, z, dx, dz, &open) {
                    continue;
                }
                let d = dist[idx(x + dx, z + dz)];
                if d < best.0 {
                    best = (d, Vec2::new(dx as f32, dz as f32).normalize());
                }
            }
            [best.1.x, best.1.y, dist[i], 0.0]
        })
        .collect()
}

/// The compute shaders: `bin_main` then `steer_main`, one invocation per member.
pub const SWARM_COMPUTE_WGSL: &str = r#"
struct Member {
    position: vec4<f32>,
    velocity: vec4<f32>,
    life: vec4<f32>,
};

struct DamageVolume {
    a: vec4<f32>,
    b: vec4<f32>,
};

struct SpawnRequest {
    center: vec4<f32>,
    range: vec4<u32>,
};

struct SwarmParams {
    target_pos: vec4<f32>,
    motion: vec4<f32>,
    base_color: vec4<f32>,
    material: vec4<f32>,
    emission: vec4<f32>,
    region: vec4<f32>,
    counts: vec4<u32>,
    steering: vec4<f32>,
    combat: vec4<f32>,
    frame: vec4<u32>,
    volumes: array<DamageVolume, 64>,
    spawns: array<SpawnRequest, 8>,
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

const HIT_FLASH: f32 = 0.12;
const TAU: f32 = 6.2831855;
const TARGET_PUSH: f32 = 3.0;

@group(0) @binding(0) var<uniform> params: SwarmParams;
@group(0) @binding(1) var<storage, read> members_in: array<Member>;
@group(0) @binding(2) var<storage, read_write> members_out: array<Member>;
@group(0) @binding(3) var<storage, read_write> draws: array<DrawUniforms>;
// Per flow-field cell: direction x, direction z, distance (-1 unreachable), blocked.
@group(0) @binding(4) var<storage, read> field: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> cell_counts: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read_write> cell_members: array<u32>;
// Kills and attacks (running totals), living members and spawn claims (this step).
@group(0) @binding(7) var<storage, read_write> stats: array<atomic<u32>, 4>;

fn grid_coords(pos: vec2<f32>) -> vec2<i32> {
    let n = i32(params.counts.z);
    let c = floor((pos - params.region.xy) / (params.region.z / f32(n)));
    return clamp(vec2<i32>(c), vec2<i32>(0, 0), vec2<i32>(n - 1, n - 1));
}

fn blocked_at(pos: vec2<f32>) -> bool {
    let n = i32(params.counts.y);
    let c = vec2<i32>(floor((pos - params.region.xy) / (params.region.z / f32(n))));
    if (c.x < 0 || c.y < 0 || c.x >= n || c.y >= n) {
        return false;
    }
    return field[c.y * n + c.x].w > 0.5;
}

fn clamp_to_region(p: vec2<f32>) -> vec2<f32> {
    let lo = params.region.xy + vec2<f32>(0.01, 0.01);
    let hi = params.region.xy + vec2<f32>(params.region.z - 0.01, params.region.z - 0.01);
    return clamp(p, lo, hi);
}

// Bilinear blend of the four nearest cells' directions, skipping blocked and
// unreachable cells. Zero when there is nothing to follow.
fn flow_direction(pos: vec2<f32>) -> vec2<f32> {
    let n = i32(params.counts.y);
    let g = (pos - params.region.xy) / (params.region.z / f32(n)) - 0.5;
    let base = floor(g);
    let t = g - base;
    let origin = vec2<i32>(base);
    var sum = vec2<f32>(0.0, 0.0);
    for (var k = 0; k < 4; k++) {
        let d = vec2<i32>(k & 1, k >> 1u);
        let c = origin + d;
        if (c.x < 0 || c.y < 0 || c.x >= n || c.y >= n) {
            continue;
        }
        let cell = field[c.y * n + c.x];
        if (cell.z < 0.0 || cell.w > 0.5) {
            continue;
        }
        let w = select(1.0 - t.x, t.x, d.x == 1) * select(1.0 - t.y, t.y, d.y == 1);
        sum += cell.xy * w;
    }
    if (length(sum) > 0.0001) {
        return normalize(sum);
    }
    return vec2<f32>(0.0, 0.0);
}

fn hash32(v: u32) -> u32 {
    var x = v;
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

// A value in [0, 1) for spawn number `index` of a swarm seeded `seed`.
fn spawn_random(seed: u32, index: u32, stream: u32) -> f32 {
    let h = hash32(index ^ hash32(seed * 0x9e3779b9u + stream));
    return f32(h >> 8u) / 16777216.0;
}

fn segment_distance(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let ab = b - a;
    let len2 = dot(ab, ab);
    var t = 0.0;
    if (len2 > 1e-8) {
        t = clamp(dot(p - a, ab) / len2, 0.0, 1.0);
    }
    return length(p - (a + ab * t));
}

fn heading_toward(start: vec2<f32>, goal: vec2<f32>) -> f32 {
    let d = goal - start;
    if (length(d) < 0.0001) {
        return 0.0;
    }
    return atan2(d.x, d.y);
}

fn write_draw(i: u32, pos: vec3<f32>, heading: f32, flash: f32) {
    // Model matrix: translate * rotate about Y by heading * uniform scale.
    let scale = params.material.w;
    let c = cos(heading);
    let s = sin(heading);
    let x_axis = vec4<f32>(c, 0.0, -s, 0.0);
    let y_axis = vec4<f32>(0.0, 1.0, 0.0, 0.0);
    let z_axis = vec4<f32>(s, 0.0, c, 0.0);
    draws[i].model_matrix = mat4x4<f32>(x_axis * scale, y_axis * scale, z_axis * scale, vec4<f32>(pos, 1.0));
    // Normals only need the rotation; the shaders normalize them.
    draws[i].normal_matrix = mat4x4<f32>(x_axis, y_axis, z_axis, vec4<f32>(0.0, 0.0, 0.0, 1.0));
    draws[i].base_color = params.base_color;
    draws[i].roughness = params.material.x;
    draws[i].metallic = params.material.y;
    draws[i].has_texture = params.material.z;
    draws[i].has_skin = 0.0;
    draws[i].emission = params.emission + vec4<f32>(3.0, 2.4, 1.6, 0.0) * (flash / HIT_FLASH);
}

// A dead member draws nothing: every vertex lands on one point far below.
fn hide_draw(i: u32) {
    let zero = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    draws[i].model_matrix = mat4x4<f32>(zero, zero, zero, vec4<f32>(0.0, -10000.0, 0.0, 1.0));
}

@compute @workgroup_size(64)
fn bin_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.counts.x || members_in[i].life.x <= 0.0) {
        return;
    }
    let c = grid_coords(members_in[i].position.xz);
    let cell = u32(c.y) * params.counts.z + u32(c.x);
    let slot = atomicAdd(&cell_counts[cell], 1u);
    if (slot < params.counts.w) {
        cell_members[cell * params.counts.w + slot] = i;
    }
}

// A dead member fills a spawn request if one is left this step; otherwise
// it stays dead.
fn dead_or_revive(i: u32, m: Member) {
    if (params.frame.z > 0u) {
        let slot = atomicAdd(&stats[3], 1u);
        for (var r = 0u; r < params.frame.y; r++) {
            let req = params.spawns[r];
            if (slot < req.range.x || slot >= req.range.x + req.range.y) {
                continue;
            }
            let n = req.range.z + (slot - req.range.x);
            let seed = req.range.w;
            let radius = req.center.w * sqrt(spawn_random(seed, n, 0u));
            let angle = TAU * spawn_random(seed, n, 1u);
            let p = clamp_to_region(req.center.xz + radius * vec2<f32>(cos(angle), sin(angle)));
            let height = params.motion.z;
            let heading = heading_toward(p, params.target_pos.xz);
            let speed_factor = 0.85 + 0.3 * spawn_random(seed, n, 2u);
            members_out[i] = Member(
                vec4<f32>(p.x, height, p.y, heading),
                vec4<f32>(0.0, 0.0, 0.0, speed_factor),
                vec4<f32>(params.steering.y, params.combat.y, 0.0, 0.0),
            );
            atomicAdd(&stats[2], 1u);
            write_draw(i, vec3<f32>(p.x, height, p.y), heading, 0.0);
            return;
        }
    }
    members_out[i] = m;
    hide_draw(i);
}

@compute @workgroup_size(64)
fn steer_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.counts.x) {
        return;
    }
    let m = members_in[i];
    if (m.life.x <= 0.0) {
        dead_or_revive(i, m);
        return;
    }
    let dt = params.target_pos.w;
    let max_speed = params.motion.x;
    let acceleration = params.motion.y;
    let height = params.motion.z;
    let arrive_radius = params.motion.w;
    let separation_radius = params.region.w;
    let pos = m.position.xz;
    let speed_factor = m.velocity.w;

    // Damage volumes, measured where the member was drawn last frame.
    var health = m.life.x;
    var flash = max(0.0, m.life.z - dt);
    for (var k = 0u; k < params.frame.x; k++) {
        let volume = params.volumes[k];
        if (segment_distance(m.position.xyz, volume.a.xyz, volume.b.xyz) <= volume.a.w + params.steering.z) {
            health -= volume.b.w;
            flash = HIT_FLASH;
        }
    }
    if (health <= 0.0) {
        atomicAdd(&stats[0], 1u);
        members_out[i] = Member(m.position, vec4<f32>(0.0, 0.0, 0.0, speed_factor), vec4<f32>(0.0, 0.0, 0.0, 0.0));
        hide_draw(i);
        return;
    }

    // Toward the target: along the flow field, or straight when close or
    // when the field has nothing to follow.
    let to_target = params.target_pos.xz - pos;
    let dist = length(to_target);
    let field_cell = params.region.z / f32(params.counts.y);
    var dir = vec2<f32>(0.0, 0.0);
    if (dist > 0.0001) {
        dir = to_target / dist;
        if (dist > 2.0 * field_cell) {
            let flow = flow_direction(pos);
            if (dot(flow, flow) > 0.0) {
                dir = flow;
            }
        }
    }
    // Slow down on arrival, at the stand-off distance from the target.
    let stand_off = params.steering.w;
    let speed = max_speed * speed_factor * clamp((dist - stand_off) / arrive_radius, 0.0, 1.0);

    // Attack the target when in reach and ready.
    var cooldown = max(0.0, m.life.y - dt);
    if (params.combat.x > 0.0 && dt > 0.0 && dist <= params.combat.x && cooldown <= 0.0) {
        atomicAdd(&stats[1], 1u);
        cooldown = params.combat.y;
    }
    atomicAdd(&stats[2], 1u);

    // Away from neighbours closer than the separation radius.
    var push = vec2<f32>(0.0, 0.0);
    var pressure = 0.0;
    let n = i32(params.counts.z);
    let cap = params.counts.w;
    let home = grid_coords(pos);
    for (var dz = -1; dz <= 1; dz++) {
        for (var dx = -1; dx <= 1; dx++) {
            let c = home + vec2<i32>(dx, dz);
            if (c.x < 0 || c.y < 0 || c.x >= n || c.y >= n) {
                continue;
            }
            let cell = u32(c.y * n + c.x);
            let count = min(atomicLoad(&cell_counts[cell]), cap);
            for (var k = 0u; k < count; k++) {
                let j = cell_members[cell * cap + k];
                if (j == i) {
                    continue;
                }
                let d = pos - members_in[j].position.xz;
                let len = length(d);
                if (len > 0.00001 && len < separation_radius) {
                    let closeness = 1.0 - len / separation_radius;
                    push += d / len * closeness;
                    pressure += closeness;
                }
            }
        }
    }
    // Keep out of the target's space the way members keep out of each
    // other's, only firmer, so the crowd behind cannot squeeze them in.
    if (dist > 0.00001 && dist < stand_off) {
        let closeness = 1.0 - dist / stand_off;
        push -= to_target / dist * (closeness * TARGET_PUSH);
        pressure += closeness;
    }
    // Separation comes first: the more crowded a member is (however the
    // pushes balance out), the less it presses on toward the target, so a
    // crowd keeps its spacing instead of compressing.
    let crowding = min(1.0, pressure);
    var desired = dir * speed * (1.0 - crowding) + push * (max_speed * params.steering.x);
    let top = max_speed * speed_factor;
    let desired_len = length(desired);
    if (desired_len > top) {
        desired *= top / desired_len;
    }

    let blend = min(1.0, acceleration * dt);
    var v = m.velocity.xz + (desired - m.velocity.xz) * blend;

    // Slide along walls: never step into a blocked cell (unless already in one).
    var next = pos + v * dt;
    if (!blocked_at(pos)) {
        if (blocked_at(vec2<f32>(next.x, pos.y))) {
            next.x = pos.x;
            v.x = 0.0;
        }
        if (blocked_at(next)) {
            next.y = pos.y;
            v.y = 0.0;
        }
    }
    next = clamp_to_region(next);

    // Face the direction of travel; keep the last heading when nearly still.
    var heading = m.position.w;
    if (length(v) > 0.05) {
        heading = atan2(v.x, v.y);
    }

    members_out[i] = Member(
        vec4<f32>(next.x, height, next.y, heading),
        vec4<f32>(v.x, 0.0, v.y, speed_factor),
        vec4<f32>(health, cooldown, flash, 0.0),
    );
    write_draw(i, vec3<f32>(next.x, height, next.y), heading, flash);
}
"#;

// --- CPU copy of the shaders, for tests and the headless runner ---

fn region_min(p: &SwarmParams) -> Vec2 {
    Vec2::new(p.region[0], p.region[1])
}

fn grid_coords(p: &SwarmParams, pos: Vec2) -> (i32, i32) {
    let n = p.counts[2] as i32;
    let c = ((pos - region_min(p)) / (p.region[2] / n as f32)).floor();
    ((c.x as i32).clamp(0, n - 1), (c.y as i32).clamp(0, n - 1))
}

fn blocked_at(p: &SwarmParams, field: &[[f32; 4]], pos: Vec2) -> bool {
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
fn spawn_random(seed: u32, index: u32, stream: u32) -> f32 {
    let h = hash32(index ^ hash32(seed.wrapping_mul(0x9e37_79b9).wrapping_add(stream)));
    (h >> 8) as f32 / 16_777_216.0
}

fn segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len2 = ab.dot(ab);
    let t = if len2 > 1e-8 { ((p - a).dot(ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
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
pub fn step_swarm(members: &[Member], p: &SwarmParams, field: &[[f32; 4]]) -> (Vec<Member>, [u32; 4]) {
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
        let (a, b) = (Vec3::new(volume.a[0], volume.a[1], volume.a[2]), Vec3::new(volume.b[0], volume.b[1], volume.b[2]));
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
                [def.health.max(f32::MIN_POSITIVE), def.attack_interval * unit_random(def.seed, k, 3), 0.0, 0.0]
            } else {
                [0.0; 4]
            };
            Member {
                position: [origin.x + r * angle.cos(), origin.y, origin.z + r * angle.sin(), 0.0],
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

// --- What the GPU and CPU backends share ---

/// The entity id a swarm entity is registered under.
fn swarm_id(scene_world: &SceneWorld, entity: hecs::Entity) -> String {
    scene_world
        .entity_registry
        .iter()
        .find(|(_, &e)| e == entity)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| format!("{entity:?}"))
}

/// Where a swarm is heading, and the entity it attacks if it is one.
fn resolve_target(
    scene_world: &SceneWorld,
    id: &str,
    def: &GpuSwarmDef,
    bus: &SwarmBus,
    origin: Vec3,
) -> (Vec3, Option<String>) {
    let entity_position = |name: &str| {
        let entity = *scene_world.entity_registry.get(name)?;
        let transform = scene_world.world.get::<&Transform>(entity).ok()?;
        Some(transform.world_matrix.w_axis.truncate())
    };
    let followed = match bus.target(id) {
        Some(SwarmTarget::Point(point)) => return (*point, None),
        Some(SwarmTarget::Entity(name)) => Some(name.as_str()),
        None => def.target_entity.as_deref(),
    };
    followed
        .and_then(|name| entity_position(name).map(|p| (p, Some(name.to_string()))))
        .unwrap_or((origin, None))
}

/// State of one swarm that both backends keep beside its members.
struct SwarmCore {
    id: String,
    def: GpuSwarmDef,
    /// Members in the pool (the GPU may hold fewer than `def.count`).
    count: u32,
    region: Region,
    field: Vec<[f32; 4]>,
    /// The target cell and obstacles the current flow field was solved for.
    field_key: Option<((i32, i32), Vec<Obstacle>)>,
    /// Spawns requested so far, so each wave lands somewhere new.
    spawn_sequence: u32,
    /// Steps taken, and the last one whose counters were reported.
    step: u64,
    reported_step: u64,
    /// Kill and attack totals already reported.
    reported: [u32; 2],
}

/// What one step needs beyond the member buffers.
struct StepPlan {
    params: SwarmParams,
    field_changed: bool,
    target_id: Option<String>,
}

impl SwarmCore {
    fn new(id: String, def: &GpuSwarmDef, count: u32, origin: Vec3, bus: &mut SwarmBus) -> Self {
        let region = Region::around(origin, def);
        bus.register(&id, count, def.initial_count().min(count));
        Self {
            id,
            def: def.clone(),
            count,
            region,
            field: Vec::new(),
            field_key: None,
            spawn_sequence: 0,
            step: 0,
            reported_step: 0,
            reported: [0, 0],
        }
    }

    /// Everything about the next step except the material: target, flow
    /// field, and the scripts' damage volumes and spawn requests.
    fn plan(&mut self, scene_world: &SceneWorld, boxes: &[(Vec3, Vec3)], bus: &mut SwarmBus, origin: Vec3, dt: f32) -> StepPlan {
        self.step += 1;
        let def = &self.def;
        let (target, target_id) = resolve_target(scene_world, &self.id, def, bus, origin);

        // Re-solve the flow field when the target moves to another cell or
        // the obstacles change.
        let half = def.scale * 0.5;
        let target_2d = Vec2::new(target.x, target.z);
        let target_cell = self.region.field_cell(target_2d);
        let obstacles = plane_obstacles(boxes, origin.y, half);
        let current = self
            .field_key
            .as_ref()
            .is_some_and(|(cell, obs)| *cell == target_cell && *obs == obstacles);
        if !current {
            let blocked = blocked_cells(&self.region, &obstacles, half);
            self.field = solve_flow_field(&self.region, &blocked, target_2d);
            self.field_key = Some((target_cell, obstacles));
        }

        let attacks = def.attack_damage > 0.0 && def.attack_range > 0.0;
        let interval = def.attack_interval.max(0.01);
        let mut params = SwarmParams::zeroed();
        params.target_pos = [target.x, target.y, target.z, dt];
        params.motion = [def.speed, def.acceleration, origin.y, def.arrive_radius.max(0.001)];
        params.material[3] = def.scale;
        params.region = [self.region.min.x, self.region.min.y, self.region.size, def.spacing.max(0.01)];
        params.counts = [self.count, FIELD_CELLS, self.region.grid_cells, CELL_CAPACITY];
        params.steering = [
            def.separation,
            def.health.max(f32::MIN_POSITIVE),
            half,
            if attacks { def.attack_range * ATTACK_STOP } else { 0.0 },
        ];
        params.combat = [if attacks { def.attack_range } else { 0.0 }, interval, 0.0, 0.0];

        let (spawns, volumes) = bus.take_orders(&self.id, self.step);
        let mut requested = 0u32;
        for (slot, order) in params.spawns.iter_mut().zip(&spawns) {
            *slot = SpawnRequest {
                center: [order.center.x, order.center.y, order.center.z, order.radius],
                range: [requested, order.count, self.spawn_sequence, def.seed],
            };
            requested += order.count;
            self.spawn_sequence = self.spawn_sequence.wrapping_add(order.count);
        }
        for (slot, volume) in params.volumes.iter_mut().zip(&volumes) {
            *slot = *volume;
        }
        params.frame = [volumes.len() as u32, spawns.len() as u32, requested, 0];

        StepPlan {
            params,
            field_changed: !current,
            target_id,
        }
    }

    /// Turn the counters left by step `step` into a report on the bus.
    /// Kill and attack counters are running totals; a report that arrives
    /// after a later one is dropped, and the later one covers it.
    fn finish(&mut self, bus: &mut SwarmBus, step: u64, stats: [u32; 4], target_id: Option<String>) {
        if step <= self.reported_step {
            return;
        }
        self.reported_step = step;
        let kills = stats[STAT_KILLS].wrapping_sub(self.reported[0]);
        let attacks = stats[STAT_ATTACKS].wrapping_sub(self.reported[1]);
        self.reported = [stats[STAT_KILLS], stats[STAT_ATTACKS]];
        let damage = attacks as f32 * self.def.attack_damage;
        bus.report(&self.id, step, kills, attacks, damage, target_id, stats[STAT_ALIVE]);
    }
}

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
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
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
        let draws = empty("Swarm draw data", DRAW_UNIFORM_SIZE * u64::from(count), storage);
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
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
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
                let view = readback.buffer.slice(..).get_mapped_range();
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
        self.swarms.retain(|&entity, _| world.get::<&GpuSwarm>(entity).is_ok());

        let _ = gpu.device.poll(wgpu::Maintain::Poll);
        for state in self.swarms.values_mut() {
            state.collect_readbacks(bus);
        }

        let boxes = physics.map(static_boxes).unwrap_or_default();
        let mut active: Vec<(hecs::Entity, Option<String>)> = Vec::new();
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
                .is_some_and(|s| s.core.def != swarm.def || s.mesh != swarm.mesh_handle);
            if stale || !self.swarms.contains_key(&entity) {
                let id = swarm_id(scene_world, entity);
                let state = self.create_swarm(gpu, id, swarm, origin, draw_pool, bus);
                self.swarms.insert(entity, state);
            }

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

            let mut plan = state.core.plan(scene_world, &boxes, bus, origin, dt);
            if plan.field_changed {
                gpu.queue.write_buffer(&state.buffers.field, 0, bytemuck::cast_slice(&state.core.field));
            }
            plan.params.base_color = base_color;
            plan.params.material = [roughness, metallic, if has_texture { 1.0 } else { 0.0 }, swarm.def.scale];
            plan.params.emission = emission;
            gpu.queue.write_buffer(&state.buffers.params, 0, bytemuck::bytes_of(&plan.params));
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
            let step = state.core.step;
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
            readback.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let next = if result.is_ok() { READBACK_READY } else { READBACK_FREE };
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
        let pipelines = self.pipelines.get_or_insert_with(|| SwarmPipelines::new(device));

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
        let core = SwarmCore::new(id, &swarm.def, def.count, origin, bus);
        let buffers = SwarmBuffers::new(
            device,
            pipelines,
            &members,
            core.region.grid_cells,
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
            core.id,
            buffers.count,
            core.region.grid_cells,
            core.region.grid_cells
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
    pub fn update(&mut self, scene_world: &SceneWorld, physics: Option<&PhysicsWorld>, bus: &mut SwarmBus, dt: f32) {
        let world = &scene_world.world;
        self.swarms.retain(|&entity, _| world.get::<&GpuSwarm>(entity).is_ok());
        self.totals.retain(|entity, _| world.get::<&GpuSwarm>(*entity).is_ok());
        let boxes = physics.map(static_boxes).unwrap_or_default();

        for (entity, (swarm, transform)) in world.query::<(&GpuSwarm, &Transform)>().iter() {
            if swarm.def.count == 0 {
                self.swarms.remove(&entity);
                continue;
            }
            let origin = transform.world_matrix.w_axis.truncate();
            let stale = self.swarms.get(&entity).is_some_and(|(core, _)| core.def != swarm.def);
            if stale || !self.swarms.contains_key(&entity) {
                let id = swarm_id(scene_world, entity);
                let core = SwarmCore::new(id, &swarm.def, swarm.def.count, origin, bus);
                self.swarms.insert(entity, (core, spawn_members(&swarm.def, origin)));
                self.totals.insert(entity, [0, 0]);
            }
            let (core, members) = self.swarms.get_mut(&entity).expect("swarm was just created");
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
        params.steering = [d.separation, d.health, d.scale * 0.5, if attacks { d.attack_range * ATTACK_STOP } else { 0.0 }];
        params.combat = [if attacks { d.attack_range } else { 0.0 }, d.attack_interval, 0.0, 0.0];
        params
    }

    /// Region, solved flow field and step parameters for `def` around `origin`.
    fn setup(d: &GpuSwarmDef, origin: Vec3, obstacles: &[Obstacle], target: Vec2) -> (Region, Vec<[f32; 4]>, SwarmParams) {
        let region = Region::around(origin, d);
        let blocked = blocked_cells(&region, obstacles, d.scale * 0.5);
        let field = solve_flow_field(&region, &blocked, target);
        let params = params_for(d, &region, origin, target);
        (region, field, params)
    }

    /// Put `volumes` and `spawns` into `params` for one step.
    fn with_orders(mut params: SwarmParams, volumes: &[DamageVolume], spawns: &[SpawnRequest]) -> SwarmParams {
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
        let field = solve_flow_field(&region, &vec![false; (FIELD_CELLS * FIELD_CELLS) as usize], target);
        let n = FIELD_CELLS as i32;
        for z in 0..n {
            for x in 0..n {
                let cell = field[(z * n + x) as usize];
                let to_target = target - cell_centre(&region, x, z);
                if to_target.length() < 2.0 * region.field_cell_size() {
                    continue;
                }
                let dir = Vec2::new(cell[0], cell[1]);
                assert!(dir.dot(to_target.normalize()) > 0.7, "cell ({x}, {z}) points {dir}");
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
            assert!(!blocked[(z * n + x) as usize], "path entered the wall at ({x}, {z})");
        }
        assert_eq!((x, z), (tx, tz), "the arrows never reached the target");
    }

    #[test]
    fn walled_in_cells_cannot_reach_the_target() {
        let d = def(1);
        let region = Region::around(Vec3::ZERO, &d);
        let ring = [
            Obstacle { min: Vec2::new(-6.0, -6.0), max: Vec2::new(6.0, -5.0) },
            Obstacle { min: Vec2::new(-6.0, 5.0), max: Vec2::new(6.0, 6.0) },
            Obstacle { min: Vec2::new(-6.0, -6.0), max: Vec2::new(-5.0, 6.0) },
            Obstacle { min: Vec2::new(5.0, -6.0), max: Vec2::new(6.0, 6.0) },
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
        let o = Obstacle { min: Vec2::new(0.0, 0.0), max: Vec2::new(1.0, 1.0) };
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
        assert_eq!(obstacles, vec![Obstacle { min: Vec2::ZERO, max: Vec2::ONE }]);
    }

    #[test]
    fn members_spread_out_instead_of_stacking() {
        let d = GpuSwarmDef { spawn_radius: 0.3, ..def(150) };
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
        let (min, p10, median) = (nearest[0], nearest[nearest.len() / 10], nearest[nearest.len() / 2]);
        assert!(
            min > 0.4 * d.spacing && median > 0.6 * d.spacing,
            "nearest-neighbour distances: min {min}, 10th percentile {p10}, median {median}"
        );
    }

    #[test]
    fn members_path_around_a_wall_without_entering_it() {
        let d = GpuSwarmDef { spawn_radius: 2.0, ..def(40) };
        let origin = Vec3::new(-12.0, 0.25, 0.0);
        let target = Vec2::new(12.0, 0.0);
        // The region is centred on the origin of the test, not the spawn point.
        let (_, field, params) = setup(&d, Vec3::new(0.0, 0.25, 0.0), &[wall()], target);

        let mut members = spawn_members(&d, origin);
        for _ in 0..60 * 30 {
            members = step_swarm(&members, &params, &field).0;
            for m in &members {
                assert!(!blocked_at(&params, &field, flat(m)), "a member is inside the wall at {}", flat(m));
            }
        }
        let arrived = members.iter().filter(|m| flat(m).distance(target) < 6.0).count();
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
        assert_ne!(a, spawn_members(&GpuSwarmDef { seed: 8, ..d.clone() }, origin));
        for m in &a {
            assert!(flat(m).distance(Vec2::new(5.0, -5.0)) <= d.spawn_radius + 1e-3);
            assert_eq!(m.position[1], 1.0);
            assert!((0.85..=1.15).contains(&m.velocity[3]));
            assert_eq!(m.life[0], d.health);
        }
        // A spawn disc wider than the region is kept inside it.
        let wide = GpuSwarmDef { spawn_radius: 500.0, ..def(200) };
        for m in spawn_members(&wide, Vec3::ZERO) {
            assert!(flat(&m).length() < wide.field_size * 0.5);
        }
        // Only `initial` members start alive.
        let pool = spawn_members(&GpuSwarmDef { initial: Some(30), ..def(100) }, origin);
        assert_eq!(pool.iter().filter(|m| m.alive()).count(), 30);
    }

    #[test]
    fn a_damage_sphere_kills_what_it_covers_and_nothing_else() {
        let d = GpuSwarmDef { spawn_radius: 10.0, ..def(200) };
        let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::ZERO);
        let start = spawn_members(&d, Vec3::ZERO);
        let centre = Vec3::new(3.0, 0.0, -2.0);
        let radius = 4.0;
        let params = with_orders(params, &[DamageVolume::sphere(centre, radius, 50.0)], &[]);
        let (after, stats) = step_swarm(&start, &params, &field);

        let reach = radius + d.scale * 0.5;
        let inside = start.iter().filter(|m| flat(m).distance(Vec2::new(centre.x, centre.z)) <= reach).count();
        assert!(inside > 10, "the test needs members inside the sphere, got {inside}");
        for (before, now) in start.iter().zip(&after) {
            let covered = flat(before).distance(Vec2::new(centre.x, centre.z)) <= reach;
            assert_eq!(now.alive(), !covered, "member at {} covered: {covered}", flat(before));
        }
        assert_eq!(stats[STAT_KILLS] as usize, inside);
        assert_eq!(stats[STAT_ALIVE] as usize, 200 - inside);
    }

    #[test]
    fn damage_below_a_members_health_wounds_it_and_makes_it_glow() {
        let d = def(1);
        let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::new(20.0, 0.0));
        let start = vec![Member { position: [0.0; 4], velocity: [0.0, 0.0, 0.0, 1.0], life: [10.0, 0.0, 0.0, 0.0] }];
        let hit = with_orders(params, &[DamageVolume::sphere(Vec3::ZERO, 1.0, 6.0)], &[]);

        let (once, stats) = step_swarm(&start, &hit, &field);
        assert_eq!(stats[STAT_KILLS], 0);
        assert_eq!(once[0].life[0], 4.0);
        assert_eq!(once[0].life[2], HIT_FLASH);

        // The glow fades with time, and a second hit kills.
        let (calm, _) = step_swarm(&once, &params, &field);
        assert!(calm[0].life[2] < HIT_FLASH);
        let hit_again = with_orders(params, &[DamageVolume::sphere(Vec3::new(calm[0].position[0], 0.0, calm[0].position[2]), 1.0, 6.0)], &[]);
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
            .map(|k| Member { position: [k as f32, 0.0, 0.0, 0.0], velocity: [0.0, 0.0, 0.0, 1.0], life: [10.0, 0.0, 0.0, 0.0] })
            .collect();
        // From behind the first to between the third and fourth, at waist height.
        let line = DamageVolume::line(Vec3::new(-2.0, 0.5, 0.0), Vec3::new(2.5, 0.5, 0.0), 0.3, 20.0);
        let (after, stats) = step_swarm(&start, &with_orders(params, &[line], &[]), &field);
        let alive: Vec<bool> = after.iter().map(Member::alive).collect();
        assert_eq!(alive, [false, false, false, true, true]);
        assert_eq!(stats[STAT_KILLS], 3);
    }

    #[test]
    fn spawning_brings_dead_members_back_inside_the_disc() {
        let d = GpuSwarmDef { initial: Some(0), ..def(100) };
        let (_, field, params) = setup(&d, Vec3::new(0.0, 0.25, 0.0), &[], Vec2::ZERO);
        let start = spawn_members(&d, Vec3::new(0.0, 0.25, 0.0));
        assert!(start.iter().all(|m| !m.alive()));

        let at = Vec3::new(10.0, 0.0, -5.0);
        let order = with_orders(params, &[], &[spawn_request(0, 30, at, 4.0, 0), spawn_request(30, 20, -at, 2.0, 30)]);
        let (after, stats) = step_swarm(&start, &order, &field);
        assert_eq!(stats[STAT_ALIVE], 50);
        let alive: Vec<&Member> = after.iter().filter(|m| m.alive()).collect();
        assert_eq!(alive.len(), 50);
        let near = |m: &&Member, c: Vec3, r: f32| flat(m).distance(Vec2::new(c.x, c.z)) <= r + 1e-4;
        assert_eq!(alive.iter().filter(|m| near(m, at, 4.0)).count(), 30);
        assert_eq!(alive.iter().filter(|m| near(m, -at, 2.0)).count(), 20);
        for m in &alive {
            assert_eq!(m.position[1], 0.25, "spawned members stand on the swarm's plane");
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
        assert_ne!(step_swarm(&start, &first, &field).0, step_swarm(&start, &later, &field).0);
    }

    #[test]
    fn dead_members_neither_push_nor_move() {
        let d = def(2);
        let (_, field, params) = setup(&d, Vec3::ZERO, &[], Vec2::new(0.0, 10.0));
        let corpse = Member { position: [0.3, 0.0, 0.0, 0.0], velocity: [0.0, 0.0, 0.0, 1.0], life: [0.0; 4] };
        let living = Member { position: [0.0, 0.0, 0.0, 0.0], velocity: [0.0, 0.0, 0.0, 1.0], life: [10.0, 0.0, 0.0, 0.0] };
        let (after, stats) = step_swarm(&[living, corpse], &params, &field);
        assert_eq!(after[1], corpse);
        assert_eq!(stats[STAT_ALIVE], 1);
        // The living member moves as if it were alone.
        let (alone, _) = step_swarm(&[living], &params, &field);
        assert_eq!(after[0], alone[0], "pushed by a dead member");
        // A living member in the corpse's place would push it.
        let (pushed, _) = step_swarm(&[living, Member { life: living.life, ..corpse }], &params, &field);
        assert_ne!(pushed[0], alone[0]);
    }

    #[test]
    fn attackers_stop_short_of_the_target_and_hit_on_their_interval() {
        let d = GpuSwarmDef { spawn_radius: 6.0, attack_damage: 5.0, attack_range: 2.0, attack_interval: 0.5, ..def(12) };
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
            assert!(flat(m).distance(target) > 0.6 * d.attack_range, "a member stands on the target at {}", flat(m));
        }
        let in_reach = members.iter().filter(|m| flat(m).distance(target) <= d.attack_range).count();
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
        let in_reach = members.iter().filter(|m| flat(m).distance(target) <= d.attack_range).count() as u32;
        assert!(last_second.abs_diff(2 * in_reach) <= 2, "{last_second} attacks from {in_reach} members in reach");
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
        assert_eq!(std::mem::size_of::<SwarmParams>(), 160 + 32 * MAX_VOLUMES + 32 * MAX_SPAWNS);
    }

    fn gpu() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("no GPU adapter");
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
        staging.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
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
        let d = GpuSwarmDef { spawn_radius: 10.0, attack_damage: 3.0, attack_range: 4.0, attack_interval: 0.1, ..def(300) };
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
        let buffers = SwarmBuffers::new(&device, &pipelines, &start, region.grid_cells, wgpu::BufferUsages::COPY_SRC);
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
                    DamageVolume::line(Vec3::new(-25.0, 0.5, -4.0), Vec3::new(-5.0, 0.5, 6.0), 0.5, 6.0),
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

        let gpu_members: Vec<Member> = bytemuck::cast_slice(&read_back(&device, &queue, &buffers.members[parity])).to_vec();
        let gpu_draws: Vec<crate::renderer::DrawUniforms> = bytemuck::cast_slice(&read_back(&device, &queue, &buffers.draws)).to_vec();
        let gpu_stats: Vec<u32> = bytemuck::cast_slice(&read_back(&device, &queue, &buffers.stats)).to_vec();

        let mut moved = 0;
        for (i, (gpu, cpu)) in gpu_members.iter().zip(&cpu).enumerate() {
            for k in 0..4 {
                assert!((gpu.position[k] - cpu.position[k]).abs() < 1e-3, "member {i} position {gpu:?} vs {cpu:?}");
                assert!((gpu.velocity[k] - cpu.velocity[k]).abs() < 1e-3, "member {i} velocity {gpu:?} vs {cpu:?}");
                assert!((gpu.life[k] - cpu.life[k]).abs() < 1e-3, "member {i} life {gpu:?} vs {cpu:?}");
            }
            // From rest, a third of a second is a few tens of centimetres.
            if flat(gpu).distance(flat(&start[i])) > 0.2 {
                moved += 1;
            }
            let model = glam::Mat4::from_cols_array_2d(&gpu_draws[i].model_matrix);
            if !gpu.alive() {
                assert_eq!(model.x_axis, glam::Vec4::ZERO, "dead member {i} is still drawn");
                continue;
            }
            let expected = glam::Mat4::from_scale_rotation_translation(
                Vec3::splat(d.scale),
                glam::Quat::from_rotation_y(gpu.position[3]),
                Vec3::new(gpu.position[0], gpu.position[1], gpu.position[2]),
            );
            assert!(model.abs_diff_eq(expected, 1e-4), "member {i} model matrix\n{model}\nvs\n{expected}");
        }
        assert!(moved > 150, "only {moved} members moved");
        assert!(cpu_totals[0] > 20 && cpu_totals[1] > 0, "the test should kill and attack: {cpu_totals:?}");
        assert_eq!([gpu_stats[0], gpu_stats[1], gpu_stats[2]], [cpu_totals[0], cpu_totals[1], cpu_alive]);
    }

    /// Spawning on the GPU: which dead slot takes which spawn number varies
    /// from run to run, so compare the positions as sets.
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_spawn_matches_the_cpu_reference() {
        let (device, queue) = gpu();
        let pipelines = SwarmPipelines::new(&device);
        let d = GpuSwarmDef { initial: Some(20), ..def(500) };
        let origin = Vec3::new(0.0, 0.25, 0.0);
        let (region, field, params) = setup(&d, origin, &[], Vec2::new(20.0, 0.0));
        let start = spawn_members(&d, origin);
        let buffers = SwarmBuffers::new(&device, &pipelines, &start, region.grid_cells, wgpu::BufferUsages::COPY_SRC);
        queue.write_buffer(&buffers.field, 0, bytemuck::cast_slice(&field));
        let p = with_orders(
            params,
            &[],
            &[spawn_request(0, 200, Vec3::new(-10.0, 0.0, 5.0), 6.0, 0), spawn_request(200, 100, Vec3::new(12.0, 0.0, -8.0), 3.0, 200)],
        );
        queue.write_buffer(&buffers.params, 0, bytemuck::bytes_of(&p));
        let mut encoder = device.create_command_encoder(&Default::default());
        encode_steps(&mut encoder, &pipelines, &[(&buffers, 0)], None);
        queue.submit(std::iter::once(encoder.finish()));

        let gpu_members: Vec<Member> = bytemuck::cast_slice(&read_back(&device, &queue, &buffers.members[1])).to_vec();
        let gpu_stats: Vec<u32> = bytemuck::cast_slice(&read_back(&device, &queue, &buffers.stats)).to_vec();
        let (cpu, cpu_stats) = step_swarm(&start, &p, &field);
        assert_eq!(gpu_stats[STAT_ALIVE], 320);
        assert_eq!(gpu_stats[STAT_ALIVE], cpu_stats[STAT_ALIVE]);
        assert_eq!(gpu_stats[STAT_CLAIMS], cpu_stats[STAT_CLAIMS]);

        let sorted = |members: &[Member]| {
            let mut v: Vec<[f32; 4]> = members.iter().filter(|m| m.alive()).map(|m| m.position).collect();
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
}
