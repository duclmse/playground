# U13 — Semantic LSP and first-party VS Code client

**Status:** planned

**Purpose:** ship supported editor tooling from the shared semantic frontend.

- [ ] Support and test `sol-lsp` as a first-party component.
- [ ] Replace textual indexing with U1 binder and U4 type facts.
- [ ] Implement incremental documents, modules, diagnostics, navigation,
      completion, signatures, hover, symbols, tokens, formatting, cancellation.
- [ ] Share analysis with Monaco through a worker-safe transport.
- [ ] Ship `editors/vscode-sol` with activation, configuration, lifecycle, and
      `.lua`/`.sol` workspace support.
- [ ] Add protocol, golden, VS Code integration, packaging, and clean-install tests.

**Exit gate:** a packaged clean-profile extension passes mixed-module editor
workflows and agrees with CLI analysis.

See the [historical U13 ledger](../unified-sol-runtime-plan.md#u13--semantic-lsp-and-first-party-vs-code-client).
