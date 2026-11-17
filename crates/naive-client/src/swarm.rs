//! Native GPU swarm rendering over the shared CPU simulation contracts.

mod gpu;

pub use gpu::{encode_steps, SwarmBuffers, SwarmDraw, SwarmPipelines, SwarmSystem};
pub use naive_sim::swarm::{
    blocked_cells, plane_obstacles, solve_flow_field, spawn_members, static_boxes, step_swarm,
    CpuSwarms, DamageVolume, Member, Obstacle, Region, SpawnRequest, SwarmParams, ATTACK_STOP,
    CELL_CAPACITY, FIELD_CELLS, HIT_FLASH, MAX_SPAWNS, MAX_VOLUMES, STAT_ALIVE, STAT_ATTACKS,
    STAT_CLAIMS, STAT_KILLS,
};

use naive_sim::swarm::{swarm_id, SwarmCore};

const WORKGROUP_SIZE: u32 = 64;
/// The compute shaders: `bin_main` then `steer_main`, one invocation per member.
pub const SWARM_COMPUTE_WGSL: &str = include_str!("swarm/compute.wgsl");

#[cfg(test)]
mod tests;
