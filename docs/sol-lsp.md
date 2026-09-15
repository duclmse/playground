# sol-lsp

> Status: first version, implemented. Diagnostics, hover, go-to-definition,
> references, document symbols, workspace symbols, document highlight,
> rename, completion, and signature help all work end to end over stdio.

`crates/sol-lsp` is a Language Server Protocol server for Lua-compatible `.lua`
and superset `.sol` source. It uses `crates/sol`'s lossless parser,
`LanguageConfig`, lexical binder, and type checker rather than reimplementing
frontend semantics. It was written after reviewing `LuaHelper`
(a Go-based Lua LSP available in a local external checkout during development,
not a repository dependency) for the shape of its LSP surface - `initialize`,
`textDocument/{didOpen,didChange,didSave,didClose,definition,hover,
references,documentSymbol,rename,documentHighlight,signatureHelp,
completion}`, `completionItem/resolve`, `workspace/symbol` - though not its
implementation, which is a large, config-driven Go analysis pipeline with no
direct Rust equivalent to port.

## Architecture

- **Transport**: `tower-lsp` over stdio (`main.rs`), chosen over hand-rolling
  the protocol (as `crates/dap-server` does for DAP) because this crate's
  scope - the full LSP request surface above, not DAP's dozen-ish commands -
  makes `tower-lsp`'s request routing and `lsp-types` structs worth the extra
  `tokio` dependency this crate alone carries.
- **Document store** (`document.rs`): one `Document` per open URI
  (`DashMap<Url, Document>`), holding the raw text, independent language
  configuration, lexical bindings, and a presentation-oriented `SymbolIndex`.
  Sync is whole-document (`TextDocumentSyncKind::FULL`): every
  `didChange` carries the full new text and triggers a full re-analysis, no
  incremental patching.
- **Diagnostics** (`diagnostics.rs`): runs the exact lex -> parse -> typeck
  pipeline `sol run` uses (`crates/sol/src/main.rs::run`), minus execution.
  For `.lua`, a typeck failure tagged `sol::typeck::requires_dynamic_runtime`
  is *not* a diagnostic - it's the normal case for real Lua source (globals,
  `pairs`, dynamic function values, ...), exactly like `sol run`'s own
  fallback to the dynamic interpreter. Only genuine lex/parse errors and
  non-dynamic typeck errors get published.
- **Lexical binder** (`crates/sol/src/binder.rs`): models nested block scopes,
  shadowing, locals, parameters, loop variables, upvalues, labels/gotos, and
  global access through `_ENV`. Go-to-definition consults these bindings before
  falling back to the presentation index.
- **Symbol index** (`index.rs`): built by walking the *untyped* `ast::Program`
  (not the typed `TProgram`), so it exists even when typeck fails with
  "requires dynamic runtime" - which is the common case for `.lua` files.
  It remains a lightweight store for hover, completion, and document symbols;
  it is not the authority for lexical name resolution.
- **Position resolution** (`text.rs`): `crates/sol`'s AST carries 1-based
  *line* numbers almost everywhere (not columns), except
  `ast::Function::source_span`, which has full byte ranges. Rather than
  extend the AST, every feature resolves the identifier under the cursor and
  its occurrences directly from the document text (`word_at_position`,
  `find_word_occurrences`) - a textual, word-boundary scan, not a
  scope-aware one.

## Known limitations (v1)

- **References/rename/highlight are still textual**: they match
  every word-boundary occurrence of the identifier's *text* in the document,
  so a same-named local in an unrelated function or an unrelated global will
  be included. U13 will route these operations through the shared binder.
- **Single-file only**: no cross-file `.sol` project resolution
  (`modules.rs::compile_project`) and no cross-file rename/references/
  workspace symbol beyond documents currently open in the editor.
- **Signature help is single-line**: the active-call/active-parameter scan
  (`features.rs::call_context`) only looks backward within the cursor's own
  line, so a call whose arguments span multiple lines won't resolve.
- **No incremental sync**: every keystroke re-lexes/re-parses/re-typechecks
  the whole document. Fine at the file sizes this targets; would need
  `TextDocumentSyncKind::INCREMENTAL` plus incremental reanalysis to scale
  further.
- Diagnostics only ever report the *first* error in a document (`sol`'s
  front end returns `Result<_, String>`, not a diagnostic list), matching
  `sol run`'s own single-error-at-a-time behavior.

## Commands

```sh
cargo build --manifest-path crates/sol-lsp/Cargo.toml
cargo clippy --manifest-path crates/sol-lsp/Cargo.toml --all-targets
```

The crate includes focused frontend/binding tests; U13 adds protocol-level VS
Code integration and semantic rename/reference coverage.

Point an editor's generic LSP client at the built binary
(`crates/sol-lsp/target/debug/sol-lsp` after the build above) for `.sol` and
`.lua` files.
