#!/usr/bin/env bash
# Compares this project's Lua VM (crates/vm, a fork of piccolo - see
# crates/vm/README.md) against the reference Lua interpreter (and LuaJIT,
# if installed) on the microbenchmarks in benchmarks/. Uses hyperfine for
# statistically sound timing (warmup + multiple runs), so the whole
# process (interpreter startup + compile + execute) is measured the same
# way for every implementation - a fair, real-world "run this program"
# comparison rather than an internal-clock micro-measurement.
#
# Usage: scripts/benchmark.sh [--export-markdown FILE]
set -euo pipefail
# Resolve --export-markdown's path relative to the caller's original
# working directory, before `cd`-ing into scripts/ below - otherwise a
# relative path like `benchmarks/RESULTS.md` would be (wrongly) resolved
# relative to scripts/benchmarks/RESULTS.md instead of the repo root.
ORIG_PWD="$PWD"
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_cmd hyperfine "Install with: brew install hyperfine (or see https://github.com/sharkdp/hyperfine)"
require_cmd lua "Install a reference Lua interpreter, e.g.: brew install lua"

EXPORT_MARKDOWN=""
if [ "${1:-}" = "--export-markdown" ]; then
  EXPORT_MARKDOWN="${2:?--export-markdown requires a file path}"
  case "$EXPORT_MARKDOWN" in
    /*) : ;; # already absolute
    *) EXPORT_MARKDOWN="$ORIG_PWD/$EXPORT_MARKDOWN" ;;
  esac
  : > "$EXPORT_MARKDOWN"
fi

log "Building crates/vm's interpreter example in release mode"
cargo build --release --example interpreter --manifest-path "$ROOT/crates/vm/Cargo.toml"
VM_BIN="$ROOT/crates/vm/target/release/examples/interpreter"

log "Building crates/fastlua in release mode"
cargo build --release --manifest-path "$ROOT/crates/fastlua/Cargo.toml"
FASTLUA_BIN="$ROOT/crates/fastlua/target/release/fastlua"

LUAJIT_BIN=""
if command -v luajit >/dev/null 2>&1; then
  LUAJIT_BIN="$(command -v luajit)"
  log "LuaJIT found - including it as a bonus reference point"
else
  log "LuaJIT not found - comparing lua and vm only (install luajit to add it)"
fi

for script in "$ROOT"/benchmarks/*.lua; do
  name="$(basename "$script" .lua)"
  log "Benchmark: $name"
  args=(
    --warmup 3
    --min-runs 10
    -n "lua (reference)" "lua '$script'"
  )
  if [ -n "$LUAJIT_BIN" ]; then
    args+=(-n "luajit" "'$LUAJIT_BIN' '$script'")
  fi
  args+=(-n "vm (this project)" "'$VM_BIN' '$script'")
  # If a same-named .fl program exists (fastlua - M1 of faster_lua.md's
  # roadmap, see docs/fastlua.md), include it too - the whole reason for
  # writing these fastlua programs in the first place is to get an honest,
  # reproducible answer on whether the typed/native-compiled path is
  # actually winning on the workload it's supposed to be best at.
  fl_script="$ROOT/benchmarks/$name.fl"
  if [ -f "$fl_script" ]; then
    args+=(-n "fastlua" "'$FASTLUA_BIN' run '$fl_script'")
  fi

  if [ -n "$EXPORT_MARKDOWN" ]; then
    tmp="$(mktemp)"
    hyperfine "${args[@]}" --export-markdown "$tmp"
    { echo "## $name"; echo; cat "$tmp"; echo; } >> "$EXPORT_MARKDOWN"
    rm -f "$tmp"
  else
    hyperfine "${args[@]}"
  fi
done

if [ -n "$EXPORT_MARKDOWN" ]; then
  log "Results written to $EXPORT_MARKDOWN"
fi
