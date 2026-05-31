# naive-web

In-browser playground for nAIVE. Compiles to `wasm32-unknown-unknown` via
`wasm-pack` and renders through WebGPU.

## Build

```bash
cargo install wasm-pack    # one-time
./scripts/build-web.sh     # from the nAIVE repo root
python3 -m http.server -d web 8000
# open http://localhost:8000/
```

## Status

**v0** — proof of life. Renders a clear color on a canvas. Does not yet depend
on `naive-client`. See [`docs/web-playground-plan.md`](../../docs/web-playground-plan.md)
for the roadmap.

## Browser support

Needs WebGPU: Chrome/Edge 113+, Safari 18+, Firefox Nightly with the flag.
The frontend detects `navigator.gpu` and shows a fallback message otherwise.
