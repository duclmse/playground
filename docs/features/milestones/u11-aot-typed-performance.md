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
- [ ] Retain dynamic adapters at exported and reflective boundaries.
- [ ] Support profile-guided AOT with safe profile-change fallback.
- [ ] Compare gradually typed programs to their unchanged Lua versions.

**Exit gate:** typed advantage is measured, explained, and compatible with
mixed dynamic behavior.

See the [historical U11 ledger](../unified-sol-runtime-plan.md#u11--aot-and-annotation-driven-peak-performance).
