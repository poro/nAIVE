# init-0.1.22 — nAIVE Game Project

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
- `naive build` — Bundle for standalone distribution (for the platform you run it on)

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
        mesh: procedural:cube         # see "Available Meshes" below
        material: assets/materials/mat.yaml
      rigid_body:
        body_type: dynamic            # dynamic | fixed | kinematic
      camera:
        fov: 75
        mode: third_person            # first_person | third_person
        distance: 5.0                 # orbit distance (third_person)
        height_offset: 2.0            # above player (third_person)
        pitch_limits: [-60, 75]       # look up/down limits in degrees
      collider:
        shape: cuboid                 # cuboid | sphere | capsule
        half_extents: [x, y, z]       # for cuboid
      point_light:
        color: [r, g, b]
        intensity: 15.0
        range: 100.0
      script:
        source: logic/my_script.lua
      player: {}                      # marks as FPS player
      character_controller:
        speed: 5.0
        jump_force: 8.0
      health:
        max: 100                        # current defaults to max
      collision_damage:
        damage: 25                      # damage dealt on contact
        destroy_on_hit: false           # destroy self after hitting
```

### Available Components

`transform`, `camera`, `mesh_renderer`, `point_light`, `directional_light`,
`rigid_body`, `collider`, `character_controller`, `player`, `script`,
`gaussian_splat`, `tags`, `health`, `collision_damage`.

### Available Meshes

- `procedural:cube` — unit cube (built-in)
- `procedural:sphere` — UV sphere, radius 0.5 (built-in)
- `procedural:plane` — XZ plane, 1x1 (built-in)
- `procedural:cylinder` — Y-axis cylinder, radius 0.5, height 1.0 (built-in)
- `procedural:cone` — Y-axis cone, radius 0.5, height 1.0 (built-in)
- `procedural:torus` — torus (donut), major radius 0.3, minor radius 0.1 (built-in)
- `assets/meshes/file.gltf` — load a glTF/GLB model file
- `assets/meshes/file.ply` — load a PLY point cloud
- If a mesh file is missing, the engine silently falls back to a procedural cube

### Material Format

Materials are YAML files in `assets/materials/`:

```yaml
shader: shaders/passes/mesh_forward.slang
properties:
  base_color: [r, g, b]    # 0.0-1.0
  roughness: 0.5            # 0.0 (smooth) to 1.0 (rough)
  metallic: 0.0             # 0.0 (dielectric) to 1.0 (metal)
  emission: [r, g, b]       # emissive color (0 = no glow)
  albedo_map: assets/textures/brick.png    # optional texture (PNG/JPG/WEBP)
  normal_map: assets/textures/brick_n.png  # optional normal map
blend_mode: opaque           # opaque | transparent
cull_mode: back              # back | front | none
```

Textures are multiplied by base_color, so use `[1,1,1]` for unmodified texture appearance.
All procedural meshes have UV coordinates for texture mapping.

## Lua Scripting API — COMPLETE REFERENCE

### Script Lifecycle

Scripts are attached to entities. Each script runs in its own sandbox.
The variable `_entity_string_id` contains this entity's YAML id.
Use the `self` table for per-script persistent state.

Only `math`, `string`, `table`, `utf8` and the base functions are available:
there is no `os`, `io`, `require`, `package`, `coroutine` or `debug`.
A script that runs too long in one call (50 million Lua instructions) or uses
too much memory (256 MB across all scripts) is stopped until its file is saved again.

```lua
function init()              -- called once on entity creation
function update(dt)          -- called every frame (dt = seconds)
function fixed_update(dt)    -- called at fixed physics timestep
function on_destroy()        -- called when entity is destroyed
function on_collision(other_entity_id)    -- physics collision
function on_trigger_enter(other_entity_id) -- trigger volume enter
function on_trigger_exit(other_entity_id)  -- trigger volume exit
function on_damage(amount, source_id)     -- entity took damage (requires health component)
function on_death()                       -- entity health reached 0 (requires health component)
function on_reload()         -- called after hot-reload
```

### Entity API — `entity.*`

All functions take an entity's string ID as the first argument:

```lua
-- Transform
local x, y, z = entity.get_position(id)
entity.set_position(id, x, y, z)
local pitch, yaw, roll = entity.get_rotation(id)   -- returns degrees
entity.set_rotation(id, pitch_deg, yaw_deg, roll_deg)
local sx, sy, sz = entity.get_scale(id)
entity.set_scale(id, sx, sy, sz)

-- Entity queries
local alive = entity.exists(id)  -- true if entity is in the world

-- Lighting
entity.set_light(id, intensity)
entity.set_light_color(id, r, g, b)

-- Material overrides (runtime only)
entity.set_base_color(id, r, g, b)   -- override base albedo color
entity.set_emission(id, r, g, b)
entity.set_roughness(id, value)
entity.set_metallic(id, value)

-- Spawn new entity: entity.spawn(id, mesh, material, x, y, z, sx, sy, sz)
entity.spawn("bullet_1", "procedural:sphere", "assets/materials/default.yaml", 0, 1, 0, 0.1, 0.1, 0.1)

-- Spawn with physics and script: entity.spawn_ex(config)
entity.spawn_ex({
    id = "crate_1",
    mesh = "procedural:cube",
    material = "assets/materials/wood.yaml",
    position = {0, 5, 0},
    scale = {1, 1, 1},
    rigid_body = "dynamic",                -- "dynamic", "fixed", or "kinematic"
    collider = {
        shape = "cuboid",                   -- "sphere", "cuboid", "capsule"
        half_extents = {0.5, 0.5, 0.5},  -- for cuboid
        -- radius = 0.5,                    -- for sphere/capsule
        restitution = 0.3,
        friction = 0.5,
        is_trigger = false,
    },
    script = "logic/crate.lua",            -- optional script attachment
})

-- Swap texture at runtime: entity.set_texture(id, slot, path)
entity.set_texture("ground", "albedo", "assets/textures/grass.png")
entity.set_texture("wall", "normal", "assets/textures/brick_normal.png")

-- Create runtime mesh from Lua: mesh.create(name, vertices, indices, uvs)
mesh.create("my_terrain", {
    {-5, 0, -5}, {5, 0, -5}, {5, 0, 5}, {-5, 0, 5}
}, {
    0, 2, 1, 0, 3, 2
}, {
    {0, 0}, {1, 0}, {1, 1}, {0, 1}
})
-- Then use: entity.spawn("ground", "runtime:my_terrain", "assets/materials/grass.yaml", 0,0,0, 1,1,1)

-- Destroy entity (CAUTION: deferred to end-of-frame — see Important Notes below)
entity.destroy(id)
entity.destroy_by_prefix("bullet_")  -- bulk destroy all entities with matching prefix

-- Visibility
entity.set_visible(id, true)  -- or false to hide

-- Health & Damage (requires health component on entity)
local current, max = entity.get_health(id)
entity.set_health(id, current, max)
local new_hp = entity.damage(id, amount)   -- returns new current (clamped to 0)
local new_hp = entity.heal(id, amount)     -- returns new current (clamped to max)
local alive = entity.is_alive(id)          -- false if dead or health <= 0

-- Projectile spawning (runtime, physics-driven, auto-damages on hit)
-- entity.spawn_projectile(owner_id, mesh, material, ox, oy, oz, dx, dy, dz, speed, damage, lifetime, gravity)
entity.spawn_projectile(_entity_string_id, "procedural:sphere", "assets/materials/bullet.yaml",
    x, y, z, dx, dy, dz, 20, 10, 5.0, false)
-- owner_id: projectile won't damage its owner
-- gravity: false = flies straight, true = affected by gravity
-- automatically destroyed on hit or when lifetime expires
```

### Input API — `input.*`

Actions are defined in `input/bindings.yaml`.

```lua
input.pressed("action_name")       -- true while held down
input.just_pressed("action_name")  -- true only on the frame pressed
input.any_just_pressed()           -- true if ANY action pressed this frame
local mx, my = input.mouse_delta() -- mouse movement since last frame
local sx, sy = input.mouse_position() -- cursor position in screen pixels
local dx, dy = input.scroll_delta() -- scroll wheel delta this frame
```

### UI API — `ui.*`

Draw HUD elements (called every frame in update):

```lua
ui.text(x, y, "text string", font_size, r, g, b, a)
ui.rect(x, y, width, height, r, g, b, a)   -- filled rectangle
ui.flash(r, g, b, a, duration_seconds)      -- screen flash effect
local w = ui.screen_width()
local h = ui.screen_height()
local tw = ui.text_width("hello", 24)       -- pixel width of text at font size
```

### Audio API — `audio.*`

```lua
audio.play_sfx("sound_id", "assets/audio/file.ogg", volume)
audio.play_music("assets/audio/music.ogg", volume, fade_in_secs)
audio.stop_sound("sound_id", fade_out_secs)
audio.stop_music(fade_out_secs)
```

### Camera API — `camera.*`

```lua
-- Project world coordinates to screen pixels
local sx, sy, visible = camera.world_to_screen(x, y, z)
-- sx, sy = screen pixel coordinates
-- visible = true if the point is in front of the camera and inside the viewport

-- Unproject screen pixel to world ray (for mouse picking)
local ox, oy, oz, dx, dy, dz = camera.screen_to_ray(sx, sy)
-- ox,oy,oz = ray origin (near plane), dx,dy,dz = ray direction (normalized)
-- Use with physics.hitscan(ox,oy,oz, dx,dy,dz, range) for click-to-select
```

Camera mode is set in the scene YAML on the camera component:

```yaml
camera:
  fov: 75
  mode: first_person          # "first_person" (default) or "third_person"
  distance: 5.0               # third_person only: orbit distance behind player
  height_offset: 2.0          # third_person only: camera target height above player
  pitch_limits: [-60, 75]     # [min_degrees, max_degrees] for look up/down
```

Third-person camera automatically orbits behind the player and handles wall
collision (pulls camera closer to avoid clipping through geometry).

### Physics API — `physics.*`

```lua
-- Raycast returns: hit(bool), distance, normal_x, normal_y, normal_z
local hit, dist, nx, ny, nz = physics.raycast(ox, oy, oz, dx, dy, dz, max_dist)

-- Hitscan: raycast that returns entity info + hit point
-- Returns: hit(bool), entity_id(string), distance, hit_x, hit_y, hit_z, normal_x, normal_y, normal_z
local hit, eid, dist, hx, hy, hz, nx, ny, nz = physics.hitscan(ox, oy, oz, dx, dy, dz, range)
-- Use with entity.damage(): if hit then entity.damage(eid, 25) end
```

### Math Utilities

Added to the standard Lua `math` table:

```lua
math.lerp(a, b, t)           -- linear interpolation: a + (b - a) * t
math.clamp(value, min, max)  -- clamp value to [min, max]
```

### Events API — `events.*`

Event types are defined in `events/schema.yaml`.

```lua
events.emit("event.type", { key1 = "value1", key2 = "value2" })
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

## Important Notes

### `entity.destroy()` Is Deferred

`entity.destroy(id)` is queued and executes at END of frame. If you call
`entity.spawn(id)` with the same ID in the same frame, the spawn is a no-op
(entity still exists) and the deferred destroy then removes it — leaving no entity.

**Safe pattern:** Never destroy entities you plan to re-use. Instead, hide them
with `entity.set_visible(id, false)` and reposition. Use `entity.exists(id)` to
check if an entity is alive before operating on it.

## Known Limitations

Know what the engine does NOT have so you don't waste time:

- **No `scene.find()` in runtime scripts** — only available in the test API.
  You must know entity IDs upfront from your YAML. Plan your entity IDs.
- **No `scene.load()` in runtime scripts** — cannot switch scenes from Lua.
  For multi-screen games (title/gameplay/gameover), use a single scene with
  visibility toggling: hide/show groups of entities with `entity.set_visible()`.
- **No `events.on()` subscription** — `events.emit()` fires events but there
  is no way to listen for events in runtime scripts. Use the `game` table for
  cross-script communication instead.
- **No tween/animation API** — animate manually in `update(dt)` using `self`
  state, `math.lerp()`, and `entity.set_position/set_rotation/set_scale`.
- **No particle system** — build particle effects by spawning/destroying
  entities with `entity.spawn()` and `entity.destroy()`.
- **No camera shake API** — implement by moving the camera entity's position
  with small random offsets in `update(dt)`.
- **Camera is just an entity** — move/rotate the camera from any script using
  `entity.set_position("main_camera", x, y, z)` and
  `entity.set_rotation("main_camera", pitch, yaw, roll)`.

## Common Patterns

### Grid of Entities (boards, tile maps)
Spawn entities in `init()` of a manager script:
```lua
function init()
    for row = 0, 16 do
        for col = 0, 16 do
            local id = "tile_" .. row .. "_" .. col
            entity.spawn(id, "procedural:cube", "assets/materials/default.yaml",
                col - 8, 0, row - 8, 0.9, 0.1, 0.9)
        end
    end
end
```

### Multi-Screen Game (title, gameplay, gameover)
Define all screens in one scene. Use visibility and a state variable:
```lua
function init()
    self.state = "title"
    show_title()
end

function update(dt)
    if self.state == "title" and input.just_pressed("interact") then
        self.state = "playing"
        hide_title()
        show_gameplay()
    end
end

function show_title()
    entity.set_visible("title_text", true)
    entity.set_visible("game_board", false)
end
```

### Cross-Script Communication
Use the shared `game` table:
```lua
-- In player script:
game.score = game.score + 10
game.player_alive = true

-- In HUD script:
function update(dt)
    ui.text(10, 10, "Score: " .. (game.score or 0), 24, 1, 1, 1, 1)
end
```

### Timer / Delayed Action
```lua
function init()
    self.timer = 3.0  -- 3 second countdown
end
function update(dt)
    self.timer = self.timer - dt
    if self.timer <= 0 then
        -- do something once
        self.timer = 999  -- prevent re-trigger
    end
end
```

### Smooth Animation
```lua
function init()
    self.t = 0
end
function update(dt)
    self.t = self.t + dt
    local y = 1 + math.sin(self.t * 2) * 0.5  -- bob up and down
    entity.set_position(_entity_string_id, 0, y, 0)
end
```

### Hitscan Shooting (click to damage)
```lua
function update(dt)
    if input.just_pressed("attack") then
        -- Get player position and forward direction for the ray
        local px, py, pz = entity.get_position(_entity_string_id)
        local hit, eid, dist, hx, hy, hz = physics.hitscan(
            px, py + 1.5, pz, 0, 0, -1, 100)
        if hit and eid ~= "" then
            entity.damage(eid, 25)
        end
    end
end
```

### Damageable Enemy with Death Callback
In the scene YAML:
```yaml
  - id: enemy_01
    components:
      transform: { position: [5, 0, 0] }
      mesh_renderer: { mesh: procedural:cube, material: assets/materials/enemy.yaml }
      health: { max: 50 }
      collision_damage: { damage: 10, destroy_on_hit: false }
      collider: { shape: cuboid, half_extents: [0.5, 0.5, 0.5] }
      rigid_body: { body_type: fixed }
      script: { source: logic/enemy.lua }
```
In `logic/enemy.lua`:
```lua
function on_damage(amount, source_id)
    ui.flash(1, 0, 0, 0.3, 0.1)  -- red flash
    log("Took " .. amount .. " damage from " .. source_id)
end

function on_death()
    log(_entity_string_id .. " died!")
    entity.destroy(_entity_string_id)
end
```

### Projectile Shooting
```lua
function update(dt)
    if input.just_pressed("attack") then
        local px, py, pz = entity.get_position(_entity_string_id)
        entity.spawn_projectile(
            _entity_string_id,          -- owner (won't damage self)
            "procedural:sphere",        -- mesh
            "assets/materials/bullet.yaml",  -- material
            px, py + 1.5, pz,          -- spawn position
            0, 0, -1,                   -- direction
            30,                         -- speed
            10,                         -- damage
            5.0,                        -- lifetime (seconds)
            false                       -- gravity (false = flies straight)
        )
    end
end
```

## Test API (for tests/ scripts)

Tests run headless (no GPU). Each `test_*()` function gets a fresh runner.
Physics, scripts, events, collision damage, health and entity destruction run;
spawning entities or projectiles needs the full engine and is dropped.

```lua
scene.load("scenes/main.yaml")                -- load a scene
scene.find("entity_id")                       -- returns table or nil
  :get("transform")                           -- returns {position={x,y,z}}
wait_for_event("lifecycle.scene_loaded", 10)  -- wait with timeout
wait_seconds(1.0)                             -- advance game time
wait_frames(5)                                -- advance N frames
wait_until(function() return cond end, 10)    -- wait for condition
get_position("entity_id")                     -- returns x, y, z; errors if missing
get_game_value("key")                         -- read game table
event_occurred("event.type", {key="val"})    -- check event log
input.inject("action", "press", nil)          -- simulate input
input.inject("action", "release", nil)
input.inject("action", "axis", {0, 1})       -- axis: {x, y}
log.info("test message")
assert(condition, "error message")
```

## Dev Log

This project includes a `dev.log` file for recording engine feedback — what worked,
what didn't, pain points, and missing features. This feedback is invaluable for
improving the nAIVE engine.

- **Maintain `dev.log`** throughout development — update it as you encounter issues
  or discover things that work well
- Run `naive submit-log` to submit your feedback as a GitHub issue
- Requires `NAIVE_GITHUB_TOKEN` env var (GitHub PAT with `public_repo` scope)
- The log is tracked in git (`!dev.log` overrides `*.log` in `.gitignore`)
