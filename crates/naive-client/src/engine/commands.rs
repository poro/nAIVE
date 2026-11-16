//! Editor and MCP command dispatch, persistence, capture and benchmarking.

use super::*;
use naive_core::command::CommandKind;

impl Engine {
    /// Process commands from the command socket.
    /// Editor-enhanced commands (spawn with mesh, save_scene, etc.) are handled here
    /// at Engine level where we have access to GPU resources and caches.
    pub(super) fn process_commands(&mut self) {
        let commands = match &self.command_server {
            Some(s) => s.poll(),
            None => return,
        };

        for pending in commands {
            let cmd = pending.request.cmd.as_str();
            let kind = match pending.request.validate() {
                Ok(command) => command.kind(),
                Err(error) => {
                    let _ = pending
                        .responder
                        .send(crate::command::CommandResponse::error(error));
                    continue;
                }
            };

            // Log command for editor overlay (with detail)
            if self.args.editor_mode {
                let detail = match cmd {
                    "spawn_entity" => {
                        let eid = pending
                            .request
                            .params
                            .get("entity_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let mesh = pending
                            .request
                            .params
                            .get("components")
                            .and_then(|c| c.get("mesh_renderer"))
                            .and_then(|m| m.get("mesh"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if mesh.is_empty() {
                            format!("spawn {}", eid)
                        } else {
                            format!("spawn {} [{}]", eid, mesh)
                        }
                    }
                    "destroy_entity" => {
                        let eid = pending
                            .request
                            .params
                            .get("entity_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        format!("destroy {}", eid)
                    }
                    "modify_entity" => {
                        let eid = pending
                            .request
                            .params
                            .get("entity_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        format!("modify {}", eid)
                    }
                    "set_camera" => "set_camera".to_string(),
                    "save_scene" => {
                        let path = pending
                            .request
                            .params
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("scenes/editor_scene.yaml");
                        format!("save -> {}", path)
                    }
                    "run_lua" => {
                        let code = pending
                            .request
                            .params
                            .get("code")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let preview: String = code.chars().take(40).collect();
                        format!("lua: {}", preview)
                    }
                    other => other.to_string(),
                };
                self.editor_command_log
                    .push((detail, std::time::Instant::now()));
                // Keep only last 10
                if self.editor_command_log.len() > 10 {
                    self.editor_command_log.remove(0);
                }
            }

            let response = match kind {
                // Enhanced spawn_entity: if it has mesh_renderer, handle at Engine level
                CommandKind::SpawnEntity => {
                    let has_mesh = pending
                        .request
                        .params
                        .get("components")
                        .and_then(|c| c.get("mesh_renderer"))
                        .is_some();
                    if has_mesh {
                        self.handle_spawn_with_mesh(&pending.request)
                    } else {
                        {
                            let mut sw_opt = self.scene_world.as_ref().map(|rc| rc.borrow_mut());
                            let mut eb = self.event_bus.borrow_mut();
                            let mut is_opt = self.input_state.as_ref().map(|rc| rc.borrow_mut());
                            crate::command::handle_command_rc(
                                &pending.request,
                                sw_opt.as_deref_mut(),
                                &mut eb,
                                is_opt.as_deref_mut(),
                                &mut self.paused,
                            )
                        }
                    }
                }
                CommandKind::SaveScene => self.handle_save_scene(&pending.request),
                CommandKind::GetSceneYaml => self.handle_get_scene_yaml(),
                CommandKind::SetCamera => self.handle_set_camera(&pending.request),
                CommandKind::EditorStatus => self.handle_editor_status(),
                CommandKind::RunLua => self.handle_run_lua(&pending.request),
                CommandKind::CaptureFrame => self.handle_capture_frame(&pending.request),
                CommandKind::GpuTimings => self.handle_gpu_timings(),
                CommandKind::Benchmark => self.handle_benchmark(&pending.request),
                _ => {
                    let mut sw_opt = self.scene_world.as_ref().map(|rc| rc.borrow_mut());
                    let mut eb = self.event_bus.borrow_mut();
                    let mut is_opt = self.input_state.as_ref().map(|rc| rc.borrow_mut());
                    crate::command::handle_command_rc(
                        &pending.request,
                        sw_opt.as_deref_mut(),
                        &mut eb,
                        is_opt.as_deref_mut(),
                        &mut self.paused,
                    )
                }
            };
            let _ = pending.responder.send(response);
        }
    }

    /// Handle spawn_entity with mesh_renderer component (needs GPU resources).
    pub(super) fn handle_spawn_with_mesh(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let entity_id = match req.params.get("entity_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return CommandResponse::error("Missing 'entity_id' parameter"),
        };

        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return CommandResponse::error("GPU not initialized"),
        };
        let device = &gpu.device;

        let components = match req.params.get("components") {
            Some(c) => c,
            None => return CommandResponse::error("Missing 'components' parameter"),
        };

        let mr = match components.get("mesh_renderer") {
            Some(mr) => mr,
            None => return CommandResponse::error("Missing 'mesh_renderer' in components"),
        };

        let mesh = mr
            .get("mesh")
            .and_then(|v| v.as_str())
            .unwrap_or("procedural:cube");
        let material = mr
            .get("material")
            .and_then(|v| v.as_str())
            .unwrap_or("procedural:default");

        // Parse transform
        let mut position = [0.0f32; 3];
        let mut scale = [1.0f32; 3];
        let mut rotation = [0.0f32; 3];
        if let Some(t) = components.get("transform") {
            if let Some(arr) = t.get("position").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    position = [
                        arr[0].as_f64().unwrap_or(0.0) as f32,
                        arr[1].as_f64().unwrap_or(0.0) as f32,
                        arr[2].as_f64().unwrap_or(0.0) as f32,
                    ];
                }
            }
            if let Some(arr) = t.get("scale").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    scale = [
                        arr[0].as_f64().unwrap_or(1.0) as f32,
                        arr[1].as_f64().unwrap_or(1.0) as f32,
                        arr[2].as_f64().unwrap_or(1.0) as f32,
                    ];
                }
            }
            if let Some(arr) = t.get("rotation").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    rotation = [
                        arr[0].as_f64().unwrap_or(0.0) as f32,
                        arr[1].as_f64().unwrap_or(0.0) as f32,
                        arr[2].as_f64().unwrap_or(0.0) as f32,
                    ];
                }
            }
        }

        // Parse tags
        let tags = req
            .params
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Spawn entity, apply rotation/tags (scoped to release scene_world borrow)
        {
            let scene_world = match &self.scene_world {
                Some(sw) => sw,
                None => return CommandResponse::error("No scene loaded"),
            };
            let mut scene_world = scene_world.borrow_mut();

            if scene_world.entity_registry.contains_key(&entity_id) {
                return CommandResponse::error(format!("Entity '{}' already exists", entity_id));
            }

            // Use spawn_runtime_entity for mesh entities
            let ok = crate::world::spawn_runtime_entity(
                &mut scene_world,
                &entity_id,
                mesh,
                material,
                position,
                scale,
                device,
                &gpu.queue,
                &self.project_root,
                &mut self.mesh_cache,
                &mut self.material_cache,
                self.texture_resources.as_ref(),
            );

            if !ok {
                return CommandResponse::error(format!("Failed to spawn entity '{}'", entity_id));
            }

            // Apply rotation if non-zero
            if rotation != [0.0, 0.0, 0.0] {
                if let Some(&entity) = scene_world.entity_registry.get(&entity_id) {
                    if let Ok(mut transform) = scene_world.world.get::<&mut Transform>(entity) {
                        transform.rotation = crate::world::euler_degrees_to_quat(rotation);
                        transform.dirty = true;
                    }
                }
            }

            // Apply tags
            if !tags.is_empty() {
                if let Some(&entity) = scene_world.entity_registry.get(&entity_id) {
                    if let Ok(mut entity_tags) = scene_world
                        .world
                        .get::<&mut crate::components::Tags>(entity)
                    {
                        entity_tags.0 = tags;
                    }
                }
            }
        } // scene_world borrow released here

        // Add physics body if rigid_body/collider components are specified
        if let Some(collider_json) = components.get("collider") {
            // Parse collider shape from JSON
            let shape_str = collider_json
                .get("shape")
                .and_then(|v| v.as_str())
                .unwrap_or("box");
            let shape = match shape_str {
                "sphere" => crate::physics::PhysicsShape::Sphere {
                    radius: collider_json
                        .get("radius")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.5) as f32,
                },
                "capsule" => crate::physics::PhysicsShape::Capsule {
                    half_height: collider_json
                        .get("half_height")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.5) as f32,
                    radius: collider_json
                        .get("radius")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.3) as f32,
                },
                _ => {
                    let he = if let Some(arr) =
                        collider_json.get("half_extents").and_then(|v| v.as_array())
                    {
                        if arr.len() == 3 {
                            glam::Vec3::new(
                                arr[0].as_f64().unwrap_or(0.5) as f32,
                                arr[1].as_f64().unwrap_or(0.5) as f32,
                                arr[2].as_f64().unwrap_or(0.5) as f32,
                            )
                        } else {
                            glam::Vec3::splat(0.5)
                        }
                    } else {
                        glam::Vec3::new(scale[0] * 0.5, scale[1] * 0.5, scale[2] * 0.5)
                    };
                    crate::physics::PhysicsShape::Box { half_extents: he }
                }
            };

            let is_trigger = collider_json
                .get("is_trigger")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let restitution = collider_json
                .get("restitution")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;
            let friction = collider_json
                .get("friction")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;

            let rb_json = components.get("rigid_body");
            let body_type = rb_json
                .and_then(|rb| rb.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("static");

            let pos = glam::Vec3::from(position);
            let rot = crate::world::euler_degrees_to_quat(rotation);

            if let (Some(pw), Some(sw)) = (&self.physics_world, &self.scene_world) {
                let mut pw = pw.borrow_mut();
                let mut sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&entity_id) {
                    match body_type {
                        "dynamic" => {
                            let mass = rb_json
                                .and_then(|rb| rb.get("mass"))
                                .and_then(|v| v.as_f64())
                                .unwrap_or(1.0) as f32;
                            let ccd = rb_json
                                .and_then(|rb| rb.get("ccd"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let (rb_handle, col_handle) = pw.add_dynamic_body(
                                entity,
                                pos,
                                rot,
                                shape.clone(),
                                mass,
                                restitution,
                                friction,
                                ccd,
                            );
                            let _ = sw.world.insert(
                                entity,
                                (
                                    crate::physics::RigidBody {
                                        handle: rb_handle,
                                        body_type: crate::physics::PhysicsBodyType::Dynamic,
                                    },
                                    crate::physics::Collider {
                                        handle: col_handle,
                                        shape,
                                        is_trigger,
                                    },
                                ),
                            );
                        }
                        _ => {
                            let (rb_handle, col_handle) = pw.add_static_body(
                                entity,
                                pos,
                                rot,
                                shape.clone(),
                                is_trigger,
                                restitution,
                                friction,
                            );
                            let _ = sw.world.insert(
                                entity,
                                (
                                    crate::physics::RigidBody {
                                        handle: rb_handle,
                                        body_type: crate::physics::PhysicsBodyType::Static,
                                    },
                                    crate::physics::Collider {
                                        handle: col_handle,
                                        shape,
                                        is_trigger,
                                    },
                                ),
                            );
                        }
                    }
                }
            }
        }

        CommandResponse::ok(json!({"entity_id": entity_id}))
    }

    /// Handle save_scene: serialize current ECS state to YAML file.
    pub(super) fn handle_save_scene(
        &self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let path = req
            .params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("scenes/editor_scene.yaml");
        let full_path = match crate::project_path::resolve(&self.project_root, path) {
            Ok(path)
                if path
                    .extension()
                    .is_some_and(|ext| ext == "yaml" || ext == "yml") =>
            {
                path
            }
            Ok(_) => return CommandResponse::error("Scene path must end in .yaml or .yml"),
            Err(e) => return CommandResponse::error(e),
        };

        let yaml = match self.serialize_scene_to_yaml() {
            Some(y) => y,
            None => return CommandResponse::error("No scene to save"),
        };

        // Ensure parent directory exists
        if let Some(parent) = full_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match std::fs::write(&full_path, &yaml) {
            Ok(()) => {
                tracing::info!("Scene saved to {:?}", full_path);
                CommandResponse::ok(
                    json!({"path": full_path.display().to_string(), "bytes": yaml.len()}),
                )
            }
            Err(e) => CommandResponse::error(format!("Failed to write scene: {}", e)),
        }
    }

    /// Handle get_scene_yaml: return current scene as YAML string.
    pub(super) fn handle_get_scene_yaml(&self) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        match self.serialize_scene_to_yaml() {
            Some(yaml) => CommandResponse::ok(json!({"yaml": yaml})),
            None => CommandResponse::error("No scene loaded"),
        }
    }

    /// Handle set_camera: update editor camera position/rotation.
    pub(super) fn handle_set_camera(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let editor_cam = match &mut self.editor_camera {
            Some(c) => c,
            None => return CommandResponse::error("Not in editor mode"),
        };

        if let Some(arr) = req.params.get("position").and_then(|v| v.as_array()) {
            if arr.len() == 3 {
                editor_cam.position = glam::Vec3::new(
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[1].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                );
            }
        }
        if let Some(yaw) = req.params.get("yaw").and_then(|v| v.as_f64()) {
            editor_cam.yaw = (yaw as f32).to_radians();
        }
        if let Some(pitch) = req.params.get("pitch").and_then(|v| v.as_f64()) {
            editor_cam.pitch = (pitch as f32).to_radians();
        }
        if let Some(arr) = req.params.get("look_at").and_then(|v| v.as_array()) {
            if arr.len() == 3 {
                let target = glam::Vec3::new(
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[1].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                );
                let dir = (target - editor_cam.position).normalize_or_zero();
                editor_cam.yaw = (-dir.x).atan2(-dir.z);
                editor_cam.pitch = dir.y.asin();
            }
        }

        CommandResponse::ok(json!({
            "position": [editor_cam.position.x, editor_cam.position.y, editor_cam.position.z],
            "yaw": editor_cam.yaw.to_degrees(),
            "pitch": editor_cam.pitch.to_degrees(),
        }))
    }

    /// Handle editor_status: return editor mode info.
    pub(super) fn handle_editor_status(&self) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let entity_count = self
            .scene_world
            .as_ref()
            .map(|sw| sw.borrow().entity_registry.len())
            .unwrap_or(0);

        let camera_info = self.editor_camera.as_ref().map(|c| {
            json!({
                "position": [c.position.x, c.position.y, c.position.z],
                "yaw": c.yaw.to_degrees(),
                "pitch": c.pitch.to_degrees(),
                "speed": c.speed,
            })
        });

        let scene_path = self
            .editor_scene_path
            .as_ref()
            .map(|p| p.display().to_string());

        CommandResponse::ok(json!({
            "editor_mode": self.args.editor_mode,
            "entity_count": entity_count,
            "scene_path": scene_path,
            "camera": camera_info,
        }))
    }

    /// Handle run_lua: execute arbitrary Lua code with access to all registered APIs.
    pub(super) fn handle_run_lua(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let code = match req.params.get("code").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c,
            _ => return CommandResponse::error("Missing or empty 'code' parameter"),
        };

        let script_runtime = match &self.script_runtime {
            Some(sr) => sr,
            None => return CommandResponse::error("Script runtime not initialized"),
        };

        let lua = &script_runtime.lua;

        // Create a sandboxed environment with __index fallback to globals
        // This gives access to all registered API tables (entity, physics, particles, etc.)
        let result: Result<mlua::Value, mlua::Error> = (|| {
            let env = lua.create_table()?;
            let meta = lua.create_table()?;
            meta.set("__index", lua.globals())?;
            env.set_metatable(Some(meta));

            // Capture print output
            let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let output_clone = output.clone();
            let print_fn = lua.create_function(move |_, args: mlua::MultiValue| {
                let parts: Vec<String> = args.iter().map(|v| format!("{:?}", v)).collect();
                let line = parts.join("\t");
                if let Ok(mut out) = output_clone.lock() {
                    out.push(line);
                }
                Ok(())
            })?;
            env.set("print", print_fn)?;

            let chunk = lua.load(code).set_name("run_lua").set_environment(env);
            let value = script_runtime.run_limited(|| chunk.eval::<mlua::Value>())?;

            // Format return value
            let result_str = match &value {
                mlua::Value::Nil => "nil".to_string(),
                mlua::Value::Boolean(b) => b.to_string(),
                mlua::Value::Integer(i) => i.to_string(),
                mlua::Value::Number(n) => n.to_string(),
                mlua::Value::String(s) => s.to_string_lossy(),
                other => format!("{:?}", other),
            };

            let print_output = output.lock().map(|o| o.clone()).unwrap_or_default();

            Ok(mlua::Value::String(
                lua.create_string(
                    json!({
                        "result": result_str,
                        "print_output": print_output,
                    })
                    .to_string(),
                )?,
            ))
        })();

        match result {
            Ok(val) => {
                // Parse back the JSON we stuffed in
                if let mlua::Value::String(s) = val {
                    let s = s.to_string_lossy();
                    let parsed: serde_json::Value =
                        serde_json::from_str(&s).unwrap_or(json!({"result": s}));
                    CommandResponse::ok(parsed)
                } else {
                    CommandResponse::ok(json!({"result": "ok"}))
                }
            }
            Err(e) => CommandResponse::error(format!("Lua error: {}", e)),
        }
    }

    /// Serialize the current ECS scene state to YAML.
    pub(super) fn serialize_scene_to_yaml(&self) -> Option<String> {
        use crate::components::*;
        use crate::scene::*;

        let scene_world = self.scene_world.as_ref()?;
        let scene_world = scene_world.borrow();

        let scene_name = scene_world
            .current_scene
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Editor Scene".to_string());

        let settings = scene_world
            .current_scene
            .as_ref()
            .map(|s| s.settings.clone())
            .unwrap_or_default();

        let mut entities = Vec::new();

        for (id, &entity) in &scene_world.entity_registry {
            let mut components = ComponentMap::default();

            // Transform
            if let Ok(t) = scene_world.world.get::<&Transform>(entity) {
                let (yaw, pitch, roll) = t.rotation.to_euler(glam::EulerRot::YXZ);
                components.transform = Some(TransformDef {
                    position: t.position.to_array(),
                    rotation: [pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees()],
                    scale: t.scale.to_array(),
                });
            }

            // MeshRenderer
            if let Ok(mr) = scene_world.world.get::<&MeshRenderer>(entity) {
                let mesh_name = self
                    .mesh_cache
                    .name_for_handle(mr.mesh_handle)
                    .unwrap_or_else(|| format!("mesh:{}", mr.mesh_handle.0));
                let material_name = self
                    .material_cache
                    .name_for_handle(mr.material_handle)
                    .unwrap_or_else(|| format!("material:{}", mr.material_handle.0));
                components.mesh_renderer = Some(MeshRendererDef {
                    mesh: mesh_name,
                    material: material_name,
                    cast_shadows: true,
                    receive_shadows: true,
                });
            }

            // Camera
            if let Ok(c) = scene_world.world.get::<&Camera>(entity) {
                let role_str = match &c.role {
                    CameraRole::Main => "main".to_string(),
                    CameraRole::Other(s) => s.clone(),
                };
                components.camera = Some(CameraDef {
                    fov: c.fov_degrees,
                    near: c.near,
                    far: c.far,
                    role: role_str,
                    mode: "first_person".to_string(),
                    distance: 4.0,
                    height_offset: 1.5,
                    pitch_limits: None,
                });
            }

            // PointLight
            if let Ok(pl) = scene_world.world.get::<&PointLight>(entity) {
                components.point_light = Some(PointLightDef {
                    color: pl.color.to_array(),
                    intensity: pl.intensity,
                    range: pl.range,
                });
            }

            // DirectionalLight
            if let Ok(dl) = scene_world.world.get::<&DirectionalLight>(entity) {
                components.directional_light = Some(DirectionalLightDef {
                    direction: dl.direction.to_array(),
                    color: dl.color.to_array(),
                    intensity: dl.intensity,
                    shadow_extent: dl.shadow_extent,
                });
            }

            // GpuSwarm
            if let Ok(swarm) = scene_world
                .world
                .get::<&crate::components::GpuSwarm>(entity)
            {
                components.gpu_swarm = Some(swarm.def.clone());
            }

            // Tags
            let tags = scene_world
                .world
                .get::<&Tags>(entity)
                .map(|t| t.0.clone())
                .unwrap_or_default();

            entities.push(EntityDef {
                id: id.clone(),
                tags,
                extends: None,
                components,
            });
        }

        let scene_file = SceneFile {
            name: scene_name,
            settings,
            entities,
        };

        serde_yaml::to_string(&scene_file).ok()
    }

    pub(super) fn gpu_timings_json(&self) -> serde_json::Value {
        use serde_json::json;
        match &self.gpu_timer {
            Some(timer) => {
                let t = timer.timings();
                let passes: Vec<_> = t
                    .passes
                    .iter()
                    .map(|(name, ms)| json!({ "pass": name, "ms": ms }))
                    .collect();
                json!({
                    "available": true,
                    "frame_ms": t.frame_ms,
                    "passes": passes,
                    "frames": t.frames,
                })
            }
            None => json!({ "available": false }),
        }
    }

    /// Handle gpu_timings: see `gpu_timings_json`.
    pub(super) fn handle_gpu_timings(&self) -> crate::command::CommandResponse {
        crate::command::CommandResponse::ok(self.gpu_timings_json())
    }

    /// Handle benchmark: run `frames` whole frames (default 240) back to
    /// back, each simulated with a fixed 1/60 s step, swarms stepped and
    /// every render pass drawn into an offscreen target, without presenting;
    /// then wait for the GPU. That measures what the engine sustains, free of
    /// vsync and of the window server, with the GPU kept busy at full clock.
    /// The simulation really advances.
    pub(super) fn handle_benchmark(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let frames = req
            .params
            .get("frames")
            .and_then(|v| v.as_u64())
            .unwrap_or(240)
            .clamp(1, 10_000) as u32;
        if self.scene_world.is_none() {
            return CommandResponse::error("No scene loaded");
        }
        let Some(gpu) = &self.gpu else {
            return CommandResponse::error("The renderer is not running");
        };
        let (width, height) = (gpu.config.width, gpu.config.height);
        let target = crate::capture::create_capture_target(gpu);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(timer) = self.gpu_timer.as_mut() {
            timer.reset();
        }

        let saved_dt = self.delta_time;
        self.delta_time = 1.0 / 60.0;
        let start = std::time::Instant::now();
        for _ in 0..frames {
            if !self.paused {
                self.tick_simulation();
            }
            self.prepare_render();
            if let Some(gpu) = &self.gpu {
                self.render_scene_to(gpu, &view, self.gpu_timer.as_ref());
                if let Some(timer) = self.gpu_timer.as_mut() {
                    timer.finish_frame(&gpu.device, &gpu.queue);
                }
            }
        }
        if let Some(gpu) = &self.gpu {
            let _ = gpu.device.poll(wgpu::Maintain::Wait);
            // Pick up the timestamps that have just come back.
            if let Some(timer) = self.gpu_timer.as_mut() {
                timer.finish_frame(&gpu.device, &gpu.queue);
            }
        }
        let seconds = start.elapsed().as_secs_f64();
        self.delta_time = saved_dt;
        self.last_frame_time = Some(std::time::Instant::now());

        CommandResponse::ok(json!({
            "frames": frames,
            "seconds": seconds,
            "fps": f64::from(frames) / seconds,
            "ms_per_frame": seconds * 1000.0 / f64::from(frames),
            "width": width,
            "height": height,
            "gpu": self.gpu_timings_json(),
        }))
    }

    /// Handle capture_frame: render the scene to a PNG file inside the project.
    pub(super) fn handle_capture_frame(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        use serde_json::json;

        let relative = req
            .params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("captures/frame.png");
        let path = match crate::capture::capture_path(&self.project_root, relative) {
            Ok(path) => path,
            Err(e) => return CommandResponse::error(e),
        };
        let gpu = match &self.gpu {
            Some(gpu) => gpu,
            None => return CommandResponse::error("The renderer is not running"),
        };
        if self.scene_world.is_none() {
            return CommandResponse::error("No scene loaded");
        }

        let target = crate::capture::create_capture_target(gpu);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        self.render_scene_to(gpu, &view, None);
        match crate::capture::save_texture_png(gpu, &target, &path) {
            Ok((width, height)) => {
                tracing::info!("Frame captured to {:?}", path);
                CommandResponse::ok(json!({
                    "path": path.display().to_string(),
                    "width": width,
                    "height": height,
                }))
            }
            Err(e) => CommandResponse::error(format!("Capture failed: {}", e)),
        }
    }
}
