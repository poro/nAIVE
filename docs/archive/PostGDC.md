# Post-GDC Development Plan (March 14 → May 2026)

**Goal:** Ship Tiers 3–5 and have a "holy shit" demo ready for ASU+GSV (April) and Xbox Game Camp (late May).

---

## Phase 1: Tier 3 — GPU Scale (March 14–31)

**The flagship differentiator.** 50K entities on GPU is what separates nAIVE from "another indie engine."

### Milestone 1: GPU Compute Entity Buffer (~3 days)

**New file:** `crates/naive-client/src/gpu_horde.rs`

- Storage buffer management: entity struct (position, velocity, health, anim_time, state)
- Atomic free-list for spawn/death slot management
- Start with 4,096 entities, test buffer resize up to 65,536
- SLANG compute shader for basic flocking (separation only, no flow field yet)

**Key files to modify:**
- `crates/naive-client/src/engine.rs` — add GPU dispatch hooks to game loop
- `project/shaders/` — new `compute/horde_sim.slang`

**Risk:** SLANG compute → WGSL cross-compilation for storage buffers (HIGH)
- Test early with a minimal compute shader
- SLANG's WGSL backend may have storage buffer quirks with read-write storage
- **Fallback:** write the horde sim in raw WGSL if SLANG chokes

### Milestone 2: Neighbor Grid (~2 days)

- GPU spatial hash: bin entities into grid cells via atomic scatter
- Query API for compute shaders to iterate neighbors within a radius
- Entity-entity collision on GPU (key for horde behavior — avoidance, attack range)

**Implementation:**
- Grid cell size = 2× entity radius
- Hash: `cell = floor(pos / cell_size)`, linearize to 1D index
- Scatter pass: each entity atomically increments its cell's counter and writes its index
- Query: iterate cell + 26 neighbors (3×3×3 grid)

### Milestone 3: Flow Field Pathfinding (~2 days)

- 128×128 GPU wavefront propagation from target position
- Each cell stores a direction vector pointing toward the target
- Entities sample the flow field for steering
- Recalculate only when player moves >2 cells (not every frame)

**Implementation:**
- BFS wavefront expansion via ping-pong compute passes
- Cost field for obstacles (walls = impassable, terrain = weighted)
- Direction = normalize(lowest-cost-neighbor - current_cell)

### Milestone 4: GPU Instanced Rendering (~2 days)

- Read instance transforms directly from compute output buffer (GPU→GPU, no readback)
- Single draw call per mesh type for all 50K entities
- Integrate with existing dynamic instance buffer in `renderer.rs`

**Validation demo:** `naive demo horde` — 50K cubes chasing the player at 60 FPS.

**Risk:** GPU readback latency for damage/death events (MEDIUM)
- Use a small event buffer (just player-contact events), not the full entity buffer
- Async readback with 1-frame delay is acceptable
- Only read back: entity_id, damage_amount, death flag

### Target: March 31
- [ ] 50K entities rendering at 60 FPS
- [ ] Flow field pathfinding working
- [ ] Entities chase player and avoid each other
- [ ] LinkedIn/Twitter post with demo video

---

## Phase 2: Tier 4 — Animation (April 1–14)

### Milestone 5: Skeletal Animation (~5 days)

**New file:** `crates/naive-client/src/animation.rs`

- glTF skinned mesh loading (joints, weights, inverse bind matrices)
- Animation clip parsing and playback (position, rotation, scale keyframes)
- YAML-defined state machine:
  ```yaml
  animation:
    states:
      idle: { clip: "idle.glb", loop: true }
      run: { clip: "run.glb", loop: true }
      attack: { clip: "attack.glb", loop: false }
    transitions:
      - from: idle, to: run, condition: "speed > 0.1"
      - from: run, to: idle, condition: "speed < 0.1"
      - from: any, to: attack, condition: "event:attack"
  ```
- GPU skinning via vertex shader (joint matrices as uniform buffer)

**Test:** Load a free Mixamo character (FBX → glTF via Blender), play idle/run/attack.

### Milestone 6: Vertex Animation Textures (VAT) (~4 days)

**The bridge between skeletal animation and GPU scale.**

- Offline baking tool: glTF skeleton animation → `.exr` texture atlas
  - Each row = one frame
  - Each pixel = vertex position delta (R16G16B16A16Float)
  - Normal map as second texture
- Runtime vertex shader: sample VAT based on `anim_time` from compute buffer
- Integrate with GPU instanced rendering from Tier 3

**Risk:** EXR texture loading in wgpu (MEDIUM)
- wgpu supports `R16G16B16A16Float` format
- Need to verify: `.exr` → raw float buffer → `wgpu::Texture` upload path
- **Fallback:** use `.ktx2` or raw binary format instead of `.exr`

**Validation:** 50K animated zombies chasing the player — different animation phases per entity.

### Target: April 14
- [ ] Skeletal animation working on single characters
- [ ] VAT baking tool produces texture atlases
- [ ] 50K animated entities via VAT + GPU compute
- [ ] State machine transitions (idle → run → attack)

---

## Phase 3: Gaussian Splatting Integration (April 14–21)

### Milestone 7: Production Splat Rendering (~3 days)

Upgrade from basic splat demo to production-quality rendering.

- Pull in `wgpu-3dgs-viewer` as a crate dependency (Rust/wgpu, available on crates.io)
- Replace current basic splat rendering with the full viewer library
- Support `.ply` and `.spz` file formats
- Multi-model support: load multiple splat files in one scene
- Selection/picking for editor integration

### Milestone 8: Scene Beautification Pipeline (~4 days)

The "wow" feature — turn blocky scenes into photorealistic environments.

- **`naive export`** CLI: scene meshes → combined GLB file
- **Send to World Labs / Marble**: GLB → photorealistic Gaussian splat
- **`naive import-splat`** CLI: `.ply` → positioned in scene
- **Hybrid rendering**: invisible collision meshes + visible splat overlay
- **`naive beautify`** CLI: one-step export → generate → import

**Scene YAML:**
```yaml
entities:
  - id: forest
    components:
      gaussian_splat:
        file: "assets/splats/forest.ply"
        scale: 1.0
      collider:  # invisible, for physics
        shape: trimesh
        mesh: "assets/meshes/forest_collision.glb"
```

### Target: April 21
- [ ] `wgpu-3dgs-viewer` integrated as rendering backend
- [ ] Scene beautification pipeline working end-to-end
- [ ] Hybrid collision mesh + splat rendering
- [ ] `naive beautify` CLI command functional

---

## Phase 4: Tier 5 — Vehicles & HAVOC (April 21 → May)

### Milestone 9: Vehicle Physics (~3 days)

- Rapier3D wheel joints with spring-damper suspension
- Steering: front-axle Ackermann geometry
- Throttle/brake force application to wheel bodies
- Simple tire friction model (longitudinal + lateral slip)

**YAML:**
```yaml
vehicle:
  chassis_mass: 1500
  wheels:
    - position: [-0.8, -0.3, 1.2]
      radius: 0.35
      suspension: { rest: 0.5, stiffness: 30000, damping: 4000 }
      steering: true
      drive: true
```

### Milestone 10: Mount/Dismount System (~2 days)

- Player presses E near vehicle → reparent to seat bone/position
- Switch input context: FPS controls → vehicle controls
- Camera transitions: 3rd-person follow → vehicle chase cam
- Dismount: reverse the process, place player beside vehicle

### Milestone 11: LOD System (~2 days)

- Distance-based mesh switching (3 LOD levels)
- Smooth crossfade between LODs (alpha dither)
- Integrate with GPU instanced rendering: LOD selection per instance
- Automatic LOD generation from high-poly mesh (mesh decimation)

### Milestone 12: HAVOC Proof Game (~5 days)

**The showcase.** Everything comes together:

- 50K GPU zombie horde chasing the player (Tier 3)
- Animated zombies with attack/death animations (Tier 4 VAT)
- Photorealistic environment via Gaussian Splatting (Phase 3)
- Vehicle combat: mounted turret, drive-through hordes
- Wave-based survival with increasing difficulty
- Score tracking, health system, ammo/overheat

**Target:** A 3-minute gameplay video that demonstrates every engine capability.

### Target: Late May
- [ ] Vehicle physics and mount/dismount working
- [ ] LOD system reducing draw calls at distance
- [ ] HAVOC playable for 5+ minutes
- [ ] Demo video ready for Xbox Game Camp

---

## Event Milestones

| Date | Event | What to Show |
|------|-------|-------------|
| **Mar 9–13** | GDC | 60-sec demo video (Pre-GDC plan) |
| **Mar 31** | — | 50K GPU entities demo (LinkedIn post) |
| **April (TBD)** | ASU+GSV Panel | Animated horde + splat environments |
| **Late May** | Xbox Game Camp | HAVOC full gameplay demo |

---

## Technical Architecture

```
┌─────────────────────────────────────────────────────┐
│                    Game Loop                         │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────┐ │
│  │  Input    │→ │  Physics  │→ │  GPU Compute      │ │
│  │  (gilrs)  │  │ (Rapier)  │  │  (horde sim)      │ │
│  └──────────┘  └──────────┘  └───────────────────┘ │
│       │              │              │                │
│       ▼              ▼              ▼                │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────┐ │
│  │  Lua      │  │  ECS     │  │  Flow Field       │ │
│  │  Scripts  │  │  (hecs)  │  │  (GPU pathfind)   │ │
│  └──────────┘  └──────────┘  └───────────────────┘ │
│                      │                               │
│                      ▼                               │
│  ┌─────────────────────────────────────────────────┐ │
│  │              Render Pipeline                     │ │
│  │  ┌─────────┐ ┌──────────┐ ┌──────────────────┐ │ │
│  │  │ GBuffer │→│ Lighting │→│ Gaussian Splats  │ │ │
│  │  │ Pass    │ │ Pass     │ │ (wgpu-3dgs)      │ │ │
│  │  └─────────┘ └──────────┘ └──────────────────┘ │ │
│  │  ┌─────────┐ ┌──────────┐ ┌──────────────────┐ │ │
│  │  │ Shadows │ │ Particles│ │ GPU Instanced    │ │ │
│  │  │         │ │          │ │ (50K entities)   │ │ │
│  │  └─────────┘ └──────────┘ └──────────────────┘ │ │
│  └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

---

## Key Files Reference

| File | What Goes Here |
|------|---------------|
| `crates/naive-client/src/gpu_horde.rs` | **NEW** — GPU compute entity management |
| `crates/naive-client/src/animation.rs` | **NEW** — Skeletal animation + VAT |
| `crates/naive-client/src/vehicle.rs` | **NEW** — Vehicle physics + mount system |
| `crates/naive-client/src/lod.rs` | **NEW** — LOD switching + mesh decimation |
| `crates/naive-client/src/engine.rs` | Game loop — add GPU dispatch + animation tick |
| `crates/naive-client/src/renderer.rs` | GPU instanced rendering path |
| `crates/naive-client/src/mesh.rs` | GLB loading (recently expanded) |
| `project/shaders/compute/horde_sim.slang` | **NEW** — Horde simulation compute shader |
| `project/shaders/compute/flow_field.slang` | **NEW** — Flow field generation |
| `project/shaders/compute/neighbor_grid.slang` | **NEW** — Spatial hash grid |
| `project/shaders/vertex/vat.slang` | **NEW** — VAT vertex shader |
| `docs/nAIVE_Engine_PRD_v5.0.md` | The bible — Sections 8 + 6.3 |
| `tools/vat-baker/` | **NEW** — Offline VAT baking tool |
