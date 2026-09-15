#!/usr/bin/env bash
# Runs the whole test suite: lua-vm tests, Sol tests/strict lint/conformance and
# benchmark smoke checks, then a TypeScript typecheck + production web build.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"

log "cargo test (crates/lua-vm: unit tests + conformance suite against conformance/fixtures + conformance/expected)"
cargo test --manifest-path "$ROOT/crates/lua-vm/Cargo.toml"

log "cargo test + clippy (crates/sol-core: canonical values, heap, roots, and GC)"
cargo test --offline --manifest-path "$ROOT/crates/sol-core/Cargo.toml"
cargo clippy --offline --manifest-path "$ROOT/crates/sol-core/Cargo.toml" --all-targets -- -D warnings

log "cargo test (crates/sol: compiler, bytecode/JIT/AOT, and Lua compatibility)"
cargo test --offline --manifest-path "$ROOT/crates/sol/Cargo.toml"

log "cargo clippy (crates/sol, warnings denied)"
cargo clippy --offline --manifest-path "$ROOT/crates/sol/Cargo.toml" --all-targets -- -D warnings

log "cargo test + clippy (crates/sol-lsp)"
cargo test --offline --manifest-path "$ROOT/crates/sol-lsp/Cargo.toml"
cargo clippy --offline --manifest-path "$ROOT/crates/sol-lsp/Cargo.toml" --all-targets -- -D warnings

log "Lua 5.5 corpus-manifest regression checks"
"$ROOT/scripts/test-lua55-manifest.sh"

log "Unified product status gate (classification is not compatibility)"
"$ROOT/scripts/test-project-status.sh"

log "Focused supported-Lua fixture regressions"
"$ROOT/scripts/test-sol-conformance.sh"

log "Sol M10 typed-map benchmark smoke test"
benchmark_output="$(cargo run --quiet --offline --manifest-path "$ROOT/crates/sol/Cargo.toml" -- run "$ROOT/benchmarks/hashmap_lookup.sol")"
[ "$benchmark_output" = "50050000" ] || die "typed-map benchmark returned '$benchmark_output', expected 50050000"

if command -v npm >/dev/null 2>&1 && [ -d "$ROOT/node_modules" ]; then
  log "apps/web typecheck + build (smoke test)"
  (cd "$ROOT" && npm run build --workspace=apps/web)
else
  log "Skipping apps/web build smoke test (run 'npm install' at the repo root first)"
fi

log "All tests passed."
