# U2 — Canonical runtime object model and GC foundation

**Status:** complete (2026-09-26)

**Purpose:** establish one object identity and reachability domain.

- [x] Canonical values, handles, tables, strings, closures, upvalues, threads,
      userdata, errors, and capabilities.
- [x] `_ENV` table/upvalue semantics replace dedicated globals.
- [x] Precise roots and stack-map interfaces.
- [x] Dynamic libraries and metatables use canonical managed objects where
      they have Lua value identity.
- [x] Tracing, barriers, weak/ephemeron rules, finalizers, and coroutine roots.
- [x] Remove transitional production ownership after the coordinated cutover.

**Exit gate:** forced-collection identity, weak-reference, finalizer,
coroutine, and mixed-adapter tests pass without dual ownership.

See [table/closure/coroutine cutover](../table-closure-coroutine-cutover.md)
and the [historical U2 ledger](../unified-sol-runtime-plan.md#u2--canonical-runtime-object-model-and-gc-foundation--completed-2026-09-26).
