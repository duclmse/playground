# Phases 4-8 implementation notes

Status snapshot of the debugger work beyond Phase 3's risk-spike. Written
because the actual implementation diverged from `debug-protocol.md`'s plan
in several concrete, discovered-along-the-way ways that the spec couldn't
have anticipated - this is the "what actually happened" companion to that
design doc, in the same spirit as [risks.md](./risks.md).

## Summary

| Phase | Spec'd in                                                              | Status                                                             |
| ----- | ----------------------------------------------------------------------- | ------------------------------------------------------------------ |
| 4     | Breakpoints, Monaco integration                                         | **Engine done, tested. UI not wired.**                             |
| 5     | Call stack + stepping                                                   | **Engine done, tested. UI not wired.**                             |
| 6     | Inspector (values, locals/globals/tables/metatables)                    | **Engine done, tested (upvalues excluded). UI not wired.**         |
| 7     | Expression evaluation                                                   | **Engine done, tested (frame-scoped locals + globals). UI not wired.** |
| 8     | Conditional/hit-count/logpoint breakpoints, exception breakpoints        | **Engine done, tested.**                                           |
| 8     | Coroutine debugging, profiler, execution timeline                       | **Not built - designed below.**                                    |

"Engine" = `crates/vm` (fork) + `crates/lua-vm/src/session.rs` (the
`DebugSession` struct), all covered by Rust tests running against the real
piccolo-backed VM (`cargo test -p lua-vm session::`, 16 tests). "Wiring" =
`apps/web/src/debug-protocol.ts` + `lua-worker.ts` + `debug-session.ts`, a
complete worker-message-protocol implementation of
[debug-protocol.md](./debug-protocol.md)'s `DebugSession` TypeScript
interface, typechecked and included in the production build - but **not
called from any React component**, and therefore never exercised in a real
browser. No Monaco breakpoint gutter, call stack panel, variables tree,
step/continue buttons, or REPL/watch UI exist yet. That's the honest state:
the hard, novel part (making piccolo debuggable at all) is done and tested;
the UI is a bounded, well-specified remaining task with no open design
questions left to resolve, listed at the end of this doc.

## What the fork (`crates/vm`) added beyond Phase 3

Phase 3 added `Executor::current_running_thread`,
`Executor::step_with_granularity`, `Thread::debug_snapshot`,
`Thread::debug_lua_frame_depth`, `FunctionPrototype::line_for_pc`. This
round added:

- **`Thread::debug_frames()`** - the full call stack (all `Frame::Lua` and
  `Frame::Callback` frames, bottom to top), not just the top frame.
  Everything in Phases 5-7 is built on this; `debug_snapshot`/
  `debug_lua_frame_depth` are unchanged and still used by Phase 3's
  `debug_events.rs`.
- **`Thread::debug_read_register` / `debug_write_register`** - read/write
  one register at a given Lua-frame depth (0 = innermost). Backs
  `get_locals`, `evaluate`, and `set_variable`.
- **Named locals: a compiler patch, not just a `thread.rs` accessor.**
  `crates/vm/src/compiler/compiler.rs`'s `CompilerFunction` already tracked
  `locals: Vec<(name, register)>` *during* compilation, but discarded it
  once a scope closed - upstream piccolo never persisted a name-to-register
  mapping into the runtime `FunctionPrototype` at all. This pass adds a
  `declare_local()` helper that also records each local's `[start_pc,
  end_pc)` live range as it comes into and goes out of scope, threading a
  `local_variables: Vec<(name, register, start_pc, end_pc)>` table through
  `CompiledPrototype` and `FunctionPrototype`
  (`FunctionPrototype::local_name_at(pc, register)` does the lookup). This
  is *the* reason `get_locals`/`evaluate` can show `x`, `y`, `dummy` instead
  of `R0`, `R1`, `R2` - without it, Phase 6's inspector would only ever be
  register-indexed, which the original spec's `Scope { type: "register" }`
  fallback was explicitly hedging for. Register reuse across non-overlapping
  scopes is handled correctly (verified by a dedicated test compiling
  `local x`, then a nested-block `local y`, then a later `local z` that
  reuses `y`'s register - each resolves to the right name at the right pc).
- **One `dangerous_implicit_autorefs` fix in `string.rs`** carried over from
  Phase 3, unrelated to any of the above.

Upvalue names were *not* given the same treatment (see "Known gaps" below) -
a deliberate scope cut, not an oversight.

## `DebugSession` (`crates/lua-vm/src/session.rs`)

One struct implementing (almost) all of `debug-protocol.md`'s
`DebugSession` interface directly in Rust, exposed via `wasm_bindgen`:
`launch`/`launch_project`, `set_breakpoint`/`remove_breakpoint` (+
`set_breakpoint_condition`/`_hit_condition`/`_log_message` for Phase 8),
`continue_`/`step_over`/`step_into`/`step_out`, `get_stack_trace`,
`get_locals`/`get_globals`/`get_table_entries`/`get_metatable`, `evaluate`/
`set_variable`, `take_output`. 16 tests exercise all of it against the real
engine - breakpoints stop at the right line, stepping algorithms match
docs/debug-protocol.md#stepping-algorithms exactly (depth+line comparison
for over/into/out), the call stack reports real frame names and lines,
globals/tables lazy-expand with correct reference-identity aliasing
(`t.self = t` resolves to the same reference, not an infinite structure),
`evaluate` sees the paused frame's locals and falls back to globals, and
`set_variable` writes back into the live paused frame.

### Why it's a Rust struct, not TypeScript

`debug-protocol.md` describes `DebugSession` as a TypeScript interface with
a `LuaDebugger` implementation underneath. The actual `DebugSession` *class*
lives in Rust instead, because everything it needs - per-frame register
access, a resumable `Executor`, the named-local debug table - only exists on
this side of the `wasm-bindgen` boundary; a TypeScript implementation would
just be a thin pass-through with no logic of its own. What debug-protocol.md
calls "`LuaDebugger`" is `apps/web/src/debug-session.ts`'s TypeScript
`DebugSession` class - it owns request/response correlation over the worker
and nothing else. This is a shape change from the original design, not a
scope cut: the logic all exists, just on the other side of the boundary than
the doc assumed.

### Three findings that changed the design mid-implementation

**1. `evaluate()` cannot run inside the live paused frame.** The spec says
evaluation must "run in the selected stack frame's environment." piccolo's
`Executor` has no API for resuming execution into the middle of an
already-suspended call (`Executor::step` only ever advances the *top* frame;
there's no "call this function as if from frame N" primitive, and
`Executor`s explicitly panic on any kind of reentrant use - confirmed
reading `executor.rs`'s own doc comment, and again via Phase 3's risks.md
research). The implementation instead: snapshots the frame's *named*
locals into a fresh table, compiles the expression as a small wrapper chunk
(`local x = __locals.x; ...; return (expr)`) that takes that table as its
one argument, and runs it as a completely separate, temporary `Executor` in
the same `Lua`/globals arena. Consequences, all covered by tests:

- Global references in `expression` work normally (same globals table the
  paused program sees) - `evaluate_falls_back_to_globals`.
- Local references work by name, read at evaluation time -
  `evaluate_sees_the_paused_frames_locals`.
- An assignment to an *existing local* inside `expression` does **not**
  write back to the paused frame (it mutates the snapshot copy, which is
  then discarded) - `set_variable` exists precisely because `evaluate()`
  can't do this.
- Upvalues are invisible to `evaluate()` (not in the locals snapshot; see
  "Known gaps").

**2. `set_variable()` needed a second design pass**, because the first
draft tried to call `evaluate()` (which returns an owned `String`, by
necessity - see next finding) and then separately re-parse/re-evaluate the
value to get a real `Value` to write. That's fragile and was replaced:
`set_variable` runs the same wrapper-chunk evaluation as `evaluate`, but
does the `Thread::debug_write_register` write *inside the same `try_enter`
call* that produces the final `Value` from `take_result` - both the write
and the value's construction happen under the same `'gc` branding lifetime,
which is a hard requirement, not a style choice (next point).

**3. A `Value<'gc>` cannot cross between separate `lua.enter()`/
`try_enter()` calls**, full stop - this shaped almost every method in
`session.rs`. `Lua::enter<F, T>(&mut self, f: F) -> T where F: for<'gc>
FnOnce(Context<'gc>) -> T` fixes `T` *before* `f` runs, so `T` can never
mention the particular `'gc` that `f`'s body introduces - the compiler
rejects it outright (confirmed the hard way: an early draft did exactly
this and it doesn't compile, not "works but is unsound"). Every method that
touches a `Value` converts it to owned data (a `String` display, a
`Variable`/`StackFrame` wasm-bindgen struct) *before* returning from the
`enter`/`try_enter` closure it came from. This is also why `evaluate()`
returns `EvalResult { ok, display: String }` rather than a typed value -
there is no way to hand a live `Value` across the `wasm-bindgen` boundary at
all, since JS calls are, from Rust's perspective, separate top-level calls
each getting their own `enter()`.

### A granularity finding that affects performance, not correctness

`continue()`/stepping all drive `Executor::step_with_granularity(.., 1)` -
Phase 3's fix for the "coarse batching skips lines" problem. That fix is
necessary for correctness (a breakpoint mid-line must not be skipped over),
but it means **every `continue()` runs the whole remaining program one
opcode at a time** until it hits a breakpoint or terminates - there is no
fast path for "run to completion with no breakpoints set." For interactive
debugging of typical playground-sized scripts this is unnoticeable; it
would matter for a script that's compute-heavy *and* has no breakpoints hit
along the way (which is arguably not what a debugger's `continue()` should
be optimizing for regardless). If it becomes a real complaint, the fix is
adaptive granularity: run at granularity 64 while no breakpoint lines exist
in the current prototype, drop to granularity 1 only once one might be
about to be hit. Not implemented - no evidence yet that it needs to be.

### A tail-call finding that affects call-stack fidelity, not correctness

A proper Lua tail call (`return f()`) *replaces* the caller's frame rather
than pushing a new one - `crates/vm/src/thread/vm.rs`'s `TailCall` handling,
confirmed real by writing a call-stack test with `return inner()` first and
watching the caller's frame legitimately disappear from `debug_frames()`.
This matches real Lua semantics (proper tail calls don't grow the stack) and
is not a bug - `debug-protocol.md`'s call-stack section doesn't mention it,
so it's recorded here: a call stack view built on this will show one fewer
frame than the *source* nesting suggests whenever tail calls are involved,
which is correct, not confusing once you know to expect it.

### A pc-convention finding that affects step_out's exact stop line

Phase 3 established that piccolo's reported "current line" is always *the
next instruction about to run*, not the one just executed. `step_out`
inherits a consequence of this: `Call` opcodes place the return value
directly into the destination register as part of returning (no separate
"store result" opcode), so if a call's result is the *only* remaining work
on its source line, `step_out` lands on the line *after* the call, not back
on the call's own line - there's no observable point at which execution is
still "on" that line post-return. Covered by `step_out_returns_to_the_caller`,
with the exact opcode-table reasoning in the test's own comment.

## Known gaps (deliberate scope cuts)

- **Upvalues are not exposed at all** - no `get_upvalues()`, and
  `evaluate()`/`get_locals()` can't see them. Piccolo's compiled
  `FunctionPrototype` keeps upvalue *resolution descriptors*
  (`UpValueDescriptor`) but not names, the same gap locals had before this
  round's compiler patch - fixing it needs the identical treatment
  (`CompilerFunction.upvalues: Vec<(name, descriptor)>` already exists at
  compile time; threading the name through to `FunctionPrototype` is
  the same shape of change `local_variables` got). Cut to keep this
  deliverable finite, not because it's harder than the locals fix was.
- **`pause()` is not implemented.** `debug-session.ts`'s `pause()` throws.
  `DebugSession::continue_()` runs to completion (or a breakpoint/error) in
  one synchronous Rust call, so there's no in-flight operation to interrupt
  from JS. A responsive pause needs the Rust side to run in bounded bursts
  (e.g. `continue_burst(max_instructions) -> BurstResult { hit_pause_point:
  bool, .. }`) that the worker calls in a loop, checking a "pause requested"
  flag between bursts - the same pattern `MAX_INSTRUCTIONS` already uses for
  runaway-loop protection, just exposed incrementally instead of run once to
  a fixed cap. Not implemented; the design has no open questions, it's
  scoped mechanical work.
- **Function/Thread/UserData values are registered in the object registry
  (for identity and to keep them GC-alive) but have no getter** -
  `get_table_entries`/`get_metatable` only work on tables. `marshal_value`
  deliberately reports them as `expandable: false` rather than exposing a
  reference nothing can resolve. Adding `get_function_info`/
  `get_thread_info` would follow the same pattern as `get_table_entries`.

## Phase 8 items not built

**Coroutine debugging.** `Executor` already has the right shape for this -
`thread_stack: Vec<Thread<'gc>>` (confirmed reading `executor.rs`; this is
what `current_running_thread()` reads the *top* of) - so `getThreads()`
returning one entry per coroutine plus the main thread, and
`debug_frames()`/`get_locals` etc. taking a `thread_id` instead of always
targeting `current_running_thread()`, is a natural extension of the
existing design, not a redesign. Not attempted here for the same reason as
everything else in this section: real, non-trivial engine work
(`step_with_granularity`'s call/return depth tracking would need to become
per-thread, since a coroutine resume/yield changes the *active* thread
without the main thread's own Lua-frame depth changing) that deserves its
own implementation-and-test pass rather than a rushed addition.

**Profiler** (`FunctionStats`: calls/totalTime/selfTime/instructions).
`debug-protocol.md` is right that this "falls out of CALL/RETURN/COUNT-
equivalent events almost for free" - `DebugSession`'s `drive()` loop already
observes every call/return transition and already counts consumed
instructions (`consumed` in `drive()`, and `MAX_INSTRUCTIONS` accounting in
`run_to_completion`). A profiler would run `continue()`-equivalent logic
without ever actually stopping, accumulating a
`HashMap<chunk_name, FunctionStats>` keyed by call-frame identity. Not
built; would want its own always-runs-to-completion entry point rather than
reusing `continue()`, since breakpoints and profiling are different modes
("stop when interesting" vs. "never stop, just count").

**Execution timeline.** Recording the `line`/`call`/`return` event stream
Phase 3's `debug_events.rs` already produces, and rendering it, is mostly a
UI/visualization task once `debug_events()` exists (it does, since Phase 3)
- the interesting design question `debug-protocol.md` leaves open is
*retention*: a real script can emit hundreds of thousands of events, and
`debug_events()`'s current one-shot "run the whole thing and return every
event" shape doesn't bound memory. A timeline feature would need a
capped/downsampled ring buffer rather than the raw `Vec<DebugEvent>` Phase 3
built for its (short, test-fixture-sized) spike scripts. Not built.

## What a UI pass needs to do

Nothing below requires new engine design - it's wiring `debug-session.ts`
(already complete and typechecked) into React, plus the usual UI work:

1. **Breakpoints**: Monaco gutter click → `DebugSession.setBreakpoint`/
   `removeBreakpoint`; red dot decoration for `verified`.
2. **Run controls**: Continue/step over/into/out buttons calling the
   matching `DebugSession` methods; yellow-arrow line decoration from each
   `StopEvent.line`.
3. **Call stack panel**: `getStackTrace()`, clickable frames that change
   which frame index feeds the variables panel and `evaluate()`/
   `setVariable()` calls.
4. **Variables panel**: `getScopes()` → "Locals"/"Globals" sections,
   `getLocals`/`getGlobals` for each, `getVariables(reference)` for lazy
   table expansion, `getMetatable(reference)` for the metatable tree node
   debug-protocol.md's inspector section describes.
5. **Watch/REPL panel**: `evaluate(expression, frameIndex)`, re-run on every
   stop for watches per debug-protocol.md#evaluation.
6. **Conditional/hit-count/logpoint UI**: right-click a breakpoint to set
   `setBreakpointCondition`/`setBreakpointHitCondition`/
   `setBreakpointLogMessage` - the engine side is done and tested, this is
   purely a context-menu/dialog.

Every one of these should get a real Playwright pass against the running
app before being called done, per this repo's own standard (see
`docs/risks.md` §8's WASM-load-time verification for the precedent) - none
of the above has been exercised in an actual browser yet.
