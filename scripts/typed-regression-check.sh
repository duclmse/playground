#!/usr/bin/env bash
# Automates the wall-clock half of the L8 checklist's typed-path regression
# gate: "before and after every dynamic optimization, run the existing typed
# Sol numeric, allocation, callback, table/array, JIT, and AOT benchmarks...
# reject a material typed-path regression (initial budget: 5% outside
# measurement noise)". Runs every typed-only benchmark (a `.sol` file under
# benchmarks/ with no `.lua` counterpart - the same set scripts/benchmark.sh's
# "Sol-only" loop measures) through hyperfine and compares the result against
# a committed baseline (benchmarks/typed-baseline.json).
#
# This does NOT automate the other half of the gate - "inspect typed IR/
# assembly to confirm no LuaValue boxing or dynamic dispatch was introduced
# into strict kernels" - that requires reading generated code by hand. This
# script prints a reminder of that step at the end; it never claims to have
# performed it.
#
# Usage:
#   scripts/typed-regression-check.sh --record   # (re)write the baseline
#   scripts/typed-regression-check.sh            # compare against baseline
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"
require_cmd hyperfine "Install with: brew install hyperfine (or see https://github.com/sharkdp/hyperfine)"
require_cmd jq "Install with: brew install jq (or see https://jqlang.org)"

BASELINE_FILE="${TYPED_REGRESSION_BASELINE:-$ROOT/benchmarks/typed-baseline.json}"
THRESHOLD="${TYPED_REGRESSION_THRESHOLD:-0.05}"
MODE="check"

usage() {
  cat <<'EOF'
Usage: scripts/typed-regression-check.sh [--record]

  (no flag)   Compare current typed-benchmark timings against the committed
              baseline; exit 1 if any benchmark regressed by more than the
              threshold AND outside combined measurement noise.
  --record    Re-run the typed benchmarks and overwrite the baseline file.
              Do this deliberately (e.g. after a known, accepted typed-path
              change), not to silence a real regression.

Environment:
  TYPED_REGRESSION_BASELINE   Baseline JSON path (default: benchmarks/typed-baseline.json)
  TYPED_REGRESSION_THRESHOLD  Fractional regression budget (default: 0.05 = 5%)
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --record) MODE="record"; shift ;;
    --help) usage; exit 0 ;;
    *) die "usage: $0 [--record]" ;;
  esac
done

log "Building crates/sol in release mode"
cargo build --release --manifest-path "$ROOT/crates/sol/Cargo.toml"
SOL_BIN="$ROOT/crates/sol/target/release/sol"

# The same "typed-only" set scripts/benchmark.sh's second loop measures: a
# .sol file with no same-named .lua file, i.e. a workload with no meaningful
# dynamic-path counterpart to compare against.
typed_benchmarks=()
for sol_script in "$ROOT"/benchmarks/*.sol; do
  name="$(basename "$sol_script" .sol)"
  typed_benchmarks+=("$name")
done
log "Typed benchmarks: ${typed_benchmarks[*]}"

tmp_json="$(mktemp)"
trap 'rm -f "$tmp_json"' EXIT

current_results="{}"
for name in "${typed_benchmarks[@]}"; do
  sol_script="$ROOT/benchmarks/$name.sol"
  hyperfine --warmup 3 --min-runs 10 --export-json "$tmp_json" -- "'$SOL_BIN' run '$sol_script'" >/dev/null
  mean="$(jq '.results[0].mean' "$tmp_json")"
  stddev="$(jq '.results[0].stddev' "$tmp_json")"
  current_results="$(jq --arg name "$name" --argjson mean "$mean" --argjson stddev "$stddev" \
    '.[$name] = {mean: $mean, stddev: $stddev}' <<<"$current_results")"
done

if [[ "$MODE" == "record" ]]; then
  echo "$current_results" | jq . >"$BASELINE_FILE"
  log "Baseline written to $BASELINE_FILE"
  exit 0
fi

if [[ ! -f "$BASELINE_FILE" ]]; then
  die "no baseline at $BASELINE_FILE - run with --record first"
fi

printf '\n%-25s %12s %12s %12s %10s\n' 'Benchmark' 'Baseline(ms)' 'Now(ms)' 'Delta' 'Verdict'
regressed=0
for name in "${typed_benchmarks[@]}"; do
  baseline_mean="$(jq -r --arg name "$name" '.[$name].mean // empty' "$BASELINE_FILE")"
  if [[ -z "$baseline_mean" ]]; then
    printf '%-25s %12s %12s %12s %10s\n' "$name" '-' '-' '-' 'NO-BASELINE'
    continue
  fi
  baseline_stddev="$(jq -r --arg name "$name" '.[$name].stddev' "$BASELINE_FILE")"
  new_mean="$(jq -r --arg name "$name" '.[$name].mean' <<<"$current_results")"
  new_stddev="$(jq -r --arg name "$name" '.[$name].stddev' <<<"$current_results")"

  verdict="$(jq -n \
    --argjson baseline_mean "$baseline_mean" --argjson baseline_stddev "$baseline_stddev" \
    --argjson new_mean "$new_mean" --argjson new_stddev "$new_stddev" \
    --argjson threshold "$THRESHOLD" '
    {
      delta_fraction: (($new_mean - $baseline_mean) / $baseline_mean),
      delta_abs: ($new_mean - $baseline_mean),
      noise: ($baseline_stddev + $new_stddev)
    } | . + {
      regressed: (
        (.delta_fraction > $threshold) and (.delta_abs > .noise)
      )
    }
  ')"
  delta_fraction="$(jq -r '.delta_fraction' <<<"$verdict")"
  is_regressed="$(jq -r '.regressed' <<<"$verdict")"
  delta_pct="$(printf '%+.1f%%' "$(echo "$delta_fraction * 100" | bc -l)")"
  status="ok"
  if [[ "$is_regressed" == "true" ]]; then
    status="REGRESSED"
    regressed=$((regressed + 1))
  fi
  printf '%-25s %12.3f %12.3f %12s %10s\n' "$name" \
    "$(echo "$baseline_mean * 1000" | bc -l)" "$(echo "$new_mean * 1000" | bc -l)" "$delta_pct" "$status"
done

echo
echo "Wall-clock check only - manually inspect typed IR/assembly for touched"
echo "kernels to confirm no LuaValue boxing or dynamic dispatch was"
echo "introduced (the L8 checklist's other regression-gate requirement; not"
echo "automated by this script)."

if [[ $regressed -gt 0 ]]; then
  echo
  die "$regressed typed benchmark(s) regressed beyond ${THRESHOLD} outside measurement noise"
fi
