# nAIVE Platform PRD v5.0 — Networked Worlds Infrastructure

**Version:** 5.0
**Date:** February 14, 2026
**Author:** Mark Ollila
**License:** BSL 1.1 (converts to MIT after 3 years)
**Status:** Draft
**Classification:** Confidential
**Companion Document:** nAIVE Engine PRD v5.0 (MIT)

---

## 1. Executive Summary

The nAIVE Platform is the server infrastructure that turns nAIVE engine games into multiplayer, shareable, AI-directed experiences. While the engine (MIT) handles rendering, physics, and gameplay on the client, the platform (BSL 1.1) handles everything that lives on a server:

- **Multiplayer** — server-authoritative game state, dual WebSocket/WebRTC transport, client-side prediction reconciliation
- **World Registry** — four-word addresses (`black.squirrel.white.deer`) as the sharing mechanism
- **AI Director** — server-side system that monitors players and evolves worlds in real-time
- **LLM NPC Runtime** — persistent-memory NPCs with tiered inference for cost control
- **Telegram Bridge** — control live worlds from a chat app
- **Matchmaking** — queue-based skill matching and instance management
- **Interest Management** — server-side spatial partitioning for bandwidth scaling
- **Horde Authority** — simplified server-side simulation for GPU compute entity games

The platform adds multiplayer and network services to any game built with the engine. A game developer can ship a single-player game using only MIT-licensed engine crates. The platform is optional — but it's where the network effects, community, and long-term value live.

### Three Proof Games

Each proof game escalates what the platform must handle:

1. **Snake Sweeper** — 4-player turn-based (WebSocket, 20 Hz, ~16 KB/s per client)
2. **HAVOC** — 4-player co-op horde survival (WebRTC, 60 Hz, 50,000 GPU entities, ~360 KB/s per client)
3. **DROPZONE** — 100-player battle royale (WebRTC, 30-60 Hz, 2km² map, ~300 KB/s per client)

If the platform can handle DROPZONE, it can handle any game.

---

## 2. Platform Vision

```
"make it rain in black.squirrel.white.deer"
```

A message sent from Telegram. Within one second, every player connected to that world sees rain particles falling, hears rain audio, and watches the lighting shift. The AI parsed "make it rain" into a weather system call. The server executed it in the world's Lua environment. State deltas flowed to all connected clients. The world changed because someone asked it to — from a chat app.

**The platform's three promises:**

**For creators:** Publish your game with a four-word name. Anyone in the world can play it instantly — in a browser, no download. Multiplayer is free (you don't build it, the platform provides it).

**For players:** Hear about a game. Type four words. You're in. No download, no account, no app store. Your friend is already there.

**For AI:** You live inside the world. You remember. You adapt. You're not a tool — you're a collaborator. The Telegram bridge, the AI Director, and the LLM NPCs all run on the server, always on.

---

## 3. Architecture

### 3.1 The Server Crate

`naive-server` is the single BSL-licensed crate. It depends on `naive-core` (MIT) for shared types but has no dependency on `naive-client`.

```
┌────────────────────────────────────────────────────────┐
│  naive-server (BSL 1.1)                                 │
│                                                         │
│  ├── World Manager        (tokio, runs N worlds)       │
│  ├── Transport Layer      (WebSocket + WebRTC)         │
│  ├── World Registry       (four-word → world ID)       │
│  ├── Matchmaker           (queue, ELO, instances)      │
│  ├── AI Director          (per-world adaptive system)  │
│  ├── LLM NPC Runtime     (tiered inference)            │
│  ├── Telegram Bridge      (teloxide, AI-parsed NL)     │
│  ├── Interest Manager     (spatial partitioning)       │
│  ├── Horde Grid Sim      (simplified authority)        │
│  └── Per-World Instance:                               │
│      ├── hecs World       (canonical ECS state)        │
│      ├── Rapier Physics   (headless, no rendering)     │
│      ├── mlua ScriptRuntime (Lua game logic)           │
│      ├── EventBus         (typed pub/sub)              │
│      └── State Broadcaster (delta → transport fan-out) │
│                                                         │
│  Depends on: naive-core (MIT)                          │
│  Does NOT depend on: naive-client                      │
└────────────────────────────────────────────────────────┘
```

```toml
# Cargo.toml for naive-server
[dependencies]
naive-core = { path = "../naive-core" }
tokio = { version = "1", features = ["full"] }
tungstenite = "0.21"      # WebSocket
webrtc = "0.9"            # WebRTC DataChannels
teloxide = "0.12"         # Telegram Bot API
mlua = { version = "0.9", features = ["lua54"] }
rapier3d = "0.18"
hecs = "0.10"
sqlx = { version = "0.7", features = ["sqlite"] }
```

### 3.2 Three-Layer Stack (Server Perspective)

```
                    ┌─────────────────────────────────────────────┐
                    │              CREATOR LAYER                   │
                    │                                              │
                    │  Claude Code ──────> YAML + Lua + Assets     │
                    │  NL Compiler ──────> Scene Generation        │
                    │  Telegram ─────────> Live World Commands     │
                    │  MCP Server ──────> Agent Interaction        │
                    └──────────────────┬──────────────────────────┘
                                       │ publish / command
                    ┌──────────────────▼──────────────────────────┐
                    │              SERVER LAYER (this PRD)         │
                    │                                              │
                    │  World Registry (4-word names ↔ world IDs)  │
                    │  Per-World:                                  │
                    │    ├── hecs ECS    (canonical state)         │
                    │    ├── Rapier      (headless physics)        │
                    │    ├── mlua        (game logic scripts)      │
                    │    ├── AI Agents   (LLM NPC runtime)        │
                    │    ├── AI Director (adaptive evolution)      │
                    │    ├── Horde Sim   (simplified grid)         │
                    │    ├── Matchmaker  (queue + ELO)             │
                    │    └── Broadcaster (delta → WS/WebRTC)      │
                    │  Transport: WebSocket + WebRTC               │
                    │  Telegram Bridge  (teloxide)                 │
                    │  Asset CDN        (scene + mesh + audio)     │
                    └──────────────────┬──────────────────────────┘
                                       │ state deltas
                    ┌──────────────────▼──────────────────────────┐
                    │  CLIENT LAYER (see Engine PRD)               │
                    └─────────────────────────────────────────────┘
```

---

## 4. Server Authority Model

**The server is the single source of truth for all world state.**

- **Anti-cheat by design.** Clients send input events only. They cannot claim "my position is X" or "my score is Y." The server computes all state.
- **Consistent for all viewers.** Every connected client sees the same world.
- **External control works naturally.** Telegram commands, MCP tools, AI Director interventions all execute in the server's Lua environment.
- **Multiplayer is free.** Adding a player = spawn entity + process input stream.

### Horde Authority Split

For GPU compute entity games (HAVOC), the server is authoritative for *aggregate horde state*, not individual entity positions:

| What | Where | Why |
|------|-------|-----|
| Total enemies alive | Server | Gameplay scoring |
| Wave number, timing | Server | Game progression |
| Damage dealt to players | Server | Anti-cheat |
| Flow field | Server (computed) → Client (consumed) | Pathfinding |
| Individual goblin positions | Client (GPU compute) | 50K positions = 2.4 MB/tick. Impossible to replicate. |
| Spawn commands | Server → Client | Authority over spawning |
| Death events | Server (validated from grid counts) | Scoring |

Sending 50,000 entity positions per tick at 60 Hz = 144 MB/s per client. Instead, the server sends flow fields (~64 KB, ~1/sec), spawn commands (~100 bytes each), death counts, and aggregate state. Each client runs the same GPU compute shaders with the same flow field. Small divergences between clients are invisible in a 50,000 entity swarm.

---

## 5. Networking

### 5.1 Dual Transport Architecture

```
┌─────────────────────────────────────┐
│         Transport Layer             │
│                                     │
│  ┌───────────┐  ┌────────────────┐ │
│  │ WebSocket │  │ WebRTC         │ │
│  │ (reliable)│  │ DataChannel    │ │
│  │ TCP-based │  │ (unreliable)   │ │
│  │ 20 Hz     │  │ 60 Hz          │ │
│  └─────┬─────┘  └───────┬────────┘ │
│        └───────┬────────┘          │
│                │                    │
│    State Delta Protocol             │
│    (shared, transport-agnostic)     │
└────────────────┬───────────────────┘
                 │
          World Instance
```

Per-world YAML configuration selects the transport:

```yaml
network:
  transport: webrtc          # or "websocket"
  tick_rate: 60              # Hz
  interpolation_delay: 2     # ticks
  prediction: true
  interest_radius: 100.0     # meters
```

Both transports encode the same state delta protocol. A single server can host worlds with different transports simultaneously.

**WebRTC signaling:** HTTPS for SDP exchange, then DataChannel for all game traffic. ICE/STUN for NAT traversal.

### 5.2 State Delta Protocol

```json
{
  "tick": 3042,
  "ack_input_tick": 3040,
  "updates": [
    {
      "entity": "player_1",
      "transform": {"position": [43.2, 0, 87.1], "rotation": [0, 45, 0]},
      "health": 85,
      "shield": 40
    },
    {
      "entity": "player_2",
      "transform": {"position": [51.0, 0, 82.3]},
      "animation_state": "run"
    },
    {"spawn": {"id": "pickup_12", "type": "health_pack", "position": [40, 0, 90]}},
    {"despawn": "pickup_7"}
  ],
  "horde": {
    "flow_field_update": "base64_encoded_128x128_grid",
    "spawns": [
      {"count": 200, "position": [80, 0, 50], "type": "goblin_melee"}
    ],
    "deaths": {"total_this_tick": 47},
    "alive_count": 23847,
    "wave": 5
  },
  "audio": [
    {"type": "play_sfx", "id": "explosion_3", "file": "assets/audio/boom.mp3", "position": [45, 0, 88]}
  ]
}
```

**Bandwidth analysis:**

| Scenario | Delta Size/Tick | At Tick Rate |
|----------|----------------|-------------|
| Snake Sweeper (4 players, 20 Hz) | ~800 bytes | ~16 KB/s |
| HAVOC solo (1 player, 60 Hz) | ~4 KB | ~240 KB/s |
| HAVOC co-op (4 players, 60 Hz) | ~6 KB | ~360 KB/s |
| DROPZONE (100 players, 30 Hz) | ~8 KB (per client) | ~240 KB/s |
| Arena shooter (16 players, 60 Hz) | ~8 KB | ~480 KB/s |

### 5.3 Connection Lifecycle

```
1. Client resolves four-word name → server address + transport type
      GET https://registry.naive.world/resolve/black.squirrel.white.deer
      → { "server": "us-east.naive.world", "world_id": "a1b2c3d4",
           "transport": "webrtc", "tick_rate": 60 }

2a. (WebSocket) Standard handshake
2b. (WebRTC) HTTPS signaling → DataChannel establishment

3. Server sends: full scene snapshot + horde config
4. Client downloads assets (CDN or server, cached locally)
5. Client builds local ECS + renderer from scene definition
6. Server streams state deltas every tick (interest-managed)
7. Client sends input events to server
8. On disconnect: client state discarded, server is truth
```

### 5.4 Latency Budgets

**Snake Sweeper (v4.0 profile):**
```
Game tick: 280ms
Input → server: 20-50ms (WebSocket)
Server tick: 1-5ms
Delta → client: 20-50ms
Round-trip: ~60-120ms
Budget remaining: 160-220ms ← generous
```

**HAVOC (v5.0 profile):**
```
Game tick: 16.7ms (60 Hz)
Input → server: 5-15ms (WebRTC)
Server tick: 2-5ms
Delta → client: 5-15ms
Perceived latency: <16ms (prediction hides it)
```

---

## 6. Server-Side Prediction Reconciliation

Client-side prediction runs on the client (see Engine PRD Section 4.6). The server's role:

1. Receive input event from client with tick number
2. Process input authoritatively (character controller + Rapier physics)
3. Produce authoritative position
4. Include `ack_input_tick` in delta broadcast
5. Client compares prediction to server state and reconciles if needed

The server does NOT run prediction. It runs the authoritative simulation. The `ack_input_tick` tells the client "I've processed your inputs up to tick N" so the client can discard old prediction buffer entries.

---

## 7. Server-Authoritative Hit Detection

Weapons are defined in the Engine PRD. Hit detection runs on the server:

### 7.1 Hitscan Weapons

```
Client sends: {action: "fire", weapon: "assault_rifle", aim_direction: [0.7, 0.1, 0.7], tick: 3042}
Server receives fire event
Server rewinds entity positions to tick 3042 (lag compensation)
Server raycasts from player position in aim_direction
If hit: apply damage, broadcast hit event to all clients
If miss: broadcast miss event (impact effect at ray endpoint)
```

### 7.2 Lag Compensation

The server maintains a history buffer of entity positions (last 200ms, ~12 ticks at 60 Hz). When processing a fire event from tick 3042, the server temporarily rewinds all entities to their positions at tick 3042, performs the raycast, then restores current positions.

This ensures that what the client saw when they fired (their view of other players' positions at the time) is what the server validates against. Without lag compensation, players would need to "lead" their shots by their latency — unacceptable for competitive games.

### 7.3 Projectile Weapons

Projectiles are ECS entities simulated on the server. Each tick: update position (physics). On collision: apply splash damage. Broadcast spawn, position updates, and explosion events to clients.

---

## 8. Interest Management

Server-side spatial partitioning determines per-client entity visibility:

```
┌─────┬─────┬─────┬─────┐
│     │     │     │     │
│     │  P1 │     │     │   P1's relevance radius
│     │  ●──┼─────┼──── │   covers nearby cells
├─────┼─────┼─────┼─────┤
│     │     │     │  P2 │   P2 is outside P1's radius
│     │     │     │  ●  │   → P1 does NOT receive P2 updates
│     │     │     │     │
└─────┴─────┴─────┴─────┘
```

**Grid-based partitioning** with configurable cell size. Each client has a relevance set updated every N ticks (sticky to avoid flickering).

**Bandwidth scaling:**

| Players | Without Interest Mgmt | With Interest Mgmt | Savings |
|---------|----------------------|--------------------|---------|
| 4 | 16 entity streams | ~8 | 50% |
| 16 | 256 | ~80 | 69% |
| 100 | 10,000 | ~800 | 92% |

Interest management is critical for DROPZONE (100 players). Without it, each client receives 100 player updates per tick. With it, each client receives ~20-30 nearby players.

**For horde data:** Flow field is global (small, ~64 KB). Spawn/death events filtered by interest radius. This means clients on opposite sides of the map see slightly different horde compositions — which is fine.

---

## 9. AI Director

Server-side system that monitors player behavior telemetry and evolves the world:

```
Player Telemetry → Analysis → Intervention Plan → Graduated Response

Intervention levels:
  Subtle:         Adjust spawn rates, lighting mood, music tempo
  Moderate:       Introduce new NPCs, modify terrain, create events
  Significant:    Restructure layout, change rules, add mechanics
  Transformative: Genre shifts (requires player opt-in)
```

### 9.1 Snake Sweeper Director

- 3+ mines in a row → reduce density 20%, add food
- Clearing too easily → increase density, add obstacles
- Player stuck → flash hint showing safe path
- Mine circumnavigated → spawn bonus item

### 9.2 HAVOC Director

- Deaths > 3/minute → reduce wave size 30%, add health drops
- No deaths, high kills → increase spawn rate, add ranged/elite enemies
- Players cluster → spawn flanking waves from behind
- Vehicle found → increase density to match power spike
- Session > 20 min → boss wave

### 9.3 DROPZONE Director

- Storm pacing: shrink faster when many players alive, slower when few remain
- Loot distribution: increase rare drops in low-traffic areas to encourage exploration
- Bot difficulty: AI-controlled bots (to fill lobbies) scale to player skill
- Supply drop placement: drops in contested areas to create engagement

**Fairness guardrail:** In multiplayer, the Director never gives one player a mechanical advantage.

---

## 10. LLM NPC Agent Runtime

Entities with `agent_brain` components (defined in Engine PRD) run their inference on the server:

```yaml
- id: merchant
  components:
    agent_brain:
      personality: "Gruff but fair dwarven blacksmith. Remembers repeat customers."
      memory_slots: 50
      inference_tier: 2
```

### 10.1 Tiered Inference

| Tier | Latency | Cost/Call | Use Case | Implementation |
|------|---------|-----------|----------|---------------|
| 1 | 500ms | ~$0.01 | Complex conversation, novel situations | Claude API (Haiku/Sonnet) |
| 2 | 50ms | ~$0.001 | Routine dialogue | Distilled local model (Ollama) |
| 3 | 1ms | ~$0 | Greetings, combat barks | Pattern match + template cache |

### 10.2 Memory Architecture

- **Short-term:** Last 10 interactions (full context)
- **Long-term:** Vector-indexed memory store (embed interactions, retrieve relevant)
- **Personality:** Immutable system prompt
- **Relationships:** Per-player score (affects dialogue, trade prices)

---

## 11. Telegram Integration

### 11.1 Architecture

```
┌──────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│ Telegram  │────>│ nAIVE Bot    │────>│ AI Parser    │────>│ World Server │
│ User      │<────│ (teloxide)   │<────│ (Claude API) │<────│ (Lua exec)   │
└──────────┘     └──────────────┘     └──────────────┘     └──────────────┘
```

### 11.2 Command Flow

```
User on Telegram: "make it rain in black.squirrel.white.deer"

1. Bot extracts world reference
2. AI parser: "make it rain" → { "function": "weather.set", "args": ["rain"] }
3. Server executes in world's Lua environment: weather.set("rain")
4. State deltas broadcast to all clients
5. Bot replies: "It's raining in black.squirrel.white.deer"
```

### 11.3 Bot Commands

```
/connect snake.3d.nokia.mark       — Subscribe to world events
/disconnect                        — Stop receiving updates
/status                            — World stats
/players                           — Connected players
/worlds                            — Your published worlds
/command <anything>                — Natural language command
```

### 11.4 World Notifications (Push)

Push events to subscribed Telegram users: high scores, player joins, wave completions, boss kills.

---

## 12. World Registry and Four-Word Naming

### 12.1 Why Four Words

URLs fail for games. Nobody remembers `https://naive.world/a1b2c3d4-5678-...`

`black.squirrel.white.deer` — you can tell someone this across the room.

~2,000 curated words → 16 trillion unique addresses. Pattern: `{adjective}.{noun}.{adjective}.{noun}`.

### 12.2 Registry Architecture

```sql
-- SQLite schema
CREATE TABLE worlds (
    id         TEXT PRIMARY KEY,
    name       TEXT UNIQUE,           -- four-word address
    owner      TEXT,
    server     TEXT,                  -- server hostname
    transport  TEXT DEFAULT 'websocket',
    tick_rate  INTEGER DEFAULT 20,
    created    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    status     TEXT DEFAULT 'active', -- active, paused, archived
    config     TEXT                   -- JSON
);

CREATE TABLE matches (
    id             TEXT PRIMARY KEY,
    world_template TEXT,              -- four-word name of template
    instance_id    TEXT,
    players        TEXT,              -- JSON array
    started        TIMESTAMP,
    status         TEXT DEFAULT 'lobby'
);

CREATE TABLE dictionary (
    word       TEXT PRIMARY KEY,
    category   TEXT,                  -- noun, adjective
    syllables  INTEGER
);
```

### 12.3 REST API

```
GET  /resolve/{w1}.{w2}.{w3}.{w4}   → { server, world_id, transport, tick_rate }
POST /register                        → { name, server, owner, transport }
GET  /browse                          → { category, sort, page }
POST /publish                         → { name, assets[] }
POST /matchmake                       → { game_type, player_id, region }
GET  /match/{match_id}                → { status, players, instance }
GET  /health/{world_id}               → { tick_rate, players, uptime }
```

### 12.4 Player Identity

- **Anonymous:** Random four-word player name
- **Telegram:** Uses Telegram username
- **Persistent:** Optional account for ELO, stats, achievements

---

## 13. Matchmaking and Instance Management

### 13.1 Queue-Based Matchmaking

```
Player → selects game mode → enters queue
  → Matchmaker groups by ping region + skill (ELO ±200)
  → Server spins up world instance
  → Players receive four-word address
  → On match end → results → back to queue or exit
  → Instance torn down after disconnect + 60s grace
```

### 13.2 ELO/MMR

- New players: 1000 ELO
- Win: +25 × (1 - expected_win_probability)
- Loss: -25 × expected_win_probability

### 13.3 Instance Management

- Each match = fresh world instance (~50-100 MB memory)
- Instance pool with warm-start (pre-loaded scenes) for <3s match start
- A single 4-vCPU server runs ~20 concurrent HAVOC matches or ~5 DROPZONE matches

---

## 14. Proof Games (Platform Perspective)

### 14.1 Snake Sweeper

| Attribute | Value |
|-----------|-------|
| Transport | WebSocket |
| Tick rate | 20 Hz |
| Players | 1-4 |
| Bandwidth/client | ~16 KB/s |
| Server memory | ~50 MB |
| Platform features exercised | World registry, WebSocket transport, Telegram bridge, AI Director |

### 14.2 HAVOC

| Attribute | Value |
|-----------|-------|
| Transport | WebRTC DataChannel |
| Tick rate | 60 Hz |
| Players | 1-4 co-op |
| Bandwidth/client | ~360 KB/s |
| Server memory | ~100 MB |
| Platform features | WebRTC, prediction reconciliation, horde authority, interest mgmt, AI Director, matchmaking |

### 14.3 DROPZONE (Battle Royale)

| Attribute | Value |
|-----------|-------|
| Transport | WebRTC DataChannel |
| Tick rate | 30 Hz (100 players) → 60 Hz (<50 alive) |
| Players | 100 (solos) / 25 squads of 4 |
| Map | 2km × 2km, 32×32 chunks |
| Bandwidth/client | ~300 KB/s |
| Server memory | ~500 MB |

**What DROPZONE adds to the platform:**

| System | Description |
|--------|-------------|
| Storm system | Shrinking circle, server-authoritative position/radius/damage |
| Loot spawner | YAML loot tables → spawn entities at predefined positions |
| Bus/dropship | Moving entity, player detach, parachute physics |
| Spectator mode | Camera follows another player post-elimination |
| 100-player lobby | Matchmaker queues 100, countdown, hot-start instance |
| Dynamic tick rate | 30 Hz with 100 players → 60 Hz when <50 alive |
| Building (stretch) | Grid-snapped placement, material harvesting, structure health |

**Loot tables:**
```yaml
loot_table:
  floor_spawn:
    common:     { prob: 0.40, items: [pistol, smg, bandages] }
    uncommon:   { prob: 0.30, items: [assault_rifle, shotgun, shield_cells] }
    rare:       { prob: 0.20, items: [sniper_rifle, rocket_launcher, med_kit] }
    epic:       { prob: 0.08, items: [scar, heavy_shotgun, chug_jug] }
    legendary:  { prob: 0.02, items: [gold_scar, mythic_item] }
```

**DROPZONE performance targets:**

| Metric | Target |
|--------|--------|
| Players | 100 |
| Map | 2km × 2km |
| Server tick rate | 30-60 Hz |
| Bandwidth/client | <300 KB/s |
| Server memory/match | ~500 MB |
| Match duration | ~20 min |
| Match start time | <5s from lobby fill |

---

## 15. Licensing

### 15.1 BSL 1.1 (Business Source License)

`naive-server` is licensed under BSL 1.1.

**What you CAN do:**
- Self-host your own game servers using naive-server
- Modify the server code for your games
- Use it in commercial production
- Run servers for your studio, tournaments, private communities
- Read the source, learn from it, contribute to it

**What you CANNOT do:**
- Offer nAIVE world hosting as a commercial service that competes with the nAIVE platform
- Specifically: "managed hosting where customers upload nAIVE worlds and you charge for hosting them"

**The Additional Use Grant:** The specific restriction is: you may not use this software to provide a Managed nAIVE World Hosting Service (a service where third parties upload nAIVE world definitions and you host and serve them for a fee).

**3-year change date:** BSL 1.1 converts to MIT automatically three years after each release. Code released in 2026 becomes MIT in 2029. This is baked into the license text — it's not a promise, it's a legal obligation.

### 15.2 Why BSL (Not MIT)

The server contains the platform infrastructure — the world registry, matchmaking, AI Director, and hosting orchestration. Without protection, a well-funded competitor could:

1. Fork the server
2. Build a competing "nAIVE Cloud" hosting service
3. Undercut the nAIVE platform on price (subsidized by their other business)
4. Capture the network effects that the nAIVE community built

BSL prevents this specific scenario while allowing every legitimate use:

| Use Case | Allowed? |
|----------|----------|
| Self-host your game's servers | Yes |
| Modify server code for your game | Yes |
| Run a tournament server | Yes |
| Use in commercial production | Yes |
| Start a "nAIVE World Hosting" business | No |

The 3-year conversion ensures the code becomes fully open eventually.

### 15.3 CLA Requirements

Contributors to `naive-server` sign a Contributor License Agreement granting the project the right to relicense. This ensures the 3-year MIT conversion isn't blocked.

### 15.4 Prior Art

Other BSL projects: MariaDB, CockroachDB, HashiCorp (Terraform), Sentry. The model is well-understood by the open-source community.

---

## 16. Infrastructure

### 16.1 Early Days: Beelink + Cloudflare

The platform starts on minimal hardware:

```
Beelink EQ14 Mini PC
  CPU: Intel N150 (up to 3.6 GHz)
  RAM: 16 GB DDR4
  Storage: 500 GB NVMe + 2 TB M.2
  Network: 1 Gbps dual LAN

  Running: naive-server + cloudflared tunnel
  Capacity: ~50 concurrent worlds (Snake Sweeper class)
             ~10 concurrent HAVOC matches
```

**Cloudflare stack:**
- **cloudflared tunnel:** Already running. Routes traffic to Beelink without exposing IP.
- **Workers:** Registry API, auth, rate limiting
- **D1:** World registry database (SQLite-compatible)
- **R2:** Asset CDN (scene files, meshes, audio, textures)
- **Pages:** Web portal, WASM client hosting at `play.naive.world`

**Monthly cost:** ~$15-40 (Cloudflare free tier + paid Workers for scale)

### 16.2 Growth: VPS

When the Beelink is saturated:
- 4 vCPU, 8 GB RAM VPS: ~$40/month → ~100 worlds, ~20 HAVOC matches
- 8 vCPU, 16 GB RAM: ~$80/month → ~200 worlds, ~5 DROPZONE matches

### 16.3 Scale: Horizontal

Multiple server instances behind load balancer. World migration between servers for balancing. Cloudflare Workers route clients to the correct regional server.

---

## 17. Server Technical Specifications

### 17.1 Server Requirements

| Resource | Per World (Snake Sweeper) | Per World (HAVOC) | Per Match (DROPZONE) |
|----------|--------------------------|-------------------|----------------------|
| CPU | 1 core | 2 cores | 4 cores |
| Memory | ~50 MB | ~100 MB | ~500 MB |
| Network/client | ~16 KB/s | ~360 KB/s | ~300 KB/s |

### 17.2 Capacity Planning

| Hardware | Snake Sweeper Worlds | HAVOC Matches | DROPZONE Matches |
|----------|---------------------|---------------|------------------|
| Beelink EQ14 (N150, 16 GB) | ~50 | ~10 | ~2 |
| 4-vCPU VPS (8 GB) | ~100 | ~20 | ~5 |
| 8-vCPU VPS (32 GB) | ~200 | ~40 | ~10 |

---

## 18. Implementation Roadmap (Server)

**Phase 1 (Weeks 1-4): Headless Server + WebSocket**

| Week | Deliverable |
|------|------------|
| 1-2 | Feature-flagged Cargo build: `naive-server` compiles without wgpu |
| 3 | WebSocket state streaming: server broadcasts deltas |
| 4 | World registry: SQLite, four-word name resolution, REST API |

**Phase 2 (Weeks 5-8): Multiplayer + Telegram**

| Week | Deliverable |
|------|------------|
| 5-6 | Multiplayer Snake Sweeper: 2-4 players on shared minefield |
| 7 | Telegram bridge v2: AI-parsed NL commands |
| 8 | AI Director v1: adaptive difficulty for Snake Sweeper |

**Phase 3 (Weeks 9-16): Fast Netcode**

| Weeks | Deliverable |
|-------|------------|
| 9-10 | WebRTC DataChannel transport layer |
| 11-12 | Server-side prediction acknowledgment + lag compensation |
| 13-14 | Interest management: grid-based spatial partitioning |
| 15-16 | Matchmaking: queue service, ELO, instance management |

**Phase 4 (Weeks 17-24): HAVOC Server**

| Weeks | Deliverable |
|-------|------------|
| 17-18 | Horde grid simulator (simplified server-side horde authority) |
| 19-20 | Server-authoritative weapon hit detection + lag compensation |
| 21-22 | AI Director v2: HAVOC wave adaptation |
| 23-24 | LLM NPC runtime: tiered inference for boss encounters |

**Phase 5 (Weeks 25-32): DROPZONE Server**

| Weeks | Deliverable |
|-------|------------|
| 25-26 | 100-player instance management, dynamic tick rate |
| 27-28 | Storm system (server-authoritative shrinking circle) |
| 29-30 | Loot spawner, supply drops, spectator mode |
| 31-32 | Building system (stretch), 100-player load testing |

**Phase 6 (Weeks 33-40): Platform**

| Weeks | Deliverable |
|-------|------------|
| 33-34 | Asset CDN via Cloudflare R2 |
| 35-36 | Web portal: world browser at naive.world |
| 37-38 | World forking, permissions (public/private/invite) |
| 39-40 | Analytics dashboard, platform monitoring |

---

## 19. Risk Assessment

| Risk | Severity | Mitigation | Fallback |
|------|----------|-----------|----------|
| WebRTC complexity | Medium | Use webrtc-rs crate, HTTPS signaling | WebSocket with prediction for moderate-speed games |
| 100-player scaling (DROPZONE) | High | Interest management, dynamic tick rate | Cap at 50 players initially |
| Horde desync between clients | Low | Deterministic compute shaders, periodic correction snapshots | Invisible at 50K — 0.5m difference unnoticeable |
| LLM API cost at scale | High | Tiered inference: Tier 3 cache handles 60%+ at zero cost | Behavior trees with LLM-generated cache |
| BSL license misunderstanding | Medium | Clear docs, FAQ, examples | Community outreach |
| Beelink hardware limits | Low | VPS migration path at $40/month | Cloudflare Workers for stateless components |
| Four-word squatting | Low | 16 trillion address space | Inactive worlds archived after 90 days |
| Telegram bot rate limits | Low | Command queue, 1 cmd/sec/user | Direct WebSocket API as alternative |

---

## 20. Success Metrics

### 20.1 Technical Metrics

| Metric | Target |
|--------|--------|
| Server tick rate (Snake Sweeper, 4 players) | 20 Hz sustained |
| Server tick rate (HAVOC, 4 players) | 60 Hz sustained |
| Server tick rate (DROPZONE, 100 players) | 30 Hz sustained |
| State delta latency (WebRTC) | <15ms |
| Match start time (warm instance) | <3s |
| Concurrent worlds (Beelink) | 50+ |
| Concurrent HAVOC matches (4-vCPU VPS) | 20+ |
| Telegram command → visible change | <1 second |
| NPC response (Tier 1) | <500ms |
| NPC response (Tier 3) | <5ms |

### 20.2 Experience Metrics

- Snake Sweeper: 4 players with no perceptible desync
- HAVOC: 4 players co-op with smooth gameplay
- DROPZONE: 100 players competitive with fair hit registration
- Telegram "/command" changes live world within 1 second
- Sharing a four-word address is the only step to join
- AI Director interventions rated "helpful" or "invisible" by >70%
- NPC conversations feel meaningfully different across sessions

---

## Appendix A: Glossary

| Term | Definition |
|------|-----------|
| **Four-word address** | Human-memorable world ID: `black.squirrel.white.deer` |
| **State delta** | Compact update with only changed entity components |
| **Server authority** | Server computes all state; clients only render |
| **Tick rate** | Server updates per second (10-60 Hz) |
| **AI Director** | Server-side system that monitors and adapts worlds |
| **Interest management** | Spatial culling to limit per-client bandwidth |
| **Lag compensation** | Server rewinds state to validate hits at client's view time |
| **Horde authority** | Server tracks aggregate horde state, not individual positions |
| **BSL 1.1** | Business Source License — all use except competing hosting service |
| **Change date** | 3 years after release, BSL converts to MIT automatically |
| **WebRTC DataChannel** | Browser-native UDP-like transport for low-latency networking |
| **ELO/MMR** | Skill rating for matchmaking |
| **Instance** | A fresh world spun up for a single match |
| **Flow field** | Server-computed pathfinding grid sent to clients for horde sim |
| **naive-client** | MIT crate: renderer, audio, input, physics, scripting, all engine modules |
| **naive-core** | MIT crate: shared types, components, scene format, serialization |
| **naive-runtime** | MIT crate: single-process game binary (thin CLI that uses naive-client) |
| **naive-server** | BSL crate: world manager, transport, registry, Director, matchmaking |
