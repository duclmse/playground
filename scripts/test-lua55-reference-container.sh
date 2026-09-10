#!/usr/bin/env bash
# Build and run the pinned Lua 5.5.1 reference in a Linux container. Keeping
# the source archive outside the repository avoids checking a third-party
# release tarball into Sol while retaining a checksum-verified run.

set -eu

root_dir=$(cd "$(dirname "$0")/.." && pwd)
suite_archive=${LUA55_TESTS_ARCHIVE:-"$root_dir/lua-5.5.1-tests.tar.gz"}
source_archive=${LUA55_SOURCE_ARCHIVE:-}
results_dir=${LUA55_REFERENCE_RESULTS_DIR:-"$(mktemp -d "${TMPDIR:-/tmp}/lua55-reference-linux.XXXXXX")"}
image=${LUA55_REFERENCE_IMAGE:-debian:bookworm-slim}
readline_library=${LUA55_READLINELIB:-xuxu}

usage() {
  cat <<'EOF'
Usage: LUA55_SOURCE_ARCHIVE=/path/to/lua-5.5.1.tar.gz \
  scripts/test-lua55-reference-container.sh

Environment:
  LUA55_SOURCE_ARCHIVE          Official Lua 5.5.1 source archive (required)
  LUA55_TESTS_ARCHIVE           Lua 5.5.1 test archive (default: repo copy)
  LUA55_REFERENCE_RESULTS_DIR   Host directory for the captured reference logs
  LUA55_REFERENCE_CASES         Comma-separated smoke subset, for example api.lua
  LUA55_REFERENCE_CASE_TIMEOUT_SECONDS  Per-case test timeout (default: 30)
  LUA55_REFERENCE_IMAGE         Debian-compatible image (default: bookworm-slim)
  LUA55_READLINELIB             Readline fallback name (default: xuxu)

The container installs build tools, readline, and the pt_BR ISO-8859-1 locale;
it builds Lua and the suite's C modules using the same source headers. Results
are written as stdout/stderr/meta files and environment.txt in the host result
directory.
EOF
}

if [[ ${1:-} == --help ]]; then
  usage
  exit 0
fi
if [[ -z "$source_archive" || ! -f "$source_archive" ]]; then
  echo "LUA55_SOURCE_ARCHIVE must name the official Lua 5.5.1 source tarball" >&2
  exit 2
fi
if [[ ! -f "$suite_archive" ]]; then
  echo "Lua 5.5.1 test archive not found: $suite_archive" >&2
  exit 2
fi

mkdir -p "$results_dir"
docker run --rm \
  -v "$source_archive:/input/lua-5.5.1.tar.gz:ro" \
  -v "$suite_archive:/input/lua-5.5.1-tests.tar.gz:ro" \
  -v "$root_dir/scripts/test-lua55-reference.sh:/opt/test-lua55-reference.sh:ro" \
  -v "$root_dir/tests/lua55/reference:/opt/reference-snapshots:ro" \
  -v "$results_dir:/results" \
  -e LUA55_REFERENCE_CASES="${LUA55_REFERENCE_CASES:-}" \
  -e LUA55_REFERENCE_CASE_TIMEOUT_SECONDS="${LUA55_REFERENCE_CASE_TIMEOUT_SECONDS:-30}" \
  -e LUA_READLINELIB="$readline_library" \
  -e LUA55_REFERENCE_LOCALE=pt_BR.ISO-8859-1 \
  "$image" /bin/sh -ceu '
    expected_sha256=1c4b4068d67061f2a2231ad2b5422e77acea1487ea9890f6320af614f4373dce
    actual_sha256=$(sha256sum /input/lua-5.5.1.tar.gz | awk "{print \$1}")
    test "$actual_sha256" = "$expected_sha256"
    export DEBIAN_FRONTEND=noninteractive
    export LC_ALL=C
    apt-get update
    apt-get install -y --no-install-recommends build-essential ca-certificates libreadline-dev locales
    rm -rf /var/lib/apt/lists/*
    localedef -i pt_BR -f ISO-8859-1 pt_BR.ISO-8859-1
    export LC_ALL="$LUA55_REFERENCE_LOCALE"
    mkdir -p /work
    tar -xzf /input/lua-5.5.1.tar.gz -C /work
    tar -xzf /input/lua-5.5.1-tests.tar.gz -C /work
    make -C /work/lua-5.5.1 linux-readline
    LUA55_SOURCE_DIR=/work/lua-5.5.1 \
    LUA55_SOURCE_ARCHIVE=/input/lua-5.5.1.tar.gz \
    LUA55_REFERENCE_RESULTS_DIR=/results \
    LUA55_REFERENCE_SNAPSHOTS_DIR=/opt/reference-snapshots \
    LUA55_BUILD_LIBS=1 \
    /opt/test-lua55-reference.sh /work/lua-5.5.1-tests
  '

printf 'Lua 5.5 Linux reference results: %s\n' "$results_dir"
