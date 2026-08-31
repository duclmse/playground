# Product Brief

## Vision

A browser-based Lua IDE: an editor plus a **real debugger and object
inspector**, not an editor bolted onto `eval()`. The bar is "would this feel
usable to someone who's used VS Code's debugger," not "does it run Lua and show
stdout."

## Target users

- People learning Lua who want to _see_ what a program is doing (locals, call
  stack, stepping) rather than just its printed output.
- Developers embedding Lua elsewhere (game modding, config/scripting layers) who
  want a scratchpad to test snippets against a real Lua VM without installing
  anything.
- Educators demonstrating control flow, recursion, closures, coroutines.

## Goals (v1)

- Lua semantics validated against a conformance suite, not assumed — the
  runtime (Rust + piccolo, see [architecture.md](./architecture.md#runtime-choice-rust--piccolo))
  is a from-scratch implementation rather than the official reference, so
  "real Lua semantics" means "matches the corpus in
  [conformance.md](./conformance.md), with known deviations tracked there,"
  not an unqualified guarantee.
- Line breakpoints, step in/over/out, call stack, and a locals/globals inspector
  that behaves like Chrome DevTools' scope panel.
- Expression evaluation and a REPL, scoped to the currently selected stack
  frame.
- Safe by default: a runaway `while true do end` must not freeze the tab, and
  user code must not reach the filesystem, network, or process.
- Runs entirely client-side. No account, no server, no code leaving the browser.

## Non-goals (v1)

Explicitly deferred — not because they're unimportant, but because pulling any
of these into v1 changes the architecture materially and the core debugger needs
to be proven first:

- **Sharing / persistence beyond the local browser.** v1 is client-only
  (localStorage/IndexedDB); see [risks.md](./risks.md#3-persistence-model) for
  the decision record and what a later "shareable link" feature would require.
- Multi-user / collaborative editing, accounts, or any backend.
- LuaJIT or Lua 5.5 support (the runtime is adapter-based so this is additive
  later, not a rewrite).
- Coroutine debugging, profiling, memory inspection, execution timeline — these
  are real features (see [roadmap.md](./roadmap.md) Phase 8) but they build on a
  stepping/inspection core that doesn't exist yet.
- DAP server exposure (VS Code/Neovim attaching to the in-browser session). The
  internal protocol is modeled after DAP concepts so this stays possible, but it
  is not a v1 deliverable.

## Definition of done for v1 (MVP-1)

See [roadmap.md](./roadmap.md#mvp-1-capability-cut-line) for the exact
10-capability list. Informally: a user can write multi-file Lua, set a
breakpoint, hit it, step through code, inspect locals/globals including nested
tables, and evaluate an expression in the paused frame — all without the tab
freezing on bad input.

## Constraints

- Browser-only execution environment; no real filesystem — a virtual FS backs
  `require()`.
- Lua execution happens in a Web Worker, never the UI thread (see
  [architecture.md](./architecture.md#web-worker-architecture)).
- Instruction-count limits guard against infinite loops (see
  [debug-protocol.md](./debug-protocol.md#instruction-limits--infinite-loop-protection)).
