use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::document::DocumentStore;
use crate::features;

pub struct Backend {
    pub client: Client,
    pub docs: DocumentStore,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend {
            client,
            docs: DocumentStore::default(),
        }
    }

    async fn publish(&self, uri: Url, diagnostics: Vec<Diagnostic>) {
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "sol-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "sol-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let diagnostics = self.docs.open(uri.clone(), params.text_document.text);
        self.publish(uri, diagnostics).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        // `TextDocumentSyncKind::FULL` means the last content change carries
        // the entire new document text.
        let Some(change) = params.content_changes.pop() else {
            return;
        };
        let uri = params.text_document.uri;
        let diagnostics = self.docs.open(uri.clone(), change.text);
        self.publish(uri, diagnostics).await;
    }

    async fn did_save(&self, _: DidSaveTextDocumentParams) {}

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.close(&params.text_document.uri);
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self.docs.with_doc(&uri, |doc| features::hover(doc, pos)).flatten().map(|md| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: None,
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .docs
            .with_doc(&uri, |doc| features::definition(doc, &uri, pos))
            .flatten()
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        Ok(self.docs.with_doc(&uri, |doc| {
            let mut ranges = features::word_occurrences_at(doc, pos);
            if !include_declaration {
                if let Some((word, _)) = crate::text::word_at_position(&doc.text, pos) {
                    if let Some(f) = doc.index.find_function(&word) {
                        let decl_line = f.line.saturating_sub(1);
                        ranges.retain(|r| r.start.line != decl_line);
                    }
                }
            }
            ranges
                .into_iter()
                .map(|r| Location::new(uri.clone(), r))
                .collect::<Vec<_>>()
        }))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self.docs.with_doc(&uri, |doc| {
            features::word_occurrences_at(doc, pos)
                .into_iter()
                .map(|range| DocumentHighlight {
                    range,
                    kind: Some(DocumentHighlightKind::TEXT),
                })
                .collect::<Vec<_>>()
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        Ok(self
            .docs
            .with_doc(&uri, |doc| DocumentSymbolResponse::Nested(features::document_symbols(doc)))
            )
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        #[allow(deprecated)]
        let results = self.docs.for_each(|uri, doc| {
            features::document_symbols(doc)
                .into_iter()
                .filter(|sym| query.is_empty() || sym.name.to_lowercase().contains(&query))
                .map(|sym| SymbolInformation {
                    name: sym.name,
                    kind: sym.kind,
                    tags: None,
                    deprecated: None,
                    location: Location::new(uri.clone(), sym.selection_range),
                    container_name: None,
                })
                .collect::<Vec<_>>()
        });
        Ok(Some(results))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let edits = self.docs.with_doc(&uri, |doc| {
            features::word_occurrences_at(doc, pos)
                .into_iter()
                .map(|range| TextEdit {
                    range,
                    new_text: new_name.clone(),
                })
                .collect::<Vec<_>>()
        });
        let Some(edits) = edits.filter(|e| !e.is_empty()) else {
            return Ok(None);
        };
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri, edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        Ok(self.docs.with_doc(&uri, |doc| {
            let enclosing = doc.index.enclosing_function_at(&doc.text, pos.line + 1);
            CompletionResponse::Array(features::completions(
                &doc.index,
                doc.language,
                &enclosing,
                pos.line + 1,
            ))
        }))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .docs
            .with_doc(&uri, |doc| features::signature_help(doc, pos))
            .flatten())
    }
}
