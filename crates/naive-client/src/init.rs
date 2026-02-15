//! `naive init` — scaffold a new nAIVE game project.

use std::fs;
use std::path::Path;

pub fn create_project(name: &str) -> Result<(), String> {
    let root = Path::new(name);

    if root.exists() {
        return Err(format!("Directory '{}' already exists", name));
    }

    println!("Creating nAIVE project: {}", name);

    // Create directory tree
    let dirs = [
        "",
        "scenes",
        "logic",
        "assets",
        "assets/meshes",
        "assets/materials",
        "assets/textures",
        "assets/audio",
        "shaders",
        "shaders/passes",
        "shaders/modules",
        "pipelines",
        "input",
        "events",
        "tests",
        "docs",
    ];

    for dir in &dirs {
        let path = root.join(dir);
        fs::create_dir_all(&path)
            .map_err(|e| format!("Failed to create {}: {}", path.display(), e))?;
    }

    // naive.yaml
    write_file(
        &root.join("naive.yaml"),
        &format!(
            r#"name: "{name}"
version: "0.1.0"
engine: "naive-runtime"
default_scene: "scenes/main.yaml"
default_pipeline: "pipelines/render.yaml"

test:
  directory: "tests"

build:
  targets:
    - "macos"
    - "windows"
    - "linux"
"#
        ),
    )?;

    // CLAUDE.md
    write_file(
        &root.join("CLAUDE.md"),
        &format!(
            r#"# {name} — nAIVE Game Project

This is a game built with the nAIVE engine. Games are NOT Rust projects — they are
YAML scenes + Lua scripts + assets, run by the `naive` binary.

## Project Structure
- `naive.yaml` — Project configuration (name, version, default scene/pipeline)
- `scenes/` — Scene definitions (YAML) — entities, components, world settings
- `logic/` — Game scripts (Lua) — attached to entities via the `script` component
- `assets/` — Meshes (.gltf, .glb, .ply), materials (.yaml), textures (.png, .jpg), audio (.ogg, .wav)
- `shaders/` — Custom SLANG shaders (passes/ and modules/)
- `pipelines/` — Render pipeline definitions (YAML)
- `input/` — Input binding configurations (YAML)
- `events/` — Event schema definitions (YAML)
- `tests/` — Automated Lua test scripts (headless, no GPU needed)
- `docs/` — PRD, game design documents, and project notes

## Commands
- `naive run` — Run the game (loads default_scene from naive.yaml)
- `naive run --scene scenes/other.yaml` — Run a specific scene
- `naive test` — Run all tests (discovers test_*.lua in tests/)
- `naive test tests/test_basic.lua` — Run a specific test file
- `naive build` — Bundle for standalone distribution
- `naive build --target windows` — Cross-platform build

## Development Workflow
- Edit YAML scenes to add/modify entities and components
- Write Lua scripts for game logic (attached via `script` component)
- The engine hot-reloads scenes, scripts, materials, and shaders on file save
- Use `naive test` to verify gameplay logic in headless mode
- Each entity has a unique string `id` in the scene YAML — this is how Lua references entities

## Scene Format (YAML)

Scenes define entities with components:

```yaml
name: "Scene Name"
settings:
  ambient_light: [0.3, 0.3, 0.35]   # RGB
  gravity: [0, -9.81, 0]             # physics gravity

entities:
  - id: my_entity                    # unique string ID
    components:
      transform:
        position: [x, y, z]
        rotation: [pitch, yaw, roll]  # degrees
        scale: [sx, sy, sz]
      mesh_renderer:
        mesh: primitive://cube        # or assets/meshes/file.gltf
        material: assets/materials/mat.yaml
      rigid_body:
        body_type: dynamic            # dynamic | fixed | kinematic
      collider:
        shape: cuboid                 # cuboid | sphere | capsule
        half_extents: [x, y, z]       # for cuboid
      point_light:
        color: [r, g, b]
        intensity: 15.0
        range: 100.0
      script:
        path: logic/my_script.lua
      player: {{}}                      # marks as FPS player
      character_controller:
        speed: 5.0
        jump_force: 8.0
```

Available components: `transform`, `camera`, `mesh_renderer`, `point_light`,
`directional_light`, `rigid_body`, `collider`, `character_controller`, `player`,
`script`, `gaussian_splat`, `tags`.

## Lua Scripting API — COMPLETE REFERENCE

### Script Lifecycle

Scripts are attached to entities. Each script runs in its own sandbox.
The variable `_entity_string_id` contains this entity's YAML id.
Use the `self` table for per-script persistent state.

```lua
function init()              -- called once on entity creation
function update(dt)          -- called every frame (dt = seconds)
function fixed_update(dt)    -- called at fixed physics timestep
function on_destroy()        -- called when entity is destroyed
function on_collision(other_entity_id)    -- physics collision
function on_trigger_enter(other_entity_id) -- trigger volume enter
function on_trigger_exit(other_entity_id)  -- trigger volume exit
function on_reload()         -- called after hot-reload
```

### Entity API — `entity.*`

All functions take an entity's string ID as the first argument:

```lua
-- Transform
local x, y, z = entity.get_position(id)
entity.set_position(id, x, y, z)
entity.set_rotation(id, pitch_deg, yaw_deg, roll_deg)
local sx, sy, sz = entity.get_scale(id)
entity.set_scale(id, sx, sy, sz)

-- Lighting
entity.set_light(id, intensity)
entity.set_light_color(id, r, g, b)

-- Material overrides (runtime only)
entity.set_emission(id, r, g, b)
entity.set_roughness(id, value)
entity.set_metallic(id, value)

-- Spawn new entity: entity.spawn(id, mesh, material, x, y, z, sx, sy, sz)
entity.spawn("bullet_1", "primitive://cube", "assets/materials/default.yaml", 0, 1, 0, 0.1, 0.1, 0.1)

-- Destroy entity
entity.destroy(id)

-- Visibility
entity.set_visible(id, true)  -- or false to hide
```

### Input API — `input.*`

Actions are defined in `input/bindings.yaml`.

```lua
input.pressed("action_name")       -- true while held down
input.just_pressed("action_name")  -- true only on the frame pressed
local mx, my = input.mouse_delta() -- mouse movement since last frame
```

### UI API — `ui.*`

Draw HUD elements (called every frame in update):

```lua
ui.text(x, y, "text string", font_size, r, g, b, a)
ui.rect(x, y, width, height, r, g, b, a)   -- filled rectangle
ui.flash(r, g, b, a, duration_seconds)      -- screen flash effect
local w = ui.screen_width()
local h = ui.screen_height()
```

### Audio API — `audio.*`

```lua
audio.play_sfx("sound_id", "assets/audio/file.ogg", volume)
audio.play_music("assets/audio/music.ogg", volume, fade_in_secs)
audio.stop_sound("sound_id", fade_out_secs)
audio.stop_music(fade_out_secs)
```

### Physics API — `physics.*`

```lua
-- Raycast returns: hit(bool), distance, normal_x, normal_y, normal_z
local hit, dist, nx, ny, nz = physics.raycast(ox, oy, oz, dx, dy, dz, max_dist)
```

### Events API — `events.*`

Event types are defined in `events/schema.yaml`.

```lua
events.emit("event.type", {{ key1 = "value1", key2 = "value2" }})
```

### Game State — `game` table

Shared across all scripts. Comes with defaults; add your own keys:

```lua
game.player_health   -- default: 100
game.game_over       -- default: false
game.level_complete  -- default: false
game.score = (game.score or 0) + 10  -- add custom keys
```

### Logging

```lua
log("message")         -- appears in engine output as [Lua] message
print("debug", value)  -- also logs to engine output
```

### Per-Script State — `self` table

Persists across frames and survives hot-reload:

```lua
function init()
    self.health = 100
    self.timer = 0
end
function update(dt)
    self.timer = self.timer + dt
end
```

## Test API (for tests/ scripts)

Tests run headless (no GPU). Each `test_*()` function gets a fresh runner.

```lua
scene.load("scenes/main.yaml")                -- load a scene
scene.find("entity_id")                       -- returns table or nil
  :get("transform")                           -- returns {{position={{x,y,z}}}}
wait_for_event("lifecycle.scene_loaded", 10)  -- wait with timeout
wait_seconds(1.0)                             -- advance game time
wait_frames(5)                                -- advance N frames
wait_until(function() return cond end, 10)    -- wait for condition
get_position("entity_id")                     -- returns x, y, z
get_game_value("key")                         -- read game table
event_occurred("event.type", {{key="val"}})    -- check event log
input.inject("action", "press", nil)          -- simulate input
input.inject("action", "release", nil)
input.inject("action", "axis", {{0, 1}})       -- axis: {{x, y}}
log.info("test message")
assert(condition, "error message")
```
"#
        ),
    )?;

    // .gitignore
    write_file(
        &root.join(".gitignore"),
        r#"dist/
*.log
.DS_Store
*.swp
*.swo
*~
"#,
    )?;

    // scenes/main.yaml — minimal scene
    write_file(
        &root.join("scenes/main.yaml"),
        &format!(
            r#"name: "{name} — Main Scene"
settings:
  ambient_light: [0.3, 0.3, 0.35]
  gravity: [0, -9.81, 0]

entities:
  - id: main_camera
    components:
      transform:
        position: [0, 3, 8]
        rotation: [-15, 0, 0]
      camera:
        fov: 75
        near: 0.1
        far: 500
        role: main

  - id: sun
    components:
      transform:
        position: [10, 20, -5]
      point_light:
        color: [1.0, 0.95, 0.9]
        intensity: 15.0
        range: 100.0

  - id: floor
    components:
      transform:
        position: [0, 0, 0]
        scale: [20, 0.1, 20]
      mesh_renderer:
        mesh: primitive://cube
        material: assets/materials/default.yaml
      rigid_body:
        body_type: fixed
      collider:
        shape: cuboid
        half_extents: [10, 0.05, 10]

  - id: welcome_cube
    components:
      transform:
        position: [0, 1, 0]
      mesh_renderer:
        mesh: primitive://cube
        material: assets/materials/default.yaml
      script:
        path: logic/main.lua
"#
        ),
    )?;

    // logic/main.lua — hello world
    write_file(
        &root.join("logic/main.lua"),
        r#"-- Main game logic script
-- Attached to welcome_cube in scenes/main.yaml

function init()
    self.angle = 0
    log("Hello from " .. _entity_string_id .. "!")
end

function update(dt)
    -- Rotate the cube slowly
    self.angle = self.angle + 30 * dt
    entity.set_rotation(_entity_string_id, 0, self.angle, 0)
end
"#,
    )?;

    // assets/materials/default.yaml
    write_file(
        &root.join("assets/materials/default.yaml"),
        r#"shader: shaders/passes/mesh_forward.slang
properties:
  base_color: [0.8, 0.8, 0.8]
  roughness: 0.5
  metallic: 0.0
  emission: [0, 0, 0]
blend_mode: opaque
cull_mode: back
"#,
    )?;

    // pipelines/render.yaml — default deferred pipeline
    write_file(
        &root.join("pipelines/render.yaml"),
        r#"version: 1
settings:
  vsync: true
  hdr: true

resources:
  - name: gbuffer_albedo
    type: texture_2d
    format: rgba8
    size: viewport
  - name: gbuffer_normal
    type: texture_2d
    format: rgba16f
    size: viewport
  - name: gbuffer_depth
    type: texture_2d
    format: depth32f
    size: viewport
  - name: gbuffer_emission
    type: texture_2d
    format: rgba16f
    size: viewport
  - name: hdr_buffer
    type: texture_2d
    format: rgba16f
    size: viewport
  - name: bloom_buffer
    type: texture_2d
    format: rgba16f
    size: viewport/2
  - name: ldr_buffer
    type: texture_2d
    format: rgba8
    size: viewport
  - name: shadow_map
    type: texture_2d
    format: depth32f
    size: "[2048, 2048]"

passes:
  - name: shadow_pass
    type: shadow
    shader: shaders/passes/shadow.slang
    inputs:
      scene_meshes: auto
    outputs:
      depth: shadow_map

  - name: geometry_pass
    type: rasterize
    shader: shaders/passes/gbuffer.slang
    inputs:
      scene_meshes: auto
      scene_materials: auto
    outputs:
      color: gbuffer_albedo
      emission: gbuffer_emission
      normal: gbuffer_normal
      depth: gbuffer_depth

  - name: lighting_pass
    type: fullscreen
    shader: shaders/passes/deferred_light.slang
    inputs:
      gbuffer_albedo: gbuffer_albedo
      gbuffer_normal: gbuffer_normal
      gbuffer_depth: gbuffer_depth
      gbuffer_emission: gbuffer_emission
      shadow_map: shadow_map
      scene_lights: auto
    outputs:
      color: hdr_buffer

  - name: bloom_pass
    type: fullscreen
    shader: shaders/passes/bloom.slang
    inputs:
      hdr: hdr_buffer
    outputs:
      color: bloom_buffer

  - name: tonemap_pass
    type: fullscreen
    shader: shaders/passes/tonemap.slang
    inputs:
      hdr: hdr_buffer
      bloom: bloom_buffer
    outputs:
      color: ldr_buffer

  - name: fxaa_pass
    type: fullscreen
    shader: shaders/passes/fxaa.slang
    inputs:
      ldr: ldr_buffer
    outputs:
      color: swapchain
"#,
    )?;

    // input/bindings.yaml
    write_file(
        &root.join("input/bindings.yaml"),
        r#"actions:
  move_forward:
    - W
  move_backward:
    - S
  move_left:
    - A
  move_right:
    - D
  jump:
    - Space
  interact:
    - E
  sprint:
    - ShiftLeft
  attack:
    - Left
"#,
    )?;

    // events/schema.yaml
    write_file(
        &root.join("events/schema.yaml"),
        r#"events:
  player.interacted:
    description: "Player pressed interact"
    fields: []
  item.collected:
    description: "Player collected an item"
    fields:
      - item_id
      - item_type
  game.level_complete:
    description: "The level was completed"
    fields: []
"#,
    )?;

    // docs/PRD.md — Product Requirements Document
    write_file(
        &root.join("docs/PRD.md"),
        &format!(
            r#"# {name} — Product Requirements Document

## 1. Overview

**Project:** {name}
**Engine:** nAIVE (AI-native game engine)
**Version:** 0.1.0

### Vision
<!-- What is this game? One paragraph that captures the core experience. -->

### Target Audience
<!-- Who is this game for? -->

## 2. Core Gameplay

### Game Loop
<!-- Describe the primary gameplay loop: what does the player do repeatedly? -->

### Win/Loss Conditions
<!-- How does the player win? How do they lose? -->

### Controls
<!-- List the key actions and what they do. These map to input/bindings.yaml. -->

| Action | Default Key | Description |
|--------|------------|-------------|
| move_forward | W | Move forward |
| move_backward | S | Move backward |
| move_left | A | Strafe left |
| move_right | D | Strafe right |
| jump | Space | Jump |
| interact | E | Interact with objects |

## 3. Scenes

### Scene List
<!-- List all planned scenes/levels. -->

| Scene | File | Description |
|-------|------|-------------|
| Main | `scenes/main.yaml` | Default starting scene |

## 4. Entities & Components

### Entity Catalog
<!-- List the key entities in the game. -->

| Entity | Components | Description |
|--------|-----------|-------------|
| main_camera | transform, camera | Player camera |
| sun | transform, point_light | Scene lighting |

## 5. Events

### Event Catalog
<!-- List game events. These map to events/schema.yaml. -->

| Event | Fields | Description |
|-------|--------|-------------|
| lifecycle.scene_loaded | — | Scene finished loading |

## 6. Art & Audio

### Visual Style
<!-- Describe the visual direction. -->

### Audio
<!-- List key sounds and music tracks needed. -->

## 7. Milestones

- [ ] **v0.1.0** — Prototype: basic scene loads and runs
- [ ] **v0.2.0** — Core mechanics implemented
- [ ] **v0.3.0** — Content complete
- [ ] **v1.0.0** — Release
"#
        ),
    )?;

    // docs/GDD.md — Game Design Document
    write_file(
        &root.join("docs/GDD.md"),
        &format!(
            r#"# {name} — Game Design Document

## 1. Concept

### Elevator Pitch
<!-- One or two sentences that sell the game. -->

### Genre & References
<!-- What genre? What existing games inspired this? -->

### Unique Selling Point
<!-- What makes this game different? -->

## 2. Mechanics

### Core Mechanics
<!-- Describe each mechanic in detail. For each mechanic:
     - What does it do?
     - How does the player interact with it?
     - What entities/components does it require?
     - What Lua scripts implement it? -->

### Progression
<!-- How does difficulty or complexity increase over time? -->

### Economy / Resources
<!-- If applicable: what resources exist, how are they earned/spent? -->

## 3. World Design

### Setting
<!-- Where and when does the game take place? -->

### Level Design Principles
<!-- What makes a good level in this game? -->

### Level List
<!-- Detailed breakdown of each level/scene. -->

| Level | Scene File | Description | Key Entities |
|-------|-----------|-------------|--------------|
| 1 | `scenes/main.yaml` | Starting area | camera, sun, floor |

## 4. Narrative

### Story Summary
<!-- If applicable: what's the story? -->

### Characters
<!-- Key characters and their roles. -->

## 5. Visual Design

### Art Direction
<!-- Color palette, style (realistic, stylized, pixel), mood. -->

### Materials
<!-- Key materials and their properties. -->

| Material | File | Description |
|----------|------|-------------|
| Default | `assets/materials/default.yaml` | Base PBR material |

## 6. Audio Design

### Music
<!-- Tracks needed, mood, when they play. -->

### Sound Effects
<!-- Key sounds and when they trigger. -->

## 7. UI/UX

### HUD Elements
<!-- What's always on screen? (health, score, minimap, etc.) -->

### Menus
<!-- Main menu, pause menu, settings, etc. -->

## 8. Technical Notes

### Performance Targets
<!-- Target FPS, supported platforms, min specs. -->

### Known Constraints
<!-- Engine limitations or design constraints to be aware of. -->
"#
        ),
    )?;

    // tests/test_basic.lua
    write_file(
        &root.join("tests/test_basic.lua"),
        r#"-- tests/test_basic.lua
-- Basic automated tests for the game project.

function test_scene_loads()
    scene.load("scenes/main.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local camera = scene.find("main_camera")
    assert(camera ~= nil, "Main camera must exist")

    local floor = scene.find("floor")
    assert(floor ~= nil, "Floor entity must exist")

    log.info("Scene loads test passed!")
end

function test_welcome_cube_exists()
    scene.load("scenes/main.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local cube = scene.find("welcome_cube")
    assert(cube ~= nil, "Welcome cube must exist")

    -- Wait a moment for scripts to init
    wait_frames(2)

    log.info("Welcome cube test passed!")
end
"#,
    )?;

    println!();
    println!("  Project created at ./{}/", name);
    println!();
    println!("  Get started:");
    println!("    cd {}", name);
    println!("    naive run");
    println!();
    println!("  Project structure:");
    println!("    naive.yaml        Project configuration");
    println!("    scenes/main.yaml  Default scene");
    println!("    logic/main.lua    Game logic scripts");
    println!("    assets/           Meshes, materials, textures");
    println!("    pipelines/        Render pipeline definitions");
    println!("    input/            Input bindings");
    println!("    events/           Event schemas");
    println!("    tests/            Automated test scripts");
    println!("    docs/             PRD, game design docs");

    Ok(())
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}
