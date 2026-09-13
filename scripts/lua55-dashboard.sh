#!/usr/bin/env bash
# Release dashboard (L8 checklist: "release dashboard with corpus counts by
# manifest status, capability profile, benchmark environment, and typed
# regression status"). Aggregates data that already exists elsewhere in this
# repo (tests/lua55/manifest.toml, the local toolchain/interpreter versions,
# and the L8 checklist's own tracked gaps) into one markdown report, instead
# of requiring each of those to be checked by hand before a release claim.
#
# This is a read-only report generator: it does not invalidate a manifest
# entry or move any case from `pending` to `pass` on its own - that policy
# ("only move a case from `pending` to `pass` with an oracle-backed test",
# docs/features/lua-compatibility.md L8) still requires a human decision
# backed by scripts/test-lua55-suite.sh / scripts/test-lua55-differential.sh.
set -u

root_dir=$(cd "$(dirname "$0")/.." && pwd)
manifest=${SOL_LUA55_MANIFEST:-"$root_dir/tests/lua55/manifest.toml"}
output=""

usage() {
  cat <<'EOF'
Usage: scripts/lua55-dashboard.sh [--output FILE]

Environment:
  SOL_LUA55_MANIFEST   TOML manifest to summarize (default: tests/lua55/manifest.toml)
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      output="${2:?--output requires a file path}"
      shift 2
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      echo "usage: $0 [--output FILE]" >&2
      exit 2
      ;;
  esac
done

if [[ ! -f "$manifest" ]]; then
  echo "Lua 5.5 manifest not found: $manifest" >&2
  exit 2
fi

entries_file="$(mktemp)"
trap 'rm -f "$entries_file"' EXIT

# Reuses the same narrow scalar/string-array TOML subset the manifest is
# written in (see scripts/test-lua55-suite.sh); this reader does not
# re-validate manifest well-formedness - run scripts/test-lua55-manifest.sh
# for that - it assumes a manifest that already passes that check.
awk '
function trim(s) { sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); return s }
function string_value(value) {
  value = trim(value)
  return substr(value, 2, length(value) - 2)
}
function array_value(value,    n, i, parts, piece, out) {
  value = trim(value)
  value = substr(value, 2, length(value) - 2)
  n = split(value, parts, ",")
  out = ""
  for (i = 1; i <= n; i++) {
    piece = string_value(trim(parts[i]))
    if (piece == "") continue
    out = (out == "" ? piece : out "," piece)
  }
  return out
}
BEGIN { in_case = 0 }
/^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
/^[[:space:]]*\[\[case\]\][[:space:]]*$/ {
  if (in_case && path != "") print path "\034" status "\034" category "\034" requires
  in_case = 1; path = status = category = requires = ""
  next
}
{
  pos = index($0, "=")
  if (!in_case || pos == 0) next
  key = trim(substr($0, 1, pos - 1)); value = trim(substr($0, pos + 1))
  if (key == "path") path = string_value(value)
  else if (key == "status") status = string_value(value)
  else if (key == "category") category = string_value(value)
  else if (key == "requires") requires = array_value(value)
}
END { if (in_case && path != "") print path "\034" status "\034" category "\034" requires }
' "$manifest" >"$entries_file"

report="$(mktemp)"

{
  echo "# Lua 5.5 corpus release dashboard"
  echo
  echo "Generated $(date -u +%Y-%m-%dT%H:%M:%SZ) from \`$(basename "$manifest")\`."
  if git -C "$root_dir" rev-parse --short HEAD >/dev/null 2>&1; then
    echo "Commit: \`$(git -C "$root_dir" rev-parse --short HEAD)\`"
  fi
  echo
  echo "## Corpus counts by manifest status"
  echo
  echo '| Status | Count |'
  echo '|:--|--:|'
  awk -F '\034' '{ count[$2]++; total++ } END {
    for (s in count) printf "%s\034%d\n", s, count[s]
  }' "$entries_file" | sort -t $'\034' -k2,2 -rn | while IFS=$'\034' read -r status count; do
    printf '| %s | %s |\n' "$status" "$count"
  done
  total="$(wc -l <"$entries_file" | tr -d ' ')"
  echo "| **total** | **$total** |"
  echo
  echo "Only \`pass\`/\`adapted\` counts as an oracle-backed, file-level pass;"
  echo "moving a case out of \`pending\`/\`host-required\` requires a matching"
  echo "reference-Lua-verified test, not just a manual read of expected output"
  echo "(see the L8 checklist's \"only move ... with an oracle-backed test\" rule)."
  echo
  echo "## Capability profile (why non-passing cases are blocked)"
  echo
  echo '| Capability (`requires`) | Blocked cases |'
  echo '|:--|--:|'
  awk -F '\034' '
    $2 != "pass" && $2 != "adapted" {
      n = split($4, caps, ",")
      for (i = 1; i <= n; i++) if (caps[i] != "") count[caps[i]]++
    }
    END { for (c in count) printf "%s\034%d\n", c, count[c] }
  ' "$entries_file" | sort -t $'\034' -k2,2 -rn | while IFS=$'\034' read -r cap count; do
    printf '| `%s` | %s |\n' "$cap" "$count"
  done
  echo
  echo "A case can require more than one capability; counts are not mutually"
  echo "exclusive and will not sum to the non-passing total above."
  echo
  echo "## Benchmark environment"
  echo
  echo '| Component | Version |'
  echo '|:--|:--|'
  printf '| rustc | %s |\n' "$(rustc --version 2>/dev/null || echo 'not found')"
  printf '| cargo | %s |\n' "$(cargo --version 2>/dev/null || echo 'not found')"
  printf '| OS | %s |\n' "$(uname -sr 2>/dev/null || echo 'unknown')"
  printf '| lua (reference) | %s |\n' "$(lua -v 2>&1 | head -n1 || echo 'not found')"
  if command -v luajit >/dev/null 2>&1; then
    printf '| luajit | %s |\n' "$(luajit -v 2>&1 | head -n1)"
  else
    echo '| luajit | not installed |'
  fi
  echo
  echo "This records the same interpreter/toolchain identity"
  echo "\`scripts/benchmark.sh\` measures against, so a published number in"
  echo "\`benchmarks/RESULTS.md\` can be traced back to the machine/compiler"
  echo "revision that produced it (L8 exit gate: \"published performance"
  echo "claims state the workload, machine, compiler revision, warm-up"
  echo "policy, and comparison\")."
  echo
  echo "## Typed-path regression status"
  echo
  baseline_file="$root_dir/benchmarks/typed-baseline.json"
  if [[ -f "$baseline_file" ]]; then
    echo "A baseline exists (\`benchmarks/typed-baseline.json\`, last written"
    echo "$(date -u -r "$baseline_file" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo 'unknown time')),"
    echo "but this dashboard does not re-run the (slow) wall-clock comparison"
    echo "itself - run \`scripts/typed-regression-check.sh\` for a live"
    echo "pass/fail against that baseline."
  else
    echo "No baseline yet - run \`scripts/typed-regression-check.sh --record\`"
    echo "to create one, then \`scripts/typed-regression-check.sh\` (no flag)"
    echo "for a live pass/fail on subsequent runs."
  fi
  echo
  echo "This dashboard does not fabricate a pass/fail here on its own; it"
  echo "only reports whether the automated check"
  echo "(\`scripts/typed-regression-check.sh\`) has a baseline to compare"
  echo "against. That script still only automates the wall-clock half of the"
  echo "L8 gate - \"inspect typed IR/assembly to confirm no \`LuaValue\`"
  echo "boxing or dynamic dispatch was introduced\" remains a manual step."
} >"$report"

if [[ -n "$output" ]]; then
  cp "$report" "$output"
  echo "Dashboard written to $output"
else
  cat "$report"
fi
rm -f "$report"
