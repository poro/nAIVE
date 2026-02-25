# nAIVE Engine — Claude Code Context

## Project

AI-native game engine — create worlds with YAML, Lua, and natural language.

- **Language:** Rust (2021 edition)
- **Renderer:** wgpu + SLANG shaders
- **Physics:** Rapier3D
- **Scripting:** Lua 5.4 (mlua)
- **ECS:** hecs
- **Audio:** kira
- **Current version:** 0.1.10

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `naive-core` | Scene schema, shared types (YAML deserialization) |
| `naive-client` | Engine loop, renderer, physics, scripting, world management |
| `naive-runtime` | CLI binary (`naive`, `naive-runtime`, `naive_mcp`) |
| `naive-server` | Multiplayer server (future) |

## Key Source Files

| File | Responsibility |
|------|---------------|
| `crates/naive-core/src/scene.rs` | YAML scene schema types (`SceneDef`, `EntityDef`, `RigidBodyDef`, etc.) |
| `crates/naive-client/src/engine.rs` | Main `Engine` struct, game loop, camera, scene loading |
| `crates/naive-client/src/physics.rs` | `PhysicsWorld` — Rapier3D wrapper (bodies, colliders, CCD, impulse/force) |
| `crates/naive-client/src/world.rs` | `SceneWorld` (ECS + registry), `EntityCommandQueue`, entity spawning, pooling |
| `crates/naive-client/src/scripting.rs` | Lua API registration (physics, entity, camera, scene, events) |
| `crates/naive-client/src/renderer.rs` | wgpu render pipeline, instance buffers, particles |
| `crates/naive-client/src/test_runner.rs` | Headless test runner for `naive test` CLI command |
| `crates/naive-client/src/dev_log.rs` | `naive submit-log` — POST dev.log as GitHub Issue |
| `crates/naive-client/src/demos.rs` | `naive demo` — 15 embedded demos with interactive browser |
| `crates/naive-client/src/editor_camera.rs` | Free fly camera for `naive edit` editor mode |

## Architecture Patterns

- **Deferred commands:** Lua scripts push commands to `EntityCommandQueue`; engine processes them end-of-frame to avoid aliasing/dangling pointers.
- **Raw pointers for Lua closures:** Lua API closures capture `*mut PhysicsWorld` / `*mut SceneWorld` since mlua requires `'static` closures. Safe because Engine is pinned in the winit event loop.
- **In-place replacement for scene transitions:** `scene.load()` clears ECS and replaces physics world in-place (same memory address) so captured raw pointers remain valid.
- **Pool manager:** Entity recycling for projectiles and spawned entities to avoid allocation churn.

## Tier Status

| Tier | Description | Status |
|------|-------------|--------|
| Tier 1 | Gameplay Primitives (health, damage, hitscan, projectiles, third-person camera) | **DONE v0.1.2** |
| Tier 2 | Production Foundations (dynamic instance buffer, entity lifecycle, pooling, particles, events) | **DONE v0.1.4** |
| Tier 2.5 | Physics & Scene API (impulse/force, velocity, CCD, collider materials, entity tags, camera shake, scene loading) | **DONE v0.1.7** |
| Tier 3 | GPU Scale (50K+ GPU compute entities, neighbor-grid collisions, flow field) | Planned |
| Tier 4 | Animation & Polish (skeletal animation, VAT, UI) | Planned |

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
| `game-asset-generator` | `tools/game-asset-mcp/src/index.js` | Text→2D→3D asset pipeline |
| `meshy-ai` | `meshy-ai-mcp-server` (npx) | Meshy AI 3D generation |
| `blender` | `blender-mcp` (uvx) | Blender scene manipulation |

### Test Script

```sh
source .env
cd tools/game-asset-mcp
node test_generate.js "red sports car"
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

- **WASD** — Move camera (hold right-click for mouse look)
- **Right-click + mouse** — Look around
- **Space / Ctrl** — Move up / down
- **Shift** — 3x speed boost
- **Scroll wheel** — Adjust movement speed

### MCP Tools for Scene Editing

When the editor is running, Claude Code can use these MCP tools via `naive_mcp`:

| Tool | Description |
|------|-------------|
| `naive_spawn_entity` | Spawn entity with mesh, lights, camera. Use `mesh_renderer` component with `procedural:cube`, `procedural:sphere`, or GLB paths |
| `naive_destroy_entity` | Remove an entity by ID |
| `naive_modify_entity` | Modify transform, light properties on existing entities |
| `naive_list_entities` | List all entities with IDs and tags |
| `naive_query_entity` | Get detailed component data for an entity |
| `naive_save_scene` | Serialize current scene to YAML file |
| `naive_get_scene_yaml` | Get current scene as YAML string (for understanding context) |
| `naive_set_camera` | Move/orient the editor camera (position, yaw, pitch, look_at) |
| `naive_editor_status` | Get editor mode info, entity count, camera position |

### Procedural Meshes

Available via `mesh_renderer.mesh`:
- `procedural:cube` — Unit cube
- `procedural:sphere` — Unit sphere (32x32 segments)

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

## Homebrew

Formula lives at `../homebrew-tap/Formula/naive.rb`. Update URL, SHA256, and version on each release.
