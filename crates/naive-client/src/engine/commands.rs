//! Editor and MCP command dispatch, persistence, capture and benchmarking.

use super::*;
use naive_core::command::CommandKind;
use serde_json::json;

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

/// Undo steps kept; older ones are dropped.
const UNDO_LIMIT: usize = 32;
/// Scenes larger than this are not snapshotted for undo (too slow and large).
const UNDO_MAX_ENTITIES: usize = 5000;

/// The scene as it was before an undoable command, or why it could not be
/// captured.
pub(crate) struct UndoEntry {
    label: String,
    scene: Result<crate::scene::SceneFile, String>,
}

/// Keep an undo snapshot pending until the command's outcome is known. Errors
/// after entering user code can leave scene changes that still need undo.
struct PendingUndo {
    entry: Option<UndoEntry>,
    may_have_changed: bool,
}

impl PendingUndo {
    fn new(entry: Option<UndoEntry>) -> Self {
        Self {
            entry,
            may_have_changed: false,
        }
    }

    fn commit(self, succeeded: bool, stack: &mut Vec<UndoEntry>) {
        if succeeded || self.may_have_changed {
            if let Some(entry) = self.entry {
                stack.push(entry);
                if stack.len() > UNDO_LIMIT {
                    stack.remove(0);
                }
            }
        }
    }
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

            if kind == CommandKind::RunTests {
                self.test_jobs.start(self.project_root.clone(), pending);
                continue;
            }

            let undoable = matches!(
                kind,
                CommandKind::SpawnEntity
                    | CommandKind::DestroyEntity
                    | CommandKind::ModifyEntity
                    | CommandKind::RunLua
            );
            let entry = if undoable {
                self.snapshot_undo(kind.name())
            } else {
                None
            };
            let mut undo = PendingUndo::new(entry);
            let response = match kind {
                CommandKind::ScreenshotDiff => self.handle_screenshot_diff(&pending.request),
                CommandKind::Undo => self.handle_undo(),
                CommandKind::SpawnEntity => {
                    self.handle_spawn_entity(&pending.request, &mut undo.may_have_changed)
                }
                CommandKind::DestroyEntity => self.handle_destroy_entity(&pending.request),
                CommandKind::SaveScene => self.handle_save_scene(&pending.request),
                CommandKind::GetSceneYaml => self.handle_get_scene_yaml(),
                CommandKind::SetCamera => self.handle_set_camera(&pending.request),
                CommandKind::EditorStatus => self.handle_editor_status(),
                CommandKind::RunLua => {
                    self.handle_run_lua(&pending.request, &mut undo.may_have_changed)
                }
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
            undo.commit(response.status == "ok", &mut self.undo_stack);
            let _ = pending.responder.send(response);
        }
    }

    /// Snapshot the scene before an undoable command. A scene that cannot be
    /// snapshotted leaves a step that explains why it cannot be undone, so
    /// that undo never silently skips over a change.
    fn snapshot_undo(&self, label: &str) -> Option<UndoEntry> {
        let scene = match &self.scene_world {
            None => return None,
            Some(world) if world.borrow().entity_registry.len() > UNDO_MAX_ENTITIES => {
                Err(format!(
                    "the scene had more than {UNDO_MAX_ENTITIES} entities, too many to snapshot"
                ))
            }
            Some(_) => self.snapshot_scene_file(),
        };
        Some(UndoEntry {
            label: label.to_string(),
            scene,
        })
    }

    pub(super) fn handle_undo(&mut self) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        let Some(entry) = self.undo_stack.last() else {
            return CommandResponse::error("Nothing to undo");
        };
        match &entry.scene {
            Err(reason) => {
                CommandResponse::error(format!("Cannot undo {}: {}", entry.label, reason))
            }
            Ok(scene) => {
                // replace_scene tears down the current scene before its GPU
                // check, so reject an unavailable rebuild before calling it.
                if self.gpu.is_none() {
                    return CommandResponse::error("No GPU to rebuild the scene with");
                }
                let scene = scene.clone();
                let label = entry.label.clone();
                if self.replace_scene(scene) {
                    self.undo_stack.pop();
                    CommandResponse::ok(json!({
                        "undone": label,
                        "remaining": self.undo_stack.len(),
                    }))
                } else {
                    CommandResponse::error("No GPU to rebuild the scene with")
                }
            }
        }
    }

    pub(super) fn handle_screenshot_diff(
        &mut self,
        req: &crate::command::CommandRequest,
    ) -> crate::command::CommandResponse {
        use crate::command::CommandResponse;
        let text = |key: &str| req.params.get(key).and_then(|v| v.as_str());
        let (Some(a), Some(b)) = (text("a"), text("b")) else {
            return CommandResponse::error("screenshot_diff needs a and b");
        };
        let output = text("output").unwrap_or("captures/diff.png");
        let threshold = req
            .params
            .get("threshold")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .min(255) as u8;
        let resolve = |path: &str| crate::project_path::resolve(&self.project_root, path);
        let (a, b) = match (resolve(a), resolve(b)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(error), _) | (_, Err(error)) => return CommandResponse::error(error),
        };
        let output = match crate::capture::capture_path(&self.project_root, output) {
            Ok(path) => path,
            Err(error) => return CommandResponse::error(error),
        };
        match crate::capture::diff_frames(&a, &b, &output, threshold) {
            Ok(diff) => {
                let total = u64::from(diff.width) * u64::from(diff.height);
                CommandResponse::ok(json!({
                    "width": diff.width,
                    "height": diff.height,
                    "changed_pixels": diff.changed_pixels,
                    "changed_percent": 100.0 * diff.changed_pixels as f64 / total.max(1) as f64,
                    "max_difference": diff.max_difference,
                    "mean_difference": diff.mean_difference,
                    "diff_image": output.display().to_string(),
                }))
            }
            Err(error) => CommandResponse::error(error),
        }
    }

    /// Spawn through the scene constructor so all supported components agree.
    pub(super) fn handle_spawn_entity(
        &mut self,
        req: &crate::command::CommandRequest,
        may_have_changed: &mut bool,
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
            // A failing init hook can change other entities before the newly
            // spawned entity is removed below.
            *may_have_changed = true;
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
            "tests_running": self.test_jobs.is_running(),
            "scene_path": scene_path,
            "camera": camera_info,
        }))
    }

    /// Handle run_lua: execute arbitrary Lua code with access to all registered APIs.
    pub(super) fn handle_run_lua(
        &mut self,
        req: &crate::command::CommandRequest,
        may_have_changed: &mut bool,
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

            // Compile first: code that does not parse cannot have changed the
            // scene, so it must not cost an undo step. Like `eval`, try it as
            // an expression first so `1 + 2` returns 3.
            let compile = |source: &str| {
                lua.load(source)
                    .set_name("run_lua")
                    .set_environment(env.clone())
                    .into_function()
            };
            let chunk = match compile(&format!("return {code}")) {
                Ok(function) => function,
                Err(_) => compile(code)?,
            };
            *may_have_changed = true;
            // Entities this code spawns are the editor's (authored), not a
            // script's, so snapshots keep them. Record them even on error.
            let before = self.entity_commands.borrow().spawn_counts();
            let value = script_runtime.run_limited(|| chunk.call::<mlua::Value>(()));
            self.entity_commands
                .borrow_mut()
                .record_editor_spawns(before);
            let value = value?;

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
        let scene = self.snapshot_scene_file()?;
        serde_norway::to_string(&scene).map_err(|error| error.to_string())
    }

    /// The running scene as a scene document.
    pub(super) fn snapshot_scene_file(&self) -> Result<crate::scene::SceneFile, String> {
        let world = self.scene_world.as_ref().ok_or("No scene loaded")?.borrow();
        let physics = self.physics_world.as_ref().map(|physics| physics.borrow());
        crate::scene_snapshot::snapshot_scene(
            &world,
            &self.mesh_cache,
            &self.material_cache,
            physics.as_deref(),
        )
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
            self.tick_simulation();
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
        let include_ui = req
            .params
            .get("include_ui")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if include_ui {
            if let (Some(ui), Some(font)) = (&self.ui_renderer, &self.bitmap_font) {
                let mut encoder =
                    gpu.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Capture UI Encoder"),
                        });
                ui.borrow()
                    .render_last(&gpu.queue, &mut encoder, &view, &font.borrow());
                gpu.queue.submit(std::iter::once(encoder.finish()));
            }
        }
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
    use super::*;

    fn undo_entry(label: impl Into<String>) -> UndoEntry {
        UndoEntry {
            label: label.into(),
            scene: crate::scene::parse_scene("name: undo test\nentities: []"),
        }
    }

    #[test]
    fn failed_no_op_commands_do_not_trim_a_full_undo_history() {
        let mut stack: Vec<_> = (0..UNDO_LIMIT).map(|i| undo_entry(i.to_string())).collect();
        PendingUndo::new(Some(undo_entry("failed validation"))).commit(false, &mut stack);
        assert_eq!(stack.len(), UNDO_LIMIT);
        assert_eq!(stack.first().unwrap().label, "0");
        assert_eq!(stack.last().unwrap().label, (UNDO_LIMIT - 1).to_string());

        // A command without a loaded scene has no snapshot to insert or remove.
        PendingUndo::new(None).commit(false, &mut stack);
        assert_eq!(stack.len(), UNDO_LIMIT);
        assert_eq!(stack.first().unwrap().label, "0");

        PendingUndo::new(Some(undo_entry("successful change"))).commit(true, &mut stack);
        assert_eq!(stack.len(), UNDO_LIMIT);
        assert_eq!(stack.first().unwrap().label, "1");
        assert_eq!(stack.last().unwrap().label, "successful change");
    }

    /// Entities `run_lua` spawns are the editor's, so snapshots keep them;
    /// a script's spawns queued earlier stay script output. A failure after
    /// the spawn still records it, since the spawn goes ahead.
    #[test]
    fn run_lua_spawns_are_recorded_as_editor_authored() {
        let mut engine = Engine::new(CliArgs {
            command: None,
            scene: None,
            pipeline: None,
            project: ".".to_string(),
            socket: "/tmp/unused-naive-editor-spawn-test.sock".to_string(),
            hud: false,
            editor_mode: false,
        });
        let world = Rc::new(RefCell::new(SceneWorld::new()));
        let physics = Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::ZERO)));
        let runtime = ScriptRuntime::new();
        runtime.register_entity_api(world.clone(), physics).unwrap();
        runtime
            .register_entity_command_api(
                world.clone(),
                engine.entity_commands.clone(),
                engine.pool_manager.clone(),
            )
            .unwrap();
        engine.scene_world = Some(world);
        engine.script_runtime = Some(runtime);
        let spawn = |id: &str| {
            format!("entity.spawn_ex({{id = '{id}', mesh = 'procedural:cube', material = 'procedural:default'}})")
        };
        engine
            .script_runtime
            .as_ref()
            .unwrap()
            .lua
            .load(spawn("from_script"))
            .exec()
            .unwrap();
        let request = serde_json::from_value(json!({
            "cmd": "run_lua",
            "code": format!("{}; error('after spawn')", spawn("from_editor")),
        }))
        .unwrap();
        let mut may_have_changed = false;
        let response = engine.handle_run_lua(&request, &mut may_have_changed);
        assert_eq!(response.status, "error");
        let queue = engine.entity_commands.borrow();
        assert_eq!(queue.spawns.len(), 2);
        assert_eq!(
            queue.editor_spawns,
            std::collections::HashSet::from(["from_editor".to_string()])
        );
    }

    #[test]
    fn failed_lua_keeps_its_scene_snapshot_and_failed_undo_keeps_history() {
        let mut engine = Engine::new(CliArgs {
            command: None,
            scene: None,
            pipeline: None,
            project: ".".to_string(),
            socket: "/tmp/unused-naive-undo-test.sock".to_string(),
            hud: false,
            editor_mode: false,
        });
        let scene = crate::scene::parse_scene(
            "name: undo test\nentities:\n  - id: target\n    components:\n      transform:\n        position: [2, 0, 0]\n",
        )
        .unwrap();
        let world = Rc::new(RefCell::new(SceneWorld::new()));
        let physics = Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::ZERO)));
        crate::world::spawn_all_entities_headless(
            &mut world.borrow_mut(),
            &scene,
            &mut physics.borrow_mut(),
        );
        let runtime = ScriptRuntime::new();
        runtime
            .register_entity_api(world.clone(), physics.clone())
            .unwrap();
        engine.scene_world = Some(world.clone());
        engine.physics_world = Some(physics);
        engine.script_runtime = Some(runtime);
        engine.undo_stack.push(undo_entry("previous change"));
        let request = serde_json::from_value(json!({
            "cmd": "run_lua",
            "code": "entity.set_position('target', 9, 0, 0); error('after mutation')",
        }))
        .unwrap();
        let mut undo = PendingUndo::new(engine.snapshot_undo("run_lua"));
        let response = engine.handle_run_lua(&request, &mut undo.may_have_changed);
        assert_eq!(response.status, "error");
        assert!(response.message.unwrap().contains("after mutation"));
        undo.commit(false, &mut engine.undo_stack);
        assert_eq!(engine.undo_stack.len(), 2);
        let saved = engine.undo_stack.last().unwrap().scene.as_ref().unwrap();
        assert_eq!(
            saved.entities[0]
                .components
                .transform
                .as_ref()
                .unwrap()
                .position,
            [2., 0., 0.]
        );

        // Rebuilding is unavailable without a GPU. Neither the undo entry nor
        // the live scene may be consumed by this failed attempt.
        for _ in 0..2 {
            assert_eq!(engine.handle_undo().status, "error");
            assert_eq!(engine.undo_stack.len(), 2);
            assert_eq!(engine.undo_stack.last().unwrap().label, "run_lua");
            let world = world.borrow();
            let entity = world.entity_registry["target"];
            assert_eq!(
                world.world.get::<&Transform>(entity).unwrap().position.x,
                9.
            );
        }

        let mut blocked = undo_entry("unrepresentable change");
        blocked.scene = Err("transient projectile".to_string());
        engine.undo_stack.push(blocked);
        for _ in 0..2 {
            let response = engine.handle_undo();
            assert_eq!(response.status, "error");
            assert!(response.message.unwrap().contains("transient projectile"));
            assert_eq!(engine.undo_stack.len(), 3);
        }

        // Missing entities and missing Lua runtimes are definite no-op errors.
        let mut undo = PendingUndo::new(engine.snapshot_undo("destroy_entity"));
        let request = serde_json::from_value(json!({
            "cmd": "destroy_entity", "entity_id": "missing",
        }))
        .unwrap();
        let response = engine.handle_destroy_entity(&request);
        assert_eq!(response.status, "error");
        undo.commit(false, &mut engine.undo_stack);
        assert_eq!(engine.undo_stack.len(), 3);
        // Code that does not parse changed nothing and costs no undo step.
        undo = PendingUndo::new(engine.snapshot_undo("run_lua"));
        let request = serde_json::from_value(json!({
            "cmd": "run_lua", "code": "entity.set_position('target', 1, 2",
        }))
        .unwrap();
        let response = engine.handle_run_lua(&request, &mut undo.may_have_changed);
        assert_eq!(response.status, "error");
        assert!(!undo.may_have_changed);
        // Expressions still return their value.
        let request = serde_json::from_value(json!({"cmd": "run_lua", "code": "1 + 2"})).unwrap();
        let response = engine.handle_run_lua(&request, &mut false);
        assert_eq!(response.data.unwrap()["result"], "3");
        undo.commit(false, &mut engine.undo_stack);
        assert_eq!(engine.undo_stack.len(), 3);
        engine.script_runtime = None;
        undo = PendingUndo::new(engine.snapshot_undo("run_lua"));
        let request =
            serde_json::from_value(json!({"cmd": "run_lua", "code": "return 1"})).unwrap();
        let response = engine.handle_run_lua(&request, &mut undo.may_have_changed);
        assert_eq!(response.status, "error");
        undo.commit(false, &mut engine.undo_stack);
        assert_eq!(engine.undo_stack.len(), 3);
        engine.scene_world = None;
        assert!(engine.snapshot_undo("destroy_entity").is_none());
    }

    #[test]
    fn failed_scene_replacement_preserves_undo_history_and_current_path() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "naive-scene-replacement-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("replacement.yaml"), "name: next\nentities: []\n").unwrap();
        let mut engine = Engine::new(CliArgs {
            command: None,
            scene: None,
            pipeline: None,
            project: root.to_string_lossy().into_owned(),
            socket: String::new(),
            hud: false,
            editor_mode: false,
        });
        let original = root.join("original.yaml");
        engine.scene_path = Some(original.clone());
        engine.undo_stack.push(undo_entry("keep this change"));
        // No renderer is available, so replacement must fail without discarding
        // the current document's history. Asset preflight failures use the same return path.
        engine.load_requested_scene("replacement.yaml");
        assert_eq!(engine.scene_path, Some(original));
        assert_eq!(engine.undo_stack.len(), 1);
        assert_eq!(engine.undo_stack[0].label, "keep this change");
        std::fs::remove_dir_all(root).unwrap();
    }

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
