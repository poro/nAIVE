//! Wire protocol shared by the runtime socket and its MCP bridge.
//!
//! The registry below defines command names, typed arguments and MCP schemas
//! together. The envelope retains the existing flat JSON protocol so callers
//! can migrate without changing their messages.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

trait Argument {
    fn schema() -> Value;
    fn required() -> bool {
        true
    }
}
impl Argument for String {
    fn schema() -> Value {
        json!({"type": "string"})
    }
}
impl Argument for f32 {
    fn schema() -> Value {
        json!({"type": "number"})
    }
}
impl Argument for u32 {
    fn schema() -> Value {
        json!({"type": "integer", "minimum": 0, "maximum": u32::MAX})
    }
}
impl Argument for bool {
    fn schema() -> Value {
        json!({"type": "boolean"})
    }
}
impl Argument for Map<String, Value> {
    fn schema() -> Value {
        json!({"type": "object"})
    }
}
impl Argument for [f32; 3] {
    fn schema() -> Value {
        json!({"type": "array", "items": {"type":"number"}, "minItems": 3, "maxItems":3})
    }
}
impl Argument for Vec<String> {
    fn schema() -> Value {
        json!({"type": "array", "items": {"type":"string"}})
    }
}
impl<T: Argument> Argument for Option<T> {
    fn schema() -> Value {
        T::schema()
    }
    fn required() -> bool {
        false
    }
}

macro_rules! commands {
    ($($variant:ident => $name:literal, $description:literal, {$($field:ident: $ty:ty),* $(,)?});* $(;)?) => {
        /// Validated commands. Unknown commands/fields and incorrect argument
        /// types are rejected before they reach the engine thread.
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(tag = "cmd", deny_unknown_fields)]
        pub enum Command {
            $(#[serde(rename = $name)] $variant { $($field: $ty),* }),*
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CommandKind { $($variant),* }

        impl CommandKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];
            pub fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),* }
            }
            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant)),*, _ => None }
            }
            pub fn tool_definition(self) -> Value {
                match self { $(Self::$variant => {
                    #[allow(unused_mut)]
                    let mut properties = Map::new();
                    #[allow(unused_mut)]
                    let mut required: Vec<&str> = Vec::new();
                    $(properties.insert(stringify!($field).into(), <$ty as Argument>::schema());
                    if <$ty as Argument>::required() { required.push(stringify!($field)); })*
                    json!({"name": concat!("naive_", $name), "description": $description,
                        "inputSchema": {"type": "object", "properties": properties,
                            "required": required, "additionalProperties": false}})
                }),* }
            }
        }

        impl Command {
            pub fn kind(&self) -> CommandKind {
                match self { $(Self::$variant { .. } => CommandKind::$variant),* }
            }
        }
    }
}

commands! {
    ListEntities => "list_entities", "List the IDs and tags of all entities in the current scene.", {};
    QueryEntity => "query_entity", "Query component data for an entity by ID.", {
        entity_id: String, component: Option<String>
    };
    ModifyEntity => "modify_entity", "Patch an entity's transform or light components.", {
        entity_id: String, components: Map<String, Value>
    };
    SpawnEntity => "spawn_entity", "Spawn an entity with optional components (transform, mesh_renderer, collider, rigid_body, point_light or camera) and tags. Procedural mesh names include procedural:cube and procedural:sphere.", {
        entity_id: String, components: Option<Map<String, Value>>, tags: Option<Vec<String>>
    };
    DestroyEntity => "destroy_entity", "Destroy an entity by ID.", { entity_id: String };
    EmitEvent => "emit_event", "Emit a game event with an optional object payload.", {
        event_type: String, data: Option<Map<String, Value>>
    };
    QueryEvents => "query_events", "Query recent events; filter matches event type names, limit defaults to 100.", {
        filter: Option<String>, limit: Option<u32>
    };
    InjectInput => "inject_input", "Inject input: key_press/key_release with key; mouse_press/mouse_release with button; mouse_motion with dx/dy.", {
        action: String, key: Option<String>, button: Option<String>, dx: Option<f32>, dy: Option<f32>
    };
    RuntimeControl => "runtime_control", "Pause, resume or query status of the runtime.", { action: String };
    SaveScene => "save_scene", "Save scene YAML inside the project. Path defaults to scenes/editor_scene.yaml.", { path: Option<String> };
    GetSceneYaml => "get_scene_yaml", "Get the current scene as YAML.", {};
    SetCamera => "set_camera", "Move the editor camera; yaw/pitch are degrees and look_at is a world position.", {
        position: Option<[f32; 3]>, yaw: Option<f32>, pitch: Option<f32>, look_at: Option<[f32; 3]>
    };
    EditorStatus => "editor_status", "Get editor mode, entity count, camera and scene path.", {};
    CaptureFrame => "capture_frame", "Render the scene without UI to a PNG inside the project (default captures/frame.png).", { path: Option<String> };
    Benchmark => "benchmark", "Run whole frames offscreen, advancing simulation, and report throughput and GPU pass timings. Frames defaults to 240.", { frames: Option<u32> };
    GpuTimings => "gpu_timings", "GPU milliseconds per frame and render pass over the last 60 frames. Use benchmark to avoid display throttling.", {};
    RunLua => "run_lua", "Run limited Lua with the registered entity, physics, scene, event, camera, particle and audio APIs. Returns the result and print output.", { code: String }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandRequest {
    pub cmd: String,
    #[serde(flatten)]
    pub params: HashMap<String, Value>,
}

impl CommandRequest {
    pub fn validate(&self) -> Result<Command, String> {
        let value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        validate_numbers(&value)?;
        serde_json::from_value(value).map_err(|e| format!("Invalid {} command: {}", self.cmd, e))
    }

    /// MCP and the socket use exactly the same argument validation.
    pub fn from_tool(name: &str, args: &Value) -> Result<Self, String> {
        let command = name
            .strip_prefix("naive_")
            .and_then(CommandKind::from_name)
            .ok_or_else(|| format!("Unknown tool: {name}"))?;
        let args = match args {
            Value::Null => Map::new(),
            Value::Object(args) => args.clone(),
            _ => return Err("Tool arguments must be an object".into()),
        };
        if args.contains_key("cmd") {
            return Err("Tool arguments must not override cmd".into());
        }
        let request = Self {
            cmd: command.name().into(),
            params: args.into_iter().collect(),
        };
        request.validate()?;
        Ok(request)
    }
}

fn validate_numbers(value: &Value) -> Result<(), String> {
    match value {
        Value::Number(number) => {
            let number = number.as_f64().ok_or("Invalid command number")?;
            if !number.is_finite() || number.abs() > f64::from(f32::MAX) {
                return Err("Command numbers must fit a finite f32".into());
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_numbers(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_numbers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl From<Command> for CommandRequest {
    fn from(command: Command) -> Self {
        // Serializing an enum containing only JSON-compatible fields cannot fail.
        serde_json::from_value(serde_json::to_value(command).expect("serializable command"))
            .expect("command envelope")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl CommandResponse {
    pub fn ok(data: Value) -> Self {
        Self {
            status: "ok".into(),
            data: Some(data),
            message: None,
        }
    }
    pub fn ok_empty() -> Self {
        Self {
            status: "ok".into(),
            data: None,
            message: None,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: "error".into(),
            data: None,
            message: Some(message.into()),
        }
    }
}

pub fn tool_definitions() -> Vec<Value> {
    CommandKind::ALL
        .iter()
        .map(|kind| kind.tool_definition())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip_preserves_existing_flat_protocol() {
        let wire = json!({"cmd":"set_camera", "position":[1.0,2.0,3.0], "yaw":90.0});
        let request: CommandRequest = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            request.validate().unwrap(),
            Command::SetCamera { .. }
        ));
        assert_eq!(serde_json::to_value(request).unwrap(), wire);
    }

    #[test]
    fn bridge_and_socket_reject_the_same_invalid_arguments() {
        for args in [
            json!({}),
            json!({"entity_id":12}),
            json!({"entity_id":"a", "extra":true}),
        ] {
            assert!(CommandRequest::from_tool("naive_query_entity", &args).is_err());
            let mut wire = args;
            wire["cmd"] = json!("query_entity");
            assert!(serde_json::from_value::<CommandRequest>(wire)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(CommandRequest::from_tool("naive_set_camera", &json!({"position":[1,2]})).is_err());
        assert!(CommandRequest::from_tool("naive_benchmark", &json!({"frames":-1})).is_err());
        assert!(CommandRequest::from_tool("naive_set_camera", &json!({"yaw":1e100})).is_err());
        assert!(
            CommandRequest::from_tool("naive_list_entities", &json!({"cmd":"run_lua"})).is_err()
        );
    }

    #[test]
    fn every_tool_uses_the_shared_registry_and_argument_requirements() {
        for (kind, tool) in CommandKind::ALL.iter().zip(tool_definitions()) {
            assert_eq!(tool["name"], format!("naive_{}", kind.name()));
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let camera = CommandKind::SetCamera.tool_definition();
        assert_eq!(
            camera["inputSchema"]["properties"]["position"]["maxItems"],
            3
        );
        assert_eq!(
            CommandKind::RunLua.tool_definition()["inputSchema"]["required"],
            json!(["code"])
        );
    }
}
