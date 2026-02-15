use std::collections::HashMap;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use winit::event::MouseButton;

use crate::components::*;
use crate::events::EventBus;
use crate::input::InputState;
use crate::world::SceneWorld;

/// A command request received via the Unix socket.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandRequest {
    pub cmd: String,
    #[serde(flatten)]
    pub params: HashMap<String, Value>,
}

/// A command response sent back via the socket.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl CommandResponse {
    pub fn ok(data: Value) -> Self {
        Self { status: "ok".into(), data: Some(data), message: None }
    }
    pub fn ok_empty() -> Self {
        Self { status: "ok".into(), data: None, message: None }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self { status: "error".into(), data: None, message: Some(msg.into()) }
    }
}

/// A pending command awaiting processing on the main thread.
pub struct PendingCommand {
    pub request: CommandRequest,
    pub responder: mpsc::Sender<CommandResponse>,
}

/// Command socket server. Runs a tokio runtime on a background thread,
/// accepts connections on a Unix domain socket, and forwards commands
/// to the main thread via a channel.
pub struct CommandServer {
    cmd_rx: mpsc::Receiver<PendingCommand>,
    pub socket_path: String,
}

impl CommandServer {
    pub fn start(socket_path: &str) -> Result<Self, String> {
        let _ = std::fs::remove_file(socket_path);

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let path = socket_path.to_string();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime for command server");

            rt.block_on(async move {
                let listener = match tokio::net::UnixListener::bind(&path) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("Failed to bind command socket at {}: {}", path, e);
                        return;
                    }
                };
                tracing::info!("Command socket listening on {}", path);

                loop {
                    match listener.accept().await {
                        Ok((stream, _addr)) => {
                            let tx = cmd_tx.clone();
                            tokio::spawn(handle_connection(stream, tx));
                        }
                        Err(e) => {
                            tracing::warn!("Command socket accept error: {}", e);
                        }
                    }
                }
            });
        });

        Ok(Self { cmd_rx, socket_path: socket_path.to_string() })
    }

    /// Poll for pending commands (non-blocking).
    pub fn poll(&self) -> Vec<PendingCommand> {
        let mut cmds = Vec::new();
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            cmds.push(cmd);
        }
        cmds
    }
}

impl Drop for CommandServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    cmd_tx: mpsc::Sender<PendingCommand>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: CommandRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = CommandResponse::error(format!("Invalid JSON: {}", e));
                let j = serde_json::to_string(&resp).unwrap_or_default();
                let _ = writer.write_all(format!("{}\n", j).as_bytes()).await;
                continue;
            }
        };

        let (resp_tx, resp_rx) = mpsc::channel();
        let pending = PendingCommand { request, responder: resp_tx };

        if cmd_tx.send(pending).is_err() {
            let resp = CommandResponse::error("Engine shut down");
            let j = serde_json::to_string(&resp).unwrap_or_default();
            let _ = writer.write_all(format!("{}\n", j).as_bytes()).await;
            break;
        }

        match resp_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(response) => {
                let j = serde_json::to_string(&response).unwrap_or_default();
                let _ = writer.write_all(format!("{}\n", j).as_bytes()).await;
            }
            Err(_) => {
                let resp = CommandResponse::error("Command timed out");
                let j = serde_json::to_string(&resp).unwrap_or_default();
                let _ = writer.write_all(format!("{}\n", j).as_bytes()).await;
            }
        }
    }
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
    match req.cmd.as_str() {
        "list_entities" => cmd_list_entities(scene_world),
        "query_entity" => cmd_query_entity(req, scene_world),
        "modify_entity" => cmd_modify_entity(req, scene_world),
        "spawn_entity" => cmd_spawn_entity(req, scene_world),
        "destroy_entity" => cmd_destroy_entity(req, scene_world),
        "emit_event" => cmd_emit_event(req, event_bus),
        "query_events" => cmd_query_events(req, event_bus),
        "inject_input" => cmd_inject_input(req, input_state),
        "runtime_control" => cmd_runtime_control(req, paused),
        _ => CommandResponse::error(format!("Unknown command: {}", req.cmd)),
    }
}

fn get_str_param<'a>(req: &'a CommandRequest, key: &str) -> Option<&'a str> {
    req.params.get(key).and_then(|v| v.as_str())
}

fn get_f32_param(req: &CommandRequest, key: &str) -> Option<f32> {
    req.params.get(key).and_then(|v| v.as_f64()).map(|v| v as f32)
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

fn cmd_list_entities(scene_world: &Option<SceneWorld>) -> CommandResponse {
    let sw = match scene_world {
        Some(sw) => sw,
        None => return CommandResponse::error("No scene loaded"),
    };
    let entities: Vec<Value> = sw.entity_registry.iter().map(|(id, &entity)| {
        let tags = sw.world.get::<&Tags>(entity)
            .map(|t| t.0.clone()).unwrap_or_default();
        json!({"id": id, "tags": tags})
    }).collect();
    CommandResponse::ok(json!({"entities": entities}))
}

fn cmd_query_entity(req: &CommandRequest, scene_world: &Option<SceneWorld>) -> CommandResponse {
    let entity_id = match get_str_param(req, "entity_id") {
        Some(id) => id,
        None => return CommandResponse::error("Missing 'entity_id' parameter"),
    };
    let sw = match scene_world {
        Some(sw) => sw,
        None => return CommandResponse::error("No scene loaded"),
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
            data.insert("transform".into(), json!({
                "position": [t.position.x, t.position.y, t.position.z],
                "rotation": [t.rotation.x, t.rotation.y, t.rotation.z, t.rotation.w],
                "scale": [t.scale.x, t.scale.y, t.scale.z],
            }));
        }
    }
    if include("camera") {
        if let Ok(c) = sw.world.get::<&Camera>(entity) {
            data.insert("camera".into(), json!({
                "fov": c.fov_degrees, "near": c.near, "far": c.far,
                "role": format!("{:?}", c.role),
            }));
        }
    }
    if include("point_light") {
        if let Ok(pl) = sw.world.get::<&PointLight>(entity) {
            data.insert("point_light".into(), json!({
                "color": [pl.color.x, pl.color.y, pl.color.z],
                "intensity": pl.intensity, "range": pl.range,
            }));
        }
    }
    if include("player") {
        if let Ok(p) = sw.world.get::<&Player>(entity) {
            data.insert("player".into(), json!({
                "yaw": p.yaw, "pitch": p.pitch,
                "height": p.height, "radius": p.radius,
            }));
        }
    }
    if include("mesh_renderer") {
        if let Ok(mr) = sw.world.get::<&MeshRenderer>(entity) {
            data.insert("mesh_renderer".into(), json!({
                "mesh_handle": mr.mesh_handle.0,
                "material_handle": mr.material_handle.0,
            }));
        }
    }

    if filter.is_some() && data.is_empty() {
        return CommandResponse::error(format!(
            "Component '{}' not found on '{}'", filter.unwrap(), entity_id
        ));
    }
    CommandResponse::ok(Value::Object(data))
}

fn cmd_modify_entity(req: &CommandRequest, scene_world: &mut Option<SceneWorld>) -> CommandResponse {
    let entity_id = match get_str_param(req, "entity_id") {
        Some(id) => id.to_string(),
        None => return CommandResponse::error("Missing 'entity_id' parameter"),
    };
    let sw = match scene_world {
        Some(sw) => sw,
        None => return CommandResponse::error("No scene loaded"),
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
                if let Some(v) = json_array_to_vec3(arr) { transform.position = v; }
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
                if let Some(v) = json_array_to_vec3(arr) { transform.scale = v; }
            }
            transform.dirty = true;
        }
    }

    if let Some(pl) = components.get("point_light") {
        if let Ok(mut light) = sw.world.get::<&mut PointLight>(entity) {
            if let Some(arr) = pl.get("color").and_then(|v| v.as_array()) {
                if let Some(v) = json_array_to_vec3(arr) { light.color = v; }
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

fn cmd_spawn_entity(req: &CommandRequest, scene_world: &mut Option<SceneWorld>) -> CommandResponse {
    let entity_id = match get_str_param(req, "entity_id") {
        Some(id) => id.to_string(),
        None => return CommandResponse::error("Missing 'entity_id' parameter"),
    };
    let sw = match scene_world {
        Some(sw) => sw,
        None => return CommandResponse::error("No scene loaded"),
    };
    if sw.entity_registry.contains_key(&entity_id) {
        return CommandResponse::error(format!("Entity '{}' already exists", entity_id));
    }

    let components = req.params.get("components");
    let tags_param = req.params.get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut transform = Transform::default();
    if let Some(t) = components.and_then(|c| c.get("transform")) {
        if let Some(arr) = t.get("position").and_then(|v| v.as_array()) {
            if let Some(v) = json_array_to_vec3(arr) { transform.position = v; }
        }
        if let Some(arr) = t.get("scale").and_then(|v| v.as_array()) {
            if let Some(v) = json_array_to_vec3(arr) { transform.scale = v; }
        }
        if let Some(arr) = t.get("rotation").and_then(|v| v.as_array()) {
            if arr.len() == 3 {
                transform.rotation = crate::world::euler_degrees_to_quat([
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[1].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                ]);
            }
        }
    }

    let eid = EntityId(entity_id.clone());
    let tags = Tags(tags_param);

    let entity = if let Some(pl) = components.and_then(|c| c.get("point_light")) {
        let color = pl.get("color").and_then(|v| v.as_array())
            .and_then(|a| json_array_to_vec3(a)).unwrap_or(glam::Vec3::ONE);
        let intensity = pl.get("intensity").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let range = pl.get("range").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
        sw.world.spawn((eid, tags, transform, PointLight { color, intensity, range }))
    } else if let Some(cam) = components.and_then(|c| c.get("camera")) {
        let fov = cam.get("fov").and_then(|v| v.as_f64()).unwrap_or(75.0) as f32;
        let near = cam.get("near").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
        let far = cam.get("far").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
        let role_str = cam.get("role").and_then(|v| v.as_str()).unwrap_or("other");
        let role = if role_str == "main" { CameraRole::Main } else { CameraRole::Other(role_str.into()) };
        sw.world.spawn((eid, tags, transform, Camera { fov_degrees: fov, near, far, role, aspect_ratio: 16.0/9.0 }))
    } else {
        sw.world.spawn((eid, tags, transform))
    };

    sw.entity_registry.insert(entity_id.clone(), entity);
    CommandResponse::ok(json!({"entity_id": entity_id}))
}

fn cmd_destroy_entity(req: &CommandRequest, scene_world: &mut Option<SceneWorld>) -> CommandResponse {
    let entity_id = match get_str_param(req, "entity_id") {
        Some(id) => id.to_string(),
        None => return CommandResponse::error("Missing 'entity_id' parameter"),
    };
    let sw = match scene_world {
        Some(sw) => sw,
        None => return CommandResponse::error("No scene loaded"),
    };
    let entity = match sw.entity_registry.remove(&entity_id) {
        Some(e) => e,
        None => return CommandResponse::error(format!("Entity '{}' not found", entity_id)),
    };
    let _ = sw.world.despawn(entity);
    CommandResponse::ok_empty()
}

// --- Event commands ---

fn cmd_emit_event(req: &CommandRequest, event_bus: &mut EventBus) -> CommandResponse {
    let event_type = match get_str_param(req, "event_type") {
        Some(t) => t.to_string(),
        None => return CommandResponse::error("Missing 'event_type' parameter"),
    };
    let data: HashMap<String, Value> = req.params.get("data")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    event_bus.emit(&event_type, data);
    CommandResponse::ok_empty()
}

fn cmd_query_events(req: &CommandRequest, event_bus: &EventBus) -> CommandResponse {
    let filter = get_str_param(req, "filter").map(String::from);
    let limit = req.params.get("limit")
        .and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(100);

    let log = event_bus.get_log();
    let events: Vec<Value> = log.iter()
        .filter(|e| filter.as_ref().map_or(true, |f| e.event_type.contains(f.as_str())))
        .rev()
        .take(limit)
        .map(|e| json!({
            "event_type": e.event_type,
            "data": e.data,
            "timestamp": e.timestamp,
        }))
        .collect();
    CommandResponse::ok(json!({"events": events, "count": events.len()}))
}

// --- Input commands ---

fn cmd_inject_input(req: &CommandRequest, input_state: &mut Option<InputState>) -> CommandResponse {
    let input = match input_state {
        Some(i) => i,
        None => return CommandResponse::error("Input system not initialized"),
    };
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
            let btn = match get_str_param(req, "button").unwrap_or("Left") {
                "Left" => MouseButton::Left,
                "Right" => MouseButton::Right,
                "Middle" => MouseButton::Middle,
                other => return CommandResponse::error(format!("Unknown button: {}", other)),
            };
            input.inject_mouse_press(btn);
        }
        "mouse_release" => {
            let btn = match get_str_param(req, "button").unwrap_or("Left") {
                "Left" => MouseButton::Left,
                "Right" => MouseButton::Right,
                "Middle" => MouseButton::Middle,
                other => return CommandResponse::error(format!("Unknown button: {}", other)),
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
        "pause" => { *paused = true; CommandResponse::ok(json!({"paused": true})) }
        "resume" => { *paused = false; CommandResponse::ok(json!({"paused": false})) }
        "status" => CommandResponse::ok(json!({"paused": *paused})),
        other => CommandResponse::error(format!("Unknown runtime action: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_list_entities_no_scene() {
        let mut scene: Option<SceneWorld> = None;
        let resp = cmd_list_entities(&scene);
        assert_eq!(resp.status, "error");
    }

    #[test]
    fn test_runtime_control() {
        let mut paused = false;
        let req = CommandRequest { cmd: "runtime_control".into(), params: {
            let mut m = HashMap::new();
            m.insert("action".into(), json!("pause"));
            m
        }};
        let resp = cmd_runtime_control(&req, &mut paused);
        assert_eq!(resp.status, "ok");
        assert!(paused);

        let req2 = CommandRequest { cmd: "runtime_control".into(), params: {
            let mut m = HashMap::new();
            m.insert("action".into(), json!("resume"));
            m
        }};
        let resp2 = cmd_runtime_control(&req2, &mut paused);
        assert_eq!(resp2.status, "ok");
        assert!(!paused);
    }
}
