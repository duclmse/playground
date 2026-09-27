#!/usr/bin/env bash
# Automates the wall-clock half of the L8 checklist's typed-path regression
# gate, plus the mechanically-checkable part of its IR-inspection half:
# "before and after every dynamic optimization, run the existing typed Sol
# numeric, allocation, callback, table/array, JIT, and AOT benchmarks...
# reject a material typed-path regression (initial budget: 5% outside
# measurement noise) and inspect typed IR/assembly to confirm no LuaValue
# boxing or dynamic dispatch was introduced into strict kernels".
#
# Wall-clock half: runs every typed-only benchmark (a `.sol` file under
# benchmarks/ with no `.lua` counterpart - the same set scripts/benchmark.sh's
# "Sol-only" loop measures) through hyperfine and compares the result against
# a committed baseline (benchmarks/typed-baseline.json).
#
# IR half: runs each typed-only benchmark once more through
# `sol run --dump-ir`, which dumps every compiled function's Cranelift CLIF
# IR to stderr, and greps the dump for the two ways dynamic dispatch shows up
# in typed CLIF: an indirect call (`call_indirect`, vs. a statically-resolved
# `call`) or a call to one of the runtime helpers that exist specifically to
# service `any`-typed/dynamic values (`sol_dynamic_binary`, `sol_dynamic_compare`,
# `sol_dynamic_neg`, `sol_truth` - see crate/sol/src/codegen.rs's
# declare_runtime). A typed-only benchmark has no `any`/dynamic values by
# construction, so any of these appearing in its own dump is a strong signal
# that a strict kernel started boxing/dispatching dynamically. This does NOT
# detect boxing itself (Box/Unbox compile to inline bit-packing, not a call -
# see crate/sol/src/codegen.rs), nor does it replace reading generated
# assembly by hand for subtler regressions; it automates the specific,
# mechanically-checkable "did a dynamic-dispatch call creep in" question.
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

log "Building crate/sol in release mode"
target_root=$(cargo_target_root)
CARGO_TARGET_DIR="$target_root" cargo build --release --manifest-path "$ROOT/crate/sol/Cargo.toml"
SOL_BIN="$target_root/release/sol"

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

log "Checking for dynamic dispatch in typed IR (dump-ir + grep)"
DISPATCH_PATTERN='call_indirect|sol_dynamic_binary|sol_dynamic_compare|sol_dynamic_neg|sol_truth'
dispatch_found=0
for name in "${typed_benchmarks[@]}"; do
  sol_script="$ROOT/benchmarks/$name.sol"
  # Force immediate promotion so every function actually gets JIT-compiled
  # and dumped, regardless of how many times this benchmark happens to call
  # it at runtime - otherwise a kernel below the default promote/OSR
  # thresholds would produce an empty dump and silently pass unchecked.
  ir="$(SOL_PROMOTE_THRESHOLD=1 SOL_OSR_THRESHOLD=1 "$SOL_BIN" run --dump-ir "$sol_script" 2>&1 >/dev/null)"
  if hits="$(grep -Ein "$DISPATCH_PATTERN" <<<"$ir")"; then
    echo "DYNAMIC DISPATCH DETECTED in $name:"
    echo "$hits"
    dispatch_found=$((dispatch_found + 1))
  fi
done
if [[ $dispatch_found -gt 0 ]]; then
  echo
  die "$dispatch_found typed benchmark(s) show dynamic dispatch (call_indirect or sol_dynamic_*/sol_truth) in their compiled IR"
fi
log "No dynamic dispatch detected in any typed benchmark's IR"

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
echo "IR dispatch check passed above (no call_indirect/sol_dynamic_*/sol_truth"
echo "in any typed benchmark). This does not detect boxing itself (Box/Unbox"
echo "compile to inline bit-packing, not a call) - for subtler regressions,"
echo "still inspect generated assembly for touched kernels by hand."

if [[ $regressed -gt 0 ]]; then
  echo
  die "$regressed typed benchmark(s) regressed beyond ${THRESHOLD} outside measurement noise"
fi
