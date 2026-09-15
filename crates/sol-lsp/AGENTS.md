# crates/sol-lsp

Standalone Cargo crate. An LSP server for both typed `.sol` and
Lua-compatible `.lua` source, over stdio via `tower-lsp`, built on top of
`crates/sol` as a path dependency (its shared lexer/parser/typeck front end,
gated by `parser::SourceMode`). Full design/scope/limitations: `docs/sol-lsp.md`.

## Layout

- `main.rs` - stdio transport entrypoint (`tower_lsp::Server`).
- `backend.rs` - the `LanguageServer` trait impl; thin wiring onto
  `features.rs`, no feature logic of its own.
- `document.rs` - per-URI `Document` (text, `SourceMode`, `SymbolIndex`) and
  the `DocumentStore` (`DashMap<Url, Document>`).
- `diagnostics.rs` - runs `sol`'s lex/parse/typeck pipeline and turns its
  `Result<_, String>` errors into LSP diagnostics.
- `index.rs` - builds a per-document `SymbolIndex` by walking the untyped
  `ast::Program` (works even when typeck fails with
  `requires_dynamic_runtime`, the normal case for `.lua`).
- `text.rs` - position/range helpers; textual (word-boundary) identifier
  resolution, since `crates/sol`'s AST mostly only carries line numbers, not
  columns.
- `features.rs` - the actual hover/definition/references/document-symbol/
  rename/completion/signature-help implementations, each a plain function
  over a `Document`.

## Change guidelines

- This crate reads `crates/sol`'s public API only (`sol::{lexer, parser,
  ast, typeck, compile_program_with_mode}`); it must not need to modify
  `crates/sol` itself to add a feature. If a feature genuinely needs
  something `crates/sol` doesn't expose (e.g. typed hover using `TProgram`
  instead of the untyped `ast::Program`), that's a deliberate, separate
  change to `crates/sol`'s public surface - call it out, don't reach into
  private internals.
- Keep `.lua` and `.sol` handling in the same code paths wherever the
  underlying `crates/sol` front end already unifies them (lexer/parser via
  `SourceMode`); don't fork logic per-language unless the languages'
  semantics actually diverge (as diagnostics' `requires_dynamic_runtime`
  handling must, since dynamic Lua constructs are expected, not errors).
- `index.rs`/`text.rs`'s symbol resolution is textual/heuristic by design
  (see `docs/sol-lsp.md`); don't silently assume it's scope-exact when
  building new features on top of it.

## Commands

```sh
cargo build --manifest-path crates/sol-lsp/Cargo.toml
cargo clippy --manifest-path crates/sol-lsp/Cargo.toml --all-targets
```
