# U11 — AOT and annotation-driven peak performance

**Status:** in progress

**Purpose:** make optional types a predictable accelerator on the same engine.

- [ ] Feed checked annotations and inference into shared SSA. Explicitly
      skipped, not forgotten: the plan's own item 1 scoped this as an
      optional, decoupled stretch goal ("not a prerequisite for items 2-5...
      if schedule pressure hits, cut this item entirely and the rest of the
      plan still delivers every U11 checklist line") - items 2-7 all landed
      without it, confirming the plan's own prediction, so it was cut rather
      than attempted as a partial slice.
- [ ] Remove proven-unnecessary guards and generic calls. Item 2
      (2026-10-02): the typed tier's only runtime guard - the per-call tag
      re-check in `interp::try_speculative` before entering a
      speculatively-specialized `any`-parameter body - is now statically
      elided (`Speculative::proven`, set from
      `jit::is_speculative_exhaustive`) whenever a whole-program proof shows
      every call site always boxes the same concrete type directly at the
      call, and the function is never captured as a first-class value. An
      intermediate `any`-typed local between the box and the call defeats
      the proof (conservatively, not unsoundly - no reaching-definitions
      dataflow attempted yet), so the guard stays in place there. Remaining
      scope for this bullet: generic runtime calls elsewhere in the typed
      tier are untouched by this item.
- [ ] Specialize generics, records, arrays, maps, and callbacks across modules.
      Item 3 (2026-10-02): the only generic-shaped specialization mechanism
      in the typed tier was `typeck.rs`'s `check_call` "map" branch
      (`types.rs:253-257`'s `TExprKind::ArrayMap`), hard-coded to exactly one
      monomorphization, `Array<i64>` + `fn(i64) -> i64`. Generalized it to
      monomorphize per call site over either scalar type the runtime already
      has a dedicated unboxed array representation for (`I64` or `F64`,
      matching `new_array_i64`/`new_array_f64`'s existing split) instead of
      being fixed to `i64`: `TExprKind::ArrayMap` now carries its resolved
      `elem: Type` (`typeck.rs`'s `check_call`), threaded through
      `escape.rs`/`optimize.rs`/`jit.rs`'s tree-walkers and `verify.rs`'s IR
      check, with `codegen.rs` choosing between a new `sol_array_map_f64`
      runtime helper (`runtime.rs`, registered in `jit.rs`'s JIT symbol
      table alongside the existing `sol_array_map_i64`) and the original one
      based on that field - confirmed via `--dump-ir` that an `f64`
      instantiation calls `sol_array_map_f64`, never `sol_array_map_i64` or
      any `sol_dynamic_*` boxing path (`tests/programs.rs`'s
      `generic_map_over_f64_arrays_is_specialized_in_all_tiers`).
      **Remaining scope, explicitly not attempted here:** this generalizes
      *which concrete type* the one existing built-in specializes on, not
      *the mechanism* into real user-definable generic function syntax
      (type parameters on `function` declarations, call-site type inference,
      an instantiation cache) that the plan's fuller framing ("applies to
      user-defined generic functions and callback parameters generally")
      describes - there is no generic-function syntax anywhere in
      `ast.rs`/`parser.rs` today, and building one is a language-design-sized
      feature, not a bounded follow-on to the existing single-intrinsic
      mechanism. Measured, not adopted: left for a future milestone rather
      than attempted as a partial/unsound slice.
- [ ] Retain dynamic adapters at exported and reflective boundaries. Item 4
      (2026-10-02, partial): this is a verification item against items 2/3's
      already-landed changes, not new mechanism. Added
      `tests/programs.rs`'s
      `debug_repl_keeps_working_after_a_speculative_candidate_is_proven_exhaustive_and_promoted`,
      which runs `sol debug` against the item-2 fixture
      (`speculative_exhaustive_any_param.sol`) with
      `SOL_SPECULATIVE_THRESHOLD=1` so the whole-program proof fires and
      `triple` is promoted to native *during* the debug session, and confirms
      `debug.rs`'s call-boundary hook (`Hooks::on_call_enter`,
      `debug.rs:113-120`) still breakpoints, backtraces, and reaches the
      correct final answer (`5310`) both before and after the promotion -
      the cross-tier guarantee `debug.rs:1-14`'s header comment claims is now
      pinned by a real test, not just asserted in a comment. The FFI extern
      boundary (`TExternFunction`, `types.rs:144-148`) needed no new
      coverage: neither item touches extern dispatch (item 2 is scoped to
      the `any`-parameter speculative tag guard, item 3 to `ArrayMap`'s
      `elem` field), and the existing
      `ffi_extern_function_works_once_the_caller_is_promoted_to_native`
      already pins that boundary across tiers.
      **Not resolved, left open:** investigated whether `require()`-based
      dynamic field dispatch (`local m = require("x"); m.f(...)`) can defeat
      item 2's whole-program proof, since `jit::is_speculative_exhaustive`'s
      call-site walk (`jit.rs:860-1009`) only recognizes literal
      `TExprKind::Call` and disqualifying `FunctionRef` nodes, never
      `CallIndirect` - a dynamic dispatch call is invisible to it either way
      (neither counted as a call site nor flagged unsound). Calls crossing
      the dynamic/typed boundary do route through a separate semantic-adapter
      mechanism (`LuaPartition::mixed`, `typeck.rs:232-236`;
      `load_with_natives`'s "semantic adapter slots" comment,
      `lua_runtime/init.rs:820-822`) distinct from `interp::try_speculative`'s
      internal per-call tag guard, which is the structural reason this did
      not reproduce as an observable type-safety break in manual probing -
      but that was exploratory, not a committed fixture, and is not an
      exhaustive proof. Left unchecked and flagged here rather than claimed
      as verified.
- [x] Support profile-guided AOT with safe profile-change fallback. Item 5
      (2026-10-02): `aot.rs`'s own header (`aot.rs:1-10`) notes AOT has no
      interpreter/JIT tier to promote into, so U10's guard/deopt-snapshot
      mechanism (the usual meaning of "profile-change fallback") is
      structurally inapplicable here - there is no bytecode tier to fall back
      to. Scoped instead to the one AOT decision that's correct regardless of
      whether the profile is stale or wrong, per the plan's own guidance
      (`unified-sol-runtime-plan.md` §3's item-5 scope note): inlining is a
      pure code-shape transform that never changes a program's output
      (`codegen.rs`'s `compute_inlinable` only gates *whether* a call site's
      body is substituted in, never alters it), so a function a prior
      `sol run --profile-out` recorded as `promoted` (crossed
      `SOL_PROMOTE_THRESHOLD` calls, `tier.rs:16-24`) gets a wider inlining
      budget in `sol build --profile-in` - `INLINE_MAX_STMTS_HOT = 40` vs the
      default `INLINE_MAX_STMTS = 20` (`codegen.rs`'s constants above
      `compute_inlinable`). A wrong "was hot" guess costs only extra code
      size, never incorrect behavior - the "safe...fallback" property holds
      by construction rather than needing a runtime check. Reused
      `Profile::promoted` (`tier.rs:59-62`) as the hot-function signal rather
      than adding a new counter/format: it already means "demonstrably hot in
      a representative run." `aot::build` takes `profile: Option<&Profile>`
      (`aot.rs:39-44`) and derives the hot set at `aot.rs:102-105`; `sol build
      --profile-in <file>` (`main.rs`'s `build_cmd`) loads it via the same
      `Profile::load` text format `sol run --profile-in` already uses - no
      new CLI surface beyond the one flag. `profile: None` (`sol build`'s
      default, no `--profile-in`) computes an empty hot set, so inlining
      decisions are byte-identical to before this item - pinned by
      `tests/programs.rs`'s
      `profile_guided_aot_build_prints_identical_output_with_or_without_a_profile`,
      and the budget-widening logic itself by `codegen.rs`'s
      `inline_budget_tests::a_function_between_the_default_and_hot_budgets_is_only_inlinable_when_marked_hot`.
      **Measured, not just landed:** `benchmarks/function_calls.sol`'s `work`
      (22 statements - between the two budgets) called 2,000,000,000 times:
      disassembly (`otool -tV`) confirms the baseline AOT build keeps a real
      `call _work` in `__sol_main`, while the profile-guided build inlines it
      away entirely (0 calls, optimizer folds the 21 chained `+1`s into
      direct arithmetic on the caller's value) - user time dropped from
      ~2.49s to ~1.24s (roughly 2x) across 3 runs each, both builds printing
      the identical correct answer. **Explicitly out of scope** (per the
      plan's own item-5 note): compiling a speculative-and-fallback path
      alongside a runtime dispatch guard for a true speculative profile-guided
      optimization (e.g. a profile-informed type/shape guess) - AOT has no
      bytecode tier to bail into if such a guess were wrong, so that shape of
      profile guidance is left to a future milestone, not attempted as an
      unsound partial slice here.
- [x] Compare gradually typed programs to their unchanged Lua versions. Item 6
      (2026-10-02): `benchmarks/gradual-manifest.json` + new
      `scripts/gradual-benchmark.sh` measure `loop_sum`, `function_calls`, and
      `hashmap_lookup` at 2-4 annotation-coverage steps each (0% `.lua`, 0%
      `.sol` with no annotations, one or two partial steps, 100% typed),
      reusing `scripts/typed-regression-check.sh`'s 5%-threshold/noise-budget
      formula and failing loudly (nonzero exit) on any unexplained
      step-over-step regression. Canonical run: all three benchmarks are
      monotonic or explained end to end (zero unexplained `FAIL` rows, exit
      0) - `loop_sum` 1077ms (.lua) -> 974ms (.sol, 0%) -> 28.7ms (return-type
      only) -> 29.1ms (fully typed); `function_calls` 16431ms -> 18291ms (0%,
      **EXPLAINED** +11.3% - both steps are 0%-annotation, root-caused to
      `local function work(x)` (upvalue read, zero guard,
      `lua_bytecode/compile_calls.rs`'s `Resolved::Local`/`Upval`) vs this
      step's top-level `function work(x)` (`_ENV`-probed `GetGlobal`,
      `lua_runtime/dispatch/bytecode.rs:309-430`), not an annotation cost -
      isolated via a same-binary A/B/C (lua/local-fn 16.4s, sol/local-fn
      17.4s, sol/global-fn 18.2s)) -> 17.4ms (signature only) -> 17.3ms
      (fully typed); `hashmap_lookup` 11.8ms -> 11.5ms (0%) -> 3.8ms (map
      type only) -> 3.5ms (fully typed). The fail-loudly gate was verified
      for real against a manifest with the `explained` field removed, which
      reproduced the `function_calls` `FAIL` row and nonzero exit.
      See `benchmarks/RESULTS.md`'s dated gradual-benchmark section for the
      full table.

Item 7 (2026-10-02): built the exit gate's own measurement tooling. Nothing in
the benchmark harness previously measured `sol build` (AOT) output at all, and
no script computed §7.3's "Typed advantage" ratio. Extended
`scripts/benchmark.sh` with a `build_aot_binary` helper that AOT-builds each
benchmark's `.sol` twin once, outside the timed hyperfine window, and adds a
"sol (aot)" row alongside the existing "sol" (tiered JIT) and Lua-runtime
rows, degrading gracefully (a warning and an omitted row, not an aborted run)
on a build failure. New `scripts/typed-advantage-gate.sh` computes the gate
itself: for the 10 benchmarks with both a `.lua` and typed `.sol` twin (§7.1:
"a benchmark with no LuaJIT-equivalent behavior may inform Sol tuning but
cannot support the comparative headline"), it measures LuaJIT's and Sol's AOT
build's wall-clock mean via hyperfine - AOT, not `sol run`'s tiered JIT, since
AOT is this milestone's peak-performance path - and reports the geometric
mean of the per-benchmark ratios. §7.3's "a separately published target": as
of this writing none has ever actually been published anywhere in `docs/`
(grepped for it), so rather than invent one, the script reports the measured
ratio unconditionally and only evaluates pass/fail if a target is explicitly
supplied via `--target`/`TYPED_ADVANTAGE_TARGET` - matching U10 item 8's
precedent of an honest current number over a fabricated gate. Canonical
measured run (release build, 2026-10-02): geometric mean **1.089x** (Sol AOT
averages ~9% faster than LuaJIT across the suite), but the spread is wide and
not uniformly favorable - clear wins on `function_calls` (2.36x), `loop_sum`
(1.75x), `table_array` (1.33x), `matrix` (1.22x), and `fib` (1.12x); losses on
`objects` (0.59x), `hashmap_lookup` (0.79x), `nested_loop` (0.82x),
`string_concat` (0.90x), and `gc_alloc` (0.91x). §7.3's other clause,
"preserving mixed-call and compatibility behavior," is not re-measured by this
new script - confirmed instead by the full `cargo test --manifest-path
crate/sol/Cargo.toml` suite (131 lib tests, 70 `programs.rs` tests, 0 failed)
and a fresh `scripts/typed-regression-check.sh` run, which reproduces the same
three "REGRESSED" rows (`any_dynamic` +22.4%, `objects` +20.9%, `vector_add`
+44.8%) that item 2's note already investigated and confirmed as pre-existing
measurement noise predating every U11 item, not a new finding - a third
independent run, now after items 3/5/6 landed too, stays consistent with that
conclusion. **Not resolved, left open:** no target has been published for the
gate to actually pass or fail against, so the "Exit gate" below cannot be
marked reached on this item alone.

**Exit gate:** typed advantage is measured, explained, and compatible with
mixed dynamic behavior. Measurement tooling now exists (item 7) and reports
geomean 1.089x as of 2026-10-02, but **not yet reached** - no published target
exists for the gate to pass against, and deliverables 1-4 remain partial,
open, or stretch scope.

See the [historical U11 ledger](../unified-sol-runtime-plan.md#u11--aot-and-annotation-driven-peak-performance).
