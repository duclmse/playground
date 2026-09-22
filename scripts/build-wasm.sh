#!/usr/bin/env bash
# The old generator depended on the retired vendored crates/vm fork. The
# checked-in package remains usable while the sol-core browser adapter lands.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./lib.sh

die "crates/vm has been retired; implement the sol-core WASM adapter before regenerating packages/lua-runtime/pkg"
