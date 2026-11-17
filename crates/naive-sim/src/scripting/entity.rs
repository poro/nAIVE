//! Lua entity API registration.

use super::{ScriptRuntime, SharedPhysicsWorld, SharedSceneWorld};
use crate::components::{EntityId, Health, MaterialOverride, PointLight, Tags, Transform};
use mlua::prelude::*;

impl ScriptRuntime {
    /// Register entity manipulation API (get/set position, rotation, light).
    pub fn register_entity_api(
        &self,
        scene_world: SharedSceneWorld,
        physics_world: SharedPhysicsWorld,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let entity_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // entity.get_position(entity_string_id) -> x, y, z
        let sw = scene_world.clone();
        let get_pos_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(transform) = sw.world.get::<&Transform>(entity) {
                        return Ok((
                            transform.position.x,
                            transform.position.y,
                            transform.position.z,
                        ));
                    }
                }
                Ok((0.0f32, 0.0f32, 0.0f32))
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("get_position", get_pos_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_position(entity_string_id, x, y, z)
        let sw = scene_world.clone();
        let set_pos_fn = self
            .lua
            .create_function(move |_, (id, x, y, z): (String, f32, f32, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
                        transform.position = glam::Vec3::new(x, y, z);
                        transform.dirty = true;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_position", set_pos_fn)
            .map_err(|e| e.to_string())?;

        // entity.get_rotation(entity_string_id) -> pitch_deg, yaw_deg, roll_deg
        let sw = scene_world.clone();
        let get_rot_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(transform) = sw.world.get::<&Transform>(entity) {
                        let (yaw_rad, pitch_rad, roll_rad) =
                            transform.rotation.to_euler(glam::EulerRot::YXZ);
                        return Ok((
                            pitch_rad.to_degrees(),
                            yaw_rad.to_degrees(),
                            roll_rad.to_degrees(),
                        ));
                    }
                }
                Ok((0.0f32, 0.0f32, 0.0f32))
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("get_rotation", get_rot_fn)
            .map_err(|e| e.to_string())?;

        // entity.exists(entity_string_id) -> bool
        let sw = scene_world.clone();
        let exists_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                Ok(sw.entity_registry.contains_key(&id))
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("exists", exists_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_rotation(entity_string_id, pitch_deg, yaw_deg, roll_deg)
        let sw = scene_world.clone();
        let pw = physics_world.clone();
        let set_rot_fn = self
            .lua
            .create_function(move |_, (id, pitch, yaw, roll): (String, f32, f32, f32)| {
                let sw = sw.borrow_mut();
                let new_rot = crate::world::euler_degrees_to_quat([pitch, yaw, roll]);
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
                        transform.rotation = new_rot;
                        transform.dirty = true;
                    }
                    // Sync rotation to physics for kinematic bodies
                    if let Ok(rb) = sw.world.get::<&crate::physics::RigidBody>(entity) {
                        if rb.body_type == crate::physics::PhysicsBodyType::Kinematic {
                            let rb_handle = rb.handle;
                            drop(rb);
                            let mut pw = pw.borrow_mut();
                            if let Some(body) = pw.rigid_body_set.get_mut(rb_handle) {
                                let pose = rapier3d::math::Pose::from_parts(
                                    body.position().translation,
                                    crate::physics::to_rapier_rot(new_rot.normalize()),
                                );
                                body.set_next_kinematic_position(pose);
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_rotation", set_rot_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_light(entity_string_id, intensity)
        let sw = scene_world.clone();
        let set_light_fn = self
            .lua
            .create_function(move |_, (id, intensity): (String, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut light) = sw.world.get::<&mut PointLight>(entity) {
                        light.intensity = intensity;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_light", set_light_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_light_color(entity_string_id, r, g, b)
        let sw = scene_world.clone();
        let set_light_color_fn = self
            .lua
            .create_function(move |_, (id, r, g, b): (String, f32, f32, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut light) = sw.world.get::<&mut PointLight>(entity) {
                        light.color = glam::Vec3::new(r, g, b);
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_light_color", set_light_color_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_emission(entity_string_id, r, g, b)
        let sw = scene_world.clone();
        let set_emission_fn = self
            .lua
            .create_function(move |_, (id, r, g, b): (String, f32, f32, f32)| {
                let mut sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                    if has_override {
                        if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity)
                        {
                            mat_override.emission = Some([r, g, b]);
                        }
                    } else {
                        let _ = sw.world.insert_one(
                            entity,
                            MaterialOverride {
                                emission: Some([r, g, b]),
                                ..Default::default()
                            },
                        );
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_emission", set_emission_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_roughness(entity_string_id, value)
        let sw = scene_world.clone();
        let set_roughness_fn = self
            .lua
            .create_function(move |_, (id, value): (String, f32)| {
                let mut sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                    if has_override {
                        if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity)
                        {
                            mat_override.roughness = Some(value);
                        }
                    } else {
                        let _ = sw.world.insert_one(
                            entity,
                            MaterialOverride {
                                roughness: Some(value),
                                ..Default::default()
                            },
                        );
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_roughness", set_roughness_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_metallic(entity_string_id, value)
        let sw = scene_world.clone();
        let set_metallic_fn = self
            .lua
            .create_function(move |_, (id, value): (String, f32)| {
                let mut sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                    if has_override {
                        if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity)
                        {
                            mat_override.metallic = Some(value);
                        }
                    } else {
                        let _ = sw.world.insert_one(
                            entity,
                            MaterialOverride {
                                metallic: Some(value),
                                ..Default::default()
                            },
                        );
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_metallic", set_metallic_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_base_color(entity_string_id, r, g, b)
        let sw = scene_world.clone();
        let set_base_color_fn = self
            .lua
            .create_function(move |_, (id, r, g, b): (String, f32, f32, f32)| {
                let mut sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    let has_override = sw.world.get::<&MaterialOverride>(entity).is_ok();
                    if has_override {
                        if let Ok(mut mat_override) = sw.world.get::<&mut MaterialOverride>(entity)
                        {
                            mat_override.base_color = Some([r, g, b]);
                        }
                    } else {
                        let _ = sw.world.insert_one(
                            entity,
                            MaterialOverride {
                                base_color: Some([r, g, b]),
                                ..Default::default()
                            },
                        );
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_base_color", set_base_color_fn)
            .map_err(|e| e.to_string())?;

        // entity.get_health(id) -> current, max
        let sw = scene_world.clone();
        let get_health_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(health) = sw.world.get::<&Health>(entity) {
                        return Ok((health.current, health.max));
                    }
                }
                Ok((0.0f32, 0.0f32))
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("get_health", get_health_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_health(id, current, max)
        let sw = scene_world.clone();
        let set_health_fn = self
            .lua
            .create_function(move |_, (id, current, max): (String, f32, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                        health.current = current;
                        health.max = max;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_health", set_health_fn)
            .map_err(|e| e.to_string())?;

        // entity.damage(id, amount) -> new_current
        let sw = scene_world.clone();
        let damage_fn = self
            .lua
            .create_function(move |_, (id, amount): (String, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                        health.current = (health.current - amount).max(0.0);
                        return Ok(health.current);
                    }
                }
                Ok(0.0f32)
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("damage", damage_fn)
            .map_err(|e| e.to_string())?;

        // entity.heal(id, amount) -> new_current
        let sw = scene_world.clone();
        let heal_fn = self
            .lua
            .create_function(move |_, (id, amount): (String, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                        health.current = (health.current + amount).min(health.max);
                        return Ok(health.current);
                    }
                }
                Ok(0.0f32)
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("heal", heal_fn)
            .map_err(|e| e.to_string())?;

        // entity.is_alive(id) -> bool
        let sw = scene_world.clone();
        let is_alive_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(health) = sw.world.get::<&Health>(entity) {
                        return Ok(health.current > 0.0 && !health.dead);
                    }
                }
                Ok(true) // entities without health are considered alive
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("is_alive", is_alive_fn)
            .map_err(|e| e.to_string())?;

        // entity.has_tag(id, tag) -> bool
        let sw = scene_world.clone();
        let has_tag_fn = self
            .lua
            .create_function(move |_, (id, tag): (String, String)| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(tags) = sw.world.get::<&Tags>(entity) {
                        return Ok(tags.0.contains(&tag));
                    }
                }
                Ok(false)
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("has_tag", has_tag_fn)
            .map_err(|e| e.to_string())?;

        // entity.add_tag(id, tag)
        let sw = scene_world.clone();
        let add_tag_fn = self
            .lua
            .create_function(move |_, (id, tag): (String, String)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut tags) = sw.world.get::<&mut Tags>(entity) {
                        if !tags.0.contains(&tag) {
                            tags.0.push(tag);
                        }
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("add_tag", add_tag_fn)
            .map_err(|e| e.to_string())?;

        // entity.remove_tag(id, tag)
        let sw = scene_world.clone();
        let remove_tag_fn = self
            .lua
            .create_function(move |_, (id, tag): (String, String)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut tags) = sw.world.get::<&mut Tags>(entity) {
                        tags.0.retain(|t| t != &tag);
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("remove_tag", remove_tag_fn)
            .map_err(|e| e.to_string())?;

        // entity.get_tag(id) -> first tag string or nil
        let sw = scene_world.clone();
        let get_tag_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(tags) = sw.world.get::<&Tags>(entity) {
                        if let Some(first) = tags.0.first() {
                            return Ok(Some(first.clone()));
                        }
                    }
                }
                Ok(None)
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("get_tag", get_tag_fn)
            .map_err(|e| e.to_string())?;

        // entity.get_tags(id) -> table of all tags
        let sw = scene_world.clone();
        let get_tags_fn = self
            .lua
            .create_function(move |lua, id: String| {
                let sw = sw.borrow();
                let result = lua.create_table()?;
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(tags) = sw.world.get::<&Tags>(entity) {
                        for (i, tag) in tags.0.iter().enumerate() {
                            result.set(i + 1, tag.clone())?;
                        }
                    }
                }
                Ok(result)
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("get_tags", get_tags_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("entity", entity_table)
            .map_err(|e| e.to_string())?;

        // --- scene table (Tier 2: Runtime Entity Queries) ---
        let scene_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // scene.find_by_tag(tag) -> {entity_id1, entity_id2, ...}
        let sw = scene_world.clone();
        let find_by_tag_fn = self
            .lua
            .create_function(move |lua, tag: String| {
                let sw = sw.borrow();
                let result = lua.create_table()?;
                let mut idx = 1;
                for (_entity, (tags, entity_id)) in sw
                    .world
                    .query::<(hecs::Entity, (&Tags, &EntityId))>()
                    .iter()
                {
                    if tags.0.contains(&tag) {
                        result.set(idx, entity_id.0.clone())?;
                        idx += 1;
                    }
                }
                Ok(result)
            })
            .map_err(|e| e.to_string())?;
        scene_table
            .set("find_by_tag", find_by_tag_fn)
            .map_err(|e| e.to_string())?;

        // scene.find_by_tags(tag1, tag2, ...) -> entities with ALL specified tags
        let sw = scene_world.clone();
        let find_by_tags_fn = self
            .lua
            .create_function(move |lua, tags_arg: LuaMultiValue| {
                let sw = sw.borrow();
                let required_tags: Vec<String> = tags_arg
                    .into_iter()
                    .filter_map(|v| {
                        if let LuaValue::String(s) = v {
                            Some(s.to_string_lossy().to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                let result = lua.create_table()?;
                let mut idx = 1;
                for (_entity, (tags, entity_id)) in sw
                    .world
                    .query::<(hecs::Entity, (&Tags, &EntityId))>()
                    .iter()
                {
                    if required_tags.iter().all(|rt| tags.0.contains(rt)) {
                        result.set(idx, entity_id.0.clone())?;
                        idx += 1;
                    }
                }
                Ok(result)
            })
            .map_err(|e| e.to_string())?;
        scene_table
            .set("find_by_tags", find_by_tags_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("scene", scene_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
