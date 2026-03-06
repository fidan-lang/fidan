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

// ── Named-arg goto-def result ───────────────────────────────────────────────

/// Result returned by `find_named_arg_param`.
enum NamedArgLookup {
    /// Parameter declaration found in the current document.
    InDoc(Span),
    /// The method owning the parameter lives in an imported document.
    /// The caller should call `resolve_member_cross_doc(recv_ty, method_name)` and
    /// search the returned `SymbolEntry::param_names` for `param_name`.
    CrossModule {
        recv_ty: String,
        method_name: String,
        param_name: String,
    },
}

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
                inlay_hint_sites: result.inlay_hint_sites,
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
                            inlay_hint_sites: vec![], // not shown for background docs
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
                                format!("```fidan\n{} {} -> {}\n```", kw, var_name, ret_type);
                            // Also set ty_name so member accesses on `x` can be resolved
                            // if the return type is an object type.
                            sym_entry.ty_name = Some(ret_type.clone());
                        }
                        // Also update the inlay hint label: it was set to `-> dynamic`
                        // during analysis but the real return type is now known.
                        if let Some((span, _)) =
                            doc.identifier_spans.iter().find(|(_, n)| n == var_name)
                        {
                            let end = span.end;
                            if let Some(hint) = doc
                                .inlay_hint_sites
                                .iter_mut()
                                .find(|h| h.byte_offset == end && h.is_type_hint)
                            {
                                hint.label = format!(" -> {}", ret_type);
                            }
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
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), " ".to_string()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
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
            Found(Span),                              // declaration span in the current document
            CrossDoc(String, String),                 // (type_name, member_name)
            CrossDocNamedArg(String, String, String), // (recv_ty, method_name, param_name)
            ImportDoc(Url, String), // (import_file_url, symbol_name) — for `module.Type`
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
            // Fallback: resolve named call-arguments (e.g. `times` in `foo(times = 10)`).
            let named_arg =
                find_named_arg_param(&doc.symbol_table, spans, hit_idx, cur_span, &doc.text);
            let named_to_phase1 = |l: NamedArgLookup| -> Phase1 {
                match l {
                    NamedArgLookup::InDoc(span) => Phase1::Found(span),
                    NamedArgLookup::CrossModule {
                        recv_ty,
                        method_name,
                        param_name,
                    } => Phase1::CrossDocNamedArg(recv_ty, method_name, param_name),
                }
            };
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
                        None => named_arg.map(named_to_phase1).unwrap_or(Phase1::NotFound),
                    }
                }
            } else {
                named_arg.map(named_to_phase1).unwrap_or(Phase1::NotFound)
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
            Phase1::CrossDocNamedArg(recv_ty, method, param) => {
                match self.resolve_member_cross_doc(&recv_ty, &method) {
                    Some((src_uri, e)) => {
                        let span = match e.param_names.iter().find(|(n, _)| *n == param) {
                            Some((_, s)) => *s,
                            None => return Ok(None),
                        };
                        (src_uri, span)
                    }
                    None => return Ok(None),
                }
            }
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
        let pos = &params.text_document_position.position;
        let trigger = params
            .context
            .as_ref()
            .and_then(|c| c.trigger_character.as_deref());

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };

        // ── Dot-triggered member completion ────────────────────────────────
        if trigger == Some(".") {
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let cursor = lsp_pos_to_offset(&file, pos);
            // Find the identifier token immediately before the `.` at `cursor - 1`.
            let dot_pos = cursor.saturating_sub(1);
            let recv_name = doc
                .identifier_spans
                .iter()
                .rev()
                .find(|(span, _)| span.end <= dot_pos)
                .map(|(_, name)| name.as_str());

            let ty_name = recv_name.and_then(|rn| {
                doc.symbol_table
                    .get(rn)
                    .and_then(|e| e.ty_name.as_deref())
                    .map(|s| s.to_string())
            });

            if let Some(ty) = ty_name {
                let prefix = format!("{}.", ty);
                let items: Vec<CompletionItem> = doc
                    .symbol_table
                    .all()
                    .filter(|(name, _)| name.starts_with(&prefix))
                    .map(|(name, entry)| {
                        let member = &name[prefix.len()..];
                        let kind = Some(match &entry.kind {
                            SymKind::Method => CompletionItemKind::METHOD,
                            SymKind::Field => CompletionItemKind::FIELD,
                            _ => CompletionItemKind::FIELD,
                        });
                        let insert_text = if matches!(entry.kind, SymKind::Method)
                            && !entry.param_types.is_empty()
                        {
                            Some(format!("{}($0)", member))
                        } else {
                            None
                        };
                        CompletionItem {
                            label: member.to_string(),
                            kind,
                            insert_text_format: insert_text
                                .as_ref()
                                .map(|_| InsertTextFormat::SNIPPET),
                            insert_text,
                            documentation: Some(Documentation::MarkupContent(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: entry.detail.clone(),
                            })),
                            ..Default::default()
                        }
                    })
                    .collect();
                return Ok(Some(CompletionResponse::Array(items)));
            }
        }

        // ── Standard (non-dot) completion ──────────────────────────────────

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
                // For actions/objects with parameters, provide snippet insert text.
                let insert_text = if matches!(entry.kind, SymKind::Action | SymKind::Object)
                    && !entry.param_types.is_empty()
                {
                    Some(format!("{}($0)", name))
                } else {
                    None
                };
                CompletionItem {
                    label: name.clone(),
                    kind,
                    insert_text_format: insert_text.as_ref().map(|_| InsertTextFormat::SNIPPET),
                    insert_text,
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
                insert_text: Some(format!("{}($0)", builtin)),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    // ── Signature help ─────────────────────────────────────────────────────

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> RpcResult<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = &params.text_document_position_params.position;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
        let cursor = lsp_pos_to_offset(&file, pos) as usize;
        let src = doc.text.as_bytes();

        // Walk backward from cursor to locate the opening `(` of the current call.
        let mut depth: i32 = 0;
        let mut open_paren: Option<usize> = None;
        let mut i = cursor.saturating_sub(1);
        loop {
            match src.get(i) {
                Some(b')') | Some(b']') => depth += 1,
                Some(b'(') | Some(b'[') => {
                    if depth == 0 {
                        open_paren = Some(i);
                        break;
                    }
                    depth -= 1;
                }
                None => break,
                _ => {}
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        let open = match open_paren {
            Some(o) => o,
            None => return Ok(None),
        };

        // Find the function name: the identifier ending just before the `(`.
        let fn_name = doc
            .identifier_spans
            .iter()
            .rev()
            .find(|(span, _)| span.end as usize <= open)
            .map(|(_, name)| name.as_str());
        let fn_name = match fn_name {
            Some(n) => n,
            None => return Ok(None),
        };

        // Look up the function/action in the symbol table.
        let entry = match doc.symbol_table.get(fn_name) {
            Some(e) => e,
            None => return Ok(None),
        };
        if entry.param_types.is_empty() {
            return Ok(None);
        }

        // Count commas at depth 0 between `open` and `cursor` to find the active param.
        let mut active_param = 0u32;
        let mut pd: i32 = 0;
        for &byte in &src[open + 1..cursor.min(src.len())] {
            match byte {
                b'(' | b'[' => pd += 1,
                b')' | b']' => pd -= 1,
                b',' if pd == 0 => active_param += 1,
                _ => {}
            }
        }

        // Build parameter labels from the hover detail or param_types.
        let params: Vec<ParameterInformation> = entry
            .param_types
            .iter()
            .enumerate()
            .map(|(idx, ty)| {
                // Try to get the parameter name from the signature in `detail`.
                // detail looks like: `action foo with (x -> integer, y -> string) ...`
                let label = extract_param_label_from_detail(&entry.detail, idx)
                    .unwrap_or_else(|| format!("param{} -> {}", idx + 1, ty));
                ParameterInformation {
                    label: ParameterLabel::Simple(label),
                    documentation: None,
                }
            })
            .collect();

        let sig_label = build_signature_label(fn_name, &entry.detail);
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: sig_label,
                documentation: None,
                parameters: Some(params),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        }))
    }

    // ── References ─────────────────────────────────────────────────────────

    async fn references(&self, params: ReferenceParams) -> RpcResult<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = &params.text_document_position.position;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
        let cursor = lsp_pos_to_offset(&file, pos);

        // Find the symbol name at the cursor.
        let sym_name = match doc
            .identifier_spans
            .iter()
            .find(|(s, _)| cursor >= s.start && cursor < s.end)
        {
            Some((_, name)) => name.clone(),
            None => return Ok(None),
        };

        // Collect every occurrence of that name across this document's identifier_spans.
        let locs: Vec<Location> = doc
            .identifier_spans
            .iter()
            .filter(|(_, n)| n == &sym_name)
            .map(|(span, _)| Location {
                uri: uri.clone(),
                range: convert::span_to_range(&file, *span),
            })
            .collect();

        Ok(if locs.is_empty() { None } else { Some(locs) })
    }

    // ── Rename ─────────────────────────────────────────────────────────────

    async fn rename(&self, params: RenameParams) -> RpcResult<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = &params.text_document_position.position;
        let new_name = &params.new_name;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
        let cursor = lsp_pos_to_offset(&file, pos);

        let sym_name = match doc
            .identifier_spans
            .iter()
            .find(|(s, _)| cursor >= s.start && cursor < s.end)
        {
            Some((_, name)) => name.clone(),
            None => return Ok(None),
        };

        let edits: Vec<TextEdit> = doc
            .identifier_spans
            .iter()
            .filter(|(_, n)| n == &sym_name)
            .map(|(span, _)| TextEdit {
                range: convert::span_to_range(&file, *span),
                new_text: new_name.clone(),
            })
            .collect();

        if edits.is_empty() {
            return Ok(None);
        }
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri.clone(), edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    // ── Document symbol (outline) ──────────────────────────────────────────

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> RpcResult<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());

        let mut symbols: Vec<DocumentSymbol> = Vec::new();

        // Objects first — build them with their member children.
        let mut object_names: Vec<String> = doc
            .symbol_table
            .all()
            .filter(|(name, entry)| !name.contains('.') && matches!(entry.kind, SymKind::Object))
            .map(|(name, _)| name.clone())
            .collect();
        object_names.sort();

        for obj_name in &object_names {
            let entry = match doc.symbol_table.get(obj_name) {
                Some(e) => e,
                None => continue,
            };
            let prefix = format!("{}.", obj_name);
            let mut children: Vec<DocumentSymbol> = doc
                .symbol_table
                .all()
                .filter(|(name, _)| name.starts_with(&prefix))
                .map(|(name, child)| {
                    let member = &name[prefix.len()..];
                    let kind = match &child.kind {
                        SymKind::Method => SymbolKind::METHOD,
                        SymKind::Field => SymbolKind::FIELD,
                        _ => SymbolKind::FIELD,
                    };
                    #[allow(deprecated)]
                    DocumentSymbol {
                        name: member.to_string(),
                        detail: None,
                        kind,
                        tags: None,
                        deprecated: None,
                        range: convert::span_to_range(&file, child.span),
                        selection_range: convert::span_to_range(&file, child.span),
                        children: None,
                    }
                })
                .collect();
            children.sort_by(|a, b| a.name.cmp(&b.name));

            #[allow(deprecated)]
            symbols.push(DocumentSymbol {
                name: obj_name.clone(),
                detail: None,
                kind: SymbolKind::CLASS,
                tags: None,
                deprecated: None,
                range: convert::span_to_range(&file, entry.span),
                selection_range: convert::span_to_range(&file, entry.span),
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            });
        }

        // Top-level actions.
        let mut actions: Vec<(String, _)> = doc
            .symbol_table
            .all()
            .filter(|(name, entry)| !name.contains('.') && matches!(entry.kind, SymKind::Action))
            .map(|(n, e)| (n.clone(), e.clone()))
            .collect();
        actions.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry) in actions {
            #[allow(deprecated)]
            symbols.push(DocumentSymbol {
                name,
                detail: None,
                kind: SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                range: convert::span_to_range(&file, entry.span),
                selection_range: convert::span_to_range(&file, entry.span),
                children: None,
            });
        }

        // Top-level variables.
        let mut vars: Vec<(String, _)> = doc
            .symbol_table
            .all()
            .filter(|(name, entry)| {
                !name.contains('.') && matches!(entry.kind, SymKind::Variable { .. })
            })
            .map(|(n, e)| (n.clone(), e.clone()))
            .collect();
        vars.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry) in vars {
            let kind = if matches!(entry.kind, SymKind::Variable { is_const: true }) {
                SymbolKind::CONSTANT
            } else {
                SymbolKind::VARIABLE
            };
            #[allow(deprecated)]
            symbols.push(DocumentSymbol {
                name,
                detail: None,
                kind,
                tags: None,
                deprecated: None,
                range: convert::span_to_range(&file, entry.span),
                selection_range: convert::span_to_range(&file, entry.span),
                children: None,
            });
        }

        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    // ── Folding ranges ─────────────────────────────────────────────────────

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> RpcResult<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let text = doc.text.clone();
        drop(doc);

        let ranges = compute_folding_ranges(&text);
        Ok(if ranges.is_empty() {
            None
        } else {
            Some(ranges)
        })
    }

    // ── Inlay hints ────────────────────────────────────────────────────────

    async fn inlay_hint(&self, params: InlayHintParams) -> RpcResult<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
        let range = params.range;

        let hints: Vec<InlayHint> = doc
            .inlay_hint_sites
            .iter()
            .filter_map(|site| {
                let pos = offset_to_lsp_pos(&file, site.byte_offset);
                // Only return hints within the requested range.
                if pos.line < range.start.line || pos.line > range.end.line {
                    return None;
                }
                Some(InlayHint {
                    position: pos,
                    label: InlayHintLabel::String(site.label.clone()),
                    kind: if site.is_type_hint {
                        Some(InlayHintKind::TYPE)
                    } else {
                        Some(InlayHintKind::PARAMETER)
                    },
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                })
            })
            .collect();

        Ok(if hints.is_empty() { None } else { Some(hints) })
    }

    // ── Code actions ───────────────────────────────────────────────────────

    async fn code_action(&self, params: CodeActionParams) -> RpcResult<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let range = &params.range;

        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());

        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        for diag in &doc.diagnostics {
            // Only offer fixes for diagnostics that overlap the requested range.
            if !ranges_overlap(&diag.range, range) {
                continue;
            }
            // Extract structured fixes stored in diagnostic data.
            let fixes = match diag.data.as_ref().and_then(|v| v.as_array()) {
                Some(arr) => arr.clone(),
                None => continue,
            };
            for fix in &fixes {
                let message = fix["message"].as_str().unwrap_or("Apply fix").to_string();
                let replacement = fix["replacement"].as_str().unwrap_or("").to_string();
                let start = fix["start"].as_u64().unwrap_or(0) as u32;
                let end = fix["end"].as_u64().unwrap_or(0) as u32;
                let span = fidan_source::Span {
                    file: fidan_source::FileId(0),
                    start,
                    end,
                };
                let edit_range = convert::span_to_range(&file, span);

                let mut changes = std::collections::HashMap::new();
                changes.insert(
                    uri.clone(),
                    vec![TextEdit {
                        range: edit_range,
                        new_text: replacement,
                    }],
                );
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: message,
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diag.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    is_preferred: Some(true),
                    ..Default::default()
                }));
            }
        }

        Ok(if actions.is_empty() {
            None
        } else {
            Some(actions)
        })
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

// ── Folding range helpers ─────────────────────────────────────────────────────

/// Compute folding ranges by tracking matching `{`/`}` pairs in the source,
/// ignoring braces inside strings and comments.
fn compute_folding_ranges(text: &str) -> Vec<FoldingRange> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let lines: Vec<&str> = text.lines().collect();
    // Precompute byte offset → line number (0-based) via the line-start table.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && i + 1 < n {
            line_starts.push(i + 1);
        }
    }
    let byte_to_line = |pos: usize| -> u32 {
        match line_starts.binary_search(&pos) {
            Ok(l) => l as u32,
            Err(l) => (l.saturating_sub(1)) as u32,
        }
    };

    let mut stack: Vec<usize> = Vec::new(); // byte offsets of unmatched `{`
    let mut ranges: Vec<FoldingRange> = Vec::new();
    let mut i = 0;
    let mut in_string = false;
    let mut in_line_comment = false;

    while i < n {
        let b = bytes[i];
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_string {
            if b == b'\\' {
                i += 2;
                continue;
            } // skip escape
            if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
            }
            b'#' => {
                in_line_comment = true;
            }
            b'{' => {
                stack.push(i);
            }
            b'}' => {
                if let Some(open) = stack.pop() {
                    let start_line = byte_to_line(open);
                    let end_line = byte_to_line(i);
                    if end_line > start_line {
                        // Fold from end of opening line to line before closing brace.
                        ranges.push(FoldingRange {
                            start_line,
                            start_character: None,
                            end_line: end_line.saturating_sub(1),
                            end_character: None,
                            kind: Some(FoldingRangeKind::Region),
                            collapsed_text: None,
                        });
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    // Block comments `#/ ... /#`
    let src = text;
    let mut pos = 0;
    while let Some(start) = src[pos..].find("#/").map(|p| pos + p) {
        if let Some(rel) = src[start + 2..].find("/#") {
            let end = start + 2 + rel + 2;
            let sl = byte_to_line(start);
            let el = byte_to_line(end);
            if el > sl {
                ranges.push(FoldingRange {
                    start_line: sl,
                    start_character: None,
                    end_line: el,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                });
            }
            pos = end;
        } else {
            break;
        }
    }

    // Consecutive line comments that span ≥3 lines.
    let mut comment_start: Option<u32> = None;
    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with("##") || trimmed.starts_with('#');
        if is_comment {
            if comment_start.is_none() {
                comment_start = Some(line_idx as u32);
            }
        } else if let Some(cs) = comment_start.take() {
            let ce = line_idx as u32 - 1;
            if ce - cs >= 2 {
                ranges.push(FoldingRange {
                    start_line: cs,
                    start_character: None,
                    end_line: ce,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                });
            }
        }
    }

    ranges.sort_by_key(|r| (r.start_line, r.end_line));
    ranges
}

// ── Range overlap helper ──────────────────────────────────────────────────────

fn ranges_overlap(a: &Range, b: &Range) -> bool {
    a.start.line <= b.end.line && b.start.line <= a.end.line
}

// ── Signature help helpers ────────────────────────────────────────────────────

/// Extract the Nth parameter label from a hover detail string.
/// The detail looks like: `action foo with (x: integer, y: string) returns T`.
fn extract_param_label_from_detail(detail: &str, idx: usize) -> Option<String> {
    // Find `with (...)` section.
    let with_pos = detail.find("with (")?;
    let after = &detail[with_pos + 6..];
    let close = after.find(')')?;
    let params_str = &after[..close];
    let param = params_str.split(',').nth(idx)?;
    Some(param.trim().to_string())
}

/// Build a concise one-line signature label from the hover detail.
fn build_signature_label(fn_name: &str, detail: &str) -> String {
    // The detail is a markdown block: ```fidan\naction foo ...\n```
    // Extract the declaration line.
    for line in detail.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("action ")
            || trimmed.starts_with("action ")
            || trimmed.contains(fn_name)
        {
            // Strip markdown backtick wrapping.
            let clean: String = trimmed.chars().filter(|&c| c != '`').collect();
            if !clean.is_empty() {
                return clean;
            }
        }
    }
    fn_name.to_string()
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

/// Convert a byte offset to an LSP `Position` (0-based line, UTF-16 character).
fn offset_to_lsp_pos(file: &SourceFile, offset: u32) -> Position {
    let off = offset as usize;
    let line = match file.line_starts.binary_search(&(off as u32)) {
        Ok(l) => l,
        Err(l) => l.saturating_sub(1),
    };
    let line_start = file.line_starts[line] as usize;
    let col_bytes = off.saturating_sub(line_start);
    // Convert the byte column to UTF-16 code units.
    let line_text = file.src.get(line_start..).unwrap_or("");
    let mut utf16_col = 0u32;
    let mut remaining = col_bytes;
    for ch in line_text.chars() {
        if remaining == 0 {
            break;
        }
        let byte_len = ch.len_utf8();
        if remaining < byte_len {
            break;
        }
        remaining -= byte_len;
        utf16_col += ch.len_utf16() as u32;
    }
    Position {
        line: line as u32,
        character: utf16_col,
    }
}

// ── Named-argument go-to-definition ────────────────────────────────────────────────

/// Try to resolve a named call-argument identifier to the parameter's declaration span.
///
/// Returns `Some(span)` when:
///  * the text after the cursor (skipping whitespace) starts with `=` or `set ` —
///    meaning this identifier is the *name* of a named argument;
///  * we can locate the callee by scanning backward through `identifier_spans` to
///    find the first identifier whose `[end .. cur_span.start]` slice contains `(`;
///  * that callee (or an ancestor via the inheritance chain) has a parameter
///    with the same name.
fn find_named_arg_param(
    symbol_table: &crate::symbols::SymbolTable,
    identifier_spans: &[(Span, String)],
    hit_idx: usize,
    cur_span: &Span,
    text: &str,
) -> Option<NamedArgLookup> {
    // 1. Confirm named-argument context.
    let after = text.get(cur_span.end as usize..)?;
    let rest = after.trim_start_matches(|c: char| c == ' ' || c == '\t');
    if !rest.starts_with('=') && !rest.starts_with("set ") && !rest.starts_with("set\t") {
        return None;
    }
    let param_name = identifier_spans[hit_idx].1.clone();

    // 2. Scan backward for the callee identifier (the one followed by `(`).
    for i in (0..hit_idx).rev() {
        let (fn_span, fn_name) = &identifier_spans[i];
        let between = match text.get(fn_span.end as usize..cur_span.start as usize) {
            Some(s) => s,
            None => break,
        };
        if !between.contains('(') {
            // Past a statement boundary — stop searching.
            if between.contains(')') || between.contains(';') {
                break;
            }
            continue;
        }

        // 3a. Direct lookup — global action named `fn_name`.
        if let Some(entry) = symbol_table.get(fn_name) {
            if let Some((_, span)) = entry.param_names.iter().find(|(n, _)| *n == param_name) {
                return Some(NamedArgLookup::InDoc(*span));
            }
        }

        // 3b. Method lookup via the receiver variable at index i-1.
        if i > 0 {
            let (_, recv_name) = &identifier_spans[i - 1];
            // Resolve the concrete type of the receiver (or fall back to the name itself
            // for the case where the receiver IS the type, e.g. `TRex.new(...)`).
            let start_ty = symbol_table
                .get(recv_name)
                .and_then(|e| e.ty_name.as_deref())
                .unwrap_or(recv_name.as_str())
                .to_string();
            // Walk the inheritance chain.
            let mut cur_ty = start_ty;
            for _ in 0..8 {
                let key = format!("{}.{}", cur_ty, fn_name);
                if let Some(entry) = symbol_table.get(&key) {
                    if let Some((_, span)) =
                        entry.param_names.iter().find(|(n, _)| *n == param_name)
                    {
                        return Some(NamedArgLookup::InDoc(*span));
                    }
                    // Method found in local table but no matching param — stop.
                    break;
                }
                // This type is not in the local symbol table.  Walk up to its parent;
                // if there is no parent entry either, the type lives in an imported
                // document — escalate to a cross-module lookup.
                match symbol_table.get(&cur_ty).and_then(|e| e.ty_name.clone()) {
                    Some(p) => cur_ty = p,
                    None => {
                        return Some(NamedArgLookup::CrossModule {
                            recv_ty: cur_ty,
                            method_name: fn_name.clone(),
                            param_name,
                        });
                    }
                }
            }
        }
        break; // Only consider the nearest callee.
    }
    None
}
