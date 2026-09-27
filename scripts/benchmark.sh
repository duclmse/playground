#!/usr/bin/env bash
# Compares Sol against the reference Lua interpreter (and LuaJIT, if
# installed) on the microbenchmarks in benchmarks/. Uses hyperfine for
# statistically sound timing (warmup + multiple runs), so the whole
# process (interpreter startup + compile + execute) is measured the same
# way for every implementation - a fair, real-world "run this program"
# comparison rather than an internal-clock micro-measurement.
#
# Each benchmark is measured twice (L8 checklist: "report interpreter cold
# start and steady-state cache/JIT results separately"): once as "<name>"
# with hyperfine's usual --warmup 3 --min-runs 10 (repeated process
# launches, so any OS-level disk-cache/JIT-cache warm-up has already
# happened), and once as "<name> (cold)" with --warmup 0 --runs 1 (the very
# first, unwarmed process launch). A single unwarmed run has no variance to
# report (stddev is 0), which is expected, not a measurement bug.
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

SUMMARY_FILE="$(mktemp)"
cleanup_summary() {
  rm -f "$SUMMARY_FILE"
}
trap cleanup_summary EXIT

record_summary() {
  local benchmark_name="$1"
  local csv_file="$2"
  awk -F, -v benchmark="$benchmark_name" 'NR > 1 {
    printf "%s\t%s\t%s\t%s\t%s\t%s\n", benchmark, $1, $2, $3, $7, $8
  }' "$csv_file" >> "$SUMMARY_FILE"
}

run_group() {
  local benchmark_name="$1"
  shift
  local csv_file
  csv_file="$(mktemp)"
  if [ -n "$EXPORT_MARKDOWN" ]; then
    local markdown_file
    markdown_file="$(mktemp)"
    hyperfine "$@" --export-csv "$csv_file" --export-markdown "$markdown_file"
    { echo "## $benchmark_name"; echo; cat "$markdown_file"; echo; } >> "$EXPORT_MARKDOWN"
    rm -f "$markdown_file"
  else
    hyperfine "$@" --export-csv "$csv_file"
  fi
  record_summary "$benchmark_name" "$csv_file"
  rm -f "$csv_file"
}

print_summary() {
  local benchmark_count measurement_count
  benchmark_count="$(awk -F '\t' '!seen[$1]++ { count++ } END { print count + 0 }' "$SUMMARY_FILE")"
  measurement_count="$(awk 'END { print NR + 0 }' "$SUMMARY_FILE")"
  printf '\nBenchmark summary: %s cases, %s measurements (milliseconds; lower is better)\n\n' \
    "$benchmark_count" "$measurement_count"
  printf '%-25s %-20s %12s %12s %14s\n' \
    'Benchmark' 'Runtime' 'Mean' 'Std dev' 'Runtime / Sol'
  awk -F '\t' '
    {
      benchmark[NR] = $1
      runtime[NR] = $2
      mean[NR] = $3
      deviation[NR] = $4
      if ($2 == "sol") sol_mean[$1] = $3
    }
    END {
      for (i = 1; i <= NR; i++) {
        relative = "-"
        if (sol_mean[benchmark[i]] > 0) {
          relative = sprintf("%.2fx", mean[i] / sol_mean[benchmark[i]])
        }
        printf "%-25s %-20s %12.3f %12.3f %14s\n", \
          benchmark[i], runtime[i], mean[i] * 1000, deviation[i] * 1000, relative
      }
    }
  ' "$SUMMARY_FILE"
}

append_markdown_summary() {
  {
    echo '## Overall summary'
    echo
    echo '| Benchmark | Runtime | Mean | Std dev | Runtime / Sol |'
    echo '|:--|:--|--:|--:|--:|'
    awk -F '\t' '
      {
        benchmark[NR] = $1
        runtime[NR] = $2
        mean[NR] = $3
        deviation[NR] = $4
        if ($2 == "sol") sol_mean[$1] = $3
      }
      END {
        for (i = 1; i <= NR; i++) {
          relative = "-"
          if (sol_mean[benchmark[i]] > 0) {
            relative = sprintf("%.2fx", mean[i] / sol_mean[benchmark[i]])
          }
          printf "| %s | %s | %.3f ms | %.3f ms | %s |\n", \
            benchmark[i], runtime[i], mean[i] * 1000, deviation[i] * 1000, relative
        }
      }
    ' "$SUMMARY_FILE"
    echo
  } >> "$EXPORT_MARKDOWN"
}

log "Building crate/sol in release mode"
target_root=$(cargo_target_root)
CARGO_TARGET_DIR="$target_root" cargo build --release --manifest-path "$ROOT/crate/sol/Cargo.toml"
SOL_BIN="$target_root/release/sol"

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
  commands=()
  for runtime in "${LUA_RUNTIMES[@]}"; do
    label="$runtime"
    [ "$runtime" = "lua" ] && label="lua (reference)"
    commands+=(-n "$label" "'$runtime' '$script'")
  done
  # Also run the same .lua source through Sol's own dynamic interpreter
  # (`sol run`, the `.lua`-compatibility path `lua_runtime.rs` implements -
  # see docs/features/lua-superset-plan.md Phase 6) - this is the honest
  # LuaJIT comparison for ordinary dynamic Lua code, as opposed to the typed
  # `.sol` row below. Budgets are overridden generously: `LuaRuntime::new`'s
  # defaults are sized for untrusted embedded code and would abort a
  # benchmark-sized workload partway through (see the `SOL_LUA_*_BUDGET`
  # addendum in docs/features/lua-compatibility.md).
  commands+=(-n "sol (dynamic)" "SOL_LUA_INSTRUCTION_BUDGET=18446744073709551615 SOL_LUA_CALL_DEPTH_BUDGET=1000000 SOL_LUA_ALLOCATION_BUDGET=18446744073709551615 '$SOL_BIN' run '$script'")
  # If a same-named .sol program exists (Sol's typed compilation path - see
  # docs/sol.md), include it too - the whole reason for writing these Sol
  # programs in the first place is to get an honest, reproducible answer on
  # whether the typed/native-compiled path is actually winning on the workload
  # it's supposed to be best at.
  sol_script="$ROOT/benchmarks/$name.sol"
  if [ -f "$sol_script" ]; then
    commands+=(-n "sol" "'$SOL_BIN' run '$sol_script'")
  fi

  run_group "$name" --warmup 3 --min-runs 10 "${commands[@]}"
  run_group "$name (cold)" --warmup 0 --runs 1 "${commands[@]}"
done

# Some Sol benchmarks compare two compiler modes or measure a typed-only
# optimization and deliberately have no meaningful Lua source counterpart.
# Include those instead of silently dropping them from the suite.
for sol_script in "$ROOT"/benchmarks/*.sol; do
  name="$(basename "$sol_script" .sol)"
  if [ -f "$ROOT/benchmarks/$name.lua" ]; then
    continue
  fi
  if [ -n "$FILTER" ] && [ "$name" != "$FILTER" ]; then
    continue
  fi
  log "Benchmark: $name (Sol-only)"
  commands=(-n "sol" "'$SOL_BIN' run '$sol_script'")
  run_group "$name" --warmup 3 --min-runs 10 "${commands[@]}"
  run_group "$name (cold)" --warmup 0 --runs 1 "${commands[@]}"
done

print_summary
if [ -n "$EXPORT_MARKDOWN" ]; then
  append_markdown_summary
  log "Results written to $EXPORT_MARKDOWN"
fi
