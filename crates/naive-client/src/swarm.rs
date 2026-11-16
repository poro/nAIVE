//! GPU swarms (Tier 4): many members simulated by compute shaders and drawn
//! with the deferred pipeline's instanced shaders.
//!
//! A swarm is one ECS entity with a `GpuSwarm` component. Its members exist
//! only in GPU buffers, owned here and keyed by that entity. Each frame, per
//! swarm:
//!
//! 1. `bin_main` files every member into a cell of a neighbour grid. Cells
//!    hold up to `CELL_CAPACITY` members; the rest go unseen that frame.
//! 2. `steer_main` moves each member along a flow field toward the target,
//!    pushes it away from neighbours closer than the separation radius,
//!    slides it along walls, and writes its draw data (the 256-byte
//!    `DrawUniforms` the G-buffer and shadow shaders read).
//!
//! Members are double-buffered, so every member reads the same frame's
//! positions. The flow field is solved on the CPU around static colliders
//! and uploaded when the target changes cell or the obstacles change.
//! Nothing is read back.
//!
//! `step_swarm` is a CPU copy of the shaders' maths, for tests.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use glam::{Vec2, Vec3};
use wgpu::util::DeviceExt;

use crate::components::{GpuSwarm, Hidden, MaterialOverride, MeshHandle, Transform};
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::physics::PhysicsWorld;
use crate::renderer::{DrawUniformPool, GpuState, DRAW_UNIFORM_SIZE};
use crate::scene::GpuSwarmDef;
use crate::texture_cache::TextureHandle;
use crate::world::SceneWorld;

const WORKGROUP_SIZE: u32 = 64;
/// The flow field is this many cells on a side.
pub const FIELD_CELLS: u32 = 128;
/// Members one neighbour-grid cell can list.
pub const CELL_CAPACITY: u32 = 8;
/// Upper bound on neighbour-grid cells per side, to bound memory.
const MAX_GRID_CELLS: u32 = 512;

/// One member, as the compute shaders store it.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Member {
    /// xyz: position. w: heading, the yaw in radians it faces.
    pub position: [f32; 4],
    /// xyz: velocity. w: this member's speed factor.
    pub velocity: [f32; 4],
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
    /// x: separation strength.
    pub steering: [f32; 4],
}

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
@group(0) @binding(1) var<storage, read> members_in: array<Member>;
@group(0) @binding(2) var<storage, read_write> members_out: array<Member>;
@group(0) @binding(3) var<storage, read_write> draws: array<DrawUniforms>;
// Per flow-field cell: direction x, direction z, distance (-1 unreachable), blocked.
@group(0) @binding(4) var<storage, read> field: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> cell_counts: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read_write> cell_members: array<u32>;

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

@compute @workgroup_size(64)
fn bin_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.counts.x) {
        return;
    }
    let c = grid_coords(members_in[i].position.xz);
    let cell = u32(c.y) * params.counts.z + u32(c.x);
    let slot = atomicAdd(&cell_counts[cell], 1u);
    if (slot < params.counts.w) {
        cell_members[cell * params.counts.w + slot] = i;
    }
}

@compute @workgroup_size(64)
fn steer_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.counts.x) {
        return;
    }
    let m = members_in[i];
    let dt = params.target_pos.w;
    let max_speed = params.motion.x;
    let acceleration = params.motion.y;
    let height = params.motion.z;
    let arrive_radius = params.motion.w;
    let separation_radius = params.region.w;
    let pos = m.position.xz;
    let speed_factor = m.velocity.w;

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
    let speed = max_speed * speed_factor * min(1.0, dist / arrive_radius);

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
    let lo = params.region.xy + vec2<f32>(0.01, 0.01);
    let hi = params.region.xy + vec2<f32>(params.region.z - 0.01, params.region.z - 0.01);
    next = clamp(next, lo, hi);

    // Face the direction of travel; keep the last heading when nearly still.
    var heading = m.position.w;
    if (length(v) > 0.05) {
        heading = atan2(v.x, v.y);
    }

    members_out[i] = Member(vec4<f32>(next.x, height, next.y, heading), vec4<f32>(v.x, 0.0, v.y, speed_factor));

    // Model matrix: translate * rotate about Y by heading * uniform scale.
    let scale = params.material.w;
    let c = cos(heading);
    let s = sin(heading);
    let x_axis = vec4<f32>(c, 0.0, -s, 0.0);
    let y_axis = vec4<f32>(0.0, 1.0, 0.0, 0.0);
    let z_axis = vec4<f32>(s, 0.0, c, 0.0);
    draws[i].model_matrix = mat4x4<f32>(x_axis * scale, y_axis * scale, z_axis * scale, vec4<f32>(next.x, height, next.y, 1.0));
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

// --- CPU copy of the shaders, for tests ---

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

/// One step of the whole swarm: `bin_main` then `steer_main`. Binning goes in
/// member order; on the GPU the order inside a cell varies.
pub fn step_swarm(members: &[Member], p: &SwarmParams, field: &[[f32; 4]]) -> Vec<Member> {
    let grid_n = p.counts[2] as usize;
    let cap = p.counts[3] as usize;
    let mut counts = vec![0u32; grid_n * grid_n];
    let mut slots = vec![0u32; grid_n * grid_n * cap];
    for (i, m) in members.iter().enumerate() {
        let (x, z) = grid_coords(p, Vec2::new(m.position[0], m.position[2]));
        let cell = z as usize * grid_n + x as usize;
        let slot = counts[cell] as usize;
        counts[cell] += 1;
        if slot < cap {
            slots[cell * cap + slot] = i as u32;
        }
    }
    members
        .iter()
        .enumerate()
        .map(|(i, m)| steer(i as u32, m, members, p, field, &counts, &slots))
        .collect()
}

fn steer(
    i: u32,
    m: &Member,
    members: &[Member],
    p: &SwarmParams,
    field: &[[f32; 4]],
    counts: &[u32],
    slots: &[u32],
) -> Member {
    let dt = p.target_pos[3];
    let [max_speed, acceleration, height, arrive_radius] = p.motion;
    let separation_radius = p.region[3];
    let pos = Vec2::new(m.position[0], m.position[2]);
    let speed_factor = m.velocity[3];

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
    let speed = max_speed * speed_factor * (dist / arrive_radius).min(1.0);

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
    let lo = region_min(p) + Vec2::splat(0.01);
    let hi = region_min(p) + Vec2::splat(p.region[2] - 0.01);
    next = next.clamp(lo, hi);

    let mut heading = m.position[3];
    if v.length() > 0.05 {
        heading = v.x.atan2(v.y);
    }

    Member {
        position: [next.x, height, next.y, heading],
        velocity: [v.x, 0.0, v.y, speed_factor],
    }
}

/// Initial members: spread over a disc of `spawn_radius` around `origin`
/// (kept inside the region), at rest, each with its own speed factor.
pub fn spawn_members(def: &GpuSwarmDef, origin: Vec3) -> Vec<Member> {
    let radius = def.spawn_radius.min(def.field_size * 0.5 - 0.5).max(0.0);
    (0..def.count)
        .map(|k| {
            let u = unit_random(def.seed, k, 0);
            let v = unit_random(def.seed, k, 1);
            let w = unit_random(def.seed, k, 2);
            let r = radius * u.sqrt();
            let angle = std::f32::consts::TAU * v;
            Member {
                position: [origin.x + r * angle.cos(), origin.y, origin.z + r * angle.sin(), 0.0],
                velocity: [0.0, 0.0, 0.0, 0.85 + 0.3 * w],
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
                ],
            })
        };
        let bind_groups = [bind_group(0), bind_group(1)];
        Self {
            params,
            members,
            draws,
            field,
            cell_counts,
            _cell_members: cell_members,
            bind_groups,
            count,
        }
    }
}

/// Record one step for each `(swarm, parity)`: clear its grid, bin, steer.
pub fn encode_steps(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &SwarmPipelines,
    steps: &[(&SwarmBuffers, usize)],
) {
    for (buffers, _) in steps {
        encoder.clear_buffer(&buffers.cell_counts, 0, None);
    }
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("Swarm step"),
        timestamp_writes: None,
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

/// GPU state of one swarm.
struct SwarmGpu {
    def: GpuSwarmDef,
    mesh: MeshHandle,
    region: Region,
    buffers: SwarmBuffers,
    /// Which member buffer the next step reads.
    parity: usize,
    draw_bind_group: wgpu::BindGroup,
    /// The target cell and obstacles the current flow field was solved for.
    field_key: Option<((i32, i32), Vec<Obstacle>)>,
    albedo_texture: Option<TextureHandle>,
    hidden: bool,
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

    /// Number of members across all swarms.
    pub fn member_count(&self) -> u32 {
        self.swarms.values().map(|s| s.buffers.count).sum()
    }

    /// Bring the GPU state in line with the world's `GpuSwarm` entities,
    /// then advance every swarm by `dt` seconds and rewrite its draw data.
    /// A `dt` of zero (while paused) only rewrites the draw data.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        gpu: &GpuState,
        scene_world: &SceneWorld,
        physics: Option<&PhysicsWorld>,
        mesh_cache: &MeshCache,
        material_cache: &MaterialCache,
        draw_pool: &DrawUniformPool,
        dt: f32,
    ) {
        let world = &scene_world.world;

        // Drop swarms whose entity is gone or no longer has the component.
        self.swarms.retain(|&entity, _| world.get::<&GpuSwarm>(entity).is_ok());
        let boxes = physics.map(static_boxes).unwrap_or_default();

        let mut active: Vec<hecs::Entity> = Vec::new();
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

            let def = &swarm.def;
            let state = self.swarms.get_mut(&entity).expect("swarm state was just created");
            state.albedo_texture = material.albedo_texture;
            state.hidden = hidden.is_some();

            // Re-solve the flow field when the target moves to another cell
            // or the obstacles change.
            let half = def.scale * 0.5;
            let target_2d = Vec2::new(target.x, target.z);
            let target_cell = state.region.field_cell(target_2d);
            let obstacles = plane_obstacles(&boxes, origin.y, half);
            let current = state
                .field_key
                .as_ref()
                .is_some_and(|(cell, obs)| *cell == target_cell && *obs == obstacles);
            if !current {
                let blocked = blocked_cells(&state.region, &obstacles, half);
                let field = solve_flow_field(&state.region, &blocked, target_2d);
                gpu.queue.write_buffer(&state.buffers.field, 0, bytemuck::cast_slice(&field));
                state.field_key = Some((target_cell, obstacles));
            }

            let params = SwarmParams {
                target_pos: [target.x, target.y, target.z, dt],
                motion: [def.speed, def.acceleration, origin.y, def.arrive_radius.max(0.001)],
                base_color,
                material: [roughness, metallic, if has_texture { 1.0 } else { 0.0 }, def.scale],
                emission,
                region: [state.region.min.x, state.region.min.y, state.region.size, def.spacing.max(0.01)],
                counts: [state.buffers.count, FIELD_CELLS, state.region.grid_cells, CELL_CAPACITY],
                steering: [def.separation, 0.0, 0.0, 0.0],
            };
            gpu.queue.write_buffer(&state.buffers.params, 0, bytemuck::bytes_of(&params));
            active.push(entity);
        }

        let Some(pipelines) = &self.pipelines else {
            return;
        };
        if active.is_empty() {
            return;
        }
        {
            let steps: Vec<(&SwarmBuffers, usize)> = active
                .iter()
                .map(|e| {
                    let state = &self.swarms[e];
                    (&state.buffers, state.parity)
                })
                .collect();
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Swarm step"),
                });
            encode_steps(&mut encoder, pipelines, &steps);
            gpu.queue.submit(std::iter::once(encoder.finish()));
        }
        for entity in &active {
            if let Some(state) = self.swarms.get_mut(entity) {
                state.parity = 1 - state.parity;
            }
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
        swarm: &GpuSwarm,
        origin: Vec3,
        draw_pool: &DrawUniformPool,
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
        let region = Region::around(origin, &def);
        let members = spawn_members(&def, origin);
        let buffers = SwarmBuffers::new(
            device,
            pipelines,
            &members,
            region.grid_cells,
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

        tracing::info!(
            "Swarm created: {} members, {}x{} neighbour grid",
            buffers.count,
            region.grid_cells,
            region.grid_cells
        );
        SwarmGpu {
            def: swarm.def.clone(),
            mesh: swarm.mesh_handle,
            region,
            buffers,
            parity: 0,
            draw_bind_group,
            field_key: None,
            albedo_texture: None,
            hidden: false,
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
        }
    }

    /// Region, solved flow field and step parameters for `def` around `origin`.
    fn setup(d: &GpuSwarmDef, origin: Vec3, obstacles: &[Obstacle], target: Vec2) -> (Region, Vec<[f32; 4]>, SwarmParams) {
        let region = Region::around(origin, d);
        let blocked = blocked_cells(&region, obstacles, d.scale * 0.5);
        let field = solve_flow_field(&region, &blocked, target);
        let params = SwarmParams {
            target_pos: [target.x, origin.y, target.y, 1.0 / 60.0],
            motion: [d.speed, d.acceleration, origin.y, d.arrive_radius],
            base_color: [0.2, 0.6, 0.2, 1.0],
            material: [0.5, 0.0, 0.0, d.scale],
            emission: [0.0; 4],
            region: [region.min.x, region.min.y, region.size, d.spacing],
            counts: [d.count, FIELD_CELLS, region.grid_cells, CELL_CAPACITY],
            steering: [d.separation, 0.0, 0.0, 0.0],
        };
        (region, field, params)
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
            members = step_swarm(&members, &params, &field);
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
        // The region is centred on the origin of the test, not the spawn point.
        let d = GpuSwarmDef { field_size: 64.0, ..d };
        let target = Vec2::new(12.0, 0.0);
        let region = Region::around(Vec3::new(0.0, 0.25, 0.0), &d);
        let blocked = blocked_cells(&region, &[wall()], d.scale * 0.5);
        let field = solve_flow_field(&region, &blocked, target);
        let (_, _, mut params) = setup(&d, Vec3::new(0.0, 0.25, 0.0), &[wall()], target);
        params.region = [region.min.x, region.min.y, region.size, d.spacing];

        let mut members = spawn_members(&d, origin);
        for _ in 0..60 * 30 {
            members = step_swarm(&members, &params, &field);
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
            members = step_swarm(&members, &params, &field);
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
        let mut members = vec![Member { position: [0.0, 0.0, 0.25, 0.0], velocity: [0.0, 0.0, 0.0, 1.0] }];
        for _ in 0..30 {
            members = step_swarm(&members, &params, &field);
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
        for (a, b) in start.iter().zip(step_swarm(&start, &params, &field)) {
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
        }
        // A spawn disc wider than the region is kept inside it.
        let wide = GpuSwarmDef { spawn_radius: 500.0, ..def(200) };
        for m in spawn_members(&wide, Vec3::ZERO) {
            assert!(flat(&m).length() < wide.field_size * 0.5);
        }
    }

    #[test]
    fn swarm_structs_match_the_shader_layout() {
        assert_eq!(std::mem::size_of::<Member>(), 32);
        assert_eq!(std::mem::size_of::<SwarmParams>(), 128);
    }

    /// Run the compute shaders on a real GPU and compare them with
    /// `step_swarm`. CI has no GPU, so run it by hand:
    /// `cargo test -p naive-client --lib -- --ignored gpu_step`
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_step_matches_the_cpu_reference() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("no GPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
        let pipelines = SwarmPipelines::new(&device);

        // Sparse enough that no grid cell overflows, so both sides see the
        // same neighbours; only the order inside a cell differs.
        let d = GpuSwarmDef { spawn_radius: 10.0, ..def(300) };
        let origin = Vec3::new(0.0, 0.25, 0.0);
        let (region, field, params) = setup(&d, origin, &[wall()], Vec2::new(14.0, 3.0));
        let start: Vec<Member> = spawn_members(&d, origin)
            .into_iter()
            .map(|mut m| {
                m.position[0] -= 14.0;
                m
            })
            .collect();
        let buffers = SwarmBuffers::new(&device, &pipelines, &start, region.grid_cells, wgpu::BufferUsages::COPY_SRC);
        queue.write_buffer(&buffers.field, 0, bytemuck::cast_slice(&field));
        queue.write_buffer(&buffers.params, 0, bytemuck::bytes_of(&params));

        let steps = 20;
        let mut parity = 0;
        let mut cpu = start.clone();
        for _ in 0..steps {
            let mut encoder = device.create_command_encoder(&Default::default());
            encode_steps(&mut encoder, &pipelines, &[(&buffers, parity)]);
            queue.submit(std::iter::once(encoder.finish()));
            parity = 1 - parity;
            cpu = step_swarm(&cpu, &params, &field);
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
        let gpu_members: Vec<Member> = bytemuck::cast_slice(&read(&buffers.members[parity])).to_vec();
        let gpu_draws: Vec<crate::renderer::DrawUniforms> = bytemuck::cast_slice(&read(&buffers.draws)).to_vec();

        let mut moved = 0;
        for (i, (gpu, cpu)) in gpu_members.iter().zip(&cpu).enumerate() {
            for k in 0..4 {
                assert!((gpu.position[k] - cpu.position[k]).abs() < 1e-3, "member {i} position {gpu:?} vs {cpu:?}");
                assert!((gpu.velocity[k] - cpu.velocity[k]).abs() < 1e-3, "member {i} velocity {gpu:?} vs {cpu:?}");
            }
            // From rest, a third of a second is a few tens of centimetres.
            if flat(gpu).distance(flat(&start[i])) > 0.2 {
                moved += 1;
            }
            let model = glam::Mat4::from_cols_array_2d(&gpu_draws[i].model_matrix);
            let expected = glam::Mat4::from_scale_rotation_translation(
                Vec3::splat(d.scale),
                glam::Quat::from_rotation_y(gpu.position[3]),
                Vec3::new(gpu.position[0], gpu.position[1], gpu.position[2]),
            );
            assert!(model.abs_diff_eq(expected, 1e-4), "member {i} model matrix\n{model}\nvs\n{expected}");
        }
        assert!(moved > 250, "only {moved} members moved");
    }
}
