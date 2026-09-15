# sol-lsp

> Status: first version, implemented. Diagnostics, hover, go-to-definition,
> references, document symbols, workspace symbols, document highlight,
> rename, completion, and signature help all work end to end over stdio.

`crates/sol-lsp` is a Language Server Protocol server for both of `crates/sol`'s
source languages - typed `.sol` and Lua-compatible `.lua` - built on top of
`crates/sol` as a library (path dependency), reusing its shared lexer/parser/
typeck front end (`parser::SourceMode::Sol` vs. `SourceMode::Lua`) rather than
reimplementing any part of it. It was written after reviewing `LuaHelper`
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
  (`DashMap<Url, Document>`), holding the raw text, its `SourceMode` (by file
  extension - `.lua` is `SourceMode::Lua`, everything else including `.sol`/
  `.fl`/untitled is `SourceMode::Sol`, matching `main.rs::source_mode`), and a
  `SymbolIndex`. Sync is whole-document (`TextDocumentSyncKind::FULL`): every
  `didChange` carries the full new text and triggers a full re-analysis, no
  incremental patching.
- **Diagnostics** (`diagnostics.rs`): runs the exact lex -> parse -> typeck
  pipeline `sol run` uses (`crates/sol/src/main.rs::run`), minus execution.
  For `.lua`, a typeck failure tagged `sol::typeck::requires_dynamic_runtime`
  is *not* a diagnostic - it's the normal case for real Lua source (globals,
  `pairs`, dynamic function values, ...), exactly like `sol run`'s own
  fallback to the dynamic interpreter. Only genuine lex/parse errors and
  non-dynamic typeck errors get published.
- **Symbol index** (`index.rs`): built by walking the *untyped* `ast::Program`
  (not the typed `TProgram`), so it exists even when typeck fails with
  "requires dynamic runtime" - which is the common case for `.lua` files.
  This is deliberately not a real binder: locals are recorded as a flat,
  line-ordered list per top-level function, and a reference resolves to the
  nearest preceding same-named declaration in the same top-level function.
  That misses real nested-scope shadowing, but needed no changes to
  `crates/sol` itself to build.
- **Position resolution** (`text.rs`): `crates/sol`'s AST carries 1-based
  *line* numbers almost everywhere (not columns), except
  `ast::Function::source_span`, which has full byte ranges. Rather than
  extend the AST, every feature resolves the identifier under the cursor and
  its occurrences directly from the document text (`word_at_position`,
  `find_word_occurrences`) - a textual, word-boundary scan, not a
  scope-aware one.

## Known limitations (v1)

- **References/rename/highlight are textual, not semantic**: they match
  every word-boundary occurrence of the identifier's *text* in the document,
  so a same-named local in an unrelated function or an unrelated global will
  be included. Real scope resolution would need a proper binder shared with
  `typeck.rs`.
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

No automated test suite yet (v1 was verified with a manual JSON-RPC-over-stdio
smoke test exercising every request type against real `.sol`/`.lua` source);
see `docs/sol-lsp.md`'s limitations above for what a first regression suite
should target.

Point an editor's generic LSP client at the built binary
(`crates/sol-lsp/target/debug/sol-lsp` after the build above) for `.sol` and
`.lua` files.
