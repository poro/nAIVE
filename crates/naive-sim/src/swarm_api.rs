//! How scripts talk to swarms (Tier 4).
//!
//! Scripts address a swarm by its entity id, never a member. The `swarm`
//! Lua table queues requests on the `SwarmBus`: spawn waves, damage spheres
//! and lines, and a new target. The swarm system (the GPU one in the engine,
//! `CpuSwarms` in the headless runner) takes them on its next step and
//! reports back each swarm's kills, attacks on its target and living
//! members. On the GPU those reports arrive a frame or two late.
//! `sim::process_swarm_reports` turns them into damage on the target and
//! `swarm.kills`, `swarm.hit` and `swarm.cleared` events.
//!
//! ```lua
//! swarm.spawn("goblins", 500, x, y, z, radius)
//! swarm.damage_sphere("goblins", x, y, z, radius, damage)
//! swarm.damage_line("goblins", x1, y1, z1, x2, y2, z2, radius, damage)
//! swarm.set_target("goblins", x, y, z)   -- or swarm.follow("goblins", "player")
//! local n = swarm.alive("goblins")
//! events.on("swarm.kills", function(e) ... end)   -- e.data.swarm, .count, .alive
//! events.on("swarm.hit", function(e) ... end)     -- e.data.swarm, .target, .hits, .damage
//! events.on("swarm.cleared", function(e) ... end) -- e.data.swarm
//! ```

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use glam::Vec3;
#[cfg(feature = "scripting")]
use mlua::prelude::*;

#[cfg(feature = "scripting")]
use crate::components::GpuSwarm;
#[cfg(feature = "scripting")]
type SharedSceneWorld = Rc<RefCell<crate::world::SceneWorld>>;
use crate::swarm::{DamageVolume, MAX_SPAWNS, MAX_VOLUMES};

pub type SharedSwarmBus = Rc<RefCell<SwarmBus>>;

/// Bring back up to `count` dead members in a disc around `center`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnOrder {
    pub center: Vec3,
    pub radius: f32,
    pub count: u32,
}

/// Where a script pointed a swarm, overriding its `target_entity`.
#[derive(Clone, Debug, PartialEq)]
pub enum SwarmTarget {
    Point(Vec3),
    Entity(String),
}

/// What one step of a swarm did, for events and damage.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmReport {
    pub swarm: String,
    /// Members killed since the last report.
    pub kills: u32,
    /// Attacks on the target since the last report, and the damage they deal.
    pub hits: u32,
    pub damage: f32,
    /// The entity the swarm was attacking, if its target is one.
    pub target: Option<String>,
    pub alive: u32,
    /// The last members died and no spawns are on the way.
    pub cleared: bool,
}

#[derive(Default)]
struct Orders {
    spawns: VecDeque<SpawnOrder>,
    volumes: VecDeque<DamageVolume>,
}

#[derive(Clone, Debug, Default)]
struct Status {
    capacity: u32,
    /// Living members as of `step`.
    alive: u32,
    step: u64,
    kills: u64,
    /// Spawn orders handed to steps after `step`: (step, members).
    in_flight: Vec<(u64, u32)>,
}

/// Requests from scripts waiting for a swarm's next step, and what the
/// steps reported.
#[derive(Default)]
pub struct SwarmBus {
    orders: HashMap<String, Orders>,
    targets: HashMap<String, SwarmTarget>,
    status: HashMap<String, Status>,
    reports: Vec<SwarmReport>,
}

impl SwarmBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn queue_spawn(&mut self, swarm: &str, order: SpawnOrder) {
        if order.count > 0 {
            self.orders
                .entry(swarm.to_string())
                .or_default()
                .spawns
                .push_back(order);
        }
    }

    pub fn queue_volume(&mut self, swarm: &str, volume: DamageVolume) {
        self.orders
            .entry(swarm.to_string())
            .or_default()
            .volumes
            .push_back(volume);
    }

    pub fn set_target(&mut self, swarm: &str, target: SwarmTarget) {
        self.targets.insert(swarm.to_string(), target);
    }

    pub fn target(&self, swarm: &str) -> Option<&SwarmTarget> {
        self.targets.get(swarm)
    }

    /// A swarm was created (or re-created): it starts with `alive` of
    /// `capacity` members. Orders queued before then are kept.
    pub fn register(&mut self, swarm: &str, capacity: u32, alive: u32) {
        self.status.insert(
            swarm.to_string(),
            Status {
                capacity,
                alive,
                ..Status::default()
            },
        );
    }

    /// Hand step `step` of a swarm as many of its orders as one step takes;
    /// the rest wait for the next.
    pub fn take_orders(&mut self, swarm: &str, step: u64) -> (Vec<SpawnOrder>, Vec<DamageVolume>) {
        let Some(orders) = self.orders.get_mut(swarm) else {
            return (Vec::new(), Vec::new());
        };
        let spawns: Vec<SpawnOrder> = orders
            .spawns
            .drain(..orders.spawns.len().min(MAX_SPAWNS))
            .collect();
        let volumes: Vec<DamageVolume> = orders
            .volumes
            .drain(..orders.volumes.len().min(MAX_VOLUMES))
            .collect();
        let requested: u32 = spawns.iter().map(|s| s.count).sum();
        if requested > 0 {
            if let Some(status) = self.status.get_mut(swarm) {
                status.in_flight.push((step, requested));
            }
        }
        (spawns, volumes)
    }

    /// Record what step `step` of a swarm did.
    #[allow(clippy::too_many_arguments)]
    pub fn report(
        &mut self,
        swarm: &str,
        step: u64,
        kills: u32,
        hits: u32,
        damage: f32,
        target: Option<String>,
        alive: u32,
    ) {
        let queued = self.queued_spawns(swarm);
        let status = self.status.entry(swarm.to_string()).or_default();
        let was_alive = status.alive;
        status.alive = alive;
        status.step = step;
        status.kills += u64::from(kills);
        status.in_flight.retain(|&(s, _)| s > step);
        let on_the_way = queued + status.in_flight.iter().map(|&(_, n)| n).sum::<u32>();
        self.reports.push(SwarmReport {
            swarm: swarm.to_string(),
            kills,
            hits,
            damage,
            target,
            alive,
            cleared: was_alive > 0 && alive == 0 && on_the_way == 0,
        });
    }

    fn queued_spawns(&self, swarm: &str) -> u32 {
        self.orders
            .get(swarm)
            .map(|o| o.spawns.iter().map(|s| s.count).sum())
            .unwrap_or(0)
    }

    /// Living members: the last report, plus spawns asked for since, at
    /// most the swarm's size. Kills since the last report are not counted.
    /// Before a swarm's first step, it has `initial` of `capacity`.
    pub fn alive(&self, swarm: &str, initial: u32, capacity: u32) -> u32 {
        let (alive, capacity, in_flight) = match self.status.get(swarm) {
            Some(s) => (
                s.alive,
                s.capacity,
                s.in_flight.iter().map(|&(_, n)| n).sum::<u32>(),
            ),
            None => (initial, capacity, 0),
        };
        let coming = self.queued_spawns(swarm) + in_flight;
        alive.saturating_add(coming).min(capacity)
    }

    pub fn capacity(&self, swarm: &str) -> Option<u32> {
        self.status.get(swarm).map(|s| s.capacity)
    }

    /// Members killed so far, as reported.
    pub fn kills(&self, swarm: &str) -> u64 {
        self.status.get(swarm).map(|s| s.kills).unwrap_or(0)
    }

    pub fn take_reports(&mut self) -> Vec<SwarmReport> {
        std::mem::take(&mut self.reports)
    }
}

/// The swarm's size and how many start alive, or an error naming `function`
/// when `id` is not a swarm.
#[cfg(feature = "scripting")]
fn swarm_def(scene_world: &SharedSceneWorld, function: &str, id: &str) -> LuaResult<(u32, u32)> {
    let sw = scene_world.borrow();
    sw.entity_registry
        .get(id)
        .and_then(|&entity| sw.world.get::<&GpuSwarm>(entity).ok())
        .map(|swarm| (swarm.def.count, swarm.def.initial_count()))
        .ok_or_else(|| LuaError::RuntimeError(format!("swarm.{function}: no swarm with id '{id}'")))
}

#[cfg(feature = "scripting")]
fn finite(function: &str, values: &[f32]) -> LuaResult<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(LuaError::RuntimeError(format!(
            "swarm.{function}: arguments must be finite numbers"
        )))
    }
}

#[cfg(feature = "scripting")]
type SpawnArgs = (String, f64, f32, f32, f32, f32);
#[cfg(feature = "scripting")]
type SphereArgs = (String, f32, f32, f32, f32, f32);
#[cfg(feature = "scripting")]
type LineArgs = (String, f32, f32, f32, f32, f32, f32, f32, f32);

/// Register the `swarm` table in `lua`.
#[cfg(feature = "scripting")]
pub fn register_swarm_api(
    lua: &Lua,
    bus: SharedSwarmBus,
    scene_world: SharedSceneWorld,
) -> Result<(), String> {
    let table = lua.create_table().map_err(|e| e.to_string())?;
    let set = |name: &str, f: LuaFunction| table.set(name, f).map_err(|e| e.to_string());

    // swarm.spawn(id, count, x, y, z, radius)
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "spawn",
        lua.create_function(move |_, (id, count, x, y, z, radius): SpawnArgs| {
            let (capacity, _) = swarm_def(&sw, "spawn", &id)?;
            finite("spawn", &[x, y, z, radius, count as f32])?;
            let count = count.max(0.0).round().min(f64::from(capacity)) as u32;
            b.borrow_mut().queue_spawn(
                &id,
                SpawnOrder {
                    center: Vec3::new(x, y, z),
                    radius: radius.max(0.0),
                    count,
                },
            );
            Ok(())
        })
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.damage_sphere(id, x, y, z, radius, damage)
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "damage_sphere",
        lua.create_function(move |_, (id, x, y, z, radius, damage): SphereArgs| {
            swarm_def(&sw, "damage_sphere", &id)?;
            finite("damage_sphere", &[x, y, z, radius, damage])?;
            b.borrow_mut().queue_volume(
                &id,
                DamageVolume::sphere(Vec3::new(x, y, z), radius, damage),
            );
            Ok(())
        })
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.damage_line(id, x1, y1, z1, x2, y2, z2, radius, damage): hurts
    // every member within `radius` of the segment, like a piercing shot.
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "damage_line",
        lua.create_function(
            move |_, (id, x1, y1, z1, x2, y2, z2, radius, damage): LineArgs| {
                swarm_def(&sw, "damage_line", &id)?;
                finite("damage_line", &[x1, y1, z1, x2, y2, z2, radius, damage])?;
                let volume = DamageVolume::line(
                    Vec3::new(x1, y1, z1),
                    Vec3::new(x2, y2, z2),
                    radius,
                    damage,
                );
                b.borrow_mut().queue_volume(&id, volume);
                Ok(())
            },
        )
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.set_target(id, x, y, z)
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "set_target",
        lua.create_function(move |_, (id, x, y, z): (String, f32, f32, f32)| {
            swarm_def(&sw, "set_target", &id)?;
            finite("set_target", &[x, y, z])?;
            b.borrow_mut()
                .set_target(&id, SwarmTarget::Point(Vec3::new(x, y, z)));
            Ok(())
        })
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.follow(id, entity_id): chase (and attack) that entity.
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "follow",
        lua.create_function(move |_, (id, target): (String, String)| {
            swarm_def(&sw, "follow", &id)?;
            if !sw.borrow().entity_registry.contains_key(&target) {
                return Err(LuaError::RuntimeError(format!(
                    "swarm.follow: no entity with id '{target}'"
                )));
            }
            b.borrow_mut().set_target(&id, SwarmTarget::Entity(target));
            Ok(())
        })
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.alive(id) -> living members (as of the last report, plus spawns since)
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "alive",
        lua.create_function(move |_, id: String| {
            let (count, initial) = swarm_def(&sw, "alive", &id)?;
            Ok(b.borrow().alive(&id, initial, count))
        })
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.capacity(id) -> members the swarm can hold
    let (b, sw) = (bus.clone(), scene_world.clone());
    set(
        "capacity",
        lua.create_function(move |_, id: String| {
            let (count, _) = swarm_def(&sw, "capacity", &id)?;
            Ok(b.borrow().capacity(&id).unwrap_or(count))
        })
        .map_err(|e| e.to_string())?,
    )?;

    // swarm.kills(id) -> members killed so far
    let (b, sw) = (bus, scene_world);
    set(
        "kills",
        lua.create_function(move |_, id: String| {
            swarm_def(&sw, "kills", &id)?;
            Ok(b.borrow().kills(&id))
        })
        .map_err(|e| e.to_string())?,
    )?;

    lua.globals().set("swarm", table).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(count: u32) -> SpawnOrder {
        SpawnOrder {
            center: Vec3::ZERO,
            radius: 5.0,
            count,
        }
    }

    #[test]
    fn alive_counts_spawns_that_have_not_been_reported_yet() {
        let mut bus = SwarmBus::new();
        // Before the swarm's first step: its initial members, plus what is queued.
        assert_eq!(bus.alive("g", 10, 100), 10);
        bus.queue_spawn("g", order(5));
        assert_eq!(bus.alive("g", 10, 100), 15);

        let mut bus = SwarmBus::new();
        bus.register("g", 100, 0);
        assert_eq!(bus.alive("g", 0, 100), 0);

        bus.queue_spawn("g", order(30));
        assert_eq!(bus.alive("g", 0, 100), 30, "queued spawns count");
        bus.take_orders("g", 1);
        assert_eq!(
            bus.alive("g", 0, 100),
            30,
            "spawns handed to a step count until it reports"
        );

        // The GPU reports step 1 two frames later; step 2 has spawned more by then.
        bus.queue_spawn("g", order(90));
        bus.take_orders("g", 2);
        assert_eq!(
            bus.alive("g", 0, 100),
            100,
            "never more than the swarm holds"
        );
        bus.report("g", 1, 0, 0, 0.0, None, 30);
        assert_eq!(bus.alive("g", 0, 100), 100);
        bus.report("g", 2, 0, 0, 0.0, None, 100);
        assert_eq!(bus.alive("g", 0, 100), 100);
    }

    #[test]
    fn cleared_only_when_the_last_members_die_and_none_are_coming() {
        let mut bus = SwarmBus::new();
        bus.register("g", 100, 10);
        bus.report("g", 1, 4, 0, 0.0, None, 6);
        bus.report("g", 2, 6, 0, 0.0, None, 0);
        let reports = bus.take_reports();
        assert_eq!(
            reports.iter().map(|r| r.cleared).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(bus.kills("g"), 10);

        // The last member dies in step 4, but step 5 already spawned a wave
        // (the GPU reports late): not cleared.
        bus.report("g", 3, 0, 0, 0.0, None, 1);
        bus.queue_spawn("g", order(5));
        bus.take_orders("g", 5);
        bus.report("g", 4, 1, 0, 0.0, None, 0);
        assert!(
            !bus.take_reports()[1].cleared,
            "a wave handed to a later step is still coming"
        );
        bus.report("g", 5, 0, 0, 0.0, None, 5);
        bus.report("g", 6, 5, 0, 0.0, None, 0);
        assert!(bus.take_reports()[1].cleared);
    }

    #[test]
    fn a_step_takes_at_most_its_share_of_orders() {
        let mut bus = SwarmBus::new();
        bus.register("g", 100, 0);
        for _ in 0..MAX_VOLUMES + 5 {
            bus.queue_volume("g", DamageVolume::sphere(Vec3::ZERO, 1.0, 1.0));
        }
        for _ in 0..MAX_SPAWNS + 2 {
            bus.queue_spawn("g", order(1));
        }
        let (spawns, volumes) = bus.take_orders("g", 1);
        assert_eq!((spawns.len(), volumes.len()), (MAX_SPAWNS, MAX_VOLUMES));
        let (spawns, volumes) = bus.take_orders("g", 2);
        assert_eq!((spawns.len(), volumes.len()), (2, 5));
    }
}
