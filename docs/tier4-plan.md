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

Milestone 1 (instanced drawing) removed that per-entity cost: entities sharing a mesh and albedo texture are now one draw, and all draw data goes up in one upload per frame. Same scenes and machine:

| Entities | Plain meshes | Physics bodies |
|---|---|---|
| 2,000 to 2,500 | 120 fps | 120 fps |
| 4,000 to 5,000 | 120 fps | 120 fps |
| 8,000 to 10,000 | 120 fps | 120 fps |
| 20,000 | 120 fps | not measured |
| 40,000 to 50,000 | 120 fps | not measured |
| 80,000 | 77 fps | not measured |
| 120,000 | 54 fps | not measured |
| 160,000 | 39 fps | not measured |

120 fps is the display's refresh rate, the cap. The plain-mesh ceiling for 60 fps is now about 100,000 static entities. What remains per entity is the CPU work of gathering each one's matrix and material every frame, which a GPU-side swarm does not need.

The physics column needs a caution: the benchmark's balls settle and Rapier puts them to sleep, so it measures sleeping bodies. Its old numbers were limited by drawing, not physics.

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

As built in milestone 3 (`crates/naive-client/src/swarm.rs`):

- **Two passes per swarm per frame.** `bin_main` files members into the neighbour grid (cells at least `spacing` wide, 8 members each, counts cleared every frame). `steer_main` reads the 3 by 3 cells around each member. Members are double-buffered, so every member reads the same frame's positions.
- **Separation comes first.** Each neighbour closer than `spacing` pushes; the summed closeness of all neighbours (not the net push, which cancels inside a crowd) damps how hard a member presses toward the target. Without that, a crowd around a target compressed to half its spacing in the tests; with it, it keeps its spacing.
- **Obstacles come from the physics world.** Any fixed collider whose box crosses the members' plane is a wall: the floor under their feet is not, a wall is, a beam overhead is not. Solved with Dijkstra on an 8-connected grid without corner cutting, re-solved when the target changes cell or the obstacles change. A wall is stored as blocked cells; members slide along them and never step into one (one that starts inside can walk out).
- **The swarm's square.** The flow field and grid cover a square of `field_size` (default 256 m, so 2 m flow cells) centred where the swarm spawned; members are clamped inside it. Members steer straight at the target within two flow cells of it, or where the field has no route.
- Flow directions come from an 8-direction grid and are blended between cells, so paths are a little angular; good enough for a horde.
- **No Rapier for members.** Members are always moving, so Rapier could never put them to sleep, and stepping 50,000 awake bodies on the CPU every fixed step is far beyond the frame budget. (An earlier draft put a number on this from the physics benchmark; that benchmark measures sleeping bodies, so the number did not hold.) Members collide with each other through the grid and with the world through the flow field's obstacles.

### Talking to the rest of the game without stalling

Reading member data back every frame would stall the GPU, so nothing does.

- **Game to swarm:** damage volumes (spheres, capsules, rays) are passed in a small uniform array each frame. Weapons, explosions and vehicles all become volumes.
- **Swarm to game:** the shader accumulates counters (kills, hits on the player) in a small buffer that is read back asynchronously, one or two frames late, and turned into events.

Lua surface, all at swarm level (as built in milestone 4; `docs/GAME_DEVELOPMENT_GUIDE.md` has the reference):

```lua
swarm.spawn("goblins", 500, x, y, z, radius)
swarm.set_target("goblins", x, y, z)       -- or swarm.follow("goblins", "player")
swarm.damage_sphere("goblins", x, y, z, radius, damage)
swarm.damage_line("goblins", x1, y1, z1, x2, y2, z2, radius, damage)
swarm.alive("goblins"); swarm.capacity("goblins"); swarm.kills("goblins")
events.on("swarm.kills", function(e) ... end)    -- e.data.swarm, count, alive
events.on("swarm.hit", function(e) ... end)      -- e.data.swarm, target, hits, damage
events.on("swarm.cleared", function(e) ... end)  -- e.data.swarm
```

As built in milestone 4 (`swarm.rs`, `swarm_api.rs`, `sim::process_swarm_reports`):

- **`count` is a pool.** Each member has health; one at zero or below is dead: it is not binned, pushes nobody and is drawn at zero size. `initial` says how many start alive. `swarm.spawn` brings dead slots back: dead members claim spawn slots with an atomic counter, and spawn number n of the swarm decides where its member lands in the disc, so each wave lands somewhere new.
- **Damage volumes** are segments with a radius (a sphere is a segment whose ends meet), up to 64 per swarm per frame in its uniform buffer; more wait for the next frame. A line hurts every member along it, like a piercing shot; a first-hit ray is not built. Hurt members glow for 0.12 s.
- **Contact damage.** With `attack_damage` above zero, members attack the target entity from within `attack_range`, every `attack_interval`. They stop at 0.8 of the range, and the target's space pushes members out three times as hard as a neighbour does, so the crowd behind cannot squeeze the front rank onto the player.
- **Counters, read back without waiting.** Each swarm's step leaves four counters in a 16-byte buffer: kill and attack totals, and this step's living members and spawn claims. The buffer is copied to one of three mappable buffers and read when the GPU is done, a frame or two later; a frame with no free buffer skips its copy and the running totals catch up. Reports become damage on the target (with `on_damage`, the swarm's id as the source) and `swarm.kills`, `swarm.hit` and `swarm.cleared` events. `swarm.alive` adds spawns requested since the last report, so a wave counts as soon as it is asked for.

### What has to be built in the engine

- Instanced pipelines for the shadow and G-buffer passes. Done in milestone 1: both passes read per-entity data from a storage buffer indexed by instance.
- The swarm compute step. Done in milestone 2, differently from this plan's first draft: the swarm system (`crates/naive-client/src/swarm.rs`) owns its compute pipeline and dispatches it once per frame, before rendering, instead of being a `PassType::Compute` pass in the project's render pipeline file. Simulation is engine logic rather than a render pass a project should configure, existing projects need no pipeline changes, and a frame capture renders the scene again without stepping the swarm a second time. `PassType::Compute` stays unsupported until something needs it. The shader is hand-written WGSL; SLANG is not involved.
- Each swarm writes its members' draw data (the same 256-byte entries the instanced shaders read) into its own buffer, and is drawn with one instanced draw per pass using that buffer. One binding holds at most the device's storage-binding size, 128 MB by default, so a swarm is capped at 524,288 members; larger counts are clamped with a warning.

### Testing

The headless test runner has no GPU, so the compute path cannot run in `naive test` or in CI as it stands. The plan is a CPU reference implementation of one simulation step in Rust, with the same maths, covered by unit tests (separation keeps spacing, members converge along the flow field, damage volumes kill what they touch). The GPU path is checked by a benchmark scene and by eye.

As built:

- `swarm::step_swarm` is the CPU reference of both passes, with unit tests: members spread out instead of stacking (nearest neighbours above 0.4 of the spacing, median above 0.6), path around a wall to the target without ever entering it, never exceed top speed, stay on their plane, face the way they move; the flow field points at the target in the open, routes round a wall's end, marks walled-in cells unreachable; only boxes crossing the plane are obstacles; spawning is deterministic.
- A test parses and validates every built-in WGSL shader with naga, the compiler wgpu uses, and checks each shader's copy of `DrawUniforms` is 256 bytes. It runs in CI without a GPU, and caught a reserved word in the first swarm shader.
- `gpu_step_matches_the_cpu_reference` runs the real compute shaders and compares 300 members over 20 steps, with a wall in the way, damage spheres and lines, and members attacking the target, with the CPU reference (positions, velocities, health and draw matrices within 1e-3; kill, attack and living counts exactly). `gpu_spawn_matches_the_cpu_reference` compares a spawn of two waves as sets of positions, since which dead slot takes which spawn varies on the GPU. Both need a GPU, so they are `#[ignore]`d in CI: `cargo test -p naive-client --lib -- --ignored gpu_`.
- Milestone 4's CPU tests: a sphere kills what it covers and nothing else, damage below a member's health wounds it, a line hurts only along its length, spawning fills dead slots inside the disc (and never more than the pool), dead members neither push nor move, attackers stop short of the target and hit on their interval; and the `SwarmBus` counts in-flight spawns and only reports "cleared" when nothing is on the way.
- **The headless runner now runs swarms.** `CpuSwarms` steps them with `step_swarm` and reports on the same frame, so `naive test` covers the Lua API, events and contact damage in CI: `tests/test_swarm.lua` (spawning, wounding then killing, lines, attacks that kill a dummy, retargeting, errors for unknown ids) and `tests/test_havoc.lua` (waves start on time, clearing one starts the next, a held rifle and the slam kill goblins through the real player script, the horde hurts a player who stands still).
- Claude can look at the result with `capture_frame`.

## Milestones

Each one ends with a number or a scene to look at.

| | Milestone | Done when | Size |
|---|---|---|---|
| 0 | Stop the per-entity bone upload | Measured above. Pull request #11 | Done |
| 1 | Instanced drawing for ECS entities that share a mesh and material | Measured above; regression captures pixel-identical | Done |
| 2 | Compute pass plumbing and the `gpu_swarm` component: members move toward a target, drawn instanced | `scenes/tier4_swarm.yaml`: 50,000 to 400,000 members at 120 fps (the display cap) | Done |
| 3 | Neighbour grid and flow field | Members spread out instead of stacking and path around obstacles: tests above, and the four walls in `tier4_swarm`; still 400,000 members at 120 fps | Done |
| 4 | Damage volumes, contact damage, death, spawn waves, the Lua API and events | A HAVOC wave 1 to 3 prototype: 1,000 to 5,000 members you can fight: `scenes/tier4_havoc.yaml`; still 400,000 members at 120 fps | Done |
| 5 | Scale-up and tidy | 50,000 members at 60 fps on this MacBook; README's Tier 4 row marked done | Medium |

Milestone 1 came before any compute work because swarms need instancing anyway, and it lifted the ceiling for ordinary entities on the way: from about 6,000 to about 100,000 static entities at 60 fps.

Milestone 2 measured, release build, same MacBook, `tier4_swarm` scene with its member count changed:

| Members | Frame rate |
|---|---|
| 50,000 | 119 fps |
| 100,000 | 117 fps |
| 200,000 | 118 fps |
| 400,000 | 118 fps |

All at the display's 120 Hz cap, so the real ceiling is higher; the 524,288-member limit per swarm comes first. In milestone 2 members only steered toward slots around the target.

Milestone 4 added health, damage volumes, attacks, spawning and the counters, and measured back to back with the previous `master` build: 50,000 members at 120.0 fps (`master` 120.1) and 400,000 at 119.6 (`master` 118.8). With a script firing the full 64 damage spheres every frame, 400,000 members still ran at 120 fps.

`scenes/tier4_havoc.yaml` is the milestone's prototype: three waves (1,000, 2,500, then 5,000 goblins with 30 health) spawn around the arena's edge, path around ruins and close on the player, who has a piercing rifle (hold the left mouse button) and a ground slam (E). Captures of it show the four spawn groups, the horde routing round the ruins' ends and packing around the player, a clean 7 m hole after a slam, and wounded goblins glowing.

Milestone 3 added the neighbour grid, separation and the flow field, and the same measurement did not move: 50,000, 100,000, 200,000 and 400,000 members all ran at 118 to 120 fps, still the display cap, now with four walls to path around. The GPU step for 400,000 members fits in a frame with room to spare; how much room is not measured (it would need GPU timestamp queries).

## Not in Tier 4

- **Animation.** Members are rigid meshes. Vertex animation textures are Tier 5 on the roadmap.
- **Level of detail and billboards.** The PRD ties these to vertex animation.
- **Multiplayer.** Swarm state is local to one machine.
- **The dependency upgrades** (`wgpu`, `rapier3d`). Tier 4 is built on `wgpu` 24.

## Risks

- **Looking at the screen.** Since `capture_frame` (pull request #13) Claude can look at renders itself, but a capture is one still frame; motion and feel still want you to run a scene.
- **The GPU path is outside CI.** CI validates the shaders and the CPU reference covers the maths; the GPU-versus-CPU test only runs by hand.
- **Projectiles no longer "hit" horde members physically.** They become rays or small damage volumes. Anything that relied on a Rapier contact with a goblin has to be expressed that way.
- **The 50,000 figure is met, combat included**, at the display cap; milestone 5's job is to show it at 60 fps with headroom measured (GPU timestamp queries) and tidy up.
- **Kill positions stay on the GPU.** Scripts learn how many died, not where, so effects at a death (gore, drops) can only go where the script aimed the damage.

## Decisions

Confirmed with the plan's approval on 2026-11-16:

1. **Swarm members are not individually scriptable.** Scripts get the swarm-level API above. Anything that needs its own script (a boss, an elite with special behaviour) stays an ECS entity.
2. **No animation in Tier 4.** Rigid meshes until Tier 5.
3. **Milestone 0 ships on its own**, ahead of the rest.
