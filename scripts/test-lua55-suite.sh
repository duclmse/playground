#!/usr/bin/env bash
# Run the Lua 5.5.1 top-level corpus through Sol with a checked manifest.
# The manifest prevents C API/host tests from being presented as unexplained
# language failures while still allowing an explicit raw diagnostic run.

set -u

root_dir=$(cd "$(dirname "$0")/.." && pwd)
suite_dir=${1:-"$root_dir/lua-5.5.1-tests"}
manifest=${SOL_LUA55_MANIFEST:-"$root_dir/tests/lua55/manifest.toml"}
sol_bin=${SOL_BIN:-"$root_dir/crates/sol/target/debug/sol"}
results_dir=${SOL_LUA55_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/sol-lua55.XXXXXX")"}
keep_results=${SOL_LUA55_RESULTS_DIR:+1}
run_pending=${SOL_LUA55_RUN_PENDING:-1}
run_host_required=${SOL_LUA55_RUN_HOST_REQUIRED:-0}
validate_only=${SOL_LUA55_VALIDATE_ONLY:-0}

usage() {
  cat <<'EOF'
Usage: scripts/test-lua55-suite.sh [lua-5.5.1-tests-directory]

Environment:
  SOL_BIN                       Sol executable (default: debug build)
  SOL_LUA55_MANIFEST            TOML manifest to validate and run
  SOL_LUA55_RESULTS_DIR         Preserve logs in this directory
  SOL_LUA55_RUN_PENDING=0       Classify pending cases without invoking Sol
  SOL_LUA55_RUN_HOST_REQUIRED=1 Also run host-required cases for diagnostics
  SOL_LUA55_VALIDATE_ONLY=1     Validate manifest coverage and exit
  SOL_LUA55_REQUIRE_PASS=1      Fail unless every manifest case passes
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

mkdir -p "$results_dir"
entries_file="$results_dir/manifest.tsv"

# The manifest deliberately uses only scalar strings and string arrays. Keep
# this reader narrow so a malformed inventory fails loudly instead of silently
# changing a corpus result.
if ! awk '
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
    die("unknown status '\''" status "'\'' for " path)
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
' "$manifest" >"$entries_file"; then
  rm -rf "$results_dir"
  exit 2
fi

manifest_paths='|'
manifest_count=0
while IFS=$'\034' read -r path status category requires fixture note; do
  if [[ $path == /* || $path == */* || $path == *".."* || ! -f "$suite_dir/$path" ]]; then
    echo "manifest: source path is not a top-level corpus file: $path" >&2
    rm -rf "$results_dir"
    exit 2
  fi
  case "$manifest_paths" in
    *"|$path|"*)
      echo "manifest: duplicate source path: $path" >&2
      rm -rf "$results_dir"
      exit 2
      ;;
  esac
  manifest_paths="${manifest_paths}${path}|"
  manifest_count=$((manifest_count + 1))
done <"$entries_file"

corpus_count=0
for case_file in "$suite_dir"/*.lua; do
  [[ -f "$case_file" ]] || continue
  case_name=$(basename "$case_file")
  corpus_count=$((corpus_count + 1))
  case "$manifest_paths" in
    *"|$case_name|"*) ;;
    *)
      echo "manifest: missing corpus entry for $case_name" >&2
      rm -rf "$results_dir"
      exit 2
      ;;
  esac
done

if [[ $manifest_count -ne $corpus_count ]]; then
  echo "manifest: has $manifest_count entries for $corpus_count top-level corpus files" >&2
  rm -rf "$results_dir"
  exit 2
fi

if [[ $validate_only == 1 ]]; then
  printf 'Lua 5.5 manifest: %d entries cover %d top-level files\n' "$manifest_count" "$corpus_count"
  rm -rf "$results_dir"
  exit 0
fi

if [[ ! -x "$sol_bin" ]]; then
  cargo build --offline --manifest-path "$root_dir/crates/sol/Cargo.toml" >&2 || exit $?
fi

passed=0
pending=0
skipped=0
failed=0
diverged=0
run_case() {
  local path=$1
  local status=$2
  local fixture=$3
  local source="$suite_dir/$path"
  local target=$source
  if [[ -n "$fixture" ]]; then
    target="$root_dir/$fixture"
  fi
  if [[ ! -f "$target" ]]; then
    printf 'FAIL  %-18s manifest fixture not found: %s\n' "$path" "$target"
    failed=$((failed + 1))
    return
  fi
  if "$sol_bin" run "$target" >"$results_dir/$path.log" 2>&1; then
    case "$status" in
      pass|adapted)
        printf 'PASS  %-18s %s\n' "$path" "$status"
        passed=$((passed + 1))
        ;;
      pending)
        printf 'PEND  %-18s unexpectedly runs; classify after oracle comparison\n' "$path"
        pending=$((pending + 1))
        ;;
      diverges)
        printf 'DIVG  %-18s documented divergence\n' "$path"
        diverged=$((diverged + 1))
        ;;
    esac
  else
    first_line=$(sed -n '1p' "$results_dir/$path.log")
    case "$status" in
      pending)
        printf 'PEND  %-18s %s\n' "$path" "$first_line"
        pending=$((pending + 1))
        ;;
      diverges)
        printf 'DIVG  %-18s %s\n' "$path" "$first_line"
        diverged=$((diverged + 1))
        ;;
      *)
        printf 'FAIL  %-18s %s\n' "$path" "$first_line"
        failed=$((failed + 1))
        ;;
    esac
  fi
}

while IFS=$'\034' read -r path status category requires fixture note; do
  case "$status" in
    host-required)
      if [[ $run_host_required == 1 ]]; then
        run_case "$path" pending "$fixture"
      else
        printf 'SKIP  %-18s requires: %s\n' "$path" "${requires:-declared host capability}"
        skipped=$((skipped + 1))
      fi
      ;;
    pending)
      if [[ $run_pending == 1 ]]; then
        run_case "$path" "$status" "$fixture"
      else
        printf 'PEND  %-18s %s\n' "$path" "$note"
        pending=$((pending + 1))
      fi
      ;;
    pass|adapted|diverges)
      run_case "$path" "$status" "$fixture"
      ;;
  esac
done <"$entries_file"

printf '\nLua 5.5 corpus: %d passed, %d pending, %d host-required, %d diverges, %d failed\n' \
  "$passed" "$pending" "$skipped" "$diverged" "$failed"
if [[ -n "$keep_results" ]]; then
  printf 'Logs: %s\n' "$results_dir"
else
  rm -rf "$results_dir"
fi

if [[ ${SOL_LUA55_REQUIRE_PASS:-0} == 1 ]] && ((passed != manifest_count)); then
  exit 1
fi
