#!/usr/bin/env bash
# Local deploy for development: builds the wasm runtime (if missing or
# stale) then starts the Vite dev server for apps/web.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd npm "Install Node.js: https://nodejs.org"

PKG_WASM="$ROOT/packages/lua-runtime/pkg/lua_vm_bg.wasm"
NEEDS_BUILD=1
if [ -f "$PKG_WASM" ]; then
  NEEDS_BUILD=0
  while IFS= read -r -d '' src; do
    if [ "$src" -nt "$PKG_WASM" ]; then
      NEEDS_BUILD=1
      break
    fi
  done < <(find \
    "$ROOT/crates/lua-vm/src" "$ROOT/crates/lua-vm/Cargo.toml" \
    "$ROOT/crates/vm/src" "$ROOT/crates/vm/Cargo.toml" \
    -type f -print0)
fi

if [ "$NEEDS_BUILD" = "1" ]; then
  log "lua-vm wasm build is missing or stale, rebuilding"
  ./build-wasm.sh
else
  log "lua-vm wasm build is up to date, skipping rebuild"
fi

if [ ! -d "$ROOT/node_modules" ]; then
  log "node_modules missing, running npm install"
  (cd "$ROOT" && npm install)
fi

log "Starting apps/web dev server"
(cd "$ROOT" && npm run dev --workspace=apps/web -- "$@")
