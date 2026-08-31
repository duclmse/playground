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

Pre-implementation. No code has been written yet. `risks.md` §1 (piccolo's
debug-introspection surface) should be spiked before committing to Phase 3
of the roadmap — it's the one architectural assumption in this doc set that
hasn't been proven against real code.
