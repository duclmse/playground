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
