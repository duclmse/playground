#!/usr/bin/env bash
# Local deploy for development using the checked-in generated WASM package,
# then starts the Vite dev server for apps/web.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd npm "Install Node.js: https://nodejs.org"

PKG_WASM="$ROOT/packages/lua-runtime/pkg/lua_vm_bg.wasm"
[ -f "$PKG_WASM" ] || die "generated WASM package is missing; the retired crates/vm backend can no longer regenerate it"
log "Using checked-in WASM package pending the sol-core browser adapter"

if [ ! -d "$ROOT/node_modules" ]; then
  log "node_modules missing, running npm install"
  (cd "$ROOT" && npm install)
fi

log "Starting apps/web dev server"
(cd "$ROOT" && npm run dev --workspace=apps/web -- "$@")
