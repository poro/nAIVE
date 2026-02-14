# nAIVE v3 — Networked Worlds Architecture

## Vision

nAIVE becomes a platform where anyone can create, share, and inhabit 3D worlds using natural language. Worlds are created with AI (Claude Code), addressed by memorable four-word names, controlled from anywhere (Telegram, CLI, browser), and rendered locally on each connected client.

```
[Creators]              [Server]                [Clients]
 Claude Code  ──>  nAIVE World Server  <──>  nAIVE Renderer
 Telegram Bot ──>   (state authority)   ──>  (local GPU)
```

## Core Concepts

### Four-Word World Names

Every world gets a human-memorable address: `black.squirrel.white.deer`

- 4 words from a curated dictionary of ~2000 common words
- 2000^4 = 16 trillion unique addresses
- Easy to remember, easy to share verbally
- Maps to a world ID on the server — DNS for virtual worlds

### Three-Layer Architecture

| Layer | Role | Runs |
|-------|------|------|
| **Creator** | Builds worlds via AI or manual authoring | Developer machine |
| **Server** | Holds canonical world state, runs logic, dispatches events | Cloud / headless |
| **Client** | Renders the world, captures input, plays audio | End-user device (local GPU) |

### Hybrid Authority Model

The server is the single source of truth for world state:

- **Server runs**: ECS, physics (Rapier), game logic (Lua scripts), event dispatch
- **Client runs**: Rendering (wgpu), audio (Kira), input capture, UI
- **Server streams**: State deltas (entity transforms, spawns, despawns) to clients
- **Client sends**: Input events (key presses, not positions) to server

This gives anti-cheat, consistent state for all viewers, and lets Telegram commands work — the server just executes them in the world's Lua environment.

## Creation Flow

### Example: 3D Snake Game

```
You:     "I want to make a 3D snake game"

Claude:  -> generates project/scenes/snake.yaml
         -> generates project/logic/snake_controller.lua
         -> generates project/logic/food_spawner.lua
         -> generates materials for snake segments

You:     cargo run -- --scene scenes/snake.yaml
         -> local window opens, you play-test

You:     "publish this as snake.3d.nokia.mark"
         -> pushes scene + assets to nAIVE world server

Friend:  naive-client connect snake.3d.nokia.mark
         -> downloads scene definition
         -> renders locally, receives state updates via WebSocket
```

### What the AI Generates

A complete world is just files:

- `scenes/*.yaml` — scene graph (entities, components, hierarchy)
- `logic/*.lua` — game logic (movement, spawning, scoring, AI)
- `assets/materials/*.yaml` — PBR material definitions
- `assets/meshes/*.gltf` — 3D models (or procedural mesh references)
- `assets/audio/*.wav` — sound effects and music

Claude Code already knows the nAIVE format. You describe what you want, it generates the files, you test locally, then publish.

## Network Protocol

### Connection Lifecycle

```
1. Client connects to server via WebSocket
2. Client sends: { "join": "snake.3d.nokia.mark" }
3. Server sends: Scene snapshot (full YAML + asset manifest)
4. Client downloads assets it doesn't have (from CDN or server)
5. Server streams state deltas every tick
6. Client renders locally, sends input events back
```

### State Delta Format

```json
{
  "tick": 1042,
  "updates": [
    {"entity": "snake_head", "transform": {"position": [3, 0, 5]}},
    {"entity": "snake_seg_4", "transform": {"position": [2, 0, 5]}},
    {"spawn": {"id": "food_7", "mesh": "procedural:sphere", "position": [8, 0, 2]}},
    {"despawn": "food_3"}
  ]
}
```

### Input Events

```json
{"input": "key_down", "key": "W", "player": "player_1"}
```

Clients never send positions or game state — only inputs. Server validates and applies.

## Telegram Integration

Already partially built (GENESIS demo Telegram bot bridge). Extended for v3:

```
User on Telegram:
  "make it rain in black.squirrel.white.deer"

Bot:
  1. Parses command -> action: "weather", params: "rain"
  2. Looks up world "black.squirrel.white.deer" on server
  3. Server executes: weather.set("rain") in that world's Lua env
  4. Lua script spawns particle emitter, changes skybox, adds sound
  5. State delta broadcast to all connected clients
  6. Everyone sees rain
```

Commands can be anything the world's Lua scripts handle:
- "spawn 10 red cubes" -> `spawner.create("cube", {count=10, color="red"})`
- "change gravity to moon" -> `physics.set_gravity(0, -1.62, 0)`
- "play jazz music" -> `audio.play_music("assets/audio/jazz.wav", 0.5, 2.0)`
- "make the snake faster" -> `snake.set_speed(2.0)`

## Server Architecture

### World Registry

```
worlds/
  a1b2c3d4/                    # world ID
    name: snake.3d.nokia.mark  # four-word address
    scene: scenes/snake.yaml   # scene definition
    logic/                     # Lua scripts
    assets/                    # meshes, materials, audio
    state: <live ECS snapshot> # current world state
    clients: [ws1, ws2, ...]   # connected WebSocket sessions
```

### Headless Server Binary

`naive-server` — compiles the same ECS + physics + scripting code, but without wgpu rendering:

```
naive-server
  ├── World Manager (runs N worlds concurrently)
  ├── WebSocket Server (accepts client connections)
  ├── Telegram Bridge (receives commands)
  ├── World Registry (four-word name -> world ID)
  ├── Asset Store (serves scene + asset files)
  └── Per-World:
      ├── hecs World (ECS)
      ├── Rapier PhysicsWorld (headless)
      ├── mlua ScriptRuntime (Lua logic)
      └── State Broadcaster (delta compression + send)
```

### Thin Client Binary

`naive-client` — rendering + input + audio, no authority:

```
naive-client connect snake.3d.nokia.mark
  ├── WebSocket Client (receives state deltas)
  ├── Asset Cache (downloads + caches scene assets)
  ├── wgpu Renderer (local GPU rendering)
  ├── Kira AudioSystem (local audio)
  ├── Input Capture (keyboard, mouse -> sends to server)
  └── State Applicator (applies deltas to local ECS for rendering)
```

## Multiplayer

With server authority, multiplayer is natural:

- Each player connecting gets a player entity spawned in the world
- Server runs game logic for all players
- State deltas include all entities (all players see each other)
- For a snake game: each player gets their own snake, server handles collision between snakes

### Player Identity

- Anonymous by default (Telegram username or random guest ID)
- Four-word player names too? `brave.fox.quick.nine`
- Permissions per world (owner, editor, player, viewer)

## Implementation Phases

### Phase 1 — Local Creation Loop (current state)

What exists today:
- Claude Code generates scenes + scripts
- `cargo run` opens local render window
- Full ECS, physics, audio, scripting
- Iterate and test locally

### Phase 2 — Headless Server

- Extract rendering from engine core (feature flag or separate binary)
- `naive-server` runs worlds headless (ECS + physics + Lua, no GPU)
- WebSocket endpoint for state streaming
- World registry with four-word naming
- Single world first, then multi-world

### Phase 3 — Thin Client

- `naive-client` connects to a world by four-word name
- Downloads scene definition + assets on first connect
- Receives state deltas, renders locally
- Sends input events to server
- Asset caching for fast reconnect

### Phase 4 — Telegram Bridge (extend existing)

- Bot receives natural language commands
- AI parses intent -> maps to Lua function calls
- Server executes in target world
- All connected clients see the result

### Phase 5 — Publishing & Discovery

- `naive publish snake.3d.nokia.mark` — uploads world to server
- World browser / directory (web UI or Telegram bot)
- Version control for worlds (rollback, fork)
- Permissions (public, private, invite-only)

## What nAIVE Already Has

| Component | Status | Used By |
|-----------|--------|---------|
| Scene graph (YAML) | Done | Server + Client |
| ECS (hecs) | Done | Server + Client |
| Physics (Rapier3D) | Done | Server |
| Rendering (wgpu) | Done | Client |
| Audio (Kira) | Done | Client |
| Lua scripting (mlua) | Done | Server |
| Telegram bot bridge | Prototype | Server |
| PBR materials | Done | Client |
| Procedural meshes | Done | Client |
| FXAA post-processing | Done | Client |
| Shadow mapping | Done | Client |

## Key Design Decisions

1. **Server authority over client authority** — enables Telegram control, anti-cheat, consistent state
2. **Four-word names over URLs** — memorable, shareable verbally, fun
3. **Lua on server only** — clients are dumb renderers, logic stays authoritative
4. **WebSocket over UDP** — simpler, sufficient for the tick rates we need (10-30 Hz state updates)
5. **Scene YAML as world format** — already exists, human-readable, AI-generatable
6. **Local rendering** — no server GPU cost, scales to many worlds cheaply
