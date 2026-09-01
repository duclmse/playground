# Lua Playground - Docs

A browser-based Lua playground with a real debugger and object inspector,
built on a real Lua VM compiled to WebAssembly rather than a hand-rolled
TypeScript interpreter.

This directory was restructured from a single design narrative
(`browser-based Lua playground.md`) into a standard doc set. That original
file is kept as-is - it's the source rationale and contains useful
diagrams/examples - but the documents below are the ones to build from; they
correct a few assumptions the original made and fill in gaps it left open.

**Runtime decision:** the engine is **Rust + piccolo, compiled to
WebAssembly** - not Wasmoon/official C Lua, which the original doc and an
earlier version of this doc set assumed. See
[architecture.md](./architecture.md#runtime-choice-rust--piccolo) for why,
and [risks.md](./risks.md#decision-log) for what that changed.

## Reading order

1. [product-brief.md](./product-brief.md) - what we're building, for whom, and what v1 explicitly excludes.
2. [architecture.md](./architecture.md) - layers, project/package structure, runtime choice, worker model.
3. [debug-protocol.md](./debug-protocol.md) - the DebugSession API, state machine, stepping algorithms, value/inspector model.
4. [roadmap.md](./roadmap.md) - phased delivery plan and the MVP-1 feature cut line.
5. [risks.md](./risks.md) - open questions and technical risks that need resolving before or during the phases above, including the one that should be spiked first.
6. [conformance.md](./conformance.md) - the Lua conformance test plan, needed because the runtime is no longer the official reference implementation.
7. [phase-4-8-implementation.md](./phase-4-8-implementation.md) - what's actually built vs. designed-but-deferred for the debugger engine and its Phase 8 advanced features, and three places the implementation diverged from this doc set's original design once it was actually built.

## Status

MVP-1 slice implemented: Phases 0-2 of the roadmap (conformance harness,
piccolo-backed Lua runtime compiled to WASM, full playground UI - Monaco
editor, multi-file virtual filesystem, console) are built and verified
end-to-end, including in a real browser. Phase 3 (debug instrumentation,
risk-spike scope) is also done: `risks.md` §1 concluded piccolo's public API
has no frame/locals introspection surface (and, once the instrumentation
loop was actually built, that its `step()` has no way to shrink its internal
64-opcode batch size either), so `crates/vm` is now a vendored, patched fork
of piccolo 0.3.3 that adds both.

Phases 4-7 (breakpoints, stepping, call stack, inspector, expression
evaluation) are fully built and **verified end-to-end in a real browser**:
a `DebugSession` engine (`crates/lua-vm/src/session.rs`), a typechecked
TypeScript worker-protocol client (`apps/web/src/debug-session.ts`), and a
React UI (Monaco breakpoint gutter, call stack/locals/globals panels with
lazy table expansion, watch list, frame-scoped REPL) driven end-to-end with
Playwright against the running dev server. Phase 8's breakpoint-shaped
features (conditional/hit-count/logpoints, exception-as-stop) and its
profiler and execution timeline are engine-complete and tested but have no
UI yet. Phase 8's coroutine debugging is designed but not built - real,
non-trivial engine work (making call-stack/depth tracking thread-aware) that
was deliberately not squeezed in alongside everything else, to avoid
regressing the now-verified single-threaded debugger. See
[phase-4-8-implementation.md](./phase-4-8-implementation.md) for the full
breakdown, the engine-side findings that changed the design along the way,
the Playwright verification transcript, and exactly what's left.

What exists:

- `crates/vm` - a vendored fork of piccolo 0.3.3 (Cargo package name `vm`,
  not `piccolo`), patched to expose read-only step-debugging introspection
  upstream keeps `pub(super)` or hardcoded, plus a compiler patch retaining
  named-local debug info piccolo otherwise discards after compilation:
  `Executor::current_running_thread`, `Executor::step_with_granularity`
  (parameterizes the internal opcode-batch size `step()` hardcodes to 64),
  `Thread::debug_snapshot`, `Thread::debug_lua_frame_depth`,
  `Thread::debug_frames` (the full call stack), `Thread::debug_read_register`
  / `debug_write_register`, `FunctionPrototype::line_for_pc` /
  `local_name_at`. See `crates/vm/README.md`'s fork notice,
  `risks.md` §1, and [phase-4-8-implementation.md](./phase-4-8-implementation.md)
  for the full patch list and why each piece was needed.
- `crates/lua-vm` - the piccolo-backed runtime (`run`/`run_named`/
  `run_project`), with a sandboxed `print`, small host-added stdlib
  extensions (`table.insert`/`concat`/`sort`, `xpcall`) that piccolo itself
  doesn't ship, a `require()` backed by an in-memory virtual filesystem for
  multi-file projects, and a host-side fuel-stepped execution loop that caps
  a run at 10M instructions so a runaway `while true do end` errors out
  instead of freezing the tab. Also `debug_events.rs` (Phase 3's debug
  instrumentation, `debug_events(source) -> DebugEvent[]`, plus Phase 8's
  capped `record_timeline`), `session.rs` (Phases 4-8's `DebugSession`:
  breakpoints including conditional/hit-count/logpoints, stepping, call
  stack, locals/globals/table inspection, frame-scoped `evaluate`/
  `setVariable`), and `profiler.rs` (Phase 8's `profile()` ->
  `FunctionStats`). 38 unit tests plus the conformance suite, all against
  the real engine - see
  [phase-4-8-implementation.md](./phase-4-8-implementation.md) for what
  `DebugSession` does and doesn't cover.
- `conformance/fixtures` (`.lua` source) and `conformance/expected`
  (`.expected` output, paired by matching basename) - a 10-fixture corpus
  diffed against real Lua 5.5.1 output; see
  [conformance.md](./conformance.md)'s known-deviations ledger for what
  piccolo actually diverges on (confirmed by source
  inspection, not guesswork): no arithmetic/comparison metamethods, no
  string pattern matching, no string metatable.
- `packages/lua-runtime` - the `wasm-bindgen`-generated npm package wrapping
  `crates/lua-vm`, including the `DebugSession` class.
- `apps/web` - a Vite/React playground UI that runs the VM in a Web Worker
  (per architecture.md's worker-isolation requirement), with a Monaco editor,
  a file-tree sidebar for a multi-file virtual project (add/rename/delete
  files, choose the entry file), and client-only save/load via
  `localStorage` (per `risks.md` §4's persistence decision - no backend, no
  shareable links in v1). Also `debug-protocol.ts`/`debug-session.ts`/the
  worker's debug-message handling (a complete, typechecked TypeScript
  `DebugSession` client for the Rust engine above) and `DebugPanel.tsx`/
  `VariablesTree.tsx` (the debugger UI: Monaco breakpoint gutter, call
  stack/locals/globals/watch/REPL panels) - driven end-to-end with
  Playwright against the running dev server, not just typechecked. See
  [phase-4-8-implementation.md](./phase-4-8-implementation.md)'s "Browser
  verification" and "What a UI pass still needs to do" sections.
