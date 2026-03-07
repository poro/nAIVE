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
        "naive_save_scene" => {
            let mut c = json!({"cmd": "save_scene"});
            copy_field(args, &mut c, "path");
            c
        }
        "naive_get_scene_yaml" => json!({"cmd": "get_scene_yaml"}),
        "naive_set_camera" => {
            let mut c = json!({"cmd": "set_camera"});
            copy_field(args, &mut c, "position");
            copy_field(args, &mut c, "yaw");
            copy_field(args, &mut c, "pitch");
            copy_field(args, &mut c, "look_at");
            c
        }
        "naive_editor_status" => json!({"cmd": "editor_status"}),
        "naive_run_lua" => {
            let mut c = json!({"cmd": "run_lua"});
            copy_field(args, &mut c, "code");
            c
        }
        "naive_beautify_scene" => {
            // Special: beautify runs locally, not via engine socket
            return handle_beautify(id, args, socket_path);
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

fn handle_beautify(id: Option<Value>, args: &Value, socket_path: &str) -> Value {
    // Step 1: Get scene YAML from the running engine
    let scene_yaml = match send_command(socket_path, &json!({"cmd": "get_scene_yaml"})) {
        Ok(response) => {
            if let Some(yaml) = response["yaml"].as_str() {
                yaml.to_string()
            } else {
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": "Error: Could not get scene YAML from engine"}],
                        "isError": true
                    }
                });
            }
        }
        Err(e) => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": format!("Error connecting to engine: {}", e)}],
                    "isError": true
                }
            });
        }
    };

    // Step 2: Parse the scene
    let scene: naive_client::scene::SceneFile = match naive_client::scene::parse_scene(&scene_yaml) {
        Ok(s) => s,
        Err(e) => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": format!("Error parsing scene: {}", e)}],
                    "isError": true
                }
            });
        }
    };

    // Step 3: Build beautify config
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut config = naive_client::beautify::config_from_env();
    if let Some(style) = args.get("style").and_then(|v| v.as_str()) {
        config.style_prompt = Some(style.to_string());
    }
    if let Some(output) = args.get("output").and_then(|v| v.as_str()) {
        config.output_ply = output.to_string();
    }

    let export_only = args.get("export_only").and_then(|v| v.as_bool()).unwrap_or(false);

    if export_only {
        // Just export the GLB
        match naive_client::beautify::export_scene_to_glb(&project_root, &scene) {
            Ok(glb_data) => {
                let glb_path = project_root.join("assets/splats/beautify_export.glb");
                if let Some(parent) = glb_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&glb_path, &glb_data);
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": format!("Exported scene geometry to GLB ({} bytes) at {}", glb_data.len(), glb_path.display())
                        }]
                    }
                });
            }
            Err(e) => {
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("Export failed: {}", e)}],
                        "isError": true
                    }
                });
            }
        }
    }

    // Step 4: Run full beautification pipeline
    match naive_client::beautify::beautify_scene(&project_root, &scene, &config) {
        Ok(result) => {
            let msg = format!(
                "Beautification complete!\nPLY saved to: {}\nBackend: {}\nSplat count: {}",
                result.ply_path.display(),
                result.backend_name,
                result.splat_count.map(|c| c.to_string()).unwrap_or("unknown".to_string())
            );
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": msg}]
                }
            })
        }
        Err(e) => {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": format!("Beautification failed: {}", e)}],
                    "isError": true
                }
            })
        }
    }
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
            "description": "Spawn a new entity with given components. Supports mesh_renderer for 3D objects (meshes: 'procedural:cube', 'procedural:sphere', or path like 'assets/meshes/model.glb'; materials: 'procedural:default' or path like 'assets/materials/red.yaml'). Supports physics via rigid_body and collider components — spawned objects will fall, bounce, and collide. Also supports transform, point_light, directional_light, and camera components.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity_id": {"type": "string", "description": "Unique ID for the new entity"},
                    "components": {"type": "object", "description": "Components: transform {position, rotation, scale}, mesh_renderer {mesh, material}, rigid_body {type: 'dynamic'|'static', mass, ccd}, collider {shape: 'box'|'sphere'|'capsule', radius, half_extents: [x,y,z], half_height, restitution, friction, is_trigger}, point_light {color, intensity, range}, camera {fov, near, far, role}"},
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
        json!({
            "name": "naive_save_scene",
            "description": "Save the current scene state to a YAML file. Serializes all entities with their transforms, meshes, lights, and cameras.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Output path relative to project root (default: 'scenes/editor_scene.yaml')"}
                },
                "required": []
            }
        }),
        json!({
            "name": "naive_get_scene_yaml",
            "description": "Get the current scene state as a YAML string. Use this to understand what entities exist and their properties before making changes.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "naive_set_camera",
            "description": "Set the editor camera position and orientation. Use look_at to point the camera at a specific world position.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "position": {"type": "array", "items": {"type": "number"}, "description": "Camera position [x, y, z]"},
                    "yaw": {"type": "number", "description": "Camera yaw in degrees"},
                    "pitch": {"type": "number", "description": "Camera pitch in degrees"},
                    "look_at": {"type": "array", "items": {"type": "number"}, "description": "Point camera at this world position [x, y, z]"}
                },
                "required": []
            }
        }),
        json!({
            "name": "naive_editor_status",
            "description": "Get editor status: mode, entity count, camera position, scene path.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "naive_run_lua",
            "description": "Execute Lua code in the running engine with full API access. Available APIs: entity.spawn_dynamic(), entity.get_position(), entity.set_position(), physics.apply_impulse(), physics.set_velocity(), physics.set_gravity(), particles.spawn_burst(), camera.shake(), scene.find_by_tag(), events.emit(), audio.play(). Use for batch operations, physics manipulation, particle effects, and anything not covered by other tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "Lua code to execute. Has access to all engine APIs (entity, physics, particles, camera, scene, events, audio). Return values and print() output are captured and returned."}
                },
                "required": ["code"]
            }
        }),
        json!({
            "name": "naive_beautify_scene",
            "description": "Beautify the current scene: export geometry to GLB, send to World Labs/Marble/local GPU for Gaussian Splat generation, and import the result. The original meshes remain for physics; the splat provides photorealistic visuals.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "style": {"type": "string", "description": "Style prompt (e.g., 'photorealistic', 'stylized anime', 'watercolor'). Default: 'photorealistic'"},
                    "output": {"type": "string", "description": "Output PLY path relative to project root. Default: 'assets/splats/beautified.ply'"},
                    "export_only": {"type": "boolean", "description": "If true, only export the GLB without sending to backend. Useful for manual upload."}
                },
                "required": []
            }
        }),
    ]
}
