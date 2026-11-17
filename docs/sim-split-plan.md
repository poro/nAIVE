# Moving the simulation into `naive-sim`

**Goal.** Everything a game needs to *run*, as opposed to *display*, lives in
`crates/naive-sim`, which never depends on wgpu, winit or an audio backend (CI
checks this). `naive-client` adds the window, renderer, audio and editor on
top. That gives:

- **Headless gameplay tests.** `naive test` loads authored mesh and material
  assets, runs mesh colliders, animation clocks and particle simulation, and
  records UI draws without a GPU. Runtime GPU mesh creation and texture swaps
  still fail explicitly; native release smoke tests cover those paths.
- **A server.** `naive-server` can run the authoritative simulation.
- **The web build.** `naive-web` can depend on the simulation without
  pulling in the native renderer, Slang or audio.

## Where it stands

`naive-sim` holds `physics` (the Rapier wrapper), `input` (key, mouse and
action state, with nAIVE's own `Key` and `MouseButton`), `project_path`
(path confinement), validated CPU `assets`, `particles`, animation playback
helpers and `presentation` (camera matrices, viewport, UI intents and shake).
`naive-client` re-exports the original three modules, so
`crate::physics::…`, `crate::input::…` and `crate::project_path::…` keep
working there. The engine turns winit events into `input::InputEvent`s in
`engine/winit_input.rs`.

What blocks moving the rest (found by following each module's imports):

| Module | Why it is GPU-bound today |
|---|---|
| `world` (`SceneWorld`, spawning) | Native spawning still uploads through GPU caches; the headless path uses the shared CPU asset catalog. Both use the same asset decoders. The constructors still need a unified upload boundary. |
| `scripting` and `scripting/*` | Presentation APIs use CPU contracts, but audio and runtime mesh/texture operations still refer to native adapters. |
| `sim`, `swarm_api`, `test_runner` | Through `world` and `scripting`. |
| `scene_snapshot` | Looks up mesh and material paths in the GPU caches. |
| `swarm` | The CPU reference (`swarm/cpu.rs`, `swarm/flow.rs`) is GPU-free; the module also holds the GPU pipeline (`swarm/gpu.rs`). |

## Steps, one pull request each

1. **Input without winit.** *Done.* A `Key` and `MouseButton` enum of our
   own in `naive-sim`; the engine maps winit events onto them at the edge.
   Bindings files, Lua and `inject_input` keep their key names. Moved
   `input`.
2. **Shared CPU assets.** *Implemented; constructor extraction remains.*
   Fallible mesh, material, texture and splat decoders are shared by the native
   GPU caches and the headless asset catalog. Missing or invalid assets fail
   instead of producing placeholder geometry. Headless mesh handles retain
   vertices, triangles, skeletons and clips; trimesh colliders use the authored
   scale. Move `world` and `scene_snapshot` after consolidating upload adapters.
3. **Script side effects as commands.** *Partially implemented.* UI drawing,
   camera projection/picking, shake, particle updates and animation clocks use
   common CPU implementations. Headless tests register the corresponding Lua
   API groups and retain UI intents. A headless viewport defaults to 1280×720
   and can be changed through `TestRunner::set_viewport`. Audio playback remains
   a native concern, and runtime GPU commands fail explicitly in tests.
   Moving `scripting`, `sim`, `swarm_api`, the CPU swarm and `test_runner` into
   `naive-sim` remains separate work.
4. **Server.** `naive-server` runs `naive-sim` with a fixed step and no
   window.
5. **Web.** `mlua` with its C Lua does not build for
   `wasm32-unknown-unknown`, so the browser needs either an Emscripten build
   of Lua or a pure-Rust Lua VM before `naive-web` can run game scripts.
   Everything else in `naive-sim` (Rapier, glam, hecs) builds for wasm.

Each step keeps the client compiling through re-exports, so the next one can
land without a flag day.
