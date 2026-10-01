# U6 — Lua 5.5 compatibility completion

**Status:** in progress

**Purpose:** complete semantics before final performance claims.

## Current compatibility ledger

`tests/lua55/manifest.toml` is the release ledger. Its current classifications
are **16 pass, 1 pending, 10 host-required, and 7 divergences**. The older
summary in the historical roadmap is stale; do not use it for a release claim.
Rows become `pass` only after an unchanged pinned-oracle comparison.

## Deliverables

- [~] Complete portable grammar, coercion, `_ENV`, goto/scope, `<close>`,
      metamethod, iteration, coroutine, and error semantics.
  - Callback-native provenance is fixed: any call issued by native Rust code
    rather than by bytecode (`pcall`/`xpcall`'s protected function,
    `table.sort`'s comparator, `string.gsub`'s replacement, a coroutine's
    body) now uses real Lua's `pushglobalfuncname` qualified-name fallback
    for a "bad argument" error, matching the pinned oracle for all of those
    call shapes while an ordinary bytecode-issued call keeps its bare name.
  - `goto.lua` is promoted to `pass`: `global` is now a genuine
    contextual/soft identifier (not reserved outside `global`-declaration
    statement positions) and parse/runtime error chunk-name formatting is
    centralized on `luaO_chunkid`'s convention, so the unmodified corpus file
    runs end to end and exits 0.
  - `errors.lua` is promoted to `pass`: `MAX_NATIVE_CALL_DEPTH`'s "C stack
    overflow" wording already covered line 397's recursive
    `coroutine.create`/`resume` case. Two further genuine bugs were fixed to
    reach full completion: a generic-`for` iterator-call error used to be
    attributed to the `for` keyword's own line instead of the iterator
    expression list's line when the header spans multiple lines (real Lua's
    `forlist()` captures the iterator list's own line right after `TK_IN`);
    and Sol had no model of real Lua's "double stack fault" (`ldo.c`'s
    `ERRORSTACKSIZE`/`LUA_ERRERR`), where a second, unrelated value-stack
    overflow raised from within an already-running `xpcall` message handler
    must bypass the handler and resolve directly to "error in error
    handling" rather than retry the handler - modeled with
    `LuaRuntime::call_depth_overflowed_once`/`LuaError::double_fault`, reset
    exactly when real Lua's `luaD_pcall` would unconditionally
    `luaD_shrinkstack` on catching a non-OK status. The unmodified corpus
    file now runs end to end and matches the pinned oracle's `OK` output
    exactly.
  - The `errors.lua` fix above had two ripple effects on other previously-`pass`
    rows, both now resolved so every row that was `pass` before this work
    remains genuinely `pass`: `calls.lua`'s "C-stack overflow while handling
    C-stack overflow" test (line 160) now costs more instructions under the
    more precise `call_depth_overflowed_once` accounting, needing a raised
    `budget` (see its manifest note) with behavior unchanged and still
    oracle-exact; and `coroutine.lua` surfaced a genuine pre-existing
    `call_depth` accounting bug in `coroutine.close()`'s `Suspended` branch -
    it re-subtracted a suspended coroutine's parked `depth_charged` from the
    shared `call_depth` counter even though `resume_coroutine`'s own `Yielded`
    arm had already released that same charge back out of the counter when
    the coroutine yielded, so closing a suspended coroutine double-released
    the charge and eventually underflowed `call_depth`, panicking. Fixed by
    having `coroutine.close()` simply clear the parked value instead of
    subtracting it again.
- [x] Complete portable debug-library behavior exercised by `db.lua`.
  - The unchanged corpus now matches the pinned Lua 5.5.1 oracle end to end,
    including hook transfer metadata, stripped-code line hooks, nested dump
    metadata, and finalizer/traceback provenance. `strings.lua` is also
    oracle-backed. `vararg.lua` is promoted to `pass`: named vararg
    parameters now use a lazily materialized view (see deliverable 3 below).
- [x] Complete GC-observable collection and finalization semantics.
  - `gc.lua` is now oracle-backed end to end (`pass`, under a raised
    `budget`/`alloc_budget`): the `collectgarbage("count")` overshoot was a
    `HeapObject`/`String` footprint bug, not table-capacity retention; a
    global `call_depth` budget was also wrongly shared across simultaneously
    idle suspended coroutines; and the collector was missing a
    reentrancy guard for `__gc` finalizers that call `collectgarbage()`.
  - `gengc.lua` is now also oracle-backed end to end (`pass`, default
    budgets): `sol_core::Heap` gained a genuine young-generation-only minor
    collector (`collect_minor_with_conditional_roots`, per-object
    `GcGeneration::Young`/`Old` tracking, a write-barrier-maintained
    `remembered` set, `should_trace_during_minor`'s remembered-or-young trace
    gate), wired into `collectgarbage("step")` under `"generational"` mode.
    Validating against the real pinned corpus surfaced and fixed two further
    regressions in the new minor path: `conditional_roots` frame contents
    could be dropped for an `Old`, unremembered coroutine, and — the
    substantive fix — a minor collection's mark phase never seeded its queue
    from `self.remembered`, so an `Old` object unreachable from any current
    root that round (but still unconditionally spared from a minor sweep)
    never had its remembered edges to `Young` children traced, letting a
    still-referenced child (e.g. the object's own metatable) be reclaimed out
    from under it. See `tests/lua55/manifest.toml`'s `gengc.lua` case note
    for the full fix-by-fix detail.
- [~] Supply the native filesystem/package/`io`/`os`/locale profile.
  - Filesystem-backed loading and portions of `io`/`os` exist, but the full
    native host profile remains unqualified by the host-required corpus rows.
- [x] Implement required Lua 5.5 binary chunk load/dump compatibility.
  - `natives_load.rs`'s `lua55_binary_chunk_header`/`SOL_DUMP_MAGIC`/
    `load_binary_chunk` implement a versioned envelope (a real Lua 5.5.1
    `ldump.c` header followed by a private Sol magic/key/length envelope),
    with defined invalid-header/truncation/size-mismatch behavior and three
    fixture tests (`crate/sol/tests/lua55_dynamic_runtime_string.rs`,
    including a new one rejecting a Lua-5.5-header-shaped but foreign
    payload). True cross-process portability with a real external `luac5.5`
    is out of scope by decision, not pending; see
    `docs/features/host-native-boundary.md`.
- [x] Decide and implement the embedding/C API boundary, including native
      module tests.
  - The decision was already recorded (`docs/decisions/0002-lua-compatibility-
    profile.md`, 2026-09-15): the native profile includes source-compatible
    Lua 5.5 C API headers and ABI-compatible linking. It is already
    implemented: `crate/sol/src/lua_runtime/c_api.rs` is a real `lua.h`/
    `lauxlib.h`/`lualib.h`-compatible C API (stack ops, closures/upvalues,
    userdata, the registry, metatables, `<close>`, coroutines through the C
    API with hooks and yield continuations, `lua_load`/`lua_dump`) built as a
    linkable `cdylib`/`staticlib` (`libsol`), plus real `dlopen`-based native
    module loading (`package.loadlib`/the `cpath` C searcher). End-to-end
    fixture tests (`crate/sol/tests/native/embedding_smoke.c` +
    `sol_fixture.c`, run by `scripts/test-sol-c-api.sh`) compile and execute
    against the real built library and were reconfirmed passing during this
    work. See `docs/features/host-native-boundary.md` for the full capability
    matrix and the precise, narrow reasons the `c-api`/`ltests`-requiring
    corpus rows still stay `host-required` (PUC Lua's internal `ltests` test
    harness, not the public API, per ADR 0002's own "equivalent tests, not a
    literal port" language).
- [~] Promote rows only through unchanged reference comparisons.
  - Fifteen rows are oracle-backed; seven divergences need either a compatible
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
- [x] Compare remaining `errors.lua` runtime/name diagnostics independently.
  `errors.lua` is now `pass`: the unmodified corpus file matches the pinned
  oracle's output exactly, including native C-stack wording.

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

- [x] Treat `global` as a soft/contextual identifier where Lua permits it; do
  not reserve it globally for Sol-specific syntax. Cover declarations, table
  fields, labels, and expressions.
  - The lexer always tokenizes `global` as a plain `Ident`; the parser only
    treats it as the declaration keyword at Lua's own declaration-statement
    positions, matching real Lua's non-instrumented (non-`T`/ltests) grammar.
    Covered as an ordinary identifier in `load()`-evaluated source, a table
    field, a local variable, and a label/goto target (see
    `dynamic_lua_runtime_global_is_a_contextual_keyword_not_a_reserved_word`).
- [x] Centralize chunk-name formatting used by parse errors, runtime errors,
  and debug information so `goto.lua` and `errors.lua` cannot drift.
  - `short_src` (`natives_debug.rs`) matches real Lua's `luaO_chunkid` exactly,
    and `display_chunk_name` (`natives_load.rs`) delegates to it, so `load()`'s
    parse/runtime error prefix and `debug.getinfo`'s `short_src` agree (see
    `chunk_name_truncation_matches_reals_luao_chunkid_for_equals_at_and_string_sources`).
- [ ] Add focused tests for every pending parser diagnostic before changing
  parser recovery, scope unwinding, or bytecode emission. Assert failure kind
  and source location as well as rendered text where stable.
- [x] Promote `goto.lua` only when execution semantics and applicable
  diagnostics both match the oracle.
  - The unmodified corpus file now runs end to end under `sol run`, exits 0,
    and prints `OK`; promoted to `pass` in the manifest.

Evidence: parser/compiler unit tests plus an unchanged `goto.lua` differential
run. Sol-only typed syntax must remain unavailable to plain `.lua` source
unless the language contract explicitly permits it.

### 3. Match string and vararg identity without allocation regressions

`strings.lua` and `vararg.lua` need representation changes, not special-case
library behavior.

- [x] Split short-string interning from long-string allocation at Lua's
  `LUAI_MAXSHORTLEN` boundary (40 bytes, a byte-length threshold, not a
  Unicode-character one). This already existed (`Heap::alloc_string` vs.
  `Heap::alloc_string_fresh`/`Heap::MAX_SHORT_STRING_LEN`, landed with the U2
  canonical-heap migration) before this item was written; `strings.lua`'s
  real remaining blocker was a separate reporting bug (below), not a missing
  split.
- [x] Verify equality, hash/table-key lookup, concatenation, and GC
  reachability around the split. Long equal strings compare equal and hash to
  the same table key regardless of which call produced them (see
  `dynamic_lua_runtime_long_strings_from_different_sources_share_one_table_key`),
  but must not gain short-string identity: `LuaValue::identity_address`'s
  `String` arm was `| 1`-masking an already-unique `ObjectId`, a leftover from
  an older content-hash-based scheme that, on the `ObjectId` slot encoding,
  collapsed two adjacent long strings' `%p` addresses together. Fixed by
  removing the mask; covered by
  `dynamic_lua_runtime_two_separately_computed_long_strings_never_share_identity`.
  `strings.lua` now runs end-to-end and is promoted to `pass`.
- [x] Replace eagerly allocated named-vararg tables with a lazily materialized
  virtual vararg view. Its `n` field, numeric lookup, missing-index behavior,
  mutation/materialization boundary, and lifetime must match Lua observations.
  A compile-time safety scan (`try_lower_vararg_to_lazy_view` in
  `lua_bytecode/mod.rs`) proves, per function, that the named vararg's
  register is never captured and never used except as the `base` of an
  index/field access; when it qualifies, `new_lua_frame` leaves the register
  `Nil` instead of eagerly building a `Table`, and 4 dedicated opcodes
  (`VarargIndexGet`/`VarargIndexSet`/`VarargFieldGet`/`VarargFieldSet`)
  answer reads/in-range writes directly from `frame.varargs`, materializing a
  real `Table` only on an out-of-range or non-integer-keyed write.
- [x] Account for the view's backing arguments in frame roots and close/unwind
  paths before collection stress is enabled. Already satisfied by existing
  infrastructure: `gc.rs`'s `push_lua_frame_roots` already roots every value
  in `frame.varargs` for every live frame (pre-dating this item, originally
  for `...` expansion), so the lazy view's backing storage was already
  GC-safe with no additional root-scanning changes needed.

Evidence: `strings.lua`'s exact output match plus the table-key and identity
regression tests above satisfy this item for strings. `vararg.lua` is now
`pass` too: `dynamic_lua_runtime_named_vararg_lazy_view_answers_like_table_pack_with_no_allocation`/
`dynamic_lua_runtime_named_vararg_lazy_view_supports_in_range_writes`/
`dynamic_lua_runtime_named_vararg_lazy_view_falls_back_to_a_real_table_for_out_of_range_writes`
(`crates/sol/tests/lua55_dynamic_runtime_scoping.rs`) cover the lazy view's
zero-allocation, in-range-write, and materialize-on-demand-fallback
contracts, and the unmodified upstream `lua-5.5.1-tests/vararg.lua` now runs
end to end under `sol run`, exits 0, and prints `OK`.

### 4. Complete observable collector behavior in dependency order

Correct root ownership comes before collector tuning. Resolve the table
reference panic before interpreting a result as a generational-GC mismatch.

1. [x] Define the `collectgarbage("count")` accounting contract. The actual
   fault was not table-capacity retention as originally suspected: every
   heap-object variant's byte footprint must charge its own struct's
   `size_of`, never `size_of::<HeapObject>()` (the whole enum, sized to its
   largest variant) - the `String` arm was doing the latter, inflating every
   live string's counted cost by the gap between a string and whatever the
   biggest variant happens to be. `gc.lua`'s growth/deletion/resize/weak-table
   assertions now pass against the pinned oracle with this fixed.
2. [x] Implement a genuine young-generation collection path rather than
   simulating minor collection with a full trace. Specify young roots,
   promotion/aging, and when a major cycle is required.
   `Heap::collect_minor_with_conditional_roots` traces only the young
   generation each call (`should_trace_during_minor`'s remembered-or-young
   gate), promotes a surviving young object to `Old` on the same pass a
   major collection would, and defers all `Old`-object reachability
   decisions to the next major collection.
3. [x] Add and exercise write barriers/remembered-set maintenance for every
   old-to-young store, including table keys and values, closures, upvalues,
   coroutine stacks, and runtime-owned roots. `write_barrier` covers every
   mutation site (`table_set`, `set_metatable`, closure upvalues, native
   callable captures, thread state, upvalue set, userdata user values); the
   minor collector's mark phase also seeds its queue from `self.remembered`
   itself, not just current roots, so an `Old` object unreachable from any
   root this round but still spared by the minor sweep has its remembered
   edges to `Young` children traced too (see deliverable evidence below).
4. [x] Validate weak tables, ephemerons, finalization, and close/unwind paths
   under minor and major collections; shared machinery still needs independent
   regressions. Covered by
   `dynamic_lua_runtime_generational_mode_clears_weak_entries_in_a_single_step`
   (weak-value clearing under a minor collection) and
   `dynamic_lua_runtime_minor_collection_traces_remembered_old_objects_unreachable_from_roots`
   (finalizer resurrection across an intervening minor collection).
5. [x] Tune `step` pacing and debt only after those semantics hold, so
   `gengc.lua` observes the same phases and weak-value clearing as the oracle.
   `collectgarbage("step")` now dispatches to the minor collector directly
   under `"generational"` mode (`step_garbage`), always finishing the pass in
   one call, matching real Lua's own minor-cycle contract.

Evidence: deterministic `gc.lua` and `gengc.lua` probes that assert
reachability and phase transitions, then the corpus rows. Elapsed time or a
single heap-size sample is not sufficient collector evidence. Both `gc.lua`'s
and `gengc.lua`'s corpus rows are now promoted to `pass`.

### 5. Make the host/native boundary an explicit release decision

The ten `host-required` rows cannot become portable U6 passes by accident. The
project must record whether its supported native profile is source-only,
Lua-C-API-compatible, or a separately documented embedding profile.

- [x] Write the decision record and capability matrix for filesystem, process,
  locale, package searchers, dynamic/native modules, C callbacks, and binary
  chunks. Name what is portable, host-gated, and denyable by an embedding.
  Recorded in `docs/features/host-native-boundary.md`.
- [x] Implement selected native services behind explicit host traits; do not
  let `io`, `os`, package loading, or native modules acquire ambient access in
  the portable runtime. Already implemented via `sol_core::Capabilities`
  (`SANDBOX` default, opt-in `NATIVE_CLI`/`with_capabilities`) and
  `natives.rs`'s capability-gate dispatch; see the capability matrix in
  `docs/features/host-native-boundary.md` for the field-by-field mapping.
- [x] Treat binary chunk load/dump as a versioned-format task. Define accepted
  versions, invalid-header behavior, portable versus native support, and
  fixture-based tests rather than relying on the host's current Lua binary.
  Implemented in `natives_load.rs` (`lua55_binary_chunk_header`/
  `SOL_DUMP_MAGIC`/`load_binary_chunk`); three fixture tests in
  `crate/sol/tests/lua55_dynamic_runtime_string.rs` cover round-trip,
  header/truncation, and foreign-payload rejection. Cross-process
  portability with a real external `luac5.5` stays out of scope by decision
  (see `docs/features/host-native-boundary.md`).
- [x] Add embedding/C-API/module tests only for the approved profile and retain
  `host-required` for unsupported capabilities. The approved profile *does*
  include a real C API and native-module loader (ADR 0002), both already
  implemented (`c_api.rs`, `crate/sol/include/*.h`) and tested end-to-end by
  `crate/sol/tests/native/embedding_smoke.c` + `sol_fixture.c`
  (`scripts/test-sol-c-api.sh`, reconfirmed passing). What stays
  `host-required` is narrower than "no C API": PUC Lua's internal `ltests`
  test harness (`api.lua`, `cstack.lua`'s `if T then` block, `memerr.lua`),
  filesystem-path-based module search (`attrib.lua`, `tracegc.lua`,
  `locals.lua` — a separate, already-documented deviation, not part of this
  boundary), and standalone-executable process/REPL semantics (`all.lua`,
  `main.lua`); `code.lua` stays `host-required` for the separate
  cross-process binary-chunk reason above. See
  `docs/features/host-native-boundary.md` for the precise reasoning per row.

Evidence: the decision record (`docs/features/host-native-boundary.md`),
capability-denial tests (`crate/sol-core/src/capabilities.rs`'s unit tests
plus `natives.rs`'s gated-native tests), and binary-chunk fixture tests
(`lua55_dynamic_runtime_string.rs`). This ran alongside portable work and did
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
