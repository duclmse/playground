# Memory management

> Status: a conservative-root mark/sweep collector is implemented, with a real
> generational split, write barriers, remembered sets, and precise per-object
> pointer layouts for typed allocations. Precise Cranelift stack roots and
> incremental/interruptible major GC remain planned.

**Purpose**: provide a real collector for allocations that cannot be removed by
escape analysis and scalar replacement instead of leaking structs or arrays
that genuinely escape and outlive their creating function.

**Prerequisite**: real, non-leaked heap allocation for escaping values.

**Scope call made during implementation**: the original checklist below sketched
a _generational, moving_ collector with precise stack maps (Cranelift
safepoints) and write-barrier-backed remembered sets - the `§14`-accurate
design. That is a substantial, multi-stage undertaking in its own right (the
Cranelift stack-map/safepoint infrastructure it needs is the same machinery
wasmtime's own GC support is built on, which took a long time to land there) and
isn't required to satisfy the immediate need: reclaiming memory that escaping
allocations would otherwise leak forever. What's implemented instead - a **conservative
(stack-scanning), single-generation mark-sweep collector over a chunked bump
arena** - is a legitimate, production-precedented simplification (the
root-scanning approach is the Boehm-Demers-Weiser design; the chunked-arena
allocator is closer in spirit to a region-based collector than a true
generational one), not a shortcut: see `crates/sol/src/gc.rs`'s module doc
comment for the full reasoning. True generations, promotion, and write barriers
were deferred follow-ups at the time this was first written; they have since
been implemented (chunk-granularity promotion, `sol_gc_write_barrier`,
remembered-set-scanned minor collections - see the checklist below for what
actually landed and `gc.rs`'s module doc comment for the design). Precise
(Cranelift-safepoint-backed) collection and incremental/interruptible major GC
remain deferred follow-ups - each checklist item below is marked with what
actually happened.

**A pre-existing array-atomicity bug, found and fixed alongside the
generational work**: `Array<T>`'s data buffer was always allocated via
`sol_gc_alloc_atomic` (never traced), which is correct when `T` is a scalar
(`i64`/`f64`) but silently wrong when `T` is itself pointer-bearing (a struct,
string, another array, a map, or `any`) - the collector would never scan into
such an array's elements, and they could be reclaimed while still reachable
only through it. The only surface syntax that can construct a pointer-element
array is an `Array<T>` table literal (`typeck.rs`'s `ArrayLiteral`; the
`new_array_i64`/`new_array_f64` builtins are hardcoded to scalar element
types, so they were never affected). Fixed by adding a parallel
`sol_new_array_ptr`/`Op::NewArrayPtr` path (traced `sol_gc_alloc`, manually
zeroed to preserve the language's zero-read guarantee for unwritten elements)
selected via `Type::is_gc_pointer()` at the `ArrayLiteral` codegen/bytecode
sites in `codegen.rs`/`bccompile.rs`. Regression coverage:
`tests/fixtures/gc_array_of_structs.sol`.

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

Both are documented in `gc.rs` itself and the collector section of
`benchmarks/RESULTS.md`,
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
      unlock: `benchmarks/     gc_alloc.sol` went from 123ms (7.9× slower than
      LuaJIT) to 15.7ms (roughly on par with LuaJIT, sometimes faster) once this
      landed.
- [x] **Minor collection** → became _the_ collection (no young/old split):
      **conservative stack scanning**, not stack maps - chosen because precise
      stack maps need Cranelift safepoint support this project doesn't have yet,
      while conservative scanning is a well-understood, safe (if imprecise)
      standard technique that unblocks collection immediately. False
      retention (a stale stack word that happens to match a live block's
      address, or a chunk kept alive in its entirety by one surviving object -
      see `gc.rs`'s doc comment on chunk-level reclaim granularity) is the
      accepted tradeoff.
- [x] **Promotion + old generation**: implemented, at chunk granularity - each
      `Chunk` carries a `Generation::{Young, Old}` tag (`gc.rs`). New
      allocations only ever land in a `Young` chunk. A `Young` chunk found
      reachable at the end of *either* a minor or major collection is
      promoted to `Old` **in place** (relabeled, never copied/compacted -
      conservative root-finding can't safely move an object it might not have
      found every reference to). The fragmentation trade-off this checklist
      item originally flagged is unchanged and, if anything, sharper at chunk
      granularity: a single long-lived survivor still keeps its *whole*
      chunk's dead space alive - now for the chunk's entire remaining
      lifetime as `Old`, not just until the next collection.
- [x] **Remembered sets + write barriers**: implemented. `sol_gc_write_barrier`
      (called from `codegen.rs`'s `AssignField`/non-map `AssignIndex` and
      `interp.rs`'s `Op::SetField`/`Op::SetIndex`) sets an `Old` chunk's
      `dirty` bit whenever a store might create a new `Old -> Young` pointer
      edge - a safe over-approximation, never cleared by the barrier itself.
      A minor collection (`collect_minor`) rescans only `Young` chunks plus
      every `dirty` `Old` chunk's live blocks (the remembered set), never the
      full heap. Only a full major collection (`collect_heap`) may ever
      *clear* a `dirty` bit, by re-deriving the true `Old -> Young` edge set
      from its own complete trace - a minor collection doesn't re-verify an
      `Old` chunk's own reachability each cycle, so it can't safely tell
      whether a previously-recorded edge is now stale; clearing early there
      could make a live `Young` object invisible to the next minor
      collection (a real use-after-free). See `gc.rs`'s module doc comment
      and `sol_gc_write_barrier`'s own comment for the full reasoning.
      Regression coverage: `tests/fixtures/gc_struct_field_write_barrier.sol`
      (a struct field reassigned to a fresh `Young` array only after the
      struct itself was promoted to `Old` - the write barrier is the only
      thing keeping that array visible to a later minor collection),
      `tests/fixtures/gc_array_of_structs.sol` (a related array-atomicity fix
      this work required - see below), and
      `tests/fixtures/gc_generational_stress.sol` (a combined stress run,
      asserted via `SOL_GC_DEBUG` to actually exercise both a minor *and* at
      least one major collection, not just one or the other).
- [ ] **Incremental major GC**: still not implemented - every major collection
      (`collect_heap`) remains a full stop-the-world mark-sweep. Explicitly
      out of scope for this pass: incremental/interruptible collection is a
      separate, substantial undertaking (safely pausing mid-trace needs a
      tricolor invariant maintained across barrier-protected mutation, not
      just the generational split done here) and deserves its own dedicated
      design and test pass rather than being folded in as an afterthought.
      Revisit once a real program's pause times are actually measured and
      shown to matter.

**Honest benchmark note on the generational rewrite**: `benchmarks/gc_alloc.sol`
and `benchmarks/objects.sol` - the two workloads this rewrite was most likely
to move - were re-measured before/after (`hyperfine`, 40+ runs, release build).
Neither showed a decisive change: `gc_alloc.sol` stayed within noise of its
pre-generational number (~13ms vs. LuaJIT's ~12ms, statistically a wash both
before and after), and `objects.sol` stayed a consistent, honest LuaJIT win at
roughly the same margin as before (~16ms vs. LuaJIT's ~10ms, versus the
previously-documented 19.0ms/13.0ms - the same ~0.6-0.7x ratio, not a closed
gap). This tracks with the design: these benchmarks are short single-shot
processes with a modest live-object count, so a full stop-the-world trace was
already cheap enough that skipping the old generation's rescan doesn't move
the needle. The generational split is a real, tested correctness feature (an
`Old` struct's field can now safely point at a `Young` object without forcing
a full trace to notice), not a benchmark-driven one - a workload with a much
larger long-lived heap relative to its garbage-generation rate would be needed
to see the throughput case for it. Reported honestly rather than claimed as a
speed win it isn't, per this document's own established practice (see the
`objects.sol`/`gc_alloc.sol` "honest non-win" precedent in
`benchmarks/RESULTS.md`).
- [x] Retrofit the array runtime (`runtime.rs`) and struct allocation onto
      this allocator, removing the `Box::leak`/`mem::forget` calls called out as
      a known, documented limitation of the initial runtime. Done -
      `sol_new_array_i64/f64` and `sol_alloc` all route through
      `gc::sol_gc_alloc`/`sol_gc_alloc_atomic` (see below).
- [x] **Atomic (pointer-free) allocations** - not in the original checklist,
      added after benchmarking exposed a real cost the checklist didn't
      anticipate: `gc.rs` gained `sol_gc_alloc_atomic`, used for an array's
      data buffer (its elements are always plain `i64`/`f64` scalars, never GC
      pointers). The tracer marks such a block reachable when found but never
      reads its contents - skips an O(payload size) conservative scan that a
      large array made measurably expensive (see the benchmark note below).
      This is also a minor correctness improvement, not just speed: a scalar
      value that happened to numerically match another live block's address
      could previously cause spurious retention; an atomic block can't.
- [x] **Typed pointer layouts**: U5 adds `sol_gc_alloc_layout(size, mask)`.
      Native and typed-bytecode lowering emit one bit per managed pointer slot
      for records and `any` boxes; array/map headers describe only their buffer
      pointers. Minor tracing, major tracing, and remembered-set rederivation
      all honor the same descriptor. `u64::MAX` remains an explicit
      conservative fallback for legacy/dynamic blocks and layouts with a
      pointer beyond the compact 64-slot mask. Stack roots are still scanned
      conservatively; “precise” here describes heap object contents only.
- [x] Benchmark: `benchmarks/gc_alloc.{lua,fl}` (2,000,000 short-lived 2-field
      allocations) - see `benchmarks/RESULTS.md`'s collector section for the full
      story, in three parts: sol initially **lost to LuaJIT** on this
      workload (7.9× slower - hashing every allocation and a full heap walk on
      every collection), then **closed the gap to roughly parity** after the
      bump-arena rewrite above. That rewrite then exposed a *second*, different
      cost on `table_array` (a 32 MB single allocation): 1.26× slower than
      LuaJIT, versus 1.30× beating it pre-GC - traced via `SOL_GC_DEBUG` to
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
      bulk-zeroing a whole chunk on every reuse, since `sol_gc_alloc`
      (struct payloads, array headers) never needed it -
      `typeck.rs`/`runtime.rs` already guarantee every byte is overwritten
      immediately; only `sol_gc_alloc_atomic` (array data, a real
      language-level zero guarantee) still zeroes. A genuine trade-off,
      not a free win - couples this file to that initialization
      invariant, documented in `gc.rs` and tested by a dedicated
      regression fixture. Result, confirmed via a controlled A/B and
      10-round aggregate: sol wins **10/10 rounds** on both median
      wall-clock and user CPU time, a clear, non-overlapping gap - not
      noise. See `benchmarks/RESULTS.md`'s collector section for the full
      before/after/before/after story.

**Files**: `crates/sol/src/gc.rs` (single file, not the
`gc/{arena,collect,roots,barrier}.rs` module tree originally sketched -
right-sized for what this collector actually needs; revisit the split if/when
true generations land).
