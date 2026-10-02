#!/usr/bin/env bash
# U11 item 7 - docs/features/unified-sol-runtime-plan.md §7.3's "Typed
# advantage" release gate: "Fully typed/data-oriented suite exceeds LuaJIT
# by a separately published target while preserving mixed-call and
# compatibility behavior."
#
# Measures, for every benchmark with both a plain `.lua` and a fully-typed
# `.sol` twin under benchmarks/ (the "typed/data-oriented suite" with a
# meaningful LuaJIT comparison - §7.1: "a benchmark with no LuaJIT-equivalent
# behavior may inform Sol tuning but cannot support the comparative
# headline"), LuaJIT's wall-clock mean against Sol's AOT build (`sol build` -
# this milestone's peak-performance path, not the tiered JIT `sol run`),
# and reports the geometric mean of the per-benchmark LuaJIT/Sol ratios
# (>1 means Sol is faster).
#
# "a separately published target": as of this writing no such target has
# actually been published anywhere in docs/ (grepped for it) - rather than
# invent one, this script reports the measured ratio unconditionally and
# only evaluates pass/fail if a target is explicitly given, matching U10
# item 8's precedent of reporting an honest current number instead of a
# fabricated gate.
#
# "preserving mixed-call and compatibility behavior" is not re-measured
# here - that's the full `cargo test --manifest-path crate/sol/Cargo.toml`
# suite and `scripts/typed-regression-check.sh`'s job; this script assumes
# both already pass and measures only the throughput half of the gate.
#
# Usage:
#   scripts/typed-advantage-gate.sh [--target RATIO] [--export-json FILE]
#
# Environment:
#   TYPED_ADVANTAGE_TARGET  Same as --target.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_cmd hyperfine "Install with: brew install hyperfine (or see https://github.com/sharkdp/hyperfine)"
require_cmd jq "Install with: brew install jq (or see https://jqlang.org)"
require_cmd luajit "Install with: brew install luajit (or see https://luajit.org)"

TARGET="${TYPED_ADVANTAGE_TARGET:-}"
EXPORT_JSON=""
FILTER=""

usage() {
  cat <<'EOF'
Usage: scripts/typed-advantage-gate.sh [--target RATIO] [--export-json FILE] [--filter NAME]

  --target RATIO       Fail (nonzero exit) if the measured LuaJIT/Sol(AOT)
                        geometric mean is below RATIO. Omit to report only
                        (no published target exists yet - see this script's
                        header comment).
  --export-json FILE   Write the full per-benchmark and geomean result as JSON.
  --filter NAME         Only measure the one named benchmark.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target) TARGET="${2:?--target requires a ratio}"; shift 2 ;;
    --export-json) EXPORT_JSON="${2:?--export-json requires a file path}"; shift 2 ;;
    --filter) FILTER="${2:?--filter requires a benchmark name}"; shift 2 ;;
    --help) usage; exit 0 ;;
    *) die "usage: $0 [--target RATIO] [--export-json FILE] [--filter NAME]" ;;
  esac
done

log "Building crate/sol in release mode"
target_root=$(cargo_target_root)
CARGO_TARGET_DIR="$target_root" cargo build --release --manifest-path "$ROOT/crate/sol/Cargo.toml"
SOL_BIN="$target_root/release/sol"

# The "typed/data-oriented suite" slice with a meaningful LuaJIT comparison:
# a benchmark with both a plain .lua and a fully-typed .sol twin.
benchmarks=()
for lua_script in "$ROOT"/benchmarks/*.lua; do
  name="$(basename "$lua_script" .lua)"
  [[ -f "$ROOT/benchmarks/$name.sol" ]] || continue
  if [ -n "$FILTER" ] && [ "$name" != "$FILTER" ]; then
    continue
  fi
  benchmarks+=("$name")
done
[[ ${#benchmarks[@]} -gt 0 ]] || die "no paired .lua/.sol benchmarks found under benchmarks/"
log "Typed/data-oriented suite (paired with LuaJIT): ${benchmarks[*]}"

aot_tmp_dir="$(mktemp -d)"
tmp_json="$(mktemp)"
trap 'rm -rf "$aot_tmp_dir"; rm -f "$tmp_json"' EXIT

hyperfine_mean() {
  hyperfine --warmup 3 --min-runs 10 --export-json "$tmp_json" -- "$1" >/dev/null
  jq '.results[0].mean' "$tmp_json"
}

results="{}"
for name in "${benchmarks[@]}"; do
  lua_script="$ROOT/benchmarks/$name.lua"
  sol_script="$ROOT/benchmarks/$name.sol"
  aot_bin="$aot_tmp_dir/$name"
  "$SOL_BIN" build "$sol_script" -o "$aot_bin" >/dev/null 2>&1 \
    || die "'sol build' failed for $name.sol - fix the AOT build before running this gate"

  log "Measuring $name: luajit vs sol (aot)"
  luajit_mean="$(hyperfine_mean "luajit '$lua_script'")"
  sol_aot_mean="$(hyperfine_mean "'$aot_bin'")"
  ratio="$(jq -n --argjson l "$luajit_mean" --argjson s "$sol_aot_mean" '$l / $s')"
  results="$(jq --arg name "$name" --argjson luajit "$luajit_mean" --argjson sol_aot "$sol_aot_mean" --argjson ratio "$ratio" \
    '.[$name] = {luajit_mean: $luajit, sol_aot_mean: $sol_aot, ratio: $ratio}' <<<"$results")"
done

geomean="$(jq '[.[] | .ratio] | (reduce .[] as $r (0; . + ($r | log)) / length) | exp' <<<"$results")"

printf '\n%-25s %14s %14s %10s\n' 'Benchmark' 'LuaJIT(ms)' 'Sol AOT(ms)' 'Ratio'
for name in "${benchmarks[@]}"; do
  jq -r --arg name "$name" '
    .[$name] | "\($name)\t\(.luajit_mean * 1000)\t\(.sol_aot_mean * 1000)\t\(.ratio)"
  ' <<<"$results" | awk -F '\t' '{ printf "%-25s %14.3f %14.3f %9.2fx\n", $1, $2, $3, $4 }'
done
printf '\nGeometric mean (LuaJIT / sol (aot)), %d benchmarks: %.3fx\n' "${#benchmarks[@]}" "$geomean"

if [[ -n "$EXPORT_JSON" ]]; then
  jq -n --argjson per_benchmark "$results" --argjson geomean "$geomean" \
    '{per_benchmark: $per_benchmark, geomean_ratio: $geomean}' >"$EXPORT_JSON"
  log "Full result written to $EXPORT_JSON"
fi

if [[ -z "$TARGET" ]]; then
  echo
  log "No published target (§7.3's 'separately published target' does not yet exist in docs/) - reporting only, not evaluating pass/fail."
  exit 0
fi

passed="$(jq -n --argjson geomean "$geomean" --argjson target "$TARGET" '$geomean >= $target')"
echo
if [[ "$passed" == "true" ]]; then
  log "Typed advantage gate PASSED: ${geomean}x >= target ${TARGET}x"
else
  die "Typed advantage gate NOT MET: ${geomean}x < target ${TARGET}x"
fi
