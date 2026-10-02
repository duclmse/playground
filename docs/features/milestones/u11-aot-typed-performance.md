# U11 — AOT and annotation-driven peak performance

**Status:** planned

**Purpose:** make optional types a predictable accelerator on the same engine.

- [ ] Feed checked annotations and inference into shared SSA.
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
- [ ] Support profile-guided AOT with safe profile-change fallback.
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

**Exit gate:** typed advantage is measured, explained, and compatible with
mixed dynamic behavior.

See the [historical U11 ledger](../unified-sol-runtime-plan.md#u11--aot-and-annotation-driven-peak-performance).
