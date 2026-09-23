use dashmap::DashMap;
use sol::{
    binder::BindingIndex,
    parser::{LanguageConfig, SourceMode},
};
use tower_lsp::lsp_types::Url;

use crate::diagnostics;
use crate::index::{self, SymbolIndex};

pub struct Document {
    pub text: String,
    pub language: LanguageConfig,
    pub bindings: Option<BindingIndex>,
    pub index: SymbolIndex,
}

/// `.lua` extension -> the Lua-compatibility front end; everything else
/// (`.sol`, `.fl`, unknown/untitled) -> typed Sol, matching `sol`'s own CLI
/// convention in `main.rs::source_mode` (`.fl` is the legacy alias for
/// `.sol`).
pub fn source_mode(uri: &Url) -> SourceMode {
    let path = uri.path();
    if path.ends_with(".lua") {
        SourceMode::Lua
    } else {
        SourceMode::Sol
    }
}

impl Document {
    pub fn new(text: String, mode: SourceMode) -> (Self, Vec<tower_lsp::lsp_types::Diagnostic>) {
        let analysis = diagnostics::analyze(&text, mode);
        let index = analysis
            .syntax
            .as_ref()
            .map(|syntax| index::build(&syntax.ast))
            .unwrap_or_default();
        (
            Document {
                text,
                language: mode.into(),
                bindings: analysis.bindings,
                index,
            },
            analysis.diagnostics,
        )
    }
}

#[derive(Default)]
pub struct DocumentStore {
    docs: DashMap<Url, Document>,
}

impl DocumentStore {
    pub fn open(&self, uri: Url, text: String) -> Vec<tower_lsp::lsp_types::Diagnostic> {
        let mode = source_mode(&uri);
        let (doc, diagnostics) = Document::new(text, mode);
        self.docs.insert(uri, doc);
        diagnostics
    }

    pub fn close(&self, uri: &Url) {
        self.docs.remove(uri);
    }

    pub fn with_doc<T>(&self, uri: &Url, f: impl FnOnce(&Document) -> T) -> Option<T> {
        self.docs.get(uri).map(|entry| f(entry.value()))
    }

    pub fn for_each<T>(&self, mut f: impl FnMut(&Url, &Document) -> Vec<T>) -> Vec<T> {
        let mut out = Vec::new();
        for entry in self.docs.iter() {
            out.extend(f(entry.key(), entry.value()));
        }
        out
    }
}
