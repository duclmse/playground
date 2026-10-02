# U12 — Canonical WASM playground and debugger

**Status:** planned

**Purpose:** make the web product use the canonical runtime, not a separate VM.

- [ ] Create `sol-wasm` and `packages/sol-runtime` from the Tier-0 runtime.
- [ ] Reproduce budgets, modules, output, debug stepping, frames, locals,
      evaluation, profiling, and timeline.
- [ ] Support `.lua` and `.sol` with shared parser/type diagnostics.
- [ ] Keep worker execution and default-deny capabilities.
- [ ] Differentially switch the web adapter and remove Piccolo only after proof.
- [ ] Meet bundle-size, initialization, and responsiveness targets.

**Exit gate:** canonical-runtime web E2E scenarios and portable native/WASM
fixtures agree; production no longer imports the old runtime package.

See the [historical U12 ledger](../unified-sol-runtime-plan.md#u12--canonical-wasm-playground-and-debugger).

## Work item 1 — `crate/sol` wasm32 boundary spike

**Goal:** find the minimal module set `crate/sol` needs for Tier-0-only
(interpreted, no JIT/AOT) execution of both `.lua` and `.sol` sources, and get
that configuration to `cargo check --target wasm32-unknown-unknown` clean,
without changing any behavior of the existing native default build. Not a
port: this only draws the boundary and gets a compile-clean shape, same scope
discipline as the rest of this file's unchecked deliverables above.

**Central question, answered no:** `interp.rs` does not reference any
JIT-tier type. It represents native code purely as opaque `*const u8`
pointers and receives promotion behavior via an injected closure
(`SpeculativeConfig::promote: Box<dyn Fn(u8) -> PromoteResult + 'a>`,
`interp.rs:94`) — `interp.rs`'s own `PromoteResult` (`interp.rs:21`) is a
same-named but independently-defined type from `jit.rs`'s
(`jit.rs:19` after this item's edits), never shared. `tier.rs` (the glue
connecting the two tiers) and `jit.rs`/`codegen.rs`/`aot.rs` are the only
genuinely JIT-coupled modules — confirmed by `cargo check` itself, not just
grep, after gating (see below).

**Decision: feature-gate `crate/sol` in place, not extract a new crate.**
The actual coupling surface between Tier-0 and the Cranelift-based native
tier turned out to be shallow and mostly already decoupled by the existing
opaque-pointer/injected-closure design, so option (a) from the plan (an
opt-in `jit` Cargo feature) was less invasive than physically splitting
Tier-0 modules into a new crate (option (b)) and was used instead.
`crate/sol/Cargo.toml` makes all six `cranelift-*` dependencies
`optional = true` under a `jit` feature (`default = ["jit"]`), and the `sol`
CLI binary (`main.rs`, which unconditionally uses `jit`/`tier`/`aot`) gets
`required-features = ["jit"]` so a `--no-default-features` build skips it
entirely.

**Coupling points found and how each was resolved:**
- `lib.rs`: `pub mod aot;`, `codegen;`, `jit;`, `tier;` gated behind
  `#[cfg(feature = "jit")]` — each is cranelift-coupled at its own top
  (`aot.rs:1-15` imports `cranelift_codegen::ir::*` directly; `tier.rs`
  imports `crate::jit::Jit`; confirmed via
  `grep -rn "crate::codegen\|crate::tier" crate/sol/src/*.rs` that neither
  is referenced from any other, non-gated module).
- `jit.rs`'s `called_functions` (previously `jit.rs:559-673`): a pure
  `TFunction`/`TExpr` AST walker with zero Cranelift dependency, but
  `typeck.rs`'s Tier-0-essential `check_partitioned` (the `.lua`
  native/dynamic demotion fixed point) called it at `typeck.rs:198` and
  `typeck.rs:276` (pre-edit line numbers). Relocated the function itself
  into `typeck.rs` (now defined just above `add_builtin_externs`); updated
  its one remaining internal caller in `jit.rs` (`promote`'s worklist walk)
  to `crate::typeck::called_functions`, and `main.rs`'s two CLI call sites
  (previously `main.rs:695,746`) to `sol::typeck::called_functions`.
- `strings.rs`'s `register(&mut cranelift_jit::JITBuilder)` (was
  `strings.rs:178-189`, the only cranelift-coupled item in an otherwise
  Tier-0-pure 189-line file): gated behind `#[cfg(feature = "jit")]`; its
  sole caller is `jit.rs:87`, itself fully jit-gated.
- `lua_runtime/dynjit/`: not found by the initial file-level grep sweep
  (grep over `crate/sol/src/*.rs` doesn't recurse into subdirectories) and
  only surfaced once `cargo check --target wasm32-unknown-unknown` actually
  ran. This is U9's baseline JIT for the *dynamic* `.lua` bytecode tier (a
  second, independent `cranelift_jit::JITModule`, disjoint from the typed
  tier's `jit::Jit`) — a real, load-bearing Tier-0 coupling, not just a
  convenience import: `LuaRuntime` (`lua_runtime/mod.rs:496`) holds an
  unconditional `dynjit: dynjit::DynJitState` field, constructed in
  `lua_runtime/init.rs`, and consulted from `dispatch.rs`'s per-activation
  hot counters. Resolved by cfg-splitting `dynjit/mod.rs` itself: the
  cranelift-dependent `DynJit`/`abi`/`lower`/`opt_lower`/`stubs` modules and
  the `DynJitState::Ready(DynJit)` variant are `#[cfg(feature = "jit")]`;
  a non-`jit` `DynJitState` (`Unavailable`-only) and non-`jit`
  `try_promote`/`try_optimize`/`try_osr` stubs (always returning `false`/
  `None`) sit alongside, mirroring the module's own pre-existing
  graceful-degradation shape for a *runtime* JIT-init failure
  (`DynJitState::Unavailable` already existed for "no executable-memory
  permission" before this item) — now also reachable at compile time.
  `dispatch.rs`'s `run_native` and `dispatch/bytecode.rs`'s
  `try_osr_backedge`'s native-call body are gated the same way; both are
  only ever reached once `try_promote`/`try_optimize`/`try_osr` have
  actually produced a native pointer, which the non-`jit` stubs never do.
- `gc.rs:45`'s module-level `const _: () = assert!(size_of::<usize>() ==
  8, ...)` was the one genuine non-JIT, architecture-level blocker: it's
  unconditional on all targets, but its own comment says it exists only to
  match `flush_callee_saved_registers`'s aarch64/x86_64 register-spill
  arms (`gc.rs:413-454`), which already have a portable
  `#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]`
  zero-register fallback arm for every other architecture, wasm32
  included. `init_stack_base` (which is what makes that native-stack scan
  non-trivial) is only ever called from the JIT/AOT native-call path
  (`main.rs`, `runtime.rs`'s `sol_gc_init_stack_base`) — never from
  Tier-0-only code — so on a `--no-default-features` build `STACK_BASE`
  stays its initial 0, and `collect`'s conservative native-stack scan is
  already a documented no-op for that case (same as in today's unit
  tests). Narrowed the assert to
  `#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]` to match
  `flush_callee_saved_registers`'s own scope exactly, rather than asserting
  a width assumption on architectures where it was never actually
  exercised. This only fixes the *compile*; it does not newly verify that
  conservative GC rooting is sound to actually run on wasm32 (no code runs
  yet) — that is this milestone's later execution-parity work (item 2), not
  this spike.

**Verified outcome:**
`cargo check --manifest-path crate/sol/Cargo.toml --no-default-features
--target wasm32-unknown-unknown` is clean (zero errors, zero warnings), for
both `--lib` and the full default target set (the `[[bin]]`'s
`required-features = ["jit"]` correctly excludes `main.rs`).
`cargo build --manifest-path crate/sol/Cargo.toml` (native, default
features) and `cargo test --manifest-path crate/sol/Cargo.toml` (every test
binary: unit tests, `tests/programs.rs`, `tests/lua55.rs` and its sibling
suites, `tests/sol_conformance.rs`) pass unchanged, and
`scripts/test-lua55-manifest.sh` passes — confirming this item changed only
a compile-time configuration boundary, not any native-build behavior.

**Explicitly not done here (left for later U12 items):** no `sol-wasm`
crate, no wasm-bindgen bindings, no execution-parity harness actually
*running* `.lua`/`.sol` source on wasm32, no debugger. The `--no-default-features`
build today only proves the Tier-0 module set *compiles* for wasm32 — it has
never been run there.

## Work item 2 — minimal Tier-0-only execution parity (no debugger yet)

**Correction to this item's own plan text:** the plan cited an existing
`conformance/fixtures`/`conformance/expected` corpus (10 fixtures) as the
differential-testing asset. That directory does not exist and never has —
`ls conformance/fixtures` and `git log --all -- conformance/` both confirm
it's absent from the working tree and from all of history. It's likely a
residual reference to `docs/conformance.md`'s plan for the retired
`crate/lua-vm`/piccolo runtime, never actually created. The real,
already-existing asset serving the same purpose is
`crate/sol/tests/fixtures/sol-conformance/` (34 `.sol` fixtures ported from
upstream `lua-5.5.1-tests`, with documented expected stdout in
`crate/sol/tests/sol_conformance.rs`, which runs them through the `sol` CLI
against the full tiered-JIT/AOT path) — used below instead.

**Central finding: Tier-0-only execution did not previously exist at all.**
`tier.rs`'s `Engine::new` unconditionally constructs a `jit::Jit`
(`tier.rs:97`, pre-existing), and `tier.rs` itself is
`#[cfg(feature = "jit")]`-gated (item 1). So before this item, there was no
code path that could run a typed `.sol`/`.lua` program to completion without
the `jit` feature — item 1 only proved the crate *compiles* for wasm32, not
that it can execute anything. `interp.rs` itself has no such dependency
(`Slot::Native` is a bare `*const u8`; `promote`/`osr_promote`/
`speculative.promote` are injected closures that can simply always return
`None`, meaning "keep interpreting"), so a jit-free engine was buildable
without touching `interp.rs`'s architecture.

**New module: `crate/sol/src/tier0.rs`** (unconditionally compiled, not
feature-gated — `lib.rs:41`). `tier0::Engine::new`/`new_with_budget` compiles
every function to `Slot::Bytecode` via the existing `bccompile.rs`, rejecting
(with a clear error naming the function) any function too large for Tier-0's
8-bit register budget, since there is no native fallback without `jit`. Two
compiler-injected internal externs — `__sol_string_concat`
(`strings.rs:39`) and `__sol_any_is` (`dynamic.rs:22`), both auto-added by
`typeck.rs` for string `+` and `any`-type tests and both fixed-signature — get
hand-written ABI-adapting shims (`tier0_string_concat_shim`/
`tier0_any_is_shim`) matching `interp::call_native`'s uniform
`extern "C" fn(*const u64, i64) -> u64` slot ABI. A user's own
`extern function` declaration (arbitrary native FFI, e.g.
`math.sol`'s `sqrt`/`pow` linking real libm symbols) is rejected with a clear
error: that requires `jit.rs::link_externs`'s real Cranelift ABI-marshaling
codegen, which Tier-0 has no equivalent of, and arguably couldn't use on a
wasm32 host anyway (no native libm to link against).

**Instruction-budget parity (plan goal (b)):** `interp::Runtime` gained an
`instructions_remaining: Cell<u64>` field and a new, final
`instruction_budget: u64` constructor parameter, checked once per bytecode
instruction at the top of `interpret()`'s dispatch loop
(`CallOutcome::Raised("instruction budget exceeded")` on exhaustion). Every
pre-existing call site (`interp.rs`'s own test, `tier.rs::Engine::new`,
`main.rs`'s mixed native/dynamic path) passes `u64::MAX` — zero behavior
change for `sol run`/`sol debug`/the tiered-JIT path. `tier0::Engine`
defaults to `DEFAULT_INSTRUCTION_BUDGET = 10_000_000`, matching
`crate/lua-vm`'s documented `MAX_INSTRUCTIONS` sandboxed-host convention. Unit
tests in `tier0.rs` confirm the budget trips and confirm normal programs run
unaffected.

**SourceMap population is real but incomplete for the typed tier (plan goal
(c)):** `bccompile.rs:64` builds `SourceMap::single_line(b.code.len(),
f.source_line)` — every instruction in a function reports the *same* line
(the function's declaration line), not its own line. This is not a
shortcut bug but a direct consequence of `types::TStmt`/`TExpr` (the typed
IR `bccompile.rs` compiles from) carrying no per-node line field at all —
only `TFunction` has `source_line`/`source_span`. The raw `ast::Stmt` *does*
carry a real per-statement `line: u32` (`ast.rs:128` and siblings), and
`typeck.rs`'s `check_stmt` reads it per statement (`typeck.rs:751-918`) but
only for its own diagnostics — it's dropped during lowering to `TStmt`, never
threaded through. By contrast the dynamic `.lua` bytecode tier
(`lua_bytecode/mod.rs:301`) tracks a real per-instruction `Vec<u32>`
(`state.lines`) end to end. **Consequence for item 3 (the Tier-0 debugger):**
stepping/breakpoints on typed `.sol` code can currently only resolve to
"which function," not "which line within it" — closing this gap means adding
a line field to every `TStmt`/`TExpr` variant and threading it through
`bccompile.rs`'s `Builder`, which is a real, scoped feature addition for that
item to account for, not a bug to silently fix here.

**Differential harness (plan goal (d)):** `crate/sol/tests/tier0_conformance.rs`
runs every fixture in `tests/fixtures/sol-conformance/` through
`tier0::Engine` directly (no CLI subprocess) and compares against the same
expected values `tests/sol_conformance.rs` uses for the tiered-JIT/AOT path.
33 of 34 fixtures match exactly; the one exception, `math.sol`, is the
documented native-libm-FFI case above (`KNOWN_UNSUPPORTED`). A debug build of
`interp.rs`'s recursive `dispatch`/`interpret` pair has a large enough
per-frame stack footprint to overflow the default 8MB thread stack at
`calls.sol`'s ~500-1000-deep recursion (trivial for native/JIT code, and for
`--release`, which never needed this) — the test spawns itself on a
256MB-stack thread, a standard debug-build fix, not a correctness regression;
the same per-frame cost is relevant to wasm's own linear-memory stack sizing.

**wasm-bindgen throwaway harness (plan goal (a)):** `crate/sol/Cargo.toml`
gained an optional `wasm-bindgen = "=0.2.100"` dependency (pinned to match
`crate/lua-vm/Cargo.toml`'s existing pin) under a new, non-default `wasm`
feature, and `crate/sol/src/wasm_api.rs` (`#[cfg(feature = "wasm")]`) exposes
a single `#[wasm_bindgen] pub fn run_sol(source: &str) -> Result<String,
JsValue>` wrapping `tier0::Engine` exactly as `tier0_conformance.rs` does.
Built with `cargo build --no-default-features --features wasm --target
wasm32-unknown-unknown --release` and bound with `wasm-bindgen --target
nodejs`; a throwaway Node script ran all 33 non-`math.sol` fixtures through
the real compiled `.wasm` artifact and got exact matches, plus confirmed
`math.sol` surfaces the expected extern-rejection error as a thrown
`JsValue`.

**Real finding from that harness, relevant to item 6's worker wiring:** the
generated wasm module's import table includes `env.malloc`/`env.free`/
`env.realloc`/`env.sol_c_api_shim_anchor` — raw libc-style externs declared
by `lua_runtime/c_api/auxlib.rs:561`'s `default_alloc` (the Lua C embedding
API's default allocator) and `lua_runtime/c_api.rs:728`'s linker anchor
(normally satisfied natively by `build.rs`'s compiled `c_api_shim.c`, which
has no wasm32 build path). These stay in the `.wasm` binary's required
import table even though `run_sol`'s typed-only path never reaches
`lua_runtime::c_api` at all — wasm32-unknown-unknown doesn't strip
provably-unreachable `extern "C"` imports the way a native link would. For
this throwaway harness, trivial throwing JS stubs under a local
`node_modules/env.js` were enough (never actually invoked). For the real
`packages/sol-runtime` wiring in item 6, the cleaner fix is almost certainly
feature-gating `lua_runtime::c_api` out of the browser-facing wasm build
entirely — the playground has no use for Lua's native C embedding API — or
providing real, documented worker-side implementations.

**Verified outcome:** `cargo test --manifest-path crate/sol/Cargo.toml`
(every test binary, unit + integration) passes unchanged: 0 failed across
all binaries, including the new `tier0::` unit tests and
`tier0_conformance.rs`. `cargo check --no-default-features --target
wasm32-unknown-unknown --lib` stays clean. `cargo build
--no-default-features --features wasm --target wasm32-unknown-unknown
--release` succeeds and the resulting `.wasm`, bound via `wasm-bindgen
--target nodejs`, runs correctly from real Node.js (v24.14.0).

**Explicitly not done here (left for later U12 items):** no debugger
(breakpoints/stepping/frames — item 3), no `packages/sol-runtime` or worker
wiring (item 6), no fix for the `SourceMap`/line-granularity gap above (item
3's problem to account for), no fix for the `env.malloc`/`env.free`/
`env.realloc`/`sol_c_api_shim_anchor` wasm import leakage beyond a throwaway
stub (item 6's problem).

## Work item 3 — Tier-0 debugger engine (breakpoints, stepping, stack frames)

**Goal:** prototype the underlying native engine pieces a future browser
debugger protocol (`apps/web/src/debug-protocol.ts`, read for understanding
only — not touched) would call into: breakpoint verification, stack-frame/
locals inspection with real typed display, and stepping (over/into/out), on
top of item 2's `tier0::Engine`. Explicitly a native, non-wasm Rust spike —
no wasm-bindgen bindings, no `apps/web` changes, no worker-protocol
machinery. Flagged in the plan as the single highest-risk/most-uncertain
item in this milestone.

**Deliverable 1 — closing the typed-tier `SourceMap` gap.** Item 2 found
`bccompile.rs` building `SourceMap::single_line(b.code.len(), f.source_line)`
for every typed `.sol` function — every instruction reporting the same,
function-declaration line — because `types::TStmt`/`TExpr` carried no
per-node line field. Rather than add a line field to every `TStmt`/`TExpr`
variant (a much larger, more invasive change touching every arm of every
pass), added one `u32` line alongside each top-level statement instead:
`types.rs` now defines `TBlock = Vec<(u32, TStmt)>` and every block-typed
field (`TFunction::body`, `TStmt::If`'s `then_block`/`else_block`,
`TStmt::While`/`NumericFor`'s `body`, etc.) uses it in place of
`Vec<TStmt>`/`&[TStmt]`. The line comes from a new `typeck.rs` function,
`ast_stmt_line(stmt: &ast::Stmt) -> u32`, which reads the real per-statement
line every `ast::Stmt` variant already carries (`ast.rs`'s fields) — for the
one variant with no line of its own, `ast::Stmt::Block` (a Lua `do...end`),
it takes the first inner statement's line, or `0` for an empty block (which
compiles to no bytecode, so it never needs a breakpoint slot). `check_block`
(`typeck.rs`) now returns `Result<TBlock, String>`, pairing each lowered
`TStmt` with `ast_stmt_line(s)`.

`bccompile.rs`'s `Builder` gained a parallel `lines: Vec<u32>` built 1:1 with
`code` by `emit()` (each push records `current_line`, which `compile_block`
sets from each `(line, stmt)` pair just before compiling that statement), so
every inline operand word an instruction emits inherits its instruction's
line — those words are never valid pcs for the interpreter to stop at
anyway. At the end of `compile_function`, this becomes a real
`sol_core::SourceMap::new(lines.iter().map(|&line| SourceLocation::new(line,
0)).collect())` instead of the old function-wide stub. `SourceMap::single_line`
itself still exists (used only as a test-fixture default elsewhere), but is
no longer `bccompile.rs`'s production path.

Every other pass that pattern-matches a block of statements needed updating
to the new `(u32, TStmt)` shape: `typeck.rs` (`check_block`, `called_functions`'s
`walk_stmts`, `positive_narrowing`, `check_conditional`), `optimize.rs`,
`escape.rs`, `verify.rs`, and — under the `jit` feature — `codegen.rs`
(`visit_stmts`, `uses_function_value`, `count_stmts`, `stmts_call`,
`translate_block`, `try_vectorize_elementwise_loop`,
`detect_vectorizable_loop`, `assigns_to_local`, `compile_osr_entry`) and
`jit.rs` (`walk_stmts`, `walk_stmts_for_exhaustiveness`, `rewrite_stmts`).
None of these change *what* they compute — every one simply unwraps the line
and ignores it except `bccompile.rs` itself — confirmed by both
`cargo test --manifest-path crate/sol/Cargo.toml` and
`--no-default-features` passing unchanged (see "Verified outcome" below).
`codegen.rs`'s `collect_local_types` (a pure `TFunction`/`TBlock` walker with
no Cranelift dependency, needed by the jit-free debugger for locals' types)
was relocated from `codegen.rs` (jit-gated, unreachable without `jit`) to
`types.rs` (always compiled); `codegen.rs`/`jit.rs` now call it via the
existing glob re-export, zero call-site changes beyond the move itself.

A regression test, `distinct_statement_lines_map_to_distinct_pcs`
(`crate/sol/src/debugger.rs`'s `#[cfg(test)]` module), compiles a
two-`local`-statement function and asserts the pcs mapping to line 2 and
line 3 differ — i.e. not the old single-line stub, which would have made
every line resolve to the same (or no) pc.

**Confirmed finding, as scoped: the dynamic `.lua` tier was not touched.**
`lua_bytecode/mod.rs`'s `state.lines: Vec<u32>` already tracks a real
per-instruction line end to end (confirmed by reading, not assumed) — this
item added nothing there and changed no dynamic-tier behavior.

**Deliverable 2 — per-line debug-hook mechanism.** `interp::Hooks` gained a
new method, `on_instruction(&self, func_id: u8, pc: u32, line: u32, regs:
&[u64])`, with an empty default body (the same zero-cost-when-unattached
pattern as the trait's existing call-boundary hooks: `H = ()`'s impl inlines
away entirely). Wired into `interpret()`'s dispatch loop immediately after
`frame.pc = pc as u32` — it resolves `pc`'s line from the function's real
`SourceMap` (deliverable 1) and passes the *whole* register file (named
locals plus temporaries) as it exists at that exact instant, so a debugger
reads real interpreter state, not a reconstruction. No existing `Hooks`
implementor overrides it, so no existing caller's behavior changes; not
separately re-benchmarked this session beyond the full `cargo test` suites
passing unchanged. `interp::Runtime` also gained a `bytecode_function(&self,
func_id: u8) -> Option<Rc<BcFunction>>` accessor so a debugger can read the
exact `SourceMap`/bytecode a given `Runtime` is actually executing against,
rather than a separately recompiled copy that might disagree with it;
`tier0::Engine` exposes the same lookup by name via two new accessors,
`function_id`/`function_bytecode`.

**Scope cut, stated up front because it shapes deliverables 2, 3, 5, and 6
together: this is a full-trace-recording design, not true interactive
pause/resume.** Tier-0 bytecode interpretation is deterministic with
respect to everything this spike's fixtures observe, and `interp::Runtime`'s
dispatch loop is not written as an externally-driven state machine — pausing
it mid-execution and resuming later would need either a coroutine/fiber
runtime or a substantial rewrite of `interp.rs`'s control flow, which is a
far larger change than an engine *spike* calls for. Instead,
`crate/sol/src/debugger.rs`'s `DebugSession::run` executes the target call
once to completion while a `TraceRecorder` (implementing `Hooks`) records
one `TraceStep { func_id, pc, line, depth, regs }` per instruction via
`on_instruction`, plus call depth via `on_call_enter`/`on_call_exit`.
Breakpoint hits, stepping, and locals inspection then all answer by
indexing into this recorded trace — not by querying a live, paused
interpreter. This is the single biggest intentional scope cut in this item.
It is sound for this spike's differential tests (Tier-0 execution has no
externally-visible nondeterminism here) but does **not** demonstrate that a
real browser debugger can actually suspend a long-running or
infinite-looping script mid-flight and let a user inspect it before
deciding whether to continue — that capability gap is real and is left
entirely to a later item (most likely needing either the coroutine/fiber
approach or budget-interval re-entrant execution, neither prototyped here).

**Deliverable 3 — breakpoint matching.** `debugger::verify_breakpoint(function_name,
line, source_map)` linearly scans the real per-instruction `SourceMap` built
above for the first pc whose mapped line equals the requested line, and
returns a `VerifiedBreakpoint { function_name, line, verified, pc }`. A line
with no mapped instruction (blank line, comment, or a statement
`optimize.rs` folded away) reports `verified: false`, `pc: None` — confirmed
by the `a_line_with_no_mapped_instruction_is_not_verified` unit test
(a blank line between two `local` statements). `DebugSession::set_breakpoint`
wraps this against the exact `SourceMap` its own engine is executing
(`tier0::Engine::function_bytecode`), so a verified pc is guaranteed
consistent with what `trace()` later records.

**Deliverable 4 — stack frame walk + locals with real typed display.**
`debugger::ValueRenderer::render(ty, raw)` turns a raw register word plus
its static `Type` into a `DisplayValue` (`Scalar(String)` or
`Reference { reference, summary }`), reading `sol_core`/the runtime's actual
representations directly rather than reimplementing them: `i64`/`f64`/`bool`
reinterpret the untagged word per the register file's static-type
convention; `String` reads `strings.rs`'s `[length: u64][bytes...]` layout
via its own `strings::bytes` helper; `Array`/`Map`/`Struct`/`Function`
become `Reference` handles (the raw pointer/payload word itself, stable for
the paused frame's lifetime) a caller can later expand.
`ValueRenderer::expand(ty, reference)` implements the "lazy/paginated
expansion for tables" requirement for the two layouts this spike can read
directly from outside their owning module: `Array` (the same
`{len: i64, data: *const u8}` header `interp.rs`'s own `Op::Index` reads,
then 8-byte words at `data + index*8`) and `Struct` (the same
`*(base as *const u64).add(field_index)` access `Op::GetField` uses, keyed
by `StructLayout::fields` from the program's struct table).
`DebugSession::locals_at(trace_index)` reports every local visible at a
given paused point as a `LocalView { local_id, is_param, ty, type_name,
value }`, built from `types::collect_local_types` (every `LocalId`'s static
type) plus that trace step's own captured register snapshot.

Two findings, stated explicitly rather than silently worked around:
- **Locals have no name table.** `TStmt::Local` only carries a numeric
  `LocalId`; no source identifier survives typed lowering anywhere in
  `types.rs`. `LocalView` reports each local by its numeric id
  (`local<id>`-style), not a source name — recovering real names would need
  a separate name table threaded through typeck.rs's lowering, not
  attempted here.
- **No separate "upvalue" concept exists at Tier-0 for typed `.sol`.**
  Non-escaping closures are lambda-lifted away and escaping captures become
  ordinary struct-typed locals before codegen/bccompile (`escape.rs`) — by
  the time a function reaches Tier-0 bytecode, captured state already *is* a
  local, not a distinct upvalue slot a debugger would need to list
  separately. `types.rs`'s own comment notes closures arrive with M11; this
  is an architectural finding about the current lowering, not a gap this
  item left open.
- **Not done:** unboxing an `Any`-typed local's real underlying type (no
  reverse tag→type registry exists over `value.rs`'s `TAG_*` constants —
  building one is a real feature, not attempted here) and `Map` expansion
  (`runtime.rs`'s hash-table layout is a private implementation detail with
  no public C-layout contract to read from outside it, unlike `Array`'s
  simple header+elements layout). Both report as an opaque `Reference`
  summary and stop there.

**Deliverable 5 — stepping and continue.** `crate/sol/src/debugger.rs`'s
`DebugSession` (constructed from a `TProgram`, wrapping a
`tier0::Engine<TraceRecorder>`) is the standalone, testable Rust type this
deliverable asked for — no wasm-bindgen, no browser, no worker, exercised by
its own `#[cfg(test)]` module plus the differential integration tests below.
`step_into(from)` finds the next trace index whose line or function differs
from `from`'s, at any depth. `step_over(from)` finds the next trace index at
a call depth no deeper than `from`'s with a different line — skipping
entirely over a nested (non-tail) call's own instructions. `step_out(from)`
finds the next trace index at a strictly shallower depth than `from`'s.
`continue_to_breakpoint(from)`/`first_breakpoint_hit()` scan the trace for
the next/first index whose `(func_id, pc)` matches a verified breakpoint.
Tail calls reuse the caller's depth (`interp::Runtime::dispatch`'s loop
never recurses for a tail call), matching real tail-call semantics, though
this was not separately fixture-tested here.

**Deliverable 6 — differential tests.** `crate/sol/tests/debugger.rs`
(three fixtures: a simple multi-statement function, a function with a local
and a `while` loop, and a nested call between two functions) implements the
two required differential assertions:
- `breakpoint_hit_locals_match_the_non_paused_interpreted_run`: sets a
  breakpoint just before a function's final `local` assignment, runs the
  session, and confirms the locals visible at that paused point already
  show their prior statements' correct values, then confirms the *last*
  trace index's locals agree exactly with what an independent, non-paused
  `tier0::Engine::call_outcome` run on the same program computes.
- `stepping_over_a_loop_body_produces_the_expected_line_sequence`:
  repeatedly `step_into`s from the top of a 5-iteration `while` loop and
  asserts the exact per-line visit counts (condition line visited 6 times —
  5 true checks plus 1 final false check — body lines 5 times each,
  surrounding statements once each), proving the loop actually iterates in
  the recorded trace with the real per-line `SourceMap`.
- `stepping_over_into_and_out_of_a_nested_call_behave_differently`: at a
  call site, confirms `step_over` lands back in the caller without ever
  visiting the callee's own `func_id`, `step_into` lands inside the callee
  at strictly greater depth, and `step_out` from inside the callee returns
  to the caller at its original depth.

**Verified outcome:**
`cargo test --manifest-path crate/sol/Cargo.toml` — 538 passed, 0 failed,
across 25 test binaries (unit tests plus every integration suite, including
the 3 new tests in `tests/debugger.rs` and the 2 new unit tests in
`src/debugger.rs`).
`cargo test --manifest-path crate/sol/Cargo.toml --no-default-features` —
531 passed, 0 failed, across 24 test binaries (one fewer binary: the `sol`
CLI's own doc/integration surface that requires `jit`).
`cargo check --manifest-path crate/sol/Cargo.toml --no-default-features
--target wasm32-unknown-unknown` — clean. `scripts/test-lua55-manifest.sh`
— passes (`Lua 5.5 manifest regression checks passed`). `debugger.rs` is
declared in `lib.rs` with no `#[cfg(feature = "jit")]` gate, so it is part
of the same always-compiled, always-wasm32-checked module set as `tier0.rs`
— not an assumption, confirmed by the wasm32 check above actually including
it.

**Explicitly not done here:**
- No true interactive suspend/resume of a live interpreter — see the
  full-trace-recording scope cut above. This is the biggest gap: a real
  browser debugger needs to pause a *running* script, not replay a
  completed trace.
- **The dynamic `.lua` tier did not receive per-line debug-hook support.**
  Per the task's scope boundary ("prioritize a complete, well-tested typed
  Tier-0 engine first... if not reached, that's an acceptable, explicitly
  documented gap"), all of this item's time went to the typed `.sol` path.
  `lua_bytecode`/`dispatch.rs` already has real per-instruction lines
  (`state.lines`), so the `SourceMap` half of deliverable 1 would not be
  needed again there, but `interp::Hooks::on_instruction` is specific to
  `interp.rs`'s bytecode interpreter (the typed tier's execution engine) —
  the dynamic tier's own dispatch loop (`dispatch.rs`) is a structurally
  different interpreter with no equivalent hook, and none was added.
  `DebugSession` only accepts a typed `TProgram`, not a dynamic `.lua`
  runtime. Extending to `.lua` is unstarted, not partially done.
- No name table for locals, no `Any`/`Map` expansion — see deliverable 4's
  findings above.
- No wasm-bindgen bindings, no `apps/web` changes, no worker-protocol
  wiring — out of this item's explicit scope, not attempted.
- No fuzzing or stress-testing of stepping across deeply recursive or
  exception/error-raising (`CallOutcome::Raised`) programs — the fixtures
  are small and all return normally; behavior of stepping through a trace
  that ends in a raised error was not exercised.
