#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
source "$repo_root/scripts/lib.sh"
source_dir=${SOL_LUA55_TESTS:-"$repo_root/lua-5.5.1-tests"}
build_dir=$(mktemp -d)
if [[ ${SOL_KEEP_NATIVE_BUILD:-0} == 1 ]]; then
  echo "retaining native corpus build at $build_dir" >&2
else
  trap 'rm -rf "$build_dir"' EXIT
fi
compiler=${CC:-cc}
reference_bin=${LUA55_REFERENCE_BIN:-/opt/homebrew/bin/lua5.5}

[[ -f "$source_dir/attrib.lua" ]] || {
  echo "Lua 5.5.1 corpus checkout not found at $source_dir" >&2
  exit 77
}

cargo build --manifest-path "$repo_root/crate/sol/Cargo.toml" --lib --bin sol
mkdir -p "$build_dir/libs"
cp "$source_dir/attrib.lua" "$build_dir/attrib.lua"
cp -R "$source_dir/libs/P1" "$build_dir/libs/P1"

common_flags=(
  -std=c11 -Wall -Wextra -Wno-unused-parameter -shared -fPIC
  -I"$repo_root/crate/sol/include"
  -L"$repo_root/target/debug" -lsol
)

unresolved_flags=()
if [[ "$(uname -s)" == Darwin ]]; then
  unresolved_flags=(-Wl,-undefined,dynamic_lookup)
fi

"$compiler" "${common_flags[@]}" "$source_dir/libs/lib1.c" -o "$build_dir/libs/lib1.so"
"$compiler" "${common_flags[@]}" "${unresolved_flags[@]}" "$source_dir/libs/lib11.c" -o "$build_dir/libs/lib11.so"
"$compiler" "${common_flags[@]}" "$source_dir/libs/lib2.c" -o "$build_dir/libs/lib2.so"
"$compiler" "${common_flags[@]}" "${unresolved_flags[@]}" "$source_dir/libs/lib21.c" -o "$build_dir/libs/lib21.so"
"$compiler" "${common_flags[@]}" "$source_dir/libs/lib22.c" -o "$build_dir/libs/lib2-v2.so"

if [[ "$(uname -s)" == Darwin ]]; then
  (cd "$build_dir" && DYLD_LIBRARY_PATH="$repo_root/target/debug" \
    "$repo_root/target/debug/sol" run attrib.lua) >"$build_dir/sol.stdout"
else
  (cd "$build_dir" && LD_LIBRARY_PATH="$repo_root/target/debug" \
    "$repo_root/target/debug/sol" run attrib.lua) >"$build_dir/sol.stdout"
fi

if [[ -x "$reference_bin" ]]; then
  (cd "$source_dir" && "$reference_bin" attrib.lua) >"$build_dir/reference.stdout"
  strip_sol_cli_return_line "$build_dir/sol.stdout" "$build_dir/sol.stdout.compare" 27
  diff -u "$build_dir/reference.stdout" "$build_dir/sol.stdout.compare"
else
  echo "pinned Lua 5.5.1 executable unavailable; native corpus ran without oracle diff" >&2
fi
cat "$build_dir/sol.stdout"
