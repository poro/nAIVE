//! CPU swarm simulation and shared planning for native GPU swarms.
//!
//! Layouts, targeting, flow fields, orders and reports have one implementation;
//! the native client owns compute shaders, GPU buffers and rendering.

mod cpu;
mod flow;

pub use cpu::{spawn_members, step_swarm, CpuSwarms};
pub use flow::{blocked_cells, plane_obstacles, solve_flow_field, static_boxes};

#[cfg(test)]
use cpu::{blocked_at, spawn_random};
#[cfg(test)]
mod tests;

use bytemuck::Zeroable;
use glam::{Vec2, Vec3};

use crate::components::Transform;
use crate::scene::GpuSwarmDef;
use crate::swarm_api::{SwarmBus, SwarmTarget};
use crate::world::SceneWorld;

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

// --- What the GPU and CPU backends share ---

/// The entity id a swarm entity is registered under.
#[doc(hidden)]
pub fn swarm_id(scene_world: &SceneWorld, entity: hecs::Entity) -> String {
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
#[doc(hidden)]
pub struct SwarmCore {
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
#[doc(hidden)]
pub struct StepPlan {
    pub params: SwarmParams,
    pub field_changed: bool,
    pub target_id: Option<String>,
}

impl SwarmCore {
    pub fn new(
        id: String,
        def: &GpuSwarmDef,
        count: u32,
        origin: Vec3,
        bus: &mut SwarmBus,
    ) -> Self {
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

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn definition(&self) -> &GpuSwarmDef {
        &self.def
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub fn field(&self) -> &[[f32; 4]] {
        &self.field
    }

    pub fn step(&self) -> u64 {
        self.step
    }

    /// Everything about the next step except the material: target, flow
    /// field, and the scripts' damage volumes and spawn requests.
    pub fn plan(
        &mut self,
        scene_world: &SceneWorld,
        boxes: &[(Vec3, Vec3)],
        bus: &mut SwarmBus,
        origin: Vec3,
        dt: f32,
    ) -> StepPlan {
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
        params.motion = [
            def.speed,
            def.acceleration,
            origin.y,
            def.arrive_radius.max(0.001),
        ];
        params.material[3] = def.scale;
        params.region = [
            self.region.min.x,
            self.region.min.y,
            self.region.size,
            def.spacing.max(0.01),
        ];
        params.counts = [
            self.count,
            FIELD_CELLS,
            self.region.grid_cells,
            CELL_CAPACITY,
        ];
        params.steering = [
            def.separation,
            def.health.max(f32::MIN_POSITIVE),
            half,
            if attacks {
                def.attack_range * ATTACK_STOP
            } else {
                0.0
            },
        ];
        params.combat = [
            if attacks { def.attack_range } else { 0.0 },
            interval,
            0.0,
            0.0,
        ];

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
    pub fn finish(
        &mut self,
        bus: &mut SwarmBus,
        step: u64,
        stats: [u32; 4],
        target_id: Option<String>,
    ) {
        if step <= self.reported_step {
            return;
        }
        self.reported_step = step;
        let kills = stats[STAT_KILLS].wrapping_sub(self.reported[0]);
        let attacks = stats[STAT_ATTACKS].wrapping_sub(self.reported[1]);
        self.reported = [stats[STAT_KILLS], stats[STAT_ATTACKS]];
        let damage = attacks as f32 * self.def.attack_damage;
        bus.report(
            &self.id,
            step,
            kills,
            attacks,
            damage,
            target_id,
            stats[STAT_ALIVE],
        );
    }
}
