# Development and release checks

Install Git LFS and materialize assets before building. The default native build
requires the pinned Slang SDK described in the README. Rust is pinned in CI so
Clippy changes do not introduce unrelated failures.

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude naive-web --locked
cargo clippy --workspace --exclude naive-web --all-targets --locked -- -D warnings
cargo test --workspace --exclude naive-web --no-default-features --locked
cargo clippy --workspace --exclude naive-web --all-targets --no-default-features --locked -- -D warnings
(cd project && cargo run --locked -p naive-runtime --bin naive -- test)
cargo install cargo-audit --version 0.23.0 --locked
cargo audit
python3 scripts/release-smoke.py
```

The release smoke test builds optimized binaries without Slang, creates and bundles a fixture
project, launches the packaged runtime, loads a scene, drives the real command
socket, captures a PNG, and runs Lua tests. It also saves and reloads gameplay
components, initializes Lua and MCP spawns, checks collider and listener cleanup,
and transitions scenes through an `on_destroy` hook that edits its entity.
Linux CI uses Xvfb and Mesa software
Vulkan so this path is exercised without a physical display. Failed runs must
fail CI rather than silently skip rendering. This proves the built-in WGSL
distribution works independently of the source checkout and SDK. A default
Slang-enabled bundle still needs Slang shared libraries installed on its target;
it is not a self-contained SDK distribution. Use `--slang` for an additional
SDK-enabled smoke run, or `--no-build` to exercise already-built release binaries.

The Slang-free job deliberately installs no SDK. It tests built-in WGSL and
headless simulation; it does not validate runtime Slang compilation. The web
crate is checked separately for `wasm32-unknown-unknown`.

Dependency auditing also runs weekly to catch newly disclosed advisories.
Upgrade affected dependencies rather than adding broad advisory exclusions.
Unmaintained transitive dependencies may still produce warnings; track these
separately from exploitable vulnerabilities.

Editor commands are implemented in `naive-client/src/engine/commands.rs`.
The shared command types and MCP tool schemas live in `naive-core/src/command.rs`.
Lua APIs are grouped under `scripting/`; GPU swarm setup, flow-field solving,
CPU reference simulation and shader code live under `swarm/`.

Scene saves retain resolved entity definitions, including script paths, physics,
gameplay components, camera modes, mesh flags and unknown authored components.
Snapshots overlay supported live edits and write through a temporary file before
replacing the destination. They are scene-authoring snapshots: Lua state,
velocities and simulation progress are not savegame data. Saving fails explicitly
for transient projectiles, visibility/material overrides, generated meshes or
runtime texture changes that the scene schema cannot represent.

Entity creation uses the scene constructors, then initializes pending scripts
after ECS borrows have ended. Destruction calls `on_destroy` while the entity is
still accessible, then removes physics, scripts and entity-owned event listeners.
Script reloads keep the last working environment and state if loading or
`on_reload` fails. State nested beyond 256 tables rejects the reload safely.
When a replacement registers event listeners, they replace that entity's old
listeners. If it registers none, subscriptions created by `init` remain active
and share the current `self` table; `init` is not replayed. Those retained
callbacks keep their previous code and captured locals until explicitly replaced
from `on_reload`. These lifecycle paths are shared by native and headless code.

Project paths reject absolute paths, parent traversal and symlinks escaping the
project. The command socket is local developer control: access is limited to
the engine's user. Use a private directory and an explicit `--socket` for
concurrent instances. Processes running as the same user remain trusted.

The socket refuses every pre-existing path, including a socket left by a crash.
Choose a new `--socket` path or explicitly remove a known stale socket before
restarting. It limits connections to 16, queued commands to 64 and requests to
1 MiB; waits for responses and client input have timeouts.

glTF external resources currently require plain relative URIs; schemes and
percent-encoded paths are rejected. Packaging refuses symlinks in content and
output directories and requires a safe ASCII project name. Project confinement
assumes an untrusted process is not simultaneously modifying the project tree.
