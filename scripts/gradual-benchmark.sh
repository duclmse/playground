#!/usr/bin/env bash
# U11 item 6 - the gradually-typed benchmark suite (docs/features/
# unified-sol-runtime-plan.md §7.1's third required suite: "the same
# programs with increasing annotation coverage, measuring each step").
#
# Reads benchmarks/gradual-manifest.json: for each benchmark, an ordered
# list of steps from 0% annotation coverage (a plain .lua file, or a .sol
# file with no type annotations at all - both fall back to the same
# dynamic, budget-gated interpreter) up to 100% (the existing fully-typed
# .sol benchmark). Every step runs through the same `sol run` binary so the
# only variable between steps is annotation coverage, not which interpreter
# is doing the work.
#
# For each benchmark, measures every step with hyperfine and prints one
# table ordered by annotation coverage. Exits nonzero if any step is slower
# than the step immediately before it by more than a threshold AND outside
# combined measurement noise - the same regression formula and default
# threshold (5%) scripts/typed-regression-check.sh uses - UNLESS the
# manifest marks that step with an "explained" reason, in which case the
# number is still printed but the hard failure is suppressed.
#
# Usage: scripts/gradual-benchmark.sh [--filter NAME]
#
# Environment:
#   GRADUAL_MANIFEST            Manifest JSON path (default: benchmarks/gradual-manifest.json)
#   GRADUAL_REGRESSION_THRESHOLD  Fractional regression budget (default: 0.05 = 5%, matches typed-regression-check.sh)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_cmd hyperfine "Install with: brew install hyperfine (or see https://github.com/sharkdp/hyperfine)"
require_cmd jq "Install with: brew install jq (or see https://jqlang.org)"

MANIFEST_FILE="${GRADUAL_MANIFEST:-$ROOT/benchmarks/gradual-manifest.json}"
THRESHOLD="${GRADUAL_REGRESSION_THRESHOLD:-0.05}"
FILTER=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --filter)
      FILTER="${2:?--filter requires a benchmark name}"
      shift 2
      ;;
    *)
      die "usage: $0 [--filter NAME]"
      ;;
  esac
done

[[ -f "$MANIFEST_FILE" ]] || die "no manifest at $MANIFEST_FILE"

log "Building crate/sol in release mode"
target_root=$(cargo_target_root)
CARGO_TARGET_DIR="$target_root" cargo build --release --manifest-path "$ROOT/crate/sol/Cargo.toml"
SOL_BIN="$target_root/release/sol"

# A 0%-annotation step may fall back to the dynamic, budget-gated Lua-
# compatibility interpreter (lua_runtime.rs); its defaults are sized for
# untrusted embedded code and would abort these benchmark-sized workloads
# partway through (see scripts/benchmark.sh and docs/features/
# lua-compatibility.md's SOL_LUA_*_BUDGET addendum). Applying these
# overrides to every step is harmless for natively-compiled steps.
export SOL_LUA_INSTRUCTION_BUDGET=18446744073709551615
export SOL_LUA_CALL_DEPTH_BUDGET=1000000
export SOL_LUA_ALLOCATION_BUDGET=18446744073709551615

tmp_json="$(mktemp)"
trap 'rm -f "$tmp_json"' EXIT

total_regressed=0
benchmark_count="$(jq '.benchmarks | length' "$MANIFEST_FILE")"

for ((b = 0; b < benchmark_count; b++)); do
  name="$(jq -r ".benchmarks[$b].name" "$MANIFEST_FILE")"
  if [ -n "$FILTER" ] && [ "$name" != "$FILTER" ]; then
    continue
  fi
  step_count="$(jq ".benchmarks[$b].steps | length" "$MANIFEST_FILE")"
  log "Gradual benchmark: $name ($step_count steps)"

  printf '\n%-40s %12s %12s %10s %10s  %s\n' \
    'Step (annotation coverage)' 'Mean(ms)' 'StdDev(ms)' 'Delta' 'Verdict' 'Note'

  prev_mean=""
  prev_stddev=""
  for ((s = 0; s < step_count; s++)); do
    label="$(jq -r ".benchmarks[$b].steps[$s].label" "$MANIFEST_FILE")"
    rel_path="$(jq -r ".benchmarks[$b].steps[$s].path" "$MANIFEST_FILE")"
    explained="$(jq -r ".benchmarks[$b].steps[$s].explained // empty" "$MANIFEST_FILE")"
    script_path="$ROOT/$rel_path"
    [[ -f "$script_path" ]] || die "manifest entry for $name step $s: no such file: $script_path"

    hyperfine --warmup 3 --min-runs 10 --export-json "$tmp_json" -- \
      "'$SOL_BIN' run '$script_path'" >/dev/null
    mean="$(jq '.results[0].mean' "$tmp_json")"
    stddev="$(jq '.results[0].stddev' "$tmp_json")"

    delta_display="-"
    verdict="baseline"
    note=""
    if [[ -n "$prev_mean" ]]; then
      verdict_json="$(jq -n \
        --argjson prev_mean "$prev_mean" --argjson prev_stddev "$prev_stddev" \
        --argjson mean "$mean" --argjson stddev "$stddev" \
        --argjson threshold "$THRESHOLD" '
        {
          delta_fraction: (($mean - $prev_mean) / $prev_mean),
          delta_abs: ($mean - $prev_mean),
          noise: ($prev_stddev + $stddev)
        } | . + {
          regressed: ((.delta_fraction > $threshold) and (.delta_abs > .noise))
        }
      ')"
      delta_fraction="$(jq -r '.delta_fraction' <<<"$verdict_json")"
      is_regressed="$(jq -r '.regressed' <<<"$verdict_json")"
      delta_display="$(printf '%+.1f%%' "$(echo "$delta_fraction * 100" | bc -l)")"
      if [[ "$is_regressed" == "true" ]]; then
        if [[ -n "$explained" ]]; then
          verdict="EXPLAINED"
          note="$explained"
        else
          verdict="FAIL"
          note="slower than previous step by more than ${THRESHOLD} outside noise, no 'explained' reason in manifest"
          total_regressed=$((total_regressed + 1))
        fi
      else
        verdict="ok"
      fi
    fi

    printf '%-40s %12.3f %12.3f %10s %10s  %s\n' \
      "$label" "$(echo "$mean * 1000" | bc -l)" "$(echo "$stddev * 1000" | bc -l)" \
      "$delta_display" "$verdict" "$note"

    prev_mean="$mean"
    prev_stddev="$stddev"
  done
done

echo
if [[ $total_regressed -gt 0 ]]; then
  die "$total_regressed step(s) regressed beyond ${THRESHOLD} outside measurement noise, with no 'explained' reason - see FAIL rows above"
fi
log "All steps are monotonic (or explained) across every gradual benchmark"
