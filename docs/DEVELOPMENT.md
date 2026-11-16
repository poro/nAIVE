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
distribution works independently of the source checkout and SDK. Bundles ship
`naive-player` rather than the `naive` binary that built them; the smoke test
checks that it links no Slang library and starts it from another directory with
no subcommand, so it must find the game packaged beside it. Use `--slang` to
build the `naive` CLI with Slang as an additional run, or `--no-build` to
exercise already-built release binaries.

`naive-player` is the `naive` program built without the `slang` feature. Build
it on its own (`cargo build --release -p naive-player`): cargo unifies features
across packages built together, so building it in the same invocation as
`naive-runtime`'s default features would link Slang into it. For that reason it
is not a default workspace member. With Slang, `naive build` compiles each
`shaders/**/*.slang` to WGSL and writes it next to the source as
`<name>.slang.wgsl`; without Slang, the engine loads that file in place of
compiling. Files with no entry points are imported modules and are skipped.

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

An existing socket path is replaced only when it is a socket owned by the same
user that refuses connections, which is what a crashed or killed engine leaves.
A live engine, another user's socket, a symlink or any other file is never
touched. The server limits connections to 16, queued commands to 64 and
requests to 1 MiB; waits for responses and client input have timeouts. A game
started from a bundle opens no socket unless `--socket` is given.

glTF external resources currently require plain relative URIs; schemes and
percent-encoded paths are rejected. Packaging refuses symlinks in content and
output directories and requires a safe ASCII project name. Project confinement
assumes an untrusted process is not simultaneously modifying the project tree.

## Shipping a macOS app

`naive build` produces `dist/<name>.app`, signed ad hoc so it runs on the Mac
that built it. Gatekeeper blocks ad-hoc-signed apps downloaded from the
internet, so an app for other people must be signed with a Developer ID
Application certificate (Apple Developer Program) and notarized:

```sh
codesign --force --options runtime --timestamp \
  --sign "Developer ID Application: <Name> (<TEAMID>)" dist/<name>.app
ditto -c -k --keepParent dist/<name>.app <name>.zip
xcrun notarytool submit <name>.zip --apple-id <id> --team-id <TEAMID> \
  --password <app-specific-password> --wait
xcrun stapler staple dist/<name>.app
```

Zip the stapled app again for distribution. The release workflow runs these
steps when the signing secrets are configured.
