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

/// Each command: `Variant => "wire_name", "tool description", { field: Type => "field description", ... }`.
/// Field descriptions go into the MCP input schema so agents see argument formats.
macro_rules! commands {
    ($($variant:ident => $name:literal, $description:literal, {$($field:ident: $ty:ty $(=> $doc:literal)?),* $(,)?});* $(;)?) => {
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
                    $(
                        #[allow(unused_mut)]
                        let mut schema = <$ty as Argument>::schema();
                        $(schema["description"] = json!($doc);)?
                        properties.insert(stringify!($field).into(), schema);
                        if <$ty as Argument>::required() { required.push(stringify!($field)); }
                    )*
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
    ListEntities => "list_entities", "List all entities in the current scene with their IDs and tags.", {};
    QueryEntity => "query_entity", "Query component data for an entity by ID.", {
        entity_id: String => "The entity ID to query",
        component: Option<String> => "Return only this component, e.g. transform, mesh_renderer, point_light or camera. Omit for all components."
    };
    ModifyEntity => "modify_entity", "Patch an existing entity's transform or point light. Other components cannot be changed here; use naive_run_lua for them.", {
        entity_id: String => "The entity ID to modify",
        components: Map<String, Value> => "transform {position: [x,y,z], rotation: [x,y,z] in degrees or [x,y,z,w] quaternion, scale: [x,y,z]} and/or point_light {color: [r,g,b], intensity, range}, e.g. {\"transform\": {\"position\": [1, 2, 3]}}"
    };
    SpawnEntity => "spawn_entity", "Spawn an entity using the same component format as scene YAML. Meshes are procedural:cube, procedural:sphere or a project path such as assets/meshes/model.glb; materials are procedural:default or a path such as assets/materials/red.yaml. Bodies with rigid_body and collider fall, bounce and collide.", {
        entity_id: String => "Unique ID for the new entity",
        components: Option<Map<String, Value>> => "Components: transform {position, rotation (degrees), scale}, mesh_renderer {mesh, material, cast_shadows, receive_shadows}, rigid_body {type: 'dynamic'|'static'|'kinematic', mass, ccd} (needs a collider or character_controller), collider {shape: 'box'|'sphere'|'capsule'|'trimesh', half_extents: [x,y,z], radius, half_height, restitution, friction, is_trigger}, point_light {color, intensity, range}, directional_light, camera {fov, near, far, role}, script {source: 'logic/file.lua'}, health {max, current}, collision_damage, particle_emitter, animator, gaussian_splat {source} (not combined with mesh_renderer). Unknown components are rejected.",
        tags: Option<Vec<String>> => "Tags for the entity, e.g. [\"enemy\"]"
    };
    DestroyEntity => "destroy_entity", "Destroy an entity by ID, including its physics body, script and event listeners.", {
        entity_id: String => "The entity ID to destroy"
    };
    EmitEvent => "emit_event", "Emit a game event that scripts listening with events.on receive.", {
        event_type: String => "Event type, e.g. player.damaged",
        data: Option<Map<String, Value>> => "Event payload object; listeners read it as e.data"
    };
    QueryEvents => "query_events", "Query recent game events from the event log.", {
        filter: Option<String> => "Only return events whose type matches this filter",
        limit: Option<u32> => "Maximum number of events to return (default 100)"
    };
    InjectInput => "inject_input", "Inject synthetic keyboard or mouse input into the running game.", {
        action: String => "key_press, key_release, mouse_press, mouse_release or mouse_motion",
        key: Option<String> => "Key name for key_press/key_release, e.g. W, Space, E",
        button: Option<String> => "Mouse button for mouse_press/mouse_release: Left, Right or Middle",
        dx: Option<f32> => "Horizontal mouse movement for mouse_motion",
        dy: Option<f32> => "Vertical mouse movement for mouse_motion"
    };
    RuntimeControl => "runtime_control", "Pause, resume or get the status of the game runtime.", {
        action: String => "pause, resume or status"
    };
    SaveScene => "save_scene", "Save the current scene as YAML inside the project, keeping authored scripts, physics and gameplay components with live transform and light edits. Runtime state that a scene file cannot represent is reported as an error.", {
        path: Option<String> => "Output path relative to the project root (default scenes/editor_scene.yaml)"
    };
    GetSceneYaml => "get_scene_yaml", "Get the current scene as YAML. Use it to see which entities exist and their properties before making changes.", {};
    SetCamera => "set_camera", "Set the editor camera position and orientation. Use look_at to point the camera at a world position.", {
        position: Option<[f32; 3]> => "Camera position [x, y, z]",
        yaw: Option<f32> => "Yaw in degrees",
        pitch: Option<f32> => "Pitch in degrees",
        look_at: Option<[f32; 3]> => "Point the camera at this world position [x, y, z]"
    };
    EditorStatus => "editor_status", "Get editor status: mode, entity count, camera position and scene path.", {};
    CaptureFrame => "capture_frame", "Render the current frame to a PNG inside the project and return its path, so you can look at what the scene actually shows. By default the image is the 3D scene only; set include_ui to see the HUD as well.", {
        path: Option<String> => "Where to write the PNG, relative to the project root (default captures/frame.png)",
        include_ui: Option<bool> => "Also draw the HUD (what Lua drew with ui.text and ui.rect in the last frame) over the scene. Default false."
    };
    Benchmark => "benchmark", "Measure what the engine sustains: run whole frames back to back (simulation, swarms and every render pass) into an offscreen target without presenting, then report frames per second, milliseconds per frame and GPU time per pass. Unaffected by vsync or by the window being in the background. The simulation really advances.", {
        frames: Option<u32> => "Frames to run (default 240)"
    };
    GpuTimings => "gpu_timings", "GPU milliseconds per frame over the last 60 frames, for the whole frame and per render pass (the swarm step is \"swarms\"), from timestamp queries. On screen with vsync the GPU slows to fit the display refresh, so use naive_benchmark to measure headroom.", {};
    RunLua => "run_lua", "Run Lua in the running engine, sandboxed with instruction and memory limits. Tables: entity (spawn, spawn_dynamic, destroy, get_position/set_position, get_rotation/set_rotation, set_scale, find_by_tag, damage, heal, set_base_color, set_emission, ...), physics (apply_impulse, apply_force, set_velocity, raycast, hitscan), events (emit, on, off), audio (play_sfx, play_music, stop_music), particles (spawn_burst), camera (shake, world_to_screen), animation, mesh, input and swarm. Use it for batch edits and anything the other tools do not cover.", {
        code: String => "Lua code to run. Its return value and print() output are returned."
    }
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

    #[test]
    fn every_tool_argument_is_described_for_agents() {
        for tool in tool_definitions() {
            let properties = tool["inputSchema"]["properties"].as_object().unwrap();
            for (field, schema) in properties {
                assert!(
                    schema["description"]
                        .as_str()
                        .is_some_and(|d| !d.is_empty()),
                    "{} argument {field} has no description",
                    tool["name"]
                );
            }
        }
    }
}
