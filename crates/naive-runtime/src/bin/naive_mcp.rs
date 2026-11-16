//! MCP (Model Context Protocol) server binary for nAIVE engine.
//!
//! Bridges JSON-RPC 2.0 over stdin/stdout to the engine's Unix domain socket.
//! Usage: naive_mcp [socket_path]
//! Default socket: /tmp/naive-runtime.sock

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use naive_core::command::{Command, CommandRequest, CommandResponse};
use serde_json::{json, Value};

const DEFAULT_SOCKET: &str = "/tmp/naive-runtime.sock";
const PROTOCOL_VERSION: &str = "2024-11-05";

fn main() {
    let socket_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOCKET.into());

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

    if tool_name == "naive_beautify_scene" {
        return handle_beautify(id, args, socket_path);
    }
    let cmd_request = match CommandRequest::from_tool(tool_name, args) {
        Ok(request) => request,
        Err(error) => return json_rpc_error(id, -32602, &error),
    };

    match send_command(socket_path, &cmd_request) {
        Ok(response) => {
            let is_error = response.status != "ok";
            let text = serde_json::to_string_pretty(&response).unwrap_or_default();
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": text}],
                    "isError": is_error
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

fn send_command(socket_path: &str, command: &CommandRequest) -> Result<CommandResponse, String> {
    command.validate()?;
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Cannot connect to {}: {}", socket_path, e))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("set_read_timeout: {}", e))?;

    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("set_write_timeout: {}", e))?;

    let cmd_json = serde_json::to_string(command).map_err(|e| format!("serialize: {}", e))?;

    stream
        .write_all(cmd_json.as_bytes())
        .map_err(|e| format!("write: {}", e))?;
    stream
        .write_all(b"\n")
        .map_err(|e| format!("write newline: {}", e))?;
    stream.flush().map_err(|e| format!("flush: {}", e))?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read: {}", e))?;

    serde_json::from_str(&line).map_err(|e| format!("parse response: {}", e))
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
    let scene_yaml = match send_command(socket_path, &Command::GetSceneYaml {}.into()) {
        Ok(response) => {
            if let Some(yaml) = response
                .data
                .as_ref()
                .and_then(|data| data["yaml"].as_str())
            {
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
    let scene: naive_client::scene::SceneFile = match naive_client::scene::parse_scene(&scene_yaml)
    {
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

    let export_only = args
        .get("export_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if export_only {
        // Just export the GLB
        match naive_client::beautify::export_scene_to_glb(&project_root, &scene) {
            Ok(glb_data) => {
                let exported = (|| -> Result<std::path::PathBuf, String> {
                    let glb_path = naive_client::project_path::resolve(
                        &project_root,
                        "assets/splats/beautify_export.glb",
                    )?;
                    if let Some(parent) = glb_path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&glb_path, &glb_data).map_err(|e| e.to_string())?;
                    Ok(glb_path)
                })();
                let glb_path = match exported {
                    Ok(path) => path,
                    Err(error) => {
                        return json!({"jsonrpc": "2.0", "id": id, "result": {
                            "content": [{"type":"text", "text":format!("Export failed: {error}")}],
                            "isError":true
                        }})
                    }
                };
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
                result
                    .splat_count
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_string())
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
    let mut definitions = naive_core::command::tool_definitions();
    definitions.push(        json!({
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
        }));
    definitions
}
