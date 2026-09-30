# U8 — Inline caches and bounded profiling

**Status:** planned

**Purpose:** exploit dynamic facts without native-code dependency.

- [ ] Cache field/index, global, arithmetic/metamethod, iterator, and call targets.
- [ ] Version shapes, metatables, globals, and modules for invalidation.
- [ ] Bound mono/poly/megamorphic transitions.
- [ ] Serialize type/shape/call/allocation profiles for inspection and PGO.
- [ ] Test alias, metatable, `_ENV`, module, and debug-visible invalidation.

**Exit gate:** cache-heavy workloads improve while forced invalidation and
megamorphic workloads stay bounded and compatible.

See the [historical U8 ledger](../unified-sol-runtime-plan.md#u8--inline-caches-and-bounded-profiling).
