# Phases 4-8 implementation notes

Status snapshot of the debugger work beyond Phase 3's risk-spike. Written
because the actual implementation diverged from `debug-protocol.md`'s plan
in several concrete, discovered-along-the-way ways that the spec couldn't
have anticipated - this is the "what actually happened" companion to that
design doc, in the same spirit as [risks.md](./risks.md).

## Summary

| Phase | Spec'd in                                                              | Status                                                             |
| ----- | ----------------------------------------------------------------------- | ------------------------------------------------------------------ |
| 4     | Breakpoints, Monaco integration                                         | **Done, tested, verified in a real browser.**                      |
| 5     | Call stack + stepping                                                   | **Done, tested, verified in a real browser.**                      |
| 6     | Inspector (values, locals/globals/tables/metatables)                    | **Done, tested, verified in a real browser (upvalues excluded).**  |
| 7     | Expression evaluation                                                   | **Done, tested, verified in a real browser.**                      |
| 8     | Conditional/hit-count/logpoint breakpoints, exception breakpoints        | **Engine done, tested.**                                           |
| 8     | Coroutine debugging                                                      | **Done, tested, verified in a real browser.**                      |
| 8     | Profiler                                                                 | **Engine done, tested. No UI.**                                    |
| 8     | Execution timeline                                                       | **Engine done, tested (capped recording). No UI.**                 |

"Engine" = `crates/vm` (fork) + `crates/lua-vm/src/session.rs` (the
`DebugSession` struct) + `crates/lua-vm/src/profiler.rs` +
`crates/lua-vm/src/debug_events.rs`'s capped timeline addition, all covered
by Rust tests running against the real piccolo-backed VM (`cargo test -p
lua-vm`, 40 tests: 19 `session::`, 2 `profiler::`, 19 others). Phases 4-7 and
Phase 8's coroutine debugging are also wired all the way through:
`apps/web/src/debug-protocol.ts` + `lua-worker.ts` + `debug-session.ts`
implement
[debug-protocol.md](./debug-protocol.md)'s `DebugSession` TypeScript
interface over the worker message protocol, and `App.tsx` + `DebugPanel.tsx`
+ `VariablesTree.tsx` wire it into a real UI - a Monaco breakpoint gutter
(click to toggle, red dot for set, yellow arrow for the paused line), call
stack/locals/globals panels with lazy table+metatable expansion, a watch
list, a frame-scoped REPL, and (Phase 8) a thread selector that appears
once a coroutine is on the resume chain. **Verified against the actual
running app** with Playwright (breakpoint hit, locals/globals inspected,
step over, watch + REPL evaluated, continue to completion, output
displayed, error/exception path, re-launching after Stop, and a coroutine
scenario with per-thread call stacks/locals correctly isolated - see
"Browser verification" below for the full transcript). The profiler and
execution timeline have no
UI yet - see "What a UI pass still needs to do."

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

### A breakpoint-scoping bug found while wiring the UI (fixed)

The first cut of `set_breakpoint` took only a line number - no file/chunk.
`check_stop`'s `Continue` branch matched `bp.line == line` alone, so a
breakpoint set while looking at one open file would also fire the first
time *any other file* reached that same line number. Real for this
product specifically because it supports multi-file projects
(`require()` + a virtual FS): `set_breakpoint("lib.lua", 3)` and
`main.lua`'s own line 3 coinciding is not a corner case, it's routine.
Fixed by giving `Breakpoint`/`set_breakpoint` a `source_id` parameter
(`debug-protocol.md`'s original `Breakpoint` shape already has
`sourceId` - the first draft had just dropped it) and comparing
`(source_id, line)`, not `line` alone. Caught only because a *second* bug
- see below - initially made the regression test for this pass for the
wrong reason, which is worth recording alongside the fix:

**The `check_stop` dedup gate also only tracked line number, not
position**, for an unrelated reason (avoiding double-counting a hit
across the several opcodes one source line compiles to - see the
`drive()`/`check_stop` code comments). Two chunks sharing a line number
tripped this too: reaching line 3 in `main.lua` (no breakpoint there) set
"last checked line" to 3, so arriving at line 3 in `lib.lua` moments
later was wrongly treated as "no change, already checked" and skipped
entirely - the opposite failure mode from the first bug (a real
breakpoint silently *not* firing, rather than a wrong one firing), caught
by the same regression test
(`breakpoints_are_scoped_to_their_own_file_in_a_multi_file_project`) once
both fixes landed together. Both now compare the full `(source, line)`
pair.

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

## Profiler (`crates/lua-vm/src/profiler.rs`)

Built as designed above: `profile(source, chunk_name) -> Vec<FunctionStats>`
runs a program to completion at `step_with_granularity(.., 1)`, tracking a
call stack of `(function_id, start_instruction, child_instructions)` frames
alongside the existing depth-transition detection `session.rs`/
`debug_events.rs` already use. On a call, push a frame and increment that
function's `calls`; on a return, pop it, add its duration to
`total_instructions`, add `duration - child_instructions` to
`self_instructions` (excluding time spent in what it called), and credit
the duration to its caller's `child_instructions`. `totalTime`/`selfTime`
are instruction counts, not wall-clock milliseconds - this codebase already
treats instruction count as its timing proxy everywhere (`MAX_INSTRUCTIONS`,
the runaway-loop guard), and it doesn't vary run to run for reasons that
have nothing to do with the Lua program the way wall-clock time would (host
load, JIT warmup - neither applies to a WASM-hosted tree-walk-adjacent VM
run from a browser tab, but the general instability wall-clock timing has
for this purpose does).

A discovery that came for free from `FunctionPrototype.reference`
(`FunctionRef::Named(name, line) | Expression(line) | Chunk`, populated by
the parser independent of this round's local-variable compiler patch): a
named function (`local function foo()`/`function foo()`) already carries
its declared name at the prototype level, so `function_id` can be
`"chunk:line name"` instead of a register-based placeholder - a nicer
identifier than anything the locals patch produces, for free. 2 tests cover
self-vs-total time attribution across a nested call and repeated calls to
the same function accumulating correctly.

## Execution timeline (`debug_events.rs`)

Phase 3's `run_with_debug_events` already produces the full `line`/`call`/
`return`/`exception`/`terminated` stream; the open design question
`debug-protocol.md` leaves for a timeline feature is retention - an
unbounded `Vec<DebugEvent>` doesn't bound memory for a script that runs
long enough to emit hundreds of thousands of events. `record_timeline(source,
chunk_name, max_events) -> Timeline` (wrapping a new
`run_with_debug_events_capped` that Phase 3's original function is now a
thin wrapper around, at `max_events: usize::MAX`) truncates recording once
`max_events` is reached, exposing `Timeline.truncated: bool` so a caller
knows the recording is partial - **the program still runs to completion (or
its own error/instruction-limit stop) regardless of the cap**, only
*recording* stops early, so `take_output()`/normal execution isn't affected
by how small a UI sets the cap. 2 tests cover truncation kicking in exactly
at the cap and not firing when the recording stays under it. No UI renders
this yet - "per-function timeline visualization" is real design/build work
of its own, not a small addition on top of the capped recording.

## Coroutine debugging (`Executor::debug_thread_stack`, `DebugSession::get_threads`)

**This was initially assessed as out of scope for this pass** (an earlier
revision of this document said as much, reasoning that stepping's call/
return depth tracking would need to become per-thread first). That turned
out to be overly pessimistic once actually attempted - worth correcting the
record on, in the same spirit as this document's other "here's what we
predicted vs. what was actually true" sections:

- `Thread::debug_frames`/`debug_read_register`/`debug_write_register`/
  `debug_lua_frame_depth` (from the Phase 4-7 work above) are already
  methods on `Thread`, not `Executor` - they take *whichever* thread you
  hand them. The only missing piece was a way to get a `Thread` other than
  "the currently running one," which is a single small fork addition:
  `Executor::debug_thread_stack()` (mirroring `current_running_thread`,
  just returning the whole `thread_stack` instead of only its top).
- Stepping needed **no changes at all**. `step_over`/`step_into`/`step_out`
  compare `debug_lua_frame_depth()` snapshots of `current_running_thread()`
  - and a coroutine resume/yield already *is* a change in which thread is
    current, so "the active thread's depth" naturally tracks across
  resume/yield boundaries without any thread-awareness added to the
  stepping logic itself.

What was added: `thread_id: u32` parameters on `get_stack_trace`,
`get_locals`, `evaluate`, and `set_variable` (index into
`debug_thread_stack()`, bottom-to-top, `0` = main - existing single-file
tests all pass `0` unchanged, so this is additive, not a breaking change),
`get_threads() -> Vec<ThreadInfo>` (`id`/`status`, `"running"` for the top
of the stack, `"normal"` - Lua's own term for "resumed something and is
waiting on it" - for everything below it), and `current_thread_id()` so a
breakpoint's condition/log-message expression evaluates against whichever
thread actually hit it, not always the main thread. 2 new tests cover
`get_threads()` reporting both threads with the right statuses, and a
coroutine's stack/locals being independently inspectable from (and
correctly isolated from) the main thread's.

**What this doesn't cover** (see `Executor::debug_thread_stack`'s doc
comment for the full reasoning): only threads on the *active resume
chain* at the current pause point are visible - a coroutine that's been
created but never resumed, or one that has yielded and is sitting
suspended waiting for a future resume, isn't on `thread_stack` and so isn't
in `get_threads()`. Listing *every* coroutine a program has ever created
would need the host to track `coroutine.create` calls itself; piccolo has
no thread registry to read them back from. What's covered is the common
debugging case - pausing while a `coroutine.resume()` call is on the
stack - which is what the browser verification below exercises.

## Browser verification

Phases 4-7's UI was driven end-to-end with Playwright against the actual
dev server (`npm run dev`), not just typechecked:

1. Clicked the Monaco glyph margin to set a breakpoint on the default
   project's `for i = 1, 10 do` line, clicked "🐞 Debug".
2. Confirmed the session paused at that exact line/breakpoint, and that
   the call stack, locals (`greet` as an expandable table, `sum` = 0,
   register-indexed temporaries for compiler internals), and globals
   (the full stdlib table, each entry correctly marked expandable) all
   rendered correctly.
3. Clicked "Step Over", confirmed the line changed and locals updated to
   reflect the new state.
4. Added a watch expression (`sum`) and evaluated a REPL expression
   (`sum + 100`) against the paused frame - both returned correct values.
5. Clicked "Continue", confirmed the program ran to completion and the
   console showed the real, correct output (`Hello, Lua Playground!` /
   `sum 1..10 = 55`).
6. Separately, edited the source to call `error(...)`, confirmed the debug
   status correctly reports the exception, and confirmed a session can be
   stopped and a new one launched cleanly afterward.
7. A coroutine scenario: `coroutine.create`/`resume` with a breakpoint
   inside the coroutine's body. Confirmed the Threads panel appeared
   showing both `main (normal)` and `coroutine #1 (running)`; the default-
   selected (running) thread's call stack and locals showed the
   coroutine's own frame and its local (`x = 42`); switching to `main` in
   the thread selector showed *its* stack (paused at the `coroutine.resume`
   call site) and correctly did *not* show the coroutine's local; and
   continuing ran the program to completion with the correct output
   (`42` / `after resume`).
   (One real gotcha hit while writing this test, worth recording: typing
   Lua source into Monaco via simulated keystrokes is unreliable for
   anything with parens/`end`/indentation - Monaco's autoclose/auto-indent
   mangled a `coroutine.create(function() ... end)` block into invalid
   syntax, which then made the WASM module hit an internal panic
   (`RuntimeError: unreachable`) on the resulting malformed program. Not a
   session.rs bug - confirmed by reproducing the *exact* same source
   string in a native Rust test, which passed cleanly. Seeding
   `localStorage`'s project directly and reloading, rather than typing
   through Monaco, avoided it and is the more reliable approach for any
   future test that needs multi-line/nested Lua source in the editor.)

## What a UI pass still needs to do

Everything above is built and verified. What's left, in descending order of
value:

1. **Profiler UI**: a way to trigger `profile()` and render `FunctionStats`
   (a sortable table is enough - calls/total/self instructions per
   function) - no engine work needed, purely a new panel.
2. **Execution timeline UI**: `record_timeline()` exists; rendering it as a
   per-function timeline (debug-protocol.md's own framing: "primarily an
   educational/visualization feature") is unbuilt design+UI work.
3. **`pause()`**: still not implemented (see "Known gaps" above) - would
   need the Rust side to run in bounded bursts instead of one unbounded
   `continue()` call.
4. Smaller polish: upvalue inspection, function/thread/userdata expansion
   in the variables tree (currently table-only), hover-evaluation in the
   editor (debug-protocol.md mentions it as a third `evaluate()` surface
   alongside watches/REPL, neither of which needed new engine work to add -
   this one doesn't either, it's just unbuilt), and listing coroutines
   that aren't currently on the active resume chain (see the coroutine
   debugging section's "what this doesn't cover").
