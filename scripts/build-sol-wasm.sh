#!/usr/bin/env bash
# Regenerates the production packages/sol-runtime/pkg from crate/sol's
# `wasm` feature (generic, specialized, and checked mixed live adapters;
# see docs/features/milestones/u12-wasm-playground.md). Like the historical
# scripts/build-wasm.sh, uses the
# pre-stub shape (raw `cargo build` + standalone `wasm-bindgen` CLI, not
# wasm-pack), targeting crate/sol instead of the retired crate/lua-vm, and
# with `--no-default-features` so the `jit` feature's Cranelift dependencies
# (no wasm32 support at all - see tier0.rs's own doc comment) are never
# pulled into this build.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_wasm_target
require_wasm_bindgen_cli

target_root=$(cargo_target_root)
CARGO_TARGET_DIR="$target_root" cargo build --release \
  --manifest-path "$ROOT/crate/sol/Cargo.toml" \
  --no-default-features --features wasm \
  --target wasm32-unknown-unknown

WASM_OUT="$target_root/wasm32-unknown-unknown/release/sol.wasm"
[ -f "$WASM_OUT" ] || die "expected build output not found at $WASM_OUT"

wasm-bindgen --target web --out-dir "$ROOT/packages/sol-runtime/pkg" "$WASM_OUT"
