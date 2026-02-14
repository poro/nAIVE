use std::collections::{HashMap, HashSet};
use std::path::Path;

use hecs::World;

use crate::components::*;
use crate::mesh::MeshCache;
use crate::material::MaterialCache;
use crate::physics::{self, CharacterController, PhysicsShape, PhysicsWorld};
use crate::scene::{EntityDef, SceneFile};
use crate::splat::SplatCache;

/// Deferred entity commands from Lua scripts, processed each frame.
#[derive(Default)]
pub struct EntityCommandQueue {
    pub spawns: Vec<SpawnCommand>,
    pub destroys: Vec<String>,
    pub scale_updates: Vec<(String, [f32; 3])>,
    pub visibility_updates: Vec<(String, bool)>,
}

pub struct SpawnCommand {
    pub id: String,
    pub mesh: String,
    pub material: String,
    pub position: [f32; 3],
    pub scale: [f32; 3],
}

impl EntityCommandQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.spawns.clear();
        self.destroys.clear();
        self.scale_updates.clear();
        self.visibility_updates.clear();
    }
}

/// Spawn a runtime entity (from Lua). Simpler than scene spawning: just Transform + MeshRenderer.
#[allow(clippy::too_many_arguments)]
pub fn spawn_runtime_entity(
    scene_world: &mut SceneWorld,
    id: &str,
    mesh: &str,
    material: &str,
    position: [f32; 3],
    scale: [f32; 3],
    device: &wgpu::Device,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
) -> bool {
    if scene_world.entity_registry.contains_key(id) {
        tracing::warn!("spawn_runtime_entity: id '{}' already exists", id);
        return false;
    }

    let mesh_handle = match mesh_cache.get_or_load(device, project_root, mesh) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("spawn_runtime_entity: mesh '{}' failed: {}", mesh, e);
            return false;
        }
    };
    let material_handle = match material_cache.get_or_load(device, project_root, material) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("spawn_runtime_entity: material '{}' failed: {}", material, e);
            return false;
        }
    };

    let transform = Transform {
        position: glam::Vec3::from(position),
        scale: glam::Vec3::from(scale),
        dirty: true,
        ..Default::default()
    };
    let mesh_renderer = MeshRenderer {
        mesh_handle,
        material_handle,
    };
    let entity_id = EntityId(id.to_string());
    let tags = Tags(vec![]);

    let entity = scene_world
        .world
        .spawn((entity_id, tags, transform, mesh_renderer));
    scene_world.entity_registry.insert(id.to_string(), entity);
    true
}

/// Destroy a runtime entity by its string ID.
pub fn destroy_runtime_entity(scene_world: &mut SceneWorld, id: &str) -> bool {
    if let Some(entity) = scene_world.entity_registry.remove(id) {
        let _ = scene_world.world.despawn(entity);
        true
    } else {
        false
    }
}

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
    splat_cache: &mut SplatCache,
    physics_world: Option<&mut PhysicsWorld>,
) {
    let pw_ptr = physics_world.map(|pw| pw as *mut PhysicsWorld);
    for entity_def in &scene.entities {
        // SAFETY: we need to reborrow the physics world for each entity spawn since
        // Option<&mut T> is not Copy. The reference is valid for the entire loop.
        let pw_ref = pw_ptr.map(|ptr| unsafe { &mut *ptr });
        spawn_entity(scene_world, entity_def, device, project_root, mesh_cache, material_cache, splat_cache, pw_ref);
    }
    scene_world.current_scene = Some(scene.clone());
    tracing::info!(
        "Scene '{}' loaded: {} entities",
        scene.name,
        scene.entities.len()
    );
}

/// Spawn a single entity from its definition.
#[allow(clippy::too_many_arguments)]
fn spawn_entity(
    scene_world: &mut SceneWorld,
    entity_def: &EntityDef,
    device: &wgpu::Device,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    physics_world: Option<&mut PhysicsWorld>,
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

    // Handle gaussian splat entities
    if let Some(gs) = &entity_def.components.gaussian_splat {
        let splat_handle = match splat_cache.get_or_load(device, project_root, &gs.source) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to load splat '{}' for entity '{}': {}", gs.source, entity_def.id, e);
                return;
            }
        };
        let gaussian_splat = GaussianSplat { splat_handle };
        let entity = scene_world.world.spawn((entity_id, tags, transform, gaussian_splat));
        scene_world.entity_registry.insert(entity_def.id.clone(), entity);
        return;
    }

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
    } else if let Some(dl) = &entity_def.components.directional_light {
        let dir_light = crate::components::DirectionalLight {
            direction: glam::Vec3::from(dl.direction).normalize(),
            color: glam::Vec3::from(dl.color),
            intensity: dl.intensity,
            shadow_extent: dl.shadow_extent,
        };
        scene_world
            .world
            .spawn((entity_id, tags, transform, dir_light))
    } else {
        scene_world
            .world
            .spawn((entity_id, tags, transform))
    };

    scene_world
        .entity_registry
        .insert(entity_def.id.clone(), entity);

    // Spawn physics components if physics world is available
    if let Some(pw) = physics_world {
        let pos = if let Some(t) = &entity_def.components.transform {
            glam::Vec3::from(t.position)
        } else {
            glam::Vec3::ZERO
        };
        let rot = if let Some(t) = &entity_def.components.transform {
            euler_degrees_to_quat(t.rotation)
        } else {
            glam::Quat::IDENTITY
        };

        // Character controller takes priority
        if let Some(cc_def) = &entity_def.components.character_controller {
            let half_height = cc_def.height / 2.0 - cc_def.radius;
            let (rb_handle, col_handle) =
                pw.add_character_body(entity, pos, half_height.max(0.1), cc_def.radius);

            let rb_comp = physics::RigidBody {
                handle: rb_handle,
                body_type: physics::PhysicsBodyType::Kinematic,
            };
            let col_comp = physics::Collider {
                handle: col_handle,
                shape: PhysicsShape::Capsule {
                    half_height: half_height.max(0.1),
                    radius: cc_def.radius,
                },
                is_trigger: false,
            };
            let cc_comp = CharacterController {
                move_speed: cc_def.move_speed,
                sprint_multiplier: cc_def.sprint_multiplier,
                jump_impulse: cc_def.jump_impulse,
                step_height: cc_def.step_height,
                ..Default::default()
            };
            let player = Player {
                yaw: 0.0,
                pitch: 0.0,
                height: cc_def.height,
                radius: cc_def.radius,
            };
            let _ = scene_world.world.insert(entity, (rb_comp, col_comp, cc_comp, player));
        } else if let Some(col_def) = &entity_def.components.collider {
            let shape = parse_collider_shape(col_def);
            let is_trigger = col_def.is_trigger;

            let body_type = entity_def
                .components
                .rigid_body
                .as_ref()
                .map(|rb| rb.body_type.as_str())
                .unwrap_or("static");

            let restitution = col_def.restitution;
            let friction = col_def.friction;

            match body_type {
                "dynamic" => {
                    let mass = entity_def
                        .components
                        .rigid_body
                        .as_ref()
                        .map(|rb| rb.mass)
                        .unwrap_or(1.0);
                    let (rb_handle, col_handle) =
                        pw.add_dynamic_body(entity, pos, rot, shape.clone(), mass, restitution, friction);
                    let rb_comp = physics::RigidBody {
                        handle: rb_handle,
                        body_type: physics::PhysicsBodyType::Dynamic,
                    };
                    let col_comp = physics::Collider {
                        handle: col_handle,
                        shape,
                        is_trigger,
                    };
                    let _ = scene_world.world.insert(entity, (rb_comp, col_comp));
                }
                _ => {
                    let (rb_handle, col_handle) =
                        pw.add_static_body(entity, pos, rot, shape.clone(), is_trigger, restitution, friction);
                    let rb_comp = physics::RigidBody {
                        handle: rb_handle,
                        body_type: physics::PhysicsBodyType::Static,
                    };
                    let col_comp = physics::Collider {
                        handle: col_handle,
                        shape,
                        is_trigger,
                    };
                    let _ = scene_world.world.insert(entity, (rb_comp, col_comp));
                }
            }
        }
    }
}

/// Parse a shape from a collider definition.
pub fn parse_collider_shape(col_def: &crate::scene::ColliderDef) -> PhysicsShape {
    match col_def.shape.as_str() {
        "sphere" => PhysicsShape::Sphere {
            radius: col_def.radius.unwrap_or(0.5),
        },
        "capsule" => PhysicsShape::Capsule {
            half_height: col_def.half_height.unwrap_or(0.5),
            radius: col_def.radius.unwrap_or(0.3),
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

/// Spawn all entities headlessly (no GPU resources needed).
/// Skips MeshRenderer and GaussianSplat components. Used for test runner.
pub fn spawn_all_entities_headless(
    scene_world: &mut SceneWorld,
    scene: &SceneFile,
    physics_world: &mut PhysicsWorld,
) {
    for entity_def in &scene.entities {
        spawn_entity_headless(scene_world, entity_def, physics_world);
    }
    scene_world.current_scene = Some(scene.clone());
    tracing::info!(
        "Scene '{}' loaded headless: {} entities",
        scene.name,
        scene.entities.len()
    );
}

/// Spawn a single entity headlessly (no GPU resources).
fn spawn_entity_headless(
    scene_world: &mut SceneWorld,
    entity_def: &EntityDef,
    physics_world: &mut PhysicsWorld,
) {
    let entity_id = EntityId(entity_def.id.clone());
    let tags = Tags(entity_def.tags.clone());

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

    // Skip gaussian_splat entities in headless mode (rendering only)
    if entity_def.components.gaussian_splat.is_some() {
        return;
    }

    // Spawn entity with non-GPU components
    let entity = if let Some(cam) = &entity_def.components.camera {
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
        if let Some(pl) = &entity_def.components.point_light {
            let point_light = PointLight {
                color: glam::Vec3::from(pl.color),
                intensity: pl.intensity,
                range: pl.range,
            };
            scene_world.world.spawn((entity_id, tags, transform, camera, point_light))
        } else {
            scene_world.world.spawn((entity_id, tags, transform, camera))
        }
    } else if let Some(pl) = &entity_def.components.point_light {
        let point_light = PointLight {
            color: glam::Vec3::from(pl.color),
            intensity: pl.intensity,
            range: pl.range,
        };
        scene_world.world.spawn((entity_id, tags, transform, point_light))
    } else if let Some(dl) = &entity_def.components.directional_light {
        let dir_light = crate::components::DirectionalLight {
            direction: glam::Vec3::from(dl.direction).normalize(),
            color: glam::Vec3::from(dl.color),
            intensity: dl.intensity,
            shadow_extent: dl.shadow_extent,
        };
        scene_world.world.spawn((entity_id, tags, transform, dir_light))
    } else {
        scene_world.world.spawn((entity_id, tags, transform))
    };

    scene_world.entity_registry.insert(entity_def.id.clone(), entity);

    // Spawn physics components
    let pos = if let Some(t) = &entity_def.components.transform {
        glam::Vec3::from(t.position)
    } else {
        glam::Vec3::ZERO
    };
    let rot = if let Some(t) = &entity_def.components.transform {
        euler_degrees_to_quat(t.rotation)
    } else {
        glam::Quat::IDENTITY
    };

    if let Some(cc_def) = &entity_def.components.character_controller {
        let half_height = cc_def.height / 2.0 - cc_def.radius;
        let (rb_handle, col_handle) =
            physics_world.add_character_body(entity, pos, half_height.max(0.1), cc_def.radius);

        let rb_comp = physics::RigidBody {
            handle: rb_handle,
            body_type: physics::PhysicsBodyType::Kinematic,
        };
        let col_comp = physics::Collider {
            handle: col_handle,
            shape: PhysicsShape::Capsule {
                half_height: half_height.max(0.1),
                radius: cc_def.radius,
            },
            is_trigger: false,
        };
        let cc_comp = CharacterController {
            move_speed: cc_def.move_speed,
            sprint_multiplier: cc_def.sprint_multiplier,
            jump_impulse: cc_def.jump_impulse,
            step_height: cc_def.step_height,
            ..Default::default()
        };
        let player = Player {
            height: cc_def.height,
            radius: cc_def.radius,
            ..Default::default()
        };
        let _ = scene_world.world.insert(entity, (rb_comp, col_comp, cc_comp, player));
    } else if let Some(col_def) = &entity_def.components.collider {
        let shape = parse_collider_shape(col_def);
        let is_trigger = col_def.is_trigger;
        let restitution = col_def.restitution;
        let friction = col_def.friction;
        let body_type = entity_def
            .components
            .rigid_body
            .as_ref()
            .map(|rb| rb.body_type.as_str())
            .unwrap_or("static");

        match body_type {
            "dynamic" => {
                let mass = entity_def
                    .components
                    .rigid_body
                    .as_ref()
                    .map(|rb| rb.mass)
                    .unwrap_or(1.0);
                let (rb_handle, col_handle) =
                    physics_world.add_dynamic_body(entity, pos, rot, shape.clone(), mass, restitution, friction);
                let rb_comp = physics::RigidBody {
                    handle: rb_handle,
                    body_type: physics::PhysicsBodyType::Dynamic,
                };
                let col_comp = physics::Collider {
                    handle: col_handle,
                    shape,
                    is_trigger,
                };
                let _ = scene_world.world.insert(entity, (rb_comp, col_comp));
            }
            _ => {
                let (rb_handle, col_handle) =
                    physics_world.add_static_body(entity, pos, rot, shape.clone(), is_trigger, restitution, friction);
                let rb_comp = physics::RigidBody {
                    handle: rb_handle,
                    body_type: physics::PhysicsBodyType::Static,
                };
                let col_comp = physics::Collider {
                    handle: col_handle,
                    shape,
                    is_trigger,
                };
                let _ = scene_world.world.insert(entity, (rb_comp, col_comp));
            }
        }
    }
}

/// Reconcile a scene update: diff old vs new, spawn/despawn/patch entities.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_scene(
    scene_world: &mut SceneWorld,
    new_scene: &SceneFile,
    device: &wgpu::Device,
    project_root: &Path,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    splat_cache: &mut SplatCache,
    physics_world: Option<&mut PhysicsWorld>,
) {
    let old_scene = match &scene_world.current_scene {
        Some(s) => s.clone(),
        None => {
            spawn_all_entities(scene_world, new_scene, device, project_root, mesh_cache, material_cache, splat_cache, physics_world);
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
            spawn_entity(scene_world, entity_def, device, project_root, mesh_cache, material_cache, splat_cache, None);
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
