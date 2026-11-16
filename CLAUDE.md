# nAIVE Engine — Claude Code Context

## Project

AI-native game engine — create worlds with YAML, Lua, and natural language.

- **Language:** Rust (2021 edition)
- **Renderer:** wgpu + SLANG shaders
- **Physics:** Rapier3D
- **Scripting:** Lua 5.4 (mlua)
- **ECS:** hecs
- **Audio:** kira
- **Current version:** 0.1.19

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `naive-core` | Scene schema, shared types (YAML deserialization) |
| `naive-client` | Engine loop, renderer, physics, scripting, world management |
| `naive-runtime` | CLI binary (`naive`, `naive-runtime`, `naive_mcp`) |
| `naive-server` | Multiplayer server (stub — future) |
| `naive-web` | WASM + WebGPU build for browser playground (`crates/naive-web`, `cdylib`) |

## Key Source Files

| File | Responsibility |
|------|---------------|
| `crates/naive-core/src/scene.rs` | YAML scene schema types (`SceneDef`, `EntityDef`, `RigidBodyDef`, etc.) |
| `crates/naive-client/src/engine.rs` | Main `Engine` struct, game loop (`run_frame` → `tick_simulation`, `prepare_render`, `render_frame`), camera, scene loading |
| `crates/naive-client/src/sim.rs` | GPU-free frame steps shared by the engine and the headless test runner (character controller, fixed physics steps, collision/event dispatch, script updates) |
| `crates/naive-client/src/physics.rs` | `PhysicsWorld` — Rapier3D wrapper (bodies, colliders, CCD, impulse/force) |
| `crates/naive-client/src/world.rs` | `SceneWorld` (ECS + registry), `EntityCommandQueue`, entity spawning, pooling |
| `crates/naive-client/src/scripting.rs` | Lua API registration (physics, entity, camera, scene, events) |
| `crates/naive-client/src/renderer.rs` | wgpu render pipeline, instance buffers, particles |
| `crates/naive-client/src/pipeline/` | CompiledPipeline — YAML render-pass DAG (split into `def`/`resource`/`compiler`/`executor`/`mod`) |
| `crates/naive-client/src/debug_draw.rs` | Wireframe physics collider visualization (toggle: H key) |
| `crates/naive-client/src/command.rs` | Command socket — newline-delimited JSON over a Unix domain socket (default `/tmp/naive-runtime.sock`); `naive_mcp` bridges it to MCP |
| `crates/naive-client/src/test_runner.rs` | Headless test runner for `naive test` CLI command |
| `crates/naive-client/src/dev_log.rs` | `naive submit-log` — POST dev.log as GitHub Issue |
| `crates/naive-client/src/demos.rs` | `naive demo` — 19 embedded demos with interactive browser |
| `crates/naive-client/src/editor_camera.rs` | Free fly camera for `naive edit` editor mode |
| `crates/naive-client/src/texture_cache.rs` | TextureCache — loads PNG/JPG/WEBP textures, creates wgpu bind groups |
| `crates/naive-client/src/beautify.rs` | Scene beautification pipeline (simple mesh → Gaussian splat conversion) |
| `crates/naive-web/src/lib.rs` | WASM entry point for browser playground (wgpu WebGPU init, panic hook, logging) |

## Architecture Patterns

- **Deferred commands:** Lua scripts push commands to `EntityCommandQueue`; engine processes them end-of-frame to avoid aliasing/dangling pointers.
- **Shared mutable state via `Rc<RefCell<T>>`:** Lua API closures capture `Rc<RefCell<PhysicsWorld>>` / `Rc<RefCell<SceneWorld>>` (and similar shared types — see `SharedSceneWorld`, `SharedPhysicsWorld`, etc. in `scripting.rs`) so mlua's `'static` closures can mutate engine state without `Arc<Mutex<>>` locking. Borrow discipline matters: always drop a `borrow_mut()` before dispatching events or calling user code that might re-enter.
- **In-place replacement for scene transitions:** `scene.load()` clears the ECS and re-initializes the shared `PhysicsWorld` via `borrow_mut()` so captured `Rc` handles remain valid across scene changes.
- **Pool manager:** Entity recycling for projectiles and spawned entities to avoid allocation churn.
- **Script limits:** `ScriptRuntime` loads only `math`, `string`, `table` and `utf8`, caps Lua memory, and counts VM instructions per call. Every call from the engine into Lua must go through `ScriptRuntime::run_limited`; a script that exceeds a limit is stopped and reported on the HUD.

## Tier Status

| Tier | Description | Status |
|------|-------------|--------|
| Tier 1 | Gameplay Primitives (health, damage, hitscan, projectiles, third-person camera) | **DONE v0.1.2** |
| Tier 2 | Production Foundations (dynamic instance buffer, entity lifecycle, pooling, particles, events) | **DONE v0.1.4** |
| Tier 2.5 | Physics & Scene API (impulse/force, velocity, CCD, collider materials, entity tags, camera shake, scene loading) | **DONE v0.1.7** |
| Tier 2.7 | DX & Code Quality (physics hot-reload, component patch coverage, pipeline modularization, HUD reload notifications, debug wireframes, kinematic bodies, convex decomposition) | **DONE v0.1.15** |
| Tier 3 | Visual & Interaction (texture mapping, procedural meshes, mouse picking, screen_to_ray) | **IN PROGRESS v0.1.18** |
| Tier 4 | GPU Scale (50K+ GPU compute entities, neighbor-grid collisions, flow field) | Planned |
| Tier 5 | Animation & Polish (skeletal animation, VAT, UI) | Planned |

## AI Asset Generation

Text-to-3D pipeline: prompt → FLUX.1 (2D) → Hunyuan3D (3D GLB) → engine.

### Environment Variables (`.env`)

| Variable | Purpose |
|----------|---------|
| `SLANG_DIR` | Path to vendored SLANG SDK (`vendor`) |
| `GATEWAY_URL` | Self-hosted GPU server URL (primary 3D gen backend) |
| `GATEWAY_KEY` | API key for GPU server |
| `HF_TOKEN` | HuggingFace API token (fallback 3D gen + 2D image gen) |
| `MODEL_SPACE` | HuggingFace Space for 3D generation |
| `MESHY_API_KEY` | Meshy AI API key (alternative 3D gen) |

**Never commit `.env`** — it contains secrets. Use `.env.example` as a template.

### MCP Servers (`.mcp.json`)

| Server | Tool | Purpose |
|--------|------|---------|
| `game-asset-generator` | `tools/game-asset-mcp/src/index.js` (canonical entry per upstream `package.json` main; vendored as a submodule) | Text→2D→3D asset pipeline |
| `meshy-ai` | `meshy-ai-mcp-server` (npx) | Meshy AI 3D generation |
| `blender` | `blender-mcp` (uvx) | Blender scene manipulation |

### Test Script

```sh
source .env
node scripts/test_generate_asset.js "red sports car"
# Outputs: project/assets/meshes/generated_2d.png, generated_3d.glb
```

## Editor Mode (`naive edit`)

AI-powered scene editor. Opens a 3D viewport with a free fly camera and a command socket. Claude Code is the AI brain — the user talks to Claude Code in the terminal, Claude Code sends commands to the running engine via MCP.

### Usage

```sh
naive edit                           # Opens editor with default scene (ground + light)
naive edit --scene scenes/my.yaml    # Opens editor with an existing scene
```

### Controls

- **WASD** — Move camera (forward / left / back / right)
- **Mouse** — Look around (always active — no button needed, works on Mac trackpads)
- **Space / Q** — Move up / down
- **Shift** — 3x speed boost
- **Scroll wheel** — Adjust movement speed

### MCP Tools for Scene Editing

When the editor is running, Claude Code can use these MCP tools via `naive_mcp`:

| Tool | Description |
|------|-------------|
| `naive_spawn_entity` | Spawn entity with mesh, lights, camera, and physics. Use `mesh_renderer` component with `procedural:cube`, `procedural:sphere`, or GLB paths. Add `rigid_body` + `collider` components for physics (dynamic bodies fall and collide). |
| `naive_destroy_entity` | Remove an entity by ID |
| `naive_modify_entity` | Modify transform, light properties on existing entities |
| `naive_list_entities` | List all entities with IDs and tags |
| `naive_query_entity` | Get detailed component data for an entity |
| `naive_run_lua` | Execute Lua code with full API access (entity, physics, particles, camera, events, audio). Use for batch operations, physics manipulation, particle effects. |
| `naive_emit_event` | Emit a gameplay event into the event bus (name + payload) |
| `naive_query_events` | Query recent events from the bus (for AI introspection) |
| `naive_inject_input` | Synthesize keyboard/mouse input as if the user pressed it |
| `naive_runtime_control` | Pause / resume / step the engine runtime |
| `naive_beautify_scene` | Trigger the scene beautification pipeline (mesh → Gaussian splat) |
| `naive_save_scene` | Serialize current scene to YAML file |
| `naive_get_scene_yaml` | Get current scene as YAML string (for understanding context) |
| `naive_set_camera` | Move/orient the editor camera (position, yaw, pitch, look_at) |
| `naive_editor_status` | Get editor mode info, entity count, camera position |

### Procedural Meshes

Available via `mesh_renderer.mesh`:
- `procedural:cube` — Unit cube
- `procedural:sphere` — UV sphere (32x32 segments)
- `procedural:plane` — XZ plane (1x1)
- `procedural:cylinder` — Y-axis cylinder (radius 0.5, height 1.0)
- `procedural:cone` — Y-axis cone (radius 0.5, height 1.0)
- `procedural:torus` — Torus/donut (major 0.3, minor 0.1)

All procedural meshes include UV coordinates for texture mapping.

### Procedural Materials

Available via `mesh_renderer.material`:
- `procedural:default` — Default gray material
- Or any YAML material file path (e.g., `assets/materials/red.yaml`)

### Example: Spawn 50 Falling Balls

Claude Code can execute a sequence of `naive_spawn_entity` calls to create entities in the running editor:

```json
{"cmd": "spawn_entity", "entity_id": "ball_1", "components": {
  "transform": {"position": [0, 20, 0], "scale": [0.5, 0.5, 0.5]},
  "mesh_renderer": {"mesh": "procedural:sphere", "material": "procedural:default"}
}}
```

### GDD-to-Game Workflow

When building games from a GDD (Game Design Document), Claude Code can:

1. **Read the GDD** to understand the game concept, entities, and mechanics
2. **Generate 3D assets** using `game-asset-mcp` (text → 2D image → 3D GLB via Hunyuan/Meshy)
3. **Build YAML scenes** defining entity layout, lights, cameras
4. **Write Lua scripts** for gameplay logic (physics, input, events)
5. **Use `naive edit`** to iteratively build and test the scene live
6. **Save the scene** with `naive_save_scene` for persistence
7. **Run the game** with `naive run` to test full gameplay

### Asset Generation Integration

Claude Code triggers 3D generation via `game-asset-mcp` (configured in `.mcp.json`):
- Routes to local GPU server (Hunyuan on H100) via `GATEWAY_URL`/`GATEWAY_KEY`
- Falls back to HuggingFace Spaces via `HF_TOKEN`
- Alternative: `meshy-ai` MCP for Meshy AI generation
- Generated GLB files saved to `project/assets/meshes/`, then spawned in editor

## Building

```sh
# Requires SLANG SDK in vendor/ (see homebrew formula or download from shader-slang/slang releases)
export SLANG_DIR=vendor
cargo build
cargo test
```

CI fails on any clippy warning. Before pushing, run what it runs:

```sh
cargo test --workspace --exclude naive-web --locked
cargo clippy --workspace --exclude naive-web --all-targets --locked -- -D warnings
cd project && cargo run --locked -p naive-runtime --bin naive -- test   # Lua scene tests
```

## Homebrew

Formula lives at `../homebrew-tap/Formula/naive.rb`. Update URL, SHA256, and version on each release.
