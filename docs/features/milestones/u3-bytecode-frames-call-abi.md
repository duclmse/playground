# U3 — Unified bytecode, frames, and semantic call ABI

**Status:** complete (2026-09-16)

**Purpose:** run typed and untyped functions through one resumable semantic
engine.

- [x] Generic and specialized executable prototypes share a function model.
- [x] Standardize varargs, multi-results, tails, errors, protected calls,
      yield/resume, and source maps.
- [x] Use resumable heap/trampolined frames.
- [x] Support all dynamic/typed call directions with identity-preserving
      boundary adapters.
- [x] Route eligible `.lua` and `.sol` execution through the shared engine.
- [x] Retain a differential legacy path only as a non-production check.

**Exit gate:** mixed calls, errors, tails, yields, and collection agree across
the unified path.

See the [historical U3 ledger](../unified-sol-runtime-plan.md#u3--unified-bytecode-frames-and-semantic-call-abi--completed-2026-09-16).
