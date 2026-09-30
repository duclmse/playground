# U9 — Baseline dynamic JIT

**Status:** planned

**Purpose:** remove hot untyped dispatch overhead.

- [ ] Lower generic and cached bytecode quickly to Cranelift.
- [ ] Provide semantic slow-path stubs.
- [ ] Implement safepoints, stack maps, errors, and coroutine fallback.
- [ ] Define bounded hot-function compilation policy.
- [ ] Add direct entries for stable call targets.
- [ ] Manage code-cache lifecycle, invalidation, and executable-memory safety.

**Exit gate:** baseline-JIT readiness passes within published compile-latency
and code-memory budgets, with interpreter/JIT differential and GC tests.

See the [historical U9 ledger](../unified-sol-runtime-plan.md#u9--baseline-dynamic-jit).
