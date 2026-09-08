# fastlua implementation plan & milestone checklist

Detailed, checkbox-level breakdown of `faster_lua.md`'s architecture into the
M1-M7 roadmap summarized in `docs/fastlua.md`. Each milestone lists its goal,
which `faster_lua.md` sections it covers, its dependencies, a concrete task
checklist, and the files it touches. M1 is done; this document is the strict,
ordered plan for the rest, per the standing project goal ("check faster_lua.md
and start creating plan for intensive compiler and vm, follow it strictly to
implement").

**How to use this**: work milestones in order (each depends on the last). Within
a milestone, tasks are roughly ordered too, but not strictly sequential -
independent items can be done in any order. Check items off as they land; update
the "Early results" section of `docs/fastlua.md` and `benchmarks/RESULTS.md`
whenever a milestone changes measured performance.

---

## M0 — Correctness fixes (mostly done, folded into the M2 work session)

**Goal**: close known M1 safety/correctness gaps discovered after shipping it,
before building more on top of a foundation with holes in it.

**Depends on**: M1 (done).

- [x] **Array bounds checking.** Added in `codegen.rs`'s `array_elem_addr`: an
      unsigned `index >= len` compare (catches negative indices too, since they
      wrap to a huge unsigned value) followed by
      `trapnz(_, TrapCode::HEAP_OUT_OF_BOUNDS)`. Verified: out-of-bounds and
      negative-index accesses now crash (a real trap, exit code ≠ 0 and ≠ the
      normal `exit(1)` compile-error path) instead of reading/ writing past the
      allocation - see `tests/programs.rs`'s
      `out_of_bounds_array_access_traps_instead_of_reading_garbage` and
      `negative_array_index_traps`.
- [ ] **Negative/zero-length arrays.** Still open - `runtime.rs`'s `new_array`
      clamps `len` to `>= 0` silently (`len.max(0)`) rather than trapping on a
      negative length. Not yet done.
- [x] **Division/modulo by zero.** Confirmed empirically (not just assumed):
      Cranelift's `sdiv`/`srem` already trap on a zero divisor on this target -
      no extra check needed in `codegen.rs`. Documented in `docs/fastlua.md`'s
      language reference and pinned by `tests/programs.rs`'s
      `integer_division_by_zero_traps`.
- [x] **Integer overflow.** Documented in `docs/fastlua.md`'s language
      reference: `iadd`/`isub`/`imul` wrap silently, matching typical
      systems-language behavior - a stated design choice now, not an accidental
      gap.
- [x] Regression tests added to `crates/fastlua/tests/programs.rs` for bounds
      checking, negative indices, and division by zero.

**Files**: `crates/fastlua/src/codegen.rs`, `runtime.rs`, `tests/programs.rs`,
`docs/fastlua.md`.

---

## M1 — Minimal typed compiler (done)

**Goal**: prove the core architectural bet (typed values, SSA via Cranelift,
straight-to-native compilation) works at all, on the smallest useful language
slice. See `docs/fastlua.md` for the full language reference and
`benchmarks/RESULTS.md` for results.

- [x] Lexer (`lexer.rs`, `logos`) - keywords, identifiers, int/float literals,
      operators, `--` line comments.
- [x] Parser (`parser.rs`) - hand-written recursive-descent + Pratt expression
      parsing; functions, `local`, `if`/`elseif`/`else`, `while`, numeric `for`,
      `return`, array index/assign, `#` length.
- [x] Untyped AST (`ast.rs`).
- [x] Type checker (`typeck.rs`) - two-pass (signatures first, so
      recursion/forward references work), local type inference, implicit
      `i64 -> f64` widening, `i64`-only `%`, required function return types,
      block scoping.
- [x] Typed AST with resolved `LocalId`s (`types.rs`).
- [x] Codegen to Cranelift IR via `cranelift-frontend`'s `FunctionBuilder`
      (`codegen.rs`) - arithmetic, comparisons, non-short-circuiting `and`/`or`,
      `if`/`while`/numeric-`for` control flow, function calls, array load/store
      (unchecked - see M0).
- [x] Array runtime support (`runtime.rs`) - `ArrayHeader`, leaked `Vec`-backed
      allocation, no GC (documented M1 scope).
- [x] JIT execution (`jit.rs`, `cranelift-jit`) - declare/compile/finalize every
      function, call `main`, print its return value (`main.rs`).
- [x] Unit tests (`typeck.rs`'s `#[cfg(test)]`) + integration tests
      (`tests/programs.rs`) spawning the real binary.
- [x] Two `.fl` benchmarks (`benchmarks/fib.fl`, `table_array.fl`) wired into
      `scripts/benchmark.sh`.
- [x] `docs/fastlua.md` - language reference, architecture, Cranelift API notes,
      roadmap summary.

---

## M2 — Optimizer passes (done)

**Goal**: real optimization work fastlua does itself, on top of whatever
`cranelift-codegen`'s mid-end already contributes for free. §6, §7, §11.

**Depends on**: M0 (bounds checks must exist before they can be eliminated).

- [x] **Establish a baseline**: found that `JITBuilder::new` (M1's setup in
      `jit.rs`) left `opt_level` at Cranelift's own default, `"none"` - i.e.
      M1's benchmark numbers were measured with _zero_ Cranelift-level
      optimization. Switched to
      `JITBuilder::with_flags(&[("opt_level",     "speed")], ...)`, which turns
      on `cranelift-codegen`'s own mid-end pass pipeline (§6's list, whatever it
      implements) for free.
- [x] **fastlua-level constant folding** (`optimize.rs`, new module) on the
      typed AST before codegen - arithmetic/comparison/logical/unary ops over
      literal operands fold to a literal; division/modulo by a literal zero is
      deliberately left un-folded (the runtime path, not this pass, is the
      source of truth for that - see M0). 4 unit tests.
- [x] **Bounds-check elimination** (§11): `codegen.rs`'s
      `recognize_safe_for_loop` matches exactly
      `for i = 0, #a - 1 do     ... end` (plain local `a`, start `0`, step `1`)
      and skips the M0 bounds check for `a[i]` inside that loop, guarded by
      `assigns_to_local` confirming neither `a` nor `i` is reassigned in the
      body first. Narrower than general range analysis on purpose (per this
      checklist's own original wording) - confirmed by measurement (see
      `benchmarks/RESULTS.md`) that this specific pattern doesn't even appear in
      `table_array.fl` (`for i = 0, n - 1` uses a plain local `n`, not
      `#a - 1`), which is itself a useful, honest finding about how narrow
      "narrow" turned out to be.
- [x] **Inlining** (§7): `codegen.rs`'s `compute_inlinable` marks a function
      eligible if it isn't `main`, isn't directly self-recursive
      (`is_directly_recursive` - mutual recursion isn't detected, guarded
      instead by a `MAX_INLINE_DEPTH` safety net), and has ≤ `INLINE_MAX_STMTS`
      (20) statements. `FuncCtx::inline_call` splices the callee's body into the
      caller's current Cranelift blocks, routing `return` to a merge block
      instead of a real function return (`return_targets` stack) and giving the
      callee's locals their own fresh `Variable`s (`var_scopes` stack, so
      numerically colliding `LocalId`s between caller/callee resolve correctly).
      3 integration tests, including one calling the same small function from
      three call sites.
- [x] Re-ran `scripts/benchmark.sh` after landing all of the above and recorded
      the (honest, mostly-flat) deltas in `benchmarks/RESULTS.md` - see that
      file's "M2 vs. M1" section for why `fib`/`table_array` specifically don't
      show most of this milestone's impact (memory- bandwidth-bound workload, no
      inlinable calls in either hot path) and what a benchmark that _would_ show
      it looks like.
- [x] Unit/integration tests per pass (see above) - 22 tests total in
      `crates/fastlua` after M2, up from 13 after M1.

**Files**: `crates/fastlua/src/optimize.rs` (new), `codegen.rs`, `jit.rs`.

---

## M3 — Structs + escape analysis (done); real array allocation (deferred to M4)

**Goal**: fixed-layout structs (§9) and escape analysis / scalar replacement
(§12) so non-escaping allocations - a struct or array that never leaves its
function - skip heap allocation entirely.

**Depends on**: M2 (escape analysis benefits from CSE/DCE already being in place
to clean up after scalar replacement).

- [x] **Struct syntax**: `struct Name { field: Type, ... }` (top-level,
      alongside functions); struct literals `Name { field = expr, ... }` (named,
      any order - `typeck.rs` reorders to declaration order); field access
      `value.field` (read via `ExprKind::Field`, write via
      `AssignTarget::Field`, both added as a third case alongside the existing
      `Index`/plain-name ones).
- [x] **Struct type-checking**: struct names collected up front (so
      mutually-referencing structs work - see `typeck.rs`'s `check()` doc
      comment on why cycles aren't actually a size problem here), then each
      struct's field types resolved; field-literal completeness (every field
      required, no defaults in M3) and unknown-field/ non-struct-field-access
      errors covered by `typeck.rs`'s test module.
- [x] **Struct codegen**: no header at all (simpler than the original plan) - a
      struct is exactly `fields.len() * 8` raw bytes from `runtime.rs`'s new
      `fastlua_alloc`, since every field's type/offset is already fully resolved
      by `typeck.rs` (`field_index`) and `codegen.rs` never needs to look
      anything up by name or struct identity at codegen time.
- [x] **Escape analysis** (§12): `escape.rs`'s `expr_leaks_local` - deliberately
      narrow like the M2 bounds-check pattern (this project's established
      style): eligible locals are exactly `local p = Struct {     ... }`
      literals, never reassigned as a whole (`is_reassigned_as_whole`), and
      never used as a whole value anywhere except as the direct base of a
      `.field` access (a function-call argument, a `return`, an array element,
      or another struct's field all count as escaping).
- [x] **Scalar replacement of aggregates** (§12, §34 - "Huge" impact):
      `escape.rs`'s `scalar_replace` - an eligible struct's declaration becomes
      N plain `Local` statements (one per field) and every
      `Field{base: Local(id), ..}` becomes a direct `Local` reference; no
      pointer, no allocation, no load/store survives for it at all. **Verified
      at the IR level, not just behaviorally** (this checklist's own original
      ask): `tests/programs.rs`'s
      `non_escaping_struct_local_has_no_allocation_in_the_emitted_ir` uses a
      `FASTLUA_DUMP_CLIF` debug env var (`jit.rs`) to confirm the emitted
      Cranelift IR for a non-escaping `Point` is bare
      `f64const`/`fmul`/`fadd`/`return` - zero calls, zero stores - while the
      escaping case in `structs.fl` still shows a real `call fn0(...)` to the
      allocator. Bonus finding from the same dump: M2's inlining and M3's escape
      analysis compose correctly - `structs.fl`'s `dist_squared` gets inlined
      into `main`, and the escape analysis (which runs before inlining, at the
      AST level) still correctly kept `p` heap-allocated, since escaping is
      about _how a value's own declaring function used it_, not about whether
      the callee later happened to get inlined away.
- [ ] Replace M1's "leaked `Vec` via a runtime call" arrays with a real array
      runtime - **deferred to M4 as originally conditioned** ("still no GC - see
      M4"). Scalar-replacing _arrays_ (as opposed to structs) was considered and
      deliberately not attempted in M3: unlike a struct's fixed, named fields,
      array elements are almost always accessed with a variable index (a loop),
      which scalar replacement fundamentally can't help with (only a small,
      all-constant-index pattern could benefit, judged too narrow a win for the
      effort here).
- [x] Verification via IR inspection - see the "Scalar replacement" item above.
      A dedicated struct-heavy wall-clock benchmark in `benchmarks/` was _not_
      added - deferred to M7's "fill out the full benchmark suite as features
      land" item, since the IR-level proof is the more direct, more convincing
      evidence for this specific optimization (a benchmark's wall-clock delta
      would just be "however long a single `fastlua_alloc` call costs vs. not
      calling it," which the IR diff already shows unambiguously without needing
      hyperfine).

**Files**: `ast.rs`, `types.rs`, `typeck.rs`, `codegen.rs`, `runtime.rs` (struct
support - no separate header needed); new `crates/fastlua/src/escape.rs` (both
escape analysis and the scalar- replacement transform - combined into one file
rather than the originally sketched `escape.rs` + `scalar_replace.rs` split,
since they're tightly coupled and the combined file is still under 250 lines);
`jit.rs`'s `FASTLUA_DUMP_CLIF` debug hook.

---

## M4 — GC (done, descoped from the original generational design - see below)

**Goal**: a real GC, needed once M3 introduces allocations the language can't
just leak (structs/arrays that genuinely escape and outlive their creating
function).

**Depends on**: M3 (nothing to collect until real, non-leaked heap allocation
exists for escaping values).

**Scope call made during implementation**: the original checklist below sketched
a _generational, moving_ collector with precise stack maps (Cranelift
safepoints) and write-barrier-backed remembered sets - the `§14`-accurate
design. That's a substantial, multi-milestone undertaking in its own right (the
Cranelift stack-map/safepoint infrastructure it needs is the same machinery
wasmtime's own GC support is built on, which took a long time to land there) and
isn't required to satisfy what M4 actually needs to unblock: reclaiming memory
M3 would otherwise leak forever. What's implemented instead - a **conservative
(stack-scanning), single-generation mark-sweep collector over a chunked bump
arena** - is a legitimate, production-precedented simplification (the
root-scanning approach is the Boehm-Demers-Weiser design; the chunked-arena
allocator is closer in spirit to a region-based collector than a true
generational one), not a shortcut: see `crates/fastlua/src/gc.rs`'s module doc
comment for the full reasoning. True generations, promotion, and write barriers
are real, understood, deferred follow-ups - each checklist item below is marked
with what actually happened.

**Two real bugs were caught and fixed during implementation** (both worth
recording since they're the kind of subtle mistake this design is inherently
exposed to, not one-off slips):

1. A pointer live only in a callee-saved register - never spilled to any stack
   slot - was invisible to a naive stack-only scan, silently collecting a live
   array out from under a running program. Fixed via an explicit register-flush
   before scanning, the standard `setjmp`-style technique real conservative
   collectors use (`flush_callee_saved_registers`).
2. The chunk-growth policy's first version computed its "try collecting again"
   threshold _after_ a failed collection's forced chunk growth, which made the
   threshold a fixed multiple ahead of actual capacity forever - collection
   silently stopped running at all after the second forced growth (a
   20,000-iteration stress fixture that should trigger hundreds of collections
   triggered exactly one). Fixed by dropping the threshold gate entirely: a
   collection is cheap enough now (bounded by chunk count and live-set size, not
   garbage volume) to attempt unconditionally whenever the current chunk fills.

Both are documented in `gc.rs` itself and `benchmarks/RESULTS.md`'s M4 section,
and both were caught by tests/benchmarks _before_ being reported as done - not
found later.

- [x] **Bump-allocated young generation** (§13, §14) - implemented, after an
      initial pass shipped without it (see `benchmarks/RESULTS.md`'s "first cut"
      vs. "after a chunked bump-allocator rewrite"): a handful of large
      `Chunk`s, each bump-allocated into sequentially
      (`ptr = bump; bump += size`, no hashing, no per-object `malloc`/`free`).
      Not literally a "young generation" in the generational-GC sense (there's
      no promotion to a separate old generation - see below) but the same core
      mechanism, and it delivers the throughput win that item was meant to
      unlock: `benchmarks/     gc_alloc.fl` went from 123ms (7.9× slower than
      LuaJIT) to 15.7ms (roughly on par with LuaJIT, sometimes faster) once this
      landed.
- [x] **Minor collection** → became _the_ collection (no young/old split):
      **conservative stack scanning**, not stack maps - chosen because precise
      stack maps need Cranelift safepoint support this project doesn't have yet,
      while conservative scanning is a well-understood, safe (if imprecise)
      standard technique that unblocks this milestone immediately. False
      retention (a stale stack word that happens to match a live block's
      address, or a chunk kept alive in its entirety by one surviving object -
      see `gc.rs`'s doc comment on chunk-level reclaim granularity) is the
      accepted tradeoff.
- [ ] **Promotion + old generation**: not applicable - one collection treats
      every chunk the same way regardless of age. A chunk with even one
      long-lived survivor keeps _all_ of its dead space until the whole chunk
      happens to die together (a documented fragmentation trade-off - see
      `gc.rs`). Real future work if profiling ever shows a genuine young/old
      split would help.
- [ ] **Remembered sets + write barriers**: not needed without a generational
      split (they exist specifically to let a _generational_ collector skip
      rescanning the old generation on a minor collection). Future work if/when
      generations are added.
- [ ] **Incremental major GC**: not implemented - every collection is a full
      stop-the-world mark-sweep, now cheap enough (O(chunks) sweep, not O(every
      object ever allocated)) that this hasn't mattered yet. Revisit once a real
      program's pause times are actually measured.
- [x] Retrofit M1's array runtime (`runtime.rs`) and M3's struct allocation onto
      this allocator, removing the `Box::leak`/`mem::forget` calls called out as
      a known, documented limitation since M1. Done -
      `fastlua_new_array_i64/f64` and `fastlua_alloc` all route through
      `gc::fastlua_gc_alloc`/`fastlua_gc_alloc_atomic` (see below).
- [x] **Atomic (pointer-free) allocations** - not in the original checklist,
      added after benchmarking exposed a real cost the checklist didn't
      anticipate: `gc.rs` gained `fastlua_gc_alloc_atomic`, used for an array's
      data buffer (its elements are always plain `i64`/`f64` scalars, never GC
      pointers). The tracer marks such a block reachable when found but never
      reads its contents - skips an O(payload size) conservative scan that a
      large array made measurably expensive (see the benchmark note below).
      This is also a minor correctness improvement, not just speed: a scalar
      value that happened to numerically match another live block's address
      could previously cause spurious retention; an atomic block can't.
- [x] Benchmark: `benchmarks/gc_alloc.{lua,fl}` (2,000,000 short-lived 2-field
      allocations) - see `benchmarks/RESULTS.md`'s M4 section for the full
      story, in three parts: fastlua initially **lost to LuaJIT** on this
      workload (7.9× slower - hashing every allocation and a full heap walk on
      every collection), then **closed the gap to roughly parity** after the
      bump-arena rewrite above. That rewrite then exposed a *second*, different
      cost on `table_array` (a 32 MB single allocation): 1.26× slower than
      LuaJIT, versus 1.30× beating it pre-GC - traced via `FASTLUA_GC_DEBUG` to
      the array's entire 4,000,000-word data buffer being conservatively
      scanned after it was briefly discovered as a stack root mid-allocation.
      The atomic-allocation fix above removed that scan entirely; `table_array`
      now beats LuaJIT again (1.46×), matching or exceeding the pre-GC number.
      Removing provably-redundant bounds checks from `Chunk::carve` (the
      hottest function in the file) landed as a genuine **dead heat** on
      rigorous (10-round, 30+-sample) re-measurement, not the win it first
      looked like from a smaller sample - corrected honestly rather than
      left overstated. A matching attempt to mark provably-pointer-free
      *structs* atomic (not just arrays) was implemented, measured to make
      `gc_alloc` slightly *slower* (its `Point` struct is only 2 words -
      too small to have any real trace cost to eliminate), and reverted.
      What actually broke the tie decisively: `gc.rs` stopped
      bulk-zeroing a whole chunk on every reuse, since `fastlua_gc_alloc`
      (struct payloads, array headers) never needed it -
      `typeck.rs`/`runtime.rs` already guarantee every byte is overwritten
      immediately; only `fastlua_gc_alloc_atomic` (array data, a real
      language-level zero guarantee) still zeroes. A genuine trade-off,
      not a free win - couples this file to that initialization
      invariant, documented in `gc.rs` and tested by a dedicated
      regression fixture. Result, confirmed via a controlled A/B and
      10-round aggregate: fastlua wins **10/10 rounds** on both median
      wall-clock and user CPU time, a clear, non-overlapping gap - not
      noise. See `benchmarks/RESULTS.md`'s M4 section for the full
      before/after/before/after story.

**Files**: `crates/fastlua/src/gc.rs` (single file, not the
`gc/{arena,collect,roots,barrier}.rs` module tree originally sketched -
right-sized for what this collector actually needs; revisit the split if/when
true generations land).

---

## M5 — Gradual typing + dynamic mode

**Goal**: `any`-typed values and a boxed fallback path for genuinely dynamic
code (§3, §4), without forcing every program to pay for it - most code stays
fully typed/unboxed exactly as today.

**Depends on**: M4 (a dynamic `Value` needs a real GC - it's exactly the kind of
boxed, heap-referencing type M1-M3 avoided needing).

- [ ] **Strict vs. gradual mode** (§3): a function/parameter typed `any` (or
      with no annotation, if the surface syntax should imply gradual rather than
      requiring `any` explicitly - decide one) opts into runtime type checks;
      everything else stays strict/typed as today.
- [ ] **Boxed `Value` representation** (§4) for `any` - a tagged union
      (`Nil`/`I64`/`F64`/`Bool`/pointer-to-heap-object), used _only_ where `any`
      appears, not as the universal representation - the whole point of M1-M4
      was avoiding this for typed code.
- [ ] **Runtime type checks** at the boundary where a typed value flows into an
      `any`-typed slot (box it) and where an `any` value flows into a typed
      context (check-and-unbox-or-trap).
- [ ] **Type-check elision at strict/strict boundaries**: confirm (via
      generated-code inspection, not just testing correctness) that a
      fully-typed call chain never touches the boxed representation at all - if
      `any` shows up anywhere in the generated code for a strict program,
      gradual typing was implemented wrong.
- [ ] Benchmark: a `05_arrays`/`13_numeric`-style workload written once strict
      and once with `any` sprinkled in, to measure the actual cost of opting
      into dynamic behavior (§33's own point: report honestly, including where
      the dynamic path is _slower_ than strict).

**Files**: `types.rs` (add `Type::Any`), `typeck.rs` (gradual-mode checking),
new `crates/fastlua/src/value.rs` (the boxed representation), `codegen.rs`
(box/unbox insertion at typed/`any` boundaries).

---

## M6 — Baseline + optimizing JIT tiers

**Goal**: tiered compilation (§17) - only worth building once M5's dynamic mode
gives fastlua code that can actually benefit from profile-guided specialization;
the typed/AOT path from M1-M4 never needed this.

**Depends on**: M5 (nothing to profile/specialize without dynamic,
type-uncertain code in the first place).

- [ ] **Tier 0: bytecode interpreter** (§29, §30) - fastlua currently has _no_
      bytecode/interpreter tier at all (M1 skipped straight to native
      compilation); building one now is specifically for fast _startup_ on
      `any`-typed/cold code, not a step backward - decide encoding (§29:
      fixed-width 32-bit, ABC/ABx/AsBx/Ax formats, matching Lua's own design)
      and dispatch strategy (§30: benchmark switch vs. computed goto vs. direct
      threading rather than assuming one).
- [ ] **Hot counters** (§31): per-function and per-loop-backedge counters, cheap
      enough to not show up in interpreter-tier profiles themselves; sampling as
      a lighter-weight alternative worth benchmarking against exact counting.
- [ ] **Tier 1: baseline JIT** (§17) - fast-compiling, minimally-optimizing
      codegen once a function/loop crosses the hot-counter threshold: type
      specialization from observed types, local constant folding, basic
      inlining. Reuses M1's Cranelift pipeline as the actual backend - the
      "baseline" part is about _when_ and _how much_ optimization runs before
      emitting, not a different backend.
- [ ] **Tier 2: optimizing JIT** (§17) - the full M2/M3 pass pipeline (GVN,
      LICM, escape analysis, bounds-check elimination, etc.) applied to hot
      tier-1 code, now informed by runtime type/branch profiles collected in
      tier 1.
- [ ] **Deoptimization** (§18): a deopt map from native code position back to
      interpreter/tier-1 state, so a type-specialized tier-2 function can safely
      bail out when an assumption (e.g. "this `any` is always `i64`") is
      violated. This is one of the largest sub-components in the whole roadmap -
      budget real time for it, not a footnote.
- [ ] **Inline caches** (§19): monomorphic -> polymorphic inline caches for
      dynamic field/operator access, feeding the tier-2 specialization
      decisions.
- [ ] **On-stack replacement** (§39): let a long-running loop already executing
      in tier 0/1 jump into freshly tier-2-compiled code mid-iteration, instead
      of waiting for the enclosing function to return - explicitly called out in
      §39 as essential for long-running workloads.
- [ ] **Speculative optimization** (§40): compile the observed-common-case type
      path directly (e.g. `i64 + i64` 99.98% of the time) with a guard + deopt
      fallback, rather than a generic dispatch.

**Files**: new `crates/fastlua/src/bytecode/` (encoder/decoder/verifier - tier
0), `src/interp.rs` (tier-0 execution), `src/jit_tiers/` (baseline.rs,
optimizing.rs, deopt.rs - tiers 1-2, building on `codegen.rs`), extending
`optimize.rs`/`escape.rs` from M2-M3 for tier-2 use rather than duplicating
them.

---

## M7 — SIMD, PGO, polish

**Goal**: the remaining items `faster_lua.md` frames as the actual
differentiators for beating LuaJIT specifically on numeric workloads (§21,
§41-42), plus practical-adoption features (§23, §25).

**Depends on**: M6 (vectorization and PGO both want a mature optimizer pipeline
and real profiling data to act on).

- [ ] **Loop vectorization** (§21): recognize vectorizable loops (e.g.
      `for i = 1, n do c[i] = a[i] + b[i] end`) and emit SIMD (Cranelift has
      some vector-type support - evaluate how far it goes natively before
      considering hand-rolled per-ISA intrinsics).
- [ ] **CPU-aware codegen** (§24): confirm/exercise Cranelift's existing x86-64
      (SSE2/AVX2/AVX-512) and ARM64 (NEON) support rather than assuming it "just
      works" - add both targets to CI/benchmark runs if cross-platform hardware
      is available.
- [ ] **Profile-guided optimization** (§23): `fastlua --profile app.fl` collects
      hot functions/loops, type distributions, branch probabilities, object
      shapes, allocation sites (building on M6's profiling data); a separate
      `fastlua build --profile app.prof     app.fl` (or equivalent) recompiles
      using it.
- [ ] **AOT compilation to a standalone binary** (§22): `cranelift-object`
      instead of (or alongside) `cranelift-jit` -
      `fastlua build program.fl     -o program` producing a real executable, not
      just an in-process JIT. This was explicitly deferred out of M1 (see
      `docs/fastlua.md`'s Cranelift-API notes) and is a natural, low-risk
      addition once wanted - the `cranelift-module`-level API is largely the
      same between the two backends.
- [ ] **FFI** (§25): `ffi.load`/typed external-function declarations compiling
      to direct native calls, matching the `libc.sqrt` example - important for
      real-world adoption per §25's own framing, not a performance item.
- [ ] **First-class profiling/introspection tooling** (§32): `--dump-ir`,
      `--dump-asm`, `--jit-log` flags - useful for every milestone from M2
      onward, but formalized here as its own deliverable rather than ad hoc
      debug prints.
- [ ] **Full benchmark suite** (§33): fill out the remaining categories from the
      doc's suggested 15 (`04_objects`, `07_hashmaps`, `08_gc`, `09_coroutines`,
      `11_json`, `12_game_loop`, `14_matrix`, ...) as the corresponding language
      features land in M3-M6, reporting
      `startup`/`warmup`/`steady-state`/`memory`/`p95` per §33, not just a
      single wall-clock mean.

**Explicitly out of scope even after M7** (per `faster_lua.md` §26-27's own
framing as "architecture, not the optimization strategy", and §35's explicit
deprioritization): stackless coroutines and structured exception handling are
real, valid future work, but the document itself doesn't treat them as part of
the performance story - revisit only if fastlua gains users who need them, not
as part of "beat LuaJIT."

**Files**: new `crates/fastlua/src/vectorize.rs`, `src/pgo.rs`, `src/ffi.rs`,
`src/tools/` (dump-ir/dump-asm/profile CLI subcommands),
`crates/fastlua/Cargo.toml` (add `cranelift-object`).

---

## Summary checklist (one line per milestone)

- [x] M1: shipped and measured (`docs/fastlua.md`, `benchmarks/RESULTS.md`)
- [x] M0: bounds checking, div-by-zero, and integer-overflow behavior
      closed/documented; negative-length arrays still open
- [x] M2: fastlua-level optimizer passes (opt_level=speed, constant folding,
      bounds-check elimination, inlining) - shipped and measured, honest flat
      result explained
- [x] M3: structs + escape analysis + scalar replacement - shipped and
      IR-verified; real (non-leaked) array allocation deferred to M4
- [ ] M4: generational GC
- [ ] M5: gradual typing + boxed dynamic-value fallback
- [ ] M6: bytecode tier 0 + baseline/optimizing JIT tiers + deopt + OSR + inline
      caches
- [ ] M7: SIMD, PGO, AOT binaries, FFI, profiling tools, full benchmark suite
