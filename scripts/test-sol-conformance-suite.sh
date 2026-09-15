#!/usr/bin/env bash
# Run the Sol conformance suite: a hand-written typed `.sol` counterpart for
# every top-level file in the pinned Lua 5.5.1 test checkout
# (lua-5.5.1-tests/*.lua), reinterpreted for Sol's typed surface where one
# exists, or an honest "not applicable" stub where none does.
#
# This is a different script from scripts/test-sol-conformance.sh, which
# predates this suite and checks a small set of already-supported `.lua`
# fixtures for exact `true` output - see that script and
# crates/sol/tests/fixtures/lua55/README.md. This one instead validates
# tests/sol-conformance/manifest.toml against
# crates/sol/tests/fixtures/sol-conformance/*.sol; see
# tests/sol-conformance/README.md.
#
# Unlike scripts/test-lua55-suite.sh, this does NOT compare against a real
# Lua 5.5.1 oracle - see tests/sol-conformance/manifest.toml's header for why
# that comparison would not be meaningful here. It instead checks each `.sol`
# fixture's `sol run` stdout against the manifest's self-authored `expected`
# value, so a change to Sol's typed semantics that silently breaks one of
# these fixtures is caught the same way any other regression would be.

set -u

root_dir=$(cd "$(dirname "$0")/.." && pwd)
suite_dir=${1:-"$root_dir/lua-5.5.1-tests"}
manifest=${SOL_CONFORMANCE_MANIFEST:-"$root_dir/tests/sol-conformance/manifest.toml"}
fixtures_dir="$root_dir/crates/sol/tests/fixtures/sol-conformance"
sol_bin=${SOL_BIN:-"$root_dir/crates/sol/target/debug/sol"}
validate_only=${SOL_CONFORMANCE_VALIDATE_ONLY:-0}

usage() {
  cat <<'EOF'
Usage: scripts/test-sol-conformance-suite.sh [lua-5.5.1-tests-directory]

Environment:
  SOL_BIN                          Sol executable (default: debug build)
  SOL_CONFORMANCE_MANIFEST         TOML manifest to validate and run
  SOL_CONFORMANCE_VALIDATE_ONLY=1  Validate manifest/fixture coverage and exit
EOF
}

if [[ ${1:-} == --help ]]; then
  usage
  exit 0
fi

if [[ ! -f "$manifest" ]]; then
  echo "Sol conformance manifest not found: $manifest" >&2
  exit 2
fi

entries_file=$(mktemp "${TMPDIR:-/tmp}/sol-conformance.XXXXXX")
trap 'rm -f "$entries_file"' EXIT

# Deliberately narrow TOML subset reader, matching
# scripts/test-lua55-suite.sh's approach: fail loudly on anything
# unexpected instead of silently misreading the manifest.
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
function emit() {
  if (!in_case) return
  if (path == "" || sol == "" || category == "" || status == "" || expected == "" || note == "") {
    die("case starting at line " case_line " must contain path, sol, category, status, expected, and note")
  }
  if (status != "ported" && status != "not-applicable") {
    die("unknown status '\''" status "'\'' for " path)
  }
  if (seen[path]++) die("duplicate path " path)
  print path "\034" sol "\034" status "\034" category "\034" expected "\034" note
}
BEGIN { in_case = 0; bad = 0 }
/^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
/^[[:space:]]*\[\[case\]\][[:space:]]*$/ {
  emit(); in_case = 1; case_line = NR
  path = sol = category = status = expected = note = ""
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
  if (key == "path" || key == "sol" || key == "category" || key == "status" || key == "expected" || key == "note") {
    value = string_value(value, key)
  } else {
    die("unknown case key " key " at line " NR); next
  }
  if (key == "path") path = value
  else if (key == "sol") sol = value
  else if (key == "category") category = value
  else if (key == "status") status = value
  else if (key == "expected") expected = value
  else if (key == "note") note = value
}
END { emit(); exit bad }
' "$manifest" >"$entries_file"; then
  exit 2
fi

manifest_paths='|'
manifest_count=0
while IFS=$'\034' read -r path sol status category expected note; do
  if [[ ! -f "$fixtures_dir/$sol" ]]; then
    echo "manifest: missing fixture for $path: $fixtures_dir/$sol" >&2
    exit 2
  fi
  case "$manifest_paths" in
    *"|$path|"*)
      echo "manifest: duplicate source path: $path" >&2
      exit 2
      ;;
  esac
  manifest_paths="${manifest_paths}${path}|"
  manifest_count=$((manifest_count + 1))
done <"$entries_file"

if [[ -d "$suite_dir" ]]; then
  corpus_count=0
  for case_file in "$suite_dir"/*.lua; do
    [[ -f "$case_file" ]] || continue
    case_name=$(basename "$case_file")
    corpus_count=$((corpus_count + 1))
    case "$manifest_paths" in
      *"|$case_name|"*) ;;
      *)
        echo "manifest: missing corpus entry for $case_name" >&2
        exit 2
        ;;
    esac
  done
  if [[ $manifest_count -ne $corpus_count ]]; then
    echo "manifest: has $manifest_count entries for $corpus_count top-level corpus files" >&2
    exit 2
  fi
else
  echo "note: $suite_dir not present; skipped 1:1 corpus-coverage check, only validated manifest/fixture consistency" >&2
fi

if [[ $validate_only == 1 ]]; then
  printf 'Sol conformance manifest: %d entries, %d fixtures present\n' "$manifest_count" "$manifest_count"
  exit 0
fi

if [[ ! -x "$sol_bin" ]]; then
  cargo build --offline --manifest-path "$root_dir/crates/sol/Cargo.toml" >&2 || exit $?
fi

ported=0
not_applicable=0
failed=0
while IFS=$'\034' read -r path sol status category expected note; do
  target="$fixtures_dir/$sol"
  stderr_file=$(mktemp "${TMPDIR:-/tmp}/sol-conformance-stderr.XXXXXX")
  actual=$("$sol_bin" run "$target" 2>"$stderr_file")
  rc=$?
  if [[ $rc -ne 0 ]]; then
    printf 'FAIL  %-14s %-24s exit=%d %s\n' "$sol" "$category" "$rc" "$(head -n1 "$stderr_file")"
    failed=$((failed + 1))
  elif [[ $actual != "$expected" ]]; then
    printf 'FAIL  %-14s %-24s expected %s, got %s\n' "$sol" "$category" "$expected" "$actual"
    failed=$((failed + 1))
  elif [[ $status == ported ]]; then
    printf 'PORT  %-14s %-24s -> %s\n' "$sol" "$category" "$actual"
    ported=$((ported + 1))
  else
    printf 'N/A   %-14s %-24s %s\n' "$sol" "$category" "$note"
    not_applicable=$((not_applicable + 1))
  fi
  rm -f "$stderr_file"
done <"$entries_file"

printf '\nSol conformance suite: %d ported, %d not-applicable, %d failed (of %d total)\n' \
  "$ported" "$not_applicable" "$failed" "$manifest_count"

if ((failed != 0)); then
  exit 1
fi
