# U10 — Optimizing SSA JIT, OSR, and deoptimization

**Status:** in progress

**Purpose:** reach LuaJIT-class untyped-Lua performance using proof and profiles.

- [x] Lift bytecode to shared SSA with proof provenance.
      `crate/sol/src/sol_ir.rs`'s `lift_proto`/`propagate_proofs` - see the
      item-3 note below for the honest scope of what `lift_proto` actually
      models.
- [ ] Insert, hoist, and fuse guards with full deoptimization snapshots.
      `Inst::Guard`/`hoist_loop_invariant_guards`/`fuse_redundant_guards`
      exist as scaffolding (`sol_ir.rs`) but `lift_proto` never constructs a
      real `Inst::Guard` - everything proven so far is unconditionally true
      dataflow, not a falsifiable runtime speculation, so there is nothing
      yet for these passes to operate on outside their own hand-built test
      graphs. Real guard insertion needs a source of genuinely speculative
      (not just provably-true) facts - e.g. an inline-cache hit-rate profile
      - which item 3 below deliberately did not add.
- [ ] Specialize arithmetic, tables, calls, loops, allocation, and iteration.
      **Partially built, narrower than this bullet's text - see the item-3
      note below.** Only arithmetic/unary-op specialization shipped, and
      only for the bounded subset `sol_ir::lift_proto` fully lifts
      (straight-line constant/arithmetic/branch/return code - no tables,
      calls, loops, allocation, or iteration instructions are modeled by
      `sol_ir` yet, so none of those can be specialized by this pass either).
- [ ] Inline stable dynamic and typed calls.
- [ ] Enter optimized loops with OSR and exit through precise side exits.
- [ ] Reconstruct inlined frames for errors, coroutines, profiling, and debug.
- [ ] Bound recompilation with failure counters and widening.

**Exit gate:** dynamic parity passes before final performance claims.

**Item 3 note (proof-specialized arithmetic lowering):** built as a second
Cranelift lowering, `crate/sol/src/lua_runtime/dynjit/opt_lower.rs`, layered
on top of U9's baseline tier (`dynjit/lower.rs`) rather than replacing it -
both share the exact same `abi::NativeFn` ABI, flat per-register native
array, and per-bytecode-pc Cranelift block structure, so `run_native`
(`dispatch.rs`) and the `use_native` gate (`drive_result`, `dispatch.rs`)
needed only to also accept the new `NativeStatus::Optimized(ptr)` variant
alongside `NativeStatus::Native(ptr)` - no other dispatch logic changed.

Scope is deliberately a strict *subset* of `lower::is_eligible`'s own
whitelist, not an extension of it: `opt_lower::is_eligible` requires
`sol_ir::lift_proto(proto).fully_lifted`, and `lift_proto` only models
`LoadConst`(non-string)/`LoadNil`/`LoadBool`/`Move`/uncaptured
`NewLocal`/`DetachCell`, `Not`/`Neg`/`BitNot`, `Binary`/`IntegerBinary`,
`Jump`/`JumpIfFalse`/`JumpIfTrue`, and a `Return` of zero or one value - no
field/global/table/upvalue access, no calls, no `for` loops, and (as a
consequence of `NewClosure` falling outside that modeled set) no captured
registers either. A `Proto` eligible for this tier is therefore always also
eligible for the baseline tier, so in practice a hot `Proto` reaches
`Native` first and only later, if it stays hot past a second, higher
threshold (`SOL_LUA_OPTIMIZE_THRESHOLD`, default 2000 vs.
`SOL_LUA_PROMOTE_THRESHOLD`'s default 200), reaches `Optimized`.

The actual specialization: `propagate_proofs`'s forward dataflow is a
*conservative* single pass (its own doc comment) - a loop-carried register
reassigned inside the loop body gets `Proof::None` at its phi (the
back-edge incoming isn't known yet in forward order), so today this only
ever fires for straight-line, constant-seeded arithmetic, not loop
accumulators. Where it does fire - both operands of a `Binary`/
`IntegerBinary` `Add`/`Sub`/`Mul`, or `Neg`/`BitNot`'s one operand, proven
`Integer` (or `Float` for `Neg`) - `opt_lower` skips the runtime tag-check
branch `lower.rs`'s own lowering of those same instructions always emits,
going straight to the arithmetic, since the check can never fail. This is a
pure optimization over already-true facts, not speculation: there is no
corresponding guard or deopt path for the skipped check, by construction.
Every other operator (comparisons, bitwise ops, `Div`/`Pow`, `Concat`,
`And`/`Or`) and every unproven case still falls back to `dynjit_binary`
(the same stub `lower.rs` calls), since a proven *result* tag for those
(e.g. comparisons always yielding `Boolean`) says nothing about whether the
primitive, non-metamethod path even applies.

Differential coverage: `lua55_dynamic_runtime_jit.rs`'s
`constant_driven_arithmetic_is_optimized_to_native_code_with_no_behavior_change`
and its `_survives_gc_stress_mode` counterpart, both asserting byte-identical
output against the fully interpreted run for a `Proto` driven through
`try_promote` then `try_optimize` on its very first activation.

Benchmark (measured, not adopted as a general claim): a manual release-mode
comparison - a driving `for` loop (20,000,000 iterations, itself interpreted;
the loop body is not eligible for either native tier) calling a small
six-operation straight-line arithmetic `compute(a, b)` each iteration, once
with `SOL_LUA_OPTIMIZE_THRESHOLD` effectively disabled (`compute` stays at
the U9 baseline native tier) and once with it set low enough that `compute`
reaches `Optimized` almost immediately - showed **no measurable difference**
(22.15s vs. 22.21s wall time, within run-to-run noise). The per-call cost
this workload actually pays - argument marshaling, the native-call
trampoline, and the per-activation counter bumps and safepoint check in
`dispatch.rs` - dwarfs the handful of tag-check branches this tier elides
inside `compute`'s own small body. Skipping those branches is real and
correct, but on today's scope (no inlining, no loop specialization) it is
not yet a workload where that matters; items 4-8 (inlining stable calls,
OSR into hot loops) are the parts of the U10 plan this specialization is
actually building toward paying off under.

See the [historical U10 ledger](../unified-sol-runtime-plan.md#u10--optimizing-ssa-jit-osr-and-deoptimization).
