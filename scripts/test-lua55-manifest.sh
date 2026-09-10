#!/usr/bin/env bash
# Regression checks for the deliberately narrow Lua 5.5 manifest reader.
# It uses a temporary miniature corpus, so this test can run in CI without the
# ignored upstream Lua checkout. When that checkout is available, validate its
# full 34-file inventory too.

set -eu

root_dir=$(cd "$(dirname "$0")/.." && pwd)
runner="$root_dir/scripts/test-lua55-suite.sh"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/sol-lua55-manifest.XXXXXX")
suite_dir="$work_dir/suite"
manifest="$work_dir/manifest.toml"
bad_manifest="$work_dir/bad-manifest.toml"
bad_status_manifest="$work_dir/bad-status.toml"
upstream_list="$root_dir/tests/lua55/upstream-files.txt"
trap 'rm -rf "$work_dir"' EXIT

mkdir -p "$suite_dir"
: >"$suite_dir/alpha.lua"
: >"$suite_dir/beta.lua"
cat >"$manifest" <<'EOF'
version = 1
[[case]]
path = "alpha.lua"
category = "test"
status = "pending"
requires = []
note = "accepted test entry"
[[case]]
path = "beta.lua"
category = "host"
status = "host-required"
requires = ["filesystem"]
note = "accepted host entry"
EOF

SOL_LUA55_MANIFEST="$manifest" SOL_LUA55_VALIDATE_ONLY=1 "$runner" "$suite_dir"

cat >"$bad_manifest" <<'EOF'
version = 1
[[case]]
path = "alpha.lua"
category = "test"
status = "pending"
requires = []
note = "must fail coverage validation"
EOF
if SOL_LUA55_MANIFEST="$bad_manifest" SOL_LUA55_VALIDATE_ONLY=1 "$runner" "$suite_dir" >/dev/null 2>&1; then
  echo "manifest regression: missing corpus coverage was accepted" >&2
  exit 1
fi

cat >"$bad_status_manifest" <<'EOF'
version = 1
[[case]]
path = "alpha.lua"
category = "test"
status = "unknown"
requires = []
note = "must fail schema validation"
[[case]]
path = "beta.lua"
category = "test"
status = "pending"
requires = []
note = "keeps coverage complete"
EOF
if SOL_LUA55_MANIFEST="$bad_status_manifest" SOL_LUA55_VALIDATE_ONLY=1 "$runner" "$suite_dir" >/dev/null 2>&1; then
  echo "manifest regression: unknown status was accepted" >&2
  exit 1
fi

upstream_fixture_suite="$work_dir/upstream-suite"
mkdir -p "$upstream_fixture_suite"
while IFS= read -r path; do
  [[ -n "$path" && $path != \#* ]] || continue
  : >"$upstream_fixture_suite/$path"
done <"$upstream_list"
SOL_LUA55_VALIDATE_ONLY=1 "$runner" "$upstream_fixture_suite"

upstream_suite=${1:-"$root_dir/lua-5.5.1-tests"}
if [[ -d "$upstream_suite" ]]; then
  find "$upstream_suite" -maxdepth 1 -type f -name '*.lua' -print | sed 's#.*/##' | sort >"$work_dir/actual-upstream-files.txt"
  sed '/^#/d; /^$/d' "$upstream_list" | sort >"$work_dir/expected-upstream-files.txt"
  if ! cmp -s "$work_dir/expected-upstream-files.txt" "$work_dir/actual-upstream-files.txt"; then
    echo "manifest regression: upstream file inventory changed" >&2
    diff -u "$work_dir/expected-upstream-files.txt" "$work_dir/actual-upstream-files.txt" >&2
  fi
  SOL_LUA55_VALIDATE_ONLY=1 "$runner" "$upstream_suite"
fi

printf 'Lua 5.5 manifest regression checks passed\n'
