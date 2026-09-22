#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT

cargo build --manifest-path "$repo_root/crates/sol/Cargo.toml" --lib

case "$(uname -s)" in
  Darwin)
    library_dir="$repo_root/target/debug"
    shared_ext=dylib
    export_name=libsol.dylib
    ;;
  Linux)
    library_dir="$repo_root/target/debug"
    shared_ext=so
    export_name=libsol.so
    ;;
  *)
    echo "native C API fixture is unsupported on $(uname -s)" >&2
    exit 77
    ;;
esac

cc -std=c11 -Wall -Wextra -Werror \
  -I"$repo_root/crates/sol/include" \
  "$repo_root/crates/sol/tests/native/embedding_smoke.c" \
  -L"$library_dir" -lsol -o "$build_dir/embedding_smoke"

cc -std=c11 -Wall -Wextra -Werror -shared -fPIC \
  -I"$repo_root/crates/sol/include" \
  "$repo_root/crates/sol/tests/native/sol_fixture.c" \
  -L"$library_dir" -lsol -o "$build_dir/sol_fixture.$shared_ext"

if [[ "$(uname -s)" == Darwin ]]; then
  DYLD_LIBRARY_PATH="$library_dir" "$build_dir/embedding_smoke" "$build_dir/sol_fixture.$shared_ext"
else
  LD_LIBRARY_PATH="$library_dir" "$build_dir/embedding_smoke" "$build_dir/sol_fixture.$shared_ext"
fi

test -f "$library_dir/$export_name"
echo "Sol Lua 5.5 C API and native fixture build passed"
