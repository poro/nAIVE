//! GPU-free entity construction from an authored definition and resolved handles.

use hecs::World;

use crate::components::*;
use crate::physics::{self, CharacterController, PhysicsShape, PhysicsTrimesh, PhysicsWorld};
use crate::scene::{EntityDef, SceneFile};

use super::{euler_degrees_to_quat, parse_collider_shape, SceneEntityDefinition, SceneWorld};

/// Resolve backend-specific assets without exposing GPU resources to construction.
pub trait SpawnAssetResolver {
    fn mesh(&mut self, path: &str) -> Result<MeshHandle, String>;
    fn material(&mut self, path: &str) -> Result<MaterialHandle, String>;
    fn splat(&mut self, path: &str) -> Result<SplatHandle, String>;
    fn trimesh(&mut self, mesh: MeshHandle, scale: glam::Vec3) -> Result<PhysicsTrimesh, String>;
}

impl SpawnAssetResolver for naive_sim::assets::AssetCatalog {
    fn mesh(&mut self, path: &str) -> Result<MeshHandle, String> {
        naive_sim::assets::AssetCatalog::mesh(self, path).map_err(|error| error.to_string())
    }
    fn material(&mut self, path: &str) -> Result<MaterialHandle, String> {
        naive_sim::assets::AssetCatalog::material(self, path).map_err(|error| error.to_string())
    }
    fn splat(&mut self, path: &str) -> Result<SplatHandle, String> {
        self.validate_splat(path).map_err(|error| error.to_string())
    }
    fn trimesh(&mut self, mesh: MeshHandle, scale: glam::Vec3) -> Result<PhysicsTrimesh, String> {
        self.mesh_data(mesh)
            .map(|mesh| mesh.physics_trimesh(scale))
            .ok_or_else(|| "resolved mesh has no CPU triangle geometry".into())
    }
}

/// Opaque asset handles and resolved geometry, ready for CPU construction.
pub(crate) struct PreparedSpawnAssets {
    mesh: Option<MeshRenderer>,
    splat: Option<GaussianSplat>,
    swarm: Option<GpuSwarm>,
    trimesh: Option<PhysicsTrimesh>,
}

impl PreparedSpawnAssets {
    #[cfg(test)]
    pub(super) fn schema_fixture(definition: &EntityDef) -> Self {
        Self {
            mesh: None,
            splat: None,
            swarm: definition
                .components
                .gpu_swarm
                .as_ref()
                .map(|def| GpuSwarm {
                    def: def.clone(),
                    mesh_handle: MeshHandle(0),
                    material_handle: MaterialHandle(0),
                }),
            trimesh: None,
        }
    }
}

/// Resolve every required asset before allocating an entity or physics body.
pub(crate) fn prepare_spawn_assets<R: SpawnAssetResolver>(
    definition: &EntityDef,
    resolver: &mut R,
) -> Result<PreparedSpawnAssets, String> {
    let prepare = |resolver: &mut R| -> Result<PreparedSpawnAssets, String> {
        let splat = definition
            .components
            .gaussian_splat
            .as_ref()
            .map(|splat| {
                resolver
                    .splat(&splat.source)
                    .map(|splat_handle| GaussianSplat { splat_handle })
            })
            .transpose()?;
        // An authored splat takes precedence over the ordinary mesh renderer.
        let mesh = if splat.is_some() {
            None
        } else {
            definition
                .components
                .mesh_renderer
                .as_ref()
                .map(|mesh| {
                    Ok::<_, String>(MeshRenderer {
                        mesh_handle: resolver.mesh(&mesh.mesh)?,
                        material_handle: resolver.material(&mesh.material)?,
                    })
                })
                .transpose()?
        };
        let swarm = definition
            .components
            .gpu_swarm
            .as_ref()
            .map(|swarm| {
                Ok::<_, String>(GpuSwarm {
                    def: swarm.clone(),
                    mesh_handle: resolver.mesh(&swarm.mesh)?,
                    material_handle: resolver.material(&swarm.material)?,
                })
            })
            .transpose()?;
        let trimesh = if definition.components.character_controller.is_none()
            && definition
                .components
                .collider
                .as_ref()
                .is_some_and(|collider| collider.shape == "trimesh")
        {
            let mesh = mesh
                .as_ref()
                .ok_or("trimesh collider requires a mesh_renderer")?;
            let scale = definition
                .components
                .transform
                .as_ref()
                .map(|transform| glam::Vec3::from(transform.scale))
                .unwrap_or(glam::Vec3::ONE);
            let geometry = resolver.trimesh(mesh.mesh_handle, scale)?;
            validate_trimesh(&geometry)?;
            Some(geometry)
        } else {
            None
        };
        Ok(PreparedSpawnAssets {
            mesh,
            splat,
            swarm,
            trimesh,
        })
    };
    prepare(resolver).map_err(|error| format!("Entity '{}': {error}", definition.id))
}

fn validate_trimesh((vertices, indices): &PhysicsTrimesh) -> Result<(), String> {
    if vertices.is_empty()
        || indices.is_empty()
        || vertices
            .iter()
            .any(|vertex| !vertex.x.is_finite() || !vertex.y.is_finite() || !vertex.z.is_finite())
        || indices
            .iter()
            .flatten()
            .any(|&index| index as usize >= vertices.len())
    {
        return Err("trimesh collider requires finite vertices and valid triangle indices".into());
    }
    Ok(())
}

/// Validate a complete scene before tearing down or populating live state.
/// Prepared geometry is ephemeral; resolvers retain only their asset caches.
pub(crate) fn preflight_scene(
    scene: &SceneFile,
    resolver: &mut impl SpawnAssetResolver,
) -> Result<(), String> {
    let mut ids = std::collections::HashSet::with_capacity(scene.entities.len());
    for definition in &scene.entities {
        if !ids.insert(definition.id.as_str()) {
            return Err(format!(
                "Scene '{}': duplicate entity ID '{}'",
                scene.name, definition.id
            ));
        }
    }
    for definition in &scene.entities {
        prepare_spawn_assets(definition, resolver)?;
    }
    Ok(())
}

/// The shared native/headless spawning path. Duplicates do not load assets.
pub fn spawn_entity_with_resolver(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    physics_world: Option<&mut PhysicsWorld>,
    resolver: &mut impl SpawnAssetResolver,
) -> Result<hecs::Entity, String> {
    if scene_world.entity_registry.contains_key(&definition.id) {
        return Err(format!("Entity '{}' already exists", definition.id));
    }
    let prepared = prepare_spawn_assets(definition, resolver)?;
    construct_entity(scene_world, definition, prepared, physics_world)
}

/// The sole CPU constructor. It receives no device, queue or renderer caches.
pub(crate) fn construct_entity(
    scene_world: &mut SceneWorld,
    definition: &EntityDef,
    mut assets: PreparedSpawnAssets,
    physics_world: Option<&mut PhysicsWorld>,
) -> Result<hecs::Entity, String> {
    if scene_world.entity_registry.contains_key(&definition.id) {
        return Err(format!("Entity '{}' already exists", definition.id));
    }
    let components = &definition.components;
    let shape = if components.character_controller.is_some() {
        None
    } else {
        components
            .collider
            .as_ref()
            .map(|collider| {
                if collider.shape == "trimesh" {
                    assets
                        .trimesh
                        .take()
                        .map(|(vertices, indices)| PhysicsShape::Trimesh { vertices, indices })
                        .ok_or_else(|| {
                            format!(
                                "Entity '{}': trimesh collider has no resolved geometry",
                                definition.id
                            )
                        })
                } else {
                    Ok(parse_collider_shape(collider))
                }
            })
            .transpose()?
    };
    let transform = components
        .transform
        .as_ref()
        .map(|transform| Transform {
            position: transform.position.into(),
            rotation: euler_degrees_to_quat(transform.rotation),
            scale: transform.scale.into(),
            world_matrix: glam::Mat4::IDENTITY,
            parent: None,
            dirty: true,
        })
        .unwrap_or_default();
    let position = transform.position;
    let rotation = transform.rotation;
    let entity = scene_world.world.spawn((
        EntityId(definition.id.clone()),
        Tags(definition.tags.clone()),
        transform,
    ));
    attach_definition_components(&mut scene_world.world, entity, definition);
    if let Some(mesh) = assets.mesh {
        let _ = scene_world.world.insert_one(entity, mesh);
    }
    if let Some(splat) = assets.splat {
        let _ = scene_world.world.insert_one(entity, splat);
    }
    if let Some(swarm) = assets.swarm {
        let _ = scene_world.world.insert_one(entity, swarm);
    }
    if let Some(health) = &components.health {
        let _ = scene_world.world.insert_one(
            entity,
            Health {
                current: health.current.unwrap_or(health.max),
                max: health.max,
                dead: false,
            },
        );
    }
    if let Some(damage) = &components.collision_damage {
        let _ = scene_world.world.insert_one(
            entity,
            CollisionDamage {
                damage: damage.damage,
                destroy_on_hit: damage.destroy_on_hit,
            },
        );
    }
    if let Some(physics) = physics_world {
        if let Some(controller) = &components.character_controller {
            let half_height = (controller.height / 2.0 - controller.radius).max(0.1);
            let (body, collider) =
                physics.add_character_body(entity, position, half_height, controller.radius);
            let _ = scene_world.world.insert(
                entity,
                (
                    physics::RigidBody {
                        handle: body,
                        body_type: physics::PhysicsBodyType::Kinematic,
                    },
                    physics::Collider {
                        handle: collider,
                        shape: PhysicsShape::Capsule {
                            half_height,
                            radius: controller.radius,
                        },
                        is_trigger: false,
                    },
                    CharacterController {
                        move_speed: controller.move_speed,
                        sprint_multiplier: controller.sprint_multiplier,
                        jump_impulse: controller.jump_impulse,
                        step_height: controller.step_height,
                        ..Default::default()
                    },
                    Player {
                        height: controller.height,
                        radius: controller.radius,
                        ..Default::default()
                    },
                ),
            );
        } else if let (Some(collider), Some(shape)) = (&components.collider, shape) {
            let kind = components
                .rigid_body
                .as_ref()
                .map(|body| body.body_type.as_str())
                .unwrap_or("static");
            let (body, handle, body_type) = match kind {
                "dynamic" => {
                    let mass = components
                        .rigid_body
                        .as_ref()
                        .map(|body| body.mass)
                        .unwrap_or(1.0);
                    let ccd = components.rigid_body.as_ref().is_some_and(|body| body.ccd);
                    let (body, handle) = physics.add_dynamic_body_with_sensor(
                        entity,
                        position,
                        rotation,
                        shape.clone(),
                        mass,
                        collider.restitution,
                        collider.friction,
                        ccd,
                        collider.is_trigger,
                    );
                    (body, handle, physics::PhysicsBodyType::Dynamic)
                }
                "kinematic" => {
                    let (body, handle) = physics.add_kinematic_body(
                        entity,
                        position,
                        rotation,
                        shape.clone(),
                        collider.is_trigger,
                        collider.restitution,
                        collider.friction,
                    );
                    (body, handle, physics::PhysicsBodyType::Kinematic)
                }
                _ => {
                    let (body, handle) = physics.add_static_body(
                        entity,
                        position,
                        rotation,
                        shape.clone(),
                        collider.is_trigger,
                        collider.restitution,
                        collider.friction,
                    );
                    (body, handle, physics::PhysicsBodyType::Static)
                }
            };
            let _ = scene_world.world.insert(
                entity,
                (
                    physics::RigidBody {
                        handle: body,
                        body_type,
                    },
                    physics::Collider {
                        handle,
                        shape,
                        is_trigger: collider.is_trigger,
                    },
                ),
            );
        }
    }
    scene_world
        .entity_registry
        .insert(definition.id.clone(), entity);
    Ok(entity)
}

/// Attach independent authored components consistently, even when an entity
/// also carries a mesh, a camera, a light or a splat.
fn attach_definition_components(world: &mut World, entity: hecs::Entity, definition: &EntityDef) {
    let _ = world.insert_one(entity, SceneEntityDefinition(definition.clone()));
    let components = &definition.components;
    if let Some(script) = &components.script {
        let _ = world.insert_one(
            entity,
            Script {
                source: script.source.clone().into(),
                initialized: false,
            },
        );
    }
    if let Some(camera) = &components.camera {
        let _ = world.insert_one(
            entity,
            Camera {
                fov_degrees: camera.fov,
                near: camera.near,
                far: camera.far,
                role: if camera.role == "main" {
                    CameraRole::Main
                } else {
                    CameraRole::Other(camera.role.clone())
                },
                aspect_ratio: 16.0 / 9.0,
            },
        );
        if camera.mode == "third_person" {
            let limits = camera.pitch_limits.unwrap_or([-60.0, 75.0]);
            let _ = world.insert_one(
                entity,
                CameraMode::ThirdPerson {
                    distance: camera.distance,
                    height_offset: camera.height_offset,
                    pitch_min: limits[0].to_radians(),
                    pitch_max: limits[1].to_radians(),
                },
            );
        }
    }
    if let Some(light) = &components.point_light {
        let _ = world.insert_one(
            entity,
            PointLight {
                color: light.color.into(),
                intensity: light.intensity,
                range: light.range,
            },
        );
    }
    if let Some(light) = &components.directional_light {
        let _ = world.insert_one(
            entity,
            DirectionalLight {
                direction: glam::Vec3::from(light.direction).normalize_or_zero(),
                color: light.color.into(),
                intensity: light.intensity,
                shadow_extent: light.shadow_extent,
            },
        );
    }
    if let Some(emitter) = &components.particle_emitter {
        let _ = world.insert_one(
            entity,
            ParticleEmitter {
                config: ParticleConfig {
                    max_particles: emitter.max_particles,
                    spawn_rate: emitter.spawn_rate,
                    lifetime: emitter.lifetime,
                    initial_speed: emitter.initial_speed,
                    direction: emitter.direction.into(),
                    spread: emitter.spread,
                    size: emitter.size,
                    color_start: emitter.color_start,
                    color_end: emitter.color_end,
                    gravity_scale: emitter.gravity_scale,
                },
                enabled: emitter.enabled,
            },
        );
    }
}
