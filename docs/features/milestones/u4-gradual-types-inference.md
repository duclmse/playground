# U4 — Gradual type system and sound static inference

**Status:** complete (2026-09-16), with a scoped representation follow-up.

**Purpose:** remove dynamic checks only when proof preserves Lua behavior.

- [x] Bounded lattice, unions, widening, narrowing, nil elimination, return
      inference, and local call signatures.
- [x] CFG/SSA escape, alias, mutation, and effect analysis.
- [x] Infer local table shapes and nonescaping closure signatures.
- [x] Optional annotation contracts and `off`/`infer`/`strict` policy.
- [x] Explain proven and remaining dynamic operations.
- [~] Replace the legacy typed-only heap-allocating `any` representation with
      canonical tagged values; unified scalar boundaries already avoid boxing.

**Exit gate:** inference proofs remove only redundant checks and all
annotation-free compatibility fixtures remain unchanged.

See [gradual typing](../gradual-typing.md) and the [historical U4 ledger](../unified-sol-runtime-plan.md#u4--gradual-type-system-and-sound-static-inference--completed-2026-09-16).
