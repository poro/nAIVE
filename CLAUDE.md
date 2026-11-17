# nAIVE Engine — Claude Code Context

## Project

AI-native game engine — create worlds with YAML, Lua, and natural language.

- **Language:** Rust (2021 edition)
- **Renderer:** wgpu + SLANG shaders
- **Physics:** Rapier3D
- **Scripting:** Lua 5.4 (mlua)
- **ECS:** hecs
- **Audio:** kira
- **Current version:** 0.1.22

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `naive-core` | Scene schema, shared types, typed command requests/responses and MCP tool schemas |
| `naive-sim` | CPU physics, input, project paths, validated asset decoding, particles, animation playback helpers and presentation contracts (camera/viewport/UI intents). Must never depend on wgpu/winit/kira; CI checks. Native world/script adapters remain in `naive-client`. Plan: `docs/sim-split-plan.md` |
| `naive-client` | Engine loop, renderer, scripting, world management |
| `naive-runtime` | CLI binary (`naive`, `naive-runtime`, `naive_mcp`) |
| `naive-player` | `naive` built without Slang; `naive build` ships it in game bundles. Build it alone (`cargo build -p naive-player`) so feature unification cannot link Slang into it |
| `naive-server` | Multiplayer server (stub — future) |
| `naive-web` | Minimal WASM + WebGPU clear-canvas prototype (`crates/naive-web`, `cdylib`); not the native game runtime |

## Key Source Files

| File | Responsibility |
|------|---------------|
| `crates/naive-core/src/scene.rs` | YAML scene schema types (`SceneDef`, `EntityDef`, `RigidBodyDef`, etc.) |
| `crates/naive-core/src/command.rs` | Typed command wire protocol, response envelope and shared MCP tool definitions |
| `crates/naive-client/src/engine.rs` | Main `Engine` struct, game loop (`run_frame` → `tick_simulation`, `prepare_render`, `render_frame`), camera, scene loading |
| `crates/naive-client/src/engine/commands.rs` | Editor/MCP dispatch, entity creation, persistence, Lua evaluation, frame capture and benchmarking |
| `crates/naive-client/src/sim.rs` | GPU-free frame steps shared by the engine and the headless test runner (character controller, fixed physics steps, collision/event dispatch, collision damage, projectiles, health, entity destruction, script updates). Collect what a script callback needs first, release every borrow, then call the script. |
| `crates/naive-sim/src/physics.rs` | `PhysicsWorld` — Rapier3D wrapper (bodies, colliders, CCD, impulse/force) |
| `crates/naive-client/src/world.rs` | `SceneWorld` (ECS + registry), `EntityCommandQueue`, entity spawning, pooling |
| `crates/naive-client/src/world/construction.rs` | Shared asset resolver/preflight and the sole CPU entity constructor, including physics; no device or GPU cache inputs |
| `crates/naive-client/src/world/native_assets.rs` | Native GPU cache adapter for the shared asset resolver |
| `crates/naive-client/src/world/spawning.rs` | Native/headless compatibility wrappers and shared projectile/dynamic recipes and finishers |
| `crates/naive-client/src/scene_snapshot.rs` | Scene-authoring snapshots from retained entity definitions and supported live ECS values; rejects unrepresentable runtime edits |
| `crates/naive-client/src/scripting.rs` | Sandboxed Lua runtime, CPU/memory limits, shared API handles, script loading, hooks and hot reload |
| `crates/naive-client/src/scripting/` | Lua API registration split by domain (`base`, `input`, `physics`, `entity`, `events`, `audio`, `lifecycle`, `mesh`, `camera`, `particles`, `ui`, `animation`); regression tests in `tests.rs` |
| `crates/naive-client/src/renderer.rs` | wgpu render pipeline, instance buffers, particles |
| `crates/naive-client/src/pipeline/` | CompiledPipeline — YAML render-pass DAG (split into `def`/`resource`/`compiler`/`executor`/`mod`) |
| `crates/naive-client/src/swarm.rs` | Shared swarm layouts, region types, target resolution, step planning and report processing; public API re-exports preserve existing callers |
| `crates/naive-client/src/swarm/` | `flow.rs`: obstacles and pathfinding; `cpu.rs`: reference simulation and headless backend; `gpu.rs`: pipelines, buffers, instanced draws and async readback; `compute.wgsl`: compute shader; `tests.rs`: CPU/GPU regression tests. Plan: `docs/tier4-plan.md` |
| `crates/naive-client/src/gpu_timer.rs` | GPU time per render and compute pass from timestamp queries, read back asynchronously; behind the `gpu_timings` and `benchmark` commands and the render debug overlay |
| `crates/naive-client/src/swarm_api.rs` | The `swarm` Lua table (spawn, damage spheres and lines, targets, counts) and the `SwarmBus` carrying requests to swarms and reports back; `sim::process_swarm_reports` turns reports into target damage and `swarm.kills` / `swarm.hit` / `swarm.cleared` events |
| `crates/naive-client/src/debug_draw.rs` | Wireframe physics collider visualization (toggle: H key) |
| `crates/naive-client/src/command.rs` | Command socket — newline-delimited JSON over a Unix domain socket (default `/tmp/naive-runtime.sock`); `naive_mcp` bridges it to MCP |
| `crates/naive-sim/src/project_path.rs` | Project-relative path confinement, symlink validation and glTF external-resource validation |
| `crates/naive-client/src/test_runner.rs` | Headless test runner for `naive test` CLI command |
| `crates/naive-client/src/test_execution.rs` | Shared CLI/editor test subprocess protocol, hard deadlines, bounded output and background admission |
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
- **Project paths:** Resolve scene, script, asset and output paths with `project_path::resolve` (or `resolve_with_extension`) before filesystem access. Do not replace this with `project_root.join`: validation rejects absolute paths, parent traversal and symlinks that escape the project. glTF imports must also validate external resource URIs.
- **Module boundaries:** Keep Lua API registration in the corresponding `scripting/` domain module, swarm simulation in `swarm/cpu.rs`, pathfinding in `swarm/flow.rs`, GPU resources in `swarm/gpu.rs`, and editor commands in `engine/commands.rs`. Shared command schemas belong in `naive-core/src/command.rs` so the engine and MCP bridge use the same definitions.

## Tier Status

| Tier | Description | Status |
|------|-------------|--------|
| Tier 1 | Gameplay Primitives (health, damage, hitscan, projectiles, third-person camera) | **DONE v0.1.2** |
| Tier 2 | Production Foundations (dynamic instance buffer, entity lifecycle, pooling, particles, events) | **DONE v0.1.4** |
| Tier 2.5 | Physics & Scene API (impulse/force, velocity, CCD, collider materials, entity tags, camera shake, scene loading) | **DONE v0.1.7** |
| Tier 2.7 | DX & Code Quality (physics hot-reload, component patch coverage, pipeline modularization, HUD reload notifications, debug wireframes, kinematic bodies, convex decomposition) | **DONE v0.1.15** |
| Tier 3 | Visual & Interaction (texture mapping, procedural meshes, mouse picking, screen_to_ray) | **IN PROGRESS v0.1.18** |
| Tier 4 | GPU Scale (50K+ GPU compute entities, neighbor-grid collisions, flow field) | **DONE** (v0.1.21) — instanced drawing, `gpu_swarm` movement, separation, flow-field pathing, damage, death, spawn waves and the Lua swarm API; 50,000 goblins in a fight run at about 1,000 fps on an M4 Max (`docs/tier4-plan.md`; prototype `scenes/tier4_havoc.yaml`) |
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
| `naive_capture_frame` | Render the current frame to a PNG inside the project (default `captures/frame.png`) so the agent can look at the scene. `include_ui: true` also draws the HUD as the last frame drew it (while paused, scripts draw no HUD). |
| `naive_benchmark` | Run N whole frames back to back offscreen (no vsync, no window throttling) and report fps, ms per frame and GPU time per pass. The simulation advances. Use this, not the on-screen frame rate, to measure performance |
| `naive_run_tests` | Run Lua tests in a supervised subprocess (one file or all from `naive.yaml`); `timeout_seconds` is 1–300, default 30. One suite at a time; the editor stays responsive and `editor_status.tests_running` reports activity |
| `naive_screenshot_diff` | Compare two PNG captures in the project: changed pixels, max and mean difference, and a diff image (changes in red) |
| `naive_undo` | Undo the last `spawn_entity`, `destroy_entity`, `modify_entity` or `run_lua` by restoring the scene snapshot taken before it (32 steps; scripts restart) |
| `naive_gpu_timings` | GPU ms per frame over the last 60 frames, per pass. On screen with vsync the GPU clocks down to just fit the refresh, so these read high |

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
# Requires the SLANG SDK unpacked in vendor/ (see the README). .cargo/config.toml
# already sets SLANG_DIR; do not export SLANG_DIR=vendor yourself (a relative
# value breaks the shader-slang-sys build).
cargo build
cargo test --workspace --exclude naive-web
```

CI fails on any clippy warning. Before pushing, run what it runs:

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude naive-web --locked
cargo clippy --workspace --exclude naive-web --all-targets --locked -- -D warnings
cd project && cargo run --locked -p naive-runtime --bin naive -- test   # Lua scene tests
```

For rendering changes, also compare `capture_frame` PNGs from master and the
branch (byte-identical for a static scene, or look at them) and say which in
the pull request.

## Pull requests

- Every change goes through a pull request; nothing is pushed straight to
  `master`, version bumps included.
- A pull request that changes more than about 500 lines of code (not counting
  the lockfile, formatting or moved files) gets a review before it merges: a
  person, or at least `/code-review` with its findings answered in the PR.
- The description says how the change was verified (tests, captures,
  benchmarks) and what was not checked.

## Releases

1. `scripts/bump-version.sh X.Y.Z`, then a pull request with the result.
2. When it merges, `.github/workflows/release.yml` sees the new version on master,
   tags the merge commit `vX.Y.Z` and releases it (pushing such a tag by hand
   releases too). It builds `naive` and `naive-player`, bundles the
   demo project as an app (notarized when the Developer ID secrets are set),
   attaches `nAIVE-X.Y.Z-source.tar.gz` (the source with the Git LFS files, which
   GitHub's own archives may lack), publishes the release and prints that
   tarball's `url` and `sha256` for Homebrew.
4. Update `../homebrew-tap/Formula/naive.rb` with them; the formula builds from
   that release asset, not from GitHub's archive. The formula must also
   build `crates/naive-player` (`cargo install` it alongside
   `crates/naive-runtime`) so `naive build` can bundle it.
