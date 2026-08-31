#!/usr/bin/env bash
# Shared helpers sourced by the other scripts in this directory. Not meant
# to be run directly.
set -euo pipefail

# Repo root, regardless of where a script is invoked from.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Must match the `wasm-bindgen = "=X.Y.Z"` pin in crates/lua-vm/Cargo.toml —
# the wasm-bindgen CLI and the crate's wasm-bindgen dependency have to be
# the exact same version or the generated JS glue fails at runtime.
WASM_BINDGEN_VERSION="0.2.100"

log() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' not found on PATH. $2"
}

require_wasm_target() {
  rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$' \
    || die "wasm32-unknown-unknown target not installed. Run: rustup target add wasm32-unknown-unknown"
}

require_wasm_bindgen_cli() {
  require_cmd wasm-bindgen "Install with: cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION} --locked"
  local installed
  installed="$(wasm-bindgen --version | awk '{print $2}')"
  [ "$installed" = "$WASM_BINDGEN_VERSION" ] || die \
    "wasm-bindgen CLI is v${installed}, but crates/lua-vm/Cargo.toml pins v${WASM_BINDGEN_VERSION}. Run: cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION} --locked"
}
