//! Headless entry points and shared runtime spawn recipes and finishers.

use crate::components::{Projectile, Transform};
use crate::physics::{PhysicsWorld, RigidBody};
use crate::scene::*;

#[cfg(any(test, feature = "test-support"))]
use super::construction::{construct_entity, PreparedSpawnAssets};
use super::{spawn_entity_with_resolver, DynamicSpawnCommand, ProjectileSpawnCommand, SceneWorld};

/// Destroy a runtime entity by its string ID.
pub fn destroy_runtime_entity(scene_world: &mut SceneWorld, id: &str) -> bool {
    if let Some(entity) = scene_world.entity_registry.remove(id) {
        let _ = scene_world.world.despawn(entity);
        true
    } else {
        false
    }
}

/// Resolve assets through the same contract used by native GPU caches.
pub fn spawn_entity_headless_with_assets(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    physics_world: &mut PhysicsWorld,
    assets: &mut crate::assets::AssetCatalog,
) -> Result<(), String> {
    spawn_entity_with_resolver(scene_world, definition, Some(physics_world), assets).map(|_| ())
}

/// Construct schema-only fixtures through the same CPU constructor.
#[cfg(any(test, feature = "test-support"))]
pub fn spawn_entity_headless(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    physics_world: &mut PhysicsWorld,
) {
    if let Err(error) = construct_entity(
        scene_world,
        definition,
        PreparedSpawnAssets::schema_fixture(definition),
        Some(physics_world),
    ) {
        tracing::error!("schema fixture: {error}");
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn spawn_all_entities_headless(
    scene_world: &mut SceneWorld,
    scene: &SceneFile,
    physics_world: &mut PhysicsWorld,
) {
    for definition in &scene.entities {
        spawn_entity_headless(scene_world, definition, physics_world);
    }
    scene_world.current_scene = Some(scene.clone());
}

impl ProjectileSpawnCommand {
    pub fn entity_def(&self) -> EntityDef {
        EntityDef {
            id: self.id.clone(),
            tags: vec!["projectile".into()],
            extends: None,
            components: ComponentMap {
                transform: Some(TransformDef {
                    position: self.position,
                    rotation: [0.0; 3],
                    scale: self.scale,
                }),
                mesh_renderer: Some(MeshRendererDef {
                    mesh: self.mesh.clone(),
                    material: self.material.clone(),
                    cast_shadows: true,
                    receive_shadows: true,
                }),
                rigid_body: Some(RigidBodyDef {
                    body_type: "dynamic".into(),
                    mass: 0.1,
                    ccd: true,
                }),
                collider: Some(ColliderDef {
                    shape: "sphere".into(),
                    half_extents: None,
                    radius: Some(0.1),
                    half_height: None,
                    is_trigger: false,
                    restitution: 0.0,
                    friction: 0.5,
                }),
                collision_damage: Some(CollisionDamageDef {
                    damage: self.damage,
                    destroy_on_hit: true,
                }),
                ..Default::default()
            },
        }
    }
}

impl DynamicSpawnCommand {
    pub fn entity_def(&self) -> EntityDef {
        EntityDef {
            id: self.id.clone(),
            tags: vec!["dynamic".into()],
            extends: None,
            components: ComponentMap {
                transform: Some(TransformDef {
                    position: self.position,
                    rotation: [0.0; 3],
                    scale: [self.radius * 2.0; 3],
                }),
                mesh_renderer: Some(MeshRendererDef {
                    mesh: self.mesh.clone(),
                    material: self.material.clone(),
                    cast_shadows: true,
                    receive_shadows: true,
                }),
                rigid_body: Some(RigidBodyDef {
                    body_type: "dynamic".into(),
                    mass: self.mass,
                    ccd: false,
                }),
                collider: Some(ColliderDef {
                    shape: "sphere".into(),
                    half_extents: None,
                    radius: Some(self.radius),
                    half_height: None,
                    is_trigger: false,
                    restitution: self.restitution,
                    friction: self.friction,
                }),
                ..Default::default()
            },
        }
    }
}

pub fn finish_projectile_spawn(
    scene_world: &mut SceneWorld,
    physics_world: &mut PhysicsWorld,
    entity: hecs::Entity,
    command: &ProjectileSpawnCommand,
) {
    let _ = scene_world.world.insert_one(
        entity,
        Projectile {
            damage: command.damage,
            lifetime: command.lifetime,
            age: 0.0,
            owner_id: command.owner_id.clone(),
        },
    );
    let body = scene_world
        .world
        .get::<&RigidBody>(entity)
        .ok()
        .map(|body| body.handle);
    if let Some(body) = body {
        physics_world.set_linvel(
            body,
            glam::Vec3::from(command.direction).normalize_or_zero() * command.speed,
            !command.gravity,
        );
    }
}

pub fn finish_dynamic_spawn(
    scene_world: &mut SceneWorld,
    physics_world: &mut PhysicsWorld,
    entity: hecs::Entity,
    command: &DynamicSpawnCommand,
) {
    let _ = scene_world.world.insert_one(
        entity,
        Projectile {
            damage: 0.0,
            lifetime: command.lifetime,
            age: 0.0,
            owner_id: String::new(),
        },
    );
    if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
        transform.scale = glam::Vec3::splat(command.radius * 2.0);
        transform.dirty = true;
    }
    let body = scene_world
        .world
        .get::<&RigidBody>(entity)
        .ok()
        .map(|body| body.handle);
    if let Some(body) = body {
        physics_world.set_linvel(body, command.velocity.into(), false);
        if let Some(body) = physics_world.rigid_body_set.get_mut(body) {
            body.activation_mut().normalized_linear_threshold = -1.0;
        }
    }
}
