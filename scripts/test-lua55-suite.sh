#!/usr/bin/env bash
# Run every top-level Lua 5.5 conformance case through Sol and retain a
# per-file compiler/runtime log. The suite deliberately reports unsupported
# features as failures; set SOL_LUA55_REQUIRE_PASS=1 to make those fail CI.

set -u

root_dir=$(cd "$(dirname "$0")/.." && pwd)
suite_dir=${1:-"$root_dir/lua-5.5.1-tests"}
sol_bin=${SOL_BIN:-"$root_dir/crates/sol/target/debug/sol"}
results_dir=${SOL_LUA55_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/sol-lua55.XXXXXX")"}
keep_results=${SOL_LUA55_RESULTS_DIR:+1}

mkdir -p "$results_dir"

if [[ ! -d "$suite_dir" ]]; then
  echo "Lua 5.5 suite directory not found: $suite_dir" >&2
  exit 2
fi

if [[ ! -x "$sol_bin" ]]; then
  cargo build --offline --manifest-path "$root_dir/crates/sol/Cargo.toml" >&2 || exit $?
fi

passed=0
failed=0
for case_file in "$suite_dir"/*.lua; do
  [[ -f "$case_file" ]] || continue
  case_name=$(basename "$case_file")
  if "$sol_bin" run "$case_file" >"$results_dir/$case_name.log" 2>&1; then
    printf 'PASS  %s\n' "$case_name"
    passed=$((passed + 1))
  else
    first_line=$(sed -n '1p' "$results_dir/$case_name.log")
    printf 'FAIL  %s  %s\n' "$case_name" "$first_line"
    failed=$((failed + 1))
  fi
done

printf '\nLua 5.5 corpus: %d passed, %d failed\n' "$passed" "$failed"
if [[ -n "$keep_results" ]]; then
  printf 'Logs: %s\n' "$results_dir"
else
  rm -rf "$results_dir"
fi

if [[ ${SOL_LUA55_REQUIRE_PASS:-0} == 1 && $failed -ne 0 ]]; then
  exit 1
fi
