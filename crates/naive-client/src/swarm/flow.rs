//! Static obstacle rasterization and flow-field pathfinding.

use super::*;

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::physics::PhysicsWorld;

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
