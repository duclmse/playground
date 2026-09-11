# Optimization

> Status: implemented, with scoped follow-up work documented below.

**Goal**: real optimization work sol does itself, on top of whatever
`cranelift-codegen`'s mid-end already contributes for free. §6, §7, §11.

**Prerequisite**: bounds checks must exist before they can be eliminated; see
[Safety and correctness](safety.md).

- [x] **Establish a baseline**: found that the initial `JITBuilder::new` setup in
      `jit.rs`) left `opt_level` at Cranelift's own default, `"none"` - i.e.
      the initial benchmark numbers were measured with _zero_ Cranelift-level
      optimization. Switched to
      `JITBuilder::with_flags(&[("opt_level",     "speed")], ...)`, which turns
      on `cranelift-codegen`'s own mid-end pass pipeline (§6's list, whatever it
      implements) for free.
- [x] **sol-level constant folding** (`optimize.rs`, new module) on the
      typed AST before codegen - arithmetic/comparison/logical/unary ops over
      literal operands fold to a literal; division/modulo by a literal zero is
      deliberately left un-folded (the runtime path, not this pass, is the
      source of truth for that - see [Safety and correctness](safety.md)). Four unit tests.
- [x] **Bounds-check elimination** (§11): `codegen.rs`'s
      `recognize_safe_for_loop` matches exactly
      `for i = 0, #a - 1 do     ... end` (plain local `a`, start `0`, step `1`)
      and skips the bounds check for `a[i]` inside that loop, guarded by
      `assigns_to_local` confirming neither `a` nor `i` is reassigned in the
      body first. Narrower than general range analysis on purpose (per this
      checklist's own original wording) - confirmed by measurement (see
      `benchmarks/RESULTS.md`) that this specific pattern doesn't even appear in
      `table_array.sol` (`for i = 0, n - 1` uses a plain local `n`, not
      `#a - 1`), which is itself a useful, honest finding about how narrow
      "narrow" turned out to be.
- [x] **Inlining** (§7): `codegen.rs`'s `compute_inlinable` marks a function
      eligible if it isn't `main`, isn't directly self-recursive
      (`is_directly_recursive` - mutual recursion isn't detected, guarded
      instead by a `MAX_INLINE_DEPTH` safety net), and has ≤ `INLINE_MAX_STMTS`
      (20) statements. `FuncCtx::inline_call` splices the callee's body into the
      caller's current Cranelift blocks, routing `return` to a merge block
      instead of a real function return (`return_targets` stack) and giving the
      callee's locals their own fresh `Variable`s (`var_scopes` stack, so
      numerically colliding `LocalId`s between caller/callee resolve correctly).
      3 integration tests, including one calling the same small function from
      three call sites.
- [x] Re-ran `scripts/benchmark.sh` after landing all of the above and recorded
      the (honest, mostly-flat) deltas in `benchmarks/RESULTS.md` - see that
      file's historical optimizer comparison for why `fib`/`table_array`
      specifically do not show most of this feature's impact
      (memory-bandwidth-bound workload, no
      inlinable calls in either hot path) and what a benchmark that _would_ show
      it looks like.
- [x] Unit/integration tests per pass (see above) - 22 tests total in
      `crates/sol` after the optimization work, up from 13 initially.

**Files**: `crates/sol/src/optimize.rs` (new), `codegen.rs`, `jit.rs`.
