//! tower-lsp `LanguageServer` implementation for Fidan.

use crate::{
    analysis,
    convert::whole_document_range,
    document::Document,
    store::DocumentStore,
};
use fidan_fmt::{FormatOptions, format_source};
use std::sync::Arc;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

/// The stateful backend object shared across all LSP requests.
pub struct FidanLsp {
    client: Client,
    store:  Arc<DocumentStore>,
}

impl FidanLsp {
    pub fn new(client: Client) -> Self {
        Self { client, store: Arc::new(DocumentStore::new()) }
    }

    /// Re-analyse `text`, update the document store and push diagnostics to
    /// the editor.
    async fn refresh(&self, uri: &Url, version: i32, text: &str) {
        let result = analysis::analyze(text, uri.as_str());
        self.store.insert(
            uri.clone(),
            Document { version, text: text.to_owned(), diagnostics: result.diagnostics.clone() },
        );
        self.client
            .publish_diagnostics(uri.clone(), result.diagnostics, Some(version))
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for FidanLsp {
    // ── Lifecycle ──────────────────────────────────────────────────────────

    async fn initialize(&self, _params: InitializeParams) -> RpcResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // Always send the full document text on every change — keeps
                // the implementation simple until incremental sync is needed.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name:    "fidan-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "fidan language server ready")
            .await;
    }

    async fn shutdown(&self) -> RpcResult<()> {
        Ok(())
    }

    // ── Document lifecycle ─────────────────────────────────────────────────

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let td = params.text_document;
        self.refresh(&td.uri, td.version, &td.text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // We use FULL sync, so there is exactly one change containing the
        // complete new text.
        if let Some(change) = params.content_changes.into_iter().last() {
            self.refresh(&params.text_document.uri, params.text_document.version, &change.text)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.store.remove(&params.text_document.uri);
        // Clear any lingering diagnostics so the editor gutter is clean.
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    // ── Formatting ─────────────────────────────────────────────────────────

    async fn formatting(&self, params: DocumentFormattingParams) -> RpcResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;

        let text = match self.store.get(uri) {
            Some(doc) => doc.text.clone(),
            None      => return Ok(None),
        };

        let opts = FormatOptions {
            indent_width: params.options.tab_size as usize,
            ..Default::default()
        };

        let formatted = format_source(&text, &opts);

        // If the text is already correctly formatted, return an empty edit
        // list so the editor does not record an unnecessary undo history entry.
        if formatted == text {
            return Ok(Some(vec![]));
        }

        Ok(Some(vec![TextEdit {
            range:    whole_document_range(&text),
            new_text: formatted,
        }]))
    }
}
