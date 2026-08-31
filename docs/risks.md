# Risks & Open Questions

Gaps and unverified assumptions found while reviewing the plan. Ordered
roughly by how much they'd cost to discover late.

## Decision log

- **Runtime engine changed from Wasmoon (official C Lua via WASM) to Rust +
  piccolo (pure-Rust Lua-like VM via WASM).** Rationale in
  [architecture.md](./architecture.md#runtime-choice-rust--piccolo). This
  resolved what were risks §1 and §2 against the Wasmoon design (the
  debug-binding gap and the `SharedArrayBuffer`/cross-origin-isolation
  requirement — both marked resolved below) and introduced a new central
  risk in their place: piccolo's debug-introspection surface, and Lua
  semantic conformance more broadly (mitigated by
  [conformance.md](./conformance.md), a new dedicated doc).

## 1. piccolo debug-introspection surface (the central risk)

The entire debugger (Phases 3–8) depends on getting per-`step()`
introspection out of piccolo's fuel-driven executor: current source line,
call-stack frames, named locals, and upvalues.

**What's known:**

- piccolo's execution model (an `Executor` stepped via something like
  `step(&mut ctx, &mut fuel)`, backed by `gc-arena` for
  interruption-safe GC) is *exactly* the right shape for pause/resume — no
  Atomics/threads/blocking needed, since "pause" is just "the host stops
  calling `step()`." This is a genuine, structural improvement over the
  Wasmoon design.
- What's **not** established: whether piccolo's public API exposes enough
  of the executor's internal frame/call-stack/local-variable state for our
  instrumentation layer to build `DebugEvent`s (line/call/return) and
  `StackFrame`/`Scope` data from it (see
  [debug-protocol.md](./debug-protocol.md#debug-events)). piccolo is built
  for embedding Lua-like scripting in Rust applications (e.g. game
  engines) — introspection for an external debugger UI is not its stated
  design goal the way `lua_sethook`/`lua_getlocal`/`lua_getinfo` are
  official Lua's.

**Likely outcome:** some combination of (a) using whatever introspection
piccolo already exposes publicly, (b) forking piccolo to add the missing
hooks ourselves, or (c) upstreaming those hooks as a contribution. Any of
these is workable, but the estimate for Phase 3 depends entirely on which
one it turns out to be — that's the spike.

**Recommended action:** time-box a spike before Phase 3 proper: from a
running `Executor`, after a single `step()` call, (1) determine the current
source line, (2) enumerate the current frame's locals by name and value,
(3) confirm nested `step()` calls can report a `CALL` boundary. If any of
the three isn't reachable through piccolo's existing public API, scope
whether it's a small fork/patch or a deeper change before committing to the
rest of the roadmap's estimates.

## 2. SharedArrayBuffer / cross-origin-isolation requirement — RESOLVED (not applicable)

This was a real requirement under the Wasmoon design (official C Lua runs
synchronously on the WASM call stack, so pausing it required
`Atomics.wait`/`notify` on a `SharedArrayBuffer`, which in turn requires
`Cross-Origin-Opener-Policy`/`Cross-Origin-Embedder-Policy` response
headers). Under piccolo's fuel-stepped model, pausing doesn't block
anything — the host simply stops calling `step()` — so this constraint
doesn't apply. Kept here, marked resolved, rather than deleted, so the
reasoning survives if the engine choice is ever revisited.

## 3. Lua semantic conformance (new, direct consequence of the piccolo decision)

Wasmoon inherited conformance for free by running the actual reference
implementation. piccolo doesn't, so this plan now owns it. Full test plan
and known-deviations ledger live in their own document —
[conformance.md](./conformance.md) — because this isn't a one-time
check-the-box risk, it's an ongoing gate that needs to run in CI for the
life of the project. See that document for the fixture-sourcing approach,
what's in/out of scope, and the CI gating plan.

Recorded here as a pointer, not a duplicate, so this file stays the single
"what's unresolved" index: **treat conformance.md's known-deviations ledger
as load-bearing** for the product-brief's "no reimplementation gaps" claim
(product-brief.md has been updated to say "validated against a conformance
suite, deviations tracked" instead of an unqualified guarantee).

## 4. Persistence model

**Decision:** v1 is client-only — localStorage/IndexedDB, no backend, no
shareable links. This matches the fully client-side architecture the rest
of the plan assumes and keeps Phase 2 ("save/load project") simple.

Recorded here rather than left implicit because it's a real product
decision, not a default: a Compiler-Explorer/repl.it-style shareable link is
a plausible and valuable v2 feature, but it requires *some* backend (even a
minimal snippet-storage service) and should be scoped as its own phase
rather than folded into "save/load" as if it were free.

## 5. Testing strategy for stepping logic

Not addressed in the original plan. Step-over/step-into/step-out (see
[debug-protocol.md](./debug-protocol.md#stepping-algorithms)) are exactly
the kind of logic that looks obviously correct and then breaks on recursion,
tail calls, or C-function frames. Recommended approach:

- Unit-test the step/breakpoint *decision logic* against a fake runtime that
  emits a scripted sequence of `DebugEvent`s — this tests
  `DebugSessionManager`'s state transitions without needing the real
  piccolo-backed VM in every test run.
- Separately, golden-file integration tests: a set of fixture `.lua`
  scripts (including recursion and nested function calls) with an expected
  sequence of stop locations for each step operation, run against the real
  piccolo-backed engine. These can share fixtures with
  [conformance.md](./conformance.md)'s corpus where the same scripts are
  useful for both purposes.
- Contract-test the worker message protocol (`WorkerRequest`/`WorkerEvent`)
  independently of both of the above, since that boundary is where
  serialization bugs hide.

## 6. Source mapping for multi-file projects

The architecture diagram in [architecture.md](./architecture.md) names a
source/line-mapping component, and the plan supports multi-file `require()`
(virtual FS), but never specifies how a `StackFrame.source` / Lua chunk name
maps back to a virtual-FS path so error messages and stack traces show the
right filename. Official Lua's convention is loading each chunk with
`load(content, "@" .. virtualPath)` so the chunk name carries the path
automatically — **verify piccolo has an equivalent chunk-naming mechanism**
before assuming this transfers directly; if it doesn't, the instrumentation
layer will need to track source-path-per-chunk itself.

## 7. Default behavior for uncaught errors

Resolved in [debug-protocol.md](./debug-protocol.md#state-machine): an
uncaught error stops like a breakpoint (`reason: "exception"`) so the user
sees the failing line and locals, not just a console dump. Errors inside
`pcall`/`xpcall` don't trigger this. Recorded here because the original
plan mentioned "exception breakpoints" as a Phase 8 feature without ever
stating what happens by default before Phase 8 exists.

## 8. WASM load-time budget

Not addressed in the original plan. The compiled VM adds to initial load
time; recommend lazy-loading it (on first "Run" press, or idle-after-paint)
rather than blocking the initial page render, and showing an explicit
loading state in the UI rather than a frozen Run button. A Rust/wasm-bindgen
build with `wasm-opt` tends to produce a smaller binary than an
Emscripten-compiled C Lua + JS shim, but that's a reasonable expectation,
not a measurement — profile the actual `crates/lua-vm` output early, since
it sets the loading-UX bar.

## 9. Minor: citation accuracy in the original doc

The original plan's Fengari-comparison section links to
`JX3BOX/wasmoon-lua5.1` (a community Lua 5.1 fork of Wasmoon) as its Wasmoon
reference. Moot now that the runtime is Rust/piccolo rather than Wasmoon at
all, but noted for anyone reading the original doc as historical context for
why C-Lua-via-WASM was considered before this pivot.
