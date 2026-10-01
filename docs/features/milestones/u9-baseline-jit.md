# U9 — Baseline dynamic JIT

**Status:** in progress

**Purpose:** remove hot untyped dispatch overhead.

- [x] Lower generic and cached bytecode quickly to Cranelift. Leaf
      arithmetic/comparison/branch instructions
      (`crate/sol/src/lua_runtime/dynjit/lower.rs`'s `lower_instr`), then
      field/global/index access and `NewTable` allocation
      (`dynjit_field_get`/`dynjit_global_get`/`dynjit_new_table` stubs,
      `crate/sol/src/lua_runtime/dynjit/stubs.rs`), then
      `Call`/`TailCall`/`TForCall`/`CloseSlots`/`MarkClose`/`TForLoop`
      (`lower.rs:453-475`, `lower_tfor_loop` at `lower.rs:624`) all lower.
      `is_eligible` (`lower.rs:60`) now only excludes captured-upvalue
      registers (tracked as the item-9-follow-up item below).
- [x] Provide semantic slow-path stubs. Every instruction that can observe or
      trigger GC, metatables, or arbitrary Lua re-entry goes through an
      `extern "C"` stub that re-derives its operands from
      `frame.proto.instrs[pc]` and returns success/deopt
      (`crate/sol/src/lua_runtime/dynjit/stubs.rs`); `dynjit_mark_close`
      (`stubs.rs:898`) is the item-5 addition — safe as an ordinary stub
      because `metamethod` (`dispatch.rs:2705`) only does a metatable lookup,
      never invokes Lua.
- [x] Implement safepoints, stack maps, errors, and coroutine fallback.
      Safepoints: `emit_safepoint` (item 2) at backward branches. Errors and
      coroutine yields: `Call`/`TailCall`/`TForCall`/`CloseSlots` unconditionally
      deopt to the interpreter at their own pc rather than resuming natively
      after the call (see the item 5/6 note below) — `run_native`
      (`dispatch.rs:1130`) flushes the native `regs` shadow copy back into
      `frame.regs` before checking the stub's outcome, so the interpreter's
      existing, unmodified call/return/`pcall`/coroutine/`<close>` handling
      takes over with fully correct state regardless of which of those four
      instructions triggered the deopt. Differential coverage:
      `crate/sol/tests/lua55_dynamic_runtime_jit.rs` (nested calls, `pcall`
      around a call, coroutine yield from inside a promoted function, generic
      `for`, `<close>` variables), each asserted byte-identical against the
      non-promoted baseline and re-run under `SOL_LUA_GC_STRESS=1`.
- [x] Define bounded hot-function compilation policy. `SOL_LUA_PROMOTE_THRESHOLD`
      / `call_count: Cell<u32>` on `Proto` (item 2), synchronous bounded
      compilation (no background thread).
- [ ] Add direct entries for stable call targets. **Measured against the
      shipped design, not adopted — this bullet does not apply as scoped.**
      The plan's direct-entry-adapter text
      (`docs/features/unified-sol-runtime-plan.md`, U9 deliverable 5)
      presupposes item 5 built genuine park-before-call machinery: native
      code parks a frame before a call and resumes natively, in the same
      invocation, once that call returns — a direct-entry adapter would then
      skip only the `call_cache`-guard resolution overhead on that resume
      path. What item 5 actually shipped (commit `9804df8`, see that commit
      message and the bullet above) is unconditional deopt: every `Call`/
      `TailCall`/`TForCall` hands off to the interpreter and never resumes
      natively within that invocation, regardless of the call site's
      `call_cache` monomorphism or the target's `NativeStatus`. There is no
      "resumes natively after an ordinary call" path left for a direct-entry
      adapter to speed up — the call site's resolution cost is already paid
      only once, by the interpreter, after every deopt. Building the
      park-before-call machinery this bullet actually requires was assessed
      during item 5's own design (moving a `LuaFrame` out from under a raw
      pointer mid-native-execution to let native code resume after a call)
      as materially higher-risk than the baseline tier's unconditional-deopt
      tradeoff, and was deliberately not built; redoing that decision now
      only to unlock this bullet would reopen a closed, already-tested,
      already-benchmarked milestone item without new justification. No code
      was written for this bullet.
- [ ] Manage code-cache lifecycle, invalidation, and executable-memory safety.

**Exit gate:** baseline-JIT readiness passes within published compile-latency
and code-memory budgets, with interpreter/JIT differential and GC tests.

See the [historical U9 ledger](../unified-sol-runtime-plan.md#u9--baseline-dynamic-jit).
