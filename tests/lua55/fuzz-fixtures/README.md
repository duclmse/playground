# Differential fuzz fixtures

Permanent regression fixtures promoted from `scripts/test-lua55-differential.sh`'s
generated-input fuzz mode (`SOL_LUA55_DIFF_FUZZ_CASES`/`SOL_LUA55_DIFF_FUZZ_SEED`).
When a generated case diverges from reference Lua 5.5.1, its source is copied
here as `case-<seed>.lua` and recorded in that run's `failure-report.md`, so
the failure survives after the run's temporary results directory is gone
(the L8 checklist's "differential fuzz failures become permanent fixtures"
rule - see `docs/features/lua-compatibility.md`).

This directory is empty when no generated case has ever diverged - that is
the expected, honest state, not a placeholder to be filled proactively. A
fixture that lands here should be turned into a minimal, hand-reduced
regression case and added to `crates/sol/tests/lua55.rs` (or, if the
divergence traces back to a specific manifest entry, reflected in
`tests/lua55/manifest.toml`) once its root cause is understood, rather than
left to accumulate as raw generated source.
