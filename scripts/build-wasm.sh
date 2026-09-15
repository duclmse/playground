#!/usr/bin/env bash
# Builds crates/lua-vm for wasm32 and regenerates the wasm-bindgen JS glue
# into packages/lua-runtime/pkg - the npm package apps/web depends on.
# Run this after any change to crates/lua-vm before `npm run dev`/`build`.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_wasm_target
require_wasm_bindgen_cli

log "Building crates/lua-vm for wasm32-unknown-unknown (release)"
target_root=$(cargo_target_root)
CARGO_TARGET_DIR="$target_root" cargo build --release \
  --manifest-path "$ROOT/crates/lua-vm/Cargo.toml" --target wasm32-unknown-unknown

WASM_OUT="$target_root/wasm32-unknown-unknown/release/lua_vm.wasm"
[ -f "$WASM_OUT" ] || die "expected build output not found at $WASM_OUT"

log "Generating wasm-bindgen JS glue into packages/lua-runtime/pkg"
wasm-bindgen --target web --out-dir "$ROOT/packages/lua-runtime/pkg" "$WASM_OUT"

RAW_SIZE="$(du -h "$WASM_OUT" | cut -f1)"
BOUND_SIZE="$(du -h "$ROOT/packages/lua-runtime/pkg/lua_vm_bg.wasm" | cut -f1)"
log "Done. Raw wasm: ${RAW_SIZE}, bound wasm: ${BOUND_SIZE}"
