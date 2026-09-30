# U6 — Lua 5.5 compatibility completion

**Status:** in progress

**Purpose:** complete semantics before final performance claims.

## Current compatibility ledger

`tests/lua55/manifest.toml` is the release ledger. Its current classifications
are **12 pass, 5 pending, 10 host-required, and 7 divergences**. The older
summary in the historical roadmap is stale; do not use it for a release claim.
Rows become `pass` only after an unchanged pinned-oracle comparison.

## Deliverables

- [~] Complete portable grammar, coercion, `_ENV`, goto/scope, `<close>`,
      metamethod, iteration, coroutine, and error semantics.
  - Remaining corpus blocker: callback-native provenance in `errors.lua`,
    currently `table.sort`'s comparator-name assertion.
- [x] Complete portable debug-library behavior exercised by `db.lua`.
  - The unchanged corpus now matches the pinned Lua 5.5.1 oracle end to end,
    including hook transfer metadata, stripped-code line hooks, nested dump
    metadata, and finalizer/traceback provenance. `strings.lua` is also
    oracle-backed; named vararg tables remain pending (`vararg.lua`).
- [~] Complete GC-observable collection and finalization semantics.
  - `gc.lua` retains table backing capacity in `collectgarbage("count")`.
  - `gengc.lua` needs a true young-generation minor collection and aging/
    remembered-set behavior; incremental major slices are not equivalent.
- [~] Supply the native filesystem/package/`io`/`os`/locale profile.
  - Filesystem-backed loading and portions of `io`/`os` exist, but the full
    native host profile remains unqualified by the host-required corpus rows.
- [ ] Implement required Lua 5.5 binary chunk load/dump compatibility.
- [ ] Decide and implement the embedding/C API boundary, including native
      module tests.
- [~] Promote rows only through unchanged reference comparisons.
  - Twelve rows are oracle-backed; seven divergences need either a compatible
    invocation/output path or an explicit retained profile decision.

`constructs.lua` is also pending because its large live-heap stress case is
superlinear. That is tracked as U7 performance/GC pacing work, not a semantic
promotion shortcut.

## Implementation plan

U6 is a compatibility-closure milestone, not a collection of fixture-specific
patches. For every workstream, first express the behavior in a focused
regression, then promote a corpus row only after the pinned Lua 5.5 oracle
agrees. Do not use the system Lua executable as a promotion oracle.

### 1. Stabilize debug execution and error provenance

Root safety came first because the former `db.lua` failure was a runtime-safety
fault that obscured later debug and diagnostic comparisons. That repair is now
complete; active-lines and error-provenance comparisons can proceed separately.

- [x] Reduce the later `db.lua` stale-`TableRef` panic to the smallest
  allocation/collection stress program that still exercises debugger state.
- [x] Audit roots held by debug hooks, active frames, temporary call values,
  and table-access helpers. Add a forced-collection regression at each relevant
  boundary so a raw table reference cannot outlive its owner.
- [x] Repair the ownership/rooting fault in `sol-core`/`sol` without a permanent
  no-collection escape hatch. Preserve precise roots and document every new
  root boundary.
- [x] Re-run the line-hook stress after the repair. Keep the established
  compiler convention: a multi-line expression's final instruction reports the
  expression's final source line.
- [x] Close the `errors.lua` incomplete-function ordering gap: parse-time
  `MAXVARS` enforcement now wins over the later unterminated-block error and
  preserves the function declaration line through `load()` chunk formatting.
- [ ] Compare remaining `errors.lua` runtime/name diagnostics independently.
  Native C-stack wording remains profile-specific.

Evidence: the focused forced-GC hook test, a loaded-chunk definition-line
regression, and `db.lua` progressing through its trace loop, hook-triggered
collection, active-lines loop, and the initial local-variable API checks
without a panic. `Proto::locals` now records each binding's register plus
inclusive/exclusive PC lifetime; runtime lookup follows that metadata rather
than exposing recycled temporaries, and writes preserve captured-cell aliases.
The debug API now exposes C-frame and suspended-call temporaries, exact hook
callback identity, and read-only snapshots of the interrupted Lua frame for
line/count/return-hook inspection. The remaining work is transfer metadata,
suspended-coroutine thread selectors, and write-through access to an
interrupted frame. The parser/`MAXVARS` case is
covered by direct and `load()` regressions; remaining error cases still need
stored oracle diffs. Do not change source text merely to manufacture a desired
diagnostic.

### 2. Finish Lua syntax and observable language edge cases

These small surface areas have broad parser and compiler consequences, so
isolate them before changing a corpus classification.

- [ ] Treat `global` as a soft/contextual identifier where Lua permits it; do
  not reserve it globally for Sol-specific syntax. Cover declarations, table
  fields, labels, and expressions.
- [ ] Centralize chunk-name formatting used by parse errors, runtime errors,
  and debug information so `goto.lua` and `errors.lua` cannot drift.
- [ ] Add focused tests for every pending parser diagnostic before changing
  parser recovery, scope unwinding, or bytecode emission. Assert failure kind
  and source location as well as rendered text where stable.
- [ ] Promote `goto.lua` only when execution semantics and applicable
  diagnostics both match the oracle.

Evidence: parser/compiler unit tests plus an unchanged `goto.lua` differential
run. Sol-only typed syntax must remain unavailable to plain `.lua` source
unless the language contract explicitly permits it.

### 3. Match string and vararg identity without allocation regressions

`strings.lua` and `vararg.lua` need representation changes, not special-case
library behavior.

- [ ] Split short-string interning from long-string allocation at Lua's
  `LUAI_MAXSHORTLEN` boundary (40 bytes). This is a byte-length threshold, not
  a Unicode-character threshold.
- [ ] Verify equality, hash/table-key lookup, concatenation, and GC
  reachability around the split. Long equal strings may compare equal but must
  not gain short-string identity through interning.
- [ ] Replace eagerly allocated named-vararg tables with a lazily materialized
  virtual vararg view. Its `n` field, numeric lookup, missing-index behavior,
  mutation/materialization boundary, and lifetime must match Lua observations.
- [ ] Account for the view's backing arguments in frame roots and close/unwind
  paths before collection stress is enabled.

Evidence: dedicated 39/40/41-byte boundary tests and empty/non-empty/nested
vararg tests, followed by exact `strings.lua` and `vararg.lua` output matches.
Heap or allocation assertions should live in the focused tests that justify the
representation change.

### 4. Complete observable collector behavior in dependency order

Correct root ownership comes before collector tuning. Resolve the table
reference panic before interpreting a result as a generational-GC mismatch.

1. [ ] Define the `collectgarbage("count")` accounting contract: distinguish
   logical table payload from spare vector capacity, or make the capacity policy
   match the expected accounting. Test growth, deletion, resize, and collection
   boundaries.
2. [ ] Implement a genuine young-generation collection path rather than
   simulating minor collection with a full trace. Specify young roots,
   promotion/aging, and when a major cycle is required.
3. [ ] Add and exercise write barriers/remembered-set maintenance for every
   old-to-young store, including table keys and values, closures, upvalues,
   coroutine stacks, and runtime-owned roots.
4. [ ] Validate weak tables, ephemerons, finalization, and close/unwind paths
   under minor and major collections; shared machinery still needs independent
   regressions.
5. [ ] Tune `step` pacing and debt only after those semantics hold, so
   `gengc.lua` observes the same phases and weak-value clearing as the oracle.

Evidence: deterministic `gc.lua` and `gengc.lua` probes that assert
reachability and phase transitions, then the corpus rows. Elapsed time or a
single heap-size sample is not sufficient collector evidence.

### 5. Make the host/native boundary an explicit release decision

The ten `host-required` rows cannot become portable U6 passes by accident. The
project must record whether its supported native profile is source-only,
Lua-C-API-compatible, or a separately documented embedding profile.

- [ ] Write the decision record and capability matrix for filesystem, process,
  locale, package searchers, dynamic/native modules, C callbacks, and binary
  chunks. Name what is portable, host-gated, and denyable by an embedding.
- [ ] Implement selected native services behind explicit host traits; do not
  let `io`, `os`, package loading, or native modules acquire ambient access in
  the portable runtime.
- [ ] Treat binary chunk load/dump as a versioned-format task. Define accepted
  versions, invalid-header behavior, portable versus native support, and
  fixture-based tests rather than relying on the host's current Lua binary.
- [ ] Add embedding/C-API/module tests only for the approved profile and retain
  `host-required` for unsupported capabilities.

Evidence: the decision record, capability-denial tests, and an oracle run in
the intended host configuration. This can run alongside portable work but does
not relax portable pass criteria.

### Promotion and review loop

For each proposed status change:

1. Add or tighten the smallest crate-level regression.
2. Run the named corpus row against the pinned Lua 5.5 reference and capture
   stdout, stderr, exit behavior, and any allowed normalization.
3. Run `scripts/test-lua55-manifest.sh`; update the manifest rationale and this
   ledger in the same change.
4. Run the focused Sol target, then the relevant crate suite when practical.
5. Retain a divergence only with a written product-profile rationale; otherwise
   treat it as an implementation bug.

Recommended sequence: debug/root safety and error provenance; parser edge
cases; string/vararg representations; collector observability; then the
explicit host/native decision and adapters. This avoids masking memory-safety
defects with GC work and prevents native capabilities leaking into the portable
language contract.

## Exit gate

The declared full runtime profile passes the compatibility gates in the shared
[plan](../unified-sol-runtime-plan.md#6-compatibility-and-correctness-strategy).
If embedding remains incomplete, public wording stays source-compatible rather
than fully runtime-compatible.

See [Lua compatibility](../lua-compatibility.md), the
[historical U6 ledger](../unified-sol-runtime-plan.md#u6--lua-55-compatibility-completion),
and the corpus manifest for evidence and exact repros.
