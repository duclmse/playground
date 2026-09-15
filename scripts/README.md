# Project scripts

All Bash entry points keep their existing names for CI and documentation
compatibility. Shared behavior belongs in `lib.sh`; runners should not copy
Cargo target paths or Lua-manifest readers.

## Shared contracts

- `cargo_target_root` resolves workspace build output, including an explicit
  relative or absolute `CARGO_TARGET_DIR`.
- `ensure_sol_bin debug|release` honors `SOL_BIN`, otherwise builds and selects
  the root-workspace binary.
- `parse_lua55_manifest` is the single strict reader for the narrow TOML subset
  used by `tests/lua55/manifest.toml`.
- `validate_lua55_corpus_coverage` enforces the one-entry-per-top-level-file
  inventory contract.
- `sha256_file` supports both GNU `sha256sum` and macOS `shasum`.
- `strip_sol_cli_return_line` normalizes the CLI's documented top-level return
  echo when comparing output with the PUC Lua CLI.

`lib.sh` deliberately does not change shell options. Every executable script
chooses `errexit` itself because differential/reference runners must capture
expected nonzero child statuses.

## Entry-point groups

- `test.sh` is the ordinary cross-component correctness gate.
- `test-lua55-manifest.sh`, `test-lua55-suite.sh`,
  `test-lua55-differential.sh`, and `lua55-dashboard.sh` share the checked Lua
  manifest contract but retain separate validation, execution, comparison, and
  reporting entry points.
- `test-lua55-reference.sh` runs a caller-provided pinned reference build;
  `test-lua55-reference-container.sh` supplies the reproducible Linux host
  profile. Neither silently substitutes a system Lua oracle.
- `test-sol-conformance.sh` checks the focused supported-Lua fixtures, while
  `test-sol-conformance-suite.sh` checks typed capability counterparts. Their
  similarly named outputs are intentionally kept semantically distinct.
- `test-sol-benchmarks.sh` validates every Lua benchmark against Sol's dynamic
  runtime and additionally checks same-named typed workloads before
  `benchmark.sh` performs timing. `typed-regression-check.sh` enforces the
  typed baseline and dynamic-dispatch guard.
- `build-wasm.sh`, `build.sh`, and `dev.sh` build the browser runtime, complete
  web bundle, and incremental development server respectively.
- `build-sol-demo.sh`, `run-sol-demo.sh`, and `run-sol-demo-traps.sh` separate
  AOT construction, tier agreement, and expected-failure fixtures.

Run `test-lua55-manifest.sh` after changing the shared manifest parser and run
`test.sh` after changing target resolution or a gate invoked from CI.
