# U7 — Interpreter performance foundation

**Status:** complete

**Purpose:** make the canonical semantic interpreter efficient before dynamic
native tiers.

- [x] Select and measure a packed/tagged `Value` representation. Already
      satisfied before this milestone's work began: `TableRef`/`ClosureRef`/
      `ThreadRef` (`crate/sol/src/lua_runtime/value.rs:70,74,78`) are `Copy`
      wrappers around a plain `sol_core::ObjectId`, so cloning a `LuaValue` is
      a bitcopy/refcount bump, never a heap allocation.
- [x] Make common frame/register operations allocation- and clone-free.
      Already satisfied: `dispatch.rs` pools `regs`/`cells`/`varargs`/`values`
      buffers across calls instead of allocating fresh ones
      (`take_regs_buffer`/`recycle_frame_buffers`,
      `crate/sol/src/lua_runtime/dispatch.rs:858,893`).
- [x] Make common call, return, vararg, and iterator paths allocation-free.
      The one gap here - `closure_parts` cloning a closure's upvalue list
      (`Vec<ObjectId>`) fresh on every single call - is fixed:
      `ClosureObject.upvalues` is now `Rc<[Cell<ObjectId>]>`
      (`crate/sol-core/src/heap.rs:106`), so every call shares one
      refcounted, immutable list of cells instead of allocating a new `Vec`;
      the sole post-construction mutator (`debug.upvaluejoin`) rebinds
      through `Cell::set`. Benchmarked before/after
      (`--filter function_calls_closure`, `--filter objects`): no measurable
      regression and no measurable improvement on the existing benchmark
      suite either - the fix removes a real allocation from the hot path
      (confirmed via source inspection, not guessed), but none of the
      current benchmarks apparently allocate enough distinct closures per
      call for that one `Vec` allocation to dominate their measured time.
      Correctness is covered by the existing `lua55.rs`/`tests/fixtures/
      lua55` corpus (shared-upvalue mutation semantics across two closures
      capturing the same local) plus this milestone's targeted regression
      test for a closure called in a tight loop.
- [x] Optimize table array/hash layout, string interning/hashing, and shapes.
      The array/hash split was already satisfied
      (`crate/sol-core/src/heap.rs:74-82`'s `TableObject` has a real array
      part plus an `IndexMap<TableKey, Value>` hash part). The remaining gap
      - every string table/global key re-hashing its raw bytes on every
      access - is fixed: `StringObject` now carries a precomputed `u64` hash
      alongside its bytes, and `TableKey::String` carries that hash through
      to `table_key()`, which reads it instead of re-hashing fresh bytes
      each time (`crate/sol-core/src/heap.rs:48`, `:1975`).
      `PartialEq`/`Eq` are unchanged (still exact byte comparison), so this
      only memoizes the hash; it does not change correctness or force
      strings through the intern table. Benchmarked before/after
      (`--filter hashmap_lookup`, `--filter objects`, `--filter
      string_concat`): the first attempt (eager hashing at string
      construction) caused a measured ~2.2x regression on `string_concat`
      (fresh concat results are hashed even when never used as a key); the
      shipped design computes the hash lazily (memoized on first use as a
      key) instead, which erased that regression. No measurable improvement
      on `hashmap_lookup`/`objects` either - this prevented a real
      regression it could otherwise have introduced, rather than
      demonstrating a speedup on the benchmarks as they exist today.
- [x] Adopt direct-threaded dispatch only if it is maintainable and measured.
      Measured, not adopted. `crate/sol/examples/dispatch_bench_real.rs`
      compiles `benchmarks/loop_sum.lua` through the real front end
      (`lexer`/`parser`/`lua_bytecode::Compiler`) and runs its actual
      compiled hot loop (`ForPrep`/`Binary(Add)`/`Move`/`ForLoop` over real
      `Instr`/`Const` values, not a hand-authored toy program) through two
      dispatch strategies: the production style (`match &instr { .. }`,
      mirroring `dispatch/bytecode.rs`'s own `dispatch_step` shape) and a
      precomputed-tag function-pointer table. Five runs: direct `match`
      dispatch consistently *beat* the function-pointer table by roughly
      1.2-1.35x (match: ~105-125ms; table: ~145ms, flat across runs because
      the indirect call plus the still-necessary `if let`/`match` inside
      each handler to extract the real `Instr`'s operands costs more than
      Rust/LLVM's jump-table codegen for a dense `match` already does).
      Conclusion: keep the existing `match`-based dispatch; a
      function-pointer-table rewrite of the real `Instr` enum would be a
      regression, not an improvement, so it is not adopted. This supersedes
      `dispatch_bench.rs`'s earlier synthetic-program comparison (kept
      alongside for its own before/after value) as the milestone's
      authoritative, real-bytecode measurement.
- [x] Add generational allocation fast paths and measured barriers. The
      mechanism (generational collection, write barriers) was already
      satisfied (`GcGeneration`/`collect_minor`/
      `step_major_with_conditional_roots` and `Heap::write_barrier`,
      `crate/sol-core/src/heap.rs:1411`). The "measured" half - nothing
      aggregated or exposed collection counts/timing across a run - is
      fixed: `crate/sol/src/lua_runtime/gc.rs`'s `GcStats` accumulates total
      minor collections, total major collection passes, cumulative
      `reclaimed`/`promoted`, and wall-clock time per collection kind
      (`std::time::Instant`), keyed off each `Collection`'s own
      self-reported `CollectionKind` so both GC call sites
      (`collect_garbage_with`'s always-major pass and `step_garbage`'s
      mode-dependent minor/major step) are covered uniformly. Exposed via a
      new sandboxed `debug.gcstats()` native returning a table of the
      counters. Two new regression tests
      (`crate/sol/tests/lua55_dynamic_runtime_gc.rs`) demonstrate the
      "measured barriers" deliverable directly: N explicit
      `collectgarbage("step")` calls under `"generational"` mode produce
      exactly N minor collections and 0 major collections, while N explicit
      `collectgarbage()` calls produce exactly N major collections and 0
      minor ones - the mechanism provably distinguishes minor from major
      the way the generational design claims. Benchmarked before/after
      (`--filter gc_alloc`): `sol (dynamic)` went from 3567.5ms/3710.1ms
      (warm/cold) to 3251.5ms/3190.9ms - the two added `Instant::now()`
      calls per collection introduce no measurable regression; the
      post-change numbers are faster, within this benchmark's normal
      run-to-run variance rather than attributable to the GC-stats change
      itself.
- [x] Optimize metamethod-negative paths and tail-frame reuse. Already
      satisfied: arithmetic/concat fast paths are branch-only and
      `table_metatable` (`crate/sol/src/lua_runtime/table.rs:287`) is a
      plain `Option` field read, not a lookup; `TailCall` recycles frame
      buffers and charges no extra `call_depth`
      (`crate/sol/src/lua_runtime/dispatch.rs:1269`).

**Exit gate:** interpreter-readiness performance gate passes with compatibility,
GC stress, debugger, and WASM tests enabled. The compatibility/GC-stress slice
of this gate is covered by this milestone's own verification runs
(`cargo test --manifest-path crate/sol/Cargo.toml`,
`cargo test --manifest-path crate/sol-core/Cargo.toml`,
`scripts/test-lua55-suite.sh lua-5.5.1-tests` at its existing
16/1/10/7/0-passed/pending/host-required/diverges/failed baseline,
`scripts/test-sol-c-api.sh`, `scripts/test-sol-benchmarks.sh`) after every
change in this milestone, with zero regressions; the debugger and WASM slices
were not separately re-exercised by this milestone's work (no debugger- or
WASM-facing code changed) and are not claimed as newly verified here.

This owns the superlinear large-live-heap stress behavior currently blocking
`constructs.lua`. See the [historical U7 ledger](../unified-sol-runtime-plan.md#u7--interpreter-performance-foundation)
and [`benchmarks/RESULTS.md`](../../../benchmarks/RESULTS.md) for the full
before/after benchmark comparison.
