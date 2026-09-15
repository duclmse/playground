//! `sol-lsp`: an LSP server for both `.sol` (typed) and `.lua`
//! (Lua-compatible) source, sharing `crates/sol`'s lexer/parser/typeck front
//! end. See `docs/sol-lsp.md` for scope, architecture, and known
//! limitations (this is a first version - textual, not fully scope-aware,
//! symbol resolution; see `text.rs`/`index.rs` module docs).

mod backend;
mod diagnostics;
mod document;
mod features;
mod index;
mod text;

use tower_lsp::{LspService, Server};

use backend::Backend;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
