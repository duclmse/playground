# crates/dap-server

Native Debug Adapter Protocol (DAP) server over `lua-vm::DebugSession` — the
same debug engine the browser debugger drives via wasm, here linked as a
plain native `rlib` (no wasm involved). For VS Code, or any DAP client. A
wholly separate consumer: nothing in `apps/web` talks to it, and nothing in
`apps/web` needed to change for it to exist. Full design doc:
`docs/dap-server.md` — read it before touching threading, id encoding, or
`setBreakpoints` reconciliation; the sections below summarize it.

## Layout

- `main.rs` — entry point; v1 request scope (`initialize`, `launch`,
  `setBreakpoints`, `configurationDone`, `threads`, `stackTrace`, `scopes`,
  `variables`, `evaluate`, `setVariable`, `continue`, `next`, `stepIn`,
  `stepOut`, `pause`, `disconnect`/`terminate`); owns the reader-thread /
  main-thread split.
- `session.rs` — `AdapterSession`: wraps one `lua_vm::DebugSession`
  (constructed by `launch`, not before) plus DAP-only bookkeeping `lua_vm`
  doesn't track itself — a per-source breakpoint list for
  `setBreakpoints`' bulk-replace reconciliation, and the launched program's
  chunk name (breakpoint `source_id` must match it exactly).
- `convert.rs` — DAP id-space encoding: `frameId` packs `(thread_id,
  frame_index)` since `StackFrame::index` alone isn't unique across threads;
  `variablesReference` reserves a top bit to keep synthetic scope references
  (Locals/Upvalues/Globals) from colliding with `lua_vm`'s real
  `ObjectRegistry` ids.
- `framing.rs` — DAP's `Content-Length`-framed stdio message I/O.

## Threading model (do not change casually)

One reader thread does blocking stdio reads and posts parsed requests to an
`mpsc` channel. The **main thread** owns the `DebugSession` for the
adapter's entire life and is the only thing that ever writes to stdout —
required, not just convenient, because piccolo/`gc-arena`'s GC types are
`!Send`. `continue` is acknowledged immediately, then driven in
`BURST_INSTRUCTIONS`-sized bursts via `continue_burst` (mirrors
`apps/web/src/debug-session.ts`'s own loop); between bursts the main thread
does a non-blocking `try_recv` poll, which is what makes `pause` responsive
and lets any other request (`evaluate`, `variables`, a live
`setBreakpoints`) be handled immediately mid-run rather than queued until
the next real stop. `next`/`stepIn`/`stepOut` are *not* burst-looped — they
call the underlying step method once, synchronously, matching the browser
client's behavior (a step into an unbounded call is just as uninterruptible
here as there).

## Commands

```sh
cargo test --manifest-path crates/dap-server/Cargo.toml
cargo run --manifest-path crates/dap-server/Cargo.toml   # speaks DAP over stdio; drive it from a DAP client, not a terminal
```

## Change guidelines

- This crate only works unmodified because `lua-vm` builds both `cdylib` and
  `rlib`, and `wasm-bindgen`'s proc macros no-op on a non-wasm32 target —
  don't add anything to `lua-vm` that's implicitly `wasm32`-only without a
  `cfg` gate, or this crate silently breaks.
- Keep `setBreakpoints` bulk-replace semantics: remove everything previously
  tracked for a source, then re-add the full new list (including
  condition/hitCondition/logMessage, always re-applied rather than diffed).
- New request handlers should map onto an existing `DebugSession` method
  where possible (v1's design goal); if one doesn't exist, that's new
  engine-side work in `crates/lua-vm`, not something to fake here.
- Don't let any code outside `main`'s single owning thread touch
  `DebugSession` — that's the one invariant the whole threading model rests
  on.
