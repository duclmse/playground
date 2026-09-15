#!/usr/bin/env bash
# Runs the Sol language-feature demo (examples/sol-demo/main.sol) two ways
# - the interpreted/JIT tier via `sol run`, and a standalone AOT executable
# via scripts/build-sol-demo.sh - and checks both agree with the demo's
# expected checksum. Per docs/spec/execution-and-runtime.md, every
# execution tier must produce the same observable result for supported
# features; running both tiers here is that guarantee, exercised on a
# program that touches most of the typed language surface at once.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"

ensure_sol_bin release
sol_bin=$SOL_BIN

demo_src="$ROOT/examples/sol-demo/main.sol"

# main() sums one contribution per demo_* function; see that file for the
# per-feature breakdown this total is made of (FFI 28, imported module 25,
# struct/record 59, arrays 36, maps 130, generic-for/map() 30, closures 30,
# function values 42, any/is/as narrowing 45, control flow/operators 130,
# array-of-structs 5, map specializations 11, integer overflow 1, string
# closures 6, two-level module chain 20, bitwise/floor-div/pow/string-compare
# 31).
expected=629

log "Running examples/sol-demo/main.sol (interpreted/JIT tier)..."
interpreted=$("$sol_bin" run "$demo_src")
echo "  -> $interpreted"
[[ "$interpreted" == "$expected" ]] || die "interpreted result '$interpreted' != expected $expected"

log "Building and running the AOT executable..."
"$ROOT/scripts/build-sol-demo.sh"
aot_bin="$ROOT/examples/sol-demo/build/sol-demo"
native=$("$aot_bin")
echo "  -> $native"
[[ "$native" == "$expected" ]] || die "AOT result '$native' != expected $expected"

log "Sol demo: interpreted and AOT tiers both produced $expected - every demoed feature checks out."

log "Running examples/sol-demo/traps/*.sol (expected to abort)..."
"$ROOT/scripts/run-sol-demo-traps.sh"
