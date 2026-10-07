# U12 — Canonical WASM playground and debugger

**Status:** in progress

**Purpose:** make the web product use the canonical runtime, not a separate VM.

- [x] Create the WASM adapter and `packages/sol-runtime` from Tier 0
      (feature-gated `crate/sol`, rather than a separate `sol-wasm` crate).
- [ ] Reproduce budgets, modules, output, debug stepping, frames, locals,
      evaluation, profiling, and timeline.
- [x] Support `.lua` and `.sol` with shared parser/type diagnostics.
- [x] Keep worker execution and default-deny capabilities.
- [x] Differentially switch the web adapter and remove the Piccolo production dependency.
- [x] Meet the documented portable-profile bundle/init/responsiveness targets.

Production is canonical by default. Live mixed coroutine/thread isolation
remains unqualified; see work item 20 for the qualified scalar profile and
remaining limits. Earlier default-off observations below are historical.

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

## Work item 4 — frame-scoped evaluation, table inspection, memory stats

**Goal:** extend item 3's native, non-wasm `DebugSession` spike with
`debugEvaluate`/`debugSetVariable`-equivalent expression evaluation,
`debugGetTableEntries`/`GetMetatable`-equivalent `Map` inspection, and
`debugGetMemoryStats`/`ForceGc`-equivalent memory introspection — still a
native Rust spike (no wasm-bindgen bindings, no `apps/web` changes, no
worker-protocol wiring), built directly on item 3's `TraceStep`/`ValueRenderer`
machinery rather than duplicating it.

**Deliverable 1 — `debugEvaluate`.** The plan's own wording (`local<id0>`
read, "re-entrant sub-interpreter invocation sharing the paused frame's
register file/heap") assumes a true pause/resume engine. Item 3 built the
opposite: `DebugSession` has no live paused frame at all — "paused" means
"an index into an already-fully-recorded trace" (see item 3's scope-cut note
above). So the honest architectural call here is that "frame-scoped" cannot
mean sharing a live register file/heap; it means treating the recorded
`TraceStep::regs` snapshot at the requested index as a **fixed set of
inputs** and evaluating the expression as a pure function of those inputs via
a second, independent, throwaway `tier0::Engine` — not a live re-entrant call
into the real interpreter. This still does not create a second GC-root
domain in the sense the plan worried about (a non-scalar local's snapshot
word is only ever copied as an opaque `u64`, never dereferenced unless the
expression itself indexes into it), but it does mean eval cannot observe or
cause a side effect on the real run, and cannot call the real program's other
functions (the synthetic program contains only the one function being
evaluated).

Implementation (`crate/sol/src/debugger.rs`'s "deliverable 1" section,
`DebugSession::evaluate`): a two-pass synthetic-program compile.
`crate::lib.rs`'s `compile()`/`compile_program_with_config` require a
function literally named `"main"`, with zero parameters and a
CLI-printable return type (checked directly: `lib.rs`'s `main_func` lookup
and return-type validation) — unusable for an arbitrarily-named,
parameterized eval expression, so `evaluate` instead calls
`parser::parse`/`aliases::expand`/`closures::lower`/`typeck::check`/
`verify::verify`/`optimize::optimize`/`escape::scalar_replace` directly
(`eval_compile`), mirroring `compile_program_with_config`'s pipeline minus
its `main`-only wrapper. Pass 1 wraps the expression in
`function __sol_eval(local<id>: <ty>, ...): any\n return <expr>\nend`,
with one parameter per **scalar**-typed local (`I64`/`F64`/`Bool`/`Nil`/
`String` — `eval_type_annotation`) visible in the paused function
(`types::collect_local_types`), named `local<id>` to match `locals_at`'s own
id-only convention (confirmed: `TStmt::Local` carries no source name — see
item 3's finding). Declaring the return type `any` forces
`typeck.rs::coerce` to wrap the real result in `TExprKind::Box` (confirmed by
reading `coerce`'s `Concrete -> Any` branch, lines ~703–732) unless it was
already `Any` — which cannot happen here since `Any`-typed locals are
excluded from the parameter list — so `eval_find_return_type` peels that one
`Box` node back off to discover the expression's true static type. Pass 2
recompiles the same source with that real type declared directly (so the
interpreter never unboxes an `any` at all), builds a one-off
`tier0::Engine::new(...)`, and calls it with the chosen locals' raw register
words from the trace step as arguments; the result renders through the
existing `ValueRenderer::render` (no new rendering logic).

**Scope cut, stated explicitly:** only scalar-typed locals are exposed as
synthetic parameters; `Array`/`Map`/`Struct`/`Function`/`Any`-typed locals are
not — referencing one by name in an eval expression simply fails to
typecheck as an undefined variable, rather than being silently dropped.
Exposing them would additionally need reconstructing the real program's
struct declarations and/or a reverse `Any`-tag registry inside the throwaway
program — a real, bounded gap, deferred rather than attempted. `.sol`'s
typed tier also has no mutable-global-variable concept at all (confirmed by
grepping `typeck.rs`/`types.rs`/`ast.rs`: "global" there only ever means
either Lua's dynamic `_ENV` runtime, unimplemented, or a top-level Sol
function declaration, already callable through `TExprKind::Call`), so the
plan's "eval of an expression referencing a global modified mid-session"
test case does not apply to this tier and was not attempted; the two
required differential tests (local-read, arithmetic over two locals) do not
depend on it.

**`debugSetVariable` is explicitly scoped out, not silently omitted.** A
`TraceStep`'s `regs` are a copy already computed by the one completed run
item 3's `TraceRecorder::on_instruction` recorded; mutating one recorded
step's copy cannot retroactively change what later, already-recorded steps
computed from the original unmodified execution. Supporting "set, then
observe later steps change" would require the true externally-driven
re-entrant interpreter both the plan and item 3 flagged as out of scope for
this spike — a real, separate feature, not a gap a snapshot mutation could
honestly paper over.

**Known hazard, inherited rather than specially handled:** like any Tier-0
execution, a trapping operation (e.g. integer division by zero) calls
`trap()` (`process::abort()`), aborting the whole process, not just the eval
call. `evaluate` does not sandbox against this — the same hazard already
exists for `DebugSession::run` itself, and sandboxing it (e.g. running the
synthetic engine out-of-process) is a real, separate feature.

Tests (`crate/sol/tests/debugger.rs`): `eval_of_a_local_read_matches_locals_at_reported_value`
confirms `evaluate(hit, "local0")` matches `locals_at`'s own reported value
for the same local id at the same trace index;
`eval_of_arithmetic_over_two_locals_matches_the_independently_computed_result`
confirms `evaluate(hit, "local0 * local1 + 1")` matches the value the test
independently computes from the fixture's known inputs (7, 6);
`eval_rejects_a_reference_to_an_undefined_or_out_of_scope_name` confirms a
name eval does not expose fails as a tagged `"eval:"` error rather than
resolving to something unintended.

**Deliverable 2 — `debugGetTableEntries` (`Map` expansion); no
`GetMetatable` equivalent exists.** Item 3 left `Map` expansion undone
because `runtime.rs`'s `MapI64Header` (an open-addressed table: `len`,
`capacity`, `keys: *mut i64`, `values: *mut i64`, `occupied: *mut u8`) had no
public C-layout contract outside the module. This item adds
`MapI64Header::entries(header) -> Vec<(i64, i64)>`, a `pub(crate)` reader in
`runtime.rs` that walks the same occupied-bitmap-filtered slots
`map_slot`/`map_insert`/`sol_map_get_i64` already read/write, and wires it
into `ValueRenderer::expand`'s new `Type::Map(key, value)` arm
(`crate/sol/src/debugger.rs`): each entry's label is the key rendered through
the same `ValueRenderer::render` (per `typeck.rs`'s `lower_type`, a map key's
type is always `I64` in the current M10 slice, so this always renders as a
plain decimal today, but goes through `render` rather than hand-formatting to
stay correct if that restriction loosens), paired with the value's raw word
and the map's static value type (`I64`/`F64`/`Bool` — M10's supported scalar
value types; pointer-bearing map values remain gated on precise GC layouts,
per `runtime.rs`'s own comment, unchanged by this item).

Checked directly before assuming `GetMetatable` applies: grepping
`metatable`/`Metatable` across the crate shows the concept exists only under
`lua_runtime/*` — the separate dynamic `.lua` tier, untouched by item 3 and
untouched here. The typed `.sol` tier this spike's `DebugSession` runs has no
metatable concept at all, so there is nothing for a `GetMetatable`-equivalent
to return; this is stated plainly rather than inventing a stub that always
answers "no metatable."

Test: `map_valued_local_expands_to_its_occupied_entries` builds a
`Map<i64, i64>` local populated by one map-literal statement (`{ [1] = 10,
[2] = 20 }`), confirms it renders as a `Reference` with summary `"Map<i64,
i64>"`, and confirms `expand` reports exactly the two occupied entries with
their correct values (sorted, since occupied-slot iteration order is not
source order).

**Deliverable 3 — `debugGetMemoryStats`/`ForceGc`.** Thin wrappers in
`DebugSession`: `memory_stats() -> MemoryStats { live_bytes, live_blocks }`
over `gc::live_bytes()`/`gc::live_blocks()`, and `force_gc()` over
`gc::collect()`.

Verified directly against `gc.rs` rather than assumed: `live_bytes`/
`live_blocks` are simple, non-panicking bookkeeping sums over the heap's
bump-arena chunks, unrelated to reachability — they never touch
`STACK_BASE`, so they are fully meaningful and safe in a jit-free
`DebugSession` context. `collect()` is a different story: `collect_heap`/
`collect_minor` both check `STACK_BASE == 0` and return immediately, *before*
even scanning `EXTRA_ROOTS` (the interpreter's own register-file GC roots,
registered via `RootGuard`) — and `STACK_BASE` is only ever initialized by
`gc::init_stack_base()`, called exclusively from the native JIT/AOT entry
path (`main.rs`/`aot.rs`), never from `tier0::Engine`/`interp::Runtime` (what
every `DebugSession` actually runs on). So `force_gc()` is safe to call (a
plain, non-panicking function call) but is a **complete no-op** in this
context: it reclaims nothing, ever, including genuinely unreachable garbage.
This is a real, pre-existing property of the jit-free execution path that
this item documents rather than works around — giving Tier-0 its own
stack-scanning root set would be a real, separate feature.

Note on the wire protocol's `MemoryStatsInfo` shape
(`apps/web/src/debug-protocol.ts`, read for reference only):
`totalAllocation`/`gcAllocation` map reasonably onto `live_bytes`, but there
is no Tier-0 equivalent for `externalAllocation`/`allocationDebt` — those
describe generational-GC bookkeeping concepts (`gc.rs`'s minor/major
promotion debt) that `DebugSession` does not currently expose beyond the
plain `live_bytes`/`live_blocks` totals; not invented here.

Test: `memory_stats_report_plausible_values_and_force_gc_does_not_crash` runs
an allocation-heavy fixture (a `Map<i64, i64>` grown to 2000 entries, forcing
several internal resizes), confirms `live_bytes`/`live_blocks` are non-zero
and non-decreasing after the run, then calls `force_gc()` and asserts the
stats are **unchanged** afterward — directly exercising, not just asserting
in prose, the documented no-op finding above.

**Verified outcome:**
`cargo test --manifest-path crate/sol/Cargo.toml` — 543 passed, 0 failed,
across 25 test binaries (538 from item 3 plus the 5 new tests in
`tests/debugger.rs`).
`cargo test --manifest-path crate/sol/Cargo.toml --no-default-features` —
536 passed, 0 failed, across 24 test binaries (531 from item 3 plus the same
5 new tests).
`cargo check --manifest-path crate/sol/Cargo.toml --no-default-features
--target wasm32-unknown-unknown` — clean, after `touch`ing `debugger.rs` and
`runtime.rs` first to rule out a stale-cache false pass. `scripts/test-lua55-manifest.sh`
— passes (`Lua 5.5 manifest regression checks passed`). All new code lives in
`debugger.rs`/`runtime.rs`, both declared in `lib.rs` with no
`#[cfg(feature = "jit")]` gate, so part of the same always-compiled,
always-wasm32-checked module set as item 3's own additions.

**Explicitly not done here:**
- No true interactive suspend/resume — unchanged from item 3; `evaluate`
  still only ever evaluates against a frozen trace snapshot, never a live
  paused frame.
- `debugSetVariable` — scoped out above, with the stated reasoning (a
  recorded trace's later steps cannot be retroactively affected by mutating
  an earlier step's own copy).
- Eval exposes only scalar-typed locals (`I64`/`F64`/`Bool`/`Nil`/`String`);
  `Array`/`Map`/`Struct`/`Function`/`Any`-typed locals, and references to the
  program's other top-level functions, are not reachable from an eval
  expression.
- No eval sandboxing against Tier-0 traps (e.g. division by zero aborts the
  whole process, inherited unchanged from `DebugSession::run`'s existing
  behavior).
- `GetMetatable` — does not apply to the typed `.sol` tier at all (no
  metatable concept exists there); not stubbed.
- `Any`-typed local unboxing in `ValueRenderer` — still not done, unchanged
  from item 3 (needs the same reverse tag→type registry noted there).
- No wasm-bindgen bindings, no `apps/web` changes, no worker-protocol
  wiring — out of this item's explicit scope, not attempted.
- `externalAllocation`/`allocationDebt` (the wire protocol's
  `MemoryStatsInfo` fields beyond `live_bytes`/`live_blocks`) have no
  implemented equivalent here — noted above, not invented.

## Work item 5 — profiling, execution timeline, and the coroutine/thread scope boundary

**Goal:** extend item 3/4's native, non-wasm `DebugSession` spike with
`profile`/`debugGetThreads`-equivalent coverage, still a native Rust spike
(no wasm-bindgen bindings, no `apps/web` changes, no worker-protocol
wiring), built directly on item 3's `TraceStep`/`Hooks` recording
machinery.

**Correction to this item's own plan text: `debugGetThreads`/per-thread
stack scoping has no applicable target in this spike.** The plan's wording
("reuse whatever Sol's own coroutine representation — `closures.rs`/`gc.rs`
— already exposes for suspended-thread stacks... confirm it's walkable
per-thread") assumes the typed `.sol` tier has some coroutine/thread
representation to walk. Checked directly before assuming otherwise (not
just trusting a prior grep summary): `grep -rn
"coroutine\|Coroutine" crate/sol/src/*.rs` outside `lua_runtime/*`/
`lua_bytecode/*` turns up exactly two files —
- `interp.rs:795-863`: a mixed-module `#[cfg(test)]` helper
  (`dynamic_coroutine_round_trip_through_a_typed_boundary`-style test) that
  calls `runtime.create_global_coroutine(name)`/
  `runtime.resume_coroutine_outcome(handle, arguments)` from typed `.sol`
  code through the FFI bridge — i.e. invoking the *separate dynamic `.lua`
  tier's own* coroutine machinery from a typed caller, not a typed-tier
  coroutine primitive of its own. Read in full to confirm: the coroutine
  object itself (`create_global_coroutine`) is created and resumed entirely
  within `LuaRuntime`/`lua_runtime/*`'s dynamic interpreter; the typed side
  only ever sees an opaque handle and an integer result.
- `main.rs`: one comment plus one error message
  ("attempt to yield from main outside a coroutine") about *dynamic*
  `.lua` yielding outside a coroutine — also part of the
  dynamic/mixed-module path (`main.rs`'s mixed native/dynamic entry point),
  not the typed Tier-0 engine `DebugSession` wraps.

`closures.rs`/`gc.rs` (the plan's cited location) were also read directly:
neither defines or references a coroutine/thread/fiber concept at all —
`closures.rs` is escape analysis for typed-tier closures (lambda-lifting,
scalar replacement), `gc.rs` is the tracing collector, and `sol-core`'s own
"suspended thread stacks are part of reachability semantics" invariant (the
plan's cited justification) describes the *dynamic* `.lua` tier's
coroutines (`lua_runtime/canonical.rs`'s adapter), which do root a
suspended Lua thread's stack — not anything the typed tier or
`tier0::Engine` touches.

So: the typed `.sol` language and its Tier-0 bytecode interpreter
(`interp.rs`'s main dispatch, `bccompile.rs`, `types.rs`) have **no
coroutine/thread concept of their own at all** — "coroutine" in this
codebase is exclusively a dynamic-`.lua`-tier concept, confirmed separately
by `docs/phase-4-8-implementation.md`'s own coroutine-debugging examples
(`Thread::debug_frames`/`debug_read_register`, `Executor::debug_thread_stack`)
all being about the retired `crates/vm`/piccolo-fork `.lua` runtime, not Sol's
typed tier. `DebugSession` (item 3/4) only ever runs a typed `TProgram`
through `tier0::Engine` — confirmed again here, not just inherited as an
assumption: `tier0::Engine::call`/`call_outcome` is one synchronous Rust
call with no suspended-coroutine state to enumerate, by construction. There
is therefore nothing to "confirm is walkable per-thread": the typed tier
this `DebugSession` wraps is single-threaded unconditionally, not merely
"currently only exercises one thread in these fixtures." Per this item's own
instructions, no fake/stub multi-thread model was invented to satisfy the
plan's wording.

**A small, honestly-scoped addition anyway:** `DebugSession::threads()`
(`crate/sol/src/debugger.rs`) always returns exactly one `ThreadInfo { id:
0, status: "running" }` — a constant, single-element report, loosely
shaped after `apps/web/src/debug-protocol.ts`'s `ThreadInfo` (`id`,
`status`) for wire-protocol-shape compatibility if a later item wants it.
Its doc comment states explicitly, in the same place a caller would read
it, that this is not real multi-thread/coroutine support — not a disguised
stub pretending otherwise. Covered by a trivial unit test
(`threads_always_reports_exactly_one_main_thread`,
`crate/sol/src/debugger.rs`'s `#[cfg(test)]` module).

**The profiling/timeline half of the plan is the real substance of this
item, and it did hold up:** item 3's `TraceStep`/`TraceRecorder`/`Hooks`
infrastructure already records every bytecode instruction visited
(`func_id`, `pc`, `line`, `depth`) during one complete run — already
"record everything, don't pause," the plan's own suggested approach for
this part. Built directly on it, no second instrumentation path.

**`DebugSession::profile(function_name, args) -> Vec<FunctionStats>`**
(`crate/sol/src/debugger.rs`): a one-shot instrumented run (resets and
re-runs `function_name(args)` via the same `run` mechanism item 3 built),
then aggregates the resulting trace. `FunctionStats { function_name, calls,
self_instructions, total_instructions }` is loosely shaped after
`apps/web/src/debug-protocol.ts`'s `FunctionStatsInfo` (`functionId`,
`calls`, `totalInstructions`, `selfInstructions`), read for field-naming
guidance only — this stays a native spike, no wasm-bindgen bindings, no
`apps/web` changes, same boundary item 3/4 kept.

Attribution choice, stated explicitly per the task's own request to pick
one and document why: **`self_instructions`** = the count of recorded
`TraceStep`s whose `func_id` is this function — instructions dispatched
while this function's own frame was the one actually executing, excluding
anything spent inside a callee (which records its own, different `func_id`
instead). **`total_instructions`** = `self_instructions` plus every
instruction recorded while any call this function made, directly or
transitively, was active. This is the standard self-time/total-time split
(and matches the retired `crates/lua-vm/src/profiler.rs`'s own
`self_instructions`/`total_instructions` design, confirmed by reading
`docs/phase-4-8-implementation.md`'s "Profiler" section: "on a return...
add `duration - child_instructions` to `self_instructions`... credit the
duration to its caller's `child_instructions`" — the same self-vs-total
distinction, re-derived here from a full trace rather than tracked
incrementally during a single pass, since this item already has a full
trace sitting in memory and does not need to re-earn the same bookkeeping
during execution). Implementation walks the trace once, reconstructing a
call stack (`Vec<func_id>`, one entry per active depth) purely from each
step's own `depth`/`func_id`: a step whose depth is one deeper than the
stack's current height pushes a new frame and charges one `calls` to that
function (this includes the session's own top-level call, which always
gets exactly one); a step whose depth is shallower truncates the stack back
down with no new call charged (continuing an already-counted ancestor frame
after a nested call returned); a step at the *same* depth as the stack's
current top but a *different* `func_id` is a tail call
(`TraceStep::depth`'s own doc comment: tail calls reuse the caller's depth)
— the old frame is replaced in place and the new function is charged one
`calls`. Every step then adds one to `self_instructions` for its own
`func_id` alone, and one to `total_instructions` for every function
currently on the reconstructed stack (itself plus every live ancestor).

**`DebugSession::record_timeline(function_name, args) -> Vec<TimelineEvent>`**
(`crate/sol/src/debugger.rs`): same one-shot-run mechanism, deriving a
chronological event list from the same trace's `depth`/`func_id`
transitions rather than a second recording pass. `TimelineEvent { kind,
function_name, depth, step_index }` is loosely shaped after
`apps/web/src/debug-protocol.ts`'s `TimelineEventInfo` (`eventType`, `line`,
`duration`, ...), read for field-naming guidance only. `TimelineEventKind`
covers `CallEnter`/`CallExit` (the task's stated minimum) plus `TailCall`
(a same-depth, different-`func_id` transition, which falls directly out of
the same walk at no extra cost — not a separate line-level "line stepped"
event, since a caller wanting per-line detail already has `trace()` itself,
which carries a per-instruction `line`). The one synthetic event:
the top-level call's own closing `CallExit` has no following instruction to
observe it at (the run is over once it returns), so it reports
`trace().len()` — one index past the end, not a real trace index; every
other event's `step_index` is a real index into `trace()`.

**Differential tests** (`crate/sol/tests/debugger.rs`):
- `profile_attributes_self_and_total_instructions_correctly_across_a_looped_call`:
  a leaf function (`helper`) called 3 times from a loop in `main`. Cross-checks
  `profile`'s `calls`/`self_instructions` against an independent recount
  directly over the raw trace (not by calling `profile` again) — the same
  cross-checking spirit as item 3's breakpoint test. Also asserts three
  algebraic identities the attribution algorithm must satisfy, checked
  directly rather than only asserted in prose: a leaf function's
  `total_instructions` equals its `self_instructions`; the top-level
  function's `total_instructions` equals the whole trace's length; and
  summing `self_instructions` across every returned function equals the
  trace's length too (every instruction belongs to exactly one frame's self
  time).
- `record_timeline_produces_the_expected_call_enter_exit_sequence`: a
  `f(); g(); f();` sequential-call fixture (no nesting, no recursion) —
  asserts the exact `(kind, function_name)` sequence end to end
  (`CallEnter(main)`, `CallEnter(f)`, `CallExit(f)`, `CallEnter(g)`,
  `CallExit(g)`, `CallEnter(f)`, `CallExit(f)`, `CallExit(main)`), and that
  the final synthetic `CallExit`'s `step_index` is exactly `trace().len()`.

**Verified outcome:**
`cargo test --manifest-path crate/sol/Cargo.toml` — 546 passed, 0 failed,
across 25 test binaries (543 from item 4 plus 2 new integration tests in
`tests/debugger.rs` and 1 new unit test in `src/debugger.rs`).
`cargo test --manifest-path crate/sol/Cargo.toml --no-default-features` —
539 passed, 0 failed, across 24 test binaries (536 from item 4 plus the same
3 new tests).
`cargo check --manifest-path crate/sol/Cargo.toml --no-default-features
--target wasm32-unknown-unknown` — clean, after `touch`ing `debugger.rs` and
`tests/debugger.rs` first to rule out a stale-cache false pass.
`scripts/test-lua55-manifest.sh` — passes (`Lua 5.5 manifest regression
checks passed`). All new code lives in `debugger.rs`/`tests/debugger.rs`,
both already part of the always-compiled, always-wasm32-checked module set
established by items 3/4.

**Explicitly not done here:**
- Real per-thread/coroutine stack scoping — no applicable target in this
  spike, per the finding above; `DebugSession::threads()` is a constant
  one-thread shape-compatibility accessor, not a step toward real support.
- No wall-clock timing for `profile`/`record_timeline` — instruction counts
  only, the same timing proxy this codebase already uses everywhere
  (`MAX_INSTRUCTIONS`), and for the same reason the retired `lua-vm`
  profiler gave for choosing it (stable run to run, unlike wall-clock time
  under a WASM-hosted interpreter with no JIT warmup to account for).
- No bounded/truncating recording for `record_timeline` — the retired
  `crates/lua-vm/src/debug_events.rs`'s `max_events`/`truncated` cap (for an
  unboundedly long-running script) has no equivalent here; this spike's
  trace is already fully materialized in memory by `run`/`DebugSession`
  before `record_timeline` ever sees it, so there is no separate recording
  pass to cap — a real concern for a long-running script, left to whichever
  later item actually wires this to a worker/browser context with a memory
  budget.
- No wasm-bindgen bindings, no `apps/web` changes, no worker-protocol
  wiring — out of this item's explicit scope, not attempted, same as every
  prior item in this milestone.

## Work item 6 — canonical WASM playground and debugger

**Goal:** take items 1-5's native, non-wasm `DebugSession` spike and give it
a real product surface: `packages/sol-runtime` (a wasm-bindgen package built
from `crate/sol`, parallel to `packages/lua-runtime`), a build script
(`scripts/build-sol-wasm.sh`), new `#[wasm_bindgen]` entry points
(`crate/sol/src/wasm_api.rs`), and a minimal, default-off wiring point in
`apps/web` proving the whole chain actually runs in a browser.

**The product surface** (`crate/sol/src/wasm_api.rs:1-595`, below the
item-2 `run_sol` spike which stays unchanged): a non-throwing
`execute(source: &str) -> ExecuteResult { result: Option<String>, error:
Option<String> }`, and `WasmDebugSession` wrapping
`crate::debugger::DebugSession` with a one-run-per-session convention
matching the native type. Wire-shape mirror types
(`WasmValue`, `WasmBreakpoint`, `WasmTraceStep`, `WasmLocalView`,
`WasmTableEntry`, `WasmEvalResult`, `WasmMemoryStats`, `WasmThreadInfo`,
`WasmFunctionStats`, `WasmTimelineEvent`, `WasmBurstResult`) each carry
private fields plus `#[wasm_bindgen(getter)]` accessors, the pattern
established by the retired `crate/lua-vm/src/session/types.rs`.
`WasmDebugSession` exposes: `launch`, `run`, `trace_length`/`trace_step`,
`set_breakpoint`, `first_breakpoint_hit`/`continue_to_breakpoint`,
`continue_burst`, `step_into`/`step_over`/`step_out`, `locals_at`/`expand`,
`evaluate`, `memory_stats`/`force_gc`, `threads`, `profile`,
`record_timeline` — the full item 3-5 feature set, modulo the honest gaps
below.

**Honest scope, stated in `wasm_api.rs`'s own top-of-file doc comment
(lines 29-127) and restated here with the file:line evidence behind each
claim:**

- **Single-file `.sol` source only, no multi-file project loading.**
  `crate::modules::compile_project`/`Loader` resolve `import`s via real
  `std::fs::read` + path canonicalization (`modules.rs:4,
  lua_runtime/mod.rs` pattern). `modules.rs` is not feature- or
  target-gated, so it already *compiles* for `wasm32-unknown-unknown` —
  but it would *fail at runtime*: `apps/web`'s worker protocol hands in
  in-memory `names: string[]`/`contents: string[]` with no backing
  filesystem, and `wasm32-unknown-unknown`'s `std::fs` backend returns an
  `Unsupported` `io::Error` for every operation rather than acting as a
  working no-op. Parameterizing `Loader::load` over an injected
  `Fn(&Path) -> Result<Vec<u8>, String>` instead of a hardcoded
  `fs::read` call is a real, bounded follow-up, deliberately not attempted
  here so this item lands a working single-file slice rather than a
  half-wired multi-file path.
- **`.sol` source only, not idiomatic `.lua`.** `compile_bytes` with
  `parser::SourceMode::Lua` does route through the same `TProgram`/
  `tier0::Engine` pipeline (`lib.rs::compile_program_with_config`: both
  profiles share `typeck::check`, just with `allow_dynamic_fallback`
  flipped) — but verified directly, not assumed: a bare `print("hello")`
  fails to even typecheck via this path (`"unknown function 'print'
  [EDYNLUA]"`), because `print`/Lua's standard library are wired up only
  inside the separate, dynamic `LuaRuntime` (`lua_runtime/init.rs`'s
  global-table setup), which this path never constructs. So `.lua` source
  is not hard-rejected, but nothing here is tested or product-ready for it.
- **No output-buffer/`take_output` equivalent, for either language.**
  Checked directly: typed `.sol` has no `print`/`io.write`/any builtin that
  writes anything at all reachable from `.sol` source — confirmed by
  grepping `"print"` across `typeck.rs`/`codegen.rs` (zero matches: no
  `print` builtin is wired into the typed language at all). The
  `#[no_mangle] sol_print_i64`/`sol_print_f64`/`sol_print_bool`/
  `sol_print_string`/`sol_print_any` functions (`runtime.rs:271-284`,
  `strings.rs:147-154`, `dynamic.rs:188-202`) do get compiled into the
  wasm32 build and appear as harmless *exports* in `sol_bg.wasm` (not
  reachable imports) — but their only call sites in the whole crate are
  `aot.rs` (jit-feature-gated, excluded from this build) and `main.rs` (the
  CLI binary, `required-features = ["jit"]`, also excluded) — confirmed by
  grepping every call site of each symbol. Nothing in `wasm_api.rs`'s own
  call graph (`crate::debugger`, `crate::types` only — confirmed by its own
  `use` list) reaches them. A further, previously-undocumented finding
  worth flagging precisely because it would otherwise look like silent data
  loss: `sol_print_string` (`strings.rs:151`) calls `std::io::stdout()`,
  and `wasm32-unknown-unknown`'s std backend treats stdout as an inert
  no-op sink (writes succeed, bytes go nowhere) rather than erroring — so
  if a future item ever wires a `print` builtin into typed `.sol`, calling
  it in the browser would not crash and would not raise `wasm_api.rs`'s own
  `env` import problem (see below), but would also produce **zero visible
  output** without further work to redirect it through a `console.log`
  import. Not a problem today (unreachable), but a trap for whoever adds
  that builtin later if this note is skipped.
- **No globals/upvalues/metatables, no `set_variable`, no breakpoint
  conditions/hit-conditions/log-messages/removal** — items 3/4's own
  findings restated here because this file is the first place those gaps
  become *missing wasm methods* rather than prose. Conditions in
  particular look implementable without a live interpreter (a predicate
  evaluated via `DebugSession::evaluate` at each candidate trace index —
  the whole trace already exists), but doing it honestly means extending
  `debugger.rs`'s `breakpoints` storage and its
  `continue_to_breakpoint`/`first_breakpoint_hit` matching logic with its
  own differential tests — a real, bounded follow-up left for later.
- **`continue_burst` is implemented, honestly bounded.** Unlike the gaps
  above, this one needs no `debugger.rs` change at all:
  `tier0::DEFAULT_INSTRUCTION_BUDGET` (10,000,000, unconditionally enforced
  inside `interp::Runtime::interpret`'s dispatch loop since item 2) already
  makes `DebugSession::run` incapable of hanging forever. `continue_burst`
  pages through chunks of that already-complete, already-bounded trace —
  not truly incremental execution, stated in its own doc comment
  (`wasm_api.rs`'s `WasmDebugSession::continue_burst`) as well as here.

**A real, previously-undetected defect found and fixed in the course of
this item: the `lua_runtime`/`c_api` wasm32 `env` import leak.**
`cargo check --no-default-features --features wasm --target
wasm32-unknown-unknown` was already clean *before* this item started (item
1's baseline) — but `cargo check` never performs the final link step, so
it cannot detect an unresolved-extern-symbol problem. Actually building
`packages/sol-runtime/pkg` via `scripts/build-sol-wasm.sh` and inspecting
the real output revealed one: `lua_runtime::c_api` (`crate/sol/src/
lua_runtime/c_api.rs`) implements a real embeddable Lua C API
(`luaL_newstate`, `lua_pushfstring`, etc.) as `#[no_mangle] pub extern "C"
fn`s, which Rust never dead-code-eliminates (`#[no_mangle]` makes a symbol
externally reachable by definition, regardless of whether anything in the
crate calls it). Those functions reference `sol_c_api_shim_anchor`
(`c_api.rs:727-728,746,759`, resolved only by `build.rs`'s **host**-only C
compile of `c_api_shim.c` via raw `Command::new("cc")` — not the `cc`
crate, so it ignores Cargo's `TARGET` and always compiles for the host
architecture) and `lua_runtime/c_api/auxlib.rs:561-575`'s raw
`extern "C" { fn malloc/free/realloc }` declarations (meant to resolve
against a real host libc). `wasm32-unknown-unknown` has no implicit libc
and no native shim, so these stayed unresolved, surfacing as required
`env.*` imports on the compiled `.wasm` — confirmed via `strings -a
sol_bg.wasm | grep -iE "malloc|free|realloc|sol_c_api_shim_anchor"`
(all four present) and via `sol.js` containing
`import * as __wbg_star0 from 'env';`, an unsatisfiable bare module
specifier in a browser (there is no real `env` module to import from).

**Fix:** `crate/sol/src/lib.rs:31-65` gates `pub mod lua_runtime;` behind
`#[cfg(not(target_arch = "wasm32"))]` — a target-arch gate, not a new
Cargo feature, chosen specifically because a feature-based approach risks
the mandatory `--no-default-features` native baseline: `--no-default-features`
disables every feature in `default`, so a new feature meant to be "on
under `--no-default-features` but off for wasm32" cannot be expressed as a
feature at all without a non-feature condition backing it — `target_arch`
affects only wasm32 builds and leaves every native invocation, regardless
of feature flags, untouched. `lua_runtime` (the dynamic `.lua` interpreter
this gates) is not reachable at all from `wasm_api.rs`'s own entry points
regardless of this fix (see the `.lua`-source finding above), so excluding
it from the wasm32 build loses nothing this item claims to support.
Verified empirically, not just by re-running `cargo check`: after the fix,
rebuilding `packages/sol-runtime/pkg` and re-running the same `strings`/
`sol.js`-grep checks found zero matches for
`malloc`/`free`/`realloc`/`sol_c_api_shim_anchor`, zero `import` statements
of any kind in `sol.js`, and the release `sol_bg.wasm` shrank from 1.7 MB
to 726.8 KB (`lua_runtime` actually leaving the binary, not just becoming
unreachable dead code the linker still happened to keep).

**Capabilities audit** (zero ambient I/O reachable from `execute()`/
`WasmDebugSession`, confirmed by grep across the whole crate, not assumed
from the module list): `wasm_api.rs` only imports `crate::debugger` and
`crate::types`; `debugger.rs` only imports `crate::gc`, `crate::interp::
Hooks`, `crate::tier0`, `crate::types`. None of the modules actually on
that call graph (`lexer.rs`, `parser.rs`, `ast.rs`, `typeck.rs`,
`verify.rs`, `optimize.rs`, `escape.rs`, `aliases.rs`, `closures.rs`,
`bytecode.rs`, `bccompile.rs`, `interp.rs`, `tier0.rs`, `gc.rs`,
`debugger.rs`, `types.rs`, `value.rs`, `strings.rs`, `numeric.rs`) contain
any `std::fs`/`std::net`/`std::process::{exit,Command}` usage at all,
confirmed by a crate-wide grep for each. What the grep *did* turn up,
and why each is harmless or already excluded:
- `aot.rs`, `tier.rs`, `jit.rs`, `codegen.rs`, `main.rs` — all
  `#[cfg(feature = "jit")]`-gated at their `lib.rs` module declaration (or,
  for `main.rs`, a separate `[[bin]]` with `required-features = ["jit"]`),
  so none of them compile at all under this item's `--no-default-features
  --features wasm` build. (Their own `std::fs`/`std::env`/`std::process`
  usage — AOT object-file writes, profile-file I/O, `SOL_*` tuning env
  vars — is real but entirely absent from this build.)
- `lua_runtime/*` — now excluded from any wasm32 build in full (the fix
  above), so its own `std::fs` (module loading, `io.open`/`io.lines`),
  `std::process::exit`/`std::process::id` (`os.exit`/`os.tmpname`), and
  `std::io::stdout/stdin/stderr` (`print`/`io.write`/`io.read`) are not
  merely unreachable from `wasm_api.rs` — they are not compiled in at all
  for that target.
- `dynamic.rs`/`interp.rs`/`runtime.rs`'s `std::process::abort()` calls —
  intentional traps on an internal invariant violation (an impossible
  type-tag value, a GC allocation-size overflow), the same category
  `debugger.rs:684`'s own doc comment already documents as "calls `trap()`"
  for the zero-instruction-budget case — equivalent to a wasm
  `unreachable` trap that halts the module, not ambient OS process
  control.
- `gc.rs:733-737`'s `std::env::var_os("SOL_GC_DEBUG")` — a read-only
  diagnostic toggle; a browser has no process environment, so this always
  reads `None` and silently falls back to default (non-debug) behavior.
- `runtime.rs:271-284`/`strings.rs:147-154`/`dynamic.rs:188-202`'s
  `sol_print_*` functions — covered above (compiled in as harmless unused
  exports, unreachable from `.sol` source, and, for the one that touches
  `std::io`, a no-op on this target even if it were somehow reached).

**`packages/sol-runtime`** (`packages/sol-runtime/package.json`,
`scripts/build-sol-wasm.sh`): parallel to `packages/lua-runtime`'s
pre-wasm-pack shape — a raw `cargo build --release --no-default-features
--features wasm --target wasm32-unknown-unknown` against
`crate/sol/Cargo.toml`, then the standalone `wasm-bindgen` CLI
(`--target web`) into `packages/sol-runtime/pkg`. npm name
`@lua-playground/sol-runtime`, matching `@lua-playground/runtime`'s naming
convention.

**`apps/web` wiring — a single, default-off import-site switch**
(`apps/web/src/lua-worker.ts`): added one static import
(`import initSol, { execute as executeSol } from
"@lua-playground/sol-runtime"`) and one module-level constant,
`SOL_ENGINE_ENABLED = import.meta.env.VITE_SOL_ENGINE === "1"`, read once.
Default (the env var unset) leaves every existing message handler,
including every other branch of the `"run"` case, completely unchanged.
When the flag is on *and* the worker's `"run"` message carries exactly one
file whose name ends in `.sol`, that one case is answered by `executeSol`
instead of `execute_project`, converting `ExecuteResult` into the same
`WorkerEvent` shape the existing path already produces. Every other
combination (multiple files, a `.lua`/other entry, or the flag off) falls
through to the pre-existing `@lua-playground/runtime` path, byte-for-byte
unchanged — this is intentionally the one worker message with a direct,
honest equivalent on the new engine's surface (`execute()`'s own doc
comment: "matching `crate/lua-vm`'s own `execute()` calling convention so
`apps/web`'s existing `"run"` worker-message handler can be adapted to
call this with a minimal diff").

One caveat worth stating plainly: because the new import is static (not a
dynamic `import()`), `sol_bg.wasm` (~744 KB raw, ~233 KB gzipped) is now
bundled as a binary asset in every build's `dist/assets/`, regardless of
the flag — it is only ever actually *fetched over the network* by a real
browser when `init()`/`initSol()` runs, which only happens when the flag
is on, but it does add to the built artifact's on-disk footprint
unconditionally. Scoped as acceptable for this item (a working, provably
real wiring point) rather than invested in code-splitting the worker
bundle, which is a separate, pre-existing concern this app already flags
in its own build output ("Some chunks are larger than 500 kB...").

**Verified outcomes:**
- `cargo test --manifest-path crate/sol/Cargo.toml` (default features,
  `jit` only) — 546 passed, 0 failed, across 25 test binaries. Identical
  total to item 5's baseline: `wasm_api.rs` is behind `#[cfg(feature =
  "wasm")]`, not part of this build at all.
- `cargo test --manifest-path crate/sol/Cargo.toml --no-default-features`
  — 539 passed, 0 failed, across 24 test binaries. Identical total to item
  5's baseline, and specifically re-run *after* the `lib.rs` `lua_runtime`
  wasm32-gating fix to confirm that a target-arch gate (as opposed to a
  feature gate) cannot regress a native, non-wasm32 build under any
  feature combination — confirmed.
- `cargo test --manifest-path crate/sol/Cargo.toml --no-default-features
  --features wasm` (native, exercising the new surface's own tests) — 546
  passed, 0 failed, across 24 test binaries: 137 unit tests (130 existing
  + 7 new in `wasm_api.rs`'s own `#[cfg(test)]` module, exercisable
  natively because `#[wasm_bindgen]` compiles as plain Rust off
  `wasm32`), the remainder identical to the `--no-default-features`
  baseline above.
- `cargo check --manifest-path crate/sol/Cargo.toml --no-default-features
  --features wasm --target wasm32-unknown-unknown` (the lib target,
  the item's actual required baseline) — clean.
- The same check with `--tests` added fails — expected and not a defect:
  the native-only integration tests `tests/lua55_dynamic_runtime_*.rs`
  reference `sol::lua_runtime` directly (that is what they test), and
  `lua_runtime` is correctly excluded from wasm32 by this item's own fix.
  Those tests were never meant to target wasm32 and are not part of this
  item's required baseline; the native `cargo test` runs above already
  compile and pass them.
- `bash scripts/build-sol-wasm.sh` — succeeds, producing
  `packages/sol-runtime/pkg/{sol.js, sol.d.ts, sol_bg.wasm (726.8 KB),
  sol_bg.wasm.d.ts}`. Re-inspected post-fix per the "fix" paragraph above:
  no `env` import, no `malloc`/`free`/`realloc`/`sol_c_api_shim_anchor`
  symbols, zero `import` statements of any kind in `sol.js`.
- `npm run build --workspace=apps/web` — passes both with the flag at its
  default (unset/off) position and with `VITE_SOL_ENGINE=1` set; `tsc -b`
  (part of that script) reports no type errors from the new code in
  either position.
- **Real-browser verification**, not merely a build check: served the
  `VITE_SOL_ENGINE=1` production build via `vite preview`, launched a
  headless Chromium (Playwright, already a devDependency) against it, and
  instantiated the real built worker chunk
  (`dist/assets/lua-worker-*.js`) directly, posting the exact `"run"`
  worker message `apps/web`'s UI itself sends. Two cases, both against the
  actual shipped bundle, not a mock: `function main(): i64 return 40 + 2
  end` → `{"output":"42","error":null}` (the success path, genuinely
  executed inside the browser's own wasm instance); a deliberately
  ill-typed program → `{"output":"","error":"line 2: expected type i64,
  found f64 [ETYPE001]","errorSource":"main.sol"}` (the error path,
  correctly surfaced through to the worker's `"result"` event rather than
  throwing an uncaught exception). Confirms genuine end-to-end
  reachability: wasm32 build → `wasm-bindgen` glue → bundled worker →
  real browser execution.
- `scripts/test-lua55-manifest.sh` — passes (`Lua 5.5 manifest regression
  checks passed`), unaffected.

**Explicitly not done here** (beyond the honest-scope list above):
- No code-splitting/dynamic `import()` for `@lua-playground/sol-runtime`
  in `apps/web` — the static-import bundle-size caveat above is accepted,
  not solved, in this item.
- No UI affordance to toggle `VITE_SOL_ENGINE` at runtime — it is a
  build-time env var today, set before `npm run build`/`npm run dev`, not
  a user-facing switch; adding one is a separate, small follow-up once the
  underlying engine gap list above is smaller.
- No multi-file `.sol` project support, no idiomatic `.lua` support, no
  output buffer, no globals/upvalues/metatables/`set_variable`, no
  breakpoint log messages — see the
  honest-scope list above for each; none were invented as disguised stubs.

## Work item 7 — breakpoint lifecycle and trace filters

The product wrapper in item 6 deliberately shipped only bare source-location
matching. This follow-up completes the bounded debugger slice that does not
require a live suspend/resume interpreter: each `VerifiedBreakpoint` now has a
stable session-local ID; `DebugSession` retains every requested breakpoint
(including an unverified one) and supports removal, an optional boolean
condition, and an optional minimum hit count. `first_breakpoint_hit` and
`continue_to_breakpoint` use the same matcher, so their behavior cannot drift.

Conditions are evaluated against the candidate recorded trace step through the
existing frame-scoped evaluator. They deliberately use its documented
`local<N>` names rather than source identifiers, and a malformed or non-boolean
condition is non-matching rather than a debugger crash. Hit counts are raw
candidate source-location visits in the immutable recorded trace. The
WASM wrapper exposes the stable ID plus `remove_breakpoint`,
`set_breakpoint_condition`, and `set_breakpoint_hit_condition`; this is an API
completion only, not worker wiring or a claim of live execution pausing.

`crate/sol/tests/debugger.rs` covers condition evaluation against the exact
candidate register snapshot, hit-count filtering, removal, and unknown IDs.
`wasm_api.rs` covers the corresponding exported lifecycle. Breakpoint log
messages remain open because the Tier-0 wrapper has no debugger-output event
channel; emitting a fabricated log would be a stub rather than support.

## Work item 8 — in-memory `.sol` project loading

`modules::compile_project_from_sources` adds the browser-safe counterpart to
filesystem project loading. It accepts an entry path and a complete in-memory
file set, resolves dotted imports only within that set, and shares the existing
module validation and namespace lowering with native projects. Virtual paths
must be non-empty and relative; absolute paths and `..` traversal are rejected.
Tier-0 rejects imported `.lua` modules explicitly because the browser build has
no dynamic Lua runtime or native capability fallback.

`wasm_api::execute_project(entry, names, contents)` exposes this path using
the worker protocol's parallel file-array shape and preserves `execute()`'s
non-throwing result contract. Regression coverage proves a nested in-memory
import reaches the expected result and that an escaping entry path is rejected.
After regenerating `packages/sol-runtime/pkg`, the opt-in worker route now
uses this API for every all-`.sol` project (and keeps `.lua` or mixed projects
on the compatibility runtime). A real headless-Chromium run against the
production worker bundle confirmed a nested `math.base` import returns `42`.
Debugger project sessions remain on the compatibility runtime.

## Work item 9 — in-memory project debugger sessions

`WasmDebugSession::launch_project(entry, names, contents)` now builds a
canonical debugger session from the same in-memory typed project graph as
`execute_project`. The session retains the existing Tier-0 limitations, but
breakpoints can target namespace-lowered imported functions (for example,
`math.base.answer`) as well as `main`. The web debug protocol remains on the
compatibility runtime: its stack, mutable-variable, globals, and output APIs
do not yet have equivalent canonical project-session methods. A focused WASM
API regression launches a two-file project, breaks in its imported function,
and verifies the final result is `42`.

## Work item 10 — defer canonical WASM loading on the compatibility path

The worker now type-imports the canonical package and dynamically imports it
only when `VITE_SOL_ENGINE=1`; initialization of the old runtime is unchanged.
The default production build no longer emits `sol_bg.wasm` and its worker is
25.12 KB, while the opt-in build includes the 808.54 KB raw / 254.52 KB gzipped
canonical WASM asset and a 42.59 KB worker. This removes the prior static-import
cost from the normal compatibility build without claiming a runtime UI toggle.
A real headless-Chromium production-worker smoke test confirms the deferred
module loads and runs the in-memory nested-import project to `42`.

## Work item 11 — canonical typed-project debugger worker route

When `VITE_SOL_ENGINE=1`, `apps/web/src/lua-worker.ts` now launches
`WasmDebugSession::launch_project` for all-`.sol` debugger requests as well
as using `execute_project` for normal runs. The adapter preserves the existing
worker protocol for the Tier-0 capabilities that have honest equivalents:
breakpoint lifecycle/filters, stepping over the recorded trace, one main
thread, current-frame locals, array/map/struct expansion, frame-scoped eval,
memory statistics, and the existing one-shot profiling/timeline requests. The
WASM trace now includes its lowered function name,
so stack entries are intelligible rather than exposing numeric bytecode IDs.

The adapter keeps an opaque `type_id` for each value reference returned by the
WASM API, allowing later `debugGetTableEntries` messages to call the canonical
lazy expander without duplicating Rust layouts in JavaScript. Entry-file
breakpoints map to the required typed `main` function; imported functions can
be targeted by their namespace-lowered name. A browser regression in
`apps/web/e2e/sol-engine-differential.mjs` launches a real production worker,
sets an entry breakpoint, reads `local0`, and evaluates `local0 + 2` to `42`.

This is intentionally not a claim of full debugger parity. Tier-0 still
records an entire bounded execution before reporting its first stop, so there
is no live pause; it exposes only the active trace frame rather than a full
live stack; typed lowering has no upvalue/global scope; and variable mutation,
metatables, print output, and breakpoint log messages remain unsupported.
Canonical timeline events currently carry the entry source and instruction
delta but not a recovered source line or `local0` payload.
Those requests either report an explicit error or their protocol's empty
meaningful result, never silently fall back to the retired runtime for a
canonical `.sol` project. `.lua` and mixed projects remain on the compatibility
runtime until the dynamic canonical runtime has a browser-safe product path.

## Work item 12 — canonical dynamic Lua execution in WASM

The canonical package now compiles `lua_runtime` for `wasm32` instead of
excluding the entire dynamic runtime. The native Lua C embedding ABI remains
present for native builds but its `no_mangle` exports are not wasm roots, and
`build.rs` does not build/link the host C shim for wasm. This keeps the
interpreter, tables, closures, coroutines, standard-library output buffer and
capability checks available in the browser without admitting native loading or
host allocation imports.

`execute_lua` runs a single Lua chunk under the runtime's default-deny
capabilities and returns its captured output. `execute_lua_project` registers
all supplied non-entry `.lua` files as exact dotted `require` names (for
example `math/base.lua` becomes `require("math.base")`) and runs the supplied
entry entirely from memory. The opt-in worker now routes all-Lua run projects
through these canonical APIs. Focused regressions cover captured `print`
output and a nested in-memory `require` returning `42`.

This closes canonical browser execution for ordinary all-Lua run projects,
but not the dynamic debugger protocol, mixed `.lua`/`.sol` module calls, or
the final removal of `@lua-playground/runtime`; those retain explicit scope
until equivalent canonical adapters and end-to-end differentials exist.

## Work item 13 — live canonical Lua debugger and browser qualification

The opt-in worker now routes all-Lua debug, profiling, and timeline projects
to the canonical runtime as well as normal runs. `LuaDebugSession` retains
the actual trampoline frames between instruction slices; it does not run the
program ahead of a stop or replay a recorded trace. Breakpoints use executable
source lines, including nested registered module paths. Continue, burst,
step-into/over/out, conditional and hit-count breakpoints, named locals,
interpolated logpoints with ordered output,
upvalues, full live Lua frames, table/metatable expansion, frame-scoped
evaluation, local/upvalue editing, output polling, and forced precise GC are
exposed through `WasmLuaDebugSession` and the existing worker protocol.
Inspector references retain their canonical objects until session disposal.
Replacing a session releases the previous WASM session and inspection roots.

Registered `require` loaders have an explicit native continuation so a module
can stop before its next instruction and resume with its cache publication
still pending. Error unwinds clear the partial module cache before an outer
protected call retries it. The actual project filename is retained rather
than inferred from a module's dotted name. Project validation rejects escaping,
duplicate, ambiguous, and unsupported file paths, and run requests validate
the entry even for a one-file project. Output emitted before an uncaught error
is preserved. Browser run and debug instruction budgets are both 10 million.

WASM construction/collection use an internal diagnostic timer that does not
call unsupported `Instant::now`; browser GC durations remain zero without a
host clock capability. The native embedding allocator is not a WASM host-libc
import. `scripts/test-sol-wasm.mjs` instantiates the linked module and audits
its imports, runs the nine shared portable fixtures in
`crate/sol/tests/wasm-portable.tsv`, and checks live module debugging and GC.
The same fixture contract runs against the native adapter in the `wasm`
feature's Rust tests, including closures, byte strings, metamethods,
coroutines, collection, and denied native loading.

Analysis uses opt-in bytecode accounting, with per-function activation,
self/inclusive instruction counts and bounded call/return/source-line timeline events
carrying source, line, a live first-local snapshot, and instruction delta.
It does not allocate a complete instruction replay. Native-instruction
accounting remains separate parity work.

Verified with the focused driver/adapter tests, generated-WASM smoke tests,
both production web builds, and real Chromium workers. The browser differential
now includes six genuine old/new Lua-output matches, canonical live-debugger
inspection/mutation/profiling/timeline checks, a 1,000-instruction runaway-loop
burst (under 2 ms in the qualification run), and the pre-existing DOM debugger
regression for upvalue/local editing and memory/GC. The 90 typed fixture
classifications remain migration diagnostics, not evidence of semantic parity
with the Lua-only legacy engine. The native Sol crate suite and Lua manifest
checks pass. Qualification also exposed a mixed-module CLI output regression:
Lua chunks still print only explicit output, while typed `main()` results are
again rendered for specialized mixed projects, as required by their tests.

The canonical WASM is currently 1,939,524 bytes raw / 584,296 bytes gzip; the
opt-in build still ships both runtime assets. This is not the final bundle-size
or initialization qualification. Full `cargo test --workspace` is currently
blocked by unrelated private `Analysis` imports in the `ebnf` crate.
`cargo test --workspace --exclude ebnf` passes for the canonical runtime,
core, decompiler, and LSP crates.

U12 is still in progress. Live stops inside coroutine `resume`/`wrap` and
blocking host callbacks, complete thread inspection,
typed live suspension/full-frame inspection, mixed browser modules, and the
final production removal of the legacy runtime remain required before its
exit gate can be checked. The feature flag is still default-off.

## Work item 14 — live coroutine resume chains

The canonical Lua debugger now schedules `coroutine.resume` and `wrap`
without retaining a Rust resume stack. Parent frames are parked in their
canonical coroutine state while the child is active; a control outcome hands
the debugger a pending resume request, distinct from a Lua yield or a debugger
slice expiration. Yield, completion, protected/wrapped errors, and `<close>`
self-close restore the parent and its hook/depth accounting. The main frame
has the same depth charge as normal execution, and a focused low-depth
native/debug differential verifies the limit behavior.

The worker exposes the entire active resume chain (main thread zero,
normal parents, running child), with isolated per-thread frames, locals,
upvalues, evaluation, and mutation. Normal-parent evaluation temporarily
selects that thread's frames and pins the original active chain, so explicit
collection cannot lose a parked child. Pending incoming values have precise
roots until the result is installed in a frame; they are then released rather
than extending weak-object lifetimes. Zero-instruction continuation settling
places a resumed frame before its next opcode. Per-frame breakpoint position
tracking prevents a yielded call from spuriously hitting again upon resume.

Regression coverage includes nested resume chains, child edits surviving
yield/resume and forced collection, runaway `wrap` bursts, native callable
coroutine bodies, protected error values, self-close finalizers, dead/running
resume rejection, weak-object lifetime parity, and depth limits. The real WASM
smoke test and Chromium worker scenario stop inside a coroutine, inspect both
threads, edit the child, collect, and resume to the expected output. The
existing DOM debugger checks remain part of browser qualification.

The item-13 coroutine/thread gap is closed for the active-chain protocol;
blocking host callbacks still execute atomically, and idle coroutines are not
invented as inspectable active threads. Typed live suspension/full frames,
mixed browser projects, final production cutover, and bundle/initialization
qualification still keep U12's exit gate open.

## Work item 15 — resumable specialized frames and portable typed execution

Typed Tier-0 execution now has an explicit live-frame driver. Its arithmetic,
memory, and control-flow opcodes use the same dispatcher as the ordinary
interpreter; calls, proper tail calls, and array-map callbacks return control
outcomes instead of retaining recursive Rust interpreter/callback stacks.
Frames retain unboxed registers and registered root ranges across bounded
bursts. Editing a paused register changes subsequent execution, and runaway
instruction/depth limits return errors without a native stack overflow.
Root guards now remove their own ranges, so suspended sessions can be released
out of order without unrooting another session.

The normal typed WASM `execute`/`execute_project` path uses these live frames.
All 33 portable typed conformance fixtures match their native expectations in
the actual linked WASM, in addition to the shared portable Lua fixture
contract. The native conformance suite independently exercises the live
driver in bounded bursts against the same typed corpus. Explicit native libm
FFI remains the documented unsupported browser fixture; it is not silently
substituted or counted as passing.

Debug compilation retains source assignments and aggregate layouts instead
of applying constant propagation or scalar replacement. Source-local names
survive typed lowering, and bytecode has half-open initialized-local scope
ranges, including parameter, shadowed block-local, and numeric-loop scopes.
Focused tests cover real nested-frame mutation, map callback suspension,
tail-call reuse, depth/budget exhaustion, source-local mutation, shadowing,
initialization visibility, and loop-scope exit.

The typed collector's allocation header decoding/writes are now explicitly
64-bit on wasm32 as well as native targets. Array/map pointer fields occupy
whole 64-bit slots, with initialized padding on 32-bit targets and checked
field offsets. This fixes the latent wasm32 header-width panic and packed-map
pointer-layout mismatch; it does **not** by itself enable rooted typed
collection. The existing typed debug `force_gc` remains a no-op until its
portable collection/root-ownership adapter is implemented.

Verification includes the Sol native crate suite, the jit-free WASM-feature
library tests, native typed conformance, regenerated WASM smoke tests, the
opt-in production web build, and real Chromium worker/UI scenarios. The
linked canonical WASM measures 1,987,960 bytes raw / 602,426 bytes with Node's
default gzip; both old and canonical runtime assets still ship in the opt-in
build. These measurements are intermediate, not final bundle qualification.

U12 remains in progress. The typed worker debugger still uses the historical
trace adapter: it must be connected to the live driver with named full-frame
inspection, evaluation/mutation, portable rooted collection, and bounded
analysis. Mixed/generic browser projects, production removal of the legacy
package, and final bundle/initialization qualification also remain open.

## Work item 16 — live typed browser debugger and registered-root collection

The opt-in all-Sol debugger now uses `WasmTypedDebugSession`, driven by the
resumable specialized frames from item 15 rather than an instruction replay.
The Lua and typed live adapters share worker-protocol handling. Typed sessions
verify breakpoints against actual source filenames across every function,
retain full caller/child stacks and call-site locations, expose initialized
named locals, and evaluate/edit the selected real frame. Conditions, hit
counts, interpolated logpoints, stepping, output polling, and retained
array/map/record inspector references use this live state. Captures follow
the current typed lambda-lifted-parameter contract; no fictitious dynamic
globals or metatables are added to specialized code.

Frame expressions use the existing parser/type checker and specialized
representations, including aggregate parameters and resolved record layouts.
The parser receives the project's known record names. Assignment expressions
are checked against the destination type before changing a register.
Evaluation results retain their temporary bytecode owner and registered
root, including nested string-literal storage inside a newly assigned
record. Declaration injection, indirect specialized callbacks, and generic
arithmetic helpers without recoverable adapters are rejected explicitly.
Checked portable operations report divide/modulo-by-zero, invalid array
length, bounds, and boxed-type failures as errors; ordinary native tiered
execution retains its existing trap behavior.

Typed WASM forced collection now scans registered owners without requiring
a native stack. Live registers, inspector handles, and retained evaluation
results stay rooted. Historical exported trace sessions also register their
snapshot buffers, so a simultaneous live collection cannot invalidate a
retained compatibility trace. Native conservative-stack collection keeps its
existing API and guard. This closes item 15's forced-collection no-op for the
new browser adapter; it is not a claim that the legacy specialized heap has
already completed the roadmap's broader object-model migration.

Five focused adapter regressions cover live nested-frame edits, aggregate
evaluation/assignment and literal lifetime, recoverable expression failures,
condition/hit/log/step behavior, and bounded runaway execution without a
history buffer. The linked-WASM smoke test stops in an imported typed module,
edits both it and its caller, and verifies the changed result. It also forces
collection of a discarded large array while retaining paused array/map
graphs, an inspector reference after termination, and a concurrent historical
trace. Chromium exercises the same live worker protocol and real reclamation.
The native Sol suite, 177 jit-free WASM-feature library tests, linked-WASM
33-fixture typed qualification, Lua fixture checks, manifest validation, and
opt-in web production build pass.

The canonical WASM now measures 2,071,325 bytes raw / 631,726 bytes with Node's
default gzip. The production flag remains default-off and the opt-in bundle
still ships both engines. Bounded typed profiling/timeline still need to
replace their historical one-shot trace path. Mixed/generic browser projects,
the final legacy-package removal, and bundle/initialization qualification
remain required before U12 is complete.

## Work item 17 — bounded typed profiling and timeline

Typed worker analysis requests now use live specialized execution instead of
the historical replay adapter. Profiling retains per-function call,
self-bytecode, and inclusive-bytecode counters only. Inclusive totals use
activation intervals finalized at return, tail replacement, or error, rather
than walking every ancestor at each opcode. Proper tail calls count each new
activation without growing the stack. Native helper calls are counted, but
native machine instructions are not guessed.

Timeline storage is capped before recording, at the requested limit or
100,000 events, whichever is smaller. Events carry actual source filenames,
lines, initialized first-local snapshots, and bytecode deltas. Line changes
and loop backedges are recorded without retaining an instruction/register
history. Profiling disables event snapshots entirely. Both requests use a
fresh execution and preserve the user's paused session. Runtime failures are
reported rather than silently treated as completed recordings.

Function counters are 64-bit internally in both analysis adapters, and the
WASM wire uses exact JS numbers within browser budget/depth bounds. This
prevents recursive inclusive totals from wrapping at 32 bits. The unused
typed replay branches and helpers have been removed from the web worker;
the historical exported canonical API remains available for compatibility,
but the product worker no longer routes requests through it.

Five native analysis regressions cover call/self/inclusive accounting,
source/local snapshots, repeated proper tail calls, bounded runaway failure,
and the 32-bit overflow boundary. The actual WASM smoke test and Chromium
worker scenario verify profiling and a two-event truncated timeline while
retaining a paused typed session. The Sol native suite, 182 jit-free
WASM-feature library tests, actual linked-WASM fixtures, and both web build
configurations pass. The canonical module measures 2,110,084 bytes raw /
644,175 bytes with Node's default gzip; the opt-in worker is 63.14 KB.

This closes item 16's typed-analysis replay gap. Generic/mixed browser
projects, final legacy-package removal, and bundle/initialization
qualification still keep U12 in progress and its production flag default-off.

## Work item 18 — shared semantic selection and generic projects

The CLI's AST-based execution selector is now a shared library facility.
The browser uses it before choosing a generic or specialized adapter;
filename defaults select accepted syntax, not permanent runtime semantics.
Annotation-free `.sol` executes with canonical Lua values, division, captured
output, and chunk returns that are not implicitly echoed. Explicit typed
contracts, casts, records, imports/exports, and other typed-only surfaces are
not silently boxed into this path.

Generic in-memory projects can contain both `.lua` and `.sol` frontend
profiles. The deterministic `require` loader retains each module's parser
configuration and uses the same live trampoline, module cache, precise roots,
and standard library. Original filenames remain available to breakpoints,
frames, profiling, and timeline events. Registration validates every AST,
rejects escaping/duplicate paths and ambiguous module names, and never
enables host filesystem or native loading. This slice covers `require`, not
an extension-enabled replacement for Lua's dynamic `load`/`dofile` parsing.

Live debugging, frame edits, forced collection, profiling, and bounded
timeline requests use the generic project adapter. A browser regression
also caught and fixed executable-line verification for separately compiled
top-level generic functions: their breakpoints could previously stop without
being marked verified. Focused native and actual linked-WASM checks now
assert both verification and execution.

Qualification covers a Lua entry requiring an extension-enabled Sol module
which itself requires another Lua module, cached module identity, a real
paused module-local edit changing output from 42 to 44, forced collection,
and separate analysis preserving the paused session. The jit-free WASM
library suite has 188 passing tests; the complete native Sol suite, actual
linked-WASM fixtures, Chromium worker/DOM debugging, both web build
configurations, and Lua corpus-manifest validation pass. The canonical
module measures 2,131,010 bytes raw / 651,981 bytes with Node's default gzip;
the opt-in worker is 65.55 KB.

Generic projects containing both extensions are supported, but projects
crossing mandatory typed/generic call boundaries still need a shared bridge
adapter. Production still imports the old package and initializes both
engines under the default-off flag. That bridge, final legacy removal, and
bundle/initialization qualification remain U12 completion requirements.

## Work item 19 — parked semantic-call continuations

The specialized live driver can now park a semantic-slot call as an explicit
request instead of rejecting it or invoking its blocking callback. The
request owns rooted raw arguments, including after a proper tail call has
removed the caller. The scheduler can inspect the callee ID and arguments,
then complete exactly one continuation with a value or error. Caller frames
and map continuations stay live; waiting does not consume more bytecode
budget, and duplicate completion is rejected without changing execution.

The generic live driver also retains its terminal return values as rooted
results available to a semantic scheduler. They remain distinct from
captured stdout, preserve multiple returns and nil, and survive collection
after frames have been removed. They are not implicitly displayed as
playground output.

Four specialized regressions cover live caller mutation, proper tail calls,
error cleanup, and mapped callbacks while ensuring blocking callbacks are
never invoked. A generic regression covers terminal table/string/nil results,
forced collection, and failed-result isolation. All 193 jit-free WASM-feature
library tests pass, as do the native live-driver tests. This is continuation
infrastructure, not yet the mixed browser bridge: checked cross-tier value
conversion, shared execution budgets/depth, module initialization/state,
reentrant scheduling, and combined debugger/analysis frames still need an
adapter before production routing can change.

The complete native Sol suite and regenerated canonical WASM pass their
regression checks, including linked-module smoke qualification, Chromium
worker/DOM debugging, and the opt-in production web build. It measures 2,144,737 bytes raw /
653,577 bytes with Node's default gzip; these remain observations, not proof
that the final bundle/initialization exit targets have been met.

## Work item 20 — checked mixed bridge and production cutover

The portable scheduler now joins specialized `Execution` frames and generic
Lua frames using parked semantic requests in both directions. Scalar arguments
and results are checked at each boundary; typed registers remain unboxed.
Reentrant calls share the canonical import graph, module cache, initializers,
captured output, remaining opcode budget, and depth limit. Protected generic
calls catch boundary errors without a recursive Rust callback stack. Debugger
inspection and edits operate on both live representations; forced collection
preserves paused values. Profiling and capped timelines use fresh executions
and leave the user's paused session intact. Mixed profiling accounts actual
bytecodes with O(stack-depth) live-stack work per opcode, not retained execution
history or guessed native instruction counts.

Namespace lowering now preserves imported function-value identity and respects
parameter/local/loop shadowing. Declared exported `main` functions are not
mistaken for synthesized module initializers. Source filenames are retained
for nested project entries and module breakpoints. Typed root floats retain
the specialized display format, and typed root strings are copied into a
rooted canonical string for display (not a new string foreign-call ABI).

The production worker imports only `@lua-playground/sol-runtime`, initializes
one WASM module once, and has no default-off runtime switch. `apps/web` no
longer depends on `@lua-playground/runtime`; root build/dev wrappers regenerate
the canonical package. The historical `packages/lua-runtime` workspace and
retired adapter sources remain migration history, not production imports or
bundle assets. Generated bindings are regenerated, never hand-edited.

`tests/wasm-mixed.tsv` drives eight identical portable native-core/WASM fixtures:
scalar imports, reentry, protected errors, shared module cache/callable identity,
function values and lexical shadowing, initializer-once behavior, float
formatting, and a string root result. These supplement the existing 33 typed
and generic Lua linked-WASM fixtures; they do not claim the full Lua 5.5 corpus
or native-JIT equivalence. Chromium additionally checks a real bundled mixed
worker, module breakpoints, combined live frames, both caller/callee edits,
forced GC, and analysis preserving a paused session. The old/new differential
uses the frozen legacy bundle only as a migration oracle; its typed acceptance
classification is not typed semantic parity.

### Repeatable artifact and latency gates

Run `scripts/build-sol-wasm.sh`, `npm run build --workspace apps/web`, then
`npm run qualify:runtime --workspace apps/web`. The qualification script
requires installed Playwright Chromium and permission to bind a localhost
HTTP server. `SOL_QUAL_DIST` selects an alternate built directory, and
`SOL_QUAL_REPORT` optionally writes a JSON evidence artifact.

The lab regression ceilings are 3 MiB raw WASM, 900 KiB Node-default-gzip WASM,
100 KiB raw worker JavaScript, 1,000 ms p95 initialization, 25 ms p95 warm run,
and 50 ms p95 per 1,000-operation mixed burst. Initialization samples use ten
fresh Chromium processes/workers; browser startup is excluded. Warm runs and
bursts each use 25 samples. The script rejects legacy/multiple WASM assets and
audits WASM imports for ambient `env` capabilities. These are localhost
regression gates, not WAN download or cold filesystem-cache guarantees.

Final qualification on 2026-10-07 passed on an Apple M1 Pro, macOS arm64,
Node v24.14.0, Chromium 151.0.7922.34. WASM is 2,298,290 bytes raw / 704,846
bytes with Node's default gzip; the worker is 34,757 bytes. Artifact SHA-256:
`0f53d003b3eefbf0f159b025935db12e8564d400e666dcc59a01289afefeeaab`.
P95 initialization is 34.9 ms, warm run 1.2 ms, and mixed burst 0.4 ms; all
six gates pass. Full samples are emitted by the script; the local evidence
artifact is in ignored `crate/sol/scratch/u12-qualification.json`.

Validation passed: the complete native Sol crate suite, 202 jit-free
WASM-feature library tests, regenerated/linked WASM (33 typed and eight mixed
fixtures plus generic Lua/debugger checks), production web build, Chromium
qualification, Lua corpus-manifest validation, and project-status checks.
The final frozen-old/default-canonical Chromium differential also passes:
six real Lua output comparisons, live generic/typed debugger checks, DOM
debugger-feature E2E, and 90 typed acceptance classifications (not typed
semantic parity).
Web lint passes with two pre-existing UI warnings. Strict Clippy is still
blocked by pre-existing warnings (including `sol-core`'s
`doc_lazy_continuation`); no unrelated lint suppression was introduced.

### Remaining limits

The cross-tier call ABI is `i64`/`f64`/`bool`, matching the existing native
contract. Typed modules must be imported before dynamic `require`; unused
typed sources are explicit loader errors, not silently boxed. The flattened
import interface does not discover exports nested inside lexical chunk
initializers. Aggregate/string/function foreign arguments, typed yields, and
cross-tier reentry from atomic native callbacks are unsupported and rejected.
These are explicit current language/profile limits, not filename-selected
alternative semantics.

The mixed debugger currently exposes one combined scheduler thread. Generic
coroutine-parent frame isolation and cross-tier suspended coroutine scenarios
are not yet qualified (the pure generic debugger retains its coroutine-chain
thread support). This keeps the broad debugger parity deliverable and U12
status open despite completing the requested bridge, production-package
cutover, and portable-profile artifact/latency qualification.
