# Roadmap

Eight phases, plus an up-front conformance harness. Phases 1–2 produce a
usable (non-debugging) playground; Phase 5 is the first point where a *real
debugger* exists; Phases 6–8 build out the inspector, evaluation, and
advanced features on top of it.

Before starting **Phase 3**, resolve
[risks.md §1](./risks.md#1-piccolo-debug-introspection-surface-the-central-risk) -
it determines how much of Phase 3's estimate is "instrument piccolo's
existing frame/step API" vs "extend or fork piccolo to expose what's
missing."

## Phase 0 - Conformance harness

Stand up [conformance.md](./conformance.md)'s test corpus and CI gate
*before* writing interpreter-dependent debugger code on top of piccolo.
Because piccolo is not a certified-conformant Lua implementation, every
later phase is implicitly trusting it to behave correctly on the language
features that phase exercises - better to know where the gaps are before
Phase 3's stepping logic gets blamed for what's actually a VM semantics
bug.

- Vendor/curate the fixture corpus (see conformance.md for sourcing).
- Wire up `crates/lua-vm` → run corpus → pass/fail report in CI.
- Publish the initial known-deviations ledger (even if it starts as "TBD,
  not yet run against the full corpus").

## Phase 1 - Lua runtime

`TypeScript → wasm-bindgen → piccolo (Rust, compiled to WASM) → execute code`

- Build `crates/lua-vm`: piccolo integration, Lua engine lifecycle,
  `execute()`, stdout capture, error propagation, virtual files.
- `wasm-bindgen`/`wasm-pack` build pipeline producing the artifact
  `@lua-playground/runtime` wraps.
- Deliverable: `await runtime.execute('print("Hello Lua")')`.

## Phase 2 - Playground

Monaco + Run button + Console + file tree.

- Editor, syntax highlighting, run, output, runtime errors, multiple files,
  save/load project (client-only for v1 - see
  [product-brief.md](./product-brief.md#non-goals-v1)).
- **Usable Lua playground at this point**, no debugger yet.

## Phase 3 - Debug instrumentation

Instrument piccolo's fuel-stepped execution loop to produce CALL / RETURN /
LINE / COUNT-equivalent events (`LuaDebugHook`, `DebugEvent` - see
[debug-protocol.md](./debug-protocol.md#debug-events)). Deliverable: observe
the line-by-line event stream without any UI debugging yet.

**This is the risk-spike phase.** See risks.md §1 before estimating it -
concretely, get one `step()` call to report "line changed" and expose one
local variable's value before scoping the rest of this phase.

## Phase 4 - Breakpoints

Set/remove/lookup/pause/resume, plus Monaco integration (red dot =
breakpoint, yellow arrow = current line).

## Phase 5 - Call stack + stepping

`stackTrace`, `stepInto`, `stepOver`, `stepOut`, `continue`, `pause`. **First
point where this is a real debugger.**

## Phase 6 - Inspector

The value model (`LuaValue`, `LuaTable`, `LuaFunction`, `LuaThread`,
`LuaUserdata`), then locals/globals/upvalues/tables/metatables/references
with lazy expansion. Full spec in
[debug-protocol.md](./debug-protocol.md#value--inspector-model). Cross-check
metatable/metamethod coverage against
[conformance.md](./conformance.md) as it's built - this is where a piccolo
semantics gap is most likely to surface as an inspector bug.

## Phase 7 - Expression evaluation

`evaluate()`, watch expressions, REPL, `setVariable`, hover evaluation.
**This is where the playground becomes genuinely useful**, not just
observable.

## Phase 8 - Advanced debugger

Conditional breakpoints, hit counts, logpoints, exception breakpoints,
coroutine debugging, function breakpoints, instruction limits, profiling,
memory inspection, execution timeline. See
[debug-protocol.md](./debug-protocol.md#advanced-coroutines-phase-8) for the
design of the coroutine/profiler/timeline pieces, and verify piccolo's
coroutine model against conformance.md before committing to the coroutine
debugging design.

---

## MVP-1 capability cut line

The 10 capabilities to build before anything else, roughly Phases 1–7's
non-advanced subset:

| #   | Feature                       | Phase |
| --- | ------------------------------ | ----- |
| 1   | Conformance harness (baseline) | 0     |
| 2   | Lua-via-piccolo WASM runtime    | 1     |
| 3   | Monaco editor                   | 2     |
| 4   | Console/stdout                  | 2     |
| 5   | Virtual filesystem               | 2     |
| 6   | Line breakpoints                 | 4     |
| 7   | Continue/pause                   | 4     |
| 8   | Step in/over/out                 | 5     |
| 9   | Call stack                       | 5     |
| 10  | Local/global inspector            | 6     |
| -   | Expression evaluation (stretch, Phase 7) | 7 |

Everything in Phase 8, plus metatables/upvalues detail beyond basic locals,
comes after this line.

## Architectural non-negotiables

- Keep the Lua runtime/debugger in a **Web Worker** from Phase 1 onward,
  communicating with the UI through the typed protocol in
  [debug-protocol.md](./debug-protocol.md#worker-message-protocol). This is
  no longer required for pause/resume (piccolo's fuel-stepped model doesn't
  need `Atomics`), but it still isolates expensive computation from the UI
  thread and is much more expensive to retrofit later than to build in from
  the start.
- Keep [conformance.md](./conformance.md) running in CI from Phase 0 onward
  and treat a regression there as a blocking bug, not a follow-up - it's
  the only thing standing in for "trust the reference implementation" now
  that the reference implementation isn't what's running.
