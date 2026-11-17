//! Lua mesh API registration.

use super::{ScriptRuntime, SharedEntityCommandQueue};
use mlua::prelude::*;

impl ScriptRuntime {
    /// Register mesh API (mesh.create for Lua-driven mesh generation).
    pub fn register_mesh_api(&self, cmd_queue: SharedEntityCommandQueue) -> Result<(), String> {
        let globals = self.lua.globals();
        let mesh_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // mesh.create(name, vertices_table, indices_table, uvs_table)
        // vertices: {{x,y,z}, {x,y,z}, ...}
        // indices: {0, 1, 2, ...}
        // uvs: {{u,v}, {u,v}, ...} (must match vertex count)
        let cmd = cmd_queue.clone();
        let create_fn = self
            .lua
            .create_function(
                move |_,
                      (name, verts_tbl, indices_tbl, uvs_tbl): (
                    String,
                    LuaTable,
                    LuaTable,
                    LuaTable,
                )| {
                    let mut positions = Vec::new();
                    let mut normals = Vec::new();
                    let mut uvs = Vec::new();
                    let mut indices = Vec::new();

                    // Parse vertices
                    for v in verts_tbl.sequence_values::<LuaTable>() {
                        let v = v.map_err(|e| {
                            mlua::Error::runtime(format!("mesh.create: bad vertex: {}", e))
                        })?;
                        positions.push([
                            v.get::<f32>(1).unwrap_or(0.0),
                            v.get::<f32>(2).unwrap_or(0.0),
                            v.get::<f32>(3).unwrap_or(0.0),
                        ]);
                        // Default normals pointing up
                        normals.push([0.0, 1.0, 0.0]);
                    }

                    // Parse indices
                    for idx in indices_tbl.sequence_values::<u32>() {
                        indices.push(idx.map_err(|e| {
                            mlua::Error::runtime(format!("mesh.create: bad index: {}", e))
                        })?);
                    }

                    // Parse UVs
                    for uv in uvs_tbl.sequence_values::<LuaTable>() {
                        let uv = uv.map_err(|e| {
                            mlua::Error::runtime(format!("mesh.create: bad uv: {}", e))
                        })?;
                        uvs.push([
                            uv.get::<f32>(1).unwrap_or(0.0),
                            uv.get::<f32>(2).unwrap_or(0.0),
                        ]);
                    }

                    // Pad UVs if fewer than positions
                    while uvs.len() < positions.len() {
                        uvs.push([0.0, 0.0]);
                    }

                    // Compute flat normals from triangles
                    let mut computed_normals = vec![glam::Vec3::ZERO; positions.len()];
                    for tri in indices.chunks(3) {
                        if tri.len() == 3 {
                            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                            if i0 < positions.len() && i1 < positions.len() && i2 < positions.len()
                            {
                                let v0 = glam::Vec3::from(positions[i0]);
                                let v1 = glam::Vec3::from(positions[i1]);
                                let v2 = glam::Vec3::from(positions[i2]);
                                let face_normal = (v1 - v0).cross(v2 - v0);
                                computed_normals[i0] += face_normal;
                                computed_normals[i1] += face_normal;
                                computed_normals[i2] += face_normal;
                            }
                        }
                    }
                    normals = computed_normals
                        .iter()
                        .map(|n| {
                            let n = n.normalize_or_zero();
                            if n == glam::Vec3::ZERO {
                                [0.0, 1.0, 0.0]
                            } else {
                                n.to_array()
                            }
                        })
                        .collect();

                    let command = crate::world::MeshCreateCommand {
                        name,
                        positions,
                        normals,
                        uvs,
                        indices,
                    };
                    // Reject bad geometry here, where the script can see why.
                    command.cpu_mesh().map_err(|error| {
                        mlua::Error::runtime(format!("mesh.create '{}': {error}", command.name))
                    })?;
                    cmd.borrow_mut().mesh_creates.push(command);
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        mesh_table
            .set("create", create_fn)
            .map_err(|e| e.to_string())?;

        globals.set("mesh", mesh_table).map_err(|e| e.to_string())?;
        Ok(())
    }
}
