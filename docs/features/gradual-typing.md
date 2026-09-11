# Gradual typing

> Status: explicit `any` values and the documented narrowing/value families are
> implemented; remaining dynamic operations are listed below.

**Purpose**: provide `any`-typed values and a boxed fallback path for genuinely dynamic
code (§3, §4), without forcing every program to pay for it - most code stays
fully typed/unboxed exactly as today.

**Prerequisite**: a dynamic `Value` needs a real GC because it is a boxed,
heap-referencing type.

**Scope**: the initial implementation supported only `i64`/`f64`/`bool`.
Current `any` boxes also cover nil, strings, arrays, records, maps, and
stateless functions while preserving reference identity. Capturing typed
closures and Lua dynamic tables retain their separate runtime representations.
Direct dynamic arithmetic, comparison, truth, negation, and concatenation are
implemented; indexing, calls, and fields require narrowing first.

- [x] **Strict vs. gradual mode** (§3): decided in favor of an explicit `any`
      type annotation (`function foo(x: any): any`), not "no annotation implies
      gradual" - matches faster_lua.md §3's own example syntax exactly, and
      keeps the existing rule ("every function must declare a return type")
      unchanged rather than adding a second, implicit way to opt in.
- [x] **Boxed representation** (§4) for `any` - `value.rs`: a tagged
      `{ tag: i64, payload: i64 }` block supporting scalar and documented
      reference families. `typeck.rs`'s `coerce` enforces which values may
      cross the boundary. Allocated through the *same* runtime path as struct
      literals (`sol_alloc`, self-initializing - see `gc.rs`'s zero-skip
      work) since both fields are always written immediately - no new
      allocator entry point needed at all.
- [x] **Runtime type checks** at typed/`any` boundaries - both directions
      implemented in `typeck.rs`'s `coerce` (the single existing choke point
      every assignment/call-argument/return already flows through, so this
      is where `i64 -> f64` widening already lived too):
      concrete-type-flows-into-`any` boxes (`TExprKind::Box`, always
      succeeds - the source type is always known statically); `any`-flows-
      into-concrete-type unboxes (`TExprKind::Unbox`, checked at *runtime*
      since the compiler can't know an `any` value's real type) -
      `codegen.rs` compiles the check to the exact same `icmp` + `trapnz`
      shape the array bounds check already uses (a real CPU trap on
      mismatch, not a Rust panic across the FFI boundary the box's
      allocation call crosses).
- [x] **Type-check elision at strict/strict boundaries**: guaranteed
      *structurally*, not just tested - `coerce` only ever emits
      `Box`/`Unbox` when `Type::Any` is one of the two types involved, so a
      program that never writes `any` anywhere can never produce one. Also
      confirmed via `SOL_DUMP_CLIF`: a strict call chain's IR is
      byte-for-byte identical whether or not gradual-typing support exists at
      all (the full existing benchmark suite's numbers were unchanged - see
      `benchmarks/RESULTS.md`).
- [x] Benchmark: `benchmarks/any_strict.sol` / `any_dynamic.sol` (identical
      5,000,000-iteration workload, the second with an `any`-typed function
      boundary in the hot path) - see `benchmarks/RESULTS.md`'s gradual-typing section
      for the honest number: the dynamic path is **~7-10× slower**, almost
      entirely the cost of one real heap allocation (the box) per call. This
      is the expected, correct cost of opting into gradual typing - not a
      regression to fix, the whole point of "strict by default, gradual
      where you ask for it."

**Remaining work**: capturing closure boxes, dynamic indexing/calls/field
access without prior narrowing, and the complete typed/dynamic module bridge.

**Files**: `types.rs` (`Type::Any`, `TExprKind::Box`/`Unbox`), `typeck.rs`
(`lower_type`, `coerce`), new `crates/sol/src/value.rs` (tag constants),
`codegen.rs` (`clif_type`, `translate_expr`'s box/unbox codegen, reusing the
existing struct-literal allocation path), `ast.rs`/`parser.rs` (`any` in type
position only, not a reserved identifier elsewhere).
