# nAIVE Game Development Guide

## 1. Overview

nAIVE is an AI-native game engine where **games are content, not code**. The engine provides the runtime (rendering, physics, scripting, audio) and your game is a collection of YAML scenes, Lua scripts, and assets.

Key principles:
- **Games live in their own repositories**, separate from the engine
- **YAML-first**: scenes, materials, pipelines, input bindings, and events are all declared in YAML
- **Lua scripting**: game logic is written in Lua and hot-reloaded on save
- **AI-assisted**: every project includes a `CLAUDE.md` so AI agents understand your game's structure
- **Test-driven**: automated headless tests verify gameplay logic without a GPU

## 2. Quick Start

### Install

```bash
brew install poro/tap/naive
```

Or build from source:

```bash
git clone https://github.com/poro/nAIVE.git
cd nAIVE
cargo build --release
# Add target/release/ to your PATH
```

### Create and Run a Project

```bash
naive init my-game
cd my-game
naive run
```

### Other Commands

```bash
naive test              # Run all tests
naive test tests/t.lua  # Run a specific test file
naive build             # Bundle for distribution
naive publish           # Publish to world server (coming soon)
```

## 3. Project Structure

After `naive init my-game`, you get:

```
my-game/
├── naive.yaml              # Project configuration
├── CLAUDE.md               # AI agent instructions
├── .gitignore
├── scenes/
│   └── main.yaml           # Default scene
├── logic/
│   └── main.lua            # Game logic scripts
├── assets/
│   ├── meshes/             # 3D models (.gltf, .glb, .ply)
│   ├── materials/
│   │   └── default.yaml    # Default PBR material
│   ├── textures/           # Texture images (.png, .jpg, .hdr)
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
├── tests/
│   └── test_basic.lua      # Automated gameplay tests
└── docs/
    ├── PRD.md              # Product Requirements Document
    └── GDD.md              # Game Design Document
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
| `docs/` | PRD, game design docs, project notes | `.md` |

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
version: 1                           # Scene format version (written by saves)
settings:
  ambient_light: [0.3, 0.3, 0.35]   # RGB ambient light color
  gravity: [0, -9.81, 0]             # Physics gravity vector
  sky:                               # Optional: sky background and the light it casts
    zenith: [0.16, 0.32, 0.62]       # Gradient sky colours (defaults shown)
    horizon: [0.72, 0.78, 0.86]
    ground: [0.24, 0.22, 0.2]
    intensity: 1.0                   # Brightness of the sky and its light
    # texture: assets/sky/meadow.jpg # Or an equirectangular (2:1) image

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
        role: main                   # main | <custom name>

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
        source: logic/player.lua

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
| `mesh_renderer` | 3D mesh with material reference |
| `point_light` | Point light source with color, intensity, range |
| `directional_light` | Sun-like directional light. Shadows reach `2 × shadow_extent` (default 20) from the camera, in four cascades that are sharpest near the camera |
| `rigid_body` | Physics rigid body (dynamic, fixed, kinematic) |
| `collider` | Physics collision shape (cuboid, sphere, capsule) |
| `character_controller` | FPS-style character movement (speed, jump) |
| `player` | Marks entity as the player (enables FPS controller) |
| `script` | Attaches a Lua script file |
| `gaussian_splat` | 3D Gaussian splat point cloud |
| `tags` | Searchable string tags for entity lookup |
| `health` | Health pool with max/current values for damageable entities |
| `collision_damage` | Deals damage to entities with health on physics contact |
| `gpu_swarm` | A swarm of up to hundreds of thousands of members simulated and drawn on the GPU (see Swarm API) |

## 7. Scripting

Game logic is written in Lua and attached to entities via the `script` component. Each script runs in its own sandboxed environment.

### Script Lifecycle

```lua
-- Called once when the entity is created
function init()
    log("Entity initialized: " .. _entity_string_id)
end

-- Called every frame, dt = delta time in seconds
function update(dt)
end

-- Called before every physics step; dt is always 1/120 s, whatever the frame rate.
-- A frame can run zero, one, or several steps.
function fixed_update(dt)
end

-- Called when the entity is destroyed
function on_destroy()
end

-- Called on physics collision with another entity
function on_collision(other_entity_id)
end

-- Called once when another entity enters this trigger volume
function on_trigger_enter(other_entity_id)
end

-- Called once when another entity exits this trigger volume
function on_trigger_exit(other_entity_id)
end

-- Called when entity takes damage (requires health component)
function on_damage(amount, source_entity_id)
end

-- Called when entity's health reaches 0 (requires health component)
function on_death()
end

-- Called after hot-reload (script file saved while running)
function on_reload()
end
```

Set `collider.is_trigger: true` to make a nonblocking trigger on a static,
kinematic, or dynamic body. Trigger hooks run on the trigger's script and receive
the other entity's string ID. Two overlapping triggers each receive their own
hooks. An overlap produces one enter and one exit, including when the other entity
is destroyed; continuing overlaps do not repeat enters. Events from every fixed
physics step are delivered in both native play and `naive test`. A trigger on a
static body ignores other static colliders, such as the floor it rests on or
another static trigger, because neither side can move into the other; it still
sees kinematic bodies (character controllers) and dynamic ones.

**Note:** Scripts do not receive an `entity` object. Instead, the variable `_entity_string_id` contains the entity's YAML `id` string. Use it with the `entity.*` API functions below. Per-script state can be stored on the `self` table.

### Script Limits

Scripts run with a restricted standard library and with resource limits:

- **Libraries.** `math`, `string`, `table`, `utf8` and the base functions (`pairs`, `pcall`, `print`, and so on) are available. `os`, `io`, `require`, `package`, `coroutine` and `debug` are not.
- **CPU.** One call into a script may run 50 million Lua instructions, on the order of a tenth of a second of pure Lua. A call is one hook such as `update`, the script's top-level code, or one event listener.
- **Memory.** All scripts together may hold 256 MB.

A script that goes over a limit is stopped: its functions and state are dropped, the engine keeps running, and a red message appears on screen and in the log. Save the script file to load it again. An event listener that goes over a limit is removed.

### Entity API

The `entity` table is a global API where every function takes an entity's string ID as the first argument:

```lua
-- Transform
local x, y, z = entity.get_position(_entity_string_id)
entity.set_position(_entity_string_id, x, y + 1, z)

local pitch, yaw, roll = entity.get_rotation(_entity_string_id) -- returns degrees
entity.set_rotation(_entity_string_id, pitch_deg, yaw_deg, roll_deg)

local sx, sy, sz = entity.get_scale(_entity_string_id)
entity.set_scale(_entity_string_id, sx, sy, sz)

-- Entity queries
local alive = entity.exists("some_id") -- true if entity is in the world

-- Lighting
entity.set_light(_entity_string_id, intensity)
entity.set_light_color(_entity_string_id, r, g, b)

-- Material overrides (runtime only, does not modify YAML)
entity.set_base_color(_entity_string_id, r, g, b) -- override base albedo color
entity.set_emission(_entity_string_id, r, g, b)
entity.set_roughness(_entity_string_id, value)
entity.set_metallic(_entity_string_id, value)

-- Spawn a new entity at runtime
entity.spawn("new_id", "procedural:cube", "assets/materials/default.yaml", x, y, z, sx, sy, sz)

-- Destroy an entity (CAUTION: deferred to end-of-frame — see section below)
entity.destroy("some_entity_id")
entity.destroy_by_prefix("bullet_") -- bulk destroy all entities with matching prefix

-- Show/hide an entity
entity.set_visible("some_entity_id", false)

-- Health & Damage (requires health component on entity)
local current, max = entity.get_health("enemy_01")
entity.set_health("enemy_01", current, max)
local new_hp = entity.damage("enemy_01", 25) -- returns new current (clamped to 0)
local new_hp = entity.heal("player", 10)     -- returns new current (clamped to max)
local alive = entity.is_alive("enemy_01")    -- false if dead or hp <= 0

-- Spawn a projectile (physics-driven, auto-damages on contact)
-- entity.spawn_projectile(owner_id, mesh, material, ox,oy,oz, dx,dy,dz, speed, damage, lifetime, gravity)
entity.spawn_projectile(_entity_string_id, "procedural:sphere", "assets/materials/bullet.yaml",
    x, y, z, dx, dy, dz, 20, 10, 5.0, false)
```

> **Warning: `entity.destroy()` is deferred.** Destroy commands execute at end-of-frame.
> If you call `entity.spawn(id)` with the same ID after `entity.destroy(id)` in the
> same frame, the spawn is a no-op and the deferred destroy removes the entity.
> **Safe pattern:** Never destroy entities you plan to re-use. Hide them with
> `entity.set_visible(id, false)` and reposition instead.

### Input API

```lua
-- Check if an action is currently held (defined in input/bindings.yaml)
if input.pressed("sprint") then
    -- Sprint logic
end

-- Check if an action was pressed this frame (single-frame trigger)
if input.just_pressed("jump") then
    -- Jump logic
end

-- Check if ANY action was pressed this frame (useful for "press any key" screens)
if input.any_just_pressed() then
    -- Start game
end

-- Get mouse movement since last frame
local mx, my = input.mouse_delta()
```

### Camera API

```lua
-- Project world coordinates to screen pixels
local sx, sy, visible = camera.world_to_screen(x, y, z)
-- sx, sy = screen pixel coordinates
-- visible = true if the point is in front of the camera and inside the viewport
```

Camera mode is configured in the scene YAML on the camera component:

```yaml
camera:
  fov: 75
  mode: third_person          # "first_person" (default) or "third_person"
  distance: 5.0               # orbit distance behind player (third_person)
  height_offset: 2.0          # camera target height above player (third_person)
  pitch_limits: [-60, 75]     # [min_degrees, max_degrees] for look up/down
```

Third-person camera orbits behind the player using yaw/pitch and automatically handles wall collision.

### UI API

```lua
-- Draw text: ui.text(x, y, text, font_size, r, g, b, a)
ui.text(10, 10, "Score: " .. score, 24.0, 1.0, 1.0, 1.0, 1.0)

-- Draw filled rectangle: ui.rect(x, y, width, height, r, g, b, a)
ui.rect(5, 5, 200, 40, 0.0, 0.0, 0.0, 0.5)

-- Screen flash effect: ui.flash(r, g, b, a, duration_seconds)
ui.flash(1.0, 0.0, 0.0, 0.3, 0.5)

-- Get screen dimensions
local w = ui.screen_width()
local h = ui.screen_height()

-- Measure text width in pixels at a given font size
local tw = ui.text_width("hello", 24)
```

### Audio API

```lua
-- Play a sound effect: audio.play_sfx(id, path, volume)
-- The id lets you reference this sound later (e.g. to stop it)
audio.play_sfx("explosion", "assets/audio/explosion.ogg", 1.0)

-- Play background music: audio.play_music(path, volume, fade_in_seconds)
audio.play_music("assets/audio/theme.ogg", 0.5, 2.0)

-- Stop a sound effect: audio.stop_sound(id, fade_out_seconds)
audio.stop_sound("explosion", 0.5)

-- Stop music: audio.stop_music(fade_out_seconds)
audio.stop_music(1.0)
```

### Physics API

```lua
-- Raycast: physics.raycast(origin_x, origin_y, origin_z, dir_x, dir_y, dir_z, max_dist)
-- Returns: hit (bool), distance, normal_x, normal_y, normal_z
local hit, dist, nx, ny, nz = physics.raycast(0, 1, 0, 0, -1, 0, 100.0)
if hit then
    log("Hit surface at distance " .. dist)
end

-- Hitscan: raycast that also returns the hit entity's string ID and hit point
-- Returns: hit, entity_id, distance, hit_x, hit_y, hit_z, normal_x, normal_y, normal_z
local hit, eid, dist, hx, hy, hz, nx, ny, nz = physics.hitscan(ox, oy, oz, dx, dy, dz, range)
if hit and eid ~= "" then
    entity.damage(eid, 25)  -- apply damage to the hit entity
end
```

### Swarm API

A `gpu_swarm` entity is a pool of members simulated on the GPU. They chase a
target along a flow field around static colliders, keep their spacing, and
can attack the target. Scripts address the swarm by its entity id, never a
single member.

```yaml
- id: goblins
  components:
    transform: { position: [0, 0.5, 0] }   # members walk on the plane at this height
    gpu_swarm:
      count: 5000            # pool size: members alive at once, at most
      initial: 0             # alive at the start (default: count)
      mesh: procedural:cube
      material: assets/materials/goblin.yaml
      scale: 1.0
      speed: 5.5             # metres per second
      spacing: 1.1           # distance members keep from each other
      field_size: 256        # side of the square they move in, centred here
      target_entity: player
      health: 30             # each member's health
      attack_damage: 3       # 0 (default): members never attack
      attack_range: 1.8      # they hit the target from this close, and stop just short of it
      attack_interval: 1.0   # seconds between one member's attacks
```

```lua
-- Bring back up to 500 dead members in a disc of radius 16 around (x, y, z)
swarm.spawn("goblins", 500, x, y, z, 16)

-- Hurt every member within the radius (plus half a member) of a point or a segment
swarm.damage_sphere("goblins", x, y, z, 7, 60)
swarm.damage_line("goblins", x1, y1, z1, x2, y2, z2, 0.6, 22)   -- a piercing shot

-- Chase a point, or an entity (which the members then attack)
swarm.set_target("goblins", x, y, z)
swarm.follow("goblins", "player")

swarm.alive("goblins")      -- living members
swarm.capacity("goblins")   -- the pool size
swarm.kills("goblins")      -- members killed so far
```

Damage volumes apply on the next frame; up to 64 volumes and 8 spawn requests
are served per frame and the rest wait for the next. Members' attacks take
`attack_damage` off the target entity's `health` and call its `on_damage`
with the swarm's id as the source.

Kills and attacks are counted on the GPU and read back without waiting for
it, so they arrive a frame or two late, as events:

| Event | `e.data` fields |
|-------|-----------------|
| `swarm.kills` | `swarm`, `count` (killed since the last event), `alive` |
| `swarm.hit` | `swarm`, `target`, `hits`, `damage` |
| `swarm.cleared` | `swarm` (the last members died and no spawns are on the way) |

`swarm.alive` counts members as of the last report plus spawns requested
since, so it does not drop to zero between `swarm.spawn` and the spawn.

`scenes/tier4_havoc.yaml` is a complete example: three waves, a rifle, a
ground slam and a HUD.

### Math Utilities

Added to the standard Lua `math` table:

```lua
math.lerp(a, b, t)           -- linear interpolation: a + (b - a) * t
math.clamp(value, min, max)  -- clamp value to [min, max]
```

### Events API

```lua
-- Emit an event with a data table (keys must be strings)
events.emit("item.collected", { item_id = "key_01", item_type = "key" })

-- Listen for an event. The callback gets { type = ..., data = { ... } }.
local id = events.on("swarm.kills", function(e)
    game.kills = (game.kills or 0) + e.data.count
end)
events.off(id)
```

Event types and their fields are defined in `events/schema.yaml`. Events are logged and can be checked in tests via `event_occurred()`.

### Game State

A shared `game` table is accessible from all scripts for cross-script state:

```lua
-- Read/write shared game state
game.player_health = game.player_health - 10
game.game_over = true
game.level_complete = false

-- You can add custom keys too
game.score = (game.score or 0) + 100
```

### Logging

```lua
-- Log a message (appears in engine output with [Lua] prefix)
log("Player position: " .. x .. ", " .. y .. ", " .. z)

-- print() also works (outputs to engine log)
print("debug:", some_value)
```

### Per-Script State

Each script has an isolated `self` table that persists across frames and survives hot-reload:

```lua
function init()
    self.health = 100
    self.speed = 5.0
end

function update(dt)
    -- self.health persists between frames
    if self.health <= 0 then
        entity.destroy(_entity_string_id)
    end
end
```

## 8. Testing

nAIVE includes a headless test runner that executes Lua test scripts without a GPU or window.

The runner uses the same simulation code as the engine: the character controller, physics, scripts, events, collision damage, health, entity spawning and destruction, pooling, scale and visibility changes, and scene transitions all run. Audio uses a silent backend. Operations that require GPU resources, such as texture and mesh changes, fail explicitly instead of being silently skipped. Use the packaged release smoke test to verify rendering and native command handling.

### Running Tests

```bash
# Run all test files (discovers test_*.lua in tests/ directory)
naive test

# Run a specific test file
naive test tests/test_combat.lua

# Bound the complete suite, including discovery (default 30 seconds; max 300)
naive test --timeout-seconds 60
```

### Writing Tests

Test files are Lua scripts where every `function test_*()` is automatically discovered and run. Each test gets an isolated runner with fresh game state.

Game script load, initialization, update, collision and event-listener errors fail
the test, even when the test catches a Lua error or subsequently loads another
scene. Missing configured files, discovery errors, and runs that discover no tests
exit with a failure status. A passing command therefore means tests actually ran.

Tests run in a supervised subprocess. Exceeding the suite deadline terminates
the worker and fails the command. Discovery and each test also have CPU, memory
and wall-time limits, including Rust frame waits. Test Lua cannot access the
filesystem/process libraries, runtime code generation or finalizers. Through
MCP, `naive_run_tests` accepts `timeout_seconds`, leaves the editor responsive
and rejects another suite while one is active.

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
    input.inject("move_forward", "press", nil)
    wait_seconds(1.0)
    input.inject("move_forward", "release", nil)

    local ex, ey, ez = get_position("player")
    assert(ez < sz, "Player should have moved forward (negative Z)")
end
```

### Test API

| Function | Description |
|----------|-------------|
| `scene.load(path)` | Load a scene into the headless runner |
| `scene.find(entity_id)` | Find an entity, returns table with `:get(component)` or nil. `:get("transform")` gives `position`; `:get("health")` gives `current`, `max` and `dead` |
| `wait_for_event(type, timeout)` | Advance simulation until event fires (default 10s timeout) |
| `wait_seconds(n)` | Advance n seconds of game time at fixed timestep |
| `wait_frames(n)` | Advance n simulation frames |
| `wait_until(fn, timeout)` | Advance until function returns true (errors on timeout) |
| `get_position(entity_id)` | Get entity position as x, y, z (errors if the entity does not exist) |
| `get_game_value(key)` | Read a value from the game state table |
| `event_occurred(type, filter)` | Check if an event was emitted (optional filter table) |
| `input.inject(action, type, value)` | Simulate input: type is "press", "release", or "axis" |
| `swarm.*` | The Swarm API, to fire at swarms and count their members |
| `assert(condition, message)` | Assert a condition is true |
| `log.info(message)` | Log from test output |

The headless runner steps swarms on the CPU with the same maths as the GPU,
and reports kills and attacks on the frame they happen. See
`tests/test_swarm.lua` and `tests/test_havoc.lua`.

**`scene.find()` returns** a table with an `id` field and a `:get(component)` method:

```lua
local player = scene.find("player")
local transform = player:get("transform")
assert(transform.position.y > 0, "Player above ground")
```

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
naive build
```

The bundle contains `naive-player`, the engine built without the Slang shader compiler, so it runs on machines with no SDK installed. Your Slang shaders are compiled to WGSL while bundling and saved beside each source as `<name>.slang.wgsl`; the player loads those. The player is built for the machine you bundle on, so `naive build` cannot cross-compile: to ship for another platform, run it there. macOS is the supported platform; Linux compiles but is untested, and Windows is not supported yet.

On macOS this creates an app:

```
dist/my-game.app/Contents/
├── Info.plist
├── MacOS/my-game          # naive-player, named after your game
└── Resources/game/        # naive.yaml and all game content
    ├── scenes/  logic/  assets/  pipelines/  input/  events/  tests/
    └── shaders/passes/bloom.slang + bloom.slang.wgsl ...
```

Players open `my-game.app`. On Linux the bundle is `dist/my-game-linux/` with the `my-game` binary next to a `game/` directory; players run `./my-game`. Started with no arguments, the player runs the game packaged beside it, opens no command socket, and loads the project's `default_scene`. Pass `--socket <path>` to control a bundled game over MCP while testing it.

`naive build` signs the app ad hoc, which is enough to run it on your Mac. Other Macs will refuse an app downloaded from the internet unless it is signed with an Apple Developer ID and notarized; see [development and release checks](DEVELOPMENT.md#shipping-a-macos-app).

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

The `docs/` directory contains a PRD and GDD template to help structure your game design. Keeping these up to date helps AI agents make better decisions about your project.

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
| `scenes/combat_demo.yaml` | Health/damage, hitscan, projectiles, collision damage |
| `scenes/third_person_demo.yaml` | Third-person camera orbit with wall collision |

To run an engine demo:

```bash
cd /path/to/nAIVE
naive-runtime --project project --scene scenes/genesis.yaml
```

To use a demo as a starting point for your game, copy the relevant scene and script files into your project directory.

### Sky and image-based lighting

Without `settings.sky` the background is black and surfaces get only a faint
constant ambient light, which suits dark interiors. With a sky, the deferred
pipeline draws it behind the scene (with the directional light's sun disc),
lights every surface with the sky's diffuse light, and reflects it, blurrier
on rougher surfaces. A `texture` replaces the gradient with a panorama from the
project; large images are scaled to 1024 pixels wide. The forward fallback
renderer (no pipeline) does not draw skies.
