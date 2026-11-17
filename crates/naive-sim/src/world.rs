//! Scene state, CPU entity construction and lifecycle command contracts.

mod commands;
mod construction;
mod pools;
mod spawning;

pub use commands::{
    DynamicSpawnCommand, EntityCommandQueue, MeshCreateCommand, PoolOp, ProjectileSpawnCommand,
    SpawnColliderSpec, SpawnCommand, TextureSwapCommand,
};
pub use construction::{preflight_scene, spawn_entity_with_resolver, SpawnAssetResolver};
pub use pools::EntityPoolManager;
pub use spawning::{
    destroy_runtime_entity, finish_dynamic_spawn, finish_projectile_spawn,
    spawn_entity_headless_with_assets,
};
#[cfg(any(test, feature = "test-support"))]
pub use spawning::{spawn_all_entities_headless, spawn_entity_headless};

use std::collections::HashMap;

use hecs::World;

use crate::physics::PhysicsShape;
use crate::scene::{EntityDef, SceneFile};

/// Authoring information that cannot be recovered from opaque GPU/physics
/// handles. Keeping it on the ECS entity gives it the same lifetime as the
/// entity, including scene clears and structural reloads.
#[derive(Clone)]
pub struct SceneEntityDefinition(pub EntityDef);

/// Runtime edits without a scene-file representation must not disappear when
/// the editor saves the scene. The operation that creates such state records
/// its reason here, and the snapshot reports it to the caller.
pub struct UnsavedRuntimeState(pub String);

/// Central scene state: the ECS world plus entity name registry.
pub struct SceneWorld {
    pub world: World,
    /// Maps YAML entity IDs to hecs Entity handles.
    pub entity_registry: HashMap<String, hecs::Entity>,
    /// The currently loaded scene file (for hot-reload diffing).
    pub current_scene: Option<SceneFile>,
}

impl SceneWorld {
    pub fn new() -> Self {
        Self {
            world: World::new(),
            entity_registry: HashMap::new(),
            current_scene: None,
        }
    }
}

impl Default for SceneWorld {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a shape from a collider definition.
/// Trimesh data starts empty; the caller must resolve it through an asset resolver.
pub fn parse_collider_shape(col_def: &crate::scene::ColliderDef) -> PhysicsShape {
    match col_def.shape.as_str() {
        "sphere" => PhysicsShape::Sphere {
            radius: col_def.radius.unwrap_or(0.5),
        },
        "capsule" => PhysicsShape::Capsule {
            half_height: col_def.half_height.unwrap_or(0.5),
            radius: col_def.radius.unwrap_or(0.3),
        },
        "trimesh" => PhysicsShape::Trimesh {
            vertices: Vec::new(),
            indices: Vec::new(),
        },
        _ => {
            let he = col_def.half_extents.unwrap_or([0.5, 0.5, 0.5]);
            PhysicsShape::Box {
                half_extents: glam::Vec3::from(he),
            }
        }
    }
}

/// Convert Euler degrees [pitch, yaw, roll] to a Quaternion.
pub fn euler_degrees_to_quat(euler: [f32; 3]) -> glam::Quat {
    let [pitch, yaw, roll] = euler;
    glam::Quat::from_euler(
        glam::EulerRot::YXZ,
        yaw.to_radians(),
        pitch.to_radians(),
        roll.to_radians(),
    )
}
