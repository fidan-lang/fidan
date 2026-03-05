//! tower-lsp `LanguageServer` implementation for Fidan.

use crate::{
    analysis, convert, document::Document, semantic, store::DocumentStore, symbols::SymKind,
};
use fidan_fmt::{FormatOptions, format_source};
use fidan_source::{FileId, SourceFile};
use std::sync::Arc;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

// ── Keyword / builtin completion lists ────────────────────────────────────────

const COMPLETION_KEYWORDS: &[&str] = &[
    "var", "const", "action", "object", "extends", "return",
    "if", "otherwise", "when", "then", "for", "in", "while",
    "break", "continue", "attempt", "catch", "finally",
    "panic", "use", "export", "check", "as", "oftype",
    "certain", "optional", "dynamic", "flexible", "parallel",
    "concurrent", "task", "spawn", "await", "Shared", "Pending",
    "WeakShared", "test", "tuple", "nothing", "true", "false",
    "and", "or", "not", "set", "also", "with", "returns",
    "this", "parent", "new",
];

const BUILTIN_FUNCTIONS: &[&str] = &[
    "print", "println", "eprint", "input", "len", "type",
    "string", "integer", "float", "boolean",
    "assert", "assert_eq", "assert_ne",
];

// ── Server ────────────────────────────────────────────────────────────────────

/// The stateful backend object shared across all LSP requests.
pub struct FidanLsp {
    client: Client,
    store: Arc<DocumentStore>,
}

impl FidanLsp {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            store: Arc::new(DocumentStore::new()),
        }
    }

    /// Re-analyse `text`, update the document store and push diagnostics to
    /// the editor.
    async fn refresh(&self, uri: &Url, version: i32, text: &str) {
        let result = analysis::analyze(text, uri.as_str());
        self.store.insert(
            uri.clone(),
            Document {
                version,
                text: text.to_owned(),
                diagnostics: result.diagnostics.clone(),
                semantic_tokens: result.semantic_tokens,
                symbol_table: result.symbol_table,
                identifier_spans: result.identifier_spans,
            },
        );
        self.client
            .publish_diagnostics(uri.clone(), result.diagnostics, Some(version))
            .await;
    }
}

// ── LanguageServer implementation ─────────────────────────────────────────────

#[tower_lsp::async_trait]
impl LanguageServer for FidanLsp {
    // ── Lifecycle ──────────────────────────────────────────────────────────

    async fn initialize(&self, _params: InitializeParams) -> RpcResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), " ".to_string()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                document_formatting_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            work_done_progress_options: WorkDoneProgressOptions {
                                work_done_progress: None,
                            },
                        },
                    ),
                ),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "fidan-lsp".to_string(),
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
        if let Some(change) = params.content_changes.into_iter().last() {
            self.refresh(
                &params.text_document.uri,
                params.text_document.version,
                &change.text,
            )
            .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.store.remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    // ── Hover ──────────────────────────────────────────────────────────────

    async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = &params.text_document_position_params.position;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };

        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
        let offset = lsp_pos_to_offset(&file, pos);

        let name = doc
            .identifier_spans
            .iter()
            .find(|(span, _)| offset >= span.start && offset < span.end)
            .map(|(_, n)| n.as_str());

        let name = match name {
            Some(n) => n,
            None => return Ok(None),
        };

        let entry = match doc.symbol_table.get(name) {
            Some(e) => e,
            None => return Ok(None),
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: entry.detail.clone(),
            }),
            range: None,
        }))
    }

    // ── Go-to-definition ───────────────────────────────────────────────────

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> RpcResult<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = &params.text_document_position_params.position;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };

        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
        let offset = lsp_pos_to_offset(&file, pos);

        let name = doc
            .identifier_spans
            .iter()
            .find(|(span, _)| offset >= span.start && offset < span.end)
            .map(|(_, n)| n.clone());

        let name = match name {
            Some(n) => n,
            None => return Ok(None),
        };

        let entry = match doc.symbol_table.get(&name) {
            Some(e) => e,
            None => return Ok(None),
        };

        let range = convert::span_to_range(&file, entry.span);
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range,
        })))
    }

    // ── Completion ─────────────────────────────────────────────────────────

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> RpcResult<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };

        // Declared symbols — skip "ClassName.field" qualified entries from basic completion.
        let mut items: Vec<CompletionItem> = doc
            .symbol_table
            .all()
            .filter(|(name, _)| !name.contains('.'))
            .map(|(name, entry)| {
                let kind = Some(match &entry.kind {
                    SymKind::Action | SymKind::Method => CompletionItemKind::FUNCTION,
                    SymKind::Object => CompletionItemKind::CLASS,
                    SymKind::Variable { .. } => CompletionItemKind::VARIABLE,
                    SymKind::Field => CompletionItemKind::FIELD,
                });
                CompletionItem {
                    label: name.clone(),
                    kind,
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: entry.detail.clone(),
                    })),
                    ..Default::default()
                }
            })
            .collect();

        // Language keywords.
        for &kw in COMPLETION_KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        // Built-in functions (not in typeck.actions, added explicitly).
        for &builtin in BUILTIN_FUNCTIONS {
            items.push(CompletionItem {
                label: builtin.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    // ── Semantic tokens ────────────────────────────────────────────────────

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> RpcResult<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let tokens = self
            .store
            .get(uri)
            .map(|doc| doc.semantic_tokens.clone())
            .unwrap_or_default();

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: tokens,
        })))
    }

    // ── Formatting ─────────────────────────────────────────────────────────

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> RpcResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;

        let text = match self.store.get(uri) {
            Some(doc) => doc.text.clone(),
            None => return Ok(None),
        };

        let opts = FormatOptions {
            indent_width: params.options.tab_size as usize,
            ..Default::default()
        };

        let formatted = format_source(&text, &opts);

        if formatted == text {
            return Ok(Some(vec![]));
        }

        Ok(Some(vec![TextEdit {
            range: convert::whole_document_range(&text),
            new_text: formatted,
        }]))
    }
}

// ── Position utilities ────────────────────────────────────────────────────────

/// Convert an LSP (0-based line, UTF-16 character offset) `Position` to a
/// byte offset in the source file.
fn lsp_pos_to_offset(file: &SourceFile, pos: &Position) -> u32 {
    let line = pos.line as usize;
    if line >= file.line_starts.len() {
        return file.src.len() as u32;
    }
    let line_start = file.line_starts[line] as usize;
    let line_end = if line + 1 < file.line_starts.len() {
        (file.line_starts[line + 1] as usize).saturating_sub(1) // exclude trailing '\n'
    } else {
        file.src.len()
    };
    let line_str = &file.src[line_start..line_end];
    // LSP character offsets are UTF-16 code units.
    let mut utf16 = 0u32;
    for (byte_idx, ch) in line_str.char_indices() {
        if utf16 >= pos.character {
            return (line_start + byte_idx) as u32;
        }
        utf16 += ch.len_utf16() as u32;
    }
    (line_start + line_str.len()) as u32
}
