# nAIVE Game Development Guide

## 1. Overview

nAIVE is an AI-native game engine where **games are content, not code**. The engine provides the runtime (rendering, physics, scripting, audio, networking) and your game is a collection of YAML scenes, Lua scripts, and assets.

Key principles:
- **Games live in their own repositories**, separate from the engine
- **YAML-first**: scenes, materials, pipelines, input bindings, and events are all declared in YAML
- **Lua scripting**: game logic is written in Lua and hot-reloaded on save
- **AI-assisted**: every project includes a `CLAUDE.md` so AI agents understand your game's structure
- **Test-driven**: automated headless tests verify gameplay logic without a GPU

## 2. Quick Start

```bash
# Create a new game project
naive init my-game

# Enter the project directory
cd my-game

# Run the game
naive run

# Run all tests
naive test

# Bundle for distribution
naive build
```

### Prerequisites

You need the `naive` binary (or `naive-runtime`) on your PATH. Build from source:

```bash
git clone https://github.com/anthropics/naive.git
cd naive
cargo build --release
# Add target/release/ to your PATH
```

## 3. Project Structure

After `naive init my-game`, you get:

```
my-game/
├── naive.yaml              # Project configuration
├── CLAUDE.md               # AI agent instructions
├── .gitignore              # Git ignore rules
├── scenes/
│   └── main.yaml           # Default scene
├── logic/
│   └── main.lua            # Game logic scripts
├── assets/
│   ├── meshes/             # 3D models (.gltf)
│   ├── materials/          # Material definitions (.yaml)
│   │   └── default.yaml    # Default PBR material
│   ├── textures/           # Texture images (.png, .jpg)
│   └── audio/              # Sound files (.ogg, .wav)
├── shaders/
│   ├── passes/             # Render pass shaders (.slang)
│   └── modules/            # Shared shader modules (.slang)
├── pipelines/
│   └── render.yaml         # Render pipeline definition
├── input/
│   └── bindings.yaml       # Input action mappings
├── events/
│   └── schema.yaml         # Event type definitions
└── tests/
    └── test_basic.lua      # Automated gameplay tests
```

### Directory Reference

| Directory | Purpose | File Types |
|-----------|---------|------------|
| `scenes/` | Scene definitions with entities and components | `.yaml` |
| `logic/` | Lua game scripts attached to entities | `.lua` |
| `assets/meshes/` | 3D models | `.gltf`, `.glb`, `.ply` |
| `assets/materials/` | PBR material definitions | `.yaml` |
| `assets/textures/` | Texture images | `.png`, `.jpg`, `.hdr` |
| `assets/audio/` | Sound effects and music | `.ogg`, `.wav` |
| `shaders/passes/` | Render pass shaders | `.slang` |
| `shaders/modules/` | Shared shader code | `.slang` |
| `pipelines/` | Render pipeline graphs | `.yaml` |
| `input/` | Input binding configs | `.yaml` |
| `events/` | Game event schemas | `.yaml` |
| `tests/` | Automated test scripts | `.lua` |

## 4. naive.yaml Reference

The project configuration file at the root of every nAIVE game:

```yaml
# Required
name: "My Game"                      # Project name
version: "0.1.0"                     # Semantic version

# Optional
engine: "naive-runtime"              # Engine binary name (default)
default_scene: "scenes/main.yaml"    # Scene loaded by `naive run`
default_pipeline: "pipelines/render.yaml"  # Render pipeline

# Test configuration
test:
  directory: "tests"                 # Directory to scan for test_*.lua files
  # OR explicit file list:
  # files:
  #   - "tests/test_basic.lua"
  #   - "tests/test_combat.lua"

# Build configuration
build:
  targets:                           # Platforms to build for
    - "macos"
    - "windows"
    - "linux"
```

## 5. Development Workflow

### Edit-Save-See Loop

nAIVE hot-reloads on file save. The typical workflow is:

1. Edit a scene YAML, Lua script, material, or shader
2. Save the file
3. See changes instantly in the running engine window

No restart required for:
- Scene changes (entities, components, settings)
- Lua scripts (logic, callbacks)
- Materials (colors, properties)
- Shaders (SLANG source)

### Running Specific Scenes

```bash
# Run the default scene (from naive.yaml)
naive run

# Run a specific scene
naive run --scene scenes/level_02.yaml
```

### Legacy Mode

If you're not using `naive.yaml`, the original flags still work:

```bash
naive-runtime --project path/to/project --scene scenes/my_scene.yaml
naive-runtime --project path/to/project --pipeline pipelines/custom.yaml
```

## 6. Scenes

Scenes are YAML files that define entities, their components, and world settings.

### Scene Format

```yaml
name: "My Scene"
settings:
  ambient_light: [0.3, 0.3, 0.35]   # RGB ambient light color
  gravity: [0, -9.81, 0]             # Physics gravity vector

entities:
  - id: main_camera                  # Unique entity identifier
    components:
      transform:
        position: [0, 3, 8]
        rotation: [-15, 0, 0]        # Euler angles in degrees
        scale: [1, 1, 1]             # Default if omitted
      camera:
        fov: 75                      # Field of view in degrees
        near: 0.1
        far: 500
        role: main                   # main | editor

  - id: player
    components:
      transform:
        position: [0, 1, 0]
      mesh_renderer:
        mesh: assets/meshes/character.gltf
        material: assets/materials/player.yaml
      rigid_body:
        body_type: dynamic           # dynamic | fixed | kinematic
        mass: 70.0
      collider:
        shape: capsule               # cuboid | sphere | capsule
        radius: 0.4
        half_height: 0.8
      character_controller:
        speed: 5.0
        jump_force: 8.0
      player: {}
      script:
        path: logic/player.lua

  - id: sun
    components:
      transform:
        position: [10, 20, -5]
      point_light:
        color: [1.0, 0.95, 0.9]
        intensity: 15.0
        range: 100.0
```

### Available Components

| Component | Purpose |
|-----------|---------|
| `transform` | Position, rotation, scale in 3D space |
| `camera` | Camera with FOV, near/far planes, role |
| `mesh_renderer` | 3D mesh with material |
| `point_light` | Point light source with color, intensity, range |
| `rigid_body` | Physics rigid body (dynamic, fixed, kinematic) |
| `collider` | Physics collision shape |
| `character_controller` | FPS-style character movement |
| `player` | Marks entity as the player |
| `script` | Attaches a Lua script |
| `gaussian_splat` | 3D Gaussian splat point cloud |

## 7. Scripting

Game logic is written in Lua and attached to entities via the `script` component.

### Script Lifecycle

```lua
function on_init(entity)
    -- Called once when the entity is created
    log.info("Entity initialized: " .. entity.name)
end

function on_update(entity, dt)
    -- Called every frame, dt = delta time in seconds
end

function on_fixed_update(entity, dt)
    -- Called at fixed physics timestep
end

function on_event(entity, event_type, event_data)
    -- Called when a subscribed event fires
end

function on_trigger_enter(entity, other_id)
    -- Called when another entity enters this trigger
end

function on_trigger_exit(entity, other_id)
    -- Called when another entity exits this trigger
end
```

### Entity API

```lua
-- Transform
local x, y, z = entity.get_position()
entity.set_position(x, y, z)

local rx, ry, rz = entity.get_rotation()
entity.set_rotation(rx, ry, rz)

local sx, sy, sz = entity.get_scale()
entity.set_scale(sx, sy, sz)
```

### Scene API

```lua
-- Find entities
local player = scene.find("player")

-- Load a scene
scene.load("scenes/level_02.yaml")
```

### Input API

```lua
-- Check input actions (defined in input/bindings.yaml)
if input.pressed("jump") then
    -- Jump logic
end

if input.held("sprint") then
    -- Sprint logic
end

local mx, my = input.mouse_delta()
```

### UI API

```lua
-- Display text on screen
ui.text("Score: " .. score, 10, 10)
ui.text_colored("DANGER", 1.0, 0.0, 0.0, 100, 50)

-- Panels
ui.panel(x, y, width, height, r, g, b, a)
```

### Audio API

```lua
-- Play sounds
audio.play("assets/audio/explosion.ogg")
audio.play_at("assets/audio/footstep.ogg", x, y, z)

-- Generate tones
audio.generate("sine", frequency, duration, volume)
```

### Physics API

```lua
-- Apply forces
physics.apply_force(entity, fx, fy, fz)
physics.apply_impulse(entity, ix, iy, iz)

-- Raycasting
local hit = physics.raycast(origin_x, origin_y, origin_z, dir_x, dir_y, dir_z, max_dist)
```

### Events API

```lua
-- Emit events
events.emit("item.collected", { item_id = "key_01", item_type = "key" })

-- Subscribe to events
events.on("player.damaged", function(data)
    log.info("Took " .. data.amount .. " damage!")
end)
```

### Game State

```lua
-- Shared game state (persists across scripts)
set_game_value("player_health", 100)
local health = get_game_value("player_health")
```

### Logging

```lua
log.info("Information message")
log.warn("Warning message")
log.error("Error message")
```

## 8. Testing

nAIVE includes a headless test runner that executes Lua test scripts without a GPU or window.

### Running Tests

```bash
# Run all test files (discovers test_*.lua in tests/ directory)
naive test

# Run a specific test file
naive test tests/test_combat.lua
```

### Writing Tests

Test files are Lua scripts where every `function test_*()` is automatically discovered and run:

```lua
-- tests/test_player.lua

function test_player_spawns()
    scene.load("scenes/main.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local player = scene.find("player")
    assert(player ~= nil, "Player must exist in scene")

    local x, y, z = get_position("player")
    assert(y > 0, "Player should spawn above ground")
end

function test_player_can_move()
    scene.load("scenes/main.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local sx, sy, sz = get_position("player")

    -- Inject input: walk forward for 1 second
    input.inject("move", "axis", {0, 1})
    wait_seconds(1.0)
    input.inject("move", "axis", {0, 0})

    local ex, ey, ez = get_position("player")
    assert(ez < sz, "Player should have moved forward (negative Z)")
end
```

### Test API

| Function | Description |
|----------|-------------|
| `scene.load(path)` | Load a scene |
| `wait_for_event(type)` | Block until an event fires |
| `wait_seconds(n)` | Simulate n seconds of game time |
| `wait_frames(n)` | Simulate n frames |
| `get_position(entity_id)` | Get entity position (x, y, z) |
| `get_game_value(key)` | Read shared game state |
| `input.inject(action, type, value)` | Simulate player input |
| `assert(condition, message)` | Assert a condition is true |
| `log.info(message)` | Log information |

### Test Configuration

In `naive.yaml`, configure which tests to run:

```yaml
test:
  # Scan a directory for test_*.lua files (default)
  directory: "tests"

  # OR list specific files
  files:
    - "tests/test_player.lua"
    - "tests/test_combat.lua"
    - "tests/test_inventory.lua"
```

## 9. Building

Bundle your game into a standalone distributable:

```bash
# Build for current platform
naive build

# Build for a specific target
naive build --target windows
```

This creates a `dist/` directory with:

```
dist/my-game-macos/
├── naive-runtime          # Engine binary
├── naive.yaml             # Project config
├── launch.sh              # Launcher script (or .bat on Windows)
├── scenes/                # All game content
├── logic/
├── assets/
├── shaders/
├── pipelines/
├── input/
└── events/
```

Players run the game with `./launch.sh` (or `launch.bat` on Windows).

## 10. Publishing

> **Coming Soon**: The nAIVE World Server will allow publishing games to a global network where players connect directly without downloads.

```bash
naive publish
```

When available, your game will be accessible at a unique four-word address like `bright.crystal.forest.realm`. The nAIVE runtime will stream your world to any connected device.

## 11. AI-Assisted Development

Every nAIVE project includes a `CLAUDE.md` file that helps AI coding agents understand your game. This file describes:

- Project structure and directory layout
- Available CLI commands
- Development conventions

When working with an AI agent, it reads `CLAUDE.md` to understand how to:
- Add new entities to scenes
- Write and attach Lua scripts
- Create materials and configure rendering
- Write and run tests

### Tips for AI-Assisted Development

1. **Describe what you want in natural language**: "Add a treasure chest entity that the player can open by pressing E"
2. **The AI reads your existing scenes and scripts** to match your project's style
3. **Test-driven**: Ask the AI to write tests first, then implement the feature
4. **Iterate**: Run `naive test` after each AI-generated change to verify correctness

## 12. Examples

The engine repository includes several demo scenes that demonstrate various features:

| Scene | Features |
|-------|----------|
| `scenes/relic.yaml` | Full dungeon game with player, enemies, items, doors |
| `scenes/genesis.yaml` | PBR materials, bloom, emissive surfaces, camera orbits |
| `scenes/neon_dreamscape.yaml` | Emissive neon materials, pulsing animations |
| `scenes/physics_test.yaml` | Rigid body physics, collisions, gravity |
| `scenes/ui_demo.yaml` | UI overlay, text rendering, panels |
| `scenes/audio_test.yaml` | Spatial audio, generated tones |
| `scenes/pbr_gallery.yaml` | Metallic/roughness material grid |

To run an engine demo:

```bash
cd /path/to/naive-engine
naive-runtime --project project --scene scenes/genesis.yaml
```

To use a demo as a starting point for your game, copy the relevant scene and script files into your project directory.
