#!/usr/bin/env bash
# Differential runner: run each Lua 5.5 corpus case through both Sol and a
# pinned reference Lua 5.5.1 build, and diff their stdout, stderr, and exit
# status. This is distinct from scripts/test-lua55-suite.sh (Sol vs. the
# manifest's declared expectation) and scripts/test-lua55-reference.sh
# (reference Lua vs. checked-in tests/lua55/reference/*.stdout snapshots):
# neither of those compares Sol's and reference Lua's output to each other on
# the same case. This script never substitutes the system `lua`/`lua5.5`
# binary as the oracle - see tests/lua55/README.md and AGENTS.md. It requires
# an explicit, pinned-source-built reference executable (LUA55_REFERENCE_BIN,
# matching the convention scripts/test-lua55-reference.sh already uses), and
# refuses to run if that executable is missing rather than silently falling
# back to whatever `lua` happens to be on PATH.
#
# stdout is diffed byte-for-byte. Exit status is compared only as "did this
# program fail at all" (both zero, or both nonzero) - Sol's CLI and PUC Lua's
# `lua` do not share a nonzero-exit-code convention (e.g. panics vs. runtime
# errors vs. usage errors), so requiring the literal codes to match would be
# testing an implementation detail neither side documents as stable. stderr
# is compared the same way: "did this program produce anything on stderr at
# all", not literal text - Sol's error-message wording is intentionally not a
# byte-for-byte clone of PUC Lua's C `luaL_error`/traceback formatting (see
# docs/features/lua-compatibility.md), so a literal stderr diff would mostly
# report cosmetic wording differences, not compatibility bugs. Per-case log
# files under the results directory still capture the raw stdout/stderr of
# both sides for manual inspection when a case diverges on any axis. A single
# `failure-report.md` under the results directory collects a minimized entry
# per diverging case: its source fixture, manifest capability profile
# (category/requires), which axis diverged, and the first lines of any
# stdout diff.
#
# Generated-input fuzzing (SOL_LUA55_DIFF_FUZZ_CASES): in addition to the
# fixed corpus above, this can also generate SOL_LUA55_DIFF_FUZZ_CASES
# self-contained Lua programs from a small template library (arithmetic,
# string concat, table iteration, multi-return assignment, a conditional,
# and a bounded while loop), assembled by a deterministic, seeded (no
# external dependency) linear-congruential generator - see
# `fuzz_lcg_next`/`fuzz_generate_case` below - so a run is reproducible from
# SOL_LUA55_DIFF_FUZZ_SEED alone. A diverging generated case gets its source
# saved as a permanent fixture under tests/lua55/fuzz-fixtures/ (the L8
# checklist's "differential fuzz failures become permanent fixtures" rule)
# and its seed recorded in failure-report.md (a real number, not the fixed
# corpus's `n/a`). Generated programs return normally, as ordinary Lua chunks
# do; their top-level return values are deliberately not CLI output.
set -u

fuzz_cases=${SOL_LUA55_DIFF_FUZZ_CASES:-0}
fuzz_seed=${SOL_LUA55_DIFF_FUZZ_SEED:-1}

# Deterministic linear-congruential generator (glibc's constants) - no
# external dependency, no reliance on bash's own $RANDOM (whose algorithm
# isn't guaranteed stable across bash versions/platforms, which would make
# "seeded" reproducibility a lie). Callers must invoke fuzz_lcg_next/
# fuzz_lcg_range directly (never via `$(...)`), since a command substitution
# runs in a subshell and any state it sets is lost when the subshell exits.
fuzz_lcg_state=0
fuzz_lcg_last=0
fuzz_lcg_seed() { fuzz_lcg_state=$(( $1 % 2147483648 )); }
fuzz_lcg_next() { fuzz_lcg_state=$(( (1103515245 * fuzz_lcg_state + 12345) % 2147483648 )); fuzz_lcg_last=$fuzz_lcg_state; }
fuzz_lcg_range() { fuzz_lcg_next; fuzz_lcg_last=$(( fuzz_lcg_last % $1 )); }

case_source=""

fuzz_block_arith() {
  fuzz_lcg_range 100; local a=$fuzz_lcg_last
  fuzz_lcg_range 19; local b=$(( fuzz_lcg_last + 1 ))
  case_source+="do local a, b = $a, $b
  print(\"arith\", a + b, a - b, a * b, a // b, a % b)
end
"
}

fuzz_block_concat() {
  fuzz_lcg_range 4; local i1=$(( fuzz_lcg_last + 1 ))
  fuzz_lcg_range 4; local i2=$(( fuzz_lcg_last + 1 ))
  fuzz_lcg_range 1000; local n=$fuzz_lcg_last
  case_source+="do local words = {\"alpha\", \"beta\", \"gamma\", \"delta\"}
  print(\"concat\", words[$i1] .. \"-\" .. words[$i2] .. \"-\" .. tostring($n))
end
"
}

fuzz_block_table() {
  fuzz_lcg_range 40; local n=$(( fuzz_lcg_last + 1 ))
  fuzz_lcg_range 9; local k=$(( fuzz_lcg_last + 1 ))
  case_source+="do local t = {}
  for i = 1, $n do t[i] = i * $k end
  local sum = 0
  for _, v in ipairs(t) do sum = sum + v end
  print(\"table\", sum, #t)
end
"
}

fuzz_block_multiret() {
  fuzz_lcg_range 100; local r1=$fuzz_lcg_last
  fuzz_lcg_range 100; local r2=$fuzz_lcg_last
  fuzz_lcg_range 100; local r3=$fuzz_lcg_last
  case_source+="do local function f() return $r1, $r2, $r3 end
  local a, b, c, d = f()
  print(\"multiret\", a, b, c, d)
end
"
}

fuzz_block_cond() {
  fuzz_lcg_range 200; local x=$(( fuzz_lcg_last - 100 ))
  fuzz_lcg_range 200; local threshold=$(( fuzz_lcg_last - 100 ))
  case_source+="do local x = $x
  if x > $threshold then print(\"cond\", \"gt\")
  elseif x == $threshold then print(\"cond\", \"eq\")
  else print(\"cond\", \"lt\") end
end
"
}

fuzz_block_while() {
  fuzz_lcg_range 30; local n=$(( fuzz_lcg_last + 1 ))
  case_source+="do local i, total = 0, 0
  while i < $n do total = total + i; i = i + 1 end
  print(\"while\", total, i)
end
"
}

fuzz_blocks=(fuzz_block_arith fuzz_block_concat fuzz_block_table fuzz_block_multiret fuzz_block_cond fuzz_block_while)

# Fills the global `case_source` with one self-contained generated program
# deterministically derived from $1.
fuzz_generate_case() {
  fuzz_lcg_seed "$1"
  case_source=""
  fuzz_lcg_range 4
  local block_count=$(( fuzz_lcg_last + 1 ))
  for ((i = 0; i < block_count; i++)); do
    fuzz_lcg_range ${#fuzz_blocks[@]}
    "${fuzz_blocks[$fuzz_lcg_last]}"
  done
}

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "$script_dir/lib.sh"

suite_dir=${1:-"$ROOT/lua-5.5.1-tests"}
manifest=${SOL_LUA55_MANIFEST:-"$ROOT/tests/lua55/manifest.toml"}
source_dir=${LUA55_SOURCE_DIR:-"$ROOT/lua-5.5.1"}
reference_bin=${LUA55_REFERENCE_BIN:-"$source_dir/src/lua"}
run_pending=${SOL_LUA55_DIFF_RUN_PENDING:-1}
results_dir=${SOL_LUA55_DIFF_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/sol-lua55-diff.XXXXXX")"}
keep_results=${SOL_LUA55_DIFF_RESULTS_DIR:+1}

usage() {
  cat <<'EOF'
Usage: scripts/test-lua55-differential.sh [lua-5.5.1-tests-directory]

Diffs Sol's stdout, stderr, and exit status against a pinned reference Lua
5.5.1 build's, case by case, for every manifest entry (by default including
`pending` cases, so a case that already agrees with the reference can be
reclassified). stdout is compared byte-for-byte; exit status and stderr are
compared only as "did this side fail / produce anything at all" (see the
script header comment for why literal codes/text are not required to match).

Environment:
  SOL_BIN                    Sol executable (default: debug build)
  SOL_LUA55_MANIFEST         TOML manifest to read cases from
  LUA55_REFERENCE_BIN        Built Lua 5.5.1 executable (default:
                              lua-5.5.1/src/lua) - must come from the pinned
                              source in tests/lua55/README.md; this script
                              will not substitute the system `lua` binary
  SOL_LUA55_DIFF_RUN_PENDING=0  Only diff pass/adapted/diverges cases
  SOL_LUA55_DIFF_RESULTS_DIR  Preserve per-case stdout/diff logs and the
                              minimized failure-report.md here
  SOL_LUA55_DIFF_FUZZ_CASES  Also generate and diff this many seeded,
                              synthetic Lua programs (default: 0, disabled)
  SOL_LUA55_DIFF_FUZZ_SEED   Base seed for generated cases (default: 1);
                              case N uses seed SOL_LUA55_DIFF_FUZZ_SEED + N,
                              so a run is fully reproducible from the seed
                              alone. A diverging generated case is saved to
                              tests/lua55/fuzz-fixtures/case-<seed>.lua.

This requires a real pinned-source build of reference Lua 5.5.1
(scripts/test-lua55-reference.sh's LUA55_BUILD=1 builds one); it exits
immediately, without running anything, if LUA55_REFERENCE_BIN is not an
executable file.
EOF
}

if [[ ${1:-} == --help ]]; then
  usage
  exit 0
fi
if [[ ! -d "$suite_dir" ]]; then
  echo "Lua 5.5 suite directory not found: $suite_dir" >&2
  exit 2
fi
if [[ ! -f "$manifest" ]]; then
  echo "Lua 5.5 manifest not found: $manifest" >&2
  exit 2
fi
if [[ ! -x "$reference_bin" ]]; then
  cat >&2 <<EOF
Reference Lua 5.5.1 executable not found or not executable: $reference_bin

This script deliberately does not fall back to the system \`lua\`/\`lua5.5\`
binary as the oracle (see tests/lua55/README.md). Build the pinned reference
first, e.g.:

  LUA55_BUILD=1 scripts/test-lua55-reference.sh

or point LUA55_REFERENCE_BIN at an existing pinned-source build.
EOF
  exit 2
fi
ensure_sol_bin debug || exit $?
sol_bin=$SOL_BIN

mkdir -p "$results_dir"
entries_file="$results_dir/manifest.tsv"

if ! parse_lua55_manifest "$manifest" "$entries_file"; then
  [[ -n "$keep_results" ]] || rm -rf "$results_dir"
  exit 2
fi
if ! validate_lua55_corpus_coverage "$entries_file" "$suite_dir"; then
  [[ -n "$keep_results" ]] || rm -rf "$results_dir"
  exit 2
fi

failure_report="$results_dir/failure-report.md"
{
  echo "# Lua 5.5 differential failure report"
  echo
  echo "Each entry is a minimized summary of a diverging case: its source"
  echo "fixture, capability profile (manifest category/requires, or a seed for"
  echo "a generated fuzz case), and which comparison axis diverged."
  echo
} >"$failure_report"

matched=0
diverged=0
skipped=0
while IFS=$'\034' read -r path status category requires fixture note budget alloc_budget; do
  case "$status" in
    host-required) skipped=$((skipped + 1)); continue ;;
    pending) [[ $run_pending == 1 ]] || { skipped=$((skipped + 1)); continue; } ;;
  esac
  source="$suite_dir/$path"
  target=$source
  if [[ -n "$fixture" ]]; then
    target="$ROOT/$fixture"
  fi
  if [[ ! -f "$target" ]]; then
    printf 'SKIP  %-18s fixture not found: %s\n' "$path" "$target"
    skipped=$((skipped + 1))
    continue
  fi
  sol_out="$results_dir/$path.sol.stdout"
  sol_err="$results_dir/$path.sol.stderr"
  ref_out="$results_dir/$path.reference.stdout"
  ref_err="$results_dir/$path.reference.stderr"

  # Match the reference process's working directory.  Upstream cases use
  # relative module and file paths, so invoking Sol from the repository root
  # would turn a harness artifact into a false compatibility mismatch.
  ( cd "$suite_dir" && SOL_LUA_INSTRUCTION_BUDGET=$budget SOL_LUA_ALLOCATION_BUDGET=$alloc_budget "$sol_bin" run "$target" >"$sol_out" 2>"$sol_err" )
  sol_status=$?
  ( cd "$suite_dir" && "$reference_bin" "$path" >"$ref_out" 2>"$ref_err" )
  ref_status=$?

  reasons=()
  diff -u "$ref_out" "$sol_out" >"$results_dir/$path.stdout.diff" || reasons+=("stdout")

  sol_failed=1; [[ $sol_status -eq 0 ]] && sol_failed=0
  ref_failed=1; [[ $ref_status -eq 0 ]] && ref_failed=0
  [[ $sol_failed -eq $ref_failed ]] || reasons+=("exit-status(sol=$sol_status ref=$ref_status)")

  sol_err_present=1; [[ -s "$sol_err" ]] || sol_err_present=0
  ref_err_present=1; [[ -s "$ref_err" ]] || ref_err_present=0
  [[ $sol_err_present -eq $ref_err_present ]] || reasons+=("stderr-presence(sol=$sol_err_present ref=$ref_err_present)")

  if [[ ${#reasons[@]} -eq 0 ]]; then
    printf 'MATCH %-18s\n' "$path"
    matched=$((matched + 1))
  else
    printf 'DIVG  %-18s %s (logs: %s)\n' "$path" "${reasons[*]}" "$results_dir/$path.{stdout,sol,reference}.*"
    diverged=$((diverged + 1))
    {
      echo "## $path"
      echo
      echo "- source: $target"
      echo "- status: $status"
      echo "- category: ${category:-<none>}"
      echo "- requires (capability profile): ${requires:-<none>}"
      echo "- seed: n/a (fixed corpus fixture, not a generated/fuzzed input)"
      echo "- diverged on: ${reasons[*]}"
      echo "- logs: $sol_out $sol_err $ref_out $ref_err $results_dir/$path.stdout.diff"
      if [[ -s "$results_dir/$path.stdout.diff" ]]; then
        echo
        echo '```diff'
        head -n 20 "$results_dir/$path.stdout.diff"
        echo '```'
      fi
      echo
    } >>"$failure_report"
  fi
done <"$entries_file"

printf '\nLua 5.5 differential: %d match, %d diverge, %d skipped\n' "$matched" "$diverged" "$skipped"

fuzz_matched=0
fuzz_diverged=0
if [[ $fuzz_cases -gt 0 ]]; then
  fuzz_fixtures_dir="$ROOT/tests/lua55/fuzz-fixtures"
  for ((case_index = 0; case_index < fuzz_cases; case_index++)); do
    case_seed=$(( fuzz_seed + case_index ))
    fuzz_generate_case "$case_seed"
    label="fuzz-case-$case_seed"
    fuzz_file="$results_dir/$label.lua"
    printf '%s' "$case_source" >"$fuzz_file"

    sol_out="$results_dir/$label.sol.stdout"
    sol_err="$results_dir/$label.sol.stderr"
    ref_out="$results_dir/$label.reference.stdout"
    ref_err="$results_dir/$label.reference.stderr"

    "$sol_bin" run "$fuzz_file" >"$sol_out" 2>"$sol_err"
    sol_status=$?
    "$reference_bin" "$fuzz_file" >"$ref_out" 2>"$ref_err"
    ref_status=$?

    reasons=()
    diff -u "$ref_out" "$sol_out" >"$results_dir/$label.stdout.diff" || reasons+=("stdout")

    sol_failed=1; [[ $sol_status -eq 0 ]] && sol_failed=0
    ref_failed=1; [[ $ref_status -eq 0 ]] && ref_failed=0
    [[ $sol_failed -eq $ref_failed ]] || reasons+=("exit-status(sol=$sol_status ref=$ref_status)")

    sol_err_present=1; [[ -s "$sol_err" ]] || sol_err_present=0
    ref_err_present=1; [[ -s "$ref_err" ]] || ref_err_present=0
    [[ $sol_err_present -eq $ref_err_present ]] || reasons+=("stderr-presence(sol=$sol_err_present ref=$ref_err_present)")

    if [[ ${#reasons[@]} -eq 0 ]]; then
      printf 'MATCH %-18s (seed %s)\n' "$label" "$case_seed"
      fuzz_matched=$((fuzz_matched + 1))
    else
      printf 'DIVG  %-18s %s (seed %s, logs: %s)\n' "$label" "${reasons[*]}" "$case_seed" "$results_dir/$label.*"
      fuzz_diverged=$((fuzz_diverged + 1))
      mkdir -p "$fuzz_fixtures_dir"
      fixture_path="$fuzz_fixtures_dir/case-$case_seed.lua"
      cp "$fuzz_file" "$fixture_path"
      {
        echo "## $label"
        echo
        echo "- source: $fixture_path (generated; promoted to a permanent fixture)"
        echo "- seed: $case_seed"
        echo "- diverged on: ${reasons[*]}"
        echo "- logs: $sol_out $sol_err $ref_out $ref_err $results_dir/$label.stdout.diff"
        if [[ -s "$results_dir/$label.stdout.diff" ]]; then
          echo
          echo '```diff'
          head -n 20 "$results_dir/$label.stdout.diff"
          echo '```'
        fi
        echo
      } >>"$failure_report"
    fi
  done
  printf '\nLua 5.5 differential fuzz (seed=%d, %d cases): %d match, %d diverge\n' \
    "$fuzz_seed" "$fuzz_cases" "$fuzz_matched" "$fuzz_diverged"
fi

if [[ -n "$keep_results" ]]; then
  printf 'Logs: %s\n' "$results_dir"
  if [[ $((diverged + fuzz_diverged)) -gt 0 ]]; then
    printf 'Failure report: %s\n' "$failure_report"
  fi
else
  rm -rf "$results_dir"
fi
