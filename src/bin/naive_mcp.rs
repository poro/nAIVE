//! MCP (Model Context Protocol) server binary for nAIVE engine.
//!
//! Bridges JSON-RPC 2.0 over stdin/stdout to the engine's Unix domain socket.
//! Usage: naive_mcp [socket_path]
//! Default socket: /tmp/naive-runtime.sock

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use serde_json::{json, Value};

const DEFAULT_SOCKET: &str = "/tmp/naive-runtime.sock";
const PROTOCOL_VERSION: &str = "2024-11-05";

fn main() {
    let socket_path = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_SOCKET.into());

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            Ok(_) => continue,
            Err(_) => break,
        };

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = request["method"].as_str().unwrap_or("");
        let id = request.get("id").cloned();

        // Notifications have no id and get no response
        if id.is_none() {
            continue;
        }

        let response = match method {
            "initialize" => handle_initialize(id),
            "tools/list" => handle_tools_list(id),
            "tools/call" => handle_tools_call(id, &request, &socket_path),
            _ => json_rpc_error(id, -32601, &format!("Method not found: {}", method)),
        };

        let j = serde_json::to_string(&response).unwrap_or_default();
        let _ = writeln!(stdout, "{}", j);
        let _ = stdout.flush();
    }
}

fn handle_initialize(id: Option<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "naive-mcp", "version": "0.1.0" }
        }
    })
}

fn handle_tools_list(id: Option<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "tools": tool_definitions() }
    })
}

fn handle_tools_call(id: Option<Value>, request: &Value, socket_path: &str) -> Value {
    let params = &request["params"];
    let tool_name = params["name"].as_str().unwrap_or("");
    let args = &params["arguments"];

    let cmd_request = match tool_name {
        "naive_list_entities" => json!({"cmd": "list_entities"}),
        "naive_query_entity" => {
            let mut c = json!({"cmd": "query_entity"});
            copy_field(args, &mut c, "entity_id");
            copy_field(args, &mut c, "component");
            c
        }
        "naive_modify_entity" => {
            let mut c = json!({"cmd": "modify_entity"});
            copy_field(args, &mut c, "entity_id");
            copy_field(args, &mut c, "components");
            c
        }
        "naive_spawn_entity" => {
            let mut c = json!({"cmd": "spawn_entity"});
            copy_field(args, &mut c, "entity_id");
            copy_field(args, &mut c, "components");
            copy_field(args, &mut c, "tags");
            c
        }
        "naive_destroy_entity" => {
            let mut c = json!({"cmd": "destroy_entity"});
            copy_field(args, &mut c, "entity_id");
            c
        }
        "naive_emit_event" => {
            let mut c = json!({"cmd": "emit_event"});
            copy_field(args, &mut c, "event_type");
            copy_field(args, &mut c, "data");
            c
        }
        "naive_query_events" => {
            let mut c = json!({"cmd": "query_events"});
            copy_field(args, &mut c, "filter");
            copy_field(args, &mut c, "limit");
            c
        }
        "naive_inject_input" => {
            let mut c = json!({"cmd": "inject_input"});
            copy_field(args, &mut c, "action");
            copy_field(args, &mut c, "key");
            copy_field(args, &mut c, "button");
            copy_field(args, &mut c, "dx");
            copy_field(args, &mut c, "dy");
            c
        }
        "naive_runtime_control" => {
            let mut c = json!({"cmd": "runtime_control"});
            copy_field(args, &mut c, "action");
            c
        }
        _ => return json_rpc_error(id, -32602, &format!("Unknown tool: {}", tool_name)),
    };

    match send_command(socket_path, &cmd_request) {
        Ok(response) => {
            let text = serde_json::to_string_pretty(&response).unwrap_or_default();
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": text}]
                }
            })
        }
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": format!("Error: {}", e)}],
                "isError": true
            }
        }),
    }
}

fn send_command(socket_path: &str, command: &Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Cannot connect to {}: {}", socket_path, e))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("set_read_timeout: {}", e))?;

    let cmd_json = serde_json::to_string(command)
        .map_err(|e| format!("serialize: {}", e))?;

    stream.write_all(cmd_json.as_bytes()).map_err(|e| format!("write: {}", e))?;
    stream.write_all(b"\n").map_err(|e| format!("write newline: {}", e))?;
    stream.flush().map_err(|e| format!("flush: {}", e))?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| format!("read: {}", e))?;

    serde_json::from_str(&line).map_err(|e| format!("parse response: {}", e))
}

fn copy_field(src: &Value, dst: &mut Value, key: &str) {
    if let Some(v) = src.get(key) {
        dst[key] = v.clone();
    }
}

fn json_rpc_error(id: Option<Value>, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "naive_list_entities",
            "description": "List all entities in the current scene with their IDs and tags",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }),
        json!({
            "name": "naive_query_entity",
            "description": "Query component data for a specific entity by ID",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity_id": {"type": "string", "description": "The entity ID to query"},
                    "component": {"type": "string", "description": "Specific component to query (transform, camera, point_light, player, mesh_renderer)"}
                },
                "required": ["entity_id"]
            }
        }),
        json!({
            "name": "naive_modify_entity",
            "description": "Modify components on an existing entity",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity_id": {"type": "string", "description": "The entity ID to modify"},
                    "components": {"type": "object", "description": "Component data to modify, e.g. {\"transform\": {\"position\": [1,2,3]}}"}
                },
                "required": ["entity_id", "components"]
            }
        }),
        json!({
            "name": "naive_spawn_entity",
            "description": "Spawn a new entity with given components",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity_id": {"type": "string", "description": "Unique ID for the new entity"},
                    "components": {"type": "object", "description": "Components (transform, point_light, camera)"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Tags for the entity"}
                },
                "required": ["entity_id"]
            }
        }),
        json!({
            "name": "naive_destroy_entity",
            "description": "Destroy an entity by ID",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity_id": {"type": "string", "description": "The entity ID to destroy"}
                },
                "required": ["entity_id"]
            }
        }),
        json!({
            "name": "naive_emit_event",
            "description": "Emit a game event",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "event_type": {"type": "string", "description": "Event type (e.g. 'player.damaged')"},
                    "data": {"type": "object", "description": "Event data payload"}
                },
                "required": ["event_type"]
            }
        }),
        json!({
            "name": "naive_query_events",
            "description": "Query recent game events from the event log",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "description": "Filter string to match event types"},
                    "limit": {"type": "integer", "description": "Max events to return (default 100)"}
                },
                "required": []
            }
        }),
        json!({
            "name": "naive_inject_input",
            "description": "Inject synthetic input (keyboard/mouse) into the running game",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "key_press, key_release, mouse_press, mouse_release, mouse_motion"},
                    "key": {"type": "string", "description": "Key name (W, Space, E, etc.)"},
                    "button": {"type": "string", "description": "Mouse button (Left, Right, Middle)"},
                    "dx": {"type": "number", "description": "Mouse X delta"},
                    "dy": {"type": "number", "description": "Mouse Y delta"}
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "naive_runtime_control",
            "description": "Control game runtime: pause, resume, or get status",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "pause, resume, or status"}
                },
                "required": ["action"]
            }
        }),
    ]
}
