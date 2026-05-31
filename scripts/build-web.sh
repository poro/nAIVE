#!/usr/bin/env bash
# Build the nAIVE web playground into ./web/pkg
#
# Requirements:
#   cargo install wasm-pack
#
# Usage:
#   ./scripts/build-web.sh            # release build
#   ./scripts/build-web.sh --dev      # dev build (faster, larger)
#
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="--release"
if [[ "${1:-}" == "--dev" ]]; then
  PROFILE="--dev"
fi

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "wasm-pack not found. Install with: cargo install wasm-pack" >&2
  exit 1
fi

echo "→ building naive-web (wasm32-unknown-unknown) $PROFILE"
wasm-pack build crates/naive-web \
  --target web \
  --out-dir ../../web/pkg \
  --out-name naive_web \
  $PROFILE

echo
echo "→ done. Serve the ./web directory with any static server, e.g.:"
echo "    python3 -m http.server -d web 8000"
echo "  then open http://localhost:8000/"
