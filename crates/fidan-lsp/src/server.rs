//! tower-lsp `LanguageServer` implementation for Fidan.

use crate::{
    analysis, convert, document::Document, semantic, store::DocumentStore, symbols::SymKind,
    symbols::SymbolEntry,
};
use fidan_fmt::{FormatOptions, format_source};
use fidan_source::{FileId, SourceFile, Span};
use std::collections::HashMap;
use std::sync::Arc;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

// ── Keyword / builtin completion lists ────────────────────────────────────────

const COMPLETION_KEYWORDS: &[&str] = &[
    "var",
    "const",
    "action",
    "object",
    "extends",
    "return",
    "if",
    "otherwise",
    "when",
    "then",
    "for",
    "in",
    "while",
    "break",
    "continue",
    "attempt",
    "catch",
    "finally",
    "panic",
    "use",
    "export",
    "check",
    "as",
    "oftype",
    "certain",
    "optional",
    "dynamic",
    "flexible",
    "parallel",
    "concurrent",
    "task",
    "spawn",
    "await",
    "Shared",
    "Pending",
    "WeakShared",
    "test",
    "tuple",
    "nothing",
    "true",
    "false",
    "and",
    "or",
    "not",
    "set",
    "also",
    "with",
    "returns",
    "this",
    "parent",
    "new",
];

const BUILTIN_FUNCTIONS: &[&str] = &[
    "print",
    "println",
    "eprint",
    "input",
    "len",
    "type",
    "string",
    "integer",
    "float",
    "boolean",
    "assert",
    "assert_eq",
    "assert_ne",
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
    /// the editor.  Also proactively loads any `use "file.fdn" as alias`
    /// imports that are not yet in the store.
    async fn refresh(&self, uri: &Url, version: i32, text: &str) {
        let result = analysis::analyze(text, uri.as_str());

        // Compute absolute URLs for every file-path import in this document.
        let current_path = uri.to_file_path().ok();
        let import_urls: HashMap<String, Url> = result
            .imports
            .iter()
            .filter_map(|(alias, rel_path)| {
                let abs = if rel_path.starts_with('/') || rel_path.contains(':') {
                    std::path::PathBuf::from(rel_path)
                } else if let Some(parent) = current_path.as_ref().and_then(|p| p.parent()) {
                    parent.join(rel_path)
                } else {
                    return None;
                };
                let url = Url::from_file_path(&abs).ok()?;
                Some((alias.clone(), url))
            })
            .collect();

        self.store.insert(
            uri.clone(),
            Document {
                version,
                text: text.to_owned(),
                diagnostics: result.diagnostics.clone(),
                semantic_tokens: result.semantic_tokens,
                symbol_table: result.symbol_table,
                identifier_spans: result.identifier_spans,
                imports: import_urls.clone(),
            },
        );
        // Proactively analyse imported files.  Background-loaded documents
        // (version == -1) are always re-read from disk so that edits to imported
        // files are reflected immediately without requiring the user to open them
        // in the editor.  Files that are actively open in the editor (version ≥ 0)
        // are managed through their own did-open / did-change notifications and
        // must NOT be overwritten with the on-disk version here.
        for (_, import_url) in &import_urls {
            let skip = self
                .store
                .get(import_url)
                .map(|d| d.version >= 0)
                .unwrap_or(false);
            if skip {
                continue; // actively open in editor — let did_change manage it
            }
            if let Ok(path) = import_url.to_file_path() {
                if let Ok(file_text) = std::fs::read_to_string(&path) {
                    let r = analysis::analyze(&file_text, import_url.as_str());
                    self.store.insert(
                        import_url.clone(),
                        Document {
                            version: -1, // -1 = background-loaded; reloaded on every parent refresh
                            text: file_text,
                            diagnostics: vec![], // no diagnostics for background docs
                            semantic_tokens: r.semantic_tokens,
                            symbol_table: r.symbol_table,
                            identifier_spans: r.identifier_spans,
                            imports: HashMap::new(),
                        },
                    );
                }
            }
        }

        // Patch `var x: dynamic` entries whose init was a cross-module method call.
        // Now that background docs are loaded we can resolve the actual return type.
        for (var_name, recv_ty, method_name) in &result.dynamic_var_call_sites {
            if let Some((_, entry)) = self.resolve_member_cross_doc(recv_ty, method_name) {
                if let Some(ref ret_type) = entry.return_type {
                    if let Some(mut doc) = self.store.get_mut(uri) {
                        if let Some(sym_entry) = doc.symbol_table.entries.get_mut(var_name) {
                            // Update the hover detail to show the real return type.
                            let kw = if matches!(
                                sym_entry.kind,
                                crate::symbols::SymKind::Variable { is_const: true }
                            ) {
                                "const var"
                            } else {
                                "var"
                            };
                            sym_entry.detail =
                                format!("```fidan\n{} {}: {}\n```", kw, var_name, ret_type);
                            // Also set ty_name so member accesses on `x` can be resolved
                            // if the return type is an object type.
                            sym_entry.ty_name = Some(ret_type.clone());
                        }
                    }
                }
            }
        }

        // LSP-level cross-module validation — runs after imported docs are in
        // the store so the symbol-table search can traverse the full chain.
        let extra = self.check_cross_module_diagnostics(
            text,
            uri,
            &result.cross_module_field_accesses,
            &result.cross_module_call_sites,
        );
        let mut all_diags = result.diagnostics;
        all_diags.extend(extra);
        self.client
            .publish_diagnostics(uri.clone(), all_diags, Some(version))
            .await;
    }

    /// Walk the type/parent-class chain across all open documents looking
    /// for a `"TypeName.member"` symbol entry.
    ///
    /// **Precondition**: no `DashMap` `Ref` (from `store.get()`) may be held
    /// when calling this — `store.find_in_any_doc()` iterates all shards.
    fn resolve_member_cross_doc(
        &self,
        type_name: &str,
        member_name: &str,
    ) -> Option<(Url, SymbolEntry)> {
        let mut cur_type = type_name.to_string();
        for _ in 0..8 {
            let key = format!("{}.{}", cur_type, member_name);
            if let Some(result) = self.store.find_in_any_doc(&key) {
                return Some(result);
            }
            // Follow the parent chain: get the Object entry for `cur_type`
            // from any open document and check its `ty_name` (= parent class).
            let (_, type_entry) = self.store.find_in_any_doc(&cur_type)?;
            cur_type = type_entry.ty_name?;
        }
        None
    }
    /// Check cross-module field accesses and method calls that the single-file
    /// type checker couldn't verify because the parent / receiver type lives in
    /// an imported document.  Returns supplementary LSP diagnostics.
    fn check_cross_module_diagnostics(
        &self,
        doc_text: &str,
        file_uri: &Url,
        field_accesses: &[(String, String, Span)],
        call_sites: &[fidan_typeck::CrossModuleCallSite],
    ) -> Vec<Diagnostic> {
        let file = SourceFile::new(FileId(0), file_uri.as_str(), doc_text);
        let mut diags: Vec<Diagnostic> = vec![];

        // ── Unknown field / method accesses (non-call) ────────────────────────
        for (type_name, member_name, span) in field_accesses {
            // Only emit when the type is loaded somewhere (avoids false
            // positives when the imported file hasn't been analysed yet).
            if self.store.find_in_any_doc(type_name).is_none() {
                continue;
            }
            if self
                .resolve_member_cross_doc(type_name, member_name)
                .is_some()
            {
                continue; // member found — no error
            }
            diags.push(Diagnostic {
                range: convert::span_to_range(&file, *span),
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String("E0204".into())),
                source: Some("fidan".into()),
                message: format!(
                    "object `{}` has no field or method `{}`",
                    type_name, member_name
                ),
                ..Default::default()
            });
        }

        // ── Method call argument type mismatches ──────────────────────────────
        for site in call_sites {
            match self.resolve_member_cross_doc(&site.receiver_ty, &site.method_name) {
                None => {
                    // Method doesn't exist anywhere — emit E0204 if the
                    // receiver type is known (i.e. we have definitive info).
                    if self.store.find_in_any_doc(&site.receiver_ty).is_some() {
                        diags.push(Diagnostic {
                            range: convert::span_to_range(&file, site.span),
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: Some(NumberOrString::String("E0204".into())),
                            source: Some("fidan".into()),
                            message: format!(
                                "object `{}` has no field or method `{}`",
                                site.receiver_ty, site.method_name
                            ),
                            ..Default::default()
                        });
                    }
                }
                Some((_, entry)) => {
                    // Check that all required parameters are provided (E0301).
                    let required_count = entry.param_required.iter().filter(|&&r| r).count();
                    if site.arg_tys.len() < required_count {
                        diags.push(Diagnostic {
                            range: convert::span_to_range(&file, site.span),
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: Some(NumberOrString::String("E0301".into())),
                            source: Some("fidan".into()),
                            message: format!(
                                "not enough arguments for `{}`: {} required but {} provided",
                                site.method_name,
                                required_count,
                                site.arg_tys.len()
                            ),
                            ..Default::default()
                        });
                    } else {
                        // Method found — validate argument types against param types.
                        for (i, (param_ty, arg_ty)) in entry
                            .param_types
                            .iter()
                            .zip(site.arg_tys.iter())
                            .enumerate()
                        {
                            if !Self::types_compatible(param_ty, arg_ty) {
                                diags.push(Diagnostic {
                                    range: convert::span_to_range(&file, site.span),
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: Some(NumberOrString::String("E0302".into())),
                                    source: Some("fidan".into()),
                                    message: format!(
                                        "argument {} of `{}` expects type `{}`, found `{}`",
                                        i + 1,
                                        site.method_name,
                                        param_ty,
                                        arg_ty,
                                    ),
                                    ..Default::default()
                                });
                                break; // report first mismatch only
                            }
                        }
                    }
                }
            }
        }

        diags
    }

    fn types_compatible(expected: &str, actual: &str) -> bool {
        expected == actual
            || matches!(expected, "dynamic" | "?")
            || matches!(actual, "dynamic" | "?")
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

        // Phase 1: in-document lookup while holding the DashMap read lock.
        // We drop the lock before any cross-document iteration to avoid
        // re-entrant shard locking with DashMap.
        enum Phase1 {
            Found(String),            // detail string, ready to return
            CrossDoc(String, String), // (type_name, member_name) to search across docs
            ImportDoc(Url, String),   // (import_file_url, symbol_name) — for `module.Type`
            NotFound,
        }

        let phase1 = {
            let doc = match self.store.get(uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let offset = lsp_pos_to_offset(&file, pos);
            let spans = &doc.identifier_spans;
            let hit_idx = match spans
                .iter()
                .position(|(s, _)| offset >= s.start && offset < s.end)
            {
                Some(i) => i,
                None => return Ok(None),
            };
            let (cur_span, cur_name) = &spans[hit_idx];
            let prev_name: Option<&str> = if hit_idx > 0 {
                let (prev_span, prev_name) = &spans[hit_idx - 1];
                if cur_span.start == prev_span.end + 1 {
                    Some(prev_name.as_str())
                } else {
                    None
                }
            } else {
                None
            };
            // Direct in-doc lookups: plain → qualified → type-resolved.
            let in_doc = doc.symbol_table.get(cur_name.as_str()).or_else(|| {
                let pn = prev_name?;
                if let Some(e) = doc.symbol_table.get(&format!("{}.{}", pn, cur_name)) {
                    return Some(e);
                }
                if let Some(pe) = doc.symbol_table.get(pn) {
                    if let Some(ty) = &pe.ty_name {
                        return doc.symbol_table.get(&format!("{}.{}", ty, cur_name));
                    }
                }
                None
            });
            if let Some(e) = in_doc {
                Phase1::Found(e.detail.clone())
            } else if let Some(pn) = prev_name {
                // `module.Type` — prev is a namespace alias for an imported file.
                if let Some(import_url) = doc.imports.get(pn) {
                    Phase1::ImportDoc(import_url.clone(), cur_name.clone())
                } else {
                    // Type-resolved: prev is a variable with known type.
                    let ty = doc.symbol_table.get(pn).and_then(|e| e.ty_name.clone());
                    match ty {
                        Some(t) => Phase1::CrossDoc(t, cur_name.clone()),
                        None => Phase1::NotFound,
                    }
                }
            } else if let Some(url) = doc.imports.get(cur_name.as_str()) {
                // The token is a module alias (e.g. hovering over `module` in
                // `use "test.fdn" as module`).
                let file_name = url
                    .path_segments()
                    .and_then(|mut s| s.next_back())
                    .unwrap_or("?")
                    .to_owned();
                Phase1::Found(format!(
                    "```fidan\nimport \"{}\" as {}\n```",
                    file_name, cur_name
                ))
            } else {
                Phase1::NotFound
            }
            // `doc` (DashMap Ref) is dropped here, releasing the shard lock.
        };

        // Phase 2: resolve or do cross-document parent-chain lookup.
        let detail = match phase1 {
            Phase1::Found(d) => d,
            Phase1::CrossDoc(ty, member) => match self.resolve_member_cross_doc(&ty, &member) {
                Some((_, e)) => e.detail,
                None => return Ok(None),
            },
            Phase1::ImportDoc(url, name) => {
                // Look up the symbol directly in the imported document.
                match self.store.get(&url) {
                    Some(d) => match d.symbol_table.get(&name) {
                        Some(e) => e.detail.clone(),
                        None => return Ok(None),
                    },
                    None => return Ok(None),
                }
            }
            Phase1::NotFound => return Ok(None),
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: detail,
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

        // Phase 1: in-document lookup (shard lock held).
        // `SourceFile` owns its text as `Arc<str>`, so it remains valid after
        // the `doc` lock is released.
        enum Phase1 {
            Found(Span),              // declaration span in the current document
            CrossDoc(String, String), // (type_name, member_name)
            ImportDoc(Url, String),   // (import_file_url, symbol_name) — for `module.Type`
            NotFound,
        }

        let (phase1, current_file) = {
            let doc = match self.store.get(uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let offset = lsp_pos_to_offset(&file, pos);
            let spans = &doc.identifier_spans;
            let hit_idx = match spans
                .iter()
                .position(|(s, _)| offset >= s.start && offset < s.end)
            {
                Some(i) => i,
                None => return Ok(None),
            };
            let (cur_span, cur_name) = &spans[hit_idx];
            let prev_name: Option<&str> = if hit_idx > 0 {
                let (prev_span, prev_name) = &spans[hit_idx - 1];
                if cur_span.start == prev_span.end + 1 {
                    Some(prev_name.as_str())
                } else {
                    None
                }
            } else {
                None
            };
            let in_doc = doc.symbol_table.get(cur_name.as_str()).or_else(|| {
                let pn = prev_name?;
                if let Some(e) = doc.symbol_table.get(&format!("{}.{}", pn, cur_name)) {
                    return Some(e);
                }
                if let Some(pe) = doc.symbol_table.get(pn) {
                    if let Some(ty) = &pe.ty_name {
                        return doc.symbol_table.get(&format!("{}.{}", ty, cur_name));
                    }
                }
                None
            });
            let p1 = if let Some(e) = in_doc {
                Phase1::Found(e.span)
            } else if let Some(pn) = prev_name {
                // `module.Type` — prev is a namespace alias for an imported file.
                if let Some(import_url) = doc.imports.get(pn) {
                    Phase1::ImportDoc(import_url.clone(), cur_name.clone())
                } else {
                    let ty = doc.symbol_table.get(pn).and_then(|e| e.ty_name.clone());
                    match ty {
                        Some(t) => Phase1::CrossDoc(t, cur_name.clone()),
                        None => Phase1::NotFound,
                    }
                }
            } else {
                Phase1::NotFound
            };
            (p1, file) // `doc` dropped here
        };

        // Phase 2: resolve span + source URI (may require cross-doc lookup).
        let (def_uri, span) = match phase1 {
            Phase1::Found(span) => (uri.clone(), span),
            Phase1::CrossDoc(ty, member) => match self.resolve_member_cross_doc(&ty, &member) {
                Some((src_uri, e)) => (src_uri, e.span),
                None => return Ok(None),
            },
            Phase1::ImportDoc(url, name) => {
                let span = {
                    let d = match self.store.get(&url) {
                        Some(d) => d,
                        None => return Ok(None),
                    };
                    match d.symbol_table.get(&name) {
                        Some(e) => e.span,
                        None => return Ok(None),
                    }
                    // `d` dropped here
                };
                (url, span)
            }
            Phase1::NotFound => return Ok(None),
        };

        // Build the LSP Range. Use the already-constructed `current_file` for
        // same-document definitions; re-fetch text for cross-document ones.
        let range = if def_uri == *uri {
            convert::span_to_range(&current_file, span)
        } else {
            let doc = match self.store.get(&def_uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), def_uri.as_str(), doc.text.as_str());
            convert::span_to_range(&file, span)
        };

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: def_uri,
            range,
        })))
    }

    // ── Completion ─────────────────────────────────────────────────────────

    async fn completion(&self, params: CompletionParams) -> RpcResult<Option<CompletionResponse>> {
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

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };

        // Never format while there are errors — the formatter may produce
        // `<error>` placeholder tokens that corrupt the document.
        let has_errors = doc
            .diagnostics
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::ERROR));
        if has_errors {
            return Ok(None);
        }

        let text = doc.text.clone();
        drop(doc);

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
