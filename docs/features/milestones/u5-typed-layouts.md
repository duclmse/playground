# U5 — Typed layouts and mixed-module specialization

**Status:** complete (2026-09-16)

**Purpose:** retain typed performance within the canonical runtime.

- [x] Specialize proven scalars, arrays, records, maps, and closure environments.
- [x] Preserve identity when specialized objects become dynamically visible.
- [x] Scalar-replace nonescaping literals where Lua cannot observe allocation.
- [x] Generate semantic adapters and direct typed ABIs.
- [x] Share module cache and typed contracts across `require` boundaries.
- [x] Generate precise layouts and barriers for specialized objects.

**Exit gate:** typed hot paths remain unboxed, mixed modules retain identity,
and unused dynamic support does not alter proven bytecode.

See the [historical U5 ledger](../unified-sol-runtime-plan.md#u5--typed-layouts-and-mixed-module-specialization--completed-2026-09-16).
