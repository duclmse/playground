# Lua Playground — Docs

A browser-based Lua playground with a real debugger and object inspector,
built on a real Lua VM compiled to WebAssembly rather than a hand-rolled
TypeScript interpreter.

This directory was restructured from a single design narrative
(`browser-based Lua playground.md`) into a standard doc set. That original
file is kept as-is — it's the source rationale and contains useful
diagrams/examples — but the documents below are the ones to build from; they
correct a few assumptions the original made and fill in gaps it left open.

**Runtime decision:** the engine is **Rust + piccolo, compiled to
WebAssembly** — not Wasmoon/official C Lua, which the original doc and an
earlier version of this doc set assumed. See
[architecture.md](./architecture.md#runtime-choice-rust--piccolo) for why,
and [risks.md](./risks.md#decision-log) for what that changed.

## Reading order

1. [product-brief.md](./product-brief.md) — what we're building, for whom, and what v1 explicitly excludes.
2. [architecture.md](./architecture.md) — layers, project/package structure, runtime choice, worker model.
3. [debug-protocol.md](./debug-protocol.md) — the DebugSession API, state machine, stepping algorithms, value/inspector model.
4. [roadmap.md](./roadmap.md) — phased delivery plan and the MVP-1 feature cut line.
5. [risks.md](./risks.md) — open questions and technical risks that need resolving before or during the phases above, including the one that should be spiked first.
6. [conformance.md](./conformance.md) — the Lua conformance test plan, needed because the runtime is no longer the official reference implementation.

## Status

MVP-1 slice implemented: Phases 0-2 of the roadmap (conformance harness,
piccolo-backed Lua runtime compiled to WASM, full playground UI — Monaco
editor, multi-file virtual filesystem, console) are built and verified
end-to-end, including in a real browser. Phases 3-8 (the debugger) are
explicitly deferred — `risks.md` §1 is resolved and concluded that
piccolo's public API has no frame/locals introspection surface, so a
debugger needs a piccolo fork (or upstream contribution) before that work
can start; see `risks.md` §1 and `roadmap.md` for the consequences.

What exists:

- `crates/lua-vm` — the piccolo-backed runtime (`run`/`run_named`/
  `run_project`), with a sandboxed `print`, small host-added stdlib
  extensions (`table.insert`/`concat`/`sort`, `xpcall`) that piccolo itself
  doesn't ship, a `require()` backed by an in-memory virtual filesystem for
  multi-file projects, and a host-side fuel-stepped execution loop that caps
  a run at 10M instructions so a runaway `while true do end` errors out
  instead of freezing the tab. Unit-tested and conformance-tested.
- `conformance/fixtures` — a 10-fixture corpus diffed against real Lua
  5.5.1 output; see [conformance.md](./conformance.md)'s known-deviations
  ledger for what piccolo actually diverges on (confirmed by source
  inspection, not guesswork): no arithmetic/comparison metamethods, no
  string pattern matching, no string metatable.
- `packages/lua-runtime` — the `wasm-bindgen`-generated npm package wrapping
  `crates/lua-vm`.
- `apps/web` — a Vite/React playground UI that runs the VM in a Web Worker
  (per architecture.md's worker-isolation requirement), with a Monaco editor,
  a file-tree sidebar for a multi-file virtual project (add/rename/delete
  files, choose the entry file), and client-only save/load via
  `localStorage` (per `risks.md` §4's persistence decision — no backend, no
  shareable links in v1).
