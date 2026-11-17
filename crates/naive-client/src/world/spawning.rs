//! Compatibility entry points and runtime spawn recipes over one CPU constructor.

use std::path::Path;

use crate::components::{Projectile, Transform};
use crate::material::MaterialCache;
use crate::mesh::{MeshCache, TextureResources};
use crate::physics::{PhysicsWorld, RigidBody};
use crate::scene::*;
use crate::splat::SplatCache;
use crate::texture_cache::TextureCache;

#[cfg(test)]
use super::construction::{construct_entity, PreparedSpawnAssets};
use super::native_assets::NativeAssetResolver;
use super::{
    preflight_scene, spawn_entity_with_resolver, DynamicSpawnCommand, ProjectileSpawnCommand,
    SceneWorld, SpawnCommand,
};

/// Fallible native entry point. Asset resolution finishes before ECS allocation.
#[allow(clippy::too_many_arguments)]
pub fn spawn_entity_native(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    physics_world: Option<&mut PhysicsWorld>,
    texture_resources: Option<&TextureResources>,
    texture_cache: Option<&mut TextureCache>,
) -> Result<hecs::Entity, String> {
    let mut resolver = NativeAssetResolver {
        device,
        queue,
        project_root,
        meshes: mesh_cache,
        materials: material_cache,
        splats: Some(splat_cache),
        textures: texture_cache,
        texture_resources,
    };
    spawn_entity_with_resolver(scene_world, definition, physics_world, &mut resolver)
}

/// Check native scene assets before startup, scene replacement or hot reload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn preflight_scene_native(
    scene: &SceneFile,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    texture_resources: Option<&TextureResources>,
    texture_cache: Option<&mut TextureCache>,
) -> Result<(), String> {
    let mut resolver = NativeAssetResolver {
        device,
        queue,
        project_root,
        meshes: mesh_cache,
        materials: material_cache,
        splats: Some(splat_cache),
        textures: texture_cache,
        texture_resources,
    };
    preflight_scene(scene, &mut resolver)
}

/// Logging compatibility wrapper for authored native scene spawning.
#[allow(clippy::too_many_arguments)]
pub fn spawn_entity(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    physics_world: Option<&mut PhysicsWorld>,
    texture_resources: Option<&TextureResources>,
    texture_cache: Option<&mut TextureCache>,
) {
    if let Err(error) = spawn_entity_native(
        scene_world,
        definition,
        device,
        queue,
        project_root,
        mesh_cache,
        material_cache,
        splat_cache,
        physics_world,
        texture_resources,
        texture_cache,
    ) {
        tracing::error!("{error}");
    }
}

/// Compatibility entry for native runtime mesh entities.
#[allow(clippy::too_many_arguments)]
pub fn spawn_runtime_entity(
    scene_world: &mut SceneWorld,
    id: &str,
    mesh: &str,
    material: &str,
    position: [f32; 3],
    scale: [f32; 3],
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    texture_resources: Option<&TextureResources>,
) -> bool {
    let definition = SpawnCommand {
        id: id.into(),
        mesh: mesh.into(),
        material: material.into(),
        position,
        scale,
        collider: None,
        rigid_body: None,
        script: None,
    }
    .entity_def();
    let mut resolver = NativeAssetResolver {
        device,
        queue,
        project_root,
        meshes: mesh_cache,
        materials: material_cache,
        splats: None,
        textures: None,
        texture_resources,
    };
    match spawn_entity_with_resolver(scene_world, &definition, None, &mut resolver) {
        Ok(_) => true,
        Err(error) => {
            tracing::error!("spawn_runtime_entity: {error}");
            false
        }
    }
}

/// Compatibility entry for native runtime projectiles.
#[allow(clippy::too_many_arguments)]
pub fn spawn_projectile_entity(
    scene_world: &mut SceneWorld,
    command: &ProjectileSpawnCommand,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    physics_world: &mut PhysicsWorld,
    texture_resources: Option<&TextureResources>,
) -> bool {
    let mut resolver = NativeAssetResolver {
        device,
        queue,
        project_root,
        meshes: mesh_cache,
        materials: material_cache,
        splats: None,
        textures: None,
        texture_resources,
    };
    match spawn_entity_with_resolver(
        scene_world,
        &command.entity_def(),
        Some(physics_world),
        &mut resolver,
    ) {
        Ok(entity) => {
            finish_projectile_spawn(scene_world, physics_world, entity, command);
            true
        }
        Err(error) => {
            tracing::error!("spawn_projectile_entity: {error}");
            false
        }
    }
}

/// Compatibility entry for native bouncing bodies.
#[allow(clippy::too_many_arguments)]
pub fn spawn_dynamic_entity(
    scene_world: &mut SceneWorld,
    command: &DynamicSpawnCommand,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    physics_world: &mut PhysicsWorld,
    texture_resources: Option<&TextureResources>,
) -> bool {
    let mut resolver = NativeAssetResolver {
        device,
        queue,
        project_root,
        meshes: mesh_cache,
        materials: material_cache,
        splats: None,
        textures: None,
        texture_resources,
    };
    match spawn_entity_with_resolver(
        scene_world,
        &command.entity_def(),
        Some(physics_world),
        &mut resolver,
    ) {
        Ok(entity) => {
            finish_dynamic_spawn(scene_world, physics_world, entity, command);
            true
        }
        Err(error) => {
            tracing::error!("spawn_dynamic_entity: {error}");
            false
        }
    }
}

/// Resolve assets through the same contract used by native GPU caches.
pub fn spawn_entity_headless_with_assets(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    physics_world: &mut PhysicsWorld,
    assets: &mut naive_sim::assets::AssetCatalog,
) -> Result<(), String> {
    spawn_entity_with_resolver(scene_world, definition, Some(physics_world), assets).map(|_| ())
}

/// Construct schema-only fixtures through the same CPU constructor.
#[cfg(test)]
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

#[cfg(test)]
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
