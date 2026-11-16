# Tier 4 plan: GPU-scale entities

**Status:** approved on 2026-11-16. Milestone 0 is in pull request #11; milestones 1 to 5 are not started.
**Goal:** 50,000 horde entities on screen at 60 fps, as the roadmap and the HAVOC proof game call for.
**Source spec:** section 8 of [the engine PRD](nAIVE_Engine_PRD_v5.0.md), which this plan narrows into steps.

## Where the engine is today

Measured on a release build of 0.1.20, Apple Silicon MacBook Pro, 120 Hz display. Each row is a scene with that many entities and nothing else going on.

| Entities | Plain meshes | Physics bodies (Rapier) |
|---|---|---|
| 1,000 | 106 fps | 99 fps |
| 2,000 to 2,500 | 43 fps | 47 fps |
| 4,000 to 5,000 | 17 fps | 18 fps |
| 10,000 | 1.7 fps | not measured |

So the engine falls below 60 fps at about 2,000 entities and collapses by 10,000.

A profile at 5,000 entities showed why, and it was not what the roadmap assumed:

- **Draw calls are cheap.** Issuing them was about 1% of the frame.
- **One wasted upload dominated.** The shadow and G-buffer passes wrote an 8 KB bone-matrix buffer for every entity, skinned or not, and each write allocated a GPU staging buffer. At 5,000 entities that is 10,000 allocations and 82 MB of uploads per frame. The writes had no visible effect, because only the last one in a frame counts.

Removing that upload is milestone 0 below. With it:

| Entities | Plain meshes | Physics bodies |
|---|---|---|
| 2,000 to 2,500 | 120 fps | 116 fps |
| 4,000 to 5,000 | 77 fps | 63 fps |
| 8,000 to 10,000 | 37 fps | 20 fps |
| 20,000 | 17 fps | not measured |
| 40,000 | 7.6 fps | not measured |

After the fix the cost is linear, about 2.7 microseconds per entity per frame, which puts the CPU path's 60 fps ceiling near 6,000 entities. Getting to 50,000 needs the two things the roadmap names: instanced drawing and simulation on the GPU.

## Design

### Two kinds of entity

- **ECS entities** stay as they are: players, bosses, props, projectiles. Scripts address each one. A few thousand at most.
- **A swarm** is one ECS entity carrying a `gpu_swarm` component. Its members live only in GPU buffers. Scripts talk to the swarm, never to a member.

This keeps the scene format and the Lua model intact, and it is the split the PRD describes.

### Rendering

One instanced draw per swarm per pass (shadow and G-buffer), reading member transforms from the buffer the simulation writes. The same instancing path, fed from the ECS, replaces one-draw-per-entity for ordinary entities that share a mesh and material.

### Simulation

A compute pass per frame over a buffer of members (position, velocity, health, state, type):

1. **Steer** toward the target by sampling a flow field.
2. **Separate** from neighbours using a spatial grid.
3. **Integrate**, clamp to the world bounds, apply damage, mark the dead.

Choices, each made to keep the first version simple and inside WebGPU's default limits:

- **Neighbour grid with fixed-size cells.** Each cell holds up to a fixed number of member indices, filled with an atomic counter; overflow is dropped. One pass, no prefix sum or sort. Crowding beyond the cell capacity loses some separation, which a horde hides.
- **Flow field on the CPU.** A 128 by 128 grid solved from the target's position around static obstacles, uploaded as a texture when the target changes cell. It is 16,384 cells, which is cheap.
- **No Rapier for members.** The measurements above work out to roughly 0.6 microseconds per body per physics step, so 50,000 bodies would cost about 30 ms per step. Members collide with each other through the grid and with the world through the flow field's obstacles.

### Talking to the rest of the game without stalling

Reading member data back every frame would stall the GPU, so nothing does.

- **Game to swarm:** damage volumes (spheres, capsules, rays) are passed in a small uniform array each frame. Weapons, explosions and vehicles all become volumes.
- **Swarm to game:** the shader accumulates counters (kills, hits on the player) in a small buffer that is read back asynchronously, one or two frames late, and turned into events.

Lua surface, all at swarm level:

```lua
swarm.spawn("goblins", 500, x, y, z, radius)
swarm.set_target("goblins", x, y, z)
swarm.damage_sphere("goblins", x, y, z, radius, damage)
events.on("swarm.kills", function(e) ... end)
events.on("swarm.player_hit", function(e) ... end)
```

### What has to be built in the engine

- `PassType::Compute` in the pipeline. Today the compiler rejects it and the executor skips it.
- Compute entry points in the shader path. Today only `vs_main` and `fs_main` are handled. Each compute shader also gets a hand-written WGSL fallback, as the other passes have, so it works without SLANG and on the web.
- Instanced pipelines for the shadow and G-buffer passes.

### Testing

The headless test runner has no GPU, so the compute path cannot run in `naive test` or in CI as it stands. The plan is a CPU reference implementation of one simulation step in Rust, with the same maths, covered by unit tests (separation keeps spacing, members converge along the flow field, damage volumes kill what they touch). The GPU path is checked by a benchmark scene and by eye.

## Milestones

Each one ends with a number or a scene to look at.

| | Milestone | Done when | Size |
|---|---|---|---|
| 0 | Stop the per-entity bone upload | Measured above. Pull request #11 | Done |
| 1 | Instanced drawing for ECS entities that share a mesh and material | Benchmark table re-run; three demos look unchanged | Medium |
| 2 | Compute pass plumbing and the `gpu_swarm` component: members move toward a target, drawn instanced | 50,000 moving cubes at 60 fps in a new `tier4_swarm` scene | Large |
| 3 | Neighbour grid and flow field | Members spread out instead of stacking and path around obstacles | Large |
| 4 | Damage volumes, contact damage, death, spawn waves, the Lua API and events | A HAVOC wave 1 to 3 prototype: 1,000 to 5,000 members you can fight | Large |
| 5 | Scale-up and tidy | 50,000 members at 60 fps on this MacBook; README's Tier 4 row marked done | Medium |

Milestone 1 comes before any compute work because swarms need instancing anyway, and it lifts the ceiling for ordinary entities on the way. How far it lifts it is not known until measured.

## Not in Tier 4

- **Animation.** Members are rigid meshes. Vertex animation textures are Tier 5 on the roadmap.
- **Level of detail and billboards.** The PRD ties these to vertex animation.
- **Multiplayer.** Swarm state is local to one machine.
- **The dependency upgrades** (`wgpu`, `rapier3d`). Tier 4 is built on `wgpu` 24.

## Risks

- **Nobody is looking at the screen but you.** I can measure frame rates and catch crashes, but each milestone needs you to run a scene for a minute and say it looks right.
- **SLANG compute shaders are untried here.** If the compile path fights back, the WGSL fallback carries the work and SLANG support follows later.
- **The GPU path is outside CI.** The CPU reference covers the maths, not the shader.
- **Projectiles no longer "hit" horde members physically.** They become rays or small damage volumes. Anything that relied on a Rapier contact with a goblin has to be expressed that way.
- **The 50,000 figure is a target, not a measurement.** The grid pass is the likely bottleneck, and its cost is unknown until milestone 3.

## Decisions

Confirmed with the plan's approval on 2026-11-16:

1. **Swarm members are not individually scriptable.** Scripts get the swarm-level API above. Anything that needs its own script (a boss, an elite with special behaviour) stays an ECS entity.
2. **No animation in Tier 4.** Rigid meshes until Tier 5.
3. **Milestone 0 ships on its own**, ahead of the rest.
