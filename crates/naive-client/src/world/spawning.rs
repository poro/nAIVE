//! Native compatibility entry points over the shared CPU constructor.

use std::path::Path;

use crate::material::MaterialCache;
use crate::mesh::{MeshCache, TextureResources};
use crate::physics::PhysicsWorld;
use crate::scene::{EntityDef, SceneFile};
use crate::splat::SplatCache;
use crate::texture_cache::TextureCache;

use super::native_assets::NativeAssetResolver;
use super::{
    finish_dynamic_spawn, finish_projectile_spawn, preflight_scene, spawn_entity_with_resolver,
    DynamicSpawnCommand, ProjectileSpawnCommand, SceneWorld, SpawnCommand,
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
