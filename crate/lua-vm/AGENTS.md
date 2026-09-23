# crates/lua-vm

The browser runtime: a `wasm-bindgen` layer over `crates/vm` (the vendored
Piccolo fork), compiled for `apps/web` via `packages/lua-runtime`. Builds as
both `cdylib` (wasm target) and `rlib` (so `crates/dap-server` can link it
natively — see `docs/dap-server.md`). Sandboxed Lua execution: host
filesystem, OS, and native loading capabilities must stay explicit and absent
unless a task calls for adding them.

Architecture and design docs: `docs/README.md`, `docs/architecture.md`,
`docs/debug-protocol.md`, `docs/risks.md`.

## Layout

- `lib.rs` — top-level `#[wasm_bindgen]` entry points (`run`, `run_named`,
  ...), plus the fuel/instruction-limit loop (`run_to_completion`):
  `FUEL_PER_STEP` (4096) and `MAX_INSTRUCTIONS` (10,000,000) bound every
  execution so a runaway `while true do end` terminates instead of hanging
  the worker — see `docs/debug-protocol.md`'s "Instruction limits" section
  before changing either constant.
- `debug_events.rs` — event-based execution trace (`run_with_debug_events`,
  `Timeline`, `record_timeline`) for the profiler/timeline UI.
- `profiler.rs` — sampling/aggregation on top of debug events
  (`profile`, `FunctionStats`).
- `session/` — `DebugSession`, the step-debugging engine driven by both the
  browser (via wasm) and `dap-server` (natively):
  - `mod.rs` — session state machine.
  - `execution.rs` — `launch`/`continue_burst`/stepping, built on
    `crates/vm`'s patched `Executor::step_with_granularity`.
  - `breakpoints.rs`, `evaluate.rs`, `inspector.rs`, `memory.rs` — breakpoint
    management, watch/eval expressions, variable inspection, memory stats.
  - `types.rs` — `#[wasm_bindgen]`-exported value types (`Variable`,
    `StackFrame`, `ThreadInfo`, `Breakpoint`, ...) shared by both consumers.
  - `registry.rs` — session/handle bookkeeping.
  - `tests.rs` — in-crate `DebugSession` unit tests.

## Commands

```sh
cargo test --manifest-path crates/lua-vm/Cargo.toml   # unit tests + tests/conformance.rs
scripts/test.sh                                        # this + corpus-manifest check + web build if deps exist
scripts/build-wasm.sh                                   # regenerate packages/lua-runtime/pkg after changes here
```

`tests/conformance.rs` runs `conformance/fixtures/*.lua` against
`conformance/expected/*.expected` (ground truth captured from the system Lua
5.5.1 interpreter — see `docs/conformance.md`). `scripts/build-wasm.sh`
requires the `wasm32-unknown-unknown` target and the `wasm-bindgen` CLI; its
output (`packages/lua-runtime/pkg`) is generated — don't hand-edit it.

## Change guidelines

- This crate is the shared engine behind two independent consumers (the wasm
  build for `apps/web`, and the native `rlib` build for `dap-server`) — a
  change here affects both; check `docs/dap-server.md` for why the native
  build works unmodified before adding anything `wasm32`-only without a
  `cfg` gate.
- `DebugSession` and its value types intentionally mirror what
  `crates/vm`'s patched introspection exposes (`current_running_thread`,
  `step_with_granularity`, `debug_snapshot`, `debug_lua_frame_depth`,
  `line_for_pc`); read `crates/vm/README.md`'s fork notice and
  `docs/risks.md` §1 before assuming new introspection is available upstream.
- Add or update conformance fixtures (`conformance/fixtures` +
  `conformance/expected`) alongside behavior changes to Lua semantics; keep
  ground truth generated from the real Lua interpreter, not hand-written.
- Preserve the fuel/instruction-limit invariant in `lib.rs`'s execution loop
  — it's what keeps a runaway script from freezing the browser tab.
