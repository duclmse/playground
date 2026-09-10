#!/usr/bin/env bash
# Record a per-case Lua 5.5.1 reference baseline. This script never downloads
# source or installs packages; callers explicitly provide the pinned source.

set -u

root_dir=$(cd "$(dirname "$0")/.." && pwd)
suite_dir=${1:-"$root_dir/lua-5.5.1-tests"}
source_dir=${LUA55_SOURCE_DIR:-"$root_dir/lua-5.5.1"}
archive=${LUA55_SOURCE_ARCHIVE:-"$root_dir/lua-5.5.1.tar.gz"}
expected_sha256=1c4b4068d67061f2a2231ad2b5422e77acea1487ea9890f6320af614f4373dce
lua_bin=${LUA55_REFERENCE_BIN:-"$source_dir/src/lua"}
results_dir=${LUA55_REFERENCE_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/lua55-reference.XXXXXX")"}
keep_results=${LUA55_REFERENCE_RESULTS_DIR:+1}
snapshot_dir=${LUA55_REFERENCE_SNAPSHOTS_DIR:-"$root_dir/tests/lua55/reference"}
case_timeout=${LUA55_REFERENCE_CASE_TIMEOUT_SECONDS:-0}

usage() {
  cat <<'EOF'
Usage: scripts/test-lua55-reference.sh [lua-5.5.1-tests-directory]

Environment:
  LUA55_SOURCE_DIR             Unpacked official Lua 5.5.1 source directory
  LUA55_SOURCE_ARCHIVE         Optional official source archive to checksum
  LUA55_REFERENCE_BIN          Built Lua 5.5.1 executable (defaults to src/lua)
  LUA55_BUILD=1                Build the reference executable from source
  LUA55_BUILD_LIBS=1           Build the suite's C modules using Lua headers
  LUA55_REFERENCE_CASES        Comma-separated top-level cases for a smoke run
  LUA55_REFERENCE_SNAPSHOTS_DIR  Optional deterministic stdout snapshot directory
  LUA55_REFERENCE_CASE_TIMEOUT_SECONDS  Per-case timeout when GNU timeout exists
  LUA55_REFERENCE_RESULTS_DIR  Preserve stdout/stderr/status/metadata here

The pinned Lua 5.5.1 source SHA-256 is:
  1c4b4068d67061f2a2231ad2b5422e77acea1487ea9890f6320af614f4373dce

For the full upstream distribution run, use a Linux profile with dynamic
loading, readline fallback, /dev/full, and an ISO-8859-1 Portuguese locale.
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
if [[ -f "$archive" ]]; then
  actual_sha256=$(sha256sum "$archive" | awk '{print $1}')
  if [[ $actual_sha256 != "$expected_sha256" ]]; then
    echo "Lua source checksum mismatch for $archive: $actual_sha256" >&2
    exit 2
  fi
fi
if [[ ! -d "$source_dir" ]]; then
  echo "Lua 5.5.1 source directory not found: $source_dir" >&2
  exit 2
fi
if [[ ${LUA55_BUILD:-0} == 1 ]]; then
  target=linux-readline
  if [[ $(uname -s) == Darwin ]]; then
    target=macosx
  fi
  make -C "$source_dir" "$target" >&2 || exit $?
fi
if [[ ! -x "$lua_bin" ]]; then
  echo "Lua 5.5.1 executable not found: $lua_bin (set LUA55_BUILD=1 to build)" >&2
  exit 2
fi
if ! lua_version=$("$lua_bin" -v 2>&1); then
  echo "Lua 5.5.1 executable cannot run on this host: $lua_bin" >&2
  echo "$lua_version" >&2
  exit 2
fi
if [[ $lua_version != Lua\ 5.5.1* ]]; then
  echo "Lua reference is not Lua 5.5.1: $lua_version" >&2
  exit 2
fi
if [[ ${LUA55_BUILD_LIBS:-0} == 1 ]]; then
  make -C "$suite_dir/libs" LUA_DIR="$source_dir/src" >&2 || exit $?
fi

mkdir -p "$results_dir"
{
  printf 'lua_bin=%s\n' "$lua_bin"
  printf 'lua_version=%s\n' "$lua_version"
  printf 'source_dir=%s\n' "$source_dir"
  printf 'source_sha256=%s\n' "$expected_sha256"
  printf 'suite_dir=%s\n' "$suite_dir"
  printf 'platform=%s\n' "$(uname -a)"
  printf 'locale=%s\n' "${LC_ALL:-${LANG:-unset}}"
  printf 'lua_path=%s\n' "${LUA_PATH:-./?.lua;;}"
  printf 'lua_cpath=%s\n' "${LUA_CPATH:-./libs/?.so;;}"
  printf 'snapshot_dir=%s\n' "$snapshot_dir"
  printf 'case_timeout_seconds=%s\n' "$case_timeout"
} >"$results_dir/environment.txt"

passed=0
failed=0
executed=0
selected_cases=${LUA55_REFERENCE_CASES:-}
for case_file in "$suite_dir"/*.lua; do
  [[ -f "$case_file" ]] || continue
  case_name=$(basename "$case_file")
  if [[ -n "$selected_cases" && ",$selected_cases," != *",$case_name,"* ]]; then
    continue
  fi
  executed=$((executed + 1))
  start=$(date +%s)
  lua_path=${LUA_PATH:-./?.lua;;}
  lua_cpath=${LUA_CPATH:-./libs/?.so;;}
  if [[ $case_timeout != 0 ]] && command -v timeout >/dev/null 2>&1; then
    run_command=(timeout "$case_timeout" "$BASH" -c 'cd "$1" || exit; LUA_PATH="$4" LUA_CPATH="$5" "$2" "$3"' _ "$suite_dir" "$lua_bin" "$case_name" "$lua_path" "$lua_cpath")
  else
    run_command=("$BASH" -c 'cd "$1" || exit; LUA_PATH="$4" LUA_CPATH="$5" "$2" "$3"' _ "$suite_dir" "$lua_bin" "$case_name" "$lua_path" "$lua_cpath")
  fi
  if "${run_command[@]}" >"$results_dir/$case_name.stdout" 2>"$results_dir/$case_name.stderr"; then
    status=0
  else
    status=$?
  fi
  snapshot="$snapshot_dir/$case_name.stdout"
  if [[ $status -eq 0 && -f "$snapshot" ]] && ! diff -u "$snapshot" "$results_dir/$case_name.stdout" >"$results_dir/$case_name.snapshot.diff"; then
    status=1
    printf 'FAIL  %s  deterministic stdout snapshot differs\n' "$case_name"
  fi
  if [[ $status -eq 0 ]]; then
    rm -f "$results_dir/$case_name.snapshot.diff"
    passed=$((passed + 1))
    printf 'PASS  %s\n' "$case_name"
  else
    failed=$((failed + 1))
    first_line=$(sed -n '1p' "$results_dir/$case_name.stderr")
    if [[ $status -eq 124 ]]; then
      printf 'FAIL  %s  timed out after %s seconds\n' "$case_name" "$case_timeout"
    elif [[ -n "$first_line" ]]; then
      printf 'FAIL  %s  %s\n' "$case_name" "$first_line"
    fi
  fi
  end=$(date +%s)
  {
    printf 'case=%s\n' "$case_name"
    printf 'command=%s %s\n' "$lua_bin" "$case_name"
    printf 'exit=%d\n' "$status"
    printf 'elapsed_seconds=%d\n' "$((end - start))"
  } >"$results_dir/$case_name.meta"
done

if [[ $executed -eq 0 ]]; then
  echo "No requested Lua 5.5 reference cases were found: $selected_cases" >&2
  exit 2
fi
printf '\nLua 5.5 reference: %d passed, %d failed\n' "$passed" "$failed"
if [[ -n "$keep_results" ]]; then
  printf 'Results: %s\n' "$results_dir"
else
  rm -rf "$results_dir"
fi

if [[ $failed -ne 0 ]]; then
  exit 1
fi
