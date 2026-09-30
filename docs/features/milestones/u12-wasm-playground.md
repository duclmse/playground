# U12 — Canonical WASM playground and debugger

**Status:** planned

**Purpose:** make the web product use the canonical runtime, not a separate VM.

- [ ] Create `sol-wasm` and `packages/sol-runtime` from the Tier-0 runtime.
- [ ] Reproduce budgets, modules, output, debug stepping, frames, locals,
      evaluation, profiling, and timeline.
- [ ] Support `.lua` and `.sol` with shared parser/type diagnostics.
- [ ] Keep worker execution and default-deny capabilities.
- [ ] Differentially switch the web adapter and remove Piccolo only after proof.
- [ ] Meet bundle-size, initialization, and responsiveness targets.

**Exit gate:** canonical-runtime web E2E scenarios and portable native/WASM
fixtures agree; production no longer imports the old runtime package.

See the [historical U12 ledger](../unified-sol-runtime-plan.md#u12--canonical-wasm-playground-and-debugger).
