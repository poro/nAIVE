# Moving the simulation into `naive-sim`

**Goal.** Everything a game needs to *run*, as opposed to *display*, lives in
`crates/naive-sim`, which never depends on wgpu, winit or an audio backend (CI
checks this). `naive-client` adds the window, renderer, audio and editor on
top. That gives:

- **Headless tests of all gameplay.** Today `naive test` cannot spawn meshes
  or fire weapons that need the GPU, and fails such tests explicitly.
- **A server.** `naive-server` can run the authoritative simulation.
- **The web build.** `naive-web` can depend on the simulation without
  pulling in the native renderer, Slang or audio.

## Where it stands

`naive-sim` holds `physics` (the Rapier wrapper), `input` (key, mouse and
action state, with nAIVE's own `Key` and `MouseButton`) and `project_path`
(path confinement). `naive-client` re-exports all three, so
`crate::physics::…`, `crate::input::…` and `crate::project_path::…` keep
working there. The engine turns winit events into `input::InputEvent`s in
`engine/winit_input.rs`.

What blocks moving the rest (found by following each module's imports):

| Module | Why it is GPU-bound today |
|---|---|
| `world` (`SceneWorld`, spawning) | Spawning loads meshes, materials, textures and splats through the GPU caches (`MeshCache`, `MaterialCache`, `TextureCache`, `SplatCache`). |
| `scripting` and `scripting/*` | The Lua API calls into audio (kira), UI text (`font`) and `particles`, and through `world` into the GPU caches. |
| `sim`, `swarm_api`, `test_runner` | Through `world` and `scripting`. |
| `scene_snapshot` | Looks up mesh and material paths in the GPU caches. |
| `swarm` | The CPU reference (`swarm/cpu.rs`, `swarm/flow.rs`) is GPU-free; the module also holds the GPU pipeline (`swarm/gpu.rs`). |

## Steps, one pull request each

1. **Input without winit.** *Done.* A `Key` and `MouseButton` enum of our
   own in `naive-sim`; the engine maps winit events onto them at the edge.
   Bindings files, Lua and `inject_input` keep their key names. Moved
   `input`.
2. **Assets behind a trait.** `trait Assets { fn mesh(&mut self, path) ->
   MeshHandle; fn material(…); fn texture(…); fn splat(…); fn mesh_path(handle)
   … }`, implemented by the GPU caches in `naive-client` and by a headless
   implementation that hands out handles and keeps the CPU data the
   simulation needs (bounds, trimesh colliders). Moves `world` and
   `scene_snapshot`.
3. **Script side effects as commands.** Audio, UI drawing, particles and
   camera shake become queued commands (or small traits) the client drains
   each frame; headless runs record or drop them. Moves `scripting`, `sim`,
   `swarm_api`, the CPU swarm and `test_runner`, which then registers every
   Lua API group instead of 6 of 13.
4. **Server.** `naive-server` runs `naive-sim` with a fixed step and no
   window.
5. **Web.** `mlua` with its C Lua does not build for
   `wasm32-unknown-unknown`, so the browser needs either an Emscripten build
   of Lua or a pure-Rust Lua VM before `naive-web` can run game scripts.
   Everything else in `naive-sim` (Rapier, glam, hecs) builds for wasm.

Each step keeps the client compiling through re-exports, so the next one can
land without a flag day.
