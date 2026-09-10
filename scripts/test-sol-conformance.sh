#!/usr/bin/env bash
# Run Sol's currently supported Lua conformance profile with exact output.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"

sol_bin=${SOL_BIN:-"$ROOT/crates/sol/target/debug/sol"}
if [[ ! -x "$sol_bin" ]]; then
  cargo build --offline --manifest-path "$ROOT/crates/sol/Cargo.toml"
fi

fixtures=(
  "$ROOT/crates/sol/tests/fixtures/lua55/dynamic_core.lua"
)
while IFS= read -r fixture; do fixtures+=("$fixture"); done < <(find "$ROOT/crates/sol/tests/fixtures/lua55/native" -maxdepth 1 -name '*.lua' -type f | sort)

passed=0
for fixture in "${fixtures[@]}"; do
  output="$($sol_bin run "$fixture")"
  if [[ "$output" != true ]]; then
    die "$(basename "$fixture"): expected 'true', got '$output'"
  fi
  printf 'PASS  %s\n' "$(basename "$fixture")"
  passed=$((passed + 1))
done

printf 'Sol Lua conformance: %d supported fixtures passed\n' "$passed"
