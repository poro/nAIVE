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

## Project Structure
- `naive.yaml` — Project configuration
- `scenes/` — Scene definitions (YAML)
- `logic/` — Game scripts (Lua)
- `assets/` — Meshes, materials, textures, audio
- `shaders/` — Custom SLANG shaders
- `pipelines/` — Render pipeline definitions
- `input/` — Input binding configurations
- `events/` — Event schema definitions
- `tests/` — Automated Lua test scripts
- `docs/` — PRD, game design documents, and project notes

## Commands
- `naive run` — Run the game
- `naive run --scene scenes/other.yaml` — Run a specific scene
- `naive test` — Run all tests
- `naive test tests/test_basic.lua` — Run a specific test
- `naive build` — Bundle for distribution

## Development
- Edit YAML scenes to add/modify entities and components
- Write Lua scripts for game logic (attached via `script` component)
- The engine hot-reloads scenes, scripts, materials, and shaders on save
- Use `naive test` to verify gameplay logic in headless mode
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
