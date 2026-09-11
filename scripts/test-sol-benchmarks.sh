#!/usr/bin/env bash
# Check that every benchmark is covered by the Sol runner and that paired Lua
# and Sol workloads produce numerically equivalent results before they are
# timed. Escaping closures are the sole documented language-level exception.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_cmd lua "Install a reference Lua interpreter, e.g.: brew install lua"
require_cmd awk "Install a POSIX awk implementation"

sol_bin=${SOL_BIN:-"$ROOT/crates/sol/target/debug/sol"}
if [[ ! -x "$sol_bin" ]]; then
  cargo build --offline --manifest-path "$ROOT/crates/sol/Cargo.toml"
fi

equivalent_number() {
  awk -v lua_value="$1" -v sol_value="$2" 'BEGIN {
    lua_value += 0
    sol_value += 0
    difference = lua_value - sol_value
    if (difference < 0) difference = -difference
    scale = lua_value
    if (scale < 0) scale = -scale
    candidate = sol_value
    if (candidate < 0) candidate = -candidate
    if (candidate > scale) scale = candidate
    if (scale < 1) scale = 1
    exit !(difference <= scale * 1e-12)
  }'
}

paired=0
sol_only=0
skipped=0
for lua_script in "$ROOT"/benchmarks/*.lua; do
  name=$(basename "$lua_script" .lua)
  sol_script="$ROOT/benchmarks/$name.sol"
  if [[ ! -f "$sol_script" ]]; then
    if [[ "$name" == function_calls_closure ]]; then
      printf 'SKIP  %-24s escaping Sol closures are not implemented\n' "$name"
      skipped=$((skipped + 1))
      continue
    fi
    die "$name.lua has no matching Sol benchmark"
  fi

  lua_output=$(lua "$lua_script")
  sol_output=$("$sol_bin" run "$sol_script")
  equivalent_number "$lua_output" "$sol_output" \
    || die "$name differs: Lua='$lua_output', Sol='$sol_output'"
  printf 'PASS  %-24s Lua=%s Sol=%s\n' "$name" "$lua_output" "$sol_output"
  paired=$((paired + 1))
done

for sol_script in "$ROOT"/benchmarks/*.sol; do
  name=$(basename "$sol_script" .sol)
  [[ -f "$ROOT/benchmarks/$name.lua" ]] && continue
  output=$("$sol_bin" run "$sol_script")
  printf 'PASS  %-24s Sol-only=%s\n' "$name" "$output"
  sol_only=$((sol_only + 1))
done

printf 'Sol benchmark conformance: %d paired, %d Sol-only, %d documented skip\n' \
  "$paired" "$sol_only" "$skipped"
