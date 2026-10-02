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
