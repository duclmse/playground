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
      (straight-line constant/arithmetic/branch/return code, **and
      `while`-loop-shaped control flow via backward `Jump`/`JumpIfFalse`/
      `JumpIfTrue` - corrected from this note's earlier, now-inaccurate claim
      that no loops are modeled at all; see item 4's note below for how this
      was reconciled** - no tables, calls, `for` loops, allocation, or
      iteration instructions are modeled by `sol_ir` yet, so none of those
      can be specialized by this pass either).
- [ ] Inline stable dynamic and typed calls.
- [x] Enter optimized loops with OSR and exit through precise side exits.
      See the item-4 note below.
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

**Item 4 note (OSR into the optimizing tier):** `sol_ir::lift_proto` already
fully lifted `while`-loop-shaped control flow before this item started - the
item-3 note above claimed otherwise, but `sol_ir.rs`'s own test suite
(`while_loop_backedge_becomes_a_phi`) already proved loops with arithmetic
bodies fully lift, and `opt_lower::is_eligible` never excluded them either.
So the real gap this item closes is not instruction coverage, it's *entry
timing*: an `opt_lower`-eligible `Proto` containing a loop already compiled
and ran correctly once promoted to `NativeStatus::Optimized`, but only ever
via a fresh whole-function activation (`pc == 0`, `run_native`'s only entry
gate, `dispatch.rs`) - never by jumping into the already-compiled loop body
mid-iteration from an interpreter that has already been running that same
loop for a while.

What shipped: `opt_lower::lower_proto` (`opt_lower.rs`) was split into a
shared `lower_proto_from(..., entry_pc)` plus two thin callers -
`lower_proto` (unchanged behavior, `entry_pc = 0`) and the new
`lower_osr_entry` (`entry_pc` = a loop-header bytecode pc) - since every
block in that lowering was already keyed 1:1 by bytecode pc, an OSR entry is
just a different jump target out of the function's `entry` block, reusing
every guard/safepoint/deopt wire-up unchanged. `Proto` gained two new side
tables, parallel to `call_count`/`optimize_count`: `osr_counts` (per-loop-
header backward-branch counters) and `osr_entries` (compiled OSR pointers,
cached per header pc). The interpreter's three backward-branch sites
(`Instr::Jump`/`JumpIfFalse`/`JumpIfTrue`, `dispatch/bytecode.rs`) call the
new `try_osr_backedge` whenever the branch target is `<=` the branch's own
pc (i.e. a loop header, by the ordinary definition of a back edge); once that
header's counter first exactly equals `SOL_LUA_OSR_THRESHOLD` (default 100 -
deliberately independent of, and much lower than, `SOL_LUA_PROMOTE_THRESHOLD`/
`SOL_LUA_OPTIMIZE_THRESHOLD`, mirroring the typed tier's own
`DEFAULT_OSR_THRESHOLD` vs. promote-threshold gap in `tier.rs` - a loop
iterating inside one activation needs its own, faster-firing signal,
independent of how many times the function itself has been called), an OSR
entry compiles and native code takes over mid-loop, handed the frame's
entire already-synced register array (the same precondition `run_native`
already relies on for ordinary native entry - every register the loop body
reads has already been written by the interpreter by the time a backward
branch runs). This is deliberately *not* a one-way commit the way the typed
tier's own `on_loop_backedge` (`interp.rs`) is: any stub/guard failure inside
the OSR'd code returns `outcome == 0`, and `try_osr_backedge` resumes
ordinary interpretation at the written-back `out_pc` - the exact same
deopt mechanism `run_native`'s own `outcome == 0` branch already uses, not a
new one. No new `NativeStatus` variant was needed: OSR is orthogonal to a
`Proto`'s own promote/optimize tiering state, and in practice only ever
attempted on a `Proto` that's already `opt_lower::is_eligible` regardless of
whether it has itself been promoted.

Scope honestly not covered: `for`-loop-shaped loops (`ForPrep`/`ForLoop`)
never reach OSR, for the same reason they never reach `opt_lower` at all
(`sol_ir::lift_proto` doesn't model those opcodes). `sol_ir::find_loops`'s
own `Loop`/natural-loop-detection machinery (header/latch/preheader) was not
needed here - a loop header is already directly recoverable as "the target
of any backward branch," with no need to map through `sol_ir`'s own
`BlockId`s - so this item leaves `find_loops` exactly as items 1-3 left it,
unused outside its own tests. No contrived test exercises the `Deopt` path
specifically: within this bounded eligible subset (proof-specialized
arithmetic plus the generic `dynjit_binary` stub's own tag-check fallback for
everything else), nothing speculative can go wrong for a well-typed numeric
loop - the same "nothing to falsify yet" situation the item-3 note already
describes for guards in general - so the wiring is exercised and correct
(`try_osr_backedge` reuses `run_native`'s exact encode/decode/outcome
handling, already covered), but an actual runtime misspeculation triggering
it has no constructed repro yet.

Differential coverage: `lua55_dynamic_runtime_jit.rs`'s
`a_hot_while_loop_is_entered_via_osr_mid_activation_with_no_behavior_change`
and its `_survives_gc_stress_mode` counterpart, both against
`dynjit_osr_while_sum.lua` (a `while`-loop summing 1..2000) - `SOL_LUA_OSR_
THRESHOLD` is forced low while `SOL_LUA_PROMOTE_THRESHOLD`/`SOL_LUA_OPTIMIZE_
THRESHOLD` are left at their defaults, so the test asserts the `Proto` never
goes through `try_promote`/`try_optimize` at all and still reaches native
code purely through a mid-activation OSR entry, with byte-identical output
against the fully interpreted baseline.

Benchmark (measured, not adopted as a general claim): release-mode,
`loop_sum(n)` - the exact shape above - summing 1..200,000,000 in a single
call, so unlike item 3's own benchmark the *driving loop itself* is what's
hot, never a called function. With OSR disabled (`SOL_LUA_OSR_THRESHOLD`
forced absurdly high) the loop runs fully interpreted: **28.57s**. With OSR
left at its default threshold (100): **10.59s** - a real, reproducible
**~2.7x** speedup (confirmed stable across repeated runs), with byte-
identical output (`20000000100000000` both ways). This is the first U10
specialization item with a measured win on a realistic workload, unlike
item 3's own "no measurable difference" result on a call-bound micro-
benchmark - directly validating the item-3 note's own prediction that
"items 4-8 ... are the parts of the U10 plan this specialization is actually
building toward paying off under."

See the [historical U10 ledger](../unified-sol-runtime-plan.md#u10--optimizing-ssa-jit-osr-and-deoptimization).
