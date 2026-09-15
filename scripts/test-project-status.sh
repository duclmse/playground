#!/usr/bin/env bash
set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/sol-project-status.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT

"$root_dir/scripts/project-status.sh" --check
"$root_dir/scripts/project-status.sh" --json >"$work_dir/current.json"

expected_oracle=$(awk -F '"' '$1 ~ /^[[:space:]]*status[[:space:]]*=/ && ($2 == "pass" || $2 == "adapted") { count++ } END { print count + 0 }' "$root_dir/tests/lua55/manifest.toml")
expected_total=$(awk '$0 ~ /^[[:space:]]*\[\[case\]\][[:space:]]*$/ { count++ } END { print count + 0 }' "$root_dir/tests/lua55/manifest.toml")
grep -q "\"oracle_backed_passes\": $expected_oracle" "$work_dir/current.json" || {
  echo "project status regression: current pending inventory was presented as passing" >&2
  exit 1
}
grep -q '"lua_compatibility_evidence": false' "$work_dir/current.json" || {
  echo "project status regression: typed ports were presented as Lua compatibility" >&2
  exit 1
}
grep -q '"runtime_unified": false' "$work_dir/current.json" || {
  echo "project status regression: frontend convergence was presented as runtime convergence" >&2
  exit 1
}

if ((expected_oracle < expected_total)); then
  if "$root_dir/scripts/project-status.sh" --require-full-compat >/dev/null 2>&1; then
    echo "project status regression: incomplete Lua manifest passed the full compatibility gate" >&2
    exit 1
  fi
else
  "$root_dir/scripts/project-status.sh" --require-full-compat >/dev/null
fi

printf 'Unified project status checks passed\n'
