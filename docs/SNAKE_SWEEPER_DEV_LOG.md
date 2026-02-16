# nAIVE Engine - Development Log

---

## Phase 16: UI Overlay, Text Rendering & Entity Commands

**Date:** 2026-02-13

### Summary

Added a complete immediate-mode 2D UI overlay system, bitmap font text rendering, runtime entity spawning/destruction from Lua, and visibility control. This phase enables HUDs, menus, score displays, and game-over screens -- everything needed to build a complete game UI without leaving YAML + Lua.

### New Systems

#### Bitmap Font Atlas (`src/font.rs`)
- Programmatically generated 6x8 pixel font atlas (same pattern as `audio_gen.rs`)
- 95 ASCII printable characters (32-126) arranged in a 16x6 grid = 96x48 RGBA texture
- Nearest-neighbor sampling for crisp pixel text at any scale
- `BitmapFont` struct with `create_bitmap_font()` and `glyph_uvs()` API

#### UI Overlay Renderer (`src/ui.rs`)
- Immediate-mode 2D rendering on top of the 3D scene
- Two wgpu render pipelines: colored rects (solid quads) and textured text (font atlas sampling)
- Orthographic projection in screen-space pixel coordinates
- Alpha blending (SrcAlpha / OneMinusSrcAlpha), no depth test
- Screen flash effect with auto-fade (fullscreen colored rect)
- Pre-allocated vertex/index buffers for 4096 quads per frame
- Embedded WGSL shaders (no SLANG dependency for UI)
- Renders with `LoadOp::Load` to preserve the 3D framebuffer

#### Entity Command Queue (`src/world.rs`)
- Deferred entity lifecycle commands from Lua scripts
- `spawn_runtime_entity()` - creates entity with Transform + MeshRenderer + EntityId at runtime
- `destroy_runtime_entity()` - despawns and removes from registry
- Scale updates and visibility toggling (insert/remove `Hidden` component)
- Commands processed after all Lua scripts, before transform updates

### Lua API Additions

#### Entity Commands (`entity.*`)
```lua
entity.spawn(id, mesh, material, x, y, z, sx, sy, sz)
entity.destroy(id)
entity.set_scale(id, sx, sy, sz)
entity.get_scale(id) -- returns sx, sy, sz
entity.set_visible(id, visible)
```

#### UI API (`ui.*`)
```lua
ui.text(x, y, text, size, r, g, b, a)
ui.rect(x, y, w, h, r, g, b, a)
ui.flash(r, g, b, a, duration)
ui.screen_width()  -- returns number
ui.screen_height() -- returns number
```

### New Component
- `Hidden` marker component in `components.rs` - entities with this component are skipped during rendering in both the forward renderer and the deferred pipeline

### Architecture Impact
- **Zero impact on 3D pipeline**: UI runs as a separate pass after the full deferred pipeline (shadow -> G-Buffer -> lighting -> bloom -> tonemap -> FXAA). Uses its own command encoder. Does not touch depth buffer.
- **Minimal overhead**: Only active when Lua scripts call `ui.*` functions. Pre-allocated GPU buffers avoid per-frame allocation.

### Modified Files
| File | Changes |
|------|---------|
| `src/main.rs` | Added `mod font;` and `mod ui;` declarations |
| `src/components.rs` | Added `Hidden` marker component |
| `src/renderer.rs` | Added `render_scene_to_view()`, Hidden entity filtering |
| `src/pipeline.rs` | Added `execute_pipeline_to_view()`, Hidden entity filtering |
| `src/engine.rs` | Font/UI init, entity command processing, UI overlay render pass, Lua API registration |
| `src/world.rs` | `EntityCommandQueue`, `SpawnCommand`, `spawn_runtime_entity()`, `destroy_runtime_entity()` |
| `src/scripting.rs` | `register_entity_command_api()`, `register_ui_api()` |

### New Files
| File | Purpose |
|------|---------|
| `src/font.rs` | Bitmap font atlas generator (~370 lines) |
| `src/ui.rs` | UI overlay renderer (~475 lines) |
| `project/scenes/ui_demo.yaml` | UI feature showcase scene |
| `project/logic/ui_demo.lua` | Full UI demo: HUD, text, colors, animations, spawning |
| `project/logic/ui_demo_camera.lua` | Orbit camera for UI demo |
| `project/scenes/grid_ui_demo.yaml` | Snake Sweeper prototype scene |
| `project/logic/grid_ui_demo.lua` | 7x7 grid with mines, numbers, auto-snake |
| `project/scenes/audio_test.yaml` | Audio playback test with testAssets MP3s |
| `project/logic/audio_test.lua` | Audio demo: music, SFX triggers, UI feedback |

### Demo Scenes

**UI Demo** (`scenes/ui_demo.yaml`):
```
cargo run --bin naive-runtime -- --scene scenes/ui_demo.yaml
```
Showcases: title bar, stats panel, text sizes, color palettes, animated text (pulse, rainbow), progress bar, auto-spawning entities, screen flash, visibility toggling.

**Grid UI Demo** (`scenes/grid_ui_demo.yaml`):
```
cargo run --bin naive-runtime -- --scene scenes/grid_ui_demo.yaml
```
Snake Sweeper prototype: 49 runtime-spawned tiles, Minesweeper numbers as UI text, dynamic tile emission, game state machine, auto-walking snake.

**Audio Test** (`scenes/audio_test.yaml`):
```
cargo run --bin naive-runtime -- --scene scenes/audio_test.yaml
```
Audio playback test: background music with fade-in, timed SFX triggers (eat, explosion, death), UI feedback for each sound event.

### Tests
- All 38 existing tests pass (`cargo test`)
- No panics or runtime errors in any demo scene

---

## Phase 15: Advanced Demos -- TITAN, INFERNO, VOID & FXAA

**Date:** 2026-02-12

### Summary
Added FXAA anti-aliasing pass, shadow mapping, and three flagship demo scenes pushing every engine system to the limit: TITAN (100+ entities, 32 lights), INFERNO (FPS combat arena), and VOID (cosmic Gaussian splatting).

---

## Phase 14: Rendering Overhaul -- Cook-Torrance GGX, FXAA, Shadow Mapping

**Date:** 2026-02-11

### Summary
Major rendering pipeline overhaul: replaced simple Blinn-Phong with physically-based Cook-Torrance GGX shading (D*F*G / 4*NdotL*NdotV), added directional shadow mapping with PCF soft shadows, bloom HDR glow extraction, and FXAA anti-aliasing.

---

## Phase 13: GENESIS Demo -- 65-Entity Cinematic Showcase

**Date:** 2026-02-10

### Summary
Created the GENESIS demo: a multi-act cinematic creation story with 65 entities, 24 dynamic lights, keyframe-driven camera choreography with Hermite interpolation, pillar rise animations, and Gaussian splat nebula.

---

## Phase 12: PBR Materials & Neon Dreamscape

### Summary
Added PBR material gallery (25 spheres, metallic x roughness sweep), neon dreamscape scene, and improved material system with Cook-Torrance GGX parameters.

---

## Phases 1-11: Foundation

Core engine built from scratch in Rust: window management (winit), scene system (YAML + hecs ECS), deferred rendering (wgpu), Gaussian splatting, Rapier 3D physics, Lua scripting (mlua), event bus, socket IPC, MCP server, headless testing, and audio (Kira).
