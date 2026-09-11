# Tiered execution

> Status: bytecode startup, hot counters, OSR, and a single native promotion
> tier are implemented.

**Purpose**: provide tiered compilation (§17) for code that benefits from
runtime profiles and specialization. Fully typed AOT code does not require
tiering.

**Prerequisite**: dynamic values provide type-uncertain behavior to profile and
specialize.

- [x] **Tier 0: bytecode interpreter** (§29, §30) - `bytecode.rs` (fixed-width
      32-bit iABC/iABx/iAsBx instructions, matching Lua's own encoding) +
      `bccompile.rs` (typed AST -> bytecode, registers are exactly the typed
      AST's own `LocalId`s - no separate register allocator, chosen so the
      interpreter's register file layout matches what Cranelift-compiled code
      expects for cheap OSR hand-off) + `interp.rs` (the tier-0 executor).
      Dispatch strategy decided by measurement, not assumption:
      `examples/dispatch_bench.rs` shows Rust `match` over a dense opcode enum
      beating a function-pointer table by ~3-3.5x (see
      `benchmarks/RESULTS.md`'s tiering section) - direct/computed-goto threading
      isn't expressible in safe Rust, so `match` is the implementation.
- [x] **Hot counters** (§31): per-function call counts (`interp::Runtime::counts`)
      and per-loop-backedge counts (`osr_counts`, keyed by `(func_id,
      stmt_index)`), each with its own env-overridable threshold
      (`SOL_PROMOTE_THRESHOLD`/`SOL_OSR_THRESHOLD`, `tier.rs`). Exact
      counting, not sampling - simple `Cell<u32>`/`HashMap` increments are cheap
      enough at interpreter-tier speeds that sampling's complexity wasn't
      justified; not benchmarked against sampling since exact counting never
      showed up as a bottleneck.
- [x] **Tier 1 + Tier 2 collapsed into one "promote to native" step** (§17) -
      deliberately not built as two separate compiles. `jit.rs`'s pipeline
      already reuses the full typed Cranelift pipeline (constant folding, GVN,
      LICM, bounds-check elimination, inlining, escape analysis), so a
      "baseline" compile that deliberately skips those passes would only be
      slower to run *and* slower to produce, with nothing to gain until a
      genuine type-profile-driven speculative tier (see below) gives tier 1
      something a fast-and-dumb compile could still capture that tier 2
      couldn't. `tier.rs` documents this reasoning inline.
- [x] **On-stack replacement** (§39): `jit.rs::promote_osr` compiles a synthetic
      native entry point taking all of a function's locals as parameters and
      jumping directly into a specific loop body (`codegen.rs::compile_osr_entry`,
      translating `tfunc.body[from_stmt..]`), so an already-running interpreted
      loop can switch to native mid-execution without waiting for the
      enclosing function to return. Empirically load-bearing, not optional:
      `benchmarks/table_array.sol` (whose `main` is called once, so
      function-call counting alone never promotes it) regressed 12x under
      tier-0-only execution (212.6ms vs. a forced-native 17.8ms) until OSR
      brought it back to 14.5ms - see `benchmarks/RESULTS.md`.
- [x] **Speculative optimization** (§40), **inline caches** (§19) and
      **deoptimization** (§18) - scoped down together (see "Speculative `any`-
      parameter specialization" below) into one narrower, coherent mechanism
      that matches what trap-based `any` actually makes possible, rather
      than the three being separately built pieces of a general deopt VM:
      `jit::speculative_candidate` statically finds a hot function's `any`
      parameter that's always immediately narrowed to one concrete type,
      `interp::Runtime::try_speculative` is the inline cache (the call-site tag
      guard, checked before every call), `jit::promote_speculative`/
      `jit::specialize` compile the guarded native variant, and "deopt" is
      simply falling through to the already-existing general path on a guard
      miss - no mid-execution state reconstruction needed, since the guard
      always runs before any side effects. Verified both for correctness
      (`tests/speculative_any_parameter_specializes_and_stays_correct` runs the
      same arithmetic through the interpreted-general and guarded-native paths
      and checks they agree) and for "this actually happened, not just would
      have been correct either way"
      (`..._is_visible_in_dumped_ir` asserts the specialized variant shows up
      in `SOL_DUMP_CLIF`'s output).

**Speculative `any`-parameter specialization - design note**: Sol's
`any` design uses hard runtime traps on a type mismatch (`value.rs`'s tag
check + `trap()`), not speculative typing - so classic deopt/inline-caches/
speculative-optimization (as LuaJIT or V8 build them, reconstructing precise
interpreter state from an arbitrary native PC) have nothing to attach to by
default. The design that got built instead scopes speculation to a
**guard-before-execute** boundary and a **structurally-provable** candidate,
not a probabilistic guess: `jit::speculative_candidate` only accepts a
function whose `any` parameter is used *exclusively* as the immediate operand
of `Unbox(_, T)` for one single `T` throughout the whole body (never returned
as `any`, never passed to another `any` slot, never reassigned) - the
"narrow immediately at the top of the function" shape `typeck.rs`'s own
`coerce` already produces for ordinary code like `local y: i64 = x`.
`jit::specialize` then rewrites the parameter's declared type to `T` and
deletes those now-redundant `Unbox` nodes outright (`interp::Runtime`'s guard
has already proven the tag matches before this compiled variant is ever
called - keeping the `Unbox` would just recheck something already checked).
The guard itself is load-bearing for *correctness*, not merely an
optimization: calling the specialized variant with a mismatched argument
would silently reinterpret a boxed pointer's bits as a raw scalar instead of
trapping, so `interp::Runtime::try_speculative` checks the actual argument's
tag on *every* call, unconditionally, before ever dispatching to a compiled
specialized pointer. What's deliberately out of scope: more than one
speculatable parameter per function (a plain "first `any` param found" cut,
not a fundamental limit), and any parameter whose uses are looser than
uniform immediate narrowing (correctly left un-specialized, not guessed at).

**Files**: `bytecode.rs`, `bccompile.rs`, `interp.rs` (tier 0 + hot counters +
OSR triggering + `try_speculative`'s inline-cache guard), `tier.rs` (the
`Engine` tying interpretation and promotion together, including the
speculative-candidate map built from `jit::speculative_candidate`), `jit.rs`
(`promote`/`promote_osr`/`promote_speculative`/`specialize`), `codegen.rs`
(`compile_osr_entry`), `gc.rs` (`RootGuard` - the interpreter's heap-allocated
register file needed its own GC-root registration, since the existing
conservative scanner only walked the native stack), `examples/dispatch_bench.rs`
(§30's dispatch-strategy measurement). No separate `jit_tiers/` module was
needed, matching the "Tier 1 + Tier 2 collapsed" reasoning above - there
is still only one native backend, not two, speculative specialization
included.
