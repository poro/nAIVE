# Web Playground — Roadmap

Goal: collapse "first taste" friction for nAIVE from *install Rust + slang SDK + clone repo* down to *open a URL*.

## v0 — Proof of life (DONE)

- `crates/naive-web` cdylib that compiles to `wasm32-unknown-unknown`.
- `web/` static frontend: canvas + tiny JS loader.
- `scripts/build-web.sh` wraps `wasm-pack`.
- Renders a clear color via WebGPU. No naive-client coupling yet.

Validates: toolchain (cargo → wasm-pack → ESM → WebGPU init → canvas draw).

Run locally:

```bash
cargo install wasm-pack    # one-time
./scripts/build-web.sh
python3 -m http.server -d web 8000
# open http://localhost:8000/
```

## v1 — Naive client behind a feature flag

Gate non-wasm deps in `naive-client`:

| Dep            | Action for wasm                                       |
|----------------|--------------------------------------------------------|
| shader-slang   | drop; bake WGSL at host-build time via `build.rs`      |
| notify         | drop; replace file-watch hot-reload with fetch + poll  |
| ureq           | drop or swap for `gloo-net`                            |
| tokio (full)   | trim to `sync` + `macros`; no `net`/`io-util`          |
| kira (audio)   | keep; has web backend                                  |
| rapier3d       | keep; wasm-compatible                                  |
| mlua           | keep with `vendored`; verify wasm build               |

Add `[features] web = []` to `naive-client`. Use `cfg(not(target_arch = "wasm32"))` for slang/notify/ureq paths. Switch `naive-web` to depend on `naive-client` with `default-features = false, features = ["web"]`.

## v2 — Demo browser in the browser

- Bake the 14 embedded demo YAML + Lua scripts (already `include_str!`'d) into wasm.
- Pre-compile each demo's shaders to WGSL via host `build.rs` (runs slang on the dev machine, emits `OUT_DIR/wgsl/*.wgsl`, `include_str!`'d into the wasm).
- URL routes: `/demo/<slug>` deep-links to a specific demo.
- "View source" panel showing YAML + Lua for the active demo.

## v3 — Live YAML editor

- Monaco editor pinned to the active scene's YAML.
- Edit → debounce → recompile scene → hot-swap without reload.
- Lua console with output stream.

## v4 — MCP bridge

- Optional WebSocket relay so a remote Claude/GPT agent can drive the in-browser engine via the existing MCP JSON-RPC interface.
- Same protocol as the native `naive_mcp` Unix-socket server, transport swapped to WS.

## Hosting

The `web/` directory is fully static. Any host works:

- GitHub Pages (publish `web/` from a build action)
- Cloudflare Pages
- Vercel static deployment
- The existing naive.dev origin (drop `web/` at `/play/`)

WebGPU requires HTTPS in production. Cross-origin isolation (`COOP: same-origin`, `COEP: require-corp`) is **not** required for WebGPU itself but is needed if/when we add `SharedArrayBuffer`-based threading.

## Risks

| Risk                                 | Mitigation                                       |
|--------------------------------------|--------------------------------------------------|
| WebGPU browser coverage              | Detect + show fallback message (v0 already does) |
| Slang at wasm runtime                | Pre-bake WGSL; defer slang-in-browser            |
| wasm binary size                     | wasm-opt -O3, gzip; demos load on demand         |
| Lua via mlua on wasm                 | Verify build early in v1                         |
| Cross-platform shader behavior       | CI runs both native + wasm demo render tests     |
