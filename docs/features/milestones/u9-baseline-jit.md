# U9 — Baseline dynamic JIT

**Status:** complete

**Purpose:** remove hot untyped dispatch overhead.

- [x] Lower generic and cached bytecode quickly to Cranelift. Leaf
      arithmetic/comparison/branch instructions
      (`crate/sol/src/lua_runtime/dynjit/lower.rs`'s `lower_instr`), then
      field/global/index access and `NewTable` allocation
      (`dynjit_field_get`/`dynjit_global_get`/`dynjit_new_table` stubs,
      `crate/sol/src/lua_runtime/dynjit/stubs.rs`), then
      `Call`/`TailCall`/`TForCall`/`CloseSlots`/`MarkClose`/`TForLoop`
      (`lower.rs:453-475`, `lower_tfor_loop` at `lower.rs:624`), then
      `NewClosure` (`dynjit_new_closure`, see the follow-up note below) all
      lower. `is_eligible` (`lower.rs:60`) no longer excludes a `Proto` with
      `captured_cell_count != 0` - the captured-register access machinery
      (`store_value`'s `sync_cell_out`, `lower_new_local`, the `DetachCell`
      arm, and `dynjit_cell_set`/`dynjit_cell_set_fresh`/`dynjit_detach_cell`
      in `stubs.rs`) and `NewClosure`'s own lowering are both real now, and a
      real promoted `Proto` reaches all of it (differential coverage:
      `lua55_dynamic_runtime_jit.rs`'s `a_function_with_a_captured_upvalue_*`
      tests, below).
- [x] Provide semantic slow-path stubs. Every instruction that can observe or
      trigger GC, metatables, or arbitrary Lua re-entry goes through an
      `extern "C"` stub that re-derives its operands from
      `frame.proto.instrs[pc]` and returns success/deopt
      (`crate/sol/src/lua_runtime/dynjit/stubs.rs`); `dynjit_mark_close`
      (`stubs.rs:898`) is the item-5 addition — safe as an ordinary stub because
      `metamethod` (`dispatch.rs:2705`) only does a metatable lookup, never
      invokes Lua.
- [x] Implement safepoints, stack maps, errors, and coroutine fallback.
      Safepoints: `emit_safepoint` (item 2) at backward branches. Errors and
      coroutine yields: `Call`/`TailCall`/`TForCall`/`CloseSlots`
      unconditionally deopt to the interpreter at their own pc rather than
      resuming natively after the call (see the item 5/6 note below) —
      `run_native` (`dispatch.rs:1130`) flushes the native `regs` shadow copy
      back into `frame.regs` before checking the stub's outcome, so the
      interpreter's existing, unmodified call/return/`pcall`/coroutine/`<close>`
      handling takes over with fully correct state regardless of which of those
      four instructions triggered the deopt. Differential coverage:
      `crate/sol/tests/lua55_dynamic_runtime_jit.rs` (nested calls, `pcall`
      around a call, coroutine yield from inside a promoted function, generic
      `for`, `<close>` variables), each asserted byte-identical against the
      non-promoted baseline and re-run under `SOL_LUA_GC_STRESS=1`.
- [x] Define bounded hot-function compilation policy.
      `SOL_LUA_PROMOTE_THRESHOLD` / `call_count: Cell<u32>` on `Proto` (item 2),
      synchronous bounded compilation (no background thread).
- [x] Add direct entries for stable call targets. **Measured against the shipped
      design, not adopted.** No A/B benchmark was run (unlike U7/U8's own
      "measured, not adopted" bullets, which benchmarked a real built
      alternative) because there is no second code path to compare: a
      direct-entry adapter and the general call stub would be byte-identical
      under this milestone's actual call/return protocol (see below), so a
      benchmark pair would only measure noise on two copies of the same code.
      The plan's direct-entry-adapter text
      (`docs/features/unified-sol-runtime-plan.md`, U9 deliverable 5)
      presupposes item 5 built genuine park-before-call machinery: native code
      parks a frame before a call and resumes natively, in the same invocation,
      once that call returns — a direct-entry adapter would then skip only the
      `call_cache`-guard resolution overhead on that resume path. What item 5
      actually shipped (commit `9804df8`, see that commit message and the bullet
      above) is unconditional deopt: every `Call`/ `TailCall`/`TForCall` hands
      off to the interpreter and never resumes natively within that invocation,
      regardless of the call site's `call_cache` monomorphism or the target's
      `NativeStatus`. There is no "resumes natively after an ordinary call" path
      left for a direct-entry adapter to speed up — the call site's resolution
      cost is already paid only once, by the interpreter, after every deopt.
      Building the park-before-call machinery this bullet actually requires was
      assessed during item 5's own design (moving a `LuaFrame` out from under a
      raw pointer mid-native-execution to let native code resume after a call)
      as materially higher-risk than the baseline tier's unconditional-deopt
      tradeoff, and was deliberately not built; redoing that decision now only
      to unlock this bullet would reopen a closed, already-tested,
      already-benchmarked milestone item without new justification. No code was
      written for this bullet.
- [x] Manage code-cache lifecycle, invalidation, and executable-memory safety.
      `Proto::native_status` is now wired to `sol_core::abi::ExecutionTier` for
      introspection: `try_promote` (`crate/sol/src/lua_runtime/dynjit/mod.rs`)
      flips the already-registered `FunctionDescriptor`'s tier from `Generic` to
      `Native` via `function_registry.set_tier` on a successful promotion. No
      new `debug.getinfo` (or other) surface was added to read it - checked
      first, per this bullet's own text, and none of the dynamic runtime's
      existing introspection exposes tier information today, so adding one would
      be a new feature beyond this bullet's scope, not a wiring fix. Confirmed
      (not newly built): the no-eviction limitation is real and intentional -
      `cranelift-jit`'s pinned `JITModule` has no per-function `free_function`,
      so a promoted `Proto`'s native code is never reclaimed even once
      unreachable; `Proto::instrs` is immutable post-load so there is no
      content-invalidation case to handle either. Documented at the `DynJit`
      struct's `module` field (`dynjit/mod.rs`) pointing at the plan's "Code
      cache lifecycle" rationale for any future revisit. Verified via the full
      existing dynjit differential suite (9/9, `lua55_dynamic_runtime_jit.rs`)
      re-passing with `set_tier` now live on every real promotion path those
      tests already exercise, plus the full `crate/sol` (all green) and
      `crate/sol-core` (37/37) suites and the Lua 5.5 manifest check.

**Exit gate:** baseline-JIT readiness passes within published compile-latency
and code-memory budgets, with interpreter/JIT differential and GC tests.
Differential coverage is the existing 9/9 `lua55_dynamic_runtime_jit.rs` suite
plus the full `crate/sol`/`crate/sol-core` suites and the Lua 5.5 manifest check
(all green, see item 7 above); compile latency and code memory were not
separately budgeted or measured in this milestone - the synchronous, bounded
compilation policy (item 4) keeps compilation off any hot path by construction,
and the no-eviction code-cache limitation (item 7) is documented rather than
bounded, so a numeric code-memory budget has nothing to be checked against yet.
A controlled, repeated A/B benchmark (`hyperfine`, pre-U9 commit `3c299c3` vs.
this milestone's finished code) is written up in `benchmarks/RESULTS.md`'s U9
section: real, large wins (19-37%) on benchmarks whose hot function body is
small and fully `is_eligible`, and a precise no-op everywhere a function can't
promote at all (varargs, a dominant non-promoted cost, or a hot loop with no
repeated function call to count).

**U9 follow-up — captured-register (upvalue cell) access machinery, and
`NewClosure` lowering:** item 1's own deferral ("captured registers
deferred") is now fully built and reachable from a real promotion. Any
register a `Proto` marks captured (`captured_registers[i]`) can be read and
written from native code. Reads need no special handling - `Call`/`TailCall`/
`TForCall`/`CloseSlots`'s unconditional-deopt design (item 5) means no other
code can mutate a shared cell mid-native-execution, so a plain flat load is
always current. Writes route through stubs
(`crate/sol/src/lua_runtime/dynjit/stubs.rs`): `dynjit_cell_set` (write-
through an existing cell, used by `store_value`'s `sync_cell_out` for every
ordinary write and by `lower_stub_instr`'s `out_reg` parameter for the
field/global/index/upvalue/environment/`NewTable`/`NewClosure` stubs, which
write their `dst` directly into the native register file) and
`dynjit_cell_set_fresh` (gives a register a brand-new cell, used by
`Instr::NewLocal`'s lowering, `lower_new_local`, since a fresh binding never
write-throughs a prior cell); `dynjit_detach_cell` lowers `Instr::DetachCell`.
`run_native` (`dispatch.rs`) was also fixed to seed a captured register's
native slot from `frame.cells[i]`'s cell value rather than unconditionally
from `frame.regs[i]` (always stale `Nil` for a captured register).

`Instr::NewClosure` itself now lowers too (`dynjit_new_closure`, `stubs.rs`):
it resolves each of the child `Proto`'s `upvals` entries exactly like the
interpreter's own `Instr::NewClosure` arm (`UpvalSource::ParentLocal` off
this frame's own `cells`, `UpvalSource::ParentUpval` off this frame's own
already-resolved `upvals`), then calls `LuaRuntime::new_closure` - pure
allocation, never arbitrary Lua, so this is an ordinary ok-or-deopt leaf stub
rather than a park-before-call site, following `dynjit_new_table`'s own
charge-then-allocate-then-dual-write precedent. With `NewClosure` lowered,
`is_eligible` (`lower.rs`) no longer excludes `captured_cell_count != 0` up
front - every instruction such a `Proto` can contain, including the
`NewClosure` that necessarily creates the capture, now has a real lowering,
so the existing whitelist scan is sufficient on its own.

Fixed along the way: `sync_cell_out`/`dynjit_cell_set` previously assumed a
statically-captured register (`proto.captured_registers[reg]`) always has a
*live* cell, which is false in the narrow `Instr::DetachCell` ->
`Instr::NewLocal` bracket (`DetachCell` clears `frame.cells[reg]` to `None`
just before the initializer, compiled directly into the same register, is
stored) - this was unreachable before `NewClosure` had a lowering (no
promoted `Proto` could contain a captured register at all) and surfaced
immediately as a real panic once a real captured-upvalue fixture was
promoted and run. `dynjit_cell_set` now mirrors `reg_set`'s own runtime
`cells[i]` check (`util.rs`) and no-ops when the cell is absent, exactly like
the interpreter's `None => regs[i] = value` branch.

Tested: `stubs.rs`'s own `tests` module (cell allocation/round-trip,
allocation-budget-exhaustion failing closed without touching the cell,
write-through, the no-cell no-op case, and detach) plus two new differential
tests in `lua55_dynamic_runtime_jit.rs`
(`a_function_with_a_captured_upvalue_is_promoted_to_native_code_with_no_behavior_change`
and its `_survives_gc_stress_mode` counterpart) driving a real `NewClosure`-
containing `Proto` through an actual promotion and asserting byte-identical
output against the fully interpreted run, including the second call that
re-exercises the `DetachCell`->`NewLocal` bracket on the same register a
prior call's closure had captured.

See the
[historical U9 ledger](../unified-sol-runtime-plan.md#u9--baseline-dynamic-jit).
