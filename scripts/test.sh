#!/usr/bin/env bash
# Runs the whole test suite: crates/lua-vm's unit + conformance tests
# (native, no wasm/browser needed), then a TypeScript typecheck + production
# build of apps/web as a build-time smoke test.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"

log "cargo test (crates/lua-vm: unit tests + conformance suite against conformance/fixtures)"
cargo test --manifest-path "$ROOT/crates/lua-vm/Cargo.toml"

if command -v npm >/dev/null 2>&1 && [ -d "$ROOT/node_modules" ]; then
  log "apps/web typecheck + build (smoke test)"
  (cd "$ROOT" && npm run build --workspace=apps/web)
else
  log "Skipping apps/web build smoke test (run 'npm install' at the repo root first)"
fi

log "All tests passed."
