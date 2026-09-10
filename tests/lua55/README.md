# Lua 5.5 corpus inventory

[manifest.toml](manifest.toml) maps every top-level file in the pinned Lua
5.5.1 test checkout to one of the compatibility outcomes defined in
[the M13 plan](../../docs/sol-roadmap/m13-lua-compat.md). It is deliberately
separate from `crates/sol/tests/fixtures/lua55`: those are small, focused
fixtures that can move through Sol's bytecode, JIT, OSR, and AOT tests as a
feature becomes supported.

[upstream-files.txt](upstream-files.txt) is the checked-in top-level inventory.
The manifest regression script validates it against the real checkout when one
is present, while using it to enforce manifest coverage in CI without the
ignored third-party archive.

`reference/` holds raw deterministic stdout snapshots. The reference runner
compares a matching case automatically, retaining a unified diff on mismatch;
it does not normalize Lua semantics or output ordering.

Run these commands from the repository root:

```sh
scripts/test-lua55-suite.sh lua-5.5.1-tests
scripts/test-lua55-reference.sh lua-5.5.1-tests
LUA55_SOURCE_ARCHIVE=/path/to/lua-5.5.1.tar.gz scripts/test-lua55-reference-container.sh
```

The Sol runner validates the manifest before running any case. It reports
`PASS`, `PEND`, `SKIP`, and `FAIL` separately. `PEND` denotes an implementation
target, while `SKIP` identifies a declared host capability rather than an
unexplained test loss. Set `SOL_LUA55_RUN_HOST_REQUIRED=1` for raw diagnostic
runs of those host cases, and set `SOL_LUA55_REQUIRE_PASS=1` for a strict
all-cases gate.

The reference runner requires a Lua 5.5.1 executable built from the pinned
source. It stores stdout, stderr, status, elapsed seconds, command, locale,
and platform metadata in `LUA55_REFERENCE_RESULTS_DIR` when that variable is
set. `LUA55_REFERENCE_CASES=attrib.lua,bitwise.lua` runs a short reference
smoke subset. See its `--help` output for the source, checksum, and test-module
build options.

`test-lua55-reference-container.sh` is the reproducible full-reference
profile. It builds Lua and the test C modules in Debian with dynamic loading,
readline, `/dev/full`, and the `pt_BR.ISO-8859-1` locale. It uses the pinned
source checksum in the reference runner and retains results in a temporary
directory unless `LUA55_REFERENCE_RESULTS_DIR` is set.
