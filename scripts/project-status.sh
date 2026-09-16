#!/usr/bin/env bash
# Report convergence status without conflating inventory, typed capability
# regressions, and oracle-backed Lua compatibility passes.
set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
lua_manifest=${SOL_LUA55_MANIFEST:-"$root_dir/tests/lua55/manifest.toml"}
typed_manifest=${SOL_TYPED_CAPABILITY_MANIFEST:-"$root_dir/tests/sol-conformance/manifest.toml"}
format=text
check=0
require_full=0

usage() {
  printf '%s\n' 'Usage: scripts/project-status.sh [--json] [--check] [--require-full-compat]'
}

while (($#)); do
  case "$1" in
    --json) format=json ;;
    --check) check=1 ;;
    --require-full-compat) require_full=1 ;;
    --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  shift
done

for manifest in "$lua_manifest" "$typed_manifest"; do
  [[ -f "$manifest" ]] || { echo "status: missing manifest: $manifest" >&2; exit 2; }
done

count_status() {
  local manifest=$1 status=$2
  awk -F '"' -v wanted="$status" '$1 ~ /^[[:space:]]*status[[:space:]]*=/ && $2 == wanted { count++ } END { print count + 0 }' "$manifest"
}

case_count() {
  awk '$0 ~ /^[[:space:]]*\[\[case\]\][[:space:]]*$/ { count++ } END { print count + 0 }' "$1"
}

unknown_statuses() {
  local manifest=$1 allowed=$2
  awk -F '"' -v allowed="$allowed" '
    BEGIN { count = split(allowed, values, ","); for (i = 1; i <= count; i++) known[values[i]] = 1 }
    $1 ~ /^[[:space:]]*status[[:space:]]*=/ && !known[$2] { print $2 }
  ' "$manifest"
}

lua_total=$(case_count "$lua_manifest")
lua_pass=$(count_status "$lua_manifest" pass)
lua_adapted=$(count_status "$lua_manifest" adapted)
lua_pending=$(count_status "$lua_manifest" pending)
lua_host=$(count_status "$lua_manifest" host-required)
lua_diverges=$(count_status "$lua_manifest" diverges)
lua_oracle=$((lua_pass + lua_adapted))
lua_accounted=$((lua_pass + lua_adapted + lua_pending + lua_host + lua_diverges))

typed_total=$(case_count "$typed_manifest")
typed_ported=$(count_status "$typed_manifest" ported)
typed_na=$(count_status "$typed_manifest" not-applicable)
typed_accounted=$((typed_ported + typed_na))

if ((lua_oracle == lua_total)); then compatibility_complete=true; else compatibility_complete=false; fi
if grep -q '@lua-playground/runtime' "$root_dir/apps/web/src/lua-worker.ts"; then web_runtime=piccolo; else web_runtime=sol; fi
if [[ -d "$root_dir/editors/vscode-sol" ]]; then vscode_client=true; else vscode_client=false; fi
# Frontend configuration no longer selects grammar productions after U1, but
# that does not make the execution engine unified. The partition bridge remains
# the explicit migration seam until U2 moves production objects onto the
# canonical heap and the final fallback can be removed. U3's shared semantic
# ABI intentionally keeps this differential seam available during migration.
if grep -q 'run_lua_partitioned' "$root_dir/crates/sol/src/main.rs"; then runtime_unified=false; else runtime_unified=true; fi
if [[ -f "$root_dir/crates/sol-lsp/Cargo.toml" ]]; then lsp_present=true; else lsp_present=false; fi
if [[ -f "$root_dir/crates/sol-core/src/heap.rs" ]]; then canonical_heap_foundation=true; else canonical_heap_foundation=false; fi
commit=$(git -C "$root_dir" rev-parse --short HEAD 2>/dev/null || printf unknown)

if ((check)); then
  lua_unknown=$(unknown_statuses "$lua_manifest" 'pass,adapted,pending,host-required,diverges')
  typed_unknown=$(unknown_statuses "$typed_manifest" 'ported,not-applicable')
  [[ -z "$lua_unknown" ]] || { echo "status: unknown Lua manifest status: $lua_unknown" >&2; exit 1; }
  [[ -z "$typed_unknown" ]] || { echo "status: unknown typed capability status: $typed_unknown" >&2; exit 1; }
  ((lua_total == lua_accounted)) || { echo "status: Lua cases are not fully classified" >&2; exit 1; }
  ((typed_total == typed_accounted)) || { echo "status: typed capability cases are not fully classified" >&2; exit 1; }
  upstream_count=$(sed '/^[[:space:]]*#/d; /^[[:space:]]*$/d' "$root_dir/tests/lua55/upstream-files.txt" | wc -l | tr -d ' ')
  ((lua_total == upstream_count)) || { echo "status: Lua manifest has $lua_total cases for $upstream_count inventory files" >&2; exit 1; }
  [[ -f "$root_dir/docs/features/unified-sol-runtime-plan.md" ]] || { echo "status: unified roadmap missing" >&2; exit 1; }
  [[ -f "$root_dir/docs/decisions/README.md" ]] || { echo "status: U0 decision index missing" >&2; exit 1; }
  [[ "$canonical_heap_foundation" == true ]] || { echo "status: U2 canonical heap foundation missing" >&2; exit 1; }
fi

if [[ "$format" == json ]]; then
  printf '{\n'
  printf '  "schema_version": 1,\n'
  printf '  "commit": "%s",\n' "$commit"
  printf '  "lua_compatibility": {"total": %d, "oracle_backed_passes": %d, "pass": %d, "adapted": %d, "pending": %d, "host_required": %d, "diverges": %d, "complete": %s},\n' \
    "$lua_total" "$lua_oracle" "$lua_pass" "$lua_adapted" "$lua_pending" "$lua_host" "$lua_diverges" "$compatibility_complete"
  printf '  "typed_capability_regressions": {"total": %d, "ported": %d, "not_applicable": %d, "lua_compatibility_evidence": false},\n' \
    "$typed_total" "$typed_ported" "$typed_na"
  printf '  "architecture": {"runtime_unified": %s, "canonical_heap_foundation": %s, "web_runtime": "%s", "lsp_present": %s, "vscode_client": %s}\n' \
    "$runtime_unified" "$canonical_heap_foundation" "$web_runtime" "$lsp_present" "$vscode_client"
  printf '}\n'
else
  printf 'Sol unified-product status (%s)\n' "$commit"
  printf 'Lua compatibility (unchanged upstream/oracle): %d/%d pass; %d pending; %d host-required; %d diverges\n' \
    "$lua_oracle" "$lua_total" "$lua_pending" "$lua_host" "$lua_diverges"
  printf 'Typed capability regressions (not Lua compatibility): %d ported; %d not-applicable; %d total\n' \
    "$typed_ported" "$typed_na" "$typed_total"
  printf 'Architecture: runtime_unified=%s canonical_heap_foundation=%s web_runtime=%s lsp=%s vscode_client=%s\n' \
    "$runtime_unified" "$canonical_heap_foundation" "$web_runtime" "$lsp_present" "$vscode_client"
fi

if ((require_full)) && [[ "$compatibility_complete" != true ]]; then
  printf 'status: full compatibility gate not met (%d/%d oracle-backed passes)\n' "$lua_oracle" "$lua_total" >&2
  exit 1
fi
