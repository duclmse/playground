# U11 — AOT and annotation-driven peak performance

**Status:** planned

**Purpose:** make optional types a predictable accelerator on the same engine.

- [ ] Feed checked annotations and inference into shared SSA.
- [ ] Remove proven-unnecessary guards and generic calls.
- [ ] Specialize generics, records, arrays, maps, and callbacks across modules.
- [ ] Retain dynamic adapters at exported and reflective boundaries.
- [ ] Support profile-guided AOT with safe profile-change fallback.
- [ ] Compare gradually typed programs to their unchanged Lua versions.

**Exit gate:** typed advantage is measured, explained, and compatible with
mixed dynamic behavior.

See the [historical U11 ledger](../unified-sol-runtime-plan.md#u11--aot-and-annotation-driven-peak-performance).
