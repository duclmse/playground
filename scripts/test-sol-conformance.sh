#!/usr/bin/env bash
# Run focused, currently supported Lua fixtures with exact output. Passing
# these fixtures does not promote a complete upstream corpus file.
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

printf 'Supported Lua fixture regressions: %d passed (0 complete upstream cases implied)\n' "$passed"
