//! Rebuild a scene document from authored definitions and live ECS values.
//!
//! Opaque handles cannot recover script paths, mesh flags, unknown component
//! data or other authoring information. Constructors retain that data on each
//! entity; snapshots update the properties that can be changed at runtime.

use crate::components::*;
use crate::physics::{Collider, PhysicsBodyType, PhysicsShape, PhysicsWorld, RigidBody};
use crate::scene::*;
use crate::world::{SceneEntityDefinition, SceneWorld, ScriptSpawned, UnsavedRuntimeState};

/// Read-only asset names needed to rebuild renderer-independent scene files.
/// Handles belong to the asset store that populated the world. Return its
/// registered names, including `runtime:` and `generated:` identifiers, so the
/// snapshot can reject assets without a scene-file representation.
pub trait SnapshotAssetLookup {
    fn mesh_name(&self, handle: MeshHandle) -> Option<String>;
    fn material_name(&self, handle: MaterialHandle) -> Option<String>;
}

/// A scene snapshot preserves scene-authoring state, not an arbitrary savegame.
/// Unsupported transient state is reported instead of silently discarded.
/// Entities scripts spawned ([`ScriptSpawned`]) are left out: they are the
/// scripts' output, and the scripts create them again when the scene loads.
pub fn snapshot_scene(
    world: &SceneWorld,
    assets: &impl SnapshotAssetLookup,
    physics: Option<&PhysicsWorld>,
) -> Result<SceneFile, String> {
    let mut ids: Vec<_> = world.entity_registry.keys().collect();
    ids.sort();
    let mut entities = Vec::with_capacity(ids.len());
    for id in ids {
        let entity = world.entity_registry[id];
        if !world.world.contains(entity) {
            return Err(format!("Entity registry contains missing entity '{id}'"));
        }
        if world.world.get::<&ScriptSpawned>(entity).is_ok() {
            continue;
        }
        if let Ok(state) = world.world.get::<&UnsavedRuntimeState>(entity) {
            return Err(format!("Cannot save entity '{id}': {}", state.0));
        }
        if world.world.get::<&Projectile>(entity).is_ok() {
            return Err(format!("Cannot save transient projectile '{id}': scene files do not represent projectile lifetime or velocity"));
        }
        if world.world.get::<&Hidden>(entity).is_ok() {
            return Err(format!("Cannot save hidden entity '{id}': scene files do not represent visibility overrides"));
        }
        if let Ok(material) = world.world.get::<&MaterialOverride>(entity) {
            if material.base_color.is_some()
                || material.emission.is_some()
                || material.roughness.is_some()
                || material.metallic.is_some()
            {
                return Err(format!(
                    "Cannot save material override on '{id}': write a material asset first"
                ));
            }
        }
        let mut definition = world
            .world
            .get::<&SceneEntityDefinition>(entity)
            .ok()
            .map(|definition| definition.0.clone())
            .or_else(|| {
                world
                    .current_scene
                    .as_ref()?
                    .entities
                    .iter()
                    .find(|definition| definition.id == *id)
                    .cloned()
            })
            .unwrap_or_else(|| EntityDef {
                id: id.clone(),
                tags: Vec::new(),
                extends: None,
                components: ComponentMap::default(),
            });
        definition.id.clone_from(id);
        // All component values are resolved already. A saved entity must not
        // depend on a template entity that might have been deleted at runtime.
        definition.extends = None;
        let components = &mut definition.components;

        if let Ok(transform) = world.world.get::<&Transform>(entity) {
            // Keep authored Euler values when the quaternion is unchanged,
            // including rotations beyond the canonical +/-180 degree range.
            // Omitted default transforms remain omitted until an edit occurs.
            let mut saved = components.transform.clone().unwrap_or(TransformDef {
                position: [0.; 3],
                rotation: [0.; 3],
                scale: [1.; 3],
            });
            let authored_rotation = crate::world::euler_degrees_to_quat(saved.rotation);
            let rotation_changed =
                authored_rotation != transform.rotation && authored_rotation != -transform.rotation;
            let changed = saved.position != transform.position.to_array()
                || saved.scale != transform.scale.to_array()
                || rotation_changed;
            if components.transform.is_some() || changed {
                saved.position = transform.position.to_array();
                saved.scale = transform.scale.to_array();
                if rotation_changed {
                    let (yaw, pitch, roll) = transform.rotation.to_euler(glam::EulerRot::YXZ);
                    saved.rotation = [pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees()];
                }
                components.transform = Some(saved);
            }
        }
        if let Ok(tags) = world.world.get::<&Tags>(entity) {
            definition.tags.clone_from(&tags.0);
        }
        if let Ok(mesh) = world.world.get::<&MeshRenderer>(entity) {
            let previous = components.mesh_renderer.as_ref();
            let mesh_name = assets
                .mesh_name(mesh.mesh_handle)
                .or_else(|| previous.map(|mesh| mesh.mesh.clone()))
                .ok_or_else(|| format!("Cannot resolve mesh asset for '{id}'"))?;
            let material_name = assets
                .material_name(mesh.material_handle)
                .or_else(|| previous.map(|mesh| mesh.material.clone()))
                .ok_or_else(|| format!("Cannot resolve material asset for '{id}'"))?;
            if mesh_name.starts_with("runtime:") || mesh_name.starts_with("generated:") {
                return Err(format!(
                    "Cannot save generated mesh on '{id}': export it to a mesh asset first"
                ));
            }
            components.mesh_renderer = Some(MeshRendererDef {
                mesh: mesh_name,
                material: material_name,
                cast_shadows: previous.is_none_or(|mesh| mesh.cast_shadows),
                receive_shadows: previous.is_none_or(|mesh| mesh.receive_shadows),
            });
        }
        if let Ok(camera) = world.world.get::<&Camera>(entity) {
            let mut saved: CameraDef = components
                .camera
                .clone()
                .unwrap_or_else(|| serde_norway::from_str("{}").expect("camera defaults"));
            saved.fov = camera.fov_degrees;
            saved.near = camera.near;
            saved.far = camera.far;
            saved.role = match &camera.role {
                CameraRole::Main => "main".into(),
                CameraRole::Other(role) => role.clone(),
            };
            if let Ok(mode) = world.world.get::<&CameraMode>(entity) {
                match *mode {
                    CameraMode::FirstPerson => saved.mode = "first_person".into(),
                    CameraMode::ThirdPerson {
                        distance,
                        height_offset,
                        pitch_min,
                        pitch_max,
                    } => {
                        saved.mode = "third_person".into();
                        saved.distance = distance;
                        saved.height_offset = height_offset;
                        let previous = saved.pitch_limits.unwrap_or([-60.0, 75.0]);
                        let limits = [pitch_min.to_degrees(), pitch_max.to_degrees()];
                        if previous
                            .iter()
                            .zip(limits)
                            .any(|(old, new)| (old - new).abs() > 0.0001)
                        {
                            saved.pitch_limits = Some(limits);
                        }
                    }
                }
            }
            components.camera = Some(saved);
        } else {
            components.camera = None;
        }
        if let Ok(light) = world.world.get::<&PointLight>(entity) {
            components.point_light = Some(PointLightDef {
                color: light.color.to_array(),
                intensity: light.intensity,
                range: light.range,
            });
        } else {
            components.point_light = None;
        }
        if let Ok(light) = world.world.get::<&DirectionalLight>(entity) {
            let authored_direction = components
                .directional_light
                .as_ref()
                .map(|light| light.direction);
            let direction = authored_direction
                .filter(|direction| {
                    glam::Vec3::from(*direction).normalize_or_zero() == light.direction
                })
                .unwrap_or_else(|| light.direction.to_array());
            components.directional_light = Some(DirectionalLightDef {
                direction,
                color: light.color.to_array(),
                intensity: light.intensity,
                shadow_extent: light.shadow_extent,
            });
        } else {
            components.directional_light = None;
        }
        if let Ok(script) = world.world.get::<&Script>(entity) {
            components.script = Some(ScriptDef {
                source: script.source.to_string_lossy().into_owned(),
            });
        } else {
            components.script = None;
        }
        if let Ok(health) = world.world.get::<&Health>(entity) {
            let current = match &components.health {
                Some(previous) if previous.current.is_none() && health.current == health.max => {
                    None
                }
                _ => Some(health.current),
            };
            components.health = Some(HealthDef {
                max: health.max,
                current,
            });
        } else {
            components.health = None;
        }
        if let Ok(damage) = world.world.get::<&CollisionDamage>(entity) {
            components.collision_damage = Some(CollisionDamageDef {
                damage: damage.damage,
                destroy_on_hit: damage.destroy_on_hit,
            });
        } else {
            components.collision_damage = None;
        }
        if let Ok(emitter) = world.world.get::<&ParticleEmitter>(entity) {
            let config = &emitter.config;
            components.particle_emitter = Some(ParticleEmitterDef {
                max_particles: config.max_particles,
                spawn_rate: config.spawn_rate,
                lifetime: config.lifetime,
                initial_speed: config.initial_speed,
                direction: config.direction.to_array(),
                spread: config.spread,
                size: config.size,
                color_start: config.color_start,
                color_end: config.color_end,
                gravity_scale: config.gravity_scale,
                enabled: emitter.enabled,
            });
        } else {
            components.particle_emitter = None;
        }
        if let Ok(animator) = world.world.get::<&Animator>(entity) {
            components.animator = Some(AnimatorDef {
                state: animator.controller.current_state.clip_name().into(),
                speed: animator.controller.speed,
                looping: animator.controller.looping,
            });
        }
        if let Ok(swarm) = world.world.get::<&GpuSwarm>(entity) {
            components.gpu_swarm = Some(swarm.def.clone());
        }
        if let Ok(controller) = world
            .world
            .get::<&crate::physics::CharacterController>(entity)
        {
            let mut saved: CharacterControllerDef = components
                .character_controller
                .clone()
                .unwrap_or_else(|| serde_norway::from_str("{}").expect("character defaults"));
            saved.move_speed = controller.move_speed;
            saved.sprint_multiplier = controller.sprint_multiplier;
            saved.jump_impulse = controller.jump_impulse;
            saved.step_height = controller.step_height;
            if let Ok(player) = world.world.get::<&Player>(entity) {
                saved.height = player.height;
                saved.radius = player.radius;
            }
            components.character_controller = Some(saved);
        } else {
            if let Ok(body) = world.world.get::<&RigidBody>(entity) {
                let explicit_body = components.rigid_body.is_some();
                let mut saved = components.rigid_body.clone().unwrap_or(RigidBodyDef {
                    body_type: "static".into(),
                    mass: 1.0,
                    ccd: false,
                });
                saved.body_type = match body.body_type {
                    PhysicsBodyType::Dynamic => "dynamic",
                    PhysicsBodyType::Kinematic => "kinematic",
                    PhysicsBodyType::Static => "static",
                }
                .into();
                if let Some(body) =
                    physics.and_then(|physics| physics.rigid_body_set.get(body.handle))
                {
                    if body.is_dynamic() {
                        saved.ccd = body.is_ccd_enabled();
                        saved.mass = body.mass();
                    }
                }
                if explicit_body || saved.body_type != "static" || saved.ccd {
                    components.rigid_body = Some(saved);
                }
            }
            if let Ok(collider) = world.world.get::<&Collider>(entity) {
                let mut saved = components
                    .collider
                    .clone()
                    .unwrap_or_else(|| serde_norway::from_str("{}").expect("collider defaults"));
                saved.is_trigger = collider.is_trigger;
                match &collider.shape {
                    // Preserve authored trimesh metadata in legacy worlds
                    // whose live collider was represented by a fallback box.
                    PhysicsShape::Box { .. } if saved.shape == "trimesh" => {}
                    PhysicsShape::Box { half_extents } => {
                        if !matches!(crate::world::parse_collider_shape(&saved), PhysicsShape::Box { half_extents: previous } if previous == *half_extents)
                        {
                            saved.shape = "box".into();
                            saved.half_extents = Some(half_extents.to_array());
                        }
                    }
                    PhysicsShape::Sphere { radius } => {
                        if !matches!(crate::world::parse_collider_shape(&saved), PhysicsShape::Sphere { radius: previous } if previous == *radius)
                        {
                            saved.shape = "sphere".into();
                            saved.radius = Some(*radius);
                        }
                    }
                    PhysicsShape::Capsule {
                        radius,
                        half_height,
                    } => {
                        if !matches!(crate::world::parse_collider_shape(&saved), PhysicsShape::Capsule { radius: previous_radius, half_height: previous_height } if previous_radius == *radius && previous_height == *half_height)
                        {
                            saved.shape = "capsule".into();
                            saved.radius = Some(*radius);
                            saved.half_height = Some(*half_height);
                        }
                    }
                    PhysicsShape::Trimesh { .. } => saved.shape = "trimesh".into(),
                }
                if let Some(collider) =
                    physics.and_then(|physics| physics.collider_set.get(collider.handle))
                {
                    saved.restitution = collider.restitution();
                    saved.friction = collider.friction();
                }
                components.collider = Some(saved);
            }
        }
        entities.push(definition);
    }
    Ok(SceneFile {
        version: SCENE_FORMAT_VERSION,
        name: world
            .current_scene
            .as_ref()
            .map(|scene| scene.name.clone())
            .unwrap_or_else(|| "Editor Scene".into()),
        settings: world
            .current_scene
            .as_ref()
            .map(|scene| scene.settings.clone())
            .unwrap_or_default(),
        entities,
    })
}

#[cfg(test)]
mod tests;
