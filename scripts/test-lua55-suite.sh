#!/usr/bin/env bash
# Run the Lua 5.5.1 top-level corpus through Sol with a checked manifest.
# The manifest prevents C API/host tests from being presented as unexplained
# language failures while still allowing an explicit raw diagnostic run.

set -u

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "$script_dir/lib.sh"

suite_dir=${1:-"$ROOT/lua-5.5.1-tests"}
manifest=${SOL_LUA55_MANIFEST:-"$ROOT/tests/lua55/manifest.toml"}
results_dir=${SOL_LUA55_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/sol-lua55.XXXXXX")"}
keep_results=${SOL_LUA55_RESULTS_DIR:+1}
run_pending=${SOL_LUA55_RUN_PENDING:-1}
run_host_required=${SOL_LUA55_RUN_HOST_REQUIRED:-0}
validate_only=${SOL_LUA55_VALIDATE_ONLY:-0}

usage() {
  cat <<'EOF'
Usage: scripts/test-lua55-suite.sh [lua-5.5.1-tests-directory]

Environment:
  SOL_BIN                       Sol executable (default: debug build)
  SOL_LUA55_MANIFEST            TOML manifest to validate and run
  SOL_LUA55_RESULTS_DIR         Preserve logs in this directory
  SOL_LUA55_RUN_PENDING=0       Classify pending cases without invoking Sol
  SOL_LUA55_RUN_HOST_REQUIRED=1 Also run host-required cases for diagnostics
  SOL_LUA55_VALIDATE_ONLY=1     Validate manifest coverage and exit
  SOL_LUA55_REQUIRE_PASS=1      Fail unless every manifest case passes
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
suite_dir=$(cd "$suite_dir" && pwd)
if [[ ! -f "$manifest" ]]; then
  echo "Lua 5.5 manifest not found: $manifest" >&2
  exit 2
fi

mkdir -p "$results_dir"
entries_file="$results_dir/manifest.tsv"

if ! parse_lua55_manifest "$manifest" "$entries_file"; then
  rm -rf "$results_dir"
  exit 2
fi
if ! validate_lua55_corpus_coverage "$entries_file" "$suite_dir"; then
  rm -rf "$results_dir"
  exit 2
fi
manifest_count=$LUA55_MANIFEST_COUNT
corpus_count=$LUA55_CORPUS_COUNT

if [[ $validate_only == 1 ]]; then
  printf 'Lua 5.5 manifest: %d entries cover %d top-level files\n' "$manifest_count" "$corpus_count"
  rm -rf "$results_dir"
  exit 0
fi

ensure_sol_bin debug || exit $?
sol_bin=$SOL_BIN

passed=0
pending=0
skipped=0
failed=0
diverged=0
run_case() {
  local path=$1
  local status=$2
  local fixture=$3
  local budget=$4
  local alloc_budget=$5
  local source="$suite_dir/$path"
  local target=$source
  if [[ -n "$fixture" ]]; then
    target="$ROOT/$fixture"
  fi
  if [[ ! -f "$target" ]]; then
    printf 'FAIL  %-18s manifest fixture not found: %s\n' "$path" "$target"
    failed=$((failed + 1))
    return
  fi
  # Corpus cases use relative `require`, `dofile`, and filesystem paths.
  # Run Sol from the same directory as the pinned Lua test driver, just as
  # the reference runner does, while keeping `target` absolute for adapted
  # fixtures outside that directory.
  if (cd "$suite_dir" && SOL_LUA_INSTRUCTION_BUDGET=$budget SOL_LUA_ALLOCATION_BUDGET=$alloc_budget "$sol_bin" run "$target") >"$results_dir/$path.log" 2>&1; then
    case "$status" in
      pass|adapted)
        printf 'PASS  %-18s %s\n' "$path" "$status"
        passed=$((passed + 1))
        ;;
      pending)
        printf 'PEND  %-18s unexpectedly runs; classify after oracle comparison\n' "$path"
        pending=$((pending + 1))
        ;;
      diverges)
        printf 'DIVG  %-18s documented divergence\n' "$path"
        diverged=$((diverged + 1))
        ;;
    esac
  else
    first_line=$(sed -n '1p' "$results_dir/$path.log")
    case "$status" in
      pending)
        printf 'PEND  %-18s %s\n' "$path" "$first_line"
        pending=$((pending + 1))
        ;;
      diverges)
        printf 'DIVG  %-18s %s\n' "$path" "$first_line"
        diverged=$((diverged + 1))
        ;;
      *)
        printf 'FAIL  %-18s %s\n' "$path" "$first_line"
        failed=$((failed + 1))
        ;;
    esac
  fi
}

while IFS=$'\034' read -r path status category requires fixture note budget alloc_budget; do
  case "$status" in
    host-required)
      if [[ $run_host_required == 1 ]]; then
        run_case "$path" pending "$fixture" "$budget" "$alloc_budget"
      else
        printf 'SKIP  %-18s requires: %s\n' "$path" "${requires:-declared host capability}"
        skipped=$((skipped + 1))
      fi
      ;;
    pending)
      if [[ $run_pending == 1 ]]; then
        run_case "$path" "$status" "$fixture" "$budget" "$alloc_budget"
      else
        printf 'PEND  %-18s %s\n' "$path" "$note"
        pending=$((pending + 1))
      fi
      ;;
    pass|adapted|diverges)
      run_case "$path" "$status" "$fixture" "$budget" "$alloc_budget"
      ;;
  esac
done <"$entries_file"

printf '\nLua 5.5 corpus: %d passed, %d pending, %d host-required, %d diverges, %d failed\n' \
  "$passed" "$pending" "$skipped" "$diverged" "$failed"
if [[ -n "$keep_results" ]]; then
  printf 'Logs: %s\n' "$results_dir"
else
  rm -rf "$results_dir"
fi

if [[ ${SOL_LUA55_REQUIRE_PASS:-0} == 1 ]] && ((passed != manifest_count)); then
  exit 1
fi
