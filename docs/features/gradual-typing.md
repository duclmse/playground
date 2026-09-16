# Gradual typing

> Status: explicit contracts, non-rejecting annotation-free inference, and U5
> typed layout specialization are implemented.

**Purpose**: provide checked `any` contracts plus a sound inference path for
ordinary Lua, without making uncertainty a source error or forcing proven code
to pay for dynamic dispatch.

**Prerequisite**: a dynamic `Value` needs a real GC because it is a boxed,
heap-referencing type.

**Scope**: the initial implementation supported only `i64`/`f64`/`bool`.
Current `any` boxes also cover nil, strings, arrays, records, maps, and
stateless functions while preserving reference identity. Capturing typed
closures and Lua dynamic tables retain their separate runtime representations.
Direct dynamic arithmetic, comparison, truth, negation, and concatenation are
implemented; indexing, calls, and fields require narrowing first.

- [x] **Type policies**: `off`, `infer`, and `strict` are available through
      `sol --type-policy`. They affect proofs and diagnostics, never parsing or
      runtime semantics. Explicit annotations—including explicit `any`—remain
      contracts; omitted annotations remain valid and may be inferred.
- [x] **Flow inference**: a bounded union lattice widens after four members and
      supports truthy nil elimination, dominated type tests, branch joins,
      loop headers, return fixed points, and nonescaping local-call signatures.
- [x] **Object/effect inference**: local table shapes carry escape, alias, and
      mutation facts. Function summaries track global/table access,
      allocation, calls, raises, yields, and unknown effects.
- [x] **Optimization explanations**: `sol run --type-policy infer
      --explain-types file.lua` reports each proven check removal and each
      operation that must remain dynamic.
- [x] **Boxed representation** (§4) for `any` - `value.rs`: a tagged
      `{ tag: i64, payload: i64 }` block supporting scalar and documented
      reference families. `typeck.rs`'s `coerce` enforces which values may
      cross the boundary. Allocated through the *same* runtime path as struct
      literals (`sol_alloc_layout`, self-initializing - see `gc.rs`'s zero-skip
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

At the unified semantic ABI, dynamic values use `sol_core::Value`; proven scalar
arguments/results use `BoundaryValue::Unboxed` and therefore allocate no
two-word `any` block. The legacy typed-only `Type::Any` representation still
uses a two-word allocation when a value must live in that IR. U5 gives that
block a precise payload layout and preserves the original identity of
reference payloads.

Dynamic `.lua` module contracts are never inferred: only functions with an
explicit result and explicit annotations for every parameter are importable by
typed modules. Their implementation remains generic, with checked scalar
adapters at the boundary. Capturing closures and tables that remain dynamically
observable keep the generic representation; nonescaping typed captures and
records are lambda-lifted/scalar-replaced.

**Remaining work (U6+)**: broaden non-scalar dynamic boundary conversions as
the U2 object migration completes, and lower additional profile-backed facts
in later optimization tiers without changing the explicit contract rules.

**Files**: `typeck/inference.rs` (flow lattice and reports), `types.rs`
(`Type::Any`, `TExprKind::Box`/`Unbox`), `typeck.rs`
(`lower_type`, `coerce`), new `crates/sol/src/value.rs` (tag constants),
`codegen.rs` (`clif_type`, `translate_expr`'s box/unbox codegen, reusing the
existing struct-literal allocation path), `ast.rs`/`parser.rs` (`any` in type
position only, not a reserved identifier elsewhere).
