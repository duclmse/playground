#!/usr/bin/env bash
# Compares this project's Lua VM (crates/vm, a fork of piccolo - see
# crates/vm/README.md) against the reference Lua interpreter (and LuaJIT,
# if installed) on the microbenchmarks in benchmarks/. Uses hyperfine for
# statistically sound timing (warmup + multiple runs), so the whole
# process (interpreter startup + compile + execute) is measured the same
# way for every implementation - a fair, real-world "run this program"
# comparison rather than an internal-clock micro-measurement.
#
# Usage: scripts/benchmark.sh [--filter NAME] [--export-markdown FILE]
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
FILTER=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --export-markdown)
      EXPORT_MARKDOWN="${2:?--export-markdown requires a file path}"
      case "$EXPORT_MARKDOWN" in
        /*) : ;; # already absolute
        *) EXPORT_MARKDOWN="$ORIG_PWD/$EXPORT_MARKDOWN" ;;
      esac
      shift 2
      ;;
    --filter)
      FILTER="${2:?--filter requires a benchmark name}"
      shift 2
      ;;
    *)
      die "usage: $0 [--filter NAME] [--export-markdown FILE]"
      ;;
  esac
done
if [ -n "$EXPORT_MARKDOWN" ]; then
  : > "$EXPORT_MARKDOWN"
fi

log "Building crates/vm's interpreter example in release mode"
cargo build --release --example interpreter --manifest-path "$ROOT/crates/vm/Cargo.toml"
VM_BIN="$ROOT/crates/vm/target/release/examples/interpreter"

log "Building crates/sol in release mode"
cargo build --release --manifest-path "$ROOT/crates/sol/Cargo.toml"
SOL_BIN="$ROOT/crates/sol/target/release/sol"

LUA_RUNTIMES=(lua)
LUA_RUNTIME_VERSIONS=("$(lua -v 2>&1 | head -n 1)")
for runtime in lua5.5 lua5.4 lua5.3 lua5.2 lua5.1 luajit luau; do
  if command -v "$runtime" >/dev/null 2>&1; then
    version="$("$runtime" -v 2>&1 | head -n 1)"
    seen=0
    for known_version in "${LUA_RUNTIME_VERSIONS[@]}"; do
      [ "$version" = "$known_version" ] && seen=1
    done
    if [ "$seen" -eq 0 ]; then
      LUA_RUNTIMES+=("$runtime")
      LUA_RUNTIME_VERSIONS+=("$version")
    fi
  fi
done
log "Lua runtimes: ${LUA_RUNTIMES[*]}"

for script in "$ROOT"/benchmarks/*.lua; do
  name="$(basename "$script" .lua)"
  if [ -n "$FILTER" ] && [ "$name" != "$FILTER" ]; then
    continue
  fi
  log "Benchmark: $name"
  args=(--warmup 3 --min-runs 10)
  for runtime in "${LUA_RUNTIMES[@]}"; do
    label="$runtime"
    [ "$runtime" = "lua" ] && label="lua (reference)"
    args+=(-n "$label" "'$runtime' '$script'")
  done
  args+=(-n "vm (this project)" "'$VM_BIN' '$script'")
  # If a same-named .sol program exists (Sol's typed compilation path - see
  # docs/sol.md), include it too - the whole reason for writing these Sol
  # programs in the first place is to get an honest, reproducible answer on
  # whether the typed/native-compiled path is actually winning on the workload
  # it's supposed to be best at.
  sol_script="$ROOT/benchmarks/$name.sol"
  if [ -f "$sol_script" ]; then
    args+=(-n "sol" "'$SOL_BIN' run '$sol_script'")
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
