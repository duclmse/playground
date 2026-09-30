# U14 — Integrated release qualification

**Status:** planned

**Purpose:** prove the final product claim end to end.

- [ ] Run compatibility, fuzz, tier differential, GC stress, sanitizer, native,
      WASM, LSP, VS Code, and web E2E matrices from a clean checkout.
- [ ] Publish raw benchmark data and architecture summaries.
- [ ] Audit capabilities, native loading, executable memory, FFI, and browser isolation.
- [ ] Remove obsolete fallbacks only after rollback tags and differential evidence.
- [ ] Align feature, specification, product, and known-limitation documents.
- [ ] Package CLI, runtime libraries, web assets, LSP, and extension together.

**Exit gate:** every final definition-of-done item passes; otherwise release
wording names the highest completed gate instead.

See the [historical U14 ledger](../unified-sol-runtime-plan.md#u14--integrated-release-qualification).
