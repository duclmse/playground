#!/usr/bin/env bash
# Shared helpers sourced by the other scripts in this directory. Not meant
# to be run directly. Callers own their shell-option policy; sourcing a helper
# must not silently enable `errexit` in runners that intentionally inspect
# failing child commands.

# Repo root, regardless of where a script is invoked from.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Must match the `wasm-bindgen = "=X.Y.Z"` pin in crates/lua-vm/Cargo.toml -
# the wasm-bindgen CLI and the crate's wasm-bindgen dependency have to be
# the exact same version or the generated JS glue fails at runtime.
WASM_BINDGEN_VERSION="0.2.100"
LUA55_SOURCE_SHA256="1c4b4068d67061f2a2231ad2b5422e77acea1487ea9890f6320af614f4373dce"

log() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' not found on PATH. $2"
}

cargo_target_root() {
  local target=${CARGO_TARGET_DIR:-"$ROOT/target"}
  case "$target" in
    /*) printf '%s\n' "$target" ;;
    *) printf '%s/%s\n' "$ROOT" "$target" ;;
  esac
}

ensure_sol_bin() {
  local profile=${1:-debug}
  [[ "$profile" == debug || "$profile" == release ]] \
    || die "unknown Sol build profile: $profile"
  local target_root
  target_root=$(cargo_target_root)
  if [[ -z ${SOL_BIN:-} ]]; then
    SOL_BIN="$target_root/$profile/sol"
  fi
  if [[ ! -x "$SOL_BIN" ]]; then
    require_cmd cargo "Install Rust: https://rustup.rs"
    local args=(build --offline --manifest-path "$ROOT/crates/sol/Cargo.toml")
    [[ "$profile" == release ]] && args+=(--release)
    CARGO_TARGET_DIR="$target_root" cargo "${args[@]}"
  fi
}

sha256_file() {
  local path=$1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    die "neither sha256sum nor shasum is available to verify $path"
  fi
}

# Sol's CLI prints a top-level chunk return value, unlike PUC Lua's CLI. Remove
# one known trailing return line before comparing otherwise-identical stdout.
strip_sol_cli_return_line() {
  local input=$1 output=$2 return_line=$3
  awk -v returned="$return_line" '
    { lines[NR] = $0 }
    END {
      n = NR
      if (n > 0 && lines[n] == returned) n--
      for (i = 1; i <= n; i++) print lines[i]
    }
  ' "$input" >"$output"
}

# Parse the deliberately narrow TOML subset used by tests/lua55/manifest.toml.
# Output fields are separated by ASCII FS and always ordered as:
# path, status, category, requires, fixture, note.
parse_lua55_manifest() {
  local manifest=$1 entries_file=$2
  awk '
function trim(s) { sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); return s }
function die(message) { print "manifest: " message > "/dev/stderr"; bad = 1 }
function string_value(value, key) {
  value = trim(value)
  if (substr(value, 1, 1) != "\"" || substr(value, length(value), 1) != "\"") {
    die("expected quoted string for " key " at line " NR)
    return ""
  }
  return substr(value, 2, length(value) - 2)
}
function array_value(value, key) {
  value = trim(value)
  if (substr(value, 1, 1) != "[" || substr(value, length(value), 1) != "]") {
    die("expected string array for " key " at line " NR)
    return ""
  }
  value = substr(value, 2, length(value) - 2)
  gsub(/[[:space:]]/, "", value)
  if (value == "") return ""
  if (value !~ /^"[^"]+"(,"[^"]+")*$/) {
    die("expected string array for " key " at line " NR)
    return ""
  }
  gsub(/"/, "", value)
  return value
}
function emit() {
  if (!in_case) return
  if (path == "" || category == "" || status == "" || note == "") {
    die("case starting at line " case_line " must contain path, category, status, and note")
  }
  if (status != "pass" && status != "adapted" && status != "host-required" && status != "pending" && status != "diverges") {
    die("unknown status \047" status "\047 for " path)
  }
  if (seen[path]++) die("duplicate path " path)
  print path "\034" status "\034" category "\034" requires "\034" fixture "\034" note
}
BEGIN { in_case = 0; bad = 0 }
/^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
/^[[:space:]]*\[\[case\]\][[:space:]]*$/ {
  emit(); in_case = 1; case_line = NR
  path = category = status = requires = fixture = note = ""
  next
}
{
  pos = index($0, "=")
  if (!in_case) {
    if (pos == 0) die("expected top-level key or [[case]] at line " NR)
    next
  }
  if (pos == 0) { die("expected key/value pair at line " NR); next }
  key = trim(substr($0, 1, pos - 1)); value = trim(substr($0, pos + 1))
  if (key == "path" || key == "category" || key == "status" || key == "fixture" || key == "note") {
    value = string_value(value, key)
  } else if (key == "requires") {
    value = array_value(value, key)
  } else {
    die("unknown case key " key " at line " NR); next
  }
  if (key == "path") path = value
  else if (key == "category") category = value
  else if (key == "status") status = value
  else if (key == "requires") requires = value
  else if (key == "fixture") fixture = value
  else if (key == "note") note = value
}
END { emit(); exit bad }
' "$manifest" >"$entries_file"
}

validate_lua55_corpus_coverage() {
  local entries_file=$1 suite_dir=$2
  local manifest_paths='|' manifest_count=0 corpus_count=0
  local path status category requires fixture note case_file case_name
  while IFS=$'\034' read -r path status category requires fixture note; do
    if [[ $path == /* || $path == */* || $path == *".."* || ! -f "$suite_dir/$path" ]]; then
      printf 'manifest: source path is not a top-level corpus file: %s\n' "$path" >&2
      return 1
    fi
    case "$manifest_paths" in
      *"|$path|"*)
        printf 'manifest: duplicate source path: %s\n' "$path" >&2
        return 1
        ;;
    esac
    manifest_paths="${manifest_paths}${path}|"
    manifest_count=$((manifest_count + 1))
  done <"$entries_file"

  for case_file in "$suite_dir"/*.lua; do
    [[ -f "$case_file" ]] || continue
    case_name=$(basename "$case_file")
    corpus_count=$((corpus_count + 1))
    case "$manifest_paths" in
      *"|$case_name|"*) ;;
      *)
        printf 'manifest: missing corpus entry for %s\n' "$case_name" >&2
        return 1
        ;;
    esac
  done
  if [[ $manifest_count -ne $corpus_count ]]; then
    printf 'manifest: has %d entries for %d top-level corpus files\n' "$manifest_count" "$corpus_count" >&2
    return 1
  fi
  LUA55_MANIFEST_COUNT=$manifest_count
  LUA55_CORPUS_COUNT=$corpus_count
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
