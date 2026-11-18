use std::collections::HashMap;
use std::sync::mpsc;

use serde_json::{json, Value};

use crate::components::*;
use crate::events::EventBus;
use crate::input::{InputState, MouseButton};
use crate::world::SceneWorld;

pub use naive_core::command::{CommandRequest, CommandResponse};

/// Decode MCP spawning using the same component definitions as scene files.
/// Unknown components are preserved when loading authored YAML, but a live
/// command must not claim it created a component the engine cannot construct.
pub fn spawn_definition(req: &CommandRequest) -> Result<crate::scene::EntityDef, String> {
    let naive_core::command::Command::SpawnEntity {
        entity_id,
        components,
        tags,
    } = req.validate()?
    else {
        return Err("Expected spawn_entity command".into());
    };
    if entity_id.trim().is_empty() {
        return Err("entity_id must not be empty".into());
    }
    let components: crate::scene::ComponentMap =
        serde_json::from_value(Value::Object(components.unwrap_or_default()))
            .map_err(|error| format!("Invalid entity components: {error}"))?;
    if !components.extra.is_empty() {
        let mut names: Vec<_> = components.extra.keys().map(String::as_str).collect();
        names.sort_unstable();
        return Err(format!("Unsupported live components: {}", names.join(", ")));
    }
    if components.gaussian_splat.is_some() && components.mesh_renderer.is_some() {
        return Err("An entity cannot combine gaussian_splat and mesh_renderer".into());
    }
    if components.rigid_body.is_some()
        && components.collider.is_none()
        && components.character_controller.is_none()
    {
        return Err("A rigid_body requires a collider or character_controller".into());
    }
    Ok(crate::scene::EntityDef {
        id: entity_id,
        tags: tags.unwrap_or_default(),
        extends: None,
        components,
    })
}

/// A pending command awaiting processing on the main thread.
pub struct PendingCommand {
    pub request: CommandRequest,
    pub responder: tokio::sync::oneshot::Sender<CommandResponse>,
}

/// Command socket server. Runs a tokio runtime on a background thread,
/// accepts connections on a Unix domain socket, and forwards commands
/// to the main thread via a channel.
pub struct CommandServer {
    cmd_rx: mpsc::Receiver<PendingCommand>,
    pub socket_path: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    socket_identity: (u64, u64),
}

impl CommandServer {
    pub fn start(socket_path: &str) -> Result<Self, String> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let listener = bind_socket(socket_path)?;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Cannot secure command socket: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let metadata = std::fs::symlink_metadata(socket_path).map_err(|e| e.to_string())?;
        let socket_identity = (metadata.dev(), metadata.ino());
        let uid = current_uid();
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(64);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let path = socket_path.to_string();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Cannot create command runtime: {e}"))?;
        let thread = std::thread::spawn(move || {
            rt.block_on(async move {
                let listener = match tokio::net::UnixListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("Failed to bind command socket at {}: {}", path, e);
                        return;
                    }
                };
                tracing::info!("Command socket listening on {}", path);
                let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
                loop {
                    let accepted = tokio::select! {
                        _ = &mut shutdown_rx => break,
                        accepted = listener.accept() => accepted,
                    };
                    match accepted {
                        Ok((stream, _addr)) => {
                            // File permissions are explicit, and peer credentials
                            // independently enforce same-user access, even during bind.
                            if stream
                                .peer_cred()
                                .map(|peer| peer.uid() != uid)
                                .unwrap_or(true)
                            {
                                continue;
                            }
                            let Ok(slot) = slots.clone().try_acquire_owned() else {
                                continue;
                            };
                            let tx = cmd_tx.clone();
                            tokio::spawn(async move {
                                let _slot = slot;
                                handle_connection(stream, tx).await;
                            });
                        }
                        Err(e) => {
                            tracing::warn!("Command socket accept error: {}", e);
                        }
                    }
                }
            });
        });

        Ok(Self {
            cmd_rx,
            socket_path: socket_path.to_string(),
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
            socket_identity,
        })
    }

    /// Poll for pending commands (non-blocking).
    pub fn poll(&self) -> Vec<PendingCommand> {
        let mut cmds = Vec::new();
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            if !cmd.responder.is_closed() {
                cmds.push(cmd);
            }
        }
        cmds
    }
}

impl Drop for CommandServer {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&self.socket_path) {
            if (metadata.dev(), metadata.ino()) == self.socket_identity {
                let _ = std::fs::remove_file(&self.socket_path);
            }
        }
    }
}

/// Bind the command socket. An existing path is replaced only when it is a
/// socket owned by this user that nothing is listening on, which is what a
/// crashed, killed or interrupted engine leaves behind. A live engine, a
/// symlink, another user's socket or any other file is never touched.
fn bind_socket(socket_path: &str) -> Result<std::os::unix::net::UnixListener, String> {
    use std::io::ErrorKind;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    let bind_error = |e: std::io::Error| format!("Cannot bind command socket {socket_path}: {e}");
    let error = match UnixListener::bind(socket_path) {
        Ok(listener) => return Ok(listener),
        Err(error) if error.kind() == ErrorKind::AddrInUse => error,
        Err(error) => return Err(bind_error(error)),
    };
    let metadata = std::fs::symlink_metadata(socket_path).map_err(|_| bind_error(error))?;
    if !metadata.file_type().is_socket() || metadata.uid() != current_uid() {
        return Err(format!(
            "Cannot bind command socket {socket_path}: the path exists and is not a socket owned by this user. Use a unique --socket path."
        ));
    }
    match UnixStream::connect(socket_path) {
        Ok(_) => Err(format!(
            "Another engine is already listening on {socket_path}. Stop it or use a unique --socket path."
        )),
        Err(connect) if connect.kind() == ErrorKind::ConnectionRefused => {
            // Re-check the identity so a socket that a new engine created in
            // the meantime is not removed.
            let current = std::fs::symlink_metadata(socket_path).map_err(bind_error)?;
            if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
                return Err(format!(
                    "Command socket {socket_path} changed while checking it. Try again or use a unique --socket path."
                ));
            }
            std::fs::remove_file(socket_path)
                .map_err(|e| format!("Cannot remove stale command socket {socket_path}: {e}"))?;
            tracing::info!("Removed stale command socket {}", socket_path);
            UnixListener::bind(socket_path).map_err(bind_error)
        }
        Err(connect) => Err(format!(
            "Cannot bind command socket {socket_path}: it exists and could not be checked ({connect}). Use a unique --socket path."
        )),
    }
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no arguments and cannot invalidate Rust memory.
    unsafe { libc::geteuid() }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    cmd_tx: mpsc::SyncSender<PendingCommand>,
) {
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    const MAX_COMMAND_BYTES: usize = 1024 * 1024;
    loop {
        let mut line = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(10), async {
            (&mut reader)
                .take(MAX_COMMAND_BYTES as u64 + 1)
                .read_until(b'\n', &mut line)
                .await
        })
        .await;
        if !matches!(read, Ok(Ok(n)) if n > 0) {
            break;
        }
        if line.len() > MAX_COMMAND_BYTES || line.last() != Some(&b'\n') {
            let response = CommandResponse::error("Command exceeds 1 MiB or has no newline");
            let _ = send_response(&mut writer, &response).await;
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let request: CommandRequest = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = CommandResponse::error(format!("Invalid JSON: {}", e));
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
                continue;
            }
        };
        let response_timeout = match request.validate() {
            Ok(command) => command.response_timeout(),
            Err(error) => {
                if send_response(&mut writer, &CommandResponse::error(error))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };

        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let pending = PendingCommand {
            request,
            responder: resp_tx,
        };

        if let Err(error) = cmd_tx.try_send(pending) {
            let resp = CommandResponse::error(format!("Engine command queue unavailable: {error}"));
            let _ = send_response(&mut writer, &resp).await;
            break;
        }

        match tokio::time::timeout(response_timeout, resp_rx).await {
            Ok(Ok(response)) => {
                if send_response(&mut writer, &response).await.is_err() {
                    break;
                }
            }
            _ => {
                let resp = CommandResponse::error("Command timed out");
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn send_response(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    response: &CommandResponse,
) -> Result<(), ()> {
    use tokio::io::AsyncWriteExt;
    let json = serde_json::to_vec(response).map_err(|_| ())?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        writer.write_all(&json).await?;
        writer.write_all(b"\n").await
    })
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

// ---------------------------------------------------------------------------
// Command dispatch + handlers
// ---------------------------------------------------------------------------

/// Dispatch a command to the appropriate handler.
pub fn handle_command(
    req: &CommandRequest,
    scene_world: &mut Option<SceneWorld>,
    event_bus: &mut EventBus,
    input_state: &mut Option<InputState>,
    paused: &mut bool,
) -> CommandResponse {
    handle_command_rc(
        req,
        scene_world.as_mut(),
        event_bus,
        input_state.as_mut(),
        paused,
    )
}

/// Core command dispatch that takes pre-borrowed Option refs (works with both owned and Rc<RefCell> fields).
pub fn handle_command_rc(
    req: &CommandRequest,
    scene_world: Option<&mut SceneWorld>,
    event_bus: &mut EventBus,
    input_state: Option<&mut InputState>,
    paused: &mut bool,
) -> CommandResponse {
    use naive_core::command::CommandKind;
    let command = match req.validate() {
        Ok(command) => command,
        Err(error) => return CommandResponse::error(error),
    };
    match command.kind() {
        CommandKind::ListEntities => match scene_world {
            Some(sw) => cmd_list_entities(sw),
            None => CommandResponse::error("No scene loaded"),
        },
        CommandKind::QueryEntity => match scene_world {
            Some(sw) => cmd_query_entity(req, sw),
            None => CommandResponse::error("No scene loaded"),
        },
        CommandKind::ModifyEntity => match scene_world {
            Some(sw) => cmd_modify_entity(req, sw),
            None => CommandResponse::error("No scene loaded"),
        },
        CommandKind::SpawnEntity | CommandKind::DestroyEntity => {
            CommandResponse::error("Entity lifecycle commands require engine resource context")
        }
        CommandKind::EmitEvent => cmd_emit_event(req, event_bus),
        CommandKind::QueryEvents => cmd_query_events(req, event_bus),
        CommandKind::InjectInput => match input_state {
            Some(is) => cmd_inject_input(req, is),
            None => CommandResponse::error("No input state"),
        },
        CommandKind::RuntimeControl => cmd_runtime_control(req, paused),
        _ => CommandResponse::error(format!("Unknown command: {}", req.cmd)),
    }
}

fn get_str_param<'a>(req: &'a CommandRequest, key: &str) -> Option<&'a str> {
    req.params.get(key).and_then(|v| v.as_str())
}

fn get_f32_param(req: &CommandRequest, key: &str) -> Option<f32> {
    req.params
        .get(key)
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
}

fn json_array_to_vec3(arr: &[Value]) -> Option<glam::Vec3> {
    if arr.len() == 3 {
        Some(glam::Vec3::new(
            arr[0].as_f64()? as f32,
            arr[1].as_f64()? as f32,
            arr[2].as_f64()? as f32,
        ))
    } else {
        None
    }
}

// --- Entity commands ---

fn cmd_list_entities(sw: &SceneWorld) -> CommandResponse {
    let entities: Vec<Value> = sw
        .entity_registry
        .iter()
        .map(|(id, &entity)| {
            let tags = sw
                .world
                .get::<&Tags>(entity)
                .map(|t| t.0.clone())
                .unwrap_or_default();
            json!({"id": id, "tags": tags})
        })
        .collect();
    CommandResponse::ok(json!({"entities": entities}))
}

fn cmd_query_entity(req: &CommandRequest, sw: &SceneWorld) -> CommandResponse {
    let entity_id = match get_str_param(req, "entity_id") {
        Some(id) => id,
        None => return CommandResponse::error("Missing 'entity_id' parameter"),
    };
    let entity = match sw.entity_registry.get(entity_id) {
        Some(&e) => e,
        None => return CommandResponse::error(format!("Entity '{}' not found", entity_id)),
    };

    let filter = get_str_param(req, "component");
    let include = |name: &str| filter.is_none() || filter == Some(name);
    let mut data = serde_json::Map::new();

    if include("transform") {
        if let Ok(t) = sw.world.get::<&Transform>(entity) {
            data.insert(
                "transform".into(),
                json!({
                    "position": [t.position.x, t.position.y, t.position.z],
                    "rotation": [t.rotation.x, t.rotation.y, t.rotation.z, t.rotation.w],
                    "scale": [t.scale.x, t.scale.y, t.scale.z],
                }),
            );
        }
    }
    if include("camera") {
        if let Ok(c) = sw.world.get::<&Camera>(entity) {
            data.insert(
                "camera".into(),
                json!({
                    "fov": c.fov_degrees, "near": c.near, "far": c.far,
                    "role": format!("{:?}", c.role),
                }),
            );
        }
    }
    if include("point_light") {
        if let Ok(pl) = sw.world.get::<&PointLight>(entity) {
            data.insert(
                "point_light".into(),
                json!({
                    "color": [pl.color.x, pl.color.y, pl.color.z],
                    "intensity": pl.intensity, "range": pl.range,
                }),
            );
        }
    }
    if include("player") {
        if let Ok(p) = sw.world.get::<&Player>(entity) {
            data.insert(
                "player".into(),
                json!({
                    "yaw": p.yaw, "pitch": p.pitch,
                    "height": p.height, "radius": p.radius,
                }),
            );
        }
    }
    if include("mesh_renderer") {
        if let Ok(mr) = sw.world.get::<&MeshRenderer>(entity) {
            data.insert(
                "mesh_renderer".into(),
                json!({
                    "mesh_handle": mr.mesh_handle.0,
                    "material_handle": mr.material_handle.0,
                }),
            );
        }
    }

    if let Some(component) = filter {
        if data.is_empty() {
            return CommandResponse::error(format!(
                "Component '{}' not found on '{}'",
                component, entity_id
            ));
        }
    }
    CommandResponse::ok(Value::Object(data))
}

fn cmd_modify_entity(req: &CommandRequest, sw: &mut SceneWorld) -> CommandResponse {
    let entity_id = match get_str_param(req, "entity_id") {
        Some(id) => id.to_string(),
        None => return CommandResponse::error("Missing 'entity_id' parameter"),
    };
    let entity = match sw.entity_registry.get(&entity_id) {
        Some(&e) => e,
        None => return CommandResponse::error(format!("Entity '{}' not found", entity_id)),
    };
    let components = match req.params.get("components") {
        Some(c) => c,
        None => return CommandResponse::error("Missing 'components' parameter"),
    };

    if let Some(t) = components.get("transform") {
        if let Ok(mut transform) = sw.world.get::<&mut Transform>(entity) {
            if let Some(arr) = t.get("position").and_then(|v| v.as_array()) {
                if let Some(v) = json_array_to_vec3(arr) {
                    transform.position = v;
                }
            }
            if let Some(arr) = t.get("rotation").and_then(|v| v.as_array()) {
                if arr.len() == 3 {
                    transform.rotation = crate::world::euler_degrees_to_quat([
                        arr[0].as_f64().unwrap_or(0.0) as f32,
                        arr[1].as_f64().unwrap_or(0.0) as f32,
                        arr[2].as_f64().unwrap_or(0.0) as f32,
                    ]);
                } else if arr.len() == 4 {
                    transform.rotation = glam::Quat::from_xyzw(
                        arr[0].as_f64().unwrap_or(0.0) as f32,
                        arr[1].as_f64().unwrap_or(0.0) as f32,
                        arr[2].as_f64().unwrap_or(0.0) as f32,
                        arr[3].as_f64().unwrap_or(1.0) as f32,
                    );
                }
            }
            if let Some(arr) = t.get("scale").and_then(|v| v.as_array()) {
                if let Some(v) = json_array_to_vec3(arr) {
                    transform.scale = v;
                }
            }
            transform.dirty = true;
        }
    }

    if let Some(pl) = components.get("point_light") {
        if let Ok(mut light) = sw.world.get::<&mut PointLight>(entity) {
            if let Some(arr) = pl.get("color").and_then(|v| v.as_array()) {
                if let Some(v) = json_array_to_vec3(arr) {
                    light.color = v;
                }
            }
            if let Some(i) = pl.get("intensity").and_then(|v| v.as_f64()) {
                light.intensity = i as f32;
            }
            if let Some(r) = pl.get("range").and_then(|v| v.as_f64()) {
                light.range = r as f32;
            }
        }
    }

    CommandResponse::ok_empty()
}

// --- Event commands ---

fn cmd_emit_event(req: &CommandRequest, event_bus: &mut EventBus) -> CommandResponse {
    let event_type = match get_str_param(req, "event_type") {
        Some(t) => t.to_string(),
        None => return CommandResponse::error("Missing 'event_type' parameter"),
    };
    let data: HashMap<String, Value> = req
        .params
        .get("data")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    event_bus.emit(&event_type, data);
    CommandResponse::ok_empty()
}

fn cmd_query_events(req: &CommandRequest, event_bus: &EventBus) -> CommandResponse {
    let filter = get_str_param(req, "filter").map(String::from);
    let limit = req
        .params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(100);

    let log = event_bus.get_log();
    let events: Vec<Value> = log
        .iter()
        .filter(|e| {
            filter
                .as_ref()
                .is_none_or(|f| e.event_type.contains(f.as_str()))
        })
        .rev()
        .take(limit)
        .map(|e| {
            json!({
                "event_type": e.event_type,
                "data": e.data,
                "timestamp": e.timestamp,
            })
        })
        .collect();
    CommandResponse::ok(json!({"events": events, "count": events.len()}))
}

// --- Input commands ---

fn cmd_inject_input(req: &CommandRequest, input: &mut InputState) -> CommandResponse {
    let action = match get_str_param(req, "action") {
        Some(a) => a,
        None => return CommandResponse::error("Missing 'action' parameter"),
    };
    match action {
        "key_press" => {
            let key = match get_str_param(req, "key") {
                Some(k) => k,
                None => return CommandResponse::error("Missing 'key' parameter"),
            };
            input.inject_key_press(key);
        }
        "key_release" => {
            let key = match get_str_param(req, "key") {
                Some(k) => k,
                None => return CommandResponse::error("Missing 'key' parameter"),
            };
            input.inject_key_release(key);
        }
        "mouse_press" => {
            let name = get_str_param(req, "button").unwrap_or("Left");
            let Some(btn) = MouseButton::from_name(name) else {
                return CommandResponse::error(format!("Unknown button: {}", name));
            };
            input.inject_mouse_press(btn);
        }
        "mouse_release" => {
            let name = get_str_param(req, "button").unwrap_or("Left");
            let Some(btn) = MouseButton::from_name(name) else {
                return CommandResponse::error(format!("Unknown button: {}", name));
            };
            input.inject_mouse_release(btn);
        }
        "mouse_motion" => {
            let dx = get_f32_param(req, "dx").unwrap_or(0.0);
            let dy = get_f32_param(req, "dy").unwrap_or(0.0);
            input.inject_mouse_motion(dx, dy);
        }
        other => return CommandResponse::error(format!("Unknown input action: {}", other)),
    }
    CommandResponse::ok_empty()
}

// --- Runtime commands ---

fn cmd_runtime_control(req: &CommandRequest, paused: &mut bool) -> CommandResponse {
    let action = match get_str_param(req, "action") {
        Some(a) => a,
        None => return CommandResponse::error("Missing 'action' parameter"),
    };
    match action {
        "pause" => {
            *paused = true;
            CommandResponse::ok(json!({"paused": true}))
        }
        "resume" => {
            *paused = false;
            CommandResponse::ok(json!({"paused": false}))
        }
        "status" => CommandResponse::ok(json!({"paused": *paused})),
        other => CommandResponse::error(format!("Unknown runtime action: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_spawn_decodes_combined_scene_components_and_rejects_invalid_values() {
        let request: CommandRequest = serde_json::from_value(json!({
            "cmd": "spawn_entity", "entity_id": "actor", "tags": ["enemy"],
            "components": {
                "mesh_renderer": {"mesh": "procedural:cube", "material": "procedural:default"},
                "point_light": {"intensity": 7}, "health": {"max": 100},
                "rigid_body": {"type": "kinematic"}, "collider": {"shape": "box"},
                "script": {"source": "logic/actor.lua"}
            }
        }))
        .unwrap();
        let definition = spawn_definition(&request).unwrap();
        assert_eq!(definition.tags, ["enemy"]);
        assert!(definition.components.mesh_renderer.is_some());
        assert_eq!(definition.components.point_light.unwrap().intensity, 7.0);
        assert_eq!(definition.components.health.unwrap().max, 100.0);
        assert_eq!(
            definition.components.rigid_body.unwrap().body_type,
            "kinematic"
        );
        assert_eq!(
            definition.components.script.unwrap().source,
            "logic/actor.lua"
        );
        for components in [
            json!({"health":{"max":"bad"}}),
            json!({"unknown_component":{}}),
            json!({"rigid_body":{"type":"dynamic"}}),
            json!({"gaussian_splat":{"source":"test.ply"},"mesh_renderer":{"mesh":"procedural:cube","material":"materials/default.yaml"}}),
        ] {
            let request = serde_json::from_value(
                json!({"cmd":"spawn_entity", "entity_id":"bad", "components":components}),
            )
            .unwrap();
            assert!(spawn_definition(&request).is_err());
        }
    }

    fn test_socket_path() -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        format!(
            "/tmp/naive-test-{}-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn socket_is_private_and_existing_paths_are_never_replaced() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let path = test_socket_path();
        {
            let server = CommandServer::start(&path).unwrap();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.uid(), current_uid());
            assert!(CommandServer::start(&path).is_err());
            assert_eq!(
                std::fs::symlink_metadata(&path).unwrap().ino(),
                metadata.ino()
            );
            drop(server);
        }
        assert!(!std::path::Path::new(&path).exists());
        std::fs::write(&path, "keep this file").unwrap();
        assert!(CommandServer::start(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this file");
        std::fs::remove_file(path).unwrap();
    }

    /// Leave at `path` what a crashed engine leaves behind: a socket file that
    /// nothing listens on. The socket is bound but never listens, so it
    /// refuses connections for as long as any process holds it. Dropping a
    /// std listener is not enough here: a child that another test spawns at
    /// the same moment (`posix_spawnp` tries every `PATH` entry) briefly holds
    /// a copy of every open descriptor, so the dropped listener can still
    /// accept, and the server rightly refuses to replace a live socket.
    fn leave_stale_socket(path: &str) {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        let bytes = std::path::Path::new(path).as_os_str().as_bytes();
        // SAFETY: sockaddr_un is plain old data, so all zeroes is valid.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        assert!(bytes.len() < address.sun_path.len(), "socket path too long");
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (slot, &byte) in address.sun_path.iter_mut().zip(bytes) {
            *slot = byte as libc::c_char;
        }
        // SAFETY: socket has no pointer arguments; the result is checked.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
        // SAFETY: fd is a new descriptor that nothing else owns.
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: address is a valid, NUL-terminated sockaddr_un of the given size.
        let bound = unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        assert_eq!(bound, 0, "bind: {}", std::io::Error::last_os_error());
    }

    #[test]
    fn stale_socket_from_a_crashed_engine_is_replaced() {
        use std::os::unix::fs::FileTypeExt;
        let path = test_socket_path();
        leave_stale_socket(&path);
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_socket());
        assert_eq!(
            std::os::unix::net::UnixStream::connect(&path)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::ConnectionRefused
        );
        let server = CommandServer::start(&path).expect("stale socket should be replaced");
        assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
        drop(server);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn one_pending_command_does_not_block_other_clients() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let path = test_socket_path();
        let server = CommandServer::start(&path).unwrap();
        let mut pending_client = UnixStream::connect(&path).unwrap();
        pending_client
            .write_all(b"{\"cmd\":\"list_entities\"}\n")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let pending = loop {
            if let Some(pending) = server.poll().pop() {
                break pending;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "first request never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let mut other = UnixStream::connect(&path).unwrap();
        other
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        other.write_all(b"{\"cmd\":\"not_a_command\"}\n").unwrap();
        let mut line = String::new();
        BufReader::new(other).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["status"],
            "error"
        );
        pending.responder.send(CommandResponse::ok_empty()).unwrap();
    }

    #[test]
    fn oversized_command_is_rejected_before_queueing() {
        use std::io::{BufRead, BufReader, Write};
        let path = test_socket_path();
        let server = CommandServer::start(&path).unwrap();
        let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        stream.write_all(&vec![b'x'; 1024 * 1024 + 1]).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["status"],
            "error"
        );
        assert!(server.poll().is_empty());
    }

    #[test]
    fn test_command_request_parse() {
        let json = r#"{"cmd": "list_entities"}"#;
        let req: CommandRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.cmd, "list_entities");
    }

    #[test]
    fn test_command_response_ok() {
        let resp = CommandResponse::ok(json!({"test": 42}));
        assert_eq!(resp.status, "ok");
        assert!(resp.data.is_some());
    }

    #[test]
    fn test_command_response_error() {
        let resp = CommandResponse::error("something broke");
        assert_eq!(resp.status, "error");
        assert_eq!(resp.message.unwrap(), "something broke");
    }

    #[test]
    fn test_list_entities_empty_scene() {
        let scene = SceneWorld::new();
        let resp = cmd_list_entities(&scene);
        // Empty scene should return ok with empty entities list
        assert_eq!(resp.status, "ok");
    }

    #[test]
    fn test_runtime_control() {
        let mut paused = false;
        let req = CommandRequest {
            cmd: "runtime_control".into(),
            params: {
                let mut m = HashMap::new();
                m.insert("action".into(), json!("pause"));
                m
            },
        };
        let resp = cmd_runtime_control(&req, &mut paused);
        assert_eq!(resp.status, "ok");
        assert!(paused);

        let req2 = CommandRequest {
            cmd: "runtime_control".into(),
            params: {
                let mut m = HashMap::new();
                m.insert("action".into(), json!("resume"));
                m
            },
        };
        let resp2 = cmd_runtime_control(&req2, &mut paused);
        assert_eq!(resp2.status, "ok");
        assert!(!paused);
    }
}
