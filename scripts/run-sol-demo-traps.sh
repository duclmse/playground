#!/usr/bin/env bash
# Runs each examples/sol-demo/traps/*.sol fixture and checks that it aborts.
# Per docs/spec/execution-and-runtime.md, array bounds errors, invalid
# checked casts, division by zero, and invalid allocation sizes are typed
# runtime contract violations that trap - they are not catchable as Lua
# errors, and are not recoverable "wrong answers". So "the process aborts"
# is the correct, expected outcome for every fixture here, not a bug in the
# fixture or the runner.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

require_cmd cargo "Install Rust: https://rustup.rs"

ensure_sol_bin release
sol_bin=$SOL_BIN

trap_dir="$ROOT/examples/sol-demo/traps"
fail=0
for fixture in "$trap_dir"/*.sol; do
  name=$(basename "$fixture")
  if "$sol_bin" run "$fixture" >/dev/null 2>&1; then
    echo "FAIL: $name exited 0 (expected a trap)"
    fail=1
  else
    echo "OK: $name trapped as expected"
  fi
done

[[ "$fail" -eq 0 ]] || die "one or more trap fixtures did not trap as expected"
log "All examples/sol-demo/traps/*.sol fixtures trapped as expected."
