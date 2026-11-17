use std::collections::{HashMap, HashSet};

use glam::{Quat, Vec3};
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;

use crate::components::Transform;

mod cooking;
#[cfg(test)]
mod cooking_tests;
pub use cooking::ColliderCookingStats;

// rapier does its math with its own copy of glam (`rapier3d::math`), a
// different version from the engine's, so values cross over through these.

/// Engine vector to rapier vector.
pub fn to_rapier_vec(v: Vec3) -> rapier3d::math::Vector {
    rapier3d::math::Vector::new(v.x, v.y, v.z)
}

/// Rapier vector to engine vector.
pub fn from_rapier_vec(v: rapier3d::math::Vector) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

/// Engine rotation to rapier rotation.
pub fn to_rapier_rot(q: Quat) -> rapier3d::math::Rotation {
    rapier3d::math::Rotation::from_xyzw(q.x, q.y, q.z, q.w)
}

/// Rapier rotation to engine rotation.
pub fn from_rapier_rot(q: rapier3d::math::Rotation) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// A rapier pose (position and rotation).
pub fn rapier_pose(position: Vec3, rotation: Quat) -> rapier3d::math::Pose {
    rapier3d::math::Pose::from_parts(to_rapier_vec(position), to_rapier_rot(rotation))
}

/// Length of one physics step, in seconds. The simulation only ever advances
/// in steps of exactly this size, so its results do not depend on the frame
/// rate. 120 Hz gives one step per frame on a 120 Hz display and two on a
/// 60 Hz display.
pub const FIXED_TIMESTEP: f32 = 1.0 / 120.0;

/// Most fixed steps run for a single frame. When a frame takes longer than
/// this many steps, the backlog is dropped: the simulation slows down instead
/// of falling ever further behind.
pub const MAX_STEPS_PER_FRAME: u32 = 8;

/// CPU vertices and triangle indices supplied to collider construction.
pub type PhysicsTrimesh = (Vec<rapier3d::math::Vector>, Vec<[u32; 3]>);

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
    Box {
        half_extents: Vec3,
    },
    Sphere {
        radius: f32,
    },
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Triangle mesh collider built from GLB vertex/index data.
    Trimesh {
        vertices: Vec<rapier3d::math::Vector>,
        indices: Vec<[u32; 3]>,
    },
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
    pub soft_body_set: SoftBodySet,

    // Mapping from Rapier handles to ECS entities
    pub body_to_entity: HashMap<RigidBodyHandle, hecs::Entity>,
    pub collider_to_entity: HashMap<ColliderHandle, hecs::Entity>,

    // Events from this frame
    pub collision_events: Vec<CollisionEvent>,
    pub trigger_events: Vec<TriggerEvent>,

    // Track active contact pairs from the previous step to detect new-only contacts
    active_contact_pairs: HashSet<[ColliderHandle; 2]>,

    // Directed sensor/other pairs, retaining entity handles until exit even
    // when a collider has been removed. Two sensors produce two directed pairs.
    active_trigger_pairs: HashMap<[ColliderHandle; 2], (hecs::Entity, hecs::Entity)>,

    // Frame time not yet consumed by fixed steps
    accumulator: f32,

    // Character controller
    pub character_controller: KinematicCharacterController,

    cooked_shapes: cooking::ColliderShapeCache,
}

impl PhysicsWorld {
    pub fn new(gravity: Vec3) -> Self {
        let character_controller = KinematicCharacterController {
            max_slope_climb_angle: 45.0_f32.to_radians(),
            min_slope_slide_angle: 30.0_f32.to_radians(),
            autostep: Some(CharacterAutostep {
                max_height: CharacterLength::Absolute(0.3),
                min_width: CharacterLength::Absolute(0.2),
                include_dynamic_bodies: false,
            }),
            snap_to_ground: Some(CharacterLength::Absolute(0.1)),
            ..Default::default()
        };

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
            soft_body_set: SoftBodySet::new(),
            body_to_entity: HashMap::new(),
            collider_to_entity: HashMap::new(),
            collision_events: Vec::new(),
            trigger_events: Vec::new(),
            active_contact_pairs: HashSet::new(),
            active_trigger_pairs: HashMap::new(),
            // Start half a step in. When frames arrive at a whole number of
            // steps apart, timing jitter then cannot flip a frame between one
            // step too few and one too many.
            accumulator: FIXED_TIMESTEP * 0.5,
            character_controller,
            cooked_shapes: cooking::ColliderShapeCache::default(),
        }
    }

    /// Add a static rigid body + collider (e.g., a wall or floor).
    #[allow(clippy::too_many_arguments)]
    pub fn add_static_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        rotation: Quat,
        shape: PhysicsShape,
        is_trigger: bool,
        restitution: f32,
        friction: f32,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let rb = RigidBodyBuilder::fixed()
            .translation(to_rapier_vec(position))
            .rotation(quat_to_angvector(rotation))
            .build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider_builder = self
            .shape_to_collider(&shape)
            .restitution(restitution)
            .friction(friction);
        let collider = if is_trigger {
            collider_builder
                .sensor(true)
                .active_collision_types(ActiveCollisionTypes::all())
                .build()
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

    /// Add a kinematic rigid body + collider (moved by game code, not physics).
    #[allow(clippy::too_many_arguments)]
    pub fn add_kinematic_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        rotation: Quat,
        shape: PhysicsShape,
        is_trigger: bool,
        restitution: f32,
        friction: f32,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let rb = RigidBodyBuilder::kinematic_position_based()
            .translation(to_rapier_vec(position))
            .rotation(quat_to_angvector(rotation))
            .build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider_builder = self
            .shape_to_collider(&shape)
            .restitution(restitution)
            .friction(friction);
        let collider = if is_trigger {
            collider_builder
                .sensor(true)
                .active_collision_types(ActiveCollisionTypes::all())
                .build()
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
    #[allow(clippy::too_many_arguments)]
    pub fn add_dynamic_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        rotation: Quat,
        shape: PhysicsShape,
        mass: f32,
        restitution: f32,
        friction: f32,
        ccd: bool,
    ) -> (RigidBodyHandle, ColliderHandle) {
        self.add_dynamic_body_with_sensor(
            entity,
            position,
            rotation,
            shape,
            mass,
            restitution,
            friction,
            ccd,
            false,
        )
    }

    /// Add a dynamic body, optionally using a nonblocking trigger collider.
    #[allow(clippy::too_many_arguments)]
    pub fn add_dynamic_body_with_sensor(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        rotation: Quat,
        shape: PhysicsShape,
        mass: f32,
        restitution: f32,
        friction: f32,
        ccd: bool,
        is_trigger: bool,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let mut rb_builder = RigidBodyBuilder::dynamic()
            .translation(to_rapier_vec(position))
            .rotation(quat_to_angvector(rotation));
        if ccd {
            rb_builder = rb_builder.ccd_enabled(true);
        }
        let rb = rb_builder.build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider_builder = self
            .shape_to_collider(&shape)
            .mass(mass)
            .restitution(restitution)
            .friction(friction);
        let collider = if is_trigger {
            collider_builder
                .sensor(true)
                .active_collision_types(ActiveCollisionTypes::all())
                .build()
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

    /// Add a kinematic body for the character controller.
    pub fn add_character_body(
        &mut self,
        entity: hecs::Entity,
        position: Vec3,
        half_height: f32,
        radius: f32,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let rb = RigidBodyBuilder::kinematic_position_based()
            .translation(to_rapier_vec(position))
            .build();
        let rb_handle = self.rigid_body_set.insert(rb);

        let collider = ColliderBuilder::capsule_y(half_height, radius).build();
        let col_handle =
            self.collider_set
                .insert_with_parent(collider, rb_handle, &mut self.rigid_body_set);

        self.body_to_entity.insert(rb_handle, entity);
        self.collider_to_entity.insert(col_handle, entity);

        (rb_handle, col_handle)
    }

    /// Start a frame that lasted `frame_dt` seconds: forget the previous
    /// frame's events and return how many fixed steps to run for this one.
    /// Call [`step_fixed`](Self::step_fixed) that many times.
    pub fn begin_frame(&mut self, frame_dt: f32) -> u32 {
        self.collision_events.clear();
        self.trigger_events.clear();

        self.accumulator += frame_dt.max(0.0);
        let due = (self.accumulator / FIXED_TIMESTEP).floor() as u32;
        if due > MAX_STEPS_PER_FRAME {
            self.accumulator = FIXED_TIMESTEP * 0.5;
            return MAX_STEPS_PER_FRAME;
        }
        self.accumulator -= due as f32 * FIXED_TIMESTEP;
        due
    }

    /// Advance the simulation by one fixed step. Events are added to this
    /// frame's lists rather than replacing them, so none are lost when a
    /// frame runs several steps.
    pub fn step_fixed(&mut self) {
        self.integration_params.dt = FIXED_TIMESTEP;
        self.physics_pipeline.step(
            to_rapier_vec(self.gravity),
            &self.integration_params,
            &mut self.island_manager,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.rigid_body_set,
            &mut self.collider_set,
            &mut self.impulse_joint_set,
            &mut self.multibody_joint_set,
            &mut self.soft_body_set,
            &mut self.ccd_solver,
            &(),
            &(),
        );

        // Collect collision events from narrow phase — only newly started contacts
        let mut current_pairs = HashSet::new();
        for pair in self.narrow_phase.contact_pairs() {
            if pair.has_any_active_contact() {
                let key = [pair.collider1, pair.collider2];
                current_pairs.insert(key);

                // Only emit if this pair was NOT active last step (new contact)
                if !self.active_contact_pairs.contains(&key) {
                    let entity_a = self.collider_to_entity.get(&pair.collider1).copied();
                    let entity_b = self.collider_to_entity.get(&pair.collider2).copied();
                    if let (Some(a), Some(b)) = (entity_a, entity_b) {
                        self.collision_events.push(CollisionEvent {
                            entity_a: a,
                            entity_b: b,
                            started: true,
                        });
                    }
                }
            }
        }
        self.active_contact_pairs = current_pairs;

        let mut current_triggers = HashMap::new();
        for (a, b, intersecting) in self.narrow_phase.intersection_pairs() {
            if !intersecting {
                continue;
            }
            for (sensor, other) in [(a, b), (b, a)] {
                if !self.collider_set[sensor].is_sensor() {
                    continue;
                }
                let (Some(&trigger_entity), Some(&other_entity)) = (
                    self.collider_to_entity.get(&sensor),
                    self.collider_to_entity.get(&other),
                ) else {
                    continue;
                };
                let key = [sensor, other];
                current_triggers.insert(key, (trigger_entity, other_entity));
                if !self.active_trigger_pairs.contains_key(&key) {
                    self.trigger_events.push(TriggerEvent {
                        trigger_entity,
                        other_entity,
                        entered: true,
                    });
                }
            }
        }
        for (key, &(trigger_entity, other_entity)) in &self.active_trigger_pairs {
            if !current_triggers.contains_key(key) {
                self.trigger_events.push(TriggerEvent {
                    trigger_entity,
                    other_entity,
                    entered: false,
                });
            }
        }
        self.active_trigger_pairs = current_triggers;
    }

    /// Move a character controller and return the effective movement.
    pub fn move_character(
        &mut self,
        rb_handle: RigidBodyHandle,
        col_handle: ColliderHandle,
        desired_movement: Vec3,
        dt: f32,
    ) -> (Vec3, bool) {
        // Start from the pending kinematic target rather than the current
        // position. The target is only applied by the next physics step, and a
        // frame can pass without one, so successive moves must build on each
        // other instead of overwriting.
        let start = *self.rigid_body_set[rb_handle].next_position();

        let movement = {
            let queries = self.query_pipeline(
                QueryFilter::default()
                    .exclude_rigid_body(rb_handle)
                    .exclude_sensors(),
            );
            self.character_controller.move_shape(
                dt,
                &queries,
                self.collider_set[col_handle].shape(),
                &start,
                to_rapier_vec(desired_movement),
                |_| {},
            )
        };

        let grounded = movement.grounded;
        let effective = from_rapier_vec(movement.translation);

        // Apply the movement to the rigid body
        let mut new_pose = start;
        new_pose.translation += movement.translation;
        if let Some(body) = self.rigid_body_set.get_mut(rb_handle) {
            body.set_next_kinematic_position(new_pose);
        }

        (effective, grounded)
    }

    /// Sync physics body positions back to ECS transforms.
    pub fn sync_to_ecs(&self, world: &mut hecs::World) {
        for (rb_handle, &entity) in &self.body_to_entity {
            if let Some(body) = self.rigid_body_set.get(*rb_handle) {
                if let Ok(mut transform) = world.get::<&mut Transform>(entity) {
                    transform.position = from_rapier_vec(body.position().translation);
                    transform.rotation = from_rapier_rot(body.position().rotation);
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
        let ray = Ray::new(to_rapier_vec(origin), to_rapier_vec(direction));

        if let Some((handle, intersection)) = self
            .query_pipeline(QueryFilter::default())
            .cast_ray_and_get_normal(&ray, max_distance, true)
        {
            if let Some(&entity) = self.collider_to_entity.get(&handle) {
                let normal = from_rapier_vec(intersection.normal);
                return Some((entity, intersection.time_of_impact, normal));
            }
        }
        None
    }

    /// Cast a ray and return the first hit with detailed info including entity, distance, hit point, and normal.
    /// Optionally excludes a specific entity from the results.
    pub fn raycast_detailed(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
        exclude_entity: Option<hecs::Entity>,
    ) -> Option<(hecs::Entity, f32, Vec3, Vec3)> {
        let dir = direction.normalize_or_zero();
        let ray = Ray::new(to_rapier_vec(origin), to_rapier_vec(dir));

        // Find the collider handles to exclude based on entity
        let exclude_colliders: Vec<ColliderHandle> = if let Some(excl) = exclude_entity {
            self.collider_to_entity
                .iter()
                .filter(|(_, &e)| e == excl)
                .map(|(&h, _)| h)
                .collect()
        } else {
            Vec::new()
        };

        // Use cast_ray_and_get_normal with a filter
        let filter = if let Some(excl) = exclude_entity {
            // Find the rigid body handle for the excluded entity
            let excl_rb: Option<RigidBodyHandle> = self
                .body_to_entity
                .iter()
                .find(|(_, &e)| e == excl)
                .map(|(&h, _)| h);
            if let Some(rb) = excl_rb {
                QueryFilter::default().exclude_rigid_body(rb)
            } else {
                QueryFilter::default()
            }
        } else {
            QueryFilter::default()
        };

        if let Some((handle, intersection)) =
            self.query_pipeline(filter)
                .cast_ray_and_get_normal(&ray, max_distance, true)
        {
            // Double check collider isn't in exclude list
            if exclude_colliders.contains(&handle) {
                return None;
            }
            if let Some(&entity) = self.collider_to_entity.get(&handle) {
                let toi = intersection.time_of_impact;
                let hit_point = origin + dir * toi;
                let normal = from_rapier_vec(intersection.normal);
                return Some((entity, toi, hit_point, normal));
            }
        }
        None
    }

    /// Set the linear velocity of a rigid body and optionally disable gravity.
    pub fn set_linvel(&mut self, rb_handle: RigidBodyHandle, velocity: Vec3, zero_gravity: bool) {
        if let Some(body) = self.rigid_body_set.get_mut(rb_handle) {
            body.set_linvel(to_rapier_vec(velocity), true);
            if zero_gravity {
                body.set_gravity_scale(0.0, true);
            }
        }
    }

    /// Apply an impulse to a rigid body (instantaneous velocity change).
    pub fn apply_impulse(&mut self, rb_handle: RigidBodyHandle, impulse: Vec3) {
        if let Some(body) = self.rigid_body_set.get_mut(rb_handle) {
            body.apply_impulse(to_rapier_vec(impulse), true);
        }
    }

    /// Apply a force to a rigid body (continuous acceleration).
    pub fn apply_force(&mut self, rb_handle: RigidBodyHandle, force: Vec3) {
        if let Some(body) = self.rigid_body_set.get_mut(rb_handle) {
            body.add_force(to_rapier_vec(force), true);
        }
    }

    /// Get the linear velocity of a rigid body.
    pub fn get_linvel(&self, rb_handle: RigidBodyHandle) -> Option<Vec3> {
        self.rigid_body_set
            .get(rb_handle)
            .map(|body| from_rapier_vec(body.linvel()))
    }

    /// Set the restitution (bounciness) of all colliders attached to a rigid body.
    pub fn set_restitution(&mut self, rb_handle: RigidBodyHandle, value: f32) {
        if let Some(body) = self.rigid_body_set.get(rb_handle) {
            let col_handles: Vec<ColliderHandle> = body.colliders().to_vec();
            for col_handle in col_handles {
                if let Some(col) = self.collider_set.get_mut(col_handle) {
                    col.set_restitution(value);
                }
            }
        }
    }

    /// Set the friction of all colliders attached to a rigid body.
    pub fn set_friction(&mut self, rb_handle: RigidBodyHandle, value: f32) {
        if let Some(body) = self.rigid_body_set.get(rb_handle) {
            let col_handles: Vec<ColliderHandle> = body.colliders().to_vec();
            for col_handle in col_handles {
                if let Some(col) = self.collider_set.get_mut(col_handle) {
                    col.set_friction(value);
                }
            }
        }
    }

    /// Scene queries (ray casts, shape casts) against the colliders as of
    /// the last physics step.
    pub fn query_pipeline<'a>(&'a self, filter: QueryFilter<'a>) -> QueryPipeline<'a> {
        self.broad_phase.as_query_pipeline(
            self.narrow_phase.query_dispatcher(),
            &self.rigid_body_set,
            &self.collider_set,
            filter,
        )
    }

    /// Remove a body and its colliders, emitting one exit for each active sensor
    /// overlap. Repeated removals do not emit repeated exits.
    pub fn remove_body(&mut self, rb_handle: RigidBodyHandle) {
        if let Some(body) = self.rigid_body_set.get(rb_handle) {
            let removed = body.colliders();
            self.active_trigger_pairs.retain(|pair, entities| {
                let (trigger_entity, other_entity) = *entities;
                if removed.contains(&pair[0]) || removed.contains(&pair[1]) {
                    self.trigger_events.push(TriggerEvent {
                        trigger_entity,
                        other_entity,
                        entered: false,
                    });
                    false
                } else {
                    true
                }
            });
            for handle in removed {
                self.collider_to_entity.remove(handle);
            }
        }
        self.rigid_body_set.remove(
            rb_handle,
            &mut self.island_manager,
            &mut self.collider_set,
            &mut self.impulse_joint_set,
            &mut self.multibody_joint_set,
            &mut self.soft_body_set,
            true,
        );
        self.body_to_entity.remove(&rb_handle);
        self.active_contact_pairs.retain(|pair| {
            self.collider_set.contains(pair[0]) && self.collider_set.contains(pair[1])
        });
    }
}

impl PhysicsWorld {
    /// Cooking counts and cache occupancy for triangle-mesh colliders.
    /// Removing a body keeps its geometry available for future spawns;
    /// [`PhysicsWorld::reset`] keeps it too, while a new world starts empty.
    pub fn collider_cooking_stats(&self) -> ColliderCookingStats {
        self.cooked_shapes.stats()
    }

    /// Validate and cook a triangle-mesh collider ahead of construction.
    /// Scene preflight calls this before tearing anything down, so a mesh
    /// that cannot be cooked (zero scale, extreme coordinates) rejects the
    /// load instead of failing halfway through it.
    pub fn prepare_trimesh(
        &mut self,
        vertices: &[Vector],
        indices: &[[u32; 3]],
    ) -> Result<(), String> {
        self.cooked_shapes
            .try_get_or_cook(vertices, indices)
            .map(|_| ())
    }

    /// Replace every body and collider with an empty world under `gravity`,
    /// keeping the cooked trimesh shapes that preflight prepared for the next
    /// scene.
    pub fn reset(&mut self, gravity: Vec3) {
        let cooked_shapes = std::mem::take(&mut self.cooked_shapes);
        *self = Self::new(gravity);
        self.cooked_shapes = cooked_shapes;
    }

    fn shape_to_collider(&mut self, shape: &PhysicsShape) -> ColliderBuilder {
        match shape {
            PhysicsShape::Box { half_extents } => {
                ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z)
            }
            PhysicsShape::Sphere { radius } => ColliderBuilder::ball(*radius),
            PhysicsShape::Capsule {
                half_height,
                radius,
            } => ColliderBuilder::capsule_y(*half_height, *radius),
            PhysicsShape::Trimesh { vertices, indices } => {
                ColliderBuilder::new(self.cooked_shapes.get_or_cook(vertices, indices))
            }
        }
    }
}

fn quat_to_angvector(q: Quat) -> rapier3d::math::AngVector {
    let (axis, angle) = q.to_axis_angle();
    to_rapier_vec(axis * angle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_a_body_cleans_colliders_and_handle_mappings() {
        let mut world = hecs::World::new();
        let removed = world.spawn(());
        let retained = world.spawn(());
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        let add = |physics: &mut PhysicsWorld, entity, position| {
            physics.add_static_body(
                entity,
                position,
                Quat::IDENTITY,
                PhysicsShape::Sphere { radius: 0.5 },
                false,
                0.0,
                0.5,
            )
        };
        let (body, collider) = add(&mut physics, removed, Vec3::ZERO);
        let (retained_body, retained_collider) = add(&mut physics, retained, Vec3::X * 5.0);
        physics
            .active_contact_pairs
            .insert([collider, retained_collider]);
        physics.remove_body(body);
        physics.remove_body(body); // Cleanup is safe if requested more than once.
        assert!(!physics.rigid_body_set.contains(body));
        assert!(!physics.collider_set.contains(collider));
        assert!(!physics.body_to_entity.contains_key(&body));
        assert!(!physics.collider_to_entity.contains_key(&collider));
        assert!(physics.active_contact_pairs.is_empty());
        assert_eq!(physics.body_to_entity[&retained_body], retained);
        assert_eq!(physics.collider_to_entity[&retained_collider], retained);
        assert_eq!(physics.collider_set.len(), 1);
    }

    fn add_test_body(
        physics: &mut PhysicsWorld,
        entity: hecs::Entity,
        body_type: PhysicsBodyType,
        sensor: bool,
    ) -> (RigidBodyHandle, ColliderHandle) {
        let shape = PhysicsShape::Sphere { radius: 1.0 };
        match body_type {
            PhysicsBodyType::Static => {
                physics.add_static_body(entity, Vec3::ZERO, Quat::IDENTITY, shape, sensor, 0.0, 0.5)
            }
            PhysicsBodyType::Kinematic => physics.add_kinematic_body(
                entity,
                Vec3::ZERO,
                Quat::IDENTITY,
                shape,
                sensor,
                0.0,
                0.5,
            ),
            PhysicsBodyType::Dynamic => physics.add_dynamic_body_with_sensor(
                entity,
                Vec3::ZERO,
                Quat::IDENTITY,
                shape,
                1.0,
                0.0,
                0.5,
                false,
                sensor,
            ),
        }
    }

    fn trigger_tuples(physics: &PhysicsWorld) -> HashSet<(hecs::Entity, hecs::Entity, bool)> {
        physics
            .trigger_events
            .iter()
            .map(|event| (event.trigger_entity, event.other_entity, event.entered))
            .collect()
    }

    #[test]
    fn every_sensor_body_type_enters_and_exits_each_body_type_once() {
        for sensor_type in [
            PhysicsBodyType::Static,
            PhysicsBodyType::Kinematic,
            PhysicsBodyType::Dynamic,
        ] {
            for other_type in [
                PhysicsBodyType::Static,
                PhysicsBodyType::Kinematic,
                PhysicsBodyType::Dynamic,
            ] {
                let mut world = hecs::World::new();
                let sensor = world.spawn(());
                let other = world.spawn(());
                let mut physics = PhysicsWorld::new(Vec3::ZERO);
                let (sensor_body, collider) =
                    add_test_body(&mut physics, sensor, sensor_type.clone(), true);
                add_test_body(&mut physics, other, other_type.clone(), false);
                assert!(physics.collider_set[collider].is_sensor());
                physics.begin_frame(FIXED_TIMESTEP);
                physics.step_fixed();
                assert_eq!(
                    trigger_tuples(&physics),
                    HashSet::from([(sensor, other, true)]),
                    "sensor {sensor_type:?}, other {other_type:?}"
                );
                assert!(
                    physics.collision_events.is_empty(),
                    "sensors must not cause contact damage"
                );
                physics.begin_frame(FIXED_TIMESTEP);
                physics.step_fixed();
                assert!(
                    physics.trigger_events.is_empty(),
                    "a continuing overlap is not a new enter"
                );
                physics.rigid_body_set[sensor_body]
                    .set_translation(to_rapier_vec(Vec3::X * 10.0), true);
                physics.begin_frame(FIXED_TIMESTEP);
                physics.step_fixed();
                assert_eq!(
                    trigger_tuples(&physics),
                    HashSet::from([(sensor, other, false)])
                );
                physics.begin_frame(FIXED_TIMESTEP);
                physics.step_fixed();
                assert!(
                    physics.trigger_events.is_empty(),
                    "exit must be emitted only once"
                );
            }
        }
    }

    #[test]
    fn overlapping_sensors_each_receive_their_own_events_and_removal_exits() {
        let mut world = hecs::World::new();
        let a = world.spawn(());
        let b = world.spawn(());
        let other = world.spawn(());
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        let (body_a, _) = add_test_body(&mut physics, a, PhysicsBodyType::Static, true);
        add_test_body(&mut physics, b, PhysicsBodyType::Kinematic, true);
        let (other_body, _) = add_test_body(&mut physics, other, PhysicsBodyType::Dynamic, false);
        physics.begin_frame(FIXED_TIMESTEP);
        physics.step_fixed();
        assert_eq!(
            trigger_tuples(&physics),
            HashSet::from([
                (a, b, true),
                (b, a, true),
                (a, other, true),
                (b, other, true),
            ])
        );
        assert_eq!(physics.trigger_events.len(), 4);
        physics.begin_frame(0.0);
        physics.remove_body(other_body);
        physics.remove_body(other_body);
        assert_eq!(
            trigger_tuples(&physics),
            HashSet::from([(a, other, false), (b, other, false)])
        );
        assert_eq!(physics.trigger_events.len(), 2);
        assert_eq!(physics.active_trigger_pairs.len(), 2);
        physics.begin_frame(0.0);
        physics.remove_body(body_a);
        assert_eq!(
            trigger_tuples(&physics),
            HashSet::from([(a, b, false), (b, a, false)])
        );
        assert!(physics.active_trigger_pairs.is_empty());
        physics.begin_frame(FIXED_TIMESTEP);
        physics.step_fixed();
        assert!(physics.trigger_events.is_empty());
    }

    #[test]
    fn trigger_enter_and_exit_accumulate_across_fixed_steps_in_one_frame() {
        let mut world = hecs::World::new();
        let sensor = world.spawn(());
        let other = world.spawn(());
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        add_test_body(&mut physics, sensor, PhysicsBodyType::Static, true);
        let (body, _) = add_test_body(&mut physics, other, PhysicsBodyType::Dynamic, false);
        assert_eq!(physics.begin_frame(2.0 * FIXED_TIMESTEP), 2);
        physics.step_fixed();
        physics.rigid_body_set[body].set_translation(to_rapier_vec(Vec3::X * 10.0), true);
        physics.step_fixed();
        assert_eq!(physics.trigger_events.len(), 2);
        assert!(physics.trigger_events[0].entered);
        assert!(!physics.trigger_events[1].entered);
        assert_eq!(physics.begin_frame(0.0), 0);
        assert!(physics.trigger_events.is_empty());
    }

    #[test]
    fn character_controller_passes_through_sensors() {
        let mut world = hecs::World::new();
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        physics.add_static_body(
            world.spawn(()),
            Vec3::X,
            Quat::IDENTITY,
            PhysicsShape::Sphere { radius: 0.5 },
            true,
            0.0,
            0.5,
        );
        let (body, collider) = physics.add_character_body(world.spawn(()), Vec3::ZERO, 0.5, 0.3);
        physics.step_fixed();
        let (movement, _) = physics.move_character(body, collider, Vec3::X * 2.0, FIXED_TIMESTEP);
        assert!(
            (movement.x - 2.0).abs() < 1e-4,
            "sensor blocked movement: {movement:?}"
        );
    }

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
            0.0,
            0.5,
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
            0.0,
            0.5,
        );

        // Scene queries see colliders once a step has added them to the broad phase.
        pw.step_fixed();

        // Raycast down
        let result = pw.raycast(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0), 100.0);
        assert!(result.is_some());
        let (hit_entity, distance, _normal) = result.unwrap();
        assert_eq!(hit_entity, entity);
        assert!(distance > 0.0);
    }

    const GRAVITY: Vec3 = Vec3::new(0.0, -9.81, 0.0);

    /// Feed `frames` frames of `1 / fps` seconds through the fixed-step loop
    /// and return how many steps ran.
    fn run_frames(pw: &mut PhysicsWorld, fps: f32, frames: u32) -> u32 {
        let mut total = 0;
        for _ in 0..frames {
            let steps = pw.begin_frame(1.0 / fps);
            for _ in 0..steps {
                pw.step_fixed();
            }
            total += steps;
        }
        total
    }

    /// A world with a ball half a metre above a floor.
    fn ball_over_floor() -> PhysicsWorld {
        let mut world = hecs::World::new();
        let mut pw = PhysicsWorld::new(GRAVITY);
        pw.add_static_body(
            world.spawn(()),
            Vec3::new(0.0, -0.5, 0.0),
            Quat::IDENTITY,
            PhysicsShape::Box {
                half_extents: Vec3::new(10.0, 0.5, 10.0),
            },
            false,
            0.0,
            0.5,
        );
        pw.add_dynamic_body(
            world.spawn(()),
            Vec3::new(0.0, 1.0, 0.0),
            Quat::IDENTITY,
            PhysicsShape::Sphere { radius: 0.5 },
            1.0,
            0.0,
            0.5,
            false,
        );
        pw
    }

    #[test]
    fn fixed_steps_follow_elapsed_time_not_frame_rate() {
        let two_seconds_of_steps = (2.0 / FIXED_TIMESTEP).round() as i64;
        for fps in [30.0, 60.0, 120.0, 144.0, 240.0] {
            let mut pw = PhysicsWorld::new(GRAVITY);
            let steps = run_frames(&mut pw, fps, (fps * 2.0) as u32) as i64;
            assert!(
                (steps - two_seconds_of_steps).abs() <= 1,
                "{fps} fps ran {steps} steps, expected about {two_seconds_of_steps}"
            );
        }
    }

    #[test]
    fn simulation_is_identical_at_different_frame_rates() {
        let mut heights = Vec::new();
        for fps in [30.0, 60.0, 120.0] {
            let mut world = hecs::World::new();
            let mut pw = PhysicsWorld::new(GRAVITY);
            let (ball, _) = pw.add_dynamic_body(
                world.spawn(()),
                Vec3::new(0.0, 10.0, 0.0),
                Quat::IDENTITY,
                PhysicsShape::Sphere { radius: 0.5 },
                1.0,
                0.0,
                0.5,
                false,
            );
            // One second of game time at each frame rate.
            let steps = run_frames(&mut pw, fps, fps as u32);
            assert_eq!(steps, 120, "{fps} fps");
            heights.push(pw.rigid_body_set[ball].translation().y);
        }
        assert!(heights[0] < 10.0, "the ball should have fallen");
        assert_eq!(heights[0], heights[1]);
        assert_eq!(heights[1], heights[2]);
    }

    #[test]
    fn long_frame_is_capped_and_backlog_dropped() {
        let mut pw = PhysicsWorld::new(GRAVITY);
        assert_eq!(pw.begin_frame(1.0), MAX_STEPS_PER_FRAME);
        // The backlog is gone, so the next ordinary frame runs one step.
        assert_eq!(pw.begin_frame(FIXED_TIMESTEP), 1);
    }

    #[test]
    fn collision_events_accumulate_over_a_frame_then_clear() {
        fn single_step_frame(pw: &mut PhysicsWorld) {
            assert_eq!(pw.begin_frame(FIXED_TIMESTEP), 1);
            pw.step_fixed();
        }

        // Find the step on which the ball first touches the floor.
        let mut pw = ball_over_floor();
        let mut contact_step = 0;
        for step in 1..=240 {
            single_step_frame(&mut pw);
            if !pw.collision_events.is_empty() {
                contact_step = step;
                break;
            }
        }
        assert!(contact_step > 1, "the ball never reached the floor");

        // Replay, with the contact in the first of two steps in one frame.
        let mut pw = ball_over_floor();
        for _ in 1..contact_step {
            single_step_frame(&mut pw);
        }
        assert_eq!(pw.begin_frame(2.0 * FIXED_TIMESTEP), 2);
        pw.step_fixed();
        pw.step_fixed();
        assert_eq!(
            pw.collision_events.len(),
            1,
            "the event from the first step should survive the second"
        );

        // A frame too short to run a step must not replay that event.
        assert_eq!(pw.begin_frame(0.0), 0);
        assert!(pw.collision_events.is_empty());
    }

    #[test]
    fn character_moves_build_up_between_steps() {
        let mut world = hecs::World::new();
        let mut pw = PhysicsWorld::new(GRAVITY);
        let (rb, col) = pw.add_character_body(world.spawn(()), Vec3::ZERO, 0.5, 0.3);

        // Two moves with no physics step in between, as on a display faster
        // than the step rate.
        pw.move_character(rb, col, Vec3::new(0.25, 0.0, 0.0), FIXED_TIMESTEP);
        pw.move_character(rb, col, Vec3::new(0.25, 0.0, 0.0), FIXED_TIMESTEP);
        pw.step_fixed();

        let x = pw.rigid_body_set[rb].translation().x;
        assert!(
            (x - 0.5).abs() < 1e-4,
            "expected both moves to apply, got x = {x}"
        );
    }
}
