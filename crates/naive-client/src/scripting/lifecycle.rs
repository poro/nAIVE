//! Lua lifecycle API registration.

use super::{ScriptRuntime, SharedEntityCommandQueue, SharedEntityPoolManager, SharedSceneWorld};
use crate::components::{Health, Transform};
use crate::world::{PoolOp, ProjectileSpawnCommand};
use mlua::prelude::*;

impl ScriptRuntime {
    /// Register entity lifecycle commands (spawn, destroy, visibility, pooling)
    /// that are deferred via the EntityCommandQueue.
    pub fn register_entity_command_api(
        &self,
        scene_world: SharedSceneWorld,
        cmd_queue: SharedEntityCommandQueue,
        pool_manager: SharedEntityPoolManager,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let entity_table: LuaTable = globals.get("entity").map_err(|e| e.to_string())?;

        // entity.spawn(id, mesh, material, x, y, z, sx, sy, sz)
        let cmd = cmd_queue.clone();
        let spawn_fn = self
            .lua
            .create_function(
                move |_,
                      (id, mesh, mat, x, y, z, sx, sy, sz): (
                    String,
                    String,
                    String,
                    f32,
                    f32,
                    f32,
                    f32,
                    f32,
                    f32,
                )| {
                    let mut cmd = cmd.borrow_mut();
                    cmd.spawns.push(crate::world::SpawnCommand {
                        id,
                        mesh,
                        material: mat,
                        position: [x, y, z],
                        scale: [sx, sy, sz],
                        collider: None,
                        rigid_body: None,
                        script: None,
                    });
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        entity_table
            .set("spawn", spawn_fn)
            .map_err(|e| e.to_string())?;

        // entity.spawn_ex(config_table) — spawn with optional collider, rigid_body, and script
        // config_table: { id, mesh, material, position={x,y,z}, scale={x,y,z},
        //   collider={shape, half_extents, radius, is_trigger, restitution, friction},
        //   rigid_body="dynamic"|"fixed"|"kinematic", script="logic/foo.lua" }
        let cmd = cmd_queue.clone();
        let spawn_ex_fn = self
            .lua
            .create_function(move |_, tbl: LuaTable| {
                let id: String = tbl
                    .get("id")
                    .map_err(|e| mlua::Error::runtime(format!("spawn_ex: missing id: {}", e)))?;
                let mesh: String = tbl
                    .get("mesh")
                    .unwrap_or_else(|_| "procedural:cube".to_string());
                let material: String = tbl
                    .get("material")
                    .unwrap_or_else(|_| "procedural:default".to_string());

                let position = if let Ok(pos) = tbl.get::<LuaTable>("position") {
                    [
                        pos.get::<f32>(1).unwrap_or(0.0),
                        pos.get::<f32>(2).unwrap_or(0.0),
                        pos.get::<f32>(3).unwrap_or(0.0),
                    ]
                } else {
                    [
                        tbl.get::<f32>("x").unwrap_or(0.0),
                        tbl.get::<f32>("y").unwrap_or(0.0),
                        tbl.get::<f32>("z").unwrap_or(0.0),
                    ]
                };

                let scale = if let Ok(s) = tbl.get::<LuaTable>("scale") {
                    [
                        s.get::<f32>(1).unwrap_or(1.0),
                        s.get::<f32>(2).unwrap_or(1.0),
                        s.get::<f32>(3).unwrap_or(1.0),
                    ]
                } else {
                    [
                        tbl.get::<f32>("sx").unwrap_or(1.0),
                        tbl.get::<f32>("sy").unwrap_or(1.0),
                        tbl.get::<f32>("sz").unwrap_or(1.0),
                    ]
                };

                let collider = if let Ok(col_tbl) = tbl.get::<LuaTable>("collider") {
                    Some(crate::world::SpawnColliderSpec {
                        shape: col_tbl
                            .get::<String>("shape")
                            .unwrap_or_else(|_| "sphere".to_string()),
                        half_extents: col_tbl.get::<LuaTable>("half_extents").ok().map(|t| {
                            [
                                t.get::<f32>(1).unwrap_or(0.5),
                                t.get::<f32>(2).unwrap_or(0.5),
                                t.get::<f32>(3).unwrap_or(0.5),
                            ]
                        }),
                        radius: col_tbl.get::<f32>("radius").ok(),
                        is_trigger: col_tbl.get::<bool>("is_trigger").unwrap_or(false),
                        restitution: col_tbl.get::<f32>("restitution").unwrap_or(0.3),
                        friction: col_tbl.get::<f32>("friction").unwrap_or(0.5),
                    })
                } else {
                    None
                };

                let rigid_body: Option<String> = tbl.get("rigid_body").ok();
                let script: Option<String> = tbl.get("script").ok();

                let mut cmd = cmd.borrow_mut();
                cmd.spawns.push(crate::world::SpawnCommand {
                    id,
                    mesh,
                    material,
                    position,
                    scale,
                    collider,
                    rigid_body,
                    script,
                });
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("spawn_ex", spawn_ex_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_texture(id, slot, path) — swap albedo or normal texture at runtime
        // slot: "albedo" or "normal"
        let cmd = cmd_queue.clone();
        let set_tex_fn = self
            .lua
            .create_function(move |_, (id, slot, path): (String, String, String)| {
                let mut cmd = cmd.borrow_mut();
                cmd.texture_swaps.push(crate::world::TextureSwapCommand {
                    entity_id: id,
                    slot,
                    texture_path: path,
                });
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_texture", set_tex_fn)
            .map_err(|e| e.to_string())?;

        // entity.destroy(id)
        let cmd = cmd_queue.clone();
        let destroy_fn = self
            .lua
            .create_function(move |_, id: String| {
                let mut cmd = cmd.borrow_mut();
                cmd.destroys.push(id);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("destroy", destroy_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_scale(id, sx, sy, sz)
        let sw = scene_world.clone();
        let set_scale_fn = self
            .lua
            .create_function(move |_, (id, sx, sy, sz): (String, f32, f32, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
                        transform.scale = glam::Vec3::new(sx, sy, sz);
                        transform.dirty = true;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_scale", set_scale_fn)
            .map_err(|e| e.to_string())?;

        // entity.get_scale(id) -> sx, sy, sz
        let sw = scene_world.clone();
        let get_scale_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(transform) = sw.world.get::<&Transform>(entity) {
                        return Ok((transform.scale.x, transform.scale.y, transform.scale.z));
                    }
                }
                Ok((1.0f32, 1.0f32, 1.0f32))
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("get_scale", get_scale_fn)
            .map_err(|e| e.to_string())?;

        // entity.set_visible(id, visible)
        let cmd = cmd_queue.clone();
        let set_vis_fn = self
            .lua
            .create_function(move |_, (id, visible): (String, bool)| {
                let mut cmd = cmd.borrow_mut();
                cmd.visibility_updates.push((id, visible));
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("set_visible", set_vis_fn)
            .map_err(|e| e.to_string())?;

        // entity.spawn_projectile(owner_id, mesh, material, ox, oy, oz, dx, dy, dz, speed, damage, lifetime, gravity)
        type SpawnProjectileArgs = (
            String,
            String,
            String,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            bool,
        );
        let cmd = cmd_queue.clone();
        let spawn_proj_fn = self
            .lua
            .create_function(
                move |_,
                      (
                    owner_id,
                    mesh,
                    material,
                    ox,
                    oy,
                    oz,
                    dx,
                    dy,
                    dz,
                    speed,
                    damage,
                    lifetime,
                    gravity,
                ): SpawnProjectileArgs| {
                    let mut cmd = cmd.borrow_mut();
                    cmd.projectile_counter += 1;
                    let id = format!("proj_{}", cmd.projectile_counter);
                    cmd.projectile_spawns.push(ProjectileSpawnCommand {
                        id,
                        mesh,
                        material,
                        position: [ox, oy, oz],
                        direction: [dx, dy, dz],
                        speed,
                        damage,
                        lifetime,
                        gravity,
                        owner_id,
                        scale: [1.0, 1.0, 1.0],
                    });
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        entity_table
            .set("spawn_projectile", spawn_proj_fn)
            .map_err(|e| e.to_string())?;

        // entity.spawn_dynamic(mesh, material, x, y, z, vx, vy, vz, radius, mass, restitution, friction, lifetime)
        // Spawns a dynamic rigid body that bounces and persists (no destroy-on-hit).
        type SpawnDynamicArgs = (
            String,
            String,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
        );
        let cmd = cmd_queue.clone();
        let spawn_dyn_fn = self
            .lua
            .create_function(
                move |_,
                      (
                    mesh,
                    material,
                    x,
                    y,
                    z,
                    vx,
                    vy,
                    vz,
                    radius,
                    mass,
                    restitution,
                    friction,
                    lifetime,
                ): SpawnDynamicArgs| {
                    let mut cmd = cmd.borrow_mut();
                    cmd.dynamic_counter += 1;
                    let id = format!("dyn_{}", cmd.dynamic_counter);
                    cmd.dynamic_spawns.push(crate::world::DynamicSpawnCommand {
                        id,
                        mesh,
                        material,
                        position: [x, y, z],
                        velocity: [vx, vy, vz],
                        scale: [1.0, 1.0, 1.0],
                        radius,
                        mass,
                        restitution,
                        friction,
                        lifetime,
                    });
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        entity_table
            .set("spawn_dynamic", spawn_dyn_fn)
            .map_err(|e| e.to_string())?;

        // entity.destroy_by_prefix(prefix) - bulk destroy all entities whose ID starts with prefix
        let sw = scene_world.clone();
        let cmd = cmd_queue.clone();
        let destroy_prefix_fn = self
            .lua
            .create_function(move |_, prefix: String| {
                let sw = sw.borrow();
                let mut cmd = cmd.borrow_mut();
                let ids: Vec<String> = sw
                    .entity_registry
                    .keys()
                    .filter(|id| id.starts_with(&prefix))
                    .cloned()
                    .collect();
                for id in ids {
                    cmd.destroys.push(id);
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("destroy_by_prefix", destroy_prefix_fn)
            .map_err(|e| e.to_string())?;

        // --- Tier 2: Entity Pool API ---

        // entity.pool_create(name, mesh, material, count) — create pool and pre-warm
        let cmd = cmd_queue.clone();
        let pool_mgr = pool_manager.clone();
        let next_pool_id = self.next_pool_id.clone();
        let pool_create_fn = self
            .lua
            .create_function(
                move |_, (name, mesh, material, count): (String, String, String, u32)| {
                    let mut pool_mgr = pool_mgr.borrow_mut();
                    let mut cmd = cmd.borrow_mut();
                    pool_mgr.create_pool(&name, &mesh, &material);
                    // Pre-warm by spawning `count` hidden entities
                    for _ in 0..count {
                        let i = next_pool_id.get();
                        next_pool_id.set(i + 1);
                        let entity_id = format!("_pool_{}_{}", name, i);
                        cmd.spawns.push(crate::world::SpawnCommand {
                            id: entity_id,
                            mesh: mesh.clone(),
                            material: material.clone(),
                            position: [0.0, -1000.0, 0.0], // offscreen
                            scale: [1.0, 1.0, 1.0],
                            collider: None,
                            rigid_body: None,
                            script: None,
                        });
                    }
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        entity_table
            .set("pool_create", pool_create_fn)
            .map_err(|e| e.to_string())?;

        // entity.pool_acquire(name) -> entity_id or nil
        let sw = scene_world.clone();
        let cmd = cmd_queue.clone();
        let pool_mgr = pool_manager.clone();
        let next_pool_id = self.next_pool_id.clone();
        let pool_acquire_fn = self
            .lua
            .create_function(move |_, name: String| {
                let mut pool_mgr = pool_mgr.borrow_mut();
                let mut sw = sw.borrow_mut();
                if let Some(entity_id) = pool_mgr.try_acquire(&name) {
                    // Unhide the entity, mark as active
                    if let Some(&entity) = sw.entity_registry.get(&entity_id) {
                        let _ = sw.world.remove_one::<crate::components::Hidden>(entity);
                        if let Ok(mut pooled) =
                            sw.world.get::<&mut crate::components::Pooled>(entity)
                        {
                            pooled.active = true;
                        }
                        // Reset health if present
                        if let Ok(mut health) = sw.world.get::<&mut Health>(entity) {
                            health.current = health.max;
                            health.dead = false;
                        }
                    }
                    Ok(Some(entity_id))
                } else {
                    // Pool empty — spawn a new entity
                    let mut cmd = cmd.borrow_mut();
                    if let Some((mesh, material)) = pool_mgr.get_pool_assets(&name) {
                        let reserved = next_pool_id.get();
                        next_pool_id.set(reserved + 1);
                        let entity_id = format!("_pool_{}_{}", name, reserved);
                        cmd.spawns.push(crate::world::SpawnCommand {
                            id: entity_id.clone(),
                            mesh,
                            material,
                            position: [0.0, 0.0, 0.0],
                            scale: [1.0, 1.0, 1.0],
                            collider: None,
                            rigid_body: None,
                            script: None,
                        });
                        Ok(Some(entity_id))
                    } else {
                        Ok(None)
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("pool_acquire", pool_acquire_fn)
            .map_err(|e| e.to_string())?;

        // entity.pool_release(id) — hide + return to pool
        let cmd = cmd_queue.clone();
        let pool_release_fn = self
            .lua
            .create_function(move |_, id: String| {
                let mut cmd = cmd.borrow_mut();
                cmd.pool_ops.push(PoolOp::Release(id));
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("pool_release", pool_release_fn)
            .map_err(|e| e.to_string())?;

        // entity.pool_size(name) -> total, available
        let pool_mgr = pool_manager.clone();
        let pool_size_fn = self
            .lua
            .create_function(move |_, name: String| {
                let pool_mgr = pool_mgr.borrow();
                let (total, available) = pool_mgr.pool_size(&name);
                Ok((total as u32, available as u32))
            })
            .map_err(|e| e.to_string())?;
        entity_table
            .set("pool_size", pool_size_fn)
            .map_err(|e| e.to_string())?;

        // --- scene.load(path) — deferred scene loading ---
        let scene_table: LuaTable = globals.get("scene").map_err(|e| e.to_string())?;
        let cmd = cmd_queue.clone();
        let scene_load_fn = self
            .lua
            .create_function(move |_, path: String| {
                let mut cmd = cmd.borrow_mut();
                cmd.pending_scene_load = Some(path);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        scene_table
            .set("load", scene_load_fn)
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}
