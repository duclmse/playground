# U1 — Superset frontend and semantic AST

**Status:** complete (2026-09-15)

**Purpose:** accept Lua source through the Sol frontend without a semantic mode
fork.

- [x] Common Lua grammar with contextual Sol extensions.
- [x] Separate dialect/extensions, type policy, capabilities, and execution tier.
- [x] Preserve byte spans and spelling through AST and diagnostics.
- [x] Bind locals, upvalues, labels, `_ENV`, and nested scopes lexically.
- [x] Keep `.lua` and annotation-free `.sol` ASTs semantically equivalent.
- [x] Expose parser and binder APIs to `sol-lsp`.

**Exit gate:** parser and AST differentials pass in both profiles, including
located invalid-annotation diagnostics.

See the [historical U1 ledger](../unified-sol-runtime-plan.md#u1--superset-frontend-and-semantic-ast--complete).
