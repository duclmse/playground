# U10 — Optimizing SSA JIT, OSR, and deoptimization

**Status:** planned

**Purpose:** reach LuaJIT-class untyped-Lua performance using proof and profiles.

- [ ] Lift bytecode to shared SSA with proof provenance.
- [ ] Insert, hoist, and fuse guards with full deoptimization snapshots.
- [ ] Specialize arithmetic, tables, calls, loops, allocation, and iteration.
- [ ] Inline stable dynamic and typed calls.
- [ ] Enter optimized loops with OSR and exit through precise side exits.
- [ ] Reconstruct inlined frames for errors, coroutines, profiling, and debug.
- [ ] Bound recompilation with failure counters and widening.

**Exit gate:** dynamic parity passes before final performance claims.

See the [historical U10 ledger](../unified-sol-runtime-plan.md#u10--optimizing-ssa-jit-osr-and-deoptimization).
