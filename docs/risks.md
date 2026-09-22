# Risks & Open Questions

Gaps and unverified assumptions found while reviewing the plan. Ordered roughly
by how much they'd cost to discover late.

## Decision log

- **Runtime engine changed from Wasmoon (official C Lua via WASM) to Rust +
  piccolo (pure-Rust Lua-like VM via WASM).** Rationale in
  [architecture.md](./architecture.md#historicalcurrent-browser-runtime-rust--piccolo). This
  resolved what were risks §1 and §2 against the Wasmoon design (the
  debug-binding gap and the `SharedArrayBuffer`/cross-origin-isolation
  requirement - both marked resolved below) and introduced a new central risk in
  their place: piccolo's debug-introspection surface, and Lua semantic
  conformance more broadly (mitigated by [conformance.md](./conformance.md), a
  new dedicated doc).

## 1. piccolo debug-introspection surface (the central risk) - RESOLVED: forked, Phase 3 spike complete

Spiked directly against piccolo 0.3.3 source
(`~/.cargo/registry/.../piccolo-0.3.3/src/thread/{executor,thread}.rs`,
`src/closure.rs`). Verdict: **piccolo's public API does not expose enough
introspection to build the debugger without forking it.** This was the central
unknown; it's no longer unknown, and it's the reason this MVP slice implements
Phases 0–2 (real runtime + playground UI) and defers Phases 3–8 (the debugger)
rather than attempting them on unmodified piccolo.

**What's confirmed:**

- The fuel-stepped model itself is real and exactly as documented:
  `Executor::step(self, ctx: Context<'gc>, fuel: &mut Fuel) -> bool`, driven in
  a loop by `Lua::finish()`. `Fuel::interrupt()` lets a host force early return.
  This part of the architecture is sound.
- `Closure::load(ctx, name, source)` /
  `FunctionPrototype::compile(ctx, source_name, source)` take an explicit chunk
  name, confirming risks.md §6 (source mapping) is fine - piccolo does have
  Lua's `@path`-style chunk-naming equivalent, threaded through to
  `FunctionPrototype::chunk_name` and (via
  `opcode_line_numbers: Box<[(usize, LineNumber)]>`) a real pc→line table per
  prototype.
- **But** `thread::thread::Frame` (the enum holding `bottom`/`base`/`pc`/
  `stack_size` per Lua call frame), `ThreadState` (the `frames`/`stack`/
  `open_upvalues` vectors), `LuaFrame`, and `LuaRegisters` are all `pub(super)`
  - module-private to `piccolo::thread`. `Thread`'s only public methods are
  `new`, `start`, `take_result`, `resume`, `resume_err`, `mode`, `reset` - none
  expose the call stack, frame locals, or current pc. `Executor` itself exposes
  no per-frame accessor at all.
- The one public introspection hook that exists, `Execution::upper_lua_frame()`
  (`chunk_name`/`current_function`/ `current_line` of the frame _above_ the
  current callback), only fires from inside a `Callback`/`Sequence` being run by
  the VM - it cannot be polled by host code between `step()` calls, which is the
  shape the debugger actually needs (pause after N instructions, then ask "what
  line, what locals, what call stack").

**Consequence for the roadmap:** Phase 3 ("instrument piccolo's existing
frame/step API") as originally scoped is not possible against piccolo 0.3.3
unmodified - option (a) from the old "likely outcome" list is off the table.
It's (b) fork piccolo to make `Frame`/`ThreadState` (or a purpose-built
read-only view of them) `pub`, or (c) upstream that as a contribution and depend
on a patched/forked crate in the meantime. Either way this is real, scoped Rust
work against piccolo's internals, not a thin instrumentation wrapper - treat
Phase 3's estimate accordingly, and don't start it without first deciding
fork-and-vendor vs. upstream-and-wait.

**What this MVP does instead:** ships Phases 0–2 (conformance harness, real
piccolo-backed `execute()`, Monaco-less-for-now playground UI with Run/Console)
against piccolo's public API as-is - none of that needs frame introspection. The
debugger (Phases 3–8) stays explicitly blocked on the fork-vs-upstream decision
above and is not part of this MVP slice.

**Historical decision (superseded by retirement of `crates/vm`): fork and
vendor (option (b)).** Upstreaming was
rejected for this project's timeline - piccolo's last release is 0.3.3 and
there's no indication a maintainer would review/merge a debug-introspection
PR on a useful schedule, and "wait on upstream" would block Phase 3
indefinitely on a factor this project doesn't control. Vendored fork lives at
`crates/vm` (Cargo package name `vm`, not `piccolo` - see
`crates/vm/README.md`'s fork notice), pinned to piccolo 0.3.3 plus a small,
fully-commented patch set; `crates/lua-vm` depends on it via
`vm = { path = "../vm" }`.

**What the fork actually had to add** (more than the original spike
predicted): read-only accessors matching what §1 found missing -
`Executor::current_running_thread()`, `Thread::debug_snapshot()` (chunk
name/current line/register-0 value of the top Lua frame),
`Thread::debug_lua_frame_depth()` (Lua-only call depth, ignoring host
Callback/Sequence frames), and `FunctionPrototype::line_for_pc()`. All are
new methods; no existing signatures changed, so this patch set doesn't drift
far from upstream and should stay easy to rebase if piccolo cuts a new
release.

**A second, more consequential finding surfaced only by actually building the
instrumentation loop:** shrinking `Fuel` passed to `Executor::step()` does
*not* give per-line (or even per-instruction) granularity, which the original
spike's phrasing ("pause after N instructions") assumed it would. `step()`
hardcodes a 64-opcode batch (`VM_GRANULARITY`) for its internal Lua-frame
run, which only stops early on a call/return - not a line boundary - and
`fuel.consume()` charges a flat 4-per-outer-loop-iteration regardless of how
many opcodes that batch actually ran. Net effect: straight-line code (no
calls) under 64 opcodes runs to completion inside a *single* `step()` call no
matter how small `Fuel` is, so a host polling between `step()` calls would
silently skip every intermediate line. Fixed by adding
`Executor::step_with_granularity(ctx, fuel, granularity)` - `step()` itself
is now a thin wrapper calling it with `64` (byte-for-byte unchanged
behavior for every existing caller: `execute()`/`execute_project()` still use
`step()`), and the debug instrumentation calls it with `granularity: 1`. This
is the actual reason a debugger needs a piccolo fork beyond just visibility
modifiers - the granularity is a second, independent introspection gap the
original source-reading spike didn't catch because it only reads as a
problem once you drive the loop and watch it skip lines.

**Phase 3 deliverable done:** `crates/lua-vm/src/debug_events.rs` drives the
patched `step_with_granularity(.., 1)` loop and emits the
`line`/`call`/`return`/`exception`/`terminated` event stream from
[debug-protocol.md](./debug-protocol.md#debug-events), exposed to JS via
`wasm_bindgen` (`debug_events(source) -> DebugEvent[]`) and verified by
Rust-level tests that assert the exact event sequence (including the
call/return bracketing a function call, and monotonic line events for
straight-line code) against the real piccolo-backed engine - not a mocked
runtime. This satisfies the roadmap's risk-spike bar ("get one `step()` call
to report line changed and expose one local variable's value") with real,
tested behavior rather than a one-off manual check. Out of scope for this
spike, staying explicitly deferred to later phases: breakpoints and stepping
commands (Phase 4/5), a call-stack/named-locals API (register 0 is exposed
by index only - piccolo's compiler doesn't retain local-variable names in
`FunctionPrototype`'s runtime debug info, only in its own transient
compile-time scope table, so a register→name mapping isn't available for
free and would need its own design, likely in the compiler pass rather than
the fork), and any perf tuning of the granularity-1 stepping cost (fine for a
proof-of-concept run; `while true do end`-scale scripts should stay on the
existing `granularity: 64` path until a real breakpoint/stepping design
picks the right granularity per mode).

## 2. SharedArrayBuffer / cross-origin-isolation requirement - RESOLVED (not applicable)

This was a real requirement under the Wasmoon design (official C Lua runs
synchronously on the WASM call stack, so pausing it required
`Atomics.wait`/`notify` on a `SharedArrayBuffer`, which in turn requires
`Cross-Origin-Opener-Policy`/`Cross-Origin-Embedder-Policy` response headers).
Under piccolo's fuel-stepped model, pausing doesn't block anything - the host
simply stops calling `step()` - so this constraint doesn't apply. Kept here,
marked resolved, rather than deleted, so the reasoning survives if the engine
choice is ever revisited.

## 3. Lua semantic conformance (new, direct consequence of the piccolo decision)

Wasmoon inherited conformance for free by running the actual reference
implementation. piccolo doesn't, so this plan now owns it. Full test plan and
known-deviations ledger live in their own document -
[conformance.md](./conformance.md) - because this isn't a one-time check-the-box
risk, it's an ongoing gate that needs to run in CI for the life of the project.
See that document for the fixture-sourcing approach, what's in/out of scope, and
the CI gating plan.

Recorded here as a pointer, not a duplicate, so this file stays the single
"what's unresolved" index: **treat conformance.md's known-deviations ledger as
load-bearing** for the product-brief's "no reimplementation gaps" claim
(product-brief.md has been updated to say "validated against a conformance
suite, deviations tracked" instead of an unqualified guarantee).

## 4. Persistence model

**Decision:** v1 is client-only - localStorage/IndexedDB, no backend, no
shareable links. This matches the fully client-side architecture the rest of the
plan assumes and keeps Phase 2 ("save/load project") simple.

Recorded here rather than left implicit because it's a real product decision,
not a default: a Compiler-Explorer/repl.it-style shareable link is a plausible
and valuable v2 feature, but it requires _some_ backend (even a minimal
snippet-storage service) and should be scoped as its own phase rather than
folded into "save/load" as if it were free.

## 5. Testing strategy for stepping logic

Not addressed in the original plan. Step-over/step-into/step-out (see
[debug-protocol.md](./debug-protocol.md#stepping-algorithms)) are exactly the
kind of logic that looks obviously correct and then breaks on recursion, tail
calls, or C-function frames. Recommended approach:

- Unit-test the step/breakpoint _decision logic_ against a fake runtime that
  emits a scripted sequence of `DebugEvent`s - this tests
  `DebugSessionManager`'s state transitions without needing the real
  piccolo-backed VM in every test run.
- Separately, golden-file integration tests: a set of fixture `.lua` scripts
  (including recursion and nested function calls) with an expected sequence of
  stop locations for each step operation, run against the real piccolo-backed
  engine. These can share fixtures with [conformance.md](./conformance.md)'s
  corpus where the same scripts are useful for both purposes.
- Contract-test the worker message protocol (`WorkerRequest`/`WorkerEvent`)
  independently of both of the above, since that boundary is where serialization
  bugs hide.

## 6. Source mapping for multi-file projects

The architecture diagram in [architecture.md](./architecture.md) names a
source/line-mapping component, and the plan supports multi-file `require()`
(virtual FS), but never specifies how a `StackFrame.source` / Lua chunk name
maps back to a virtual-FS path so error messages and stack traces show the right
filename. Official Lua's convention is loading each chunk with
`load(content, "@" .. virtualPath)` so the chunk name carries the path
automatically - **verify piccolo has an equivalent chunk-naming mechanism**
before assuming this transfers directly; if it doesn't, the instrumentation
layer will need to track source-path-per-chunk itself.

## 7. Default behavior for uncaught errors

Resolved in [debug-protocol.md](./debug-protocol.md#state-machine): an uncaught
error stops like a breakpoint (`reason: "exception"`) so the user sees the
failing line and locals, not just a console dump. Errors inside `pcall`/`xpcall`
don't trigger this. Recorded here because the original plan mentioned "exception
breakpoints" as a Phase 8 feature without ever stating what happens by default
before Phase 8 exists.

## 8. WASM load-time budget

**MEASURED, twice.** First at end of Phase 1 (WASM runtime only), then
re-measured after Phase 2 added Monaco - the numbers below are the current
(Phase 2-complete) reality; the superseded Phase 1-only figures are kept for
context.

**Phase 1 (WASM only, superseded):**
`cargo build --release --target wasm32-unknown-unknown` on `crates/lua-vm`
produced a 952.1 KB raw `.wasm`, shipping as 696.71 KB (236.41 KB gzip) after
`wasm-bindgen --target web` + Vite's production build. Still without `wasm-opt`
(see below).

**Phase 2 (current, WASM + Monaco editor):** `npm run build` in `apps/web` now
produces a **~15 MB `dist/` directory**. That figure is misleading on its own -
most of it is Monaco's ~50 per-language syntax-highlighting chunks (`abap-*.js`,
`julia-*.js`, `sql-*.js`, ...), which are code-split and load lazily only if a
user opens that language; a Lua-only playground never fetches them. What
actually loads eagerly on first paint:

- `index-*.js` (React + Monaco's core editor + the WASM glue code, all bundled
  together): **4.12 MB raw, 1.00 MB gzip**.
- `lua_vm_bg-*.wasm`: **700 KB raw, 235.73 KB gzip** (essentially unchanged from
  Phase 1 - `require()` and the fuel-stepped instruction-limit loop added
  negligible size).
- `lua-*.js` (the one Monaco language chunk actually needed): **3.46 KB gzip**.
- Total eager-load payload: **≈1.24 MB gzip**, not the 15 MB / 3.44 MB figure
  you'd get by (incorrectly) summing every asset in `dist/`.

This is _without_ `wasm-opt` on the Rust side or Monaco's official bundler
plugins (`monaco-editor-webpack-plugin` equivalent) on the JS side - both are
real, cheap levers if 1.24 MB gzip ever becomes a complaint. The single biggest
lever available is trimming Monaco's core bundle itself (it's ~4x the size of
the WASM Lua runtime), not the WASM payload this section was originally scoped
to track.

The original plan's Web Worker recommendation still stands and is implemented in
`apps/web`: the VM initializes inside a Web Worker (`src/lua-worker.ts`), off
the UI thread, and the UI shows an explicit "loading" gate on the Run button
rather than blocking initial page render - confirmed working via a real
Playwright browser run in both dev and production-preview builds.

≈1.24 MB gzip on first load is comparable to a mid-size JS app (most of it is
Monaco, a known-heavy but known-quantity dependency, not something this
project's own code bloated), and well within budget for a playground tool where
the user has explicitly navigated to "go write Lua." No further action needed
for MVP-1; `wasm-opt` and/or a lighter code editor are the natural next levers
if load time becomes a real complaint.

## 9. Minor: citation accuracy in the original doc

The original plan's Fengari-comparison section links to `JX3BOX/wasmoon-lua5.1`
(a community Lua 5.1 fork of Wasmoon) as its Wasmoon reference. Moot now that
the runtime is Rust/piccolo rather than Wasmoon at all, but noted for anyone
reading the original doc as historical context for why C-Lua-via-WASM was
considered before this pivot.
