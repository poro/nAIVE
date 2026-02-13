use std::collections::HashMap;

use glam::{Quat, Vec3};
use rapier3d::prelude::*;
use rapier3d::control::{KinematicCharacterController, CharacterAutostep, CharacterLength};

use crate::components::Transform;

/// Rigid body component attached to entities.
#[derive(Debug, Clone)]
pub struct RigidBody {
    pub handle: RigidBodyHandle,
    pub body_type: PhysicsBodyType,
}

/// Collider component attached to entities.
#[derive(Debug, Clone)]
pub struct Collider {
    pub handle: ColliderHandle,
    pub shape: PhysicsShape,
    pub is_trigger: bool,
}

/// Character controller marker component.
#[derive(Debug, Clone)]
pub struct CharacterController {
    pub move_speed: f32,
    pub sprint_multiplier: f32,
    pub jump_impulse: f32,
    pub grounded: bool,
    pub step_height: f32,
    pub max_slope_angle: f32,
    pub velocity: Vec3,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            move_speed: 5.0,
            sprint_multiplier: 1.8,
            jump_impulse: 7.0,
            grounded: false,
            step_height: 0.3,
            max_slope_angle: 45.0_f32.to_radians(),
            velocity: Vec3::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PhysicsBodyType {
    Dynamic,
    Static,
    Kinematic,
}

#[derive(Debug, Clone)]
pub enum PhysicsShape {
    Box { half_extents: Vec3 },
    Sphere { radius: f32 },
    Capsule { half_height: f32, radius: f32 },
    Trimesh,
}

/// Collision event emitted when two colliders touch.
#[derive(Debug, Clone)]
pub struct CollisionEvent {
    pub entity_a: hecs::Entity,
    pub entity_b: hecs::Entity,
    pub started: bool,
}

/// Trigger event emitted when an entity enters/exits a trigger volume.
#[derive(Debug, Clone)]
pub struct TriggerEvent {
    pub trigger_entity: hecs::Entity,
    pub other_entity: hecs::Entity,
    pub entered: bool,
}

/// Central physics world state.
pub struct PhysicsWorld {
    pub gravity: Vec3,
    pub rigid_body_set: RigidBodySet,
    pub collider_set: ColliderSet,
    pub integration_params: IntegrationParameters,
    pub physics_pipeline: PhysicsPipeline,
    pub island_manager: IslandManager,
    pub broad_phase: DefaultBroadPhase,
    pub narrow_phase: NarrowPhase,
    pub impulse_joint_set: ImpulseJointSet,
    pub multibody_joint_set: MultibodyJointSet,
    pub ccd_solver: CCDSolver,
    pub query_pipeline: QueryPipeline,

    // Mapping from Rapier handles to ECS entities
    pub body_to_entity: HashMap<RigidBodyHandle, hecs::Entity>,
    pub collider_to_entity: HashMap<ColliderHandle, hecs::Entity>,

    // Events from this frame
    pub collision_events: Vec<CollisionEvent>,
    pub trigger_events: Vec<TriggerEvent>,

    // Character controller
    pub character_controller: KinematicCharacterController,
}

impl PhysicsWorld {
    pub fn new(gravity: Vec3) -> Self {
        let mut character_controller = KinematicCharacterController::default();
        character_controller.max_slope_climb_angle = 45.0_f32.to_radians();
        character_controller.min_slope_slide_angle = 30.0_f32.to_radians();
        character_controller.autostep = Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(0.3),
            min_width: CharacterLength::Absolute(0.2),
            include_dynamic_bodies: false,
        });
        character_controller.snap_to_ground = Some(CharacterLength::Absolute(0.1));

        Self {
            gravity,
            rigid_body_set: RigidBodySet::new(),
            collider_set: ColliderSet::new(),
            integration_params: IntegrationParameters::default(),
            physics_pipeline: PhysicsPipeline::new(),
            island_manager: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            impulse_joint_set: ImpulseJointSet::new(),
            multibody_joint_set: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            query_pipeline: QueryPipeline::new(),
            body_to_entity: HashMap::new(),
            collider_to_entity: HashMap::new(),
            collision_events: Vec::new(),
            trigger_events: Vec::new(),
            character_controller,
        }
    }

    /// Add a static rigid body + collider (e.g., a wall or floor).
    pub fn add_static_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        rotation: Quat,
        shape: PhysicsShape,
        is_trigger: bool,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let rb = RigidBodyBuilder::fixed()
            .translation(vector![position.x, position.y, position.z])
            .rotation(quat_to_angvector(rotation))
            .build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider_builder = shape_to_collider(&shape);
        let collider = if is_trigger {
            collider_builder.sensor(true).build()
        } else {
            collider_builder.build()
        };
        let col_handle =
            self.collider_set
                .insert_with_parent(collider, rb_handle, &mut self.rigid_body_set);

        self.body_to_entity.insert(rb_handle, entity);
        self.collider_to_entity.insert(col_handle, entity);

        (rb_handle, col_handle)
    }

    /// Add a dynamic rigid body + collider.
    pub fn add_dynamic_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        rotation: Quat,
        shape: PhysicsShape,
        mass: f32,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let rb = RigidBodyBuilder::dynamic()
            .translation(vector![position.x, position.y, position.z])
            .rotation(quat_to_angvector(rotation))
            .build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider = shape_to_collider(&shape)
            .mass(mass)
            .build();
        let col_handle =
            self.collider_set
                .insert_with_parent(collider, rb_handle, &mut self.rigid_body_set);

        self.body_to_entity.insert(rb_handle, entity);
        self.collider_to_entity.insert(col_handle, entity);

        (rb_handle, col_handle)
    }

    /// Add a kinematic body for the character controller.
    pub fn add_character_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        half_height: f32,
        radius: f32,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let rb = RigidBodyBuilder::kinematic_position_based()
            .translation(vector![position.x, position.y, position.z])
            .build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider = ColliderBuilder::capsule_y(half_height, radius)
            .build();
        let col_handle =
            self.collider_set
                .insert_with_parent(collider, rb_handle, &mut self.rigid_body_set);

        self.body_to_entity.insert(rb_handle, entity);
        self.collider_to_entity.insert(col_handle, entity);

        (rb_handle, col_handle)
    }

    /// Step the physics simulation.
    pub fn step(&mut self, dt: f32) {
        self.integration_params.dt = dt;
        let gravity = vector![self.gravity.x, self.gravity.y, self.gravity.z];

        self.physics_pipeline.step(
            &gravity,
            &self.integration_params,
            &mut self.island_manager,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.rigid_body_set,
            &mut self.collider_set,
            &mut self.impulse_joint_set,
            &mut self.multibody_joint_set,
            &mut self.ccd_solver,
            Some(&mut self.query_pipeline),
            &(),
            &(),
        );

        // Collect collision events from narrow phase
        self.collision_events.clear();
        self.trigger_events.clear();
    }

    /// Move a character controller and return the effective movement.
    pub fn move_character(
        &mut self,
        rb_handle: RigidBodyHandle,
        col_handle: ColliderHandle,
        desired_movement: Vec3,
        dt: f32,
    ) -> (Vec3, bool) {
        let body = &self.rigid_body_set[rb_handle];
        let collider = &self.collider_set[col_handle];

        let movement = self.character_controller.move_shape(
            dt,
            &self.rigid_body_set,
            &self.collider_set,
            &self.query_pipeline,
            collider.shape(),
            body.position(),
            vector![desired_movement.x, desired_movement.y, desired_movement.z],
            QueryFilter::default().exclude_rigid_body(rb_handle),
            |_| {},
        );

        let grounded = movement.grounded;
        let effective = Vec3::new(
            movement.translation.x,
            movement.translation.y,
            movement.translation.z,
        );

        // Apply the movement to the rigid body
        let current_pos = body.position().translation;
        let new_pos = vector![
            current_pos.x + effective.x,
            current_pos.y + effective.y,
            current_pos.z + effective.z
        ];

        if let Some(body) = self.rigid_body_set.get_mut(rb_handle) {
            let mut new_iso = *body.position();
            new_iso.translation = new_pos.into();
            body.set_next_kinematic_position(new_iso);
        }

        (effective, grounded)
    }

    /// Sync physics body positions back to ECS transforms.
    pub fn sync_to_ecs(&self, world: &mut hecs::World) {
        for (rb_handle, &entity) in &self.body_to_entity {
            if let Some(body) = self.rigid_body_set.get(*rb_handle) {
                if let Ok(mut transform) = world.get::<&mut Transform>(entity) {
                    let pos = body.position().translation;
                    let rot = body.position().rotation;
                    transform.position = Vec3::new(pos.x, pos.y, pos.z);
                    transform.rotation = Quat::from_xyzw(rot.i, rot.j, rot.k, rot.w);
                    transform.dirty = true;
                }
            }
        }
    }

    /// Cast a ray and return the first hit.
    pub fn raycast(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<(hecs::Entity, f32, Vec3)> {
        let ray = Ray::new(
            point![origin.x, origin.y, origin.z],
            vector![direction.x, direction.y, direction.z],
        );

        if let Some((handle, intersection)) = self.query_pipeline.cast_ray_and_get_normal(
            &self.rigid_body_set,
            &self.collider_set,
            &ray,
            max_distance,
            true,
            QueryFilter::default(),
        ) {
            if let Some(&entity) = self.collider_to_entity.get(&handle) {
                let normal = Vec3::new(
                    intersection.normal.x,
                    intersection.normal.y,
                    intersection.normal.z,
                );
                return Some((entity, intersection.time_of_impact, normal));
            }
        }
        None
    }

    /// Remove a body and its colliders.
    pub fn remove_body(&mut self, rb_handle: RigidBodyHandle) {
        self.rigid_body_set.remove(
            rb_handle,
            &mut self.island_manager,
            &mut self.collider_set,
            &mut self.impulse_joint_set,
            &mut self.multibody_joint_set,
            true,
        );
        self.body_to_entity.remove(&rb_handle);
    }
}

fn shape_to_collider(shape: &PhysicsShape) -> ColliderBuilder {
    match shape {
        PhysicsShape::Box { half_extents } => {
            ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z)
        }
        PhysicsShape::Sphere { radius } => ColliderBuilder::ball(*radius),
        PhysicsShape::Capsule {
            half_height,
            radius,
        } => ColliderBuilder::capsule_y(*half_height, *radius),
        PhysicsShape::Trimesh => {
            // Fallback to unit box for unsupported trimesh
            ColliderBuilder::cuboid(0.5, 0.5, 0.5)
        }
    }
}

fn quat_to_angvector(q: Quat) -> rapier3d::na::Vector3<f32> {
    let (axis, angle) = q.to_axis_angle();
    vector![axis.x * angle, axis.y * angle, axis.z * angle]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_physics_world_creation() {
        let pw = PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0));
        assert_eq!(pw.rigid_body_set.len(), 0);
        assert_eq!(pw.collider_set.len(), 0);
    }

    #[test]
    fn test_add_static_body() {
        let mut world = hecs::World::new();
        let entity = world.spawn(());
        let mut pw = PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0));

        let (rb, col) = pw.add_static_body(
            entity,
            Vec3::ZERO,
            Quat::IDENTITY,
            PhysicsShape::Box {
                half_extents: Vec3::new(10.0, 0.1, 10.0),
            },
            false,
        );

        assert_eq!(pw.rigid_body_set.len(), 1);
        assert_eq!(pw.collider_set.len(), 1);
        assert_eq!(pw.body_to_entity[&rb], entity);
        assert_eq!(pw.collider_to_entity[&col], entity);
    }

    #[test]
    fn test_raycast() {
        let mut world = hecs::World::new();
        let entity = world.spawn(());
        let mut pw = PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0));

        // Add a floor
        pw.add_static_body(
            entity,
            Vec3::new(0.0, -1.0, 0.0),
            Quat::IDENTITY,
            PhysicsShape::Box {
                half_extents: Vec3::new(10.0, 0.5, 10.0),
            },
            false,
        );

        // Update query pipeline
        pw.query_pipeline.update(&pw.collider_set);

        // Raycast down
        let result = pw.raycast(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0), 100.0);
        assert!(result.is_some());
        let (hit_entity, distance, _normal) = result.unwrap();
        assert_eq!(hit_entity, entity);
        assert!(distance > 0.0);
    }
}
