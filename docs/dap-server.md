# DAP Server

`crates/dap-server` is a native Debug Adapter Protocol (DAP) server over
`lua_vm::DebugSession` - the same debug engine the browser's web debugger drives
via wasm (see [debug-protocol.md](./debug-protocol.md)), here linked as a plain
native `rlib` dependency instead. It's a wholly separate consumer of
`lua-vm`/`vm`: nothing in `apps/web` talks to it, and nothing in `apps/web`
needed to change for it to exist.

## Why this works unmodified

`DebugSession` and its value types (`Variable`, `StackFrame`, `ThreadInfo`, ...)
carry `#[wasm_bindgen]` attributes, but those attributes don't stand in the way
of a native build: `crates/lua-vm/Cargo.toml` already builds both `cdylib` (for
wasm) and `rlib` (for anything else) crate types, and `wasm-bindgen` is an
_unconditional_ dependency of that crate (only its `getrandom` shims are gated
to `wasm32`) - so its proc macros simply no-op down to plain Rust items on a
native target. Confirmed empirically: `cargo build`/`cargo test` for
`dap-server` link and call every `DebugSession` method (`launch`,
`continue_burst`, `get_locals`, ...) as ordinary native Rust, no `cfg_attr`
gating needed anywhere in `lua-vm`.

## Threading model

One reader thread does blocking stdio reads (`framing::read_message`) and posts
parsed requests to an `mpsc` channel. The **main thread** owns the
`DebugSession` for the adapter's entire life and is the only thing that ever
writes to stdout. This is required, not just simple: piccolo/`gc-arena`'s GC
types are `!Send`, so `DebugSession` can never hop threads.

A `continue` request is acknowledged immediately (DAP expects a prompt response,
with the real outcome signaled later via a `stopped`/`terminated` event), then
driven in `BURST_INSTRUCTIONS`-sized bursts via `continue_burst` - mirroring
`apps/web/src/debug-session.ts`'s own `continue()` loop. Between bursts, the
main thread does a **non-blocking** poll of the request channel (`try_recv`),
which is what makes `pause` responsive without a second thread ever touching
`DebugSession`. Any _other_ request that arrives mid-run (`evaluate`,
`variables`, a live `setBreakpoints`) is handled immediately from inside that
same poll loop, right where it's found - the paused-between-bursts state is
exactly as valid a snapshot as a real breakpoint stop, so there's no reason to
make the client wait for a "real" stop first. This resolves in favor of _live
interleaving_ the open question the original plan flagged (interleave vs.
queue-until-next-stop) - it fell out of the single-thread design almost for
free.

`next`/`stepIn`/`stepOut` are **not** burst-looped - they call the underlying
`step_over`/`step_into`/`step_out` once, synchronously, exactly like the browser
client does. A step that runs into an unbounded inner call is just as
uninterruptible here as it already is in the browser; this matches existing
behavior rather than inventing new pausable-stepping semantics nothing else in
the system has.

## ID encoding

DAP needs two id spaces `lua_vm` has no direct equivalent of - see
`convert.rs`'s doc comment for the full reasoning, summarized here:

- **`frameId`** (used in `stackTrace` results and as `scopes`'/`evaluate`'s
  `frameId` argument) must be unique across _every_ thread, but
  `StackFrame::index` is only unique within one thread's own stack (frame 0
  exists in every thread). `encode_frame_id`/`decode_frame_id` pack
  `(thread_id, frame_index)` into one id.
- **`variablesReference`** is one shared id space covering both "a
  table/userdata you can expand" (already numbered by `lua_vm`'s
  `ObjectRegistry`, small sequential `u32`s from 0) and "a scope container"
  (Locals/Upvalues/Globals for some frame, which `lua_vm` has no id for at all).
  `encode_scope_ref`/`decode_scope_ref` reserve the top bit for synthetic scope
  references so the two kinds can never collide with a real registry id.

## `setBreakpoints` reconciliation

DAP sends the _entire_ desired breakpoint list for a source on every call ("bulk
replace"); `lua_vm` only has imperative `set_breakpoint`/ `remove_breakpoint`.
`AdapterSession` keeps its own `source -> [(line, internal_id)]` map per source
and, on each `setBreakpoints` call, removes everything previously tracked for
that source and re-adds the new list - see `session.rs`'s
`handle_set_breakpoints`. Condition/hitCondition/ logMessage are always
re-applied rather than diffed too, since clearing them with `None` is just as
cheap as diffing would be.

`source_id`/DAP `Source.path` reconciliation: `lua_vm` matches a breakpoint
against the _chunk name_ it was compiled with, which `launch` sets to the
program file's basename (see below) - so every `setBreakpoints` call reduces
`source.path`'s basename the same way before touching `lua_vm`.

## v1 scope

**In v1** (every one of these maps directly onto an existing `DebugSession`
method - no new engine-side work was needed): `initialize`, `launch`,
`setBreakpoints` (including conditional/hit-count/logpoint variants),
`configurationDone`, `threads`, `stackTrace`, `scopes`, `variables`, `evaluate`,
`setVariable`, `continue`, `next`, `stepIn`, `stepOut`, `pause`,
`disconnect`/`terminate`. Plus two small adapter-specific extensions (DAP
explicitly allows these) exposing this project's memory-inspection feature:
`memoryStats`/`forceGc`.

**`launch` is single-file only**: its arguments are
`{"program": "<path to a .lua file>"}`, read from the local filesystem and
compiled with `lua_vm`'s plain `DebugSession::launch` (not `launch_project`) -
no `require()`/ multi-file virtual-FS support, unlike the browser client. The
chunk name (and therefore every breakpoint's `source_id`) is the file's
basename.

**Explicitly deferred** (not implemented, no partial support): `attach` (no
remote/existing-process concept exists - every session is adapter-launched via
`launch`), multi-session/multi-client support (one process = one
`DebugSession`), exception breakpoints (`setExceptionBreakpoints` - `lua_vm`
only has a blanket "stopped on runtime error", no per-exception filtering),
`restart`, data breakpoints, `disassemble`/`readMemory`, `stepBack`/reverse
debugging, DAP `completions`.

## Testing

`crates/dap-server/tests/stdio_smoke.rs` spawns the built binary as a real
subprocess and drives it through the full lifecycle over actual framed stdio
JSON - `initialize` → `initialized` event → `setBreakpoints` →
`configurationDone` → `stopped` (breakpoint hit) → `stackTrace`/`scopes`/
`variables`/`evaluate`/`setVariable` → `continue` → `output`/`terminated` →
`disconnect`, plus a breakpoint-reconciliation test and a tight-loop `pause`
test proving the non-blocking mid-burst channel poll actually works under a
genuine `while true do end`. Run with `cargo test -p dap-server` (or
`cd crates/dap-server && cargo test`).

## Trying it from VS Code

No custom extension is required for a first pass - VS Code can spawn a DAP
server executable directly from a stub `package.json` contributing one
`debuggers` entry, plus a workspace `launch.json`:

```json
// .vscode/extensions/lua-dap-stub/package.json (a minimal, unpublished stub)
{
  "name": "lua-dap-stub",
  "engines": {"vscode": "^1.60.0"},
  "contributes": {
    "debuggers": [
      {
        "type": "lua-dap",
        "label": "Lua (dap-server)",
        "program": "${workspaceFolder}/crates/dap-server/target/debug/dap-server"
      }
    ]
  }
}
```

```json
// .vscode/launch.json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "lua-dap",
      "request": "launch",
      "name": "Debug Lua file",
      "program": "${file}"
    }
  ]
}
```

Build the binary first (`cargo build -p dap-server`), open a `.lua` file, set a
breakpoint in the gutter, and hit Run and Debug. This is a documentation-only
sketch (no such stub is committed) - a real extension with proper packaging,
syntax highlighting, and a breakpoint-gutter contribution is future work, not
required to prove the adapter itself works against a real editor.
