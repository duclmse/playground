# U10 — Optimizing SSA JIT, OSR, and deoptimization

**Status:** in progress

**Purpose:** reach LuaJIT-class untyped-Lua performance using proof and profiles.

- [x] Lift bytecode to shared SSA with proof provenance.
      `crate/sol/src/sol_ir.rs`'s `lift_proto`/`propagate_proofs` - see the
      item-3 note below for the honest scope of what `lift_proto` actually
      models.
- [x] Insert, hoist, and fuse guards with full deoptimization snapshots.
      **Real for one narrow case, not yet general - see the item-7 note
      below.** `lift_proto` now constructs a genuine, falsifiable
      `Inst::Guard` (`GuardFact::ClosureIdentity`) for an inlined
      monomorphic call site, lowered to a real Cranelift compare that
      branches to the shared deopt block (full snapshot/resume-pc/flush-
      then-interpret) on mismatch - the first falsifiable guard anywhere in
      this milestone, adversarially confirmed to actually fire. Arithmetic
      tag guards remain unconditionally-true dataflow with nothing to
      falsify (item 3), and `hoist_loop_invariant_guards`/
      `fuse_redundant_guards` are exercised against this real guard's own
      hand-built-graph shape but not yet against a real multi-guard
      program, so hoisting/fusion itself is still proven only on synthetic
      graphs, not live code.
- [ ] Specialize arithmetic, tables, calls, loops, allocation, and iteration.
      **Partially built, narrower than this bullet's text - see the item-3
      note below.** Arithmetic/unary-op specialization shipped for the
      bounded subset `sol_ir::lift_proto` fully lifts (straight-line
      constant/arithmetic/branch/return code, **and `while`-loop-shaped
      control flow via backward `Jump`/`JumpIfFalse`/`JumpIfTrue` -
      corrected from this note's earlier, now-inaccurate claim that no
      loops are modeled at all; see item 4's note below for how this was
      reconciled**), plus a narrow slice of call specialization (item 7's
      note below: a monomorphic, single-block, constant/integer-arithmetic-
      only callee inlines). Tables, allocation, iteration, and every wider
      call shape remain entirely unmodeled by `sol_ir`, so none of those can
      be specialized by this pass.
- [x] Inline stable dynamic and typed calls.
      **Partially built, far narrower than this bullet's text - see the
      item-6 and item-7 notes below.** A monomorphic call site whose callee
      is single-block, non-branching, fixed-small-arity, no-upvalue, and
      constant/integer-arithmetic-only now really splices and inlines,
      gated by a real speculative `Inst::Guard` that deopts on a mismatch -
      the first falsifiable guard anywhere in this milestone. Everything
      wider (field/global/table access, closures, multi-register results,
      a callee that itself calls, and the typed-ABI half of this bullet) is
      still unsupported and falls back to an ordinary call.
- [x] Enter optimized loops with OSR and exit through precise side exits.
      See the item-4 note below.
- [ ] Reconstruct inlined frames for errors, coroutines, profiling, and debug.
      **Bookkeeping only - no live caller yet, and today's inlining shape has
      no real target for the reconstruction itself. See the item-7b note
      below.**
- [ ] Bound recompilation with failure counters and widening.
      No real falsifiable `Inst::Guard` exists yet (see the unchecked guard
      bullet above), so there is no tag-guard *widening* to build - widening
      only makes sense once a guard can actually narrow a type and then need
      re-widening on repeated failure. **What shipped instead - see the
      item-5 note below** - permanently bounds the three compile attempts
      that do exist today (`try_promote`/`try_optimize`/`try_osr_backedge`),
      closing a genuine, if currently astronomically unlikely, unbounded-
      retry gap in their `u32` activation counters.

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

**Item 5 note (bounding recompilation attempts, not guard widening):** the
plan's literal framing - "per-`GuardId` failure counter" plus "tag-guard
widening" - has no real target yet: `sol_ir.rs` builds `GuardId`/`GuardFact`/
`DeoptSnapshot`/`Inst::Guard` as scaffolding (item 2), but `lift_proto` never
constructs a real one, and `opt_lower.rs` has its own doc comment stating
guards are deliberately not wired up. Items 3-4's specialization is all over
*unconditionally true* dataflow (proven facts), not speculation that could
ever fail at runtime - there is nothing a failure counter could count yet.

So this item binds "bounded recompilation" to the real failure signal that
does exist: the per-`Proto` (`call_count`, `optimize_count`) and per-loop-
header (`osr_counts`) `u32` activation counters that gate `try_promote`/
`try_optimize`/`try_osr_backedge`. Before this item, each gate was a bare
`count == threshold` check against a `wrapping_add(1)`-driven counter that
never resets (`dispatch.rs`, `dispatch/bytecode.rs`). In practice a compile
is attempted "once" - but not as an actual invariant: after
`u32::MAX - threshold + 1` further activations/backedges past the first
attempt, the counter wraps back through 0 and re-equals `threshold`,
re-triggering an attempt that is guaranteed to fail identically forever,
since `Proto::instrs` is immutable and nothing about eligibility can change.
This is exactly the kind of recompilation storm this item's bullet asks to
bound, just found one level down from where the plan expected it - at the
compile-attempt gate itself, rather than inside a (nonexistent) guard.

What shipped: three new permanent-failure flags alongside the existing
counters on `Proto` - `promotion_failed: Cell<bool>`, `optimization_failed:
Cell<bool>`, and `osr_failed: RefCell<HashSet<usize>>` (keyed per loop-header
pc, since OSR already tracks counters per-header)
(`crate/sol/src/lua_bytecode/instr.rs:440-452`, initialized at the crate's
single `Proto` construction site,
`crate/sol/src/lua_bytecode/mod.rs:340-342`). A new pure gate,
`dynjit::should_attempt(count, threshold, already_failed) -> bool`
(`crate/sol/src/lua_runtime/dynjit/mod.rs:111`), replaces the bare `==`
checks: `!already_failed && count == threshold`. `try_promote`
(`dynjit/mod.rs:379`) and `try_optimize` (`dynjit/mod.rs:446`) now check
their own flag on entry and set it in their `Err(_)` arm, alongside the
existing `NativeStatus` reset. `try_osr`'s doc (`dynjit/mod.rs:499`) was
corrected to drop its previous (inaccurate) "only ever called once for the
whole process lifetime" claim, documenting instead that the caller owns the
failure cache; `try_osr_backedge` (`dispatch/bytecode.rs:38`) reads
`proto.osr_failed` into `already_failed`, passes it to `should_attempt`
(`dispatch/bytecode.rs:68`), and inserts the header pc into `osr_failed` on a
`None` result. `dispatch.rs:1015` and `dispatch.rs:1024` wire the same gate
into the promote/optimize call sites.

Differential coverage is a pure unit test, not an integration/benchmark one:
real wraparound needs ~4 billion activations, infeasible through the
subprocess-driven `tests/lua55_dynamic_runtime_jit.rs` harness. Instead,
`dynjit::mod::tests` (`dynjit/mod.rs:549-615`) exercises `should_attempt`
directly with a counter crafted a few steps from `u32::MAX`, stepping it
through wraparound back to the threshold value and asserting the gate stays
closed once `already_failed` is set - the exact pathological case, reproduced
deterministically instead of by brute force - plus two narrower tests
confirming a fresh counter still attempts once at threshold, and a
sub-threshold counter never attempts regardless of failure state. All three
pass, alongside the full existing suite (118 lib tests total, up from 115,
plus `sol-core`'s 37 and the Lua 5.5 manifest check, all green) - this item
changes gating only, with no behavior change on any path that was already
succeeding.

**Item 6 note (call-site identity-guard infrastructure, not inlining):** the
plan's literal framing - splice a callee's lifted body into the caller,
gated by a stable-call-site check - presupposes an `sol_ir` that can
represent a call at all. It cannot yet: `lift_proto` (confirmed by rereading
it in full before starting this item) models exactly the instruction set the
item-3 note already lists, and a bytecode `Instr::Call`/`TailCall`/`TForCall`
falls through to `Inst::Unsupported` like any other unmodeled opcode,
clearing `Function::fully_lifted` and (via `opt_lower::is_eligible`)
excluding the whole containing `Proto` from this tier - exactly like
`dynjit/lower.rs`'s own baseline tier, which lowers `Call`/`TailCall`/
`TForCall` only by unconditionally deopting (`lower.rs:549`, restated in
`stubs.rs`'s own module doc at the top of its Work-item-4-stubs section).

There is also a harder reason real splicing is further out than one item:
Lua bytecode calls are never statically resolved. `Instr::Call` invokes
whatever closure value is sitting in a register at runtime - there is no
"direct call to a known `Proto`" shape in the bytecode for `lift_proto` to
lift in the first place. The only candidate "stable call site" signal is
runtime profile data: U8's per-pc `Proto::call_cache: Vec<BoundedCache
<CallCacheEntry>>` (`lua_bytecode/instr.rs`). Trusting that profile inside
compiled code is a genuinely speculative runtime check - a real `Inst::Guard`
checking closure identity, not the unconditionally-true dataflow
`propagate_proofs` already proves - and no real `Inst::Guard` has ever been
constructed from actual bytecode anywhere in this codebase yet (items 1-5 all
left `Inst::Guard` as hand-built-test-graph-only scaffolding; see the
unchecked guard-insertion bullet and the item-5 note above). Building real
splicing in this item would therefore mean building, in one increment: call
lifting, cross-`Proto` block splicing with register renaming, argument/
return-value wiring, *and* the first-ever real speculative guard with a
working deopt-to-interpreter path - several firsts at once, exactly the
"disproportionately large for one item" case this item's own directive
flagged in advance. So, following items 3-5's own precedent of shipping the
honest, bounded thing rather than the plan's literal text: this item built
path 2 - real call-site identity-guard *infrastructure*, tested, with
bytecode-level call lifting and splicing explicitly deferred.

What shipped:

- `GuardFact` (`sol_ir.rs`) gains a second variant,
  `ClosureIdentity(ValueId, ObjectId)` - item 2's own doc comment already
  reserved this shape ("work item 2 reserves the shape" for the
  single-variant enum); this item names it and wires it through the two
  functions `hoist_loop_invariant_guards`/`fuse_redundant_guards` are
  generic over, `guard_fact_value`/`with_guard_fact_value` (`sol_ir.rs`), so
  both existing passes handle it with no variant-specific logic of their
  own - confirmed by two new tests exercising the identical hoist/fuse
  fixtures the `TagIsInteger` tests already use, but with
  `ClosureIdentity`, including a case that confirms two guards on the *same*
  register but *different* identities (e.g. a polymorphic call site
  re-checked after a deopt) survive fusion rather than being incorrectly
  collapsed.
- `BoundedCache<CallCacheEntry>::monomorphic_call_target`
  (`lua_bytecode/instr.rs`) - the call-cache-side half of "is this call site
  stable": returns the cache's one entry only when `len() == 1`. This is
  sound, not just plausible, because of `closure_parts_cached`'s own
  discipline (`lua_runtime/table.rs:110-131`, reread in full to confirm):
  it only ever calls `insert` after a `find` miss against the live guard, so
  a second `insert` only happens when a genuinely different closure identity
  was seen at that call site - and `BoundedCache::insert`'s only removal path
  is a round-robin eviction at `IC_SLOTS` capacity, never on a hit, so
  `len()` can grow but never shrinks back to 1. A call site that was ever
  polymorphic therefore stays correctly classified as non-monomorphic
  permanently, not just at the instant it's checked. (`debug.icstats()`'s own
  existing `write_cache_line` already classifies `len() == 1` as `"mono"`,
  independently corroborating this as the right threshold rather than an
  invented one.) Four new tests cover: `None` before any resolution, `Some`
  after exactly one, `None` after a second distinct identity, and `None`
  staying permanent even after the cache has round-robin-evicted back down
  to `IC_SLOTS` entries.

Explicitly not done, matching path 2's own deferral: no bytecode `Call`/
`TailCall`/`TForCall` is lifted by `sol_ir` yet, nothing calls
`monomorphic_call_target` from real lowering or lifting, no `Inst::Call` IR
node exists, and no callee body is ever spliced anywhere. Nothing in
`opt_lower.rs`/`lower.rs` changed. The typed-ABI half of this bullet (a
stable dynamic call resolving to a typed `.sol` function through the
semantic ABI) is further out still than the dynamic-call case analyzed
above, which is itself not done - no attempt was made at it.

No benchmark: this item added no new code path any `Proto` can reach at
runtime (the new `GuardFact` variant and cache helper are exercised only by
their own unit tests), so there is nothing yet to measure.

Differential coverage: none beyond the unit tests above - no
`lua55_dynamic_runtime_jit.rs` test was added, since nothing user-visible
changed; adding one would only assert that ordinary call execution is
unaffected, which the full existing suite (124 lib tests, up from 118, plus
every integration suite, `sol-core`'s 37, and the Lua 5.5 manifest check, all
green) already covers by construction, since no call-handling code path was
touched.

**Item 7 note (real call splicing: a narrow first inlining slice, extending
item 6):** item 6 built the identity-guard infrastructure and explicitly left
"bytecode-level call lifting and splicing" undone. This item does that
lifting and splicing for a deliberately narrow shape, not the general case the
plan's text describes.

What a call site must look like to be inlined, all checked by
`callee_is_inlinable` (`sol_ir.rs:683`, called from `lift_proto_impl`'s
`Instr::Call` arm, `sol_ir.rs:493`): the call's own `call_cache` slot must
already be monomorphic (`BoundedCache::monomorphic_call_target`,
`lua_bytecode/instr.rs:104`); the resolved callee must take exactly as many
fixed, non-variadic parameters as the call site passes fixed arguments, return
0 or 1 results (matching the call site's own result count), and capture no
upvalues; the callee's own body (lifted via `lift_proto_impl(_, false)` - the
non-call-lifting pass, so a callee can never itself inline a further call,
which is what keeps mutual recursion between two monomorphic call sites from
inlining without bound) must terminate its entry block directly in a `Return`/
`ImplicitReturn` of the right arity (never a `Jump`/`Branch` - a callee whose
own logic branches at all is simply not inlined); and that entry block may
contain only `Const` (non-string) and proof-provably-`Integer` `Add`/`Sub`/
`Mul` `Binary` - not even `Not`/`Neg`/`BitNot`, which `lift_proto_impl` lowers
in general but which this increment's value-shaped Cranelift splicing
(`opt_lower.rs:557`'s `lower_call`) does not yet handle. A consequence of
`lift_proto_impl` always seeding a fresh `Proto`'s block-0 registers with
`Const::Nil` (nothing before this item needed a callee's *own* parameters to
carry a real proof): a plain pass-through parameter used directly as a call
argument can never be proven `Integer`, so it can never be inlined as one -
`propagate_proofs_with_param_seed` (`sol_ir.rs:762`) only substitutes a real
proof for the callee's *own* parameters, not the caller's argument
expressions. Both new fixtures below work around this by building the call
argument from constant arithmetic in the caller (`local x = 5 + 2`) rather
than forwarding a bare parameter.

On success, `lift_proto_impl` emits a real `Inst::Guard` (`fact:
GuardFact::ClosureIdentity(current[base], entry.guard)`, the item-6 variant)
followed by an `Inst::Call{target, param_value_ids, body, result}` carrying
`callee_is_inlinable`'s whole resolved splice plan (`sol_ir.rs:677`'s
`InlinePlan`) - `opt_lower.rs` never re-derives eligibility, it mechanically
translates this plan. `lower_call` (`opt_lower.rs:557`) lowers the guard as a
plain Cranelift compare (`tag == TAG_OBJECT && payload ==
guard.raw()`, `abi.rs:44`'s doc explains why a bare tag/payload compare is
enough: every heap object shares tag `Object`, identity lives in the payload)
branching on failure to the *same* shared `self.deopt_block` every other
`opt_lower.rs` guard already uses (`BlockArg::Value(pcv)`, item 2's existing
snapshot/stamp-resume-pc/flush-then-interpret mechanism, unchanged) - no new
deopt mechanism was introduced. On success it threads the callee's body as
bare Cranelift SSA value pairs (`HashMap<ValueId, (tag, payload)>`), touching
the caller's own flat register array only to read the argument registers once
and write the final result register once.

Narrowed further than the governing task's own suggested increment ("a callee
with NO further calls inside it" was already required; this item additionally
restricts the callee's entire body to constant/integer-arithmetic-only,
single-block, non-branching, no-upvalue, fixed small arity) because that is
the exact subset `sol_ir`'s existing proof/lifting machinery already makes
sound without new work - field/global/table access, closures, and
multi-register-result calls remain `Inst::Unsupported` exactly as item 6 left
them.

Differential coverage (`crate/sol/tests/lua55_dynamic_runtime_jit.rs:561-719`,
fixtures `tests/fixtures/dynjit_inline_call_stable.lua`/
`dynjit_inline_call_deopt.lua`): `a_monomorphic_call_site_is_inlined_to_native_code_with_no_behavior_change`
and its GC-stress twin drive a call site that stays monomorphic across three
activations and assert both byte-identical output against a no-JIT baseline
run *and* the `"'caller' optimized to native code"` JIT_LOG trace (needed
because the baseline native tier already deopts every call unconditionally -
without the trace assertion a test where inlining silently never engaged
would still pass with the same output, a false-confidence risk considered and
rejected). `a_call_site_that_turns_polymorphic_deopts_the_inlined_guard_correctly`
and its GC-stress twin call the same site with `add_one` twice (populating the
cache monomorphic and triggering inlining) then `times_ten` once - a second,
distinct closure of the same arity/shape, forcing the baked-in guard to fail
and deopt back to a real call; `add_one(7) = 8` vs `times_ten(7) = 70` means a
guard that silently never fired would produce the wrong answer (`8` instead of
`70`), not just slower output, so this is a correctness assertion, not only a
behavioral one. All four tests pass, output and traces confirmed by hand
against the built CLI before writing the Rust assertions; adversarially
confirmed non-vacuous by temporarily dropping the identity half of the guard
compare (`let guard_ok = is_object;`) and observing both deopt tests fail with
the wrong `"8\n8\n8\nnil"` output, then reverting. Full suite: 128 lib tests
(up from 124), every integration suite including
`lua55_dynamic_runtime_jit.rs` at 19/19, `sol-core`'s 37, and the Lua 5.5
manifest check, all green, zero new warnings.

Benchmark (`crate/sol/scratch/inline_call_bench.lua`, gitignored scratch, not
committed - a tight loop calling a monomorphic single-arg function 20,000,000
times): run twice against the release CLI with everything else held fixed
(`SOL_LUA_PROMOTE_THRESHOLD=1` both times, so the function is native from its
first activation) and only `SOL_LUA_OPTIMIZE_THRESHOLD` varied -
`999999999` (never reached, so the call site stays on the baseline tier,
which deopts to the interpreter for every single call) vs. `2` (reached on
the call site's second activation, so nearly the whole run executes the
inlined guard-and-splice path with no per-call deopt): 32.98s real vs. 16.73s
real, both producing the identical correct `160000000` total - roughly a 2x
wall-clock improvement on a call-dominated loop, from eliminating the
deopt-to-interpreter round trip on every call once the guard proves stable.

**Item 7b note (inline-site bookkeeping, not frame reconstruction - don't
confuse with the above "Item 7 note", which despite its own label actually
completed item 6's real-inlining scope):** the plan
(`docs/features/unified-sol-runtime-plan.md` is the roadmap; the authoring
plan document's own "7. Error/coroutine/profiler/debugger frame
reconstruction through an inlined region" section is the governing spec here)
asks for reconstructing an interpreter-visible frame chain when an error,
coroutine yield, or debug/profiler read happens from inside an inlined
region, and warns up front that this is "the highest-complexity-per-line item
in the milestone" with "no partial 'ship something, measure, refine'
version."

Before building that reconstruction, this item checked whether today's
inlining shape (the Item 7 note above: constant/integer-arithmetic-only,
single-block, non-branching, no-upvalue callees) has any real instruction
inside an inlined splice that could actually trigger an error, a yield, or be
observed by any existing introspection surface. It does not, for three
independent, structural (not just currently-untested) reasons:

1. **No instruction inside an inlined splice can call anything.**
   `callee_is_inlinable` (`sol_ir.rs:683`) only accepts an entry block of
   `Const` and proof-provably-`Integer` `Add`/`Sub`/`Mul` `Binary`
   instructions - there is categorically no `Instr::Call` in an inlinable
   callee body, so nothing inside a splice can ever call `error()`,
   `assert()`, `debug.getinfo()`, or `coroutine.yield()`. This isn't an
   empirical property of the fixtures tested so far; it's enforced by
   `callee_is_inlinable`'s own whitelist.
2. **The lowered arithmetic cannot trap.** `opt_lower.rs`'s `lower_call`
   splices `Add`/`Sub`/`Mul` as plain Cranelift `iadd`/`isub`/`imul` (not the
   trapping `*_overflow` forms), matching Lua 5.5's own wrapping
   two's-complement integer semantics - so even integer overflow inside a
   spliced binary produces a silent wraparound value, never a trap, never an
   error.
3. **`debug.getinfo` has no tier-awareness to begin with.**
   `NativeFunction::DebugGetinfo` (`lua_runtime/natives_debug.rs`) reads
   frame info directly from the interpreter's own `self.frames`/`Frame::Lua`/
   `frame.header.pc` state; it has no notion of native, optimized, or inlined
   execution at all. U9's own milestone doc already recorded this same
   finding for its own item 7 ("no `debug.getinfo` (or other) surface was
   added to read tier info... none of the dynamic runtime's existing
   introspection exposes tier information today") - rechecked here and still
   true, and it means there is no existing profiler/debug hook that could
   even be made to fire mid-splice without first building that hook, which is
   out of this item's own charter.

Given that, this item built the one thing the plan's own "Inline-site
bookkeeping" bullet (plan §6) asks for as a prerequisite to the real
reconstruction, and nothing more: an `InlineMap` recording, for every
call site a caller has ever spliced inline, the (callee `Proto`, callee
bytecode pc) chain for each of the callee's own real instructions the splice
used.

What was built, all new:

- `lua_bytecode::instr::InlineMapEntry` (`lua_bytecode/instr.rs`): `{ callee:
  Rc<Proto>, value_pcs: HashMap<u32, usize> }` - a caller-side `ValueId`'s raw
  `u32` to the callee bytecode `pc` it represents. Uses only `Rc<Proto>`/
  `u32`/`usize`, not `sol_ir::ValueId`, because `lua_bytecode` has no
  dependency on `sol_ir`/`lua_runtime` (an existing, explicitly documented
  boundary) and this follows the same "side table directly on `Proto`"
  pattern as `call_cache`/`osr_entries`/`native_status` rather than inventing
  a parallel map owned elsewhere.
- `Proto::inline_map: RefCell<HashMap<usize, InlineMapEntry>>` - one entry per
  caller bytecode pc that has ever been spliced inline, populated purely
  during compilation, mirroring `call_cache`'s own lifecycle.
- `sol_ir::Inst::Call::body_pcs: HashMap<ValueId, usize>` (`sol_ir.rs`) -
  threaded through the existing `InlinePlan`/`callee_is_inlinable`
  (`sol_ir.rs:683`), built once by inverting the callee's own already-computed
  `Function::value_at_pc` and keeping only the entries `blocks[0]`'s real
  instruction list actually contains - which naturally excludes the callee's
  own parameter nil-seed `ValueId`s (`lift_proto_impl`'s block-0 seeding never
  populates `value_at_pc` for those, so inverting it already does the
  exclusion for free, nothing extra needed). `opt_lower.rs`'s `lower_call`
  copies this verbatim into the caller's `Proto::inline_map` rather than
  re-deriving it - the one real call site that creates an `InlineMapEntry`.

Tests: `sol_ir.rs`'s
`body_pcs_attributes_the_spliced_binary_to_the_callees_own_bytecode_pc`
independently re-derives the callee's expected bytecode pc (never trusting
the code under test) and confirms the one real `Binary` instruction in a
`local function add(a, b) return a + b end` callee is attributed to its own
pc while both parameter nil-seeds have no entry at all.
`opt_lower.rs`'s new
`compiling_an_inlined_call_site_populates_the_callers_inline_map` drives the
real end-to-end path - `DynJit::optimize` on the same caller/callee shape the
differential integration fixture `dynjit_inline_call_stable.lua` already
proves actually inlines (callee taken as the caller's own parameter, never
defined inline, so the caller's own `Proto` has no `NewClosure` and stays
`fully_lifted`) - and reads back `Proto::inline_map` afterward, confirming
both the exact callee `Proto` identity (`Rc::ptr_eq`) and that both of the
callee's real instructions (`add_one`'s own `1` constant load and its `x + 1`
binary) are attributed to real callee bytecode positions.

**No live caller of `Proto::inline_map` exists yet, deliberately.** Per the
reasoning above, there is currently no error, yield, or debug/profiler read
that can originate from inside today's narrow inlined region, so there is
nothing honest to wire the map up to - matching item 4's own precedent for
its `Deopt` snapshot/resume-pc mechanism ("nothing speculative can go
wrong... so the wiring is exercised and correct... but an actual runtime
misspeculation triggering it has no constructed repro yet"). Building a
synthetic error/yield capability inside an inlined region purely to exercise
this map would be scope creep (table/call/error support) belonging to a much
later item, not this one. The actual frame-reconstruction logic the plan's
item 7 describes remains unbuilt; it has no real target until a future item
widens `callee_is_inlinable`'s shape to admit an instruction that can
actually error, yield, or be introspected.

Full suite: 130 lib tests (up from 128), every integration suite including
`lua55_dynamic_runtime_jit.rs` at 19/19, `sol-core`'s 37, and the Lua 5.5
manifest check, all green, zero new warnings.

**Item 8 note (documentation pass + full benchmark re-run; exit-gate
determination):** this item is verification/documentation only - no new Rust
was written. It re-ran every differential suite (`crate/sol`'s 130 lib tests,
every integration suite including `lua55_dynamic_runtime_jit.rs` at 19/19,
`crate/sol-core`'s 37, the Lua 5.5 manifest check, the full pinned
`lua-5.5.1-tests` corpus via `scripts/test-lua55-suite.sh` - 16 passed, 1
pending, 10 host-required/skipped, 7 documented divergences, **0 failed**, no
regression against the manifest's own expectations - and `scripts/
test-sol-c-api.sh`, all green) and then ran the full `scripts/benchmark.sh`
suite (`hyperfine`, warmup 3/min-runs 10, both warm and cold) with `luajit`
2.1.1787165859 and reference Lua 5.5.1 both present on this machine, exported
to `/tmp/u10-item8/full-suite.md` (scratch, not committed) and consolidated
into [`benchmarks/RESULTS.md`](../../benchmarks/RESULTS.md)'s new `## U10`
section.

**The §7.3 dynamic-parity gate (≥1.0x LuaJIT geomean, no workload >20%
slower) is not reached - not close.** Across the 14 general-Lua benchmarks in
`benchmarks/*.lua` that have both a `luajit` and `sol (dynamic)` row, the
geometric mean of `sol (dynamic)`'s time over `luajit`'s is **~110x slower**,
ranging from 4.7x (`string_concat`) to 587x (`vararg_calls`) - every single
workload is far more than 20% slower, not a subset. See `benchmarks/
RESULTS.md`'s new section for the full table.

**A more important finding than the raw gap: `SOL_LUA_JIT_LOG=1` traces
confirm none of U10's shipped optimizations (items 3/4/6/7) engage in
*almost any* of these benchmarks at all**, for the same structural reasons
the item-3/4 notes above already predicted in the abstract - this pass
confirmed it empirically, benchmark by benchmark, rather than leaving it as a
theoretical gap:

- `loop_sum`, `nested_loop`, `matrix`, `hashmap_lookup`, `table_array`,
  `string_concat` log **no dynjit activity whatsoever** - each is a bare
  top-level numeric-`for` loop with no function call in its hot path, so the
  one `Proto` that would need to promote (the main chunk) is only ever
  activated once; `call_count` never crosses any threshold regardless of
  loop iteration count (the exact U9-era limitation `RESULTS.md`'s own U9
  section already documented).
- `objects` (`dist_squared`), `metatable_dispatch` (`Circle:area`/`Square:
  area`/`Triangle:area`), `vararg_calls` (`triple`), and `gc_alloc` (`make`)
  all reach U9's baseline `Native` tier but are explicitly logged as **not
  eligible for item-3 optimizing lowering** ("anything beyond
  arithmetic/branch/return - calls, table/global/upvalue access, closures, or
  `for` loops") - every one of these bodies touches a table/global/field,
  which `sol_ir::lift_proto` still doesn't model at all.
- `coroutine_resume`'s `while`-loop body is logged as **OSR-requested but not
  eligible** - its `coroutine.yield()` call breaks the same eligible-
  instruction-set item 3 requires, so item 4's OSR (which only needs the
  instructions item 3 already covers) can't engage either.
- `function_calls` and `function_calls_closure` are the **only** two
  benchmarks in the suite where anything U10 shipped actually fires: `work`/
  `<anonymous@4>`'s own bodies (pure chained-`Binary`-add arithmetic) reach
  item 3's `Optimized` tier. But no inlining ever engages for either - their
  *callers* each contain a numeric `for` loop, which `sol_ir::lift_proto`
  does not model, so the caller `Proto` is never `fully_lifted`/`opt_lower`-
  eligible and items 6/7's call-splicing is never attempted at that call
  site at all. These two benchmarks' real ~420x/~378x LuaJIT gap is
  therefore the per-call marshaling/trampoline/counter overhead item 3's own
  note already named as the dominant cost - exactly reproducing that note's
  "no measurable difference" finding on a real end-to-end benchmark rather
  than only a scratch micro-benchmark.

**Conclusion, stated plainly rather than rounded up**: this milestone's own
real, measured wins (item 4's ~2.7x OSR speedup, item 7's ~2x inlining
speedup) are both real but live entirely in hand-built fixtures
(`dynjit_osr_while_sum.lua`'s `while`-loop shape, `dynjit_inline_call_stable.
lua`'s monomorphic-call shape) deliberately constructed to land inside each
item's narrow eligible subset - **none of the checked-in general-purpose
`benchmarks/*.lua` suite happens to have that shape**, because real-world
idiomatic Lua favors numeric `for` over `while` for counted loops and
routinely touches tables/globals/fields, both of which fall outside every
one of `sol_ir::lift_proto`'s modeled instructions. U10 has not reached (and
on this evidence is not close to) the dynamic-parity gate; per the plan's own
exit-gate language, this release states plainly that it has not yet achieved
that goal rather than waiving or rounding up the claim. Widening
`sol_ir::lift_proto` to model `ForPrep`/`ForLoop` and table/field/global
access is the highest-leverage next investment to make any of this
milestone's real machinery (guards, OSR, inlining) reachable from ordinary,
unmodified Lua code - not a new item within U10 itself, but the natural
starting point for whatever milestone picks this back up.

See the [historical U10 ledger](../unified-sol-runtime-plan.md#u10--optimizing-ssa-jit-osr-and-deoptimization).
