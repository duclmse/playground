#!/usr/bin/env bash
# Check that every benchmark is covered by the Sol runner before timing. Every
# Lua workload is compared with Sol's dynamic runtime; same-named typed Sol
# workloads are additionally required to produce the same numeric result.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_cmd lua "Install a reference Lua interpreter, e.g.: brew install lua"
require_cmd awk "Install a POSIX awk implementation"

ensure_sol_bin debug
sol_bin=$SOL_BIN

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
dynamic_only=0
sol_only=0
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/sol-benchmark-conformance.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT
for lua_script in "$ROOT"/benchmarks/*.lua; do
  name=$(basename "$lua_script" .lua)
  sol_script="$ROOT/benchmarks/$name.sol"
  lua_output=$(lua "$lua_script")
  dynamic_raw=$(SOL_LUA_INSTRUCTION_BUDGET=18446744073709551615 \
    SOL_LUA_CALL_DEPTH_BUDGET=1000000 \
    SOL_LUA_ALLOCATION_BUDGET=18446744073709551615 \
    "$sol_bin" run "$lua_script")
  dynamic_file="$work_dir/$name.raw"
  printf '%s\n' "$dynamic_raw" >"$dynamic_file"
  dynamic_output=$(<"$dynamic_file")
  equivalent_number "$lua_output" "$dynamic_output" \
    || die "$name dynamic result differs: Lua='$lua_output', Sol dynamic='$dynamic_output'"
  if [[ -f "$sol_script" ]]; then
    typed_output=$("$sol_bin" run "$sol_script")
    equivalent_number "$lua_output" "$typed_output" \
      || die "$name typed result differs: Lua='$lua_output', Sol typed='$typed_output'"
    printf 'PASS  %-24s Lua=%s dynamic=%s typed=%s\n' \
      "$name" "$lua_output" "$dynamic_output" "$typed_output"
    paired=$((paired + 1))
  else
    printf 'PASS  %-24s Lua=%s dynamic=%s (no typed equivalent)\n' \
      "$name" "$lua_output" "$dynamic_output"
    dynamic_only=$((dynamic_only + 1))
  fi
done

for sol_script in "$ROOT"/benchmarks/*.sol; do
  name=$(basename "$sol_script" .sol)
  [[ -f "$ROOT/benchmarks/$name.lua" ]] && continue
  output=$("$sol_bin" run "$sol_script")
  printf 'PASS  %-24s Sol-only=%s\n' "$name" "$output"
  sol_only=$((sol_only + 1))
done

printf 'Sol benchmark conformance: %d Lua/typed pairs, %d dynamic-only Lua, %d typed-only Sol\n' \
  "$paired" "$dynamic_only" "$sol_only"
