//! Editor and MCP command dispatch, persistence, capture and benchmarking.

use super::*;
use naive_core::command::CommandKind;

/// Replace a scene only after the complete new document reaches disk.
fn write_scene_file(path: &Path, yaml: &str) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or("Scene path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let (temporary, mut file) = (0..32)
        .find_map(|_| {
            let temporary = parent.join(format!(
                ".naive-scene-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error.to_string())),
            }
        })
        .ok_or("Could not reserve a temporary scene file")??;
    let result = (|| {
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())
                .map_err(|error| error.to_string())?;
        }
        file.write_all(yaml.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

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
                CommandKind::SpawnEntity => self.handle_spawn_entity(&pending.request),
                CommandKind::DestroyEntity => self.handle_destroy_entity(&pending.request),
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

    /// Spawn through the scene constructor so all supported components agree.
    pub(super) fn handle_spawn_entity(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        let definition = match crate::command::spawn_definition(req) {
            Ok(definition) => definition,
            Err(error) => return CommandResponse::error(error),
        };
        let (Some(gpu), Some(world)) = (&self.gpu, self.scene_world.clone()) else {
            return CommandResponse::error("The renderer and scene must be initialized");
        };
        {
            let mut world = world.borrow_mut();
            if world.entity_registry.contains_key(&definition.id) {
                return CommandResponse::error(format!("Entity already exists: {}", definition.id));
            }
            let mut physics = self
                .physics_world
                .as_ref()
                .map(|physics| physics.borrow_mut());
            crate::world::spawn_entity(
                &mut world,
                &definition,
                &gpu.device,
                &gpu.queue,
                &self.project_root,
                &mut self.mesh_cache,
                &mut self.material_cache,
                &mut self.splat_cache,
                physics.as_deref_mut(),
                self.texture_resources.as_ref(),
                Some(&mut self.texture_cache),
            );
            if !world.entity_registry.contains_key(&definition.id) {
                return CommandResponse::error(format!(
                    "Could not load assets for {}",
                    definition.id
                ));
            }
        }
        self.register_skeletons();
        let missing_animator = definition.components.animator.is_some() && {
            let world = world.borrow();
            let entity = world.entity_registry[&definition.id];
            let missing = world
                .world
                .get::<&crate::components::Animator>(entity)
                .is_err();
            missing
        };
        if missing_animator {
            crate::sim::destroy_entity(
                &world,
                self.physics_world.as_ref(),
                self.script_runtime.as_mut(),
                &definition.id,
            );
            return CommandResponse::error(
                "The requested animator requires a mesh with skeletal animation",
            );
        }
        if let Some(runtime) = self.script_runtime.as_mut() {
            if let Err(error) =
                crate::sim::initialize_pending_scripts(&world, runtime, &self.project_root)
            {
                crate::sim::destroy_entity(
                    &world,
                    self.physics_world.as_ref(),
                    Some(runtime),
                    &definition.id,
                );
                return CommandResponse::error(error);
            }
        }
        CommandResponse::ok(serde_json::json!({"entity_id": definition.id}))
    }

    pub(super) fn handle_destroy_entity(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        let Some(id) = req.params.get("entity_id").and_then(|id| id.as_str()) else {
            return CommandResponse::error("Missing entity_id");
        };
        let Some(world) = self.scene_world.as_ref() else {
            return CommandResponse::error("No scene loaded");
        };
        if crate::sim::destroy_entity(
            world,
            self.physics_world.as_ref(),
            self.script_runtime.as_mut(),
            id,
        ) {
            CommandResponse::ok_empty()
        } else {
            CommandResponse::error(format!("Entity not found: {id}"))
        }
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
            Ok(y) => y,
            Err(error) => return CommandResponse::error(error),
        };

        match write_scene_file(&full_path, &yaml) {
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
            Ok(yaml) => CommandResponse::ok(json!({"yaml": yaml})),
            Err(error) => CommandResponse::error(error),
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
            env.set_metatable(Some(meta))?;

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

    /// Preserve authored component definitions and overlay live ECS state.
    pub(super) fn serialize_scene_to_yaml(&self) -> Result<String, String> {
        let world = self.scene_world.as_ref().ok_or("No scene loaded")?.borrow();
        let physics = self.physics_world.as_ref().map(|physics| physics.borrow());
        let scene = crate::scene_snapshot::snapshot_scene(
            &world,
            &self.mesh_cache,
            &self.material_cache,
            physics.as_deref(),
        )?;
        serde_norway::to_string(&scene).map_err(|error| error.to_string())
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
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
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

#[cfg(test)]
mod tests {
    use super::write_scene_file;

    #[test]
    fn atomic_scene_save_replaces_complete_document_and_cleans_failed_writes() {
        let root = std::env::temp_dir().join(format!(
            "naive-atomic-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("scene.yaml");
        std::fs::write(&path, "name: previous\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        write_scene_file(&path, "name: replacement\nentities: []\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "name: replacement\nentities: []\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let existing_directory = root.join("cannot-replace.yaml");
        std::fs::create_dir(&existing_directory).unwrap();
        let keep = existing_directory.join("keep");
        std::fs::write(&keep, "preserve me").unwrap();
        assert!(write_scene_file(&existing_directory, "name: invalid destination").is_err());
        assert_eq!(std::fs::read_to_string(keep).unwrap(), "preserve me");
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            2,
            "temporary scene file leaked"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
