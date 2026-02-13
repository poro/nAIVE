use std::collections::{HashMap, HashSet};
use std::path::Path;

use hecs::World;

use crate::components::*;
use crate::mesh::MeshCache;
use crate::material::MaterialCache;
use crate::scene::{EntityDef, SceneFile};

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

/// Spawn all entities from a parsed scene into the ECS world.
pub fn spawn_all_entities(
    scene_world: &mut SceneWorld,
    scene: &SceneFile,
    device: &wgpu::Device,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
) {
    for entity_def in &scene.entities {
        spawn_entity(scene_world, entity_def, device, project_root, mesh_cache, material_cache);
    }
    scene_world.current_scene = Some(scene.clone());
    tracing::info!(
        "Scene '{}' loaded: {} entities",
        scene.name,
        scene.entities.len()
    );
}

/// Spawn a single entity from its definition.
fn spawn_entity(
    scene_world: &mut SceneWorld,
    entity_def: &EntityDef,
    device: &wgpu::Device,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
) {
    let entity_id = EntityId(entity_def.id.clone());
    let tags = Tags(entity_def.tags.clone());

    // Build the transform component
    let transform = if let Some(t) = &entity_def.components.transform {
        Transform {
            position: glam::Vec3::from(t.position),
            rotation: euler_degrees_to_quat(t.rotation),
            scale: glam::Vec3::from(t.scale),
            world_matrix: glam::Mat4::IDENTITY,
            parent: None,
            dirty: true,
        }
    } else {
        Transform::default()
    };

    // Start with base components all entities have
    let entity = if let Some(mr) = &entity_def.components.mesh_renderer {
        // Load mesh and material
        let mesh_handle = match mesh_cache.get_or_load(device, project_root, &mr.mesh) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to load mesh '{}' for entity '{}': {}", mr.mesh, entity_def.id, e);
                return;
            }
        };
        let material_handle = match material_cache.get_or_load(device, project_root, &mr.material) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to load material '{}' for entity '{}': {}", mr.material, entity_def.id, e);
                return;
            }
        };
        let mesh_renderer = MeshRenderer {
            mesh_handle,
            material_handle,
        };

        if let Some(cam) = &entity_def.components.camera {
            let camera = Camera {
                fov_degrees: cam.fov,
                near: cam.near,
                far: cam.far,
                role: if cam.role == "main" {
                    CameraRole::Main
                } else {
                    CameraRole::Other(cam.role.clone())
                },
                aspect_ratio: 16.0 / 9.0,
            };
            scene_world
                .world
                .spawn((entity_id, tags, transform, mesh_renderer, camera))
        } else if let Some(pl) = &entity_def.components.point_light {
            let point_light = PointLight {
                color: glam::Vec3::from(pl.color),
                intensity: pl.intensity,
                range: pl.range,
            };
            scene_world
                .world
                .spawn((entity_id, tags, transform, mesh_renderer, point_light))
        } else {
            scene_world
                .world
                .spawn((entity_id, tags, transform, mesh_renderer))
        }
    } else if let Some(cam) = &entity_def.components.camera {
        let camera = Camera {
            fov_degrees: cam.fov,
            near: cam.near,
            far: cam.far,
            role: if cam.role == "main" {
                CameraRole::Main
            } else {
                CameraRole::Other(cam.role.clone())
            },
            aspect_ratio: 16.0 / 9.0,
        };
        scene_world
            .world
            .spawn((entity_id, tags, transform, camera))
    } else if let Some(pl) = &entity_def.components.point_light {
        let point_light = PointLight {
            color: glam::Vec3::from(pl.color),
            intensity: pl.intensity,
            range: pl.range,
        };
        scene_world
            .world
            .spawn((entity_id, tags, transform, point_light))
    } else {
        scene_world
            .world
            .spawn((entity_id, tags, transform))
    };

    scene_world
        .entity_registry
        .insert(entity_def.id.clone(), entity);
}

/// Convert Euler degrees [pitch, yaw, roll] to a Quaternion.
fn euler_degrees_to_quat(euler: [f32; 3]) -> glam::Quat {
    let [pitch, yaw, roll] = euler;
    glam::Quat::from_euler(
        glam::EulerRot::YXZ,
        yaw.to_radians(),
        pitch.to_radians(),
        roll.to_radians(),
    )
}

/// Reconcile a scene update: diff old vs new, spawn/despawn/patch entities.
pub fn reconcile_scene(
    scene_world: &mut SceneWorld,
    new_scene: &SceneFile,
    device: &wgpu::Device,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
) {
    let old_scene = match &scene_world.current_scene {
        Some(s) => s.clone(),
        None => {
            spawn_all_entities(scene_world, new_scene, device, project_root, mesh_cache, material_cache);
            return;
        }
    };

    let old_ids: HashSet<&str> = old_scene.entities.iter().map(|e| e.id.as_str()).collect();
    let new_ids: HashSet<&str> = new_scene.entities.iter().map(|e| e.id.as_str()).collect();

    // 1. Remove entities no longer in the scene
    for id in old_ids.difference(&new_ids) {
        if let Some(entity) = scene_world.entity_registry.remove(*id) {
            let _ = scene_world.world.despawn(entity);
            tracing::info!("Hot-reload: destroyed entity '{}'", id);
        }
    }

    // 2. Spawn new entities
    for entity_def in &new_scene.entities {
        if !old_ids.contains(entity_def.id.as_str()) {
            spawn_entity(scene_world, entity_def, device, project_root, mesh_cache, material_cache);
            tracing::info!("Hot-reload: spawned entity '{}'", entity_def.id);
        }
    }

    // 3. Patch modified entities
    let old_map: HashMap<&str, &EntityDef> = old_scene
        .entities
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();

    for new_def in &new_scene.entities {
        if let Some(&old_def) = old_map.get(new_def.id.as_str()) {
            if let Some(&entity) = scene_world.entity_registry.get(&new_def.id) {
                patch_entity(&mut scene_world.world, entity, old_def, new_def);
            }
        }
    }

    scene_world.current_scene = Some(new_scene.clone());
}

/// Patch an existing entity's components in-place.
fn patch_entity(
    world: &mut World,
    entity: hecs::Entity,
    _old_def: &EntityDef,
    new_def: &EntityDef,
) {
    // Patch transform
    if let Some(t) = &new_def.components.transform {
        if let Ok(mut transform) = world.get::<&mut Transform>(entity) {
            transform.position = glam::Vec3::from(t.position);
            transform.rotation = euler_degrees_to_quat(t.rotation);
            transform.scale = glam::Vec3::from(t.scale);
            transform.dirty = true;
        }
    }

    // Patch camera
    if let Some(cam) = &new_def.components.camera {
        if let Ok(mut camera) = world.get::<&mut Camera>(entity) {
            camera.fov_degrees = cam.fov;
            camera.near = cam.near;
            camera.far = cam.far;
        }
    }

    // Patch point light
    if let Some(pl) = &new_def.components.point_light {
        if let Ok(mut point_light) = world.get::<&mut PointLight>(entity) {
            point_light.color = glam::Vec3::from(pl.color);
            point_light.intensity = pl.intensity;
            point_light.range = pl.range;
        }
    }
}
