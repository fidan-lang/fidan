//! tower-lsp `LanguageServer` implementation for Fidan.

use crate::{
    analysis, convert, document::Document, semantic, store::DocumentStore, symbols::SymKind,
    symbols::SymbolEntry,
};
use fidan_config::{BUILTIN_FUNCTIONS, builtin_info, decorator_info};
use fidan_fmt::{FormatOptions, format_source, load_format_options_for_path};
use fidan_source::{FileId, SourceFile, Span};
use fidan_stdlib::{
    STDLIB_MODULES, member_doc as stdlib_member_doc,
    member_return_type as stdlib_member_return_type, module_info as stdlib_module_info,
};
use std::collections::{HashMap, HashSet};
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
    "enum",
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

fn stdlib_members(mod_name: &str) -> &'static [&'static str] {
    stdlib_module_info(mod_name)
        .map(|info| (info.exports)())
        .unwrap_or(&[])
}

fn stdlib_module_hover_markdown(mod_name: &str) -> Option<String> {
    let info = stdlib_module_info(mod_name)?;
    Some(format!("```fidan\nuse std.{mod_name}\n```\n\n{}", info.doc))
}

fn stdlib_member_hover_markdown(mod_name: &str, member_name: &str) -> Option<String> {
    stdlib_member_doc(mod_name, member_name)
}

fn decorator_hover_markdown(name: &str) -> Option<String> {
    let info = decorator_info(name)?;
    let state = if info.reserved_only {
        "Reserved for future use."
    } else {
        "Built-in language decorator."
    };
    Some(format!(
        "```fidan\n@{}\n```\n\n{}\n\n{}",
        info.name, info.doc, state
    ))
}

fn builtin_hover_markdown(name: &str) -> Option<String> {
    let info = builtin_info(name)?;
    Some(format!("```fidan\n{}\n```\n\n{}", info.signature, info.doc))
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn decorator_name_at_offset(text: &str, offset: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return None;
    }

    let mut start = offset;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }

    let mut end = offset;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }

    if start == end || start == 0 || bytes[start - 1] != b'@' {
        return None;
    }

    text.get(start..end)
}

fn patch_var_inferred_type(
    doc: &mut Document,
    var_name: &str,
    ret_type: &str,
    preserve_member_resolution: bool,
) {
    if let Some(sym_entry) = doc.symbol_table.entries.get_mut(var_name) {
        let kw = if matches!(
            sym_entry.kind,
            crate::symbols::SymKind::Variable { is_const: true }
        ) {
            "const var"
        } else {
            "var"
        };
        sym_entry.detail = format!("```fidan\n{} {} -> {}\n```", kw, var_name, ret_type);
        if preserve_member_resolution {
            sym_entry.ty_name = Some(ret_type.to_string());
        }
    }

    if let Some((span, _)) = doc.identifier_spans.iter().find(|(_, n)| n == var_name) {
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

#[cfg(test)]
fn stdlib_module_doc(mod_name: &str) -> &'static str {
    stdlib_module_info(mod_name)
        .map(|info| info.doc)
        .unwrap_or("")
}

// ── Named-arg goto-def result ───────────────────────────────────────────────
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

        let stdlib_import_map: HashMap<String, String> =
            result.stdlib_imports.into_iter().collect();

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
                stdlib_imports: stdlib_import_map,
                inlay_hint_sites: result.inlay_hint_sites,
            },
        );
        // Proactively analyse imported files.  Background-loaded documents
        // (version == -1) are always re-read from disk so that edits to imported
        // files are reflected immediately without requiring the user to open them
        // in the editor.  Files that are actively open in the editor (version ≥ 0)
        // are managed through their own did-open / did-change notifications and
        // must NOT be overwritten with the on-disk version here.
        for import_url in import_urls.values() {
            let skip = self
                .store
                .get(import_url)
                .map(|d| d.version >= 0)
                .unwrap_or(false);
            if skip {
                continue; // actively open in editor — let did_change manage it
            }
            if let Ok(path) = import_url.to_file_path()
                && let Ok(file_text) = std::fs::read_to_string(&path)
            {
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
                        stdlib_imports: HashMap::new(),
                        inlay_hint_sites: vec![], // not shown for background docs
                    },
                );
            }
        }

        // Patch `var x: dynamic` entries whose init was a cross-module method call.
        // Now that background docs are loaded we can resolve the actual return type.
        for (var_name, recv_ty, method_name) in &result.dynamic_var_call_sites {
            if let Some((_, entry)) = self.resolve_member_cross_doc(recv_ty, method_name)
                && let Some(ref ret_type) = entry.return_type
                && let Some(mut doc) = self.store.get_mut(uri)
            {
                patch_var_inferred_type(&mut doc, var_name, ret_type, true);
            }
        }

        for (var_name, mod_name, member_name) in &result.stdlib_var_call_sites {
            if let Some(ret_type) = stdlib_member_return_type(mod_name, member_name)
                && ret_type != "dynamic"
                && let Some(mut doc) = self.store.get_mut(uri)
            {
                patch_var_inferred_type(&mut doc, var_name, ret_type, false);
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

    /// Build `TextEdit`s for every W1005 (unused import) diagnostic in `uri`.
    fn build_remove_unused_imports_edits(&self, uri: &Url) -> Vec<TextEdit> {
        let doc = match self.store.get(uri) {
            Some(d) => d,
            None => return vec![],
        };
        let text = doc.text.clone();
        let diags = doc.diagnostics.clone();
        drop(doc);

        build_remove_unused_imports_edits_for_text(uri.as_str(), &text, &diags)
    }
}

fn build_remove_unused_imports_edits_for_text(
    uri_str: &str,
    text: &str,
    diagnostics: &[Diagnostic],
) -> Vec<TextEdit> {
    #[derive(Default)]
    struct GroupedImportPlan {
        remove_unused: HashSet<String>,
    }

    let file = SourceFile::new(FileId(0), uri_str, text);
    let mut edits = Vec::new();
    let mut grouped_plans: HashMap<(u32, u32), GroupedImportPlan> = HashMap::new();
    let mut fallback_delete_ranges = Vec::new();

    for diag in diagnostics {
        if diag.code != Some(NumberOrString::String("W1005".to_string())) {
            continue;
        }

        let mut had_machine_edit = false;
        if let Some(fixes) = diag.data.as_ref().and_then(|value| value.as_array()) {
            for fix in fixes {
                let start = fix["start"].as_u64().unwrap_or(0) as u32;
                let end = fix["end"].as_u64().unwrap_or(0) as u32;
                let replacement = fix["replacement"].as_str().unwrap_or("").to_string();
                let range = convert::span_to_range(
                    &file,
                    Span {
                        file: FileId(0),
                        start,
                        end,
                    },
                );
                edits.push(TextEdit {
                    range,
                    new_text: replacement,
                });
                had_machine_edit = true;
            }
        }
        if had_machine_edit {
            continue;
        }

        let Some(name) = extract_backticked_name(&diag.message) else {
            fallback_delete_ranges.push(diag.range);
            continue;
        };
        let Some((start, end)) = range_to_offsets(text, &diag.range) else {
            fallback_delete_ranges.push(diag.range);
            continue;
        };
        grouped_plans
            .entry((start as u32, end as u32))
            .or_default()
            .remove_unused
            .insert(name.to_string());
    }

    for ((span_lo, span_hi), plan) in grouped_plans {
        let lo = span_lo as usize;
        let hi = span_hi as usize;
        let Some(stmt) = text.get(lo..hi) else {
            continue;
        };
        let Some(open) = stmt.find('{') else {
            fallback_delete_ranges.push(Range {
                start: convert::span_to_range(
                    &file,
                    Span {
                        file: FileId(0),
                        start: span_lo,
                        end: span_lo,
                    },
                )
                .start,
                end: convert::span_to_range(
                    &file,
                    Span {
                        file: FileId(0),
                        start: span_hi,
                        end: span_hi,
                    },
                )
                .start,
            });
            continue;
        };
        let Some(close) = stmt.rfind('}') else {
            continue;
        };
        if close <= open {
            continue;
        }

        let prefix = &stmt[..open];
        let suffix = &stmt[close + 1..];
        let inner = &stmt[open + 1..close];
        let members = parse_grouped_import_members(inner);
        if members.is_empty() {
            continue;
        }

        let remaining: Vec<&str> = members
            .into_iter()
            .filter(|member| !plan.remove_unused.contains(*member))
            .collect();

        if remaining.is_empty() {
            let (line_lo, line_hi) = expand_statement_to_trailing_newline(text, lo, hi);
            edits.push(TextEdit {
                range: convert::span_to_range(
                    &file,
                    Span {
                        file: FileId(0),
                        start: line_lo as u32,
                        end: line_hi as u32,
                    },
                ),
                new_text: String::new(),
            });
            continue;
        }

        edits.push(TextEdit {
            range: convert::span_to_range(
                &file,
                Span {
                    file: FileId(0),
                    start: span_lo,
                    end: span_hi,
                },
            ),
            new_text: format!("{}{{{}}}{}", prefix, remaining.join(", "), suffix),
        });
    }

    for range in fallback_delete_ranges {
        edits.push(TextEdit {
            range,
            new_text: String::new(),
        });
    }

    edits
}

fn extract_backticked_name(message: &str) -> Option<&str> {
    let start = message.find('`')?;
    let rest = &message[start + 1..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

fn parse_grouped_import_members(inner: &str) -> Vec<&str> {
    inner
        .split(',')
        .map(str::trim)
        .filter(|member| !member.is_empty())
        .collect()
}

fn expand_statement_to_trailing_newline(text: &str, lo: usize, hi: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut end = hi.min(bytes.len());
    if end < bytes.len() {
        if bytes[end] == b'\r' && end + 1 < bytes.len() && bytes[end + 1] == b'\n' {
            end += 2;
        } else if matches!(bytes[end], b'\n' | b'\r') {
            end += 1;
        }
    }
    (lo, end)
}

fn range_to_offsets(text: &str, range: &Range) -> Option<(usize, usize)> {
    fn position_to_offset(text: &str, position: Position) -> Option<usize> {
        let mut line = 0u32;
        let mut offset = 0usize;
        for segment in text.split_inclusive('\n') {
            if line == position.line {
                let line_text = segment.strip_suffix('\n').unwrap_or(segment);
                let mut chars = line_text.chars();
                let mut line_offset = 0usize;
                for _ in 0..position.character {
                    line_offset += chars.next()?.len_utf8();
                }
                return Some(offset + line_offset);
            }
            offset += segment.len();
            line += 1;
        }

        if line == position.line && position.character == 0 {
            return Some(text.len());
        }

        None
    }

    Some((
        position_to_offset(text, range.start)?,
        position_to_offset(text, range.end)?,
    ))
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
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                        ]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        " ".to_string(),
                        "\"".to_string(),
                        "/".to_string(),
                    ]),
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
        enum HoverLookup {
            Found(String),            // detail string, ready to return
            CrossDoc(String, String), // (type_name, member_name) to search across docs
            ImportDoc(Url, String),   // (import_file_url, symbol_name) — for `module.Type`
            NotFound,
        }

        let hover_lookup = {
            let doc = match self.store.get(uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let offset = lsp_pos_to_offset(&file, pos);
            if let Some(name) = decorator_name_at_offset(doc.text.as_str(), offset as usize)
                && let Some(detail) = decorator_hover_markdown(name)
            {
                return Ok(Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: detail,
                    }),
                    range: None,
                }));
            }
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
            // Direct in-doc lookups: lexical scopes first, then plain →
            // qualified → type-resolved.
            let in_doc = doc
                .symbol_table
                .lookup_visible(offset, cur_name.as_str())
                .or_else(|| {
                    let pn = prev_name?;
                    if let Some(e) = doc.symbol_table.get(&format!("{}.{}", pn, cur_name)) {
                        return Some(e);
                    }
                    if let Some(pe) = doc.symbol_table.lookup_visible(offset, pn)
                        && let Some(ty) = &pe.ty_name
                    {
                        return doc.symbol_table.get(&format!("{}.{}", ty, cur_name));
                    }
                    None
                });
            if let Some(e) = in_doc {
                HoverLookup::Found(e.detail.clone())
            } else if let Some(detail) = builtin_hover_markdown(cur_name.as_str()) {
                HoverLookup::Found(detail)
            } else if let Some(pn) = prev_name
                && let Some(mod_name) = doc.stdlib_imports.get(pn)
                && let Some(detail) =
                    stdlib_member_hover_markdown(mod_name.as_str(), cur_name.as_str())
            {
                HoverLookup::Found(detail)
            } else if prev_name == Some("std") {
                match stdlib_module_hover_markdown(cur_name.as_str()) {
                    Some(detail) => HoverLookup::Found(detail),
                    None => HoverLookup::NotFound,
                }
            } else if let Some(pn) = prev_name {
                // `module.Type` — prev is a namespace alias for an imported file.
                if let Some(import_url) = doc.imports.get(pn) {
                    HoverLookup::ImportDoc(import_url.clone(), cur_name.clone())
                } else {
                    // Type-resolved: prev is a variable with known type.
                    let ty = doc
                        .symbol_table
                        .lookup_visible(offset, pn)
                        .and_then(|e| e.ty_name.clone());
                    match ty {
                        Some(t) => HoverLookup::CrossDoc(t, cur_name.clone()),
                        None => HoverLookup::NotFound,
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
                HoverLookup::Found(format!(
                    "```fidan\nimport \"{}\" as {}\n```",
                    file_name, cur_name
                ))
            } else if let Some(mod_name) = doc.stdlib_imports.get(cur_name.as_str()) {
                match stdlib_module_hover_markdown(mod_name.as_str()) {
                    Some(detail) => HoverLookup::Found(detail),
                    None => HoverLookup::NotFound,
                }
            } else {
                HoverLookup::NotFound
            }
            // `doc` (DashMap Ref) is dropped here, releasing the shard lock.
        };

        // Phase 2: resolve or do cross-document parent-chain lookup.
        let detail = match hover_lookup {
            HoverLookup::Found(d) => d,
            HoverLookup::CrossDoc(ty, member) => {
                match self.resolve_member_cross_doc(&ty, &member) {
                    Some((_, e)) => e.detail,
                    None => return Ok(None),
                }
            }
            HoverLookup::ImportDoc(url, name) => {
                // Look up the symbol directly in the imported document.
                match self.store.get(&url) {
                    Some(d) => match d.symbol_table.get(&name) {
                        Some(e) => e.detail.clone(),
                        None => return Ok(None),
                    },
                    None => return Ok(None),
                }
            }
            HoverLookup::NotFound => return Ok(None),
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
        enum DefinitionLookup {
            Found(Span),                              // declaration span in the current document
            CrossDoc(String, String),                 // (type_name, member_name)
            CrossDocNamedArg(String, String, String), // (recv_ty, method_name, param_name)
            ImportDoc(Url, String), // (import_file_url, symbol_name) — for `module.Type`
            OpenFile(Url),          // open the imported file at line 0 (alias goto-def)
            NotFound,
        }

        let (definition_lookup, current_file) = {
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
            let in_doc = doc
                .symbol_table
                .lookup_visible(offset, cur_name.as_str())
                .or_else(|| {
                    let pn = prev_name?;
                    if let Some(e) = doc.symbol_table.get(&format!("{}.{}", pn, cur_name)) {
                        return Some(e);
                    }
                    if let Some(pe) = doc.symbol_table.lookup_visible(offset, pn)
                        && let Some(ty) = &pe.ty_name
                    {
                        return doc.symbol_table.get(&format!("{}.{}", ty, cur_name));
                    }
                    None
                });
            // Fallback: resolve named call-arguments (e.g. `times` in `foo(times = 10)`).
            let named_arg =
                find_named_arg_param(&doc.symbol_table, spans, hit_idx, cur_span, &doc.text);
            let named_to_lookup = |l: NamedArgLookup| -> DefinitionLookup {
                match l {
                    NamedArgLookup::InDoc(span) => DefinitionLookup::Found(span),
                    NamedArgLookup::CrossModule {
                        recv_ty,
                        method_name,
                        param_name,
                    } => DefinitionLookup::CrossDocNamedArg(recv_ty, method_name, param_name),
                }
            };
            let p1 = if let Some(e) = in_doc {
                DefinitionLookup::Found(e.span)
            } else if let Some(pn) = prev_name {
                // `module.Type` — prev is a namespace alias for an imported file.
                if let Some(import_url) = doc.imports.get(pn) {
                    DefinitionLookup::ImportDoc(import_url.clone(), cur_name.clone())
                } else {
                    let ty = doc
                        .symbol_table
                        .lookup_visible(offset, pn)
                        .and_then(|e| e.ty_name.clone());
                    match ty {
                        Some(t) => DefinitionLookup::CrossDoc(t, cur_name.clone()),
                        None => named_arg
                            .map(named_to_lookup)
                            .unwrap_or(DefinitionLookup::NotFound),
                    }
                }
            } else if let Some(import_url) = doc.imports.get(cur_name.as_str()) {
                // Cursor is on a module alias itself — open the imported file.
                DefinitionLookup::OpenFile(import_url.clone())
            } else {
                named_arg
                    .map(named_to_lookup)
                    .unwrap_or(DefinitionLookup::NotFound)
            };
            (p1, file) // `doc` dropped here
        };

        // Phase 2: resolve span + source URI (may require cross-doc lookup).
        let (def_uri, span) = match definition_lookup {
            DefinitionLookup::Found(span) => (uri.clone(), span),
            DefinitionLookup::CrossDoc(ty, member) => {
                match self.resolve_member_cross_doc(&ty, &member) {
                    Some((src_uri, e)) => (src_uri, e.span),
                    None => return Ok(None),
                }
            }
            DefinitionLookup::CrossDocNamedArg(recv_ty, method, param) => {
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
            DefinitionLookup::ImportDoc(url, name) => {
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
            DefinitionLookup::OpenFile(url) => (url, Span::default()),
            DefinitionLookup::NotFound => return Ok(None),
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

        // ── Phase 1: all intra-document work while holding the DashMap lock ──
        //
        // We collect everything we need into owned values so that the
        // DashMap `Ref` (`doc`) is dropped before any cross-document call.

        enum DotResolution {
            /// Receiver is a variable/object — use `collect_type_members`.
            TypeName(String),
            /// Receiver is a file-module alias import — show its top-level exports.
            ModuleAlias(Url),
            /// Receiver is a stdlib module alias — show its exported member names.
            StdLibModule(String),
        }

        struct CompletionSeed {
            dot_res: Option<DotResolution>,
            /// Declared symbols (non-dot completion path).
            local_items: Vec<CompletionItem>,
            /// Named parameter entries found locally for the enclosing call.
            named_param_entries: Vec<(String, Span)>,
            /// When named params live in an imported doc: (recv_ty, method_name).
            named_param_cross: Option<(String, String)>,
            /// Import context: if the cursor is inside a `use` statement,
            /// contains either `("file", partial_path)` or `("std", partial_mod)`.
            import_ctx: Option<ImportContext>,
        }

        /// What kind of import the cursor is inside.
        enum ImportContext {
            /// Inside `use "partial/path"` — partial filesystem path typed so far.
            FilePath(String),
            /// After `use std.` — partial stdlib module name typed so far.
            StdLib(String),
            /// After `use ` (bare identifier) — partial user-module name.
            BareIdent(String),
            /// Inside `use std.<module>.{partial` — show members of that module.
            StdLibMember(String, String), // (module_name, partial)
        }

        let completion_seed = {
            let doc = match self.store.get(uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let cursor = lsp_pos_to_offset(&file, pos) as usize;
            let src = doc.text.as_bytes();

            // ── Import context detection ──────────────────────────────────────
            // Check if the cursor sits inside a `use` statement so we can offer
            // file-path or stdlib-module completion instead of general symbols.
            let import_ctx: Option<ImportContext> = {
                // Extract the line up to the cursor.
                let line_start = src[..cursor]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map(|p| p + 1)
                    .unwrap_or(0);
                let line_up_to_cursor = std::str::from_utf8(&src[line_start..cursor])
                    .unwrap_or("")
                    .trim_start();

                if let Some(rest) = line_up_to_cursor.strip_prefix("use") {
                    let rest = rest.trim_start_matches(' ');
                    if let Some(inside) = rest.strip_prefix('"') {
                        // File-path import: `use "partial/path`
                        Some(ImportContext::FilePath(inside.to_string()))
                    } else if let Some(after_std) = rest.strip_prefix("std.") {
                        // Check for grouped/destructured import: `use std.io.{partial`
                        if let Some(dot_brace) = after_std.find(".{") {
                            let mod_name = after_std[..dot_brace].to_string();
                            let after_brace = &after_std[dot_brace + 2..];
                            // partial = text after the last comma (handles `use std.io.{a, b`)
                            let partial = after_brace
                                .rsplit(',')
                                .next()
                                .unwrap_or(after_brace)
                                .trim_start()
                                .to_string();
                            Some(ImportContext::StdLibMember(mod_name, partial))
                        } else {
                            // Plain stdlib module completion: `use std.partial`
                            Some(ImportContext::StdLib(after_std.to_string()))
                        }
                    } else if !rest.is_empty()
                        && rest
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '/')
                    {
                        // Bare user-module identifier: `use mymod`
                        Some(ImportContext::BareIdent(rest.to_string()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            // If we're in an import context, skip all other completion logic
            // and return early from Phase 1.
            if import_ctx.is_some() {
                CompletionSeed {
                    dot_res: None,
                    local_items: vec![],
                    named_param_entries: vec![],
                    named_param_cross: None,
                    import_ctx,
                }
            } else {
                // ── Dot-triggered receiver resolution ────────────────────────────
                let dot_res: Option<DotResolution> = if cursor > 0
                    && (trigger == Some(".") || src.get(cursor.saturating_sub(1)) == Some(&b'.'))
                {
                    let dot_pos = (cursor as u32).saturating_sub(1);
                    let recv_chain =
                        dotted_receiver_segments(&doc.identifier_spans, &doc.text, dot_pos);

                    if let Some(first) = recv_chain.first() {
                        if recv_chain.len() == 1 {
                            if let Some(url) = doc.imports.get(first.as_str()) {
                                Some(DotResolution::ModuleAlias(url.clone()))
                            } else if let Some(mod_name) = doc.stdlib_imports.get(first.as_str()) {
                                Some(DotResolution::StdLibModule(mod_name.clone()))
                            } else {
                                resolve_dotted_receiver_type_name(
                                    &doc.symbol_table,
                                    &doc.identifier_spans,
                                    &doc.text,
                                    dot_pos,
                                )
                                .map(DotResolution::TypeName)
                            }
                        } else {
                            resolve_dotted_receiver_type_name(
                                &doc.symbol_table,
                                &doc.identifier_spans,
                                &doc.text,
                                dot_pos,
                            )
                            .map(DotResolution::TypeName)
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // If dot-triggered and resolved, skip standard items entirely.
                if dot_res.is_some() {
                    CompletionSeed {
                        dot_res,
                        local_items: vec![],
                        named_param_entries: vec![],
                        named_param_cross: None,
                        import_ctx: None,
                    }
                } else {
                    // ── Standard (non-dot) symbol items ──────────────────────────────
                    let local_items =
                        visible_symbol_completion_items(&doc.symbol_table, cursor as u32);

                    // ── Named-parameter detection ─────────────────────────────────────
                    // Walk backward to find if the cursor is inside a function call and
                    // collect parameter names for `paramName = ` suggestions.
                    let mut named_param_entries: Vec<(String, Span)> = vec![];
                    let mut named_param_cross: Option<(String, String)> = None;

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

                    if let Some(open) = open_paren
                        && let Some((fn_span, fn_name)) = doc
                            .identifier_spans
                            .iter()
                            .rev()
                            .find(|(span, _)| span.end as usize <= open)
                    {
                        // Try direct lookup first, then dot-receiver-qualified.
                        let entry_opt = doc
                            .symbol_table
                            .lookup_visible(fn_span.start, fn_name.as_str())
                            .or_else(|| {
                                let fn_start = fn_span.start as usize;
                                if fn_start > 0
                                    && src.get(fn_start.saturating_sub(1)) == Some(&b'.')
                                {
                                    let recv = doc
                                        .identifier_spans
                                        .iter()
                                        .rev()
                                        .find(|(span, _)| (span.end as usize) < fn_start)?;
                                    let ty = doc
                                        .symbol_table
                                        .lookup_visible(fn_span.start, recv.1.as_str())
                                        .and_then(|e| e.ty_name.as_deref())?;
                                    doc.symbol_table.get(&format!("{}.{}", ty, fn_name))
                                } else {
                                    None
                                }
                            });

                        if let Some(entry) = entry_opt {
                            named_param_entries = entry.param_names.clone();
                        } else {
                            // Record for cross-doc resolution in Phase 2.
                            let fn_start = fn_span.start as usize;
                            if fn_start > 0
                                && src.get(fn_start.saturating_sub(1)) == Some(&b'.')
                                && let Some((_, recv_name)) = doc
                                    .identifier_spans
                                    .iter()
                                    .rev()
                                    .find(|(span, _)| (span.end as usize) < fn_start)
                                && let Some(ty) = doc
                                    .symbol_table
                                    .lookup_visible(fn_span.start, recv_name.as_str())
                                    .and_then(|e| e.ty_name.as_deref())
                                    .map(|s| s.to_string())
                            {
                                named_param_cross = Some((ty, fn_name.clone()));
                            }
                        }
                    }

                    CompletionSeed {
                        dot_res,
                        local_items,
                        named_param_entries,
                        named_param_cross,
                        import_ctx: None,
                    }
                } // end else (standard path)
            } // end else (not import context)
            // `doc` (DashMap Ref) is dropped here.
        };

        // ── Phase 2: cross-document resolution + assemble response ────────────

        // ── Import context: file-path or stdlib completion ────────────────────
        if let Some(import_ctx) = completion_seed.import_ctx {
            let items: Vec<CompletionItem> = match import_ctx {
                ImportContext::StdLib(partial) => {
                    // Suggest matching `std.*` modules.
                    STDLIB_MODULES
                        .iter()
                        .filter(|info| info.name.starts_with(partial.as_str()))
                        .map(|info| CompletionItem {
                            label: format!("std.{}", info.name),
                            kind: Some(CompletionItemKind::MODULE),
                            insert_text: Some(format!("std.{}", info.name)),
                            documentation: Some(Documentation::MarkupContent(MarkupContent {
                                kind: MarkupKind::PlainText,
                                value: info.doc.to_string(),
                            })),
                            sort_text: Some(format!("0std.{}", info.name)),
                            ..Default::default()
                        })
                        .collect()
                }
                ImportContext::FilePath(partial) => {
                    // Suggest .fdn files and directories relative to the current file.
                    if let Ok(file_path) = uri.to_file_path() {
                        let base_dir = file_path.parent().unwrap_or(&file_path).to_path_buf();
                        // Split partial into (directory_prefix, file_prefix).
                        let (search_dir, file_prefix) =
                            if partial.contains('/') || partial.contains('\\') {
                                let sep_pos = partial.rfind(['/', '\\']).unwrap();
                                let dir_part = &partial[..sep_pos];
                                let name_part = &partial[sep_pos + 1..];
                                (base_dir.join(dir_part), name_part.to_string())
                            } else {
                                (base_dir.clone(), partial.clone())
                            };
                        // Pre-compute the directory prefix string so it can be moved into the closure.
                        let prefix_len = partial.len() - file_prefix.len();
                        let dir_prefix = partial[..prefix_len].to_string();
                        // Enumerate directory entries on a blocking thread — never call
                        // std::fs::read_dir directly on a tokio async executor thread.
                        tokio::task::spawn_blocking(move || {
                            let mut file_items: Vec<CompletionItem> = vec![];
                            if let Ok(entries) = std::fs::read_dir(&search_dir) {
                                for entry in entries.flatten() {
                                    let name = entry.file_name();
                                    let name_str = name.to_string_lossy();
                                    if !name_str.starts_with(file_prefix.as_str()) {
                                        continue;
                                    }
                                    let path = entry.path();
                                    let is_dir = path.is_dir();
                                    let is_fdn =
                                        path.extension().and_then(|e| e.to_str()) == Some("fdn");
                                    if is_dir {
                                        let dir_label = format!("{}/", name_str);
                                        let insert = format!("{}{}/", dir_prefix, name_str);
                                        file_items.push(CompletionItem {
                                            label: dir_label,
                                            kind: Some(CompletionItemKind::FOLDER),
                                            insert_text: Some(insert),
                                            ..Default::default()
                                        });
                                    } else if is_fdn {
                                        let stem = path
                                            .file_stem()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .to_string();
                                        let insert = format!("{}{}.fdn\"", dir_prefix, stem);
                                        file_items.push(CompletionItem {
                                            label: name_str.to_string(),
                                            kind: Some(CompletionItemKind::FILE),
                                            insert_text: Some(insert),
                                            ..Default::default()
                                        });
                                    }
                                }
                            }
                            file_items
                        })
                        .await
                        .unwrap_or_default()
                    } else {
                        vec![]
                    }
                }
                ImportContext::BareIdent(partial) => {
                    // Offer stdlib modules matching the bare identifier as well as
                    // any .fdn files in the current directory.
                    let mut items: Vec<CompletionItem> = vec![];

                    // Stdlib modules whose first segment starts with the partial.
                    for info in STDLIB_MODULES {
                        let full = format!("std.{}", info.name);
                        if full.starts_with(partial.as_str()) || "std".starts_with(partial.as_str())
                        {
                            items.push(CompletionItem {
                                label: full.clone(),
                                kind: Some(CompletionItemKind::MODULE),
                                insert_text: Some(full.clone()),
                                documentation: Some(Documentation::MarkupContent(MarkupContent {
                                    kind: MarkupKind::PlainText,
                                    value: info.doc.to_string(),
                                })),
                                sort_text: Some(format!("0{}", full)),
                                ..Default::default()
                            });
                        }
                    }

                    // .fdn files in the current directory (enumerated on a blocking thread).
                    if let Ok(file_path) = uri.to_file_path()
                        && let Some(base_dir) = file_path.parent().map(|p| p.to_path_buf())
                    {
                        // `partial` is already owned — move it directly into the closure,
                        // no clone needed.
                        let fdn_items = tokio::task::spawn_blocking(move || {
                            let mut fdn_items: Vec<CompletionItem> = vec![];
                            if let Ok(entries) = std::fs::read_dir(&base_dir) {
                                for entry in entries.flatten() {
                                    let path = entry.path();
                                    if path.extension().and_then(|e| e.to_str()) != Some("fdn") {
                                        continue;
                                    }
                                    // Skip the current file.
                                    if path == file_path {
                                        continue;
                                    }
                                    let stem = path
                                        .file_stem()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string();
                                    if stem.starts_with(partial.as_str()) {
                                        fdn_items.push(CompletionItem {
                                            label: stem.clone(),
                                            kind: Some(CompletionItemKind::MODULE),
                                            insert_text: Some(stem),
                                            ..Default::default()
                                        });
                                    }
                                }
                            }
                            fdn_items
                        })
                        .await
                        .unwrap_or_default();
                        items.extend(fdn_items);
                    }

                    items
                }
                ImportContext::StdLibMember(mod_name, partial) => {
                    // Suggest members of `std.<mod_name>` that start with `partial`.
                    stdlib_members(&mod_name)
                        .iter()
                        .filter(|name| name.starts_with(partial.as_str()))
                        .map(|name| CompletionItem {
                            label: name.to_string(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            documentation: stdlib_member_hover_markdown(&mod_name, name).map(
                                |value| {
                                    Documentation::MarkupContent(MarkupContent {
                                        kind: MarkupKind::Markdown,
                                        value,
                                    })
                                },
                            ),
                            ..Default::default()
                        })
                        .collect()
                }
            };
            return Ok(Some(CompletionResponse::Array(items)));
        }

        // Dot-triggered: collect members (walking full cross-module chain).
        if let Some(dot_res) = completion_seed.dot_res {
            match dot_res {
                DotResolution::TypeName(ty) => {
                    let members = self.store.collect_type_members(&ty);
                    let items: Vec<CompletionItem> = members
                        .into_iter()
                        .filter(|(name, _)| name != "new")
                        .map(|(member, entry)| {
                            let kind = Some(match &entry.kind {
                                SymKind::Method => CompletionItemKind::METHOD,
                                SymKind::Field => CompletionItemKind::FIELD,
                                SymKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
                                _ => CompletionItemKind::FIELD,
                            });
                            let insert_text =
                                if matches!(entry.kind, SymKind::Method | SymKind::EnumVariant)
                                    && !entry.param_types.is_empty()
                                {
                                    Some(format!("{}($0)", member))
                                } else {
                                    None
                                };
                            CompletionItem {
                                label: member,
                                kind,
                                insert_text_format: insert_text
                                    .as_ref()
                                    .map(|_| InsertTextFormat::SNIPPET),
                                insert_text,
                                documentation: Some(Documentation::MarkupContent(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: entry.detail,
                                })),
                                ..Default::default()
                            }
                        })
                        .collect();
                    return Ok(Some(CompletionResponse::Array(items)));
                }
                DotResolution::ModuleAlias(url) => {
                    let syms = self.store.get_doc_top_level(&url);
                    let items: Vec<CompletionItem> = syms
                        .into_iter()
                        .map(|(name, entry)| {
                            let kind = Some(match &entry.kind {
                                SymKind::Action | SymKind::Method => CompletionItemKind::FUNCTION,
                                SymKind::Object => CompletionItemKind::CLASS,
                                SymKind::Enum => CompletionItemKind::ENUM,
                                SymKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
                                SymKind::Variable { .. } => CompletionItemKind::VARIABLE,
                                SymKind::Field => CompletionItemKind::FIELD,
                            });
                            CompletionItem {
                                label: name,
                                kind,
                                documentation: Some(Documentation::MarkupContent(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: entry.detail,
                                })),
                                ..Default::default()
                            }
                        })
                        .collect();
                    return Ok(Some(CompletionResponse::Array(items)));
                }
                DotResolution::StdLibModule(mod_name) => {
                    let items: Vec<CompletionItem> = stdlib_members(&mod_name)
                        .iter()
                        .map(|name| CompletionItem {
                            label: name.to_string(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            documentation: stdlib_member_hover_markdown(&mod_name, name).map(
                                |value| {
                                    Documentation::MarkupContent(MarkupContent {
                                        kind: MarkupKind::Markdown,
                                        value,
                                    })
                                },
                            ),
                            ..Default::default()
                        })
                        .collect();
                    return Ok(Some(CompletionResponse::Array(items)));
                }
            }
        }

        // Named-param cross-doc resolution.
        let mut named_param_items: Vec<CompletionItem> = completion_seed
            .named_param_entries
            .iter()
            .map(|(name, _)| CompletionItem {
                label: format!("{} = ", name),
                kind: Some(CompletionItemKind::KEYWORD),
                insert_text: Some(format!("{} = ", name)),
                sort_text: Some(format!("0{}", name)),
                ..Default::default()
            })
            .collect();

        if named_param_items.is_empty()
            && let Some((recv_ty, method_name)) = completion_seed.named_param_cross
            && let Some((_, entry)) = self.resolve_member_cross_doc(&recv_ty, &method_name)
        {
            named_param_items = entry
                .param_names
                .iter()
                .map(|(name, _)| CompletionItem {
                    label: format!("{} = ", name),
                    kind: Some(CompletionItemKind::KEYWORD),
                    insert_text: Some(format!("{} = ", name)),
                    sort_text: Some(format!("0{}", name)),
                    ..Default::default()
                })
                .collect();
        }

        // Assemble final list: named params first (sort_text "0…" keeps them
        // at the top), then declared symbols, then keywords, then builtins.
        let mut items = named_param_items;
        items.extend(completion_seed.local_items);

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
                documentation: builtin_hover_markdown(builtin).map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
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

        // Phase 1: gather everything from the document while holding the lock.
        enum SignatureLookup {
            /// Entry resolved locally — ready to build the response.
            Found {
                fn_name: String,
                param_types: Vec<String>,
                detail: String,
                active_param: u32,
            },
            /// Entry not found locally; try cross-doc resolution.
            CrossDoc {
                recv_ty: String,
                method_name: String,
                active_param: u32,
            },
            NotFound,
        }

        let signature_lookup = {
            let doc = match self.store.get(uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let cursor = lsp_pos_to_offset(&file, pos) as usize;
            let src = doc.text.as_bytes();

            // Walk backward from cursor to locate the opening `(` of the call.
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
                None => {
                    // doc dropped
                    return Ok(None);
                }
            };

            // Find function name: identifier ending just before `(`.
            let (fn_span, fn_name) = match doc
                .identifier_spans
                .iter()
                .rev()
                .find(|(span, _)| span.end as usize <= open)
            {
                Some(x) => x,
                None => return Ok(None),
            };
            let fn_name = fn_name.clone();
            let fn_start = fn_span.start as usize;

            // Count active parameter (comma depth at 0 from `(` to cursor).
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

            // Try local lookup: direct, then receiver-qualified ("TRex.roar").
            let local_entry = doc
                .symbol_table
                .lookup_visible(fn_span.start, fn_name.as_str())
                .cloned()
                .or_else(|| {
                    if fn_start > 0 && src.get(fn_start.saturating_sub(1)) == Some(&b'.') {
                        let recv = doc
                            .identifier_spans
                            .iter()
                            .rev()
                            .find(|(span, _)| (span.end as usize) < fn_start)?;
                        let ty = doc
                            .symbol_table
                            .lookup_visible(fn_span.start, recv.1.as_str())
                            .and_then(|e| e.ty_name.as_deref())?
                            .to_string();
                        doc.symbol_table
                            .get(&format!("{}.{}", ty, fn_name))
                            .cloned()
                    } else {
                        None
                    }
                });

            if let Some(entry) = local_entry {
                if entry.param_types.is_empty() {
                    SignatureLookup::NotFound
                } else {
                    SignatureLookup::Found {
                        fn_name,
                        param_types: entry.param_types.clone(),
                        detail: entry.detail.clone(),
                        active_param,
                    }
                }
            } else if fn_start > 0 && src.get(fn_start.saturating_sub(1)) == Some(&b'.') {
                // Cross-doc: identify receiver type.
                let recv_ty = doc
                    .identifier_spans
                    .iter()
                    .rev()
                    .find(|(span, _)| (span.end as usize) < fn_start)
                    .and_then(|(_, rn)| {
                        doc.symbol_table
                            .get(rn.as_str())
                            .and_then(|e| e.ty_name.clone())
                    });
                match recv_ty {
                    Some(ty) => SignatureLookup::CrossDoc {
                        recv_ty: ty,
                        method_name: fn_name,
                        active_param,
                    },
                    None => SignatureLookup::NotFound,
                }
            } else {
                SignatureLookup::NotFound
            }
            // doc dropped here
        };

        // Phase 2: finalise response (cross-doc lookup if needed).
        let (fn_name, param_types, detail, active_param) = match signature_lookup {
            SignatureLookup::Found {
                fn_name,
                param_types,
                detail,
                active_param,
            } => (fn_name, param_types, detail, active_param),
            SignatureLookup::CrossDoc {
                recv_ty,
                method_name,
                active_param,
            } => match self.resolve_member_cross_doc(&recv_ty, &method_name) {
                Some((_, entry)) if !entry.param_types.is_empty() => (
                    method_name,
                    entry.param_types.clone(),
                    entry.detail.clone(),
                    active_param,
                ),
                _ => return Ok(None),
            },
            SignatureLookup::NotFound => return Ok(None),
        };

        // Build parameter labels from the detail string or param_types.
        let sig_params: Vec<ParameterInformation> = param_types
            .iter()
            .enumerate()
            .map(|(idx, ty)| {
                let label = extract_param_label_from_detail(&detail, idx)
                    .unwrap_or_else(|| format!("param{} -> {}", idx + 1, ty));
                ParameterInformation {
                    label: ParameterLabel::Simple(label),
                    documentation: None,
                }
            })
            .collect();

        let sig_label = build_signature_label(&fn_name, &detail);
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: sig_label,
                documentation: None,
                parameters: Some(sig_params),
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

        // Objects and enums first — build them with their member / variant children.
        let mut type_names: Vec<String> = doc
            .symbol_table
            .all()
            .filter(|(name, entry)| {
                !name.contains('.') && matches!(entry.kind, SymKind::Object | SymKind::Enum)
            })
            .map(|(name, _)| name.clone())
            .collect();
        type_names.sort();

        for type_name in &type_names {
            let entry = match doc.symbol_table.get(type_name) {
                Some(e) => e,
                None => continue,
            };
            let prefix = format!("{}.", type_name);
            let mut children: Vec<DocumentSymbol> = doc
                .symbol_table
                .all()
                .filter(|(name, _)| name.starts_with(&prefix))
                .map(|(name, child)| {
                    let member = &name[prefix.len()..];
                    let kind = match &child.kind {
                        SymKind::Method => SymbolKind::METHOD,
                        SymKind::Field => SymbolKind::FIELD,
                        SymKind::EnumVariant => SymbolKind::ENUM_MEMBER,
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
            let kind = match &entry.kind {
                SymKind::Enum => SymbolKind::ENUM,
                _ => SymbolKind::CLASS,
            };

            #[allow(deprecated)]
            symbols.push(DocumentSymbol {
                name: type_name.clone(),
                detail: None,
                kind,
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

        // ── source.organizeImports: delete all unused-import spans ──────────────
        let only = params.context.only.as_deref().unwrap_or(&[]);
        let wants_organize = only.is_empty()
            || only
                .iter()
                .any(|k| k == &CodeActionKind::SOURCE_ORGANIZE_IMPORTS);
        if wants_organize {
            let all_edits = self.build_remove_unused_imports_edits(uri);
            if !all_edits.is_empty() {
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), all_edits);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Remove unused imports".to_string(),
                    kind: Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS),
                    diagnostics: None,
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

        let opts = match uri.to_file_path() {
            Ok(path) => match load_format_options_for_path(Some(&path)) {
                Ok(Some(opts)) => opts,
                Ok(None) => FormatOptions {
                    indent_width: params.options.tab_size as usize,
                    ..Default::default()
                },
                Err(err) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("ignored .fidanfmt for {}: {err}", path.display()),
                        )
                        .await;
                    FormatOptions {
                        indent_width: params.options.tab_size as usize,
                        ..Default::default()
                    }
                }
            },
            Err(_) => FormatOptions {
                indent_width: params.options.tab_size as usize,
                ..Default::default()
            },
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
    let rest = after.trim_start_matches([' ', '\t']);
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
        if let Some(entry) = symbol_table.lookup_visible(cur_span.start, fn_name)
            && let Some((_, span)) = entry.param_names.iter().find(|(n, _)| *n == param_name)
        {
            return Some(NamedArgLookup::InDoc(*span));
        }

        // 3b. Method lookup via the receiver variable at index i-1.
        if i > 0 {
            let (_, recv_name) = &identifier_spans[i - 1];
            // Resolve the concrete type of the receiver (or fall back to the name itself
            // for the case where the receiver IS the type, e.g. `TRex.new(...)`).
            let start_ty = symbol_table
                .lookup_visible(cur_span.start, recv_name)
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

fn dotted_receiver_segments(
    identifier_spans: &[(Span, String)],
    text: &str,
    dot_pos: u32,
) -> Vec<String> {
    let mut segments = Vec::new();
    let Some(mut idx) = identifier_spans.iter().rposition(|(span, _)| {
        span.end <= dot_pos && text.get(span.end as usize..dot_pos as usize) == Some("")
    }) else {
        return segments;
    };

    segments.push(identifier_spans[idx].1.clone());
    let mut current_start = identifier_spans[idx].0.start;

    while idx > 0 {
        let prev = &identifier_spans[idx - 1];
        if text.get(prev.0.end as usize..current_start as usize) == Some(".") {
            segments.push(prev.1.clone());
            current_start = prev.0.start;
            idx -= 1;
        } else {
            break;
        }
    }

    segments.reverse();
    segments
}

fn resolve_dotted_receiver_type_name(
    symbol_table: &crate::symbols::SymbolTable,
    identifier_spans: &[(Span, String)],
    text: &str,
    dot_pos: u32,
) -> Option<String> {
    let segments = dotted_receiver_segments(identifier_spans, text, dot_pos);
    let first = segments.first()?;
    let mut current_type = {
        let entry = symbol_table.lookup_visible(dot_pos, first.as_str())?;
        match &entry.kind {
            SymKind::Object | SymKind::Enum => first.clone(),
            _ => entry.ty_name.clone()?,
        }
    };

    for segment in segments.iter().skip(1) {
        let entry = symbol_table.get(&format!("{}.{}", current_type, segment))?;
        current_type = match &entry.kind {
            SymKind::Object | SymKind::Enum => segment.clone(),
            SymKind::EnumVariant => entry
                .return_type
                .clone()
                .or_else(|| entry.ty_name.clone())?,
            _ => entry.ty_name.clone()?,
        };
    }

    Some(current_type)
}

fn completion_item_for_symbol(name: &str, entry: &SymbolEntry, sort_group: &str) -> CompletionItem {
    let kind = Some(match &entry.kind {
        SymKind::Action | SymKind::Method => CompletionItemKind::FUNCTION,
        SymKind::Object => CompletionItemKind::CLASS,
        SymKind::Enum => CompletionItemKind::ENUM,
        SymKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
        SymKind::Variable { .. } => CompletionItemKind::VARIABLE,
        SymKind::Field => CompletionItemKind::FIELD,
    });
    let insert_text = if matches!(
        entry.kind,
        SymKind::Action | SymKind::Object | SymKind::EnumVariant
    ) && !entry.param_types.is_empty()
    {
        Some(format!("{}($0)", name))
    } else {
        None
    };
    CompletionItem {
        label: name.to_string(),
        kind,
        insert_text_format: insert_text.as_ref().map(|_| InsertTextFormat::SNIPPET),
        insert_text,
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: entry.detail.clone(),
        })),
        sort_text: Some(format!("{}{}", sort_group, name)),
        ..Default::default()
    }
}

fn visible_symbol_completion_items(
    symbol_table: &crate::symbols::SymbolTable,
    cursor: u32,
) -> Vec<CompletionItem> {
    symbol_table
        .visible_unqualified_at(cursor)
        .into_iter()
        .map(|(name, entry)| {
            let is_scoped = symbol_table.is_lexical_visible(cursor, &name);
            let sort_group = if is_scoped { "1" } else { "2" };
            completion_item_for_symbol(&name, &entry, sort_group)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdlib_completion_surface_tracks_runtime_modules() {
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "async"));
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "collections"));
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "parallel"));
        assert!(!STDLIB_MODULES.iter().any(|info| info.name == "net"));
        assert!(!STDLIB_MODULES.iter().any(|info| info.name == "json"));
    }

    #[test]
    fn stdlib_completion_members_include_recent_exports() {
        assert!(stdlib_members("async").contains(&"gather"));
        assert!(stdlib_members("async").contains(&"waitAny"));
        assert!(stdlib_members("collections").contains(&"enumerate"));
        assert!(stdlib_members("collections").contains(&"chunk"));
        assert!(stdlib_members("collections").contains(&"window"));
        assert!(stdlib_members("collections").contains(&"partition"));
        assert!(stdlib_members("collections").contains(&"groupBy"));
        assert!(stdlib_members("regex").contains(&"match"));
    }

    #[test]
    fn completion_keywords_cover_recent_language_features() {
        assert!(COMPLETION_KEYWORDS.contains(&"spawn"));
        assert!(COMPLETION_KEYWORDS.contains(&"await"));
        assert!(COMPLETION_KEYWORDS.contains(&"concurrent"));
        assert!(COMPLETION_KEYWORDS.contains(&"parallel"));
        assert!(COMPLETION_KEYWORDS.contains(&"enum"));
    }

    #[test]
    fn stdlib_module_docs_cover_current_modules() {
        for info in STDLIB_MODULES {
            assert!(
                !stdlib_module_doc(info.name).is_empty(),
                "missing completion documentation for std.{}",
                info.name
            );
        }
    }

    #[test]
    fn decorator_hover_docs_cover_builtins_and_reserved_spellings() {
        let precompile =
            decorator_hover_markdown("precompile").expect("missing @precompile hover doc");
        assert!(precompile.contains("@precompile"));

        let gpu = decorator_hover_markdown("gpu").expect("missing @gpu hover doc");
        assert!(gpu.contains("Reserved for future use"));
    }

    #[test]
    fn decorator_name_lookup_requires_at_prefix() {
        let text = "@precompile\naction main {}";
        assert_eq!(decorator_name_at_offset(text, 5), Some("precompile"));
        assert_eq!(decorator_name_at_offset(text, 0), None);
        assert_eq!(decorator_name_at_offset(text, 16), None);
    }

    #[test]
    fn completion_prefers_visible_local_symbols() {
        let text = r#"var global_total = 1

action outer {
    var local_total = 2
    action helper with (certain n oftype integer) returns integer {
        return n + local_total
    }
    print(local_total)
}
"#;
        let cursor = text.find("print(local_total)").expect("cursor marker") as u32;
        let analysis = analysis::analyze(text, "file:///completion_locals.fdn");
        let items = visible_symbol_completion_items(&analysis.symbol_table, cursor);
        let labels: Vec<String> = items.iter().map(|item| item.label.clone()).collect();

        assert!(labels.contains(&"local_total".to_string()));
        assert!(labels.contains(&"helper".to_string()));
        assert!(labels.contains(&"global_total".to_string()));
        let local_index = labels
            .iter()
            .position(|label| label == "local_total")
            .unwrap();
        let global_index = labels
            .iter()
            .position(|label| label == "global_total")
            .unwrap();
        assert!(local_index < global_index);
    }

    #[test]
    fn completion_hides_locals_before_their_declaration() {
        let text = r#"action outer {
    print("before")
    var local_total = 2
}
"#;
        let cursor = text.find("print").expect("cursor marker") as u32;
        let analysis = analysis::analyze(text, "file:///completion_before_decl.fdn");
        let items = visible_symbol_completion_items(&analysis.symbol_table, cursor);
        let labels: Vec<String> = items.iter().map(|item| item.label.clone()).collect();

        assert!(!labels.contains(&"local_total".to_string()));
    }

    #[test]
    fn completion_does_not_leak_if_branch_locals() {
        let text = r#"action outer with (certain flag oftype boolean) {
    if flag {
        var then_only = 1
        print(then_only)
    } otherwise {
        var else_only = 2
        print(else_only)
    }
}
"#;
        let then_cursor = text.find("print(then_only)").expect("then cursor") as u32;
        let else_cursor = text.find("print(else_only)").expect("else cursor") as u32;
        let analysis = analysis::analyze(text, "file:///completion_if_branches.fdn");

        let then_labels: Vec<String> =
            visible_symbol_completion_items(&analysis.symbol_table, then_cursor)
                .into_iter()
                .map(|item| item.label)
                .collect();
        assert!(then_labels.contains(&"then_only".to_string()));
        assert!(!then_labels.contains(&"else_only".to_string()));

        let else_labels: Vec<String> =
            visible_symbol_completion_items(&analysis.symbol_table, else_cursor)
                .into_iter()
                .map(|item| item.label)
                .collect();
        assert!(else_labels.contains(&"else_only".to_string()));
        assert!(!else_labels.contains(&"then_only".to_string()));
    }

    #[test]
    fn completion_includes_object_and_enum_types() {
        let text = r#"enum Direction {
    North
}

object Worker {
    action run returns dynamic {
        return nothing
    }
}

action main {
    print("hi")
}
"#;
        let cursor = text.find("print").expect("cursor marker") as u32;
        let analysis = analysis::analyze(text, "file:///completion_types.fdn");
        let items = visible_symbol_completion_items(&analysis.symbol_table, cursor);

        let worker = items
            .iter()
            .find(|item| item.label == "Worker")
            .expect("Worker completion");
        assert_eq!(worker.kind, Some(CompletionItemKind::CLASS));

        let direction = items
            .iter()
            .find(|item| item.label == "Direction")
            .expect("Direction completion");
        assert_eq!(direction.kind, Some(CompletionItemKind::ENUM));
    }

    #[test]
    fn dot_receiver_type_name_supports_direct_types_and_recursive_fields() {
        let text = r#"enum Direction {
    North
    South
}

object Compass {
    var heading oftype Direction
}

object Holder {
    var compass oftype Compass
}

Direction.
Compass.
Holder.compass.
"#;
        let analysis = analysis::analyze(text, "file:///receiver_types.fdn");
        let direction_offset = text.find("Direction.").expect("Direction cursor") as u32;
        let compass_offset = text.find("Compass.").expect("Compass cursor") as u32;
        let holder_offset = text.find("Holder.compass.").expect("Holder cursor") as u32
            + "Holder.compass".len() as u32;

        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                direction_offset + "Direction".len() as u32,
            )
            .as_deref(),
            Some("Direction")
        );
        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                compass_offset + "Compass".len() as u32,
            )
            .as_deref(),
            Some("Compass")
        );
        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                holder_offset,
            )
            .as_deref(),
            Some("Compass")
        );
        assert_eq!(
            analysis
                .symbol_table
                .get("Compass.heading")
                .and_then(|entry| entry.ty_name.as_deref()),
            Some("Direction")
        );
        assert_eq!(
            analysis
                .symbol_table
                .get("Holder.compass")
                .and_then(|entry| entry.ty_name.as_deref()),
            Some("Compass")
        );
    }

    #[test]
    fn builtin_hover_docs_cover_functions_and_type_like_values() {
        let len = builtin_hover_markdown("len").expect("missing len hover doc");
        assert!(len.contains("len(value) -> integer"));

        let integer = builtin_hover_markdown("integer").expect("missing integer hover doc");
        assert!(integer.contains("integer(value) -> integer"));
    }

    #[test]
    fn stdlib_member_hover_docs_cover_recent_exports() {
        let sleep =
            stdlib_member_hover_markdown("time", "sleep").expect("missing std.time.sleep doc");
        assert!(sleep.contains("std.time.sleep(ms) -> nothing"));

        let wait_any = stdlib_member_hover_markdown("async", "waitAny")
            .expect("missing std.async.waitAny doc");
        assert!(wait_any.contains("std.async.waitAny(handles) -> Pending"));
    }

    #[test]
    fn stdlib_module_hover_docs_cover_import_targets() {
        let env = stdlib_module_hover_markdown("env").expect("missing std.env hover doc");
        assert!(env.contains("use std.env"));
        assert!(env.contains("Environment variables"));
    }

    #[test]
    fn organize_imports_rewrites_grouped_unused_member_instead_of_deleting_line() {
        let text = "use std.parallel.{parallelMap, parallelFilter, parallelReduce, parallelForEach}\n\naction main {\n    print(parallelMap)\n}\n";
        let diagnostics = vec![Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 79,
                },
            },
            severity: Some(DiagnosticSeverity::INFORMATION),
            code: Some(NumberOrString::String("W1005".to_string())),
            source: Some("fidan".to_string()),
            message: "unused import `parallelForEach`".to_string(),
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        }];

        let edits =
            build_remove_unused_imports_edits_for_text("file:///demo.fdn", text, &diagnostics);
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].new_text,
            "use std.parallel.{parallelMap, parallelFilter, parallelReduce}"
        );
        assert_eq!(edits[0].range.start.line, 0);
        assert_eq!(edits[0].range.end.line, 0);
    }
}
