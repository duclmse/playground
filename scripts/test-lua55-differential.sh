#!/usr/bin/env bash
# Differential runner: run each Lua 5.5 corpus case through both Sol and a
# pinned reference Lua 5.5.1 build, and diff their stdout directly. This is
# distinct from scripts/test-lua55-suite.sh (Sol vs. the manifest's declared
# expectation) and scripts/test-lua55-reference.sh (reference Lua vs. checked-in
# tests/lua55/reference/*.stdout snapshots): neither of those compares Sol's
# and reference Lua's output to each other on the same case. This script never
# substitutes the system `lua`/`lua5.5` binary as the oracle - see
# tests/lua55/README.md and AGENTS.md. It requires an explicit,
# pinned-source-built reference executable (LUA55_REFERENCE_BIN, matching the
# convention scripts/test-lua55-reference.sh already uses), and refuses to run
# if that executable is missing rather than silently falling back to
# whatever `lua` happens to be on PATH.
set -u

root_dir=$(cd "$(dirname "$0")/.." && pwd)
suite_dir=${1:-"$root_dir/lua-5.5.1-tests"}
manifest=${SOL_LUA55_MANIFEST:-"$root_dir/tests/lua55/manifest.toml"}
sol_bin=${SOL_BIN:-"$root_dir/crates/sol/target/debug/sol"}
source_dir=${LUA55_SOURCE_DIR:-"$root_dir/lua-5.5.1"}
reference_bin=${LUA55_REFERENCE_BIN:-"$source_dir/src/lua"}
run_pending=${SOL_LUA55_DIFF_RUN_PENDING:-1}
results_dir=${SOL_LUA55_DIFF_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/sol-lua55-diff.XXXXXX")"}
keep_results=${SOL_LUA55_DIFF_RESULTS_DIR:+1}

usage() {
  cat <<'EOF'
Usage: scripts/test-lua55-differential.sh [lua-5.5.1-tests-directory]

Diffs Sol's stdout against a pinned reference Lua 5.5.1 build's stdout, case
by case, for every manifest entry (by default including `pending` cases, so a
case that already agrees with the reference can be reclassified).

Environment:
  SOL_BIN                    Sol executable (default: debug build)
  SOL_LUA55_MANIFEST         TOML manifest to read cases from
  LUA55_REFERENCE_BIN        Built Lua 5.5.1 executable (default:
                              lua-5.5.1/src/lua) - must come from the pinned
                              source in tests/lua55/README.md; this script
                              will not substitute the system `lua` binary
  SOL_LUA55_DIFF_RUN_PENDING=0  Only diff pass/adapted/diverges cases
  SOL_LUA55_DIFF_RESULTS_DIR  Preserve per-case stdout/diff logs here

This requires a real pinned-source build of reference Lua 5.5.1
(scripts/test-lua55-reference.sh's LUA55_BUILD=1 builds one); it exits
immediately, without running anything, if LUA55_REFERENCE_BIN is not an
executable file.
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
if [[ ! -f "$manifest" ]]; then
  echo "Lua 5.5 manifest not found: $manifest" >&2
  exit 2
fi
if [[ ! -x "$reference_bin" ]]; then
  cat >&2 <<EOF
Reference Lua 5.5.1 executable not found or not executable: $reference_bin

This script deliberately does not fall back to the system \`lua\`/\`lua5.5\`
binary as the oracle (see tests/lua55/README.md). Build the pinned reference
first, e.g.:

  LUA55_BUILD=1 scripts/test-lua55-reference.sh

or point LUA55_REFERENCE_BIN at an existing pinned-source build.
EOF
  exit 2
fi
if [[ ! -x "$sol_bin" ]]; then
  cargo build --offline --manifest-path "$root_dir/crates/sol/Cargo.toml" >&2 || exit $?
fi

mkdir -p "$results_dir"
entries_file="$results_dir/manifest.tsv"

awk '
function trim(s) { sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); return s }
function string_value(value) {
  value = trim(value)
  return substr(value, 2, length(value) - 2)
}
BEGIN { in_case = 0 }
/^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
/^[[:space:]]*\[\[case\]\][[:space:]]*$/ {
  if (in_case && path != "") print path "\034" status "\034" fixture
  in_case = 1; path = status = fixture = ""
  next
}
{
  pos = index($0, "=")
  if (!in_case || pos == 0) next
  key = trim(substr($0, 1, pos - 1)); value = trim(substr($0, pos + 1))
  if (key == "path") path = string_value(value)
  else if (key == "status") status = string_value(value)
  else if (key == "fixture") fixture = string_value(value)
}
END { if (in_case && path != "") print path "\034" status "\034" fixture }
' "$manifest" >"$entries_file"

matched=0
diverged=0
skipped=0
while IFS=$'\034' read -r path status fixture; do
  case "$status" in
    host-required) skipped=$((skipped + 1)); continue ;;
    pending) [[ $run_pending == 1 ]] || { skipped=$((skipped + 1)); continue; } ;;
  esac
  source="$suite_dir/$path"
  target=$source
  if [[ -n "$fixture" ]]; then
    target="$root_dir/$fixture"
  fi
  if [[ ! -f "$target" ]]; then
    printf 'SKIP  %-18s fixture not found: %s\n' "$path" "$target"
    skipped=$((skipped + 1))
    continue
  fi
  sol_out="$results_dir/$path.sol.stdout"
  ref_out="$results_dir/$path.reference.stdout"
  "$sol_bin" run "$target" >"$sol_out" 2>&1
  ( cd "$suite_dir" && "$reference_bin" "$path" >"$ref_out" 2>&1 )
  if diff -u "$ref_out" "$sol_out" >"$results_dir/$path.diff"; then
    printf 'MATCH %-18s\n' "$path"
    matched=$((matched + 1))
  else
    printf 'DIVG  %-18s see %s\n' "$path" "$results_dir/$path.diff"
    diverged=$((diverged + 1))
  fi
done <"$entries_file"

printf '\nLua 5.5 differential: %d match, %d diverge, %d skipped\n' "$matched" "$diverged" "$skipped"
if [[ -n "$keep_results" ]]; then
  printf 'Logs: %s\n' "$results_dir"
else
  rm -rf "$results_dir"
fi
