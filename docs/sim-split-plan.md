# Moving the simulation into `naive-sim`

**Goal.** Everything a game needs to run lives in `crates/naive-sim`, which
never depends on wgpu, winit or an audio backend. `naive-client` supplies the
window, renderer, audio device and editor. CI checks the dependency boundary
with Lua enabled and disabled.

## Where it stands

`naive-sim` now owns:

- Scene state, retained authoring definitions, snapshot generation, validated
  CPU assets, entity construction, deferred command contracts and pooling.
- Physics, input, particles, animation helpers and presentation state for
  cameras, viewports, UI intents and shake.
- Shared swarm layouts, targeting, flow fields, CPU simulation and the swarm
  command/report bus.
- With the `scripting` feature: the sandboxed Lua runtime and domain APIs,
  shared frame/lifecycle steps, swarm Lua registration and the headless runner.

The default feature set has no Lua VM, preserving a CPU library that can be
checked on `wasm32-unknown-unknown`. Native clients enable `scripting` through
their dependency. Standalone consumers can enable it without depending on the
client, GPU, window or audio libraries:

```sh
cargo test --locked -p naive-sim --no-default-features
cargo test --locked -p naive-sim --features scripting
```

`naive_sim::test_runner::{TestRunner, run_test_file}` provides the same runner
used by `naive test`. The runner retains CPU/memory/instruction limits,
monotonic deadlines, sticky failures, isolated test VMs and strict asset
preflight. Unsupported GPU mesh/texture commands still fail explicitly.
The internal `test-support` feature exposes asset-free schema fixtures to
cross-crate unit tests; production scene loading uses validated assets.

Client import paths re-export canonical shared types and functions, so ECS
queries, script environments and existing native callers keep using the same
implementations. Native adapters have these responsibilities:

| Client module | Remaining native responsibility |
|---|---|
| `engine`, `engine/commands` | Window/frame scheduling, editor commands, native scene replacement and orchestration |
| `world/native_assets`, `world/spawning`, `world` | GPU cache uploads and native spawning/reconciliation over shared constructors |
| `audio` | Kira device playback through the shared `AudioBackend` contract; headless runs use `SilentAudio` |
| `particles` | GPU-compatible particle vertices/layout; `AsMut` exposes the shared CPU simulation to Lua registration |
| `swarm/gpu` and `swarm/compute.wgsl` | GPU buffers, compute, rendering and asynchronous reports over shared planning/layouts |
| `anim_system`, `scene_snapshot` | Native skeleton/palette storage and asset-name lookup adapters |
| `test_execution` | CLI/editor subprocess supervision, worker IPC and bounded background admission |

No world or physics borrow is held across Lua callbacks. Shared construction
finishes before `init`; destruction runs callbacks while components are still
available, then retires subscriptions, physics and ECS state. Parity tests
exercise shared phase ordering and runner recreation, plus real native GPU,
audio and particle adapters against the headless runner. Native and headless
orchestration policies stay at their respective entry points.

## Completed extraction steps

1. **Input without winit.** Native events map onto shared key/mouse types;
   bindings files, Lua and `inject_input` preserve their names.
2. **CPU assets, world and construction.** Native uploads and the CPU catalog
   share decoders and one fallible constructor, including scaled mesh
   colliders, ordinary spawns, projectiles and dynamic objects. Snapshots use
   read-only asset-name lookups and preserve authoring metadata and errors.
3. **Shared scripting, lifecycle and runner.** Lua registration, limits,
   reloads, callbacks, deferred CPU operations and headless execution now
   live in `naive-sim`. Device playback and GPU operations are adapter work.
   The CPU swarm and its bus also live there. Client re-exports preserve
   existing API paths; native particle collection keeps its GPU vertex type.

## Remaining integration

- **Server.** `naive-server` is still a stub. It can now use the shared world,
  lifecycle and optional Lua runner without importing the native client;
  authoritative ticking, networking and replication remain to be implemented.
- **Web.** The default simulation is checked on WebAssembly. `mlua` uses C Lua
  and cannot target `wasm32-unknown-unknown`, so browser scripts still need a
  compatible Lua backend or an Emscripten build before the web prototype can
  run game scripts. The native scripting feature is deliberately opt-in.
