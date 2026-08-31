#!/usr/bin/env bash
# Full production build: wasm runtime + apps/web static bundle.
# Output lands in apps/web/dist, ready to serve from any static host.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd npm "Install Node.js: https://nodejs.org"

./build-wasm.sh

if [ ! -d "$ROOT/node_modules" ]; then
  log "node_modules missing, running npm install"
  (cd "$ROOT" && npm install)
fi

log "Building apps/web for production"
(cd "$ROOT" && npm run build --workspace=apps/web)

log "Build complete: apps/web/dist"
