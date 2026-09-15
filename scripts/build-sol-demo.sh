#!/usr/bin/env bash
# Ahead-of-time compiles the Sol language-feature demo
# (examples/sol-demo/main.sol) to a standalone native executable via
# `sol build`. See scripts/run-sol-demo.sh to run it (alongside the
# interpreted tier) and check it against the demo's expected checksum.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"

sol_bin=${SOL_BIN:-"$ROOT/crates/sol/target/release/sol"}
if [[ ! -x "$sol_bin" ]]; then
  log "Building sol (release)..."
  cargo build --release --manifest-path "$ROOT/crates/sol/Cargo.toml"
fi

out_dir="$ROOT/examples/sol-demo/build"
mkdir -p "$out_dir"
out_bin="$out_dir/sol-demo"

log "Compiling examples/sol-demo/main.sol -> ${out_bin#"$ROOT"/}"
"$sol_bin" build "$ROOT/examples/sol-demo/main.sol" -o "$out_bin"
log "Built ${out_bin#"$ROOT"/}"
