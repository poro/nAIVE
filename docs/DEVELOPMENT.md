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
socket, captures a PNG, and runs a Lua test. Linux CI uses Xvfb and Mesa software
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
