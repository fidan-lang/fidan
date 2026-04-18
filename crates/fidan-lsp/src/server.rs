//! tower-lsp `LanguageServer` implementation for Fidan.

use crate::{
    analysis, convert, document::Document, semantic, store::DocumentStore, symbols::SymKind,
    symbols::SymbolEntry,
};
use fidan_config::{
    BUILTIN_FUNCTIONS, BuiltinReturnKind, ReceiverBuiltinKind, builtin_return_kind, decorator_info,
    editor_symbol_info, infer_receiver_member, receiver_member_param_type_names_for_type_name,
    receiver_member_return_type_name_for_type_name, receiver_member_signature_for_type_name,
    receiver_method_arity_bounds, type_name_info,
};
use fidan_fmt::{FormatOptions, format_source, load_format_options_for_path};
use fidan_source::{FileId, SourceFile, Span};
use fidan_stdlib::{
    STDLIB_MODULES, StdlibTypeSpec, infer_precise_stdlib_return_type,
    member_info as stdlib_member_info, member_return_type as stdlib_member_return_type,
    member_signature as stdlib_member_signature,
    member_signature_with_return as stdlib_member_signature_with_return,
    module_info as stdlib_module_info, parse_stdlib_type_spec,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};
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
    "handle",
    "parallel",
    "concurrent",
    "task",
    "spawn",
    "await",
    "Shared",
    "Pending",
    "WeakShared",
    "hashset",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditorFormatDefaults {
    indent_width: usize,
    max_line_len: usize,
}

impl Default for EditorFormatDefaults {
    fn default() -> Self {
        let opts = FormatOptions::default();
        Self {
            indent_width: opts.indent_width,
            max_line_len: opts.max_line_len,
        }
    }
}

fn editor_format_defaults_from_init(params: &InitializeParams) -> EditorFormatDefaults {
    let defaults = EditorFormatDefaults::default();
    let Some(options) = params.initialization_options.as_ref() else {
        return defaults;
    };

    let indent_width = json_usize(options, &["indentWidth", "indent_width"])
        .filter(|value| *value > 0)
        .unwrap_or(defaults.indent_width);
    let max_line_len = json_usize(options, &["maxLineLen", "max_line_len"])
        .filter(|value| *value > 0)
        .unwrap_or(defaults.max_line_len);

    EditorFormatDefaults {
        indent_width,
        max_line_len,
    }
}

fn json_usize(value: &serde_json::Value, keys: &[&str]) -> Option<usize> {
    for key in keys {
        let Some(raw) = value.get(*key) else {
            continue;
        };
        if let Some(n) = raw.as_u64() {
            return usize::try_from(n).ok();
        }
        if let Some(n) = raw.as_i64()
            && n > 0
        {
            return usize::try_from(n as u64).ok();
        }
    }
    None
}

fn fallback_format_options(
    request: &FormattingOptions,
    defaults: EditorFormatDefaults,
) -> FormatOptions {
    let indent_width = match request.tab_size {
        value if value > 0 => value as usize,
        _ => defaults.indent_width,
    };

    FormatOptions {
        indent_width,
        max_line_len: defaults.max_line_len,
        ..Default::default()
    }
}

fn stdlib_members(mod_name: &str) -> &'static [&'static str] {
    stdlib_module_info(mod_name)
        .map(|info| (info.exports)())
        .unwrap_or(&[])
}

fn stdlib_module_hover_markdown(mod_name: &str) -> Option<String> {
    let info = stdlib_module_info(mod_name)?;
    Some(format!("```fidan\nuse std.{mod_name}\n```\n\n{}", info.doc))
}

fn stdlib_precise_return_label(
    mod_name: &str,
    member_name: &str,
    arg_tys: &[String],
) -> Option<String> {
    let args = arg_tys
        .iter()
        .map(|ty| parse_stdlib_type_spec(ty).unwrap_or(StdlibTypeSpec::Dynamic))
        .collect::<Vec<_>>();
    infer_precise_stdlib_return_type(mod_name, member_name, &args).map(|spec| spec.to_string())
}

fn stdlib_member_hover_markdown(
    mod_name: &str,
    member_name: &str,
    arg_tys: Option<&[String]>,
) -> Option<String> {
    let info = stdlib_member_info(mod_name, member_name)?;
    let signature = arg_tys
        .and_then(|tys| stdlib_precise_return_label(mod_name, member_name, tys))
        .and_then(|ret_type| {
            stdlib_member_signature_with_return(mod_name, member_name, Some(&ret_type))
        })
        .or_else(|| stdlib_member_signature(mod_name, member_name))
        .unwrap_or_else(|| info.signature.to_string());
    Some(format!("```fidan\n{}\n```\n\n{}", signature, info.doc))
}

fn stdlib_call_args_at_offset<'a>(
    doc: &'a Document,
    offset: u32,
    mod_name: &str,
    member_name: &str,
) -> Option<&'a [String]> {
    doc.stdlib_call_sites
        .iter()
        .find(|site| {
            site.module_name == mod_name
                && site.member_name == member_name
                && offset >= site.callee_span.start
                && offset < site.callee_span.end
        })
        .map(|site| site.arg_tys.as_slice())
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
    let info = editor_symbol_info(name)?;
    Some(format!("```fidan\n{}\n```\n\n{}", info.signature, info.doc))
}

fn type_name_hover_markdown(name: &str) -> Option<String> {
    let info = type_name_info(name)?;
    Some(format!("```fidan\n{}\n```\n\n{}", info.signature, info.doc))
}

fn is_type_name_context(text: &str, span: Span) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = (span.start as usize).min(bytes.len());
    while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }

    let end = cursor;
    while cursor > 0 && is_ident_byte(bytes[cursor - 1]) {
        cursor -= 1;
    }

    matches!(text.get(cursor..end), Some("oftype" | "returns"))
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
    decl_span: Span,
    ret_type: &str,
    preserve_member_resolution: bool,
) {
    let patch_entry = |sym_entry: &mut SymbolEntry| {
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
    };

    if let Some(sym_entry) = doc.symbol_table.entries.get_mut(var_name)
        && sym_entry.span == decl_span
    {
        patch_entry(sym_entry);
    }

    for scope in &mut doc.symbol_table.lexical_scopes {
        if let Some(sym_entry) = scope.entries.get_mut(var_name)
            && sym_entry.span == decl_span
        {
            patch_entry(sym_entry);
        }
    }

    if let Some((span, _)) = doc.identifier_spans.iter().find(|(span, n)| {
        n == var_name && span.start >= decl_span.start && span.end <= decl_span.end
    }) {
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
    editor_format_defaults: Arc<RwLock<EditorFormatDefaults>>,
}

impl FidanLsp {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            store: Arc::new(DocumentStore::new()),
            editor_format_defaults: Arc::new(RwLock::new(EditorFormatDefaults::default())),
        }
    }

    /// Re-analyse `text`, update the document store and push diagnostics to
    /// the editor.  Also proactively loads file imports that are not yet in
    /// the store.
    async fn refresh(&self, uri: &Url, version: i32, text: &str) {
        let result = analysis::analyze(text, uri.as_str());

        // Compute absolute URLs for every import in this document.
        let current_path = uri.to_file_path().ok();
        let resolved_imports = resolve_document_imports(
            current_path.as_deref(),
            &result.imports,
            &result.user_module_imports,
        );

        let stdlib_import_map: HashMap<String, String> =
            result.stdlib_imports.into_iter().collect();
        let stdlib_direct_import_map: HashMap<String, (String, String)> = result
            .stdlib_direct_imports
            .into_iter()
            .map(|(binding, module_name, member_name)| (binding, (module_name, member_name)))
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
                imports: resolved_imports.namespace_imports.clone(),
                direct_imports: resolved_imports.direct_imports.clone(),
                wildcard_imports: resolved_imports.wildcard_imports.clone(),
                stdlib_imports: stdlib_import_map,
                stdlib_direct_imports: stdlib_direct_import_map,
                stdlib_call_sites: result.stdlib_call_sites,
                inlay_hint_sites: result.inlay_hint_sites,
                member_access_sites: result.member_access_sites,
            },
        );
        // Proactively analyse imported files.  Background-loaded documents
        // (version == -1) are always re-read from disk so that edits to imported
        // files are reflected immediately without requiring the user to open them
        // in the editor.  Files that are actively open in the editor (version ≥ 0)
        // are managed through their own did-open / did-change notifications and
        // must NOT be overwritten with the on-disk version here.
        let mut seen_imports = HashSet::new();
        for import_url in resolved_imports
            .namespace_imports
            .values()
            .chain(resolved_imports.direct_imports.values().map(|(url, _)| url))
            .chain(resolved_imports.wildcard_imports.iter())
            .filter(|url| seen_imports.insert((**url).clone()))
        {
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
                let background_path = path;
                let background_imports = resolve_document_imports(
                    Some(&background_path),
                    &r.imports,
                    &r.user_module_imports,
                );
                let background_stdlib_imports = r.stdlib_imports.into_iter().collect();
                let background_stdlib_direct_imports = r
                    .stdlib_direct_imports
                    .into_iter()
                    .map(|(binding, module_name, member_name)| {
                        (binding, (module_name, member_name))
                    })
                    .collect();
                self.store.insert(
                    import_url.clone(),
                    Document {
                        version: -1, // -1 = background-loaded; reloaded on every parent refresh
                        text: file_text,
                        diagnostics: vec![], // no diagnostics for background docs
                        semantic_tokens: r.semantic_tokens,
                        symbol_table: r.symbol_table,
                        identifier_spans: r.identifier_spans,
                        imports: background_imports.namespace_imports,
                        direct_imports: background_imports.direct_imports,
                        wildcard_imports: background_imports.wildcard_imports,
                        stdlib_imports: background_stdlib_imports,
                        stdlib_direct_imports: background_stdlib_direct_imports,
                        stdlib_call_sites: r.stdlib_call_sites,
                        inlay_hint_sites: vec![], // not shown for background docs
                        member_access_sites: r.member_access_sites,
                    },
                );
            }
        }

        // Patch `var x: dynamic` entries whose init was a cross-module method call.
        // Now that background docs are loaded we can resolve the actual return type.
        for site in &result.dynamic_var_call_sites {
            if let Some((_, entry)) =
                self.resolve_member_cross_doc(&site.receiver_type, &site.method_name)
                && let Some(ref ret_type) = entry.return_type
                && let Some(mut doc) = self.store.get_mut(uri)
            {
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, ret_type, true);
            }
        }

        for site in &result.stdlib_var_call_sites {
            if let Some(ret_type) = stdlib_member_return_type(&site.module_name, &site.member_name)
                && ret_type != "dynamic"
                && let Some(mut doc) = self.store.get_mut(uri)
            {
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, ret_type, false);
            }
        }

        for site in &result.imported_constructor_call_sites {
            if let Some(ret_type) = imported_constructor_type_name(
                &self.store,
                &resolved_imports.namespace_imports,
                &resolved_imports.direct_imports,
                site,
            ) && let Some(mut doc) = self.store.get_mut(uri)
            {
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, &ret_type, true);
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
        let post_patch_extra = self
            .store
            .get(uri)
            .map(|doc| doc.symbol_table.clone())
            .map(|symbol_table| {
                self.check_receiver_chain_method_diagnostics(
                    text,
                    uri,
                    &symbol_table,
                    &result.receiver_chain_method_call_sites,
                )
            })
            .unwrap_or_default();
        let mut all_diags = result.diagnostics;
        all_diags.extend(extra);
        all_diags.extend(post_patch_extra);
        all_diags = filter_wildcard_import_undefined_diagnostics(
            &self.store,
            &resolved_imports.wildcard_imports,
            all_diags,
        );
        all_diags = filter_imported_object_type_diagnostics(
            &self.store,
            &resolved_imports.direct_imports,
            all_diags,
        );
        all_diags = dedupe_diagnostics(all_diags);
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
            if let Some(result) = self.resolve_builtin_member_alias(&cur_type, member_name) {
                return Some(result);
            }
            // Follow the parent chain: get the Object entry for `cur_type`
            // from any open document and check its recorded parent class.
            if builtin_receiver_info(&cur_type).is_some() {
                return None;
            }
            let (_, type_entry) = self.store.find_in_any_doc(&cur_type)?;
            cur_type = type_entry.parent_type_name?;
        }
        None
    }

    fn resolve_builtin_member_alias(
        &self,
        type_name: &str,
        member_name: &str,
    ) -> Option<(Url, SymbolEntry)> {
        let (receiver_kind, canonical_type_name) = builtin_receiver_info(type_name)?;
        let member = infer_receiver_member(receiver_kind, member_name)?;
        let key = format!("{}.{}", canonical_type_name, member.canonical_name);
        let (uri, entry) = self.store.find_in_any_doc(&key)?;
        Some((
            uri,
            specialize_builtin_member_entry(type_name, member.canonical_name, &entry),
        ))
    }

    fn collect_completion_members(&self, type_name: &str) -> Vec<(String, SymbolEntry)> {
        let direct = self.store.collect_type_members(type_name);
        if !direct.is_empty() {
            return direct;
        }

        let Some((_, canonical_type_name)) = builtin_receiver_info(type_name) else {
            return vec![];
        };
        if canonical_type_name == type_name {
            return vec![];
        }

        self.store
            .collect_type_members(canonical_type_name)
            .into_iter()
            .map(|(member, entry)| {
                let specialized = specialize_builtin_member_entry(type_name, &member, &entry);
                (member, specialized)
            })
            .collect()
    }

    fn type_name_is_known(&self, type_name: &str) -> bool {
        self.store.find_in_any_doc(type_name).is_some()
            || type_name_info(type_name).is_some()
            || builtin_receiver_info(type_name).is_some()
    }

    fn member_arity_bounds(
        &self,
        type_name: &str,
        member_name: &str,
        entry: &SymbolEntry,
    ) -> (usize, Option<usize>) {
        if let Some((receiver_kind, _)) = builtin_receiver_info(type_name)
            && let Some(bounds) = receiver_method_arity_bounds(receiver_kind, member_name)
        {
            return bounds;
        }

        (
            entry
                .param_required
                .iter()
                .filter(|&&required| required)
                .count(),
            Some(entry.param_types.len()),
        )
    }

    fn member_entry_result_type_name(
        &self,
        member_name: &str,
        entry: &SymbolEntry,
    ) -> Option<String> {
        match &entry.kind {
            SymKind::Object | SymKind::Enum => Some(member_name.to_string()),
            SymKind::EnumVariant => entry.return_type.clone().or_else(|| entry.ty_name.clone()),
            _ => entry.ty_name.clone(),
        }
    }

    fn resolve_receiver_chain_type_name(
        &self,
        symbol_table: &crate::symbols::SymbolTable,
        segments: &[String],
        visible_offset: u32,
    ) -> Option<String> {
        let first = segments.first()?;
        let mut current_type = {
            let entry = symbol_table.lookup_visible(visible_offset, first.as_str())?;
            match &entry.kind {
                SymKind::Object | SymKind::Enum => first.clone(),
                _ => entry.ty_name.clone()?,
            }
        };

        for segment in segments.iter().skip(1) {
            if let Some(entry) = symbol_table.get(&format!("{}.{}", current_type, segment)) {
                current_type = self.member_entry_result_type_name(segment, entry)?;
                continue;
            }

            let (_, entry) = self.resolve_member_cross_doc(&current_type, segment)?;
            current_type = self.member_entry_result_type_name(segment, &entry)?;
        }

        Some(current_type)
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
            if !self.type_name_is_known(type_name) {
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
                    if self.type_name_is_known(&site.receiver_ty) {
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
                    let (required_count, max_args) =
                        self.member_arity_bounds(&site.receiver_ty, &site.method_name, &entry);
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
                    } else if let Some(max_args) = max_args
                        && site.arg_tys.len() > max_args
                    {
                        diags.push(Diagnostic {
                            range: convert::span_to_range(&file, site.span),
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: Some(NumberOrString::String("E0305".into())),
                            source: Some("fidan".into()),
                            message: format!(
                                "expected {} argument{}, got {}",
                                max_args,
                                if max_args == 1 { "" } else { "s" },
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

    fn check_receiver_chain_method_diagnostics(
        &self,
        doc_text: &str,
        file_uri: &Url,
        symbol_table: &crate::symbols::SymbolTable,
        call_sites: &[analysis::ReceiverChainMethodCallSite],
    ) -> Vec<Diagnostic> {
        let file = SourceFile::new(FileId(0), file_uri.as_str(), doc_text);
        let mut diags = Vec::new();

        for site in call_sites {
            let Some(receiver_ty) = self.resolve_receiver_chain_type_name(
                symbol_table,
                &site.receiver_segments,
                site.receiver_offset,
            ) else {
                continue;
            };
            if symbol_table
                .get(&format!("{}.{}", receiver_ty, site.method_name))
                .is_some()
            {
                continue;
            }

            match self.resolve_member_cross_doc(&receiver_ty, &site.method_name) {
                None => {
                    if self.type_name_is_known(&receiver_ty) {
                        diags.push(Diagnostic {
                            range: convert::span_to_range(&file, site.span),
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: Some(NumberOrString::String("E0204".into())),
                            source: Some("fidan".into()),
                            message: format!(
                                "object `{}` has no field or method `{}`",
                                receiver_ty, site.method_name
                            ),
                            ..Default::default()
                        });
                    }
                }
                Some((_, entry)) => {
                    let (required_count, max_args) =
                        self.member_arity_bounds(&receiver_ty, &site.method_name, &entry);
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
                    } else if let Some(max_args) = max_args
                        && site.arg_tys.len() > max_args
                    {
                        diags.push(Diagnostic {
                            range: convert::span_to_range(&file, site.span),
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: Some(NumberOrString::String("E0305".into())),
                            source: Some("fidan".into()),
                            message: format!(
                                "expected {} argument{}, got {}",
                                max_args,
                                if max_args == 1 { "" } else { "s" },
                                site.arg_tys.len()
                            ),
                            ..Default::default()
                        });
                    } else {
                        for (index, (param_ty, arg_ty)) in entry
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
                                        index + 1,
                                        site.method_name,
                                        param_ty,
                                        arg_ty,
                                    ),
                                    ..Default::default()
                                });
                                break;
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

    /// Build `TextEdit`s for organize-imports diagnostics in `uri`.
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

fn builtin_receiver_info(type_name: &str) -> Option<(ReceiverBuiltinKind, &'static str)> {
    Some(match type_name {
        "integer" => (ReceiverBuiltinKind::Integer, "integer"),
        "float" => (ReceiverBuiltinKind::Float, "float"),
        "boolean" => (ReceiverBuiltinKind::Boolean, "boolean"),
        "string" => (ReceiverBuiltinKind::String, "string"),
        "list" => (ReceiverBuiltinKind::List, "list"),
        "dict" | "map" => (ReceiverBuiltinKind::Dict, "dict"),
        "hashset" => (ReceiverBuiltinKind::HashSet, "hashset"),
        "Shared" => (ReceiverBuiltinKind::Shared, "Shared"),
        "WeakShared" => (ReceiverBuiltinKind::WeakShared, "WeakShared"),
        "Pending" => (ReceiverBuiltinKind::Pending, "Pending"),
        "action" => (ReceiverBuiltinKind::Function, "action"),
        "nothing" => (ReceiverBuiltinKind::Nothing, "nothing"),
        _ if type_name.starts_with("list oftype ") => (ReceiverBuiltinKind::List, "list"),
        _ if type_name.starts_with("dict oftype ") || type_name.starts_with("map oftype ") => {
            (ReceiverBuiltinKind::Dict, "dict")
        }
        _ if type_name.starts_with("hashset oftype ") => (ReceiverBuiltinKind::HashSet, "hashset"),
        _ if type_name.starts_with("Shared oftype ") => (ReceiverBuiltinKind::Shared, "Shared"),
        _ if type_name.starts_with("WeakShared oftype ") => {
            (ReceiverBuiltinKind::WeakShared, "WeakShared")
        }
        _ if type_name.starts_with("Pending oftype ") => (ReceiverBuiltinKind::Pending, "Pending"),
        _ => return None,
    })
}

fn specialize_builtin_member_entry(
    receiver_type_name: &str,
    member_name: &str,
    entry: &SymbolEntry,
) -> SymbolEntry {
    let Some((receiver_kind, _)) = builtin_receiver_info(receiver_type_name) else {
        return entry.clone();
    };
    let Some(member) = infer_receiver_member(receiver_kind, member_name) else {
        return entry.clone();
    };

    let mut specialized = entry.clone();
    if let Some(signature) = receiver_member_signature_for_type_name(
        receiver_kind,
        receiver_type_name,
        member.canonical_name,
    ) {
        specialized.detail = format!("```fidan\n{}\n```", signature);
    }
    if let Some(param_types) = receiver_member_param_type_names_for_type_name(
        receiver_kind,
        receiver_type_name,
        member.canonical_name,
    ) {
        specialized.param_types = param_types;
    }
    if let Some(return_type) = receiver_member_return_type_name_for_type_name(
        receiver_kind,
        receiver_type_name,
        member.canonical_name,
    ) {
        specialized.ty_name = Some(return_type.clone());
        specialized.return_type = Some(return_type);
    }

    specialized
}

struct ResolvedImports {
    namespace_imports: HashMap<String, Url>,
    direct_imports: HashMap<String, (Url, String)>,
    wildcard_imports: Vec<Url>,
}

fn resolve_document_imports(
    current_path: Option<&Path>,
    file_imports: &[analysis::FileImport],
    user_module_imports: &[analysis::UserModuleImport],
) -> ResolvedImports {
    let mut namespace_imports = HashMap::new();
    let mut direct_imports = HashMap::new();
    let mut wildcard_imports = Vec::new();

    for import in file_imports {
        let Some(url) = resolve_file_import_url(current_path, &import.path) else {
            continue;
        };
        if let Some(alias) = &import.alias {
            namespace_imports.insert(alias.clone(), url);
        } else {
            wildcard_imports.push(url);
        }
    }

    for import in user_module_imports {
        let Some(url) = resolve_user_module_import_url(current_path, &import.path, import.grouped)
        else {
            continue;
        };
        if import.grouped {
            let Some(target) = import.path.last().cloned() else {
                continue;
            };
            direct_imports.insert(target.clone(), (url, target));
        } else {
            let Some(binding) = import.alias.clone().or_else(|| import.path.last().cloned()) else {
                continue;
            };
            namespace_imports.insert(binding, url);
        }
    }

    ResolvedImports {
        namespace_imports,
        direct_imports,
        wildcard_imports,
    }
}

fn resolve_file_import_url(current_path: Option<&Path>, rel_path: &str) -> Option<Url> {
    let abs = if rel_path.starts_with('/') || rel_path.contains(':') {
        std::path::PathBuf::from(rel_path)
    } else if let Some(parent) = current_path.and_then(|path| path.parent()) {
        parent.join(rel_path)
    } else {
        return None;
    };
    Url::from_file_path(&abs).ok()
}

fn resolve_user_module_import_url(
    current_path: Option<&Path>,
    segments: &[String],
    grouped: bool,
) -> Option<Url> {
    let parent = current_path.and_then(|path| path.parent())?;
    let resolved = if grouped && segments.len() > 1 {
        resolve_user_module_candidate(parent, &[&segments[..segments.len() - 1], segments])
    } else {
        resolve_user_module_candidate(parent, &[segments])
    }?;
    Url::from_file_path(resolved).ok()
}

fn resolve_user_module_candidate(
    base_dir: &Path,
    segments_sets: &[&[String]],
) -> Option<std::path::PathBuf> {
    let mut fallback = None;
    for segments in segments_sets {
        for candidate in user_module_path_candidates(base_dir, segments) {
            if fallback.is_none() {
                fallback = Some(candidate.clone());
            }
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    fallback
}

fn user_module_path_candidates(base_dir: &Path, segments: &[String]) -> Vec<std::path::PathBuf> {
    let (dir_parts, leaf) = segments.split_at(segments.len().saturating_sub(1));

    let mut dir = base_dir.to_path_buf();
    for part in dir_parts {
        dir.push(part);
    }

    let leaf = leaf.first().map(|segment| segment.as_str()).unwrap_or("");
    vec![
        dir.join(format!("{leaf}.fdn")),
        dir.join(leaf).join("init.fdn"),
    ]
}

fn load_background_document(store: &DocumentStore, url: &Url) -> Option<()> {
    // Keep editor-managed documents authoritative. Background snapshots
    // (version < 0) are refreshed from disk on every lookup.
    let existing_version = store.get(url).map(|doc| doc.version);
    if existing_version.is_some_and(|version| version >= 0) {
        return Some(());
    }

    let path = match url.to_file_path() {
        Ok(path) => path,
        Err(_) if existing_version.is_some() => return Some(()),
        Err(_) => return None,
    };
    let file_text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) if existing_version.is_some() => return Some(()),
        Err(_) => return None,
    };
    let analysis = analysis::analyze(&file_text, url.as_str());
    let resolved_imports = resolve_document_imports(
        Some(&path),
        &analysis.imports,
        &analysis.user_module_imports,
    );
    store.insert(
        url.clone(),
        Document {
            version: -1,
            text: file_text,
            diagnostics: vec![],
            semantic_tokens: analysis.semantic_tokens,
            symbol_table: analysis.symbol_table,
            identifier_spans: analysis.identifier_spans,
            imports: resolved_imports.namespace_imports,
            direct_imports: resolved_imports.direct_imports,
            wildcard_imports: resolved_imports.wildcard_imports,
            stdlib_imports: analysis.stdlib_imports.into_iter().collect(),
            stdlib_direct_imports: analysis
                .stdlib_direct_imports
                .into_iter()
                .map(|(binding, module_name, member_name)| (binding, (module_name, member_name)))
                .collect(),
            stdlib_call_sites: analysis.stdlib_call_sites,
            inlay_hint_sites: vec![],
            member_access_sites: analysis.member_access_sites,
        },
    );
    Some(())
}

fn import_doc_entry(store: &DocumentStore, url: &Url, name: &str) -> Option<SymbolEntry> {
    load_background_document(store, url)?;
    let doc = store.get(url)?;
    doc.symbol_table.get(name).cloned()
}

enum ImportBindingDefinition {
    OpenFile(Url),
    ImportDoc(Url, String),
}

fn span_starts_import_statement(text: &str, span: Span) -> bool {
    let start = span.start as usize;
    let line_start = text[..start.min(text.len())]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line = text[line_start..].lines().next().unwrap_or("").trim_start();
    line.starts_with("use ") || line.starts_with("export use ")
}

fn import_binding_definition(
    text: &str,
    span: Span,
    name: &str,
    imports: &HashMap<String, Url>,
    direct_imports: &HashMap<String, (Url, String)>,
) -> Option<ImportBindingDefinition> {
    if !span_starts_import_statement(text, span) {
        return None;
    }
    if let Some(url) = imports.get(name) {
        return Some(ImportBindingDefinition::OpenFile(url.clone()));
    }
    if let Some((url, import_name)) = direct_imports.get(name) {
        return Some(ImportBindingDefinition::ImportDoc(
            url.clone(),
            import_name.clone(),
        ));
    }
    None
}

fn import_path_url_at_offset(text: &str, uri: &Url, offset: usize) -> Option<Url> {
    let current_path = uri.to_file_path().ok();
    let line_start = text[..offset.min(text.len())]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_end = text[offset.min(text.len())..]
        .find('\n')
        .map(|idx| offset.min(text.len()) + idx)
        .unwrap_or(text.len());
    let line = &text[line_start..line_end];
    let trimmed = line.trim_start();
    if !(trimmed.starts_with("use ") || trimmed.starts_with("export use ")) {
        return None;
    }
    let indent = line.len().saturating_sub(trimmed.len());
    let quote_start_rel = trimmed.find('"')?;
    let quote_start = line_start + indent + quote_start_rel;
    let after_quote = &trimmed[quote_start_rel + 1..];
    let quote_end_rel = after_quote.find('"')?;
    let quote_end = quote_start + 1 + quote_end_rel;
    if offset <= quote_start || offset > quote_end {
        return None;
    }
    resolve_file_import_url(current_path.as_deref(), &text[quote_start + 1..quote_end])
}

fn wildcard_import_entry(
    store: &DocumentStore,
    wildcard_imports: &[Url],
    name: &str,
) -> Option<(Url, SymbolEntry)> {
    for url in wildcard_imports {
        if let Some(entry) = import_doc_entry(store, url, name) {
            return Some((url.clone(), entry));
        }
    }
    None
}

fn wildcard_import_completion_items(
    store: &DocumentStore,
    wildcard_imports: &[Url],
) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for url in wildcard_imports {
        for (name, entry) in store.get_doc_top_level(url) {
            if seen.insert(name.clone()) {
                items.push(completion_item_for_symbol(&name, &entry, "3"));
            }
        }
    }
    items
}

fn direct_import_completion_items(
    store: &DocumentStore,
    direct_imports: &[(String, Url, String)],
) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for (binding, url, import_name) in direct_imports {
        if !seen.insert(binding.clone()) {
            continue;
        }
        if let Some(entry) = import_doc_entry(store, url, import_name) {
            items.push(completion_item_for_symbol(binding, &entry, "3"));
        }
    }
    items
}

fn filter_wildcard_import_undefined_diagnostics(
    store: &DocumentStore,
    wildcard_imports: &[Url],
    diags: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    diags
        .into_iter()
        .filter(|diag| {
            let Some(NumberOrString::String(code)) = diag.code.as_ref() else {
                return true;
            };
            if code != "E0101" {
                return true;
            }
            let Some(name) = extract_backticked_name(&diag.message) else {
                return true;
            };
            wildcard_import_entry(store, wildcard_imports, name).is_none()
        })
        .collect()
}

fn filter_imported_object_type_diagnostics(
    store: &DocumentStore,
    direct_imports: &HashMap<String, (Url, String)>,
    diags: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    diags
        .into_iter()
        .filter(|diag| {
            let Some(NumberOrString::String(code)) = diag.code.as_ref() else {
                return true;
            };
            if code != "E0105" {
                return true;
            }
            let Some(name) = extract_backticked_name(&diag.message) else {
                return true;
            };
            let Some((url, import_name)) = direct_imports.get(name) else {
                return true;
            };
            match import_doc_entry(store, url, import_name) {
                Some(entry) => !matches!(entry.kind, SymKind::Object | SymKind::Enum),
                None => true,
            }
        })
        .collect()
}

fn imported_constructor_type_name(
    store: &DocumentStore,
    namespace_imports: &HashMap<String, Url>,
    direct_imports: &HashMap<String, (Url, String)>,
    site: &analysis::ImportedConstructorCallSite,
) -> Option<String> {
    let entry = if site.is_namespace_alias {
        let url = namespace_imports.get(&site.import_binding)?;
        import_doc_entry(store, url, &site.constructor_name)?
    } else {
        let (url, import_name) = direct_imports.get(&site.import_binding)?;
        import_doc_entry(store, url, import_name)?
    };

    if matches!(entry.kind, SymKind::Object) {
        entry
            .ty_name
            .clone()
            .or(Some(site.constructor_name.clone()))
    } else {
        None
    }
}

fn dedupe_diagnostics(diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = HashSet::new();
    let mut unique = Vec::with_capacity(diags.len());

    for diag in diags {
        let key = (
            diag.range.start.line,
            diag.range.start.character,
            diag.range.end.line,
            diag.range.end.character,
            diag.code.clone(),
            diag.message.clone(),
        );
        if seen.insert(key) {
            unique.push(diag);
        }
    }

    unique
}

fn declaration_entry_at_offset(
    doc: &Document,
    offset: u32,
    hovered_name: &str,
) -> Option<SymbolEntry> {
    doc.symbol_table
        .lexical_scopes
        .iter()
        .flat_map(|scope| scope.entries.iter())
        .chain(doc.symbol_table.entries.iter())
        .filter(|(name, entry)| {
            offset >= entry.span.start
                && offset < entry.span.end
                && (name.as_str() == hovered_name || name.rsplit('.').next() == Some(hovered_name))
        })
        .min_by_key(|(_, entry)| entry.span.len())
        .map(|(_, entry)| entry.clone())
}

fn doc_comment_text_for_span(text: &str, span: Span) -> Option<String> {
    let mut line_end = text[..(span.start as usize).min(text.len())]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let mut lines = Vec::new();

    while line_end > 0 {
        let prev_end = line_end.saturating_sub(1);
        let line_start = text[..prev_end].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        let raw_line = &text[line_start..prev_end];
        let trimmed = raw_line.trim_end_matches('\r');
        let trimmed_start = trimmed.trim_start();

        if trimmed_start.is_empty() {
            break;
        }

        let Some(doc_line) = trimmed_start.strip_prefix("#>") else {
            break;
        };

        lines.push(doc_line.strip_prefix(' ').unwrap_or(doc_line).to_string());
        line_end = line_start;
    }

    if lines.is_empty() {
        None
    } else {
        lines.reverse();
        Some(lines.join("\n"))
    }
}

fn leading_doc_comment_text(text: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut started = false;

    for raw_line in text.lines() {
        let trimmed = raw_line.trim_end_matches('\r');
        let trimmed_start = trimmed.trim_start();

        if trimmed_start.is_empty() {
            if started {
                break;
            }
            continue;
        }

        let Some(doc_line) = trimmed_start.strip_prefix("#>") else {
            break;
        };

        started = true;
        lines.push(doc_line.strip_prefix(' ').unwrap_or(doc_line).to_string());
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn doc_comment_markdown(doc: &str) -> String {
    doc.lines().collect::<Vec<_>>().join("  \n")
}

fn append_doc_comment_to_hover(detail: String, text: &str, span: Span) -> String {
    match doc_comment_text_for_span(text, span) {
        Some(doc) => format!("{detail}\n\n---\n\n{}", doc_comment_markdown(&doc)),
        None => detail,
    }
}

fn append_module_doc_comment_to_hover(detail: String, text: &str) -> String {
    match leading_doc_comment_text(text) {
        Some(doc) => format!("{detail}\n\n---\n\n{}", doc_comment_markdown(&doc)),
        None => detail,
    }
}

fn user_module_import_hover_target_at_offset(
    text: &str,
    uri: &Url,
    offset: usize,
) -> Option<(Url, String)> {
    let current_path = uri.to_file_path().ok()?;
    let line_start = text[..offset.min(text.len())]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_end = text[offset.min(text.len())..]
        .find('\n')
        .map(|idx| offset.min(text.len()) + idx)
        .unwrap_or(text.len());
    let line = &text[line_start..line_end];
    let trimmed = line.trim_start();
    let indent = line.len().saturating_sub(trimmed.len());

    let rest = if let Some(rest) = trimmed.strip_prefix("use ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("export use ") {
        rest
    } else {
        return None;
    };

    if rest.starts_with('"') || rest.starts_with("std.") || rest.is_empty() {
        return None;
    }

    let grouped_end = rest.find(".{");
    let alias_end = rest.find(" as ");
    let whitespace_end = rest.find(char::is_whitespace);
    let path_end = [grouped_end, alias_end, whitespace_end]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    let module_path = rest[..path_end].trim_end();
    if module_path.is_empty() {
        return None;
    }

    let module_start = line_start + indent;
    let module_end = module_start + module_path.len();
    if offset < module_start || offset > module_end {
        return None;
    }

    let grouped = rest
        .get(path_end..)
        .is_some_and(|tail| tail.starts_with(".{"));
    let segments: Vec<String> = module_path.split('.').map(str::to_string).collect();
    let url = resolve_user_module_import_url(Some(&current_path), &segments, grouped)?;
    Some((url, module_path.to_string()))
}

fn build_remove_unused_imports_edits_for_text(
    uri_str: &str,
    text: &str,
    diagnostics: &[Diagnostic],
) -> Vec<TextEdit> {
    #[derive(Default)]
    struct GroupedImportPlan {
        remove_unused: HashSet<String>,
        duplicate_removals: HashMap<String, usize>,
    }

    let file = SourceFile::new(FileId(0), uri_str, text);
    let mut edits = Vec::new();
    let mut grouped_plans: HashMap<(u32, u32), GroupedImportPlan> = HashMap::new();
    let mut fallback_delete_ranges = Vec::new();

    for diag in diagnostics {
        let Some(NumberOrString::String(code)) = diag.code.as_ref() else {
            continue;
        };
        if !matches!(code.as_str(), "W1005" | "W1007") {
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
        grouped_plans.entry((start as u32, end as u32)).or_default();
        let plan = grouped_plans
            .get_mut(&(start as u32, end as u32))
            .expect("grouped import plan inserted above");
        match code.as_str() {
            "W1005" => {
                plan.remove_unused.insert(name.to_string());
            }
            "W1007" => {
                *plan.duplicate_removals.entry(name.to_string()).or_insert(0) += 1;
            }
            _ => {}
        }
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

        let mut plan = plan;
        let mut remaining = Vec::new();
        for member in members {
            if plan.remove_unused.contains(member) {
                continue;
            }
            if let Some(removals_left) = plan.duplicate_removals.get_mut(member)
                && *removals_left > 0
            {
                *removals_left -= 1;
                continue;
            }
            remaining.push(member);
        }

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

    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        if let Ok(mut defaults) = self.editor_format_defaults.write() {
            *defaults = editor_format_defaults_from_init(&params);
        }

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
            DocEntry(Url, SymbolEntry),
            ModuleDoc(Url, String),
            Plain(String),            // detail string, ready to return
            CrossDoc(String, String), // (type_name, member_name) to search across docs
            MemberAccess {
                receiver_type: String,
                member_name: String,
                receiver_chain: Option<(Vec<String>, u32, crate::symbols::SymbolTable)>,
            },
            ReceiverChain {
                segments: Vec<String>,
                visible_offset: u32,
                symbol_table: crate::symbols::SymbolTable,
                member_name: String,
            },
            ImportDoc(Url, String), // (import_file_url, symbol_name) — for `module.Type`
            WildcardImport(Vec<Url>, String),
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
                if doc
                    .text
                    .get(prev_span.end as usize..cur_span.start as usize)
                    == Some(".")
                {
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
                HoverLookup::DocEntry(uri.clone(), e.clone())
            } else if let Some(entry) = declaration_entry_at_offset(&doc, offset, cur_name.as_str())
            {
                HoverLookup::DocEntry(uri.clone(), entry)
            } else if let Some(pn) = prev_name
                && let Some(mod_name) = doc.stdlib_imports.get(pn)
                && let Some(detail) = stdlib_member_hover_markdown(
                    mod_name.as_str(),
                    cur_name.as_str(),
                    stdlib_call_args_at_offset(&doc, offset, mod_name.as_str(), cur_name.as_str()),
                )
            {
                HoverLookup::Plain(detail)
            } else if prev_name == Some("std") {
                match stdlib_module_hover_markdown(cur_name.as_str()) {
                    Some(detail) => HoverLookup::Plain(detail),
                    None => HoverLookup::NotFound,
                }
            } else if let Some(site) =
                member_access_site_at_offset(&doc.member_access_sites, offset)
            {
                if let Some(entry) = doc
                    .symbol_table
                    .get(&format!("{}.{}", site.receiver_type, site.member_name))
                {
                    HoverLookup::DocEntry(uri.clone(), entry.clone())
                } else {
                    let receiver_chain = if cur_span.start > 0
                        && doc.text.as_bytes().get(cur_span.start as usize - 1) == Some(&b'.')
                    {
                        let dot_pos = cur_span.start - 1;
                        let segments =
                            dotted_receiver_segments(&doc.identifier_spans, &doc.text, dot_pos);
                        if segments.is_empty() {
                            None
                        } else {
                            Some((segments, dot_pos, doc.symbol_table.clone()))
                        }
                    } else {
                        None
                    };
                    HoverLookup::MemberAccess {
                        receiver_type: site.receiver_type.clone(),
                        member_name: site.member_name.clone(),
                        receiver_chain,
                    }
                }
            } else if cur_span.start > 0
                && doc.text.as_bytes().get(cur_span.start as usize - 1) == Some(&b'.')
            {
                let dot_pos = cur_span.start - 1;
                let recv_chain =
                    dotted_receiver_segments(&doc.identifier_spans, &doc.text, dot_pos);
                if !recv_chain.is_empty() {
                    HoverLookup::ReceiverChain {
                        segments: recv_chain,
                        visible_offset: dot_pos,
                        symbol_table: doc.symbol_table.clone(),
                        member_name: cur_name.clone(),
                    }
                } else {
                    HoverLookup::NotFound
                }
            } else if is_type_name_context(doc.text.as_str(), *cur_span)
                && let Some(detail) = type_name_hover_markdown(cur_name.as_str())
            {
                HoverLookup::Plain(detail)
            } else if let Some(detail) = builtin_hover_markdown(cur_name.as_str()) {
                HoverLookup::Plain(detail)
            } else if let Some((module_url, module_path)) =
                user_module_import_hover_target_at_offset(doc.text.as_str(), uri, offset as usize)
            {
                HoverLookup::ModuleDoc(module_url, format!("```fidan\nuse {}\n```", module_path))
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
                HoverLookup::ModuleDoc(
                    url.clone(),
                    format!("```fidan\nimport \"{}\" as {}\n```", file_name, cur_name),
                )
            } else if let Some(mod_name) = doc.stdlib_imports.get(cur_name.as_str()) {
                match stdlib_module_hover_markdown(mod_name.as_str()) {
                    Some(detail) => HoverLookup::Plain(detail),
                    None => HoverLookup::NotFound,
                }
            } else if let Some((mod_name, member_name)) =
                doc.stdlib_direct_imports.get(cur_name.as_str())
                && let Some(detail) = stdlib_member_hover_markdown(
                    mod_name.as_str(),
                    member_name.as_str(),
                    stdlib_call_args_at_offset(
                        &doc,
                        offset,
                        mod_name.as_str(),
                        member_name.as_str(),
                    ),
                )
            {
                HoverLookup::Plain(detail)
            } else if let Some((url, import_name)) = doc.direct_imports.get(cur_name.as_str()) {
                HoverLookup::ImportDoc(url.clone(), import_name.clone())
            } else if !doc.wildcard_imports.is_empty() {
                HoverLookup::WildcardImport(doc.wildcard_imports.clone(), cur_name.clone())
            } else {
                HoverLookup::NotFound
            }
            // `doc` (DashMap Ref) is dropped here, releasing the shard lock.
        };

        // Phase 2: resolve or do cross-document parent-chain lookup.
        let detail = match hover_lookup {
            HoverLookup::DocEntry(doc_uri, entry) => match self.store.get(&doc_uri) {
                Some(doc) => {
                    append_doc_comment_to_hover(entry.detail.clone(), &doc.text, entry.span)
                }
                None => entry.detail,
            },
            HoverLookup::ModuleDoc(doc_uri, detail) => {
                let _ = load_background_document(&self.store, &doc_uri);
                match self.store.get(&doc_uri) {
                    Some(doc) => append_module_doc_comment_to_hover(detail, &doc.text),
                    None => detail,
                }
            }
            HoverLookup::Plain(d) => d,
            HoverLookup::CrossDoc(ty, member) => {
                match self.resolve_member_cross_doc(&ty, &member) {
                    Some((doc_uri, entry)) => match self.store.get(&doc_uri) {
                        Some(doc) => {
                            append_doc_comment_to_hover(entry.detail.clone(), &doc.text, entry.span)
                        }
                        None => entry.detail,
                    },
                    None => return Ok(None),
                }
            }
            HoverLookup::MemberAccess {
                receiver_type,
                member_name,
                receiver_chain,
            } => match self.resolve_member_cross_doc(&receiver_type, &member_name) {
                Some((doc_uri, entry)) => match self.store.get(&doc_uri) {
                    Some(doc) => {
                        append_doc_comment_to_hover(entry.detail.clone(), &doc.text, entry.span)
                    }
                    None => entry.detail,
                },
                None => {
                    let Some((segments, visible_offset, symbol_table)) = receiver_chain else {
                        return Ok(None);
                    };
                    let receiver_ty = match self.resolve_receiver_chain_type_name(
                        &symbol_table,
                        &segments,
                        visible_offset,
                    ) {
                        Some(ty) => ty,
                        None => return Ok(None),
                    };
                    match self.resolve_member_cross_doc(&receiver_ty, &member_name) {
                        Some((doc_uri, entry)) => match self.store.get(&doc_uri) {
                            Some(doc) => append_doc_comment_to_hover(
                                entry.detail.clone(),
                                &doc.text,
                                entry.span,
                            ),
                            None => entry.detail,
                        },
                        None => return Ok(None),
                    }
                }
            },
            HoverLookup::ReceiverChain {
                segments,
                visible_offset,
                symbol_table,
                member_name,
            } => {
                let receiver_ty = match self.resolve_receiver_chain_type_name(
                    &symbol_table,
                    &segments,
                    visible_offset,
                ) {
                    Some(ty) => ty,
                    None => return Ok(None),
                };

                if let Some(entry) = symbol_table.get(&format!("{}.{}", receiver_ty, member_name)) {
                    entry.detail.clone()
                } else {
                    match self.resolve_member_cross_doc(&receiver_ty, &member_name) {
                        Some((doc_uri, entry)) => match self.store.get(&doc_uri) {
                            Some(doc) => append_doc_comment_to_hover(
                                entry.detail.clone(),
                                &doc.text,
                                entry.span,
                            ),
                            None => entry.detail,
                        },
                        None => return Ok(None),
                    }
                }
            }
            HoverLookup::ImportDoc(url, name) => {
                // Look up the symbol directly in the imported document.
                match self.store.get(&url) {
                    Some(d) => match d.symbol_table.get(&name) {
                        Some(e) => append_doc_comment_to_hover(e.detail.clone(), &d.text, e.span),
                        None => return Ok(None),
                    },
                    None => return Ok(None),
                }
            }
            HoverLookup::WildcardImport(urls, name) => {
                match wildcard_import_entry(&self.store, &urls, &name) {
                    Some((doc_uri, entry)) => match self.store.get(&doc_uri) {
                        Some(doc) => {
                            append_doc_comment_to_hover(entry.detail.clone(), &doc.text, entry.span)
                        }
                        None => entry.detail,
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
            WildcardImport(Vec<Url>, String),
            OpenFile(Url), // open the imported file at line 0 (alias goto-def)
            NotFound,
        }

        let (definition_lookup, current_file) = {
            let doc = match self.store.get(uri) {
                Some(d) => d,
                None => return Ok(None),
            };
            let file = SourceFile::new(FileId(0), uri.as_str(), doc.text.as_str());
            let offset = lsp_pos_to_offset(&file, pos);
            if let Some(import_url) =
                import_path_url_at_offset(doc.text.as_str(), uri, offset as usize)
            {
                (DefinitionLookup::OpenFile(import_url), file)
            } else if let Some((module_url, _)) =
                user_module_import_hover_target_at_offset(doc.text.as_str(), uri, offset as usize)
            {
                (DefinitionLookup::OpenFile(module_url), file)
            } else {
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
                    if doc
                        .text
                        .get(prev_span.end as usize..cur_span.start as usize)
                        == Some(".")
                    {
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
                    match import_binding_definition(
                        &doc.text,
                        e.span,
                        cur_name.as_str(),
                        &doc.imports,
                        &doc.direct_imports,
                    ) {
                        Some(ImportBindingDefinition::OpenFile(url)) => {
                            DefinitionLookup::OpenFile(url)
                        }
                        Some(ImportBindingDefinition::ImportDoc(url, name)) => {
                            DefinitionLookup::ImportDoc(url, name)
                        }
                        None => DefinitionLookup::Found(e.span),
                    }
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
                } else if let Some((import_url, import_name)) =
                    doc.direct_imports.get(cur_name.as_str())
                {
                    DefinitionLookup::ImportDoc(import_url.clone(), import_name.clone())
                } else {
                    named_arg.map(named_to_lookup).unwrap_or_else(|| {
                        if doc.wildcard_imports.is_empty() {
                            DefinitionLookup::NotFound
                        } else {
                            DefinitionLookup::WildcardImport(
                                doc.wildcard_imports.clone(),
                                cur_name.clone(),
                            )
                        }
                    })
                };
                (p1, file) // `doc` dropped here
            }
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
                let span = match import_doc_entry(&self.store, &url, &name) {
                    Some(entry) => entry.span,
                    None => return Ok(None),
                };
                (url, span)
            }
            DefinitionLookup::WildcardImport(urls, name) => {
                match wildcard_import_entry(&self.store, &urls, &name) {
                    Some((src_uri, entry)) => (src_uri, entry.span),
                    None => return Ok(None),
                }
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
            /// Receiver type was inferred locally without cross-document lookup.
            TypeName(String),
            /// Receiver is a dotted chain that may need imported-doc lookup.
            ReceiverChain {
                segments: Vec<String>,
                visible_offset: u32,
                symbol_table: crate::symbols::SymbolTable,
            },
            /// Receiver is a file-module alias import — show its top-level exports.
            ModuleAlias(Url),
            /// Receiver is a stdlib module alias — show its exported member names.
            StdLibModule(String),
        }

        struct CompletionSeed {
            dot_res: Option<DotResolution>,
            dot_member_prefix: Option<String>,
            /// Declared symbols (non-dot completion path).
            local_items: Vec<CompletionItem>,
            /// Wildcard-imported file URLs available for unqualified lookup.
            wildcard_imports: Vec<Url>,
            /// Direct imported symbol bindings available for unqualified lookup.
            direct_imports: Vec<(String, Url, String)>,
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
                    dot_member_prefix: None,
                    local_items: vec![],
                    wildcard_imports: doc.wildcard_imports.clone(),
                    direct_imports: vec![],
                    named_param_entries: vec![],
                    named_param_cross: None,
                    import_ctx,
                }
            } else {
                // ── Dot-triggered receiver resolution ────────────────────────────
                let member_ctx =
                    member_completion_context(&doc.identifier_spans, &doc.text, cursor, trigger);
                let dot_res: Option<DotResolution> = if let Some(member_ctx) = member_ctx.as_ref() {
                    let dot_pos = member_ctx.dot_pos;
                    let recv_chain =
                        dotted_receiver_segments(&doc.identifier_spans, &doc.text, dot_pos);

                    if let Some(member_span) = member_ctx.member_span
                        && let Some(site) = doc
                            .member_access_sites
                            .iter()
                            .find(|site| site.member_span == member_span)
                    {
                        Some(DotResolution::TypeName(site.receiver_type.clone()))
                    } else if recv_chain.is_empty() {
                        resolve_dotted_receiver_type_name(
                            &doc.symbol_table,
                            &doc.identifier_spans,
                            &doc.text,
                            dot_pos,
                        )
                        .map(DotResolution::TypeName)
                    } else if recv_chain.len() == 1 {
                        let first = &recv_chain[0];
                        if let Some(url) = doc.imports.get(first.as_str()) {
                            Some(DotResolution::ModuleAlias(url.clone()))
                        } else if let Some(mod_name) = doc.stdlib_imports.get(first.as_str()) {
                            Some(DotResolution::StdLibModule(mod_name.clone()))
                        } else {
                            Some(DotResolution::ReceiverChain {
                                segments: recv_chain,
                                visible_offset: dot_pos,
                                symbol_table: doc.symbol_table.clone(),
                            })
                        }
                    } else {
                        Some(DotResolution::ReceiverChain {
                            segments: recv_chain,
                            visible_offset: dot_pos,
                            symbol_table: doc.symbol_table.clone(),
                        })
                    }
                } else {
                    None
                };

                // If dot-triggered and resolved, skip standard items entirely.
                if dot_res.is_some() {
                    CompletionSeed {
                        dot_res,
                        dot_member_prefix: member_ctx.map(|ctx| ctx.partial),
                        local_items: vec![],
                        wildcard_imports: doc.wildcard_imports.clone(),
                        direct_imports: vec![],
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
                        dot_member_prefix: None,
                        local_items,
                        wildcard_imports: doc.wildcard_imports.clone(),
                        direct_imports: doc
                            .direct_imports
                            .iter()
                            .map(|(binding, (url, import_name))| {
                                (binding.clone(), url.clone(), import_name.clone())
                            })
                            .collect(),
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
                            insert_text: Some(info.name.to_string()),
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
                            documentation: stdlib_member_hover_markdown(&mod_name, name, None).map(
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
            let member_prefix = completion_seed.dot_member_prefix.as_deref().unwrap_or("");
            match dot_res {
                DotResolution::TypeName(ty) => {
                    let members = self.collect_completion_members(&ty);
                    let items: Vec<CompletionItem> = members
                        .into_iter()
                        .filter(|(name, _)| {
                            member_prefix.is_empty() || name.starts_with(member_prefix)
                        })
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
                DotResolution::ReceiverChain {
                    segments,
                    visible_offset,
                    symbol_table,
                } => {
                    let Some(ty) = self.resolve_receiver_chain_type_name(
                        &symbol_table,
                        &segments,
                        visible_offset,
                    ) else {
                        return Ok(None);
                    };

                    let items: Vec<CompletionItem> = self
                        .collect_completion_members(&ty)
                        .into_iter()
                        .filter(|(name, _)| {
                            member_prefix.is_empty() || name.starts_with(member_prefix)
                        })
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
                        .filter(|(name, _)| {
                            member_prefix.is_empty() || name.starts_with(member_prefix)
                        })
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
                        .filter(|name| member_prefix.is_empty() || name.starts_with(member_prefix))
                        .map(|name| CompletionItem {
                            label: name.to_string(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            documentation: stdlib_member_hover_markdown(&mod_name, name, None).map(
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
        items.extend(direct_import_completion_items(
            &self.store,
            &completion_seed.direct_imports,
        ));

        let existing_labels: HashSet<String> =
            items.iter().map(|item| item.label.clone()).collect();
        items.extend(
            wildcard_import_completion_items(&self.store, &completion_seed.wildcard_imports)
                .into_iter()
                .filter(|item| !existing_labels.contains(&item.label)),
        );

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
            Stdlib {
                detail: String,
                param_labels: Vec<String>,
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

            if let Some(site) = doc
                .stdlib_call_sites
                .iter()
                .find(|site| site.callee_span.start == fn_span.start)
            {
                let detail = match stdlib_member_hover_markdown(
                    &site.module_name,
                    &site.member_name,
                    Some(&site.arg_tys),
                ) {
                    Some(detail) => detail,
                    None => return Ok(None),
                };
                let sig_label = signature_line_from_detail(&detail).unwrap_or_default();
                SignatureLookup::Stdlib {
                    detail,
                    param_labels: extract_param_labels_from_signature(&sig_label),
                    active_param,
                }
            } else {
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
            }
            // doc dropped here
        };

        // Phase 2: finalise response (cross-doc lookup if needed).
        let (sig_label, sig_params, active_param) = match signature_lookup {
            SignatureLookup::Found {
                fn_name,
                param_types,
                detail,
                active_param,
            } => {
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
                (
                    build_signature_label(&fn_name, &detail),
                    sig_params,
                    active_param,
                )
            }
            SignatureLookup::Stdlib {
                detail,
                param_labels,
                active_param,
            } => {
                let sig_params = param_labels
                    .into_iter()
                    .map(|label| ParameterInformation {
                        label: ParameterLabel::Simple(label),
                        documentation: None,
                    })
                    .collect();
                (
                    signature_line_from_detail(&detail).unwrap_or_default(),
                    sig_params,
                    active_param,
                )
            }
            SignatureLookup::CrossDoc {
                recv_ty,
                method_name,
                active_param,
            } => match self.resolve_member_cross_doc(&recv_ty, &method_name) {
                Some((_, entry)) if !entry.param_types.is_empty() => {
                    let sig_params: Vec<ParameterInformation> = entry
                        .param_types
                        .iter()
                        .enumerate()
                        .map(|(idx, ty)| {
                            let label = extract_param_label_from_detail(&entry.detail, idx)
                                .unwrap_or_else(|| format!("param{} -> {}", idx + 1, ty));
                            ParameterInformation {
                                label: ParameterLabel::Simple(label),
                                documentation: None,
                            }
                        })
                        .collect();
                    (
                        build_signature_label(&method_name, &entry.detail),
                        sig_params,
                        active_param,
                    )
                }
                _ => return Ok(None),
            },
            SignatureLookup::NotFound => return Ok(None),
        };
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

        // ── source.organizeImports: remove unused and duplicate imports ─────────
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
                    title: "Organize imports".to_string(),
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

        let editor_defaults = self
            .editor_format_defaults
            .read()
            .map(|guard| *guard)
            .unwrap_or_default();

        let opts = match uri.to_file_path() {
            Ok(path) => match load_format_options_for_path(Some(&path)) {
                Ok(Some(opts)) => opts,
                Ok(None) => fallback_format_options(&params.options, editor_defaults),
                Err(err) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("ignored .fidanfmt for {}: {err}", path.display()),
                        )
                        .await;
                    fallback_format_options(&params.options, editor_defaults)
                }
            },
            Err(_) => fallback_format_options(&params.options, editor_defaults),
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

fn signature_line_from_detail(detail: &str) -> Option<String> {
    let mut in_code_block = false;
    for line in detail.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block && !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn extract_param_labels_from_signature(signature: &str) -> Vec<String> {
    let Some(open) = signature.find('(') else {
        return Vec::new();
    };
    let Some(close) = signature.rfind(')') else {
        return Vec::new();
    };
    let params = signature[open + 1..close].trim();
    if params.is_empty() {
        return Vec::new();
    }

    let mut depth = 0usize;
    let mut start = 0usize;
    let mut labels = Vec::new();
    for (idx, ch) in params.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                labels.push(params[start..idx].trim().to_string());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    labels.push(params[start..].trim().to_string());
    labels
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

struct MemberCompletionContext {
    dot_pos: u32,
    partial: String,
    member_span: Option<Span>,
}

fn triggered_dot_position(
    identifier_spans: &[(Span, String)],
    text: &str,
    cursor: usize,
    trigger: Option<&str>,
) -> Option<u32> {
    let src = text.as_bytes();

    if cursor < src.len() && src.get(cursor) == Some(&b'.') {
        return Some(cursor as u32);
    }

    if cursor > 0 && src.get(cursor.saturating_sub(1)) == Some(&b'.') {
        return Some((cursor as u32).saturating_sub(1));
    }

    if trigger != Some(".") {
        return None;
    }

    identifier_spans
        .iter()
        .rfind(|(span, _)| span.end == cursor as u32)
        .map(|(span, _)| span.end)
}

fn member_completion_context(
    identifier_spans: &[(Span, String)],
    text: &str,
    cursor: usize,
    trigger: Option<&str>,
) -> Option<MemberCompletionContext> {
    let src = text.as_bytes();

    if let Some(dot_pos) = triggered_dot_position(identifier_spans, text, cursor, trigger) {
        return Some(MemberCompletionContext {
            dot_pos,
            partial: String::new(),
            member_span: None,
        });
    }

    let (span, _) = identifier_spans.iter().find(|(span, _)| {
        let cursor = cursor as u32;
        cursor >= span.start && cursor <= span.end
    })?;
    if span.start == 0 || src.get(span.start as usize - 1) != Some(&b'.') {
        return None;
    }

    let partial_end = cursor.min(span.end as usize);
    Some(MemberCompletionContext {
        dot_pos: span.start.saturating_sub(1),
        partial: text
            .get(span.start as usize..partial_end)
            .unwrap_or("")
            .to_string(),
        member_span: Some(*span),
    })
}

fn member_access_site_at_offset(
    sites: &[analysis::MemberAccessSite],
    offset: u32,
) -> Option<&analysis::MemberAccessSite> {
    sites
        .iter()
        .find(|site| offset >= site.member_span.start && offset < site.member_span.end)
}

fn resolve_dotted_receiver_type_name(
    symbol_table: &crate::symbols::SymbolTable,
    identifier_spans: &[(Span, String)],
    text: &str,
    dot_pos: u32,
) -> Option<String> {
    let segments = dotted_receiver_segments(identifier_spans, text, dot_pos);
    resolve_dotted_chain_type_name(symbol_table, &segments, dot_pos)
        .or_else(|| {
            resolve_call_expression_type_name(symbol_table, identifier_spans, text, dot_pos)
        })
        .or_else(|| resolve_literal_receiver_type_name(text, dot_pos))
}

fn builtin_return_type_name(name: &str) -> Option<String> {
    match builtin_return_kind(name)? {
        BuiltinReturnKind::Nothing => Some("nothing".to_string()),
        BuiltinReturnKind::String => Some("string".to_string()),
        BuiltinReturnKind::Integer => Some("integer".to_string()),
        BuiltinReturnKind::Float => Some("float".to_string()),
        BuiltinReturnKind::Boolean => Some("boolean".to_string()),
        BuiltinReturnKind::Dynamic => Some("dynamic".to_string()),
    }
}

fn resolve_dotted_chain_type_name(
    symbol_table: &crate::symbols::SymbolTable,
    segments: &[String],
    visible_offset: u32,
) -> Option<String> {
    let first = segments.first()?;
    let mut current_type = {
        let entry = symbol_table.lookup_visible(visible_offset, first.as_str())?;
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

fn find_matching_open_paren(text: &str, close_pos: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(close_pos) != Some(&b')') {
        return None;
    }

    let mut depth = 0i32;
    for idx in (0..=close_pos).rev() {
        match bytes[idx] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn resolve_call_expression_type_name(
    symbol_table: &crate::symbols::SymbolTable,
    identifier_spans: &[(Span, String)],
    text: &str,
    dot_pos: u32,
) -> Option<String> {
    let prefix = text.get(..dot_pos as usize)?.trim_end();
    let close_pos = prefix.len().checked_sub(1)?;
    let open_pos = find_matching_open_paren(prefix, close_pos)?;
    let callee_segments = dotted_receiver_segments(identifier_spans, text, open_pos as u32);
    let callee_name = callee_segments.last()?.clone();

    if callee_segments.len() == 1 {
        if let Some(entry) = symbol_table.lookup_visible(dot_pos, &callee_name) {
            return match &entry.kind {
                SymKind::Object | SymKind::Enum => Some(callee_name),
                _ => entry
                    .return_type
                    .clone()
                    .or_else(|| entry.ty_name.clone())
                    .or_else(|| builtin_return_type_name(&callee_name)),
            };
        }
        return builtin_return_type_name(&callee_name);
    }

    let receiver_type = resolve_dotted_chain_type_name(
        symbol_table,
        &callee_segments[..callee_segments.len().saturating_sub(1)],
        dot_pos,
    )?;
    let entry = symbol_table.get(&format!("{}.{}", receiver_type, callee_name))?;
    entry.return_type.clone().or_else(|| entry.ty_name.clone())
}

fn resolve_literal_receiver_type_name(text: &str, dot_pos: u32) -> Option<String> {
    let prefix = text.get(..dot_pos as usize)?.trim_end();
    let bytes = prefix.as_bytes();
    let end = bytes.len();
    if end == 0 {
        return None;
    }

    if bytes[end - 1] == b'"' {
        return Some("string".to_string());
    }

    if bytes[end - 1].is_ascii_digit() {
        let mut start = end;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        let mut is_float = false;
        if start > 0 && bytes[start - 1] == b'.' {
            let mut integer_start = start - 1;
            while integer_start > 0 && bytes[integer_start - 1].is_ascii_digit() {
                integer_start -= 1;
            }
            if integer_start < start - 1 {
                is_float = true;
            }
        }
        return Some(if is_float { "float" } else { "integer" }.to_string());
    }

    let mut start = end;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    match prefix.get(start..end)? {
        "true" | "false" => Some("boolean".to_string()),
        "nothing" => Some("nothing".to_string()),
        _ => None,
    }
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
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower_lsp::{LanguageServer, LspService};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn path_url(path: &Path) -> Url {
        Url::from_file_path(path).expect("file url")
    }

    fn import_test_path(rel_path: &str) -> PathBuf {
        workspace_root()
            .join("test/examples/import_test")
            .join(rel_path)
    }

    fn temp_test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("fidan-lsp-{name}-{nonce}"));
        std::fs::create_dir_all(&path).expect("create temp test dir");
        path
    }

    fn background_doc(uri: &Url, text: &str) -> Document {
        let analysis = analysis::analyze(text, uri.as_str());
        Document {
            version: -1,
            text: text.to_string(),
            diagnostics: vec![],
            semantic_tokens: analysis.semantic_tokens,
            symbol_table: analysis.symbol_table,
            identifier_spans: analysis.identifier_spans,
            imports: HashMap::new(),
            direct_imports: HashMap::new(),
            wildcard_imports: vec![],
            stdlib_imports: HashMap::new(),
            stdlib_direct_imports: HashMap::new(),
            stdlib_call_sites: analysis.stdlib_call_sites,
            inlay_hint_sites: vec![],
            member_access_sites: analysis.member_access_sites,
        }
    }

    #[test]
    fn stdlib_completion_surface_tracks_runtime_modules() {
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "async"));
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "collections"));
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "json"));
        assert!(STDLIB_MODULES.iter().any(|info| info.name == "parallel"));
        assert!(!STDLIB_MODULES.iter().any(|info| info.name == "net"));
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
        assert!(COMPLETION_KEYWORDS.contains(&"handle"));
        assert!(COMPLETION_KEYWORDS.contains(&"hashset"));
    }

    #[test]
    fn wildcard_import_helpers_resolve_symbols_and_filter_false_undefined_names() {
        let store = DocumentStore::new();
        let import_uri = Url::parse("file:///utils.fdn").expect("import uri");
        let import_text = r#"const var PI = 3.14159

action greet with (certain name oftype string) returns string {
    return name
}
"#;
        store.insert(import_uri.clone(), background_doc(&import_uri, import_text));

        let wildcard_imports = vec![import_uri.clone()];
        let (_, entry) = wildcard_import_entry(&store, &wildcard_imports, "greet")
            .expect("wildcard import should resolve greet");
        assert!(entry.detail.contains("greet"));

        let labels: Vec<String> = wildcard_import_completion_items(&store, &wildcard_imports)
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert!(labels.contains(&"greet".to_string()));
        assert!(labels.contains(&"PI".to_string()));

        let filtered = filter_wildcard_import_undefined_diagnostics(
            &store,
            &wildcard_imports,
            vec![
                Diagnostic {
                    code: Some(NumberOrString::String("E0101".into())),
                    message: "undefined name `greet`".into(),
                    ..Default::default()
                },
                Diagnostic {
                    code: Some(NumberOrString::String("E0101".into())),
                    message: "undefined name `missing`".into(),
                    ..Default::default()
                },
            ],
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].message, "undefined name `missing`");
    }

    #[test]
    fn wildcard_import_entry_loads_imported_docs_on_demand() {
        let store = DocumentStore::new();
        let import_uri = path_url(&import_test_path("utils.fdn"));

        let (resolved_uri, entry) =
            wildcard_import_entry(&store, std::slice::from_ref(&import_uri), "greet")
                .expect("wildcard import should resolve after lazy load");

        assert_eq!(resolved_uri, import_uri);
        assert!(entry.detail.contains("greet"));
    }

    #[test]
    fn direct_import_completion_items_surface_imported_objects() {
        let store = DocumentStore::new();
        let import_uri = Url::parse("file:///models.fdn").expect("import uri");
        let import_text = r#"object StorageManager {
    action new with (certain path oftype string) {}
}
"#;
        store.insert(import_uri.clone(), background_doc(&import_uri, import_text));

        let items = direct_import_completion_items(
            &store,
            &[(
                "StorageManager".to_string(),
                import_uri,
                "StorageManager".to_string(),
            )],
        );
        assert!(items.iter().any(|item| item.label == "StorageManager"));
        assert!(
            items
                .iter()
                .any(|item| item.kind == Some(CompletionItemKind::CLASS))
        );
    }

    #[test]
    fn imported_object_type_diagnostics_are_filtered_for_direct_imports() {
        let store = DocumentStore::new();
        let import_uri = Url::parse("file:///models.fdn").expect("import uri");
        let import_text = r#"object StorageManager {
    action new with (certain path oftype string) {}
}
"#;
        store.insert(import_uri.clone(), background_doc(&import_uri, import_text));

        let filtered = filter_imported_object_type_diagnostics(
            &store,
            &HashMap::from([(
                "StorageManager".to_string(),
                (import_uri, "StorageManager".to_string()),
            )]),
            vec![Diagnostic {
                code: Some(NumberOrString::String("E0105".into())),
                message: "undefined type `StorageManager`".into(),
                ..Default::default()
            }],
        );
        assert!(filtered.is_empty());
    }

    #[test]
    fn patch_var_inferred_type_updates_lexical_scope_entries() {
        let analysis = analysis::analyze(
            r#"action main {
    var client = nothing
    client
}
"#,
            "file:///locals.fdn",
        );
        let mut doc = Document {
            version: 1,
            text: "action main {\n    var client = nothing\n    client\n}\n".to_string(),
            diagnostics: vec![],
            semantic_tokens: analysis.semantic_tokens,
            symbol_table: analysis.symbol_table,
            identifier_spans: analysis.identifier_spans,
            imports: HashMap::new(),
            direct_imports: HashMap::new(),
            wildcard_imports: vec![],
            stdlib_imports: HashMap::new(),
            stdlib_direct_imports: HashMap::new(),
            stdlib_call_sites: analysis.stdlib_call_sites,
            inlay_hint_sites: analysis.inlay_hint_sites,
            member_access_sites: analysis.member_access_sites,
        };

        let decl_span = doc
            .symbol_table
            .lexical_scopes
            .iter()
            .find_map(|scope| scope.entries.get("client").map(|entry| entry.span))
            .expect("client lexical entry");

        patch_var_inferred_type(&mut doc, "client", decl_span, "StorageManager", true);

        let patched = doc
            .symbol_table
            .lexical_scopes
            .iter()
            .find_map(|scope| scope.entries.get("client"))
            .expect("patched client lexical entry");
        assert_eq!(patched.ty_name.as_deref(), Some("StorageManager"));
        assert!(patched.detail.contains("StorageManager"));
    }

    #[test]
    fn cross_module_call_diagnostics_report_too_many_arguments() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let import_uri = Url::parse("file:///models.fdn").expect("import uri");
            let import_text = r#"object StorageManager {
    action addTask with (certain name oftype string) {}
}
"#;
            backend
                .store
                .insert(import_uri.clone(), background_doc(&import_uri, import_text));

            let site = fidan_typeck::CrossModuleCallSite {
                receiver_ty: "StorageManager".to_string(),
                method_name: "addTask".to_string(),
                arg_tys: vec!["string".to_string(), "integer".to_string()],
                span: Span::new(FileId(0), 0, 20),
            };

            let diags = backend.check_cross_module_diagnostics(
                "storage.addTask(\"x\", 1)",
                &Url::parse("file:///main.fdn").expect("main uri"),
                &[],
                &[site],
            );

            assert!(diags.iter().any(|diag| {
                matches!(diag.code, Some(NumberOrString::String(ref code)) if code == "E0305")
                    && diag.message.contains("expected 1 argument, got 2")
            }));
        });
    }

    #[test]
    fn receiver_chain_method_diagnostics_report_too_many_arguments_after_constructor_patching() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let import_uri = Url::parse("file:///models.fdn").expect("import uri");
            let import_text = r#"object StorageManager {
    action addTask with (certain name oftype string) {}
}
"#;
            backend
                .store
                .insert(import_uri.clone(), background_doc(&import_uri, import_text));

            let uri = Url::parse("file:///main.fdn").expect("main uri");
            let text = r#"action main {
    var storage = StorageManager()
    storage.addTask("x", 1)
}
"#;
            let analysis = analysis::analyze(text, uri.as_str());
            let mut doc = Document {
                version: 1,
                text: text.to_string(),
                diagnostics: analysis.diagnostics.clone(),
                semantic_tokens: analysis.semantic_tokens,
                symbol_table: analysis.symbol_table,
                identifier_spans: analysis.identifier_spans,
                imports: HashMap::new(),
                direct_imports: HashMap::from([(
                    "StorageManager".to_string(),
                    (import_uri.clone(), "StorageManager".to_string()),
                )]),
                wildcard_imports: vec![],
                stdlib_imports: HashMap::new(),
                stdlib_direct_imports: HashMap::new(),
                stdlib_call_sites: analysis.stdlib_call_sites.clone(),
                inlay_hint_sites: analysis.inlay_hint_sites,
                member_access_sites: analysis.member_access_sites,
            };

            for site in &analysis.imported_constructor_call_sites {
                let ret_type = imported_constructor_type_name(
                    &backend.store,
                    &doc.imports,
                    &doc.direct_imports,
                    site,
                )
                .expect("imported constructor type");
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, &ret_type, true);
            }

            let diags = backend.check_receiver_chain_method_diagnostics(
                text,
                &uri,
                &doc.symbol_table,
                &analysis.receiver_chain_method_call_sites,
            );

            assert!(diags.iter().any(|diag| {
                matches!(diag.code, Some(NumberOrString::String(ref code)) if code == "E0305")
                    && diag.message.contains("expected 1 argument, got 2")
            }));
        });
    }

    #[test]
    fn receiver_chain_method_diagnostics_accept_builtin_aliases() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///hashset_alias_diag.fdn").expect("main uri");
            let text = r#"object StorageManager {
    var tasks oftype hashset oftype string = hashset()

    action addTask with (certain name oftype string) {
        this.tasks.add(name)
    }
}
"#;
            let analysis = analysis::analyze(text, uri.as_str());

            backend.store.insert(
                uri.clone(),
                Document {
                    version: 1,
                    text: text.to_string(),
                    diagnostics: analysis.diagnostics.clone(),
                    semantic_tokens: analysis.semantic_tokens,
                    symbol_table: analysis.symbol_table.clone(),
                    identifier_spans: analysis.identifier_spans,
                    imports: HashMap::new(),
                    direct_imports: HashMap::new(),
                    wildcard_imports: vec![],
                    stdlib_imports: HashMap::new(),
                    stdlib_direct_imports: HashMap::new(),
                    stdlib_call_sites: analysis.stdlib_call_sites,
                    inlay_hint_sites: analysis.inlay_hint_sites,
                    member_access_sites: analysis.member_access_sites,
                },
            );

            assert!(backend.resolve_member_cross_doc("hashset", "add").is_some());

            let diags = backend.check_receiver_chain_method_diagnostics(
                text,
                &uri,
                &analysis.symbol_table,
                &analysis.receiver_chain_method_call_sites,
            );

            assert!(diags.iter().all(|diag| {
                !matches!(diag.code, Some(NumberOrString::String(ref code)) if code == "E0204" || code == "E0301" || code == "E0302" || code == "E0305")
            }));
        });
    }

    #[test]
    fn receiver_chain_method_diagnostics_report_missing_args_for_builtin_aliases() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///hashset_alias_missing_arg_diag.fdn").expect("main uri");
            let text = r#"object StorageManager {
    var tasks oftype hashset oftype string = hashset()

    action addTask {
        this.tasks.add()
    }
}
"#;
            let analysis = analysis::analyze(text, uri.as_str());

            backend.store.insert(
                uri.clone(),
                Document {
                    version: 1,
                    text: text.to_string(),
                    diagnostics: analysis.diagnostics.clone(),
                    semantic_tokens: analysis.semantic_tokens,
                    symbol_table: analysis.symbol_table.clone(),
                    identifier_spans: analysis.identifier_spans,
                    imports: HashMap::new(),
                    direct_imports: HashMap::new(),
                    wildcard_imports: vec![],
                    stdlib_imports: HashMap::new(),
                    stdlib_direct_imports: HashMap::new(),
                    stdlib_call_sites: analysis.stdlib_call_sites,
                    inlay_hint_sites: analysis.inlay_hint_sites,
                    member_access_sites: analysis.member_access_sites,
                },
            );

            let diags = backend.check_receiver_chain_method_diagnostics(
                text,
                &uri,
                &analysis.symbol_table,
                &analysis.receiver_chain_method_call_sites,
            );

            assert!(diags.iter().any(|diag| {
                matches!(diag.code, Some(NumberOrString::String(ref code)) if code == "E0301")
                    && diag
                        .message
                        .contains("not enough arguments for `add`: 1 required but 0 provided")
            }));
            assert!(diags.iter().all(|diag| {
                !matches!(diag.code, Some(NumberOrString::String(ref code)) if code == "E0305")
            }));
        });
    }

    #[test]
    fn import_binding_definition_redirects_namespace_and_direct_imports() {
        let namespace_uri = Url::parse("file:///utils_lib.fdn").expect("namespace uri");
        let direct_uri = Url::parse("file:///utils_flat.fdn").expect("direct uri");
        let span = Span::new(FileId(0), 0, 13);

        let namespace = import_binding_definition(
            "use utils_lib\nprint(utils_lib.add_ints(1, 2))\n",
            span,
            "utils_lib",
            &HashMap::from([("utils_lib".to_string(), namespace_uri.clone())]),
            &HashMap::new(),
        );
        assert!(matches!(
            namespace,
            Some(ImportBindingDefinition::OpenFile(url)) if url == namespace_uri
        ));

        let direct = import_binding_definition(
            "use utils_flat.{sub_ints}\nprint(sub_ints(1, 2))\n",
            span,
            "sub_ints",
            &HashMap::new(),
            &HashMap::from([(
                "sub_ints".to_string(),
                (direct_uri.clone(), "sub_ints".to_string()),
            )]),
        );
        assert!(matches!(
            direct,
            Some(ImportBindingDefinition::ImportDoc(url, name))
                if url == direct_uri && name == "sub_ints"
        ));
    }

    #[test]
    fn resolve_document_imports_tracks_user_module_namespaces_and_direct_bindings() {
        let current_path = import_test_path("import_test.fdn");
        let analysis = analysis::analyze(
            "use utils_lib\nuse utils_flat.{sub_ints}\n",
            "file:///import_resolution_test.fdn",
        );

        let resolved = resolve_document_imports(
            Some(&current_path),
            &analysis.imports,
            &analysis.user_module_imports,
        );

        assert!(resolved.namespace_imports.contains_key("utils_lib"));
        assert_eq!(
            resolved
                .namespace_imports
                .get("utils_lib")
                .and_then(|url| url.to_file_path().ok())
                .as_deref(),
            Some(import_test_path("utils_lib.fdn").as_path())
        );

        let (direct_url, direct_name) = resolved
            .direct_imports
            .get("sub_ints")
            .expect("expected grouped import binding");
        assert_eq!(direct_name, "sub_ints");
        assert_eq!(
            direct_url.to_file_path().ok().as_deref(),
            Some(import_test_path("utils_flat.fdn").as_path())
        );
    }

    #[test]
    fn resolve_user_module_import_url_supports_init_modules() {
        let root = temp_test_dir("import-init");
        let current_path = root.join("main.fdn");
        let nested_dir = root.join("pkg");
        std::fs::create_dir_all(&nested_dir).expect("create nested module dir");
        let init_path = nested_dir.join("init.fdn");
        std::fs::write(
            &init_path,
            "action greet returns string { return \"hi\" }\n",
        )
        .expect("write init module");

        let segments = vec!["pkg".to_string()];
        let resolved = resolve_user_module_import_url(Some(&current_path), &segments, false)
            .and_then(|url| url.to_file_path().ok());

        assert_eq!(resolved.as_deref(), Some(init_path.as_path()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_user_module_import_url_does_not_fallback_non_grouped_to_parent_module() {
        let root = temp_test_dir("import-non-grouped");
        let current_path = root.join("main.fdn");
        let parent_module = root.join("pkg.fdn");
        std::fs::write(
            &parent_module,
            "action greet returns string { return \"hi\" }\n",
        )
        .expect("write parent module");

        let grouped_segments = vec!["pkg".to_string(), "member".to_string()];
        let grouped = resolve_user_module_import_url(Some(&current_path), &grouped_segments, true)
            .and_then(|url| url.to_file_path().ok());
        assert_eq!(grouped.as_deref(), Some(parent_module.as_path()));

        let expected_nested_module = root.join("pkg").join("member.fdn");
        let non_grouped =
            resolve_user_module_import_url(Some(&current_path), &grouped_segments, false)
                .and_then(|url| url.to_file_path().ok());
        assert_eq!(
            non_grouped.as_deref(),
            Some(expected_nested_module.as_path())
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_path_url_at_offset_resolves_string_import_targets() {
        let text = "use \"./utils.fdn\"\n";
        let cursor = text.find("utils.fdn").expect("import path cursor") + 2;
        let uri = path_url(&import_test_path("import_test.fdn"));

        let resolved = import_path_url_at_offset(text, &uri, cursor)
            .and_then(|url| url.to_file_path().ok())
            .expect("expected import path target");

        assert_eq!(resolved, import_test_path("utils.fdn"));
    }

    #[test]
    fn goto_definition_resolves_wildcard_file_imported_symbols() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let import_test = import_test_path("import_test.fdn");
            let uri = path_url(&import_test);
            let text = std::fs::read_to_string(&import_test).expect("import_test source");
            backend.refresh(&uri, 1, &text).await;

            let doc = backend.store.get(&uri).expect("refreshed document");
            let wildcard_imports = doc.wildcard_imports.clone();
            let identifier_spans = doc.identifier_spans.clone();
            drop(doc);
            assert!(
                wildcard_import_entry(&backend.store, &wildcard_imports, "greet").is_some(),
                "wildcard import lookup should resolve greet before goto-definition",
            );
            let greet_span = identifier_spans
                .iter()
                .find(|(_, name)| name == "greet")
                .map(|(span, _)| *span)
                .expect("greet token span");

            let file = SourceFile::new(FileId(0), uri.as_str(), text.as_str());
            let pos = convert::span_to_range(&file, greet_span).start;
            assert_eq!(
                lsp_pos_to_offset(&file, &pos),
                greet_span.start,
                "expected goto position to round-trip to the greet token start",
            );

            let response = LanguageServer::goto_definition(
                backend,
                GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await
            .expect("goto definition result")
            .expect("definition location");

            let GotoDefinitionResponse::Scalar(location) = response else {
                panic!("expected scalar definition location");
            };

            assert_eq!(location.uri, path_url(&import_test_path("utils.fdn")));
            let target_text =
                std::fs::read_to_string(import_test_path("utils.fdn")).expect("utils source");
            let target_file =
                SourceFile::new(FileId(0), location.uri.as_str(), target_text.as_str());
            let target_offset = lsp_pos_to_offset(&target_file, &location.range.start) as usize;
            let target_line = target_text[target_offset..]
                .lines()
                .next()
                .expect("target line");
            assert!(target_line.starts_with("action greet"));
        });
    }

    #[test]
    fn goto_definition_opens_grouped_user_module_import_paths() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let root = temp_test_dir("goto-grouped-user-module");
            let main_path = root.join("main.fdn");
            let imported_path = root.join("test_file_manager.fdn");
            std::fs::write(&imported_path, "object StorageManager {}\n")
                .expect("write imported module");

            let text = "use test_file_manager.{StorageManager}\n";
            std::fs::write(&main_path, text).expect("write main module");

            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();
            let uri = path_url(&main_path);
            backend.refresh(&uri, 1, text).await;

            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("test_file_manager").expect("module offset") as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let response = LanguageServer::goto_definition(
                backend,
                GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await
            .expect("goto definition result")
            .expect("definition location");

            let GotoDefinitionResponse::Scalar(location) = response else {
                panic!("expected scalar definition location");
            };
            assert_eq!(location.uri, path_url(&imported_path));

            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn goto_definition_opens_direct_user_module_import_paths() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let root = temp_test_dir("goto-direct-user-module");
            let main_path = root.join("main.fdn");
            let imported_path = root.join("test_file_manager.fdn");
            std::fs::write(&imported_path, "object StorageManager {}\n")
                .expect("write imported module");

            let text = "use test_file_manager\n";
            std::fs::write(&main_path, text).expect("write main module");

            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();
            let uri = path_url(&main_path);
            backend.refresh(&uri, 1, text).await;

            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("test_file_manager").expect("module offset") as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let response = LanguageServer::goto_definition(
                backend,
                GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await
            .expect("goto definition result")
            .expect("definition location");

            let GotoDefinitionResponse::Scalar(location) = response else {
                panic!("expected scalar definition location");
            };
            assert_eq!(location.uri, path_url(&imported_path));

            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn goto_definition_opens_string_import_paths() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let root = temp_test_dir("goto-string-import");
            let main_path = root.join("main.fdn");
            let imported_path = root.join("test_file_manager.fdn");
            std::fs::write(&imported_path, "object StorageManager {}\n")
                .expect("write imported module");

            let text = "use \"test_file_manager.fdn\"\n";
            std::fs::write(&main_path, text).expect("write main module");

            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();
            let uri = path_url(&main_path);
            backend.refresh(&uri, 1, text).await;

            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = (text
                .find("test_file_manager.fdn")
                .expect("import path offset")
                + 2) as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let response = LanguageServer::goto_definition(
                backend,
                GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await
            .expect("goto definition result")
            .expect("definition location");

            let GotoDefinitionResponse::Scalar(location) = response else {
                panic!("expected scalar definition location");
            };
            assert_eq!(location.uri, path_url(&imported_path));

            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn goto_definition_does_not_resolve_stdlib_module_import_paths() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///goto_stdlib_import.fdn").expect("main uri");
            let text = "use std.io\n";
            backend.refresh(&uri, 1, text).await;

            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("io").expect("stdlib module offset") as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let response = LanguageServer::goto_definition(
                backend,
                GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await
            .expect("goto definition result");

            assert!(
                response.is_none(),
                "stdlib imports should not resolve to files"
            );
        });
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
    fn dot_receiver_type_name_supports_builtin_literal_receivers() {
        let text = r#""hello".
    123.
    12.5.
    true.
    nothing.
    "#;
        let analysis = analysis::analyze(text, "file:///literal_receiver_types.fdn");

        let string_offset =
            text.find("\"hello\".").expect("string cursor") as u32 + "\"hello\"".len() as u32;
        let integer_offset = text.find("123.").expect("integer cursor") as u32 + 3;
        let float_offset = text.find("12.5.").expect("float cursor") as u32 + 4;
        let boolean_offset = text.find("true.").expect("boolean cursor") as u32 + 4;
        let nothing_offset = text.find("nothing.").expect("nothing cursor") as u32 + 7;

        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                string_offset,
            )
            .as_deref(),
            Some("string")
        );
        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                integer_offset,
            )
            .as_deref(),
            Some("integer")
        );
        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                float_offset,
            )
            .as_deref(),
            Some("float")
        );
        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                boolean_offset,
            )
            .as_deref(),
            Some("boolean")
        );
        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                nothing_offset,
            )
            .as_deref(),
            Some("nothing")
        );
    }

    #[test]
    fn dot_receiver_type_name_supports_builtin_call_results() {
        let text = r#"action main {
    input().
}
"#;
        let analysis = analysis::analyze(text, "file:///call_receiver_types.fdn");
        let dot_offset =
            text.find("input().").expect("call cursor") as u32 + "input()".len() as u32;

        assert_eq!(
            resolve_dotted_receiver_type_name(
                &analysis.symbol_table,
                &analysis.identifier_spans,
                text,
                dot_offset,
            )
            .as_deref(),
            Some("string")
        );
    }

    #[test]
    fn member_access_site_lookup_resolves_builtin_literal_methods() {
        let text = r#"var uppercased = "hello".upper()"#;
        let analysis = analysis::analyze(text, "file:///literal_member_hover.fdn");
        let offset = text.find(".upper").expect("upper cursor") as u32 + 1;

        let site = member_access_site_at_offset(&analysis.member_access_sites, offset)
            .expect("expected typed member-access site for upper");
        assert_eq!(site.receiver_type, "string");
        assert_eq!(site.member_name, "upper");

        let entry = analysis
            .symbol_table
            .get("string.upper")
            .expect("expected builtin string.upper symbol entry");
        assert!(entry.detail.contains("string.upper() -> string"));
    }

    #[test]
    fn builtin_hover_docs_cover_functions_and_type_like_values() {
        let len = builtin_hover_markdown("len").expect("missing len hover doc");
        assert!(len.contains("len(value) -> integer"));

        let integer = builtin_hover_markdown("integer").expect("missing integer hover doc");
        assert!(integer.contains("integer(value) -> integer"));

        let handle = builtin_hover_markdown("handle").expect("missing handle hover doc");
        assert!(handle.contains("```fidan\nhandle\n```"));
        assert!(handle.contains("Opaque native handle type"));

        let hashset = builtin_hover_markdown("hashset").expect("missing hashset hover doc");
        assert!(hashset.contains("hashset(items?) -> hashset"));
    }

    #[test]
    fn doc_comment_text_is_collected_from_adjacent_hash_gt_lines() {
        let text = "#> First line\n#> Second line\naction greet returns string {}\n";
        let span_start = text.find("action greet").expect("action span") as u32;
        let doc = doc_comment_text_for_span(text, Span::new(FileId(0), span_start, span_start + 6))
            .expect("doc comment");

        assert_eq!(doc, "First line\nSecond line");
    }

    #[test]
    fn leading_doc_comment_text_is_collected_from_start_of_file() {
        let text = "\n#> First line\n#> Second line\n\nuse std.json\n";
        let doc = leading_doc_comment_text(text).expect("module doc comment");

        assert_eq!(doc, "First line\nSecond line");
    }

    #[test]
    fn doc_comment_markdown_preserves_multiple_lines_for_hover_rendering() {
        let rendered = doc_comment_markdown("First line\nSecond line");

        assert_eq!(rendered, "First line  \nSecond line");
    }

    #[test]
    fn editor_format_defaults_are_loaded_from_initialize_options() {
        let params = InitializeParams {
            initialization_options: Some(json!({
                "indentWidth": 2,
                "maxLineLen": 72,
            })),
            ..InitializeParams::default()
        };

        assert_eq!(
            editor_format_defaults_from_init(&params),
            EditorFormatDefaults {
                indent_width: 2,
                max_line_len: 72,
            }
        );
    }

    #[test]
    fn fallback_format_options_use_editor_max_line_len() {
        let defaults = EditorFormatDefaults {
            indent_width: 4,
            max_line_len: 68,
        };
        let request = FormattingOptions {
            tab_size: 2,
            insert_spaces: true,
            properties: Default::default(),
            trim_trailing_whitespace: None,
            insert_final_newline: None,
            trim_final_newlines: None,
        };

        let opts = fallback_format_options(&request, defaults);
        assert_eq!(opts.indent_width, 2);
        assert_eq!(opts.max_line_len, 68);
    }

    #[test]
    fn stdlib_member_hover_docs_cover_recent_exports() {
        let sleep = stdlib_member_hover_markdown("time", "sleep", None)
            .expect("missing std.time.sleep doc");
        assert!(sleep.contains("std.time.sleep(ms oftype integer) -> nothing"));

        let wait_any = stdlib_member_hover_markdown("async", "waitAny", None)
            .expect("missing std.async.waitAny doc");
        assert!(
            wait_any.contains(
                "std.async.waitAny(handles oftype list oftype Pending oftype dynamic) -> Pending oftype (integer, dynamic)"
            )
        );

        let parse = stdlib_member_hover_markdown("json", "parse", None)
            .expect("missing std.json.parse doc");
        assert!(
            parse.contains("std.json.parse(text oftype string, soft oftype boolean?) -> dynamic")
        );
    }

    #[test]
    fn stdlib_member_hover_uses_precise_callsite_return_types() {
        let max = stdlib_member_hover_markdown(
            "math",
            "max",
            Some(&["integer".to_string(), "float".to_string()]),
        )
        .expect("missing std.math.max doc");
        assert!(max.contains("std.math.max(a oftype float, b oftype float) -> float"));

        let abs = stdlib_member_hover_markdown("math", "abs", Some(&["integer".to_string()]))
            .expect("missing std.math.abs doc");
        assert!(abs.contains("std.math.abs(x oftype float) -> integer"));
    }

    #[test]
    fn hover_resolves_grouped_stdlib_imports() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///stdlib_grouped_hover.fdn").expect("document uri");
            let text = "use std.collections.{enumerate}\n\naction main {\n    var rows = enumerate([10, 20])\n}\n";
            backend.refresh(&uri, 1, text).await;

            let offset = text
                .find("enumerate([10, 20])")
                .expect("call site offset") as u32;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup.value.contains(
                "std.collections.enumerate(list oftype list oftype dynamic)"
            ));
        });
    }

    #[test]
    fn hover_resolves_stdlib_namespace_member_functions() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri =
                Url::parse("file:///stdlib_namespace_member_hover.fdn").expect("document uri");
            let text =
                "use std.json\n\naction main {\n    return json.dump({}, \"tasks.json\")\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("dump").expect("dump offset") as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(
                markup
                    .value
                    .contains("std.json.dump(value oftype dynamic, path oftype string) -> boolean")
            );
        });
    }

    #[test]
    fn hover_resolves_user_module_doc_comments_on_grouped_import_paths() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let main_uri = path_url(&import_test_path("main_grouped_module_hover.fdn"));
            let imported_uri = path_url(&import_test_path("test_file_manager.fdn"));
            backend.store.insert(
                imported_uri.clone(),
                background_doc(
                    &imported_uri,
                    "#> Some doccomment\n#> Blablabla\n\nobject StorageManager {}\n",
                ),
            );

            let text = "use test_file_manager.{StorageManager}\n";
            backend.refresh(&main_uri, 1, text).await;
            let file = SourceFile::new(FileId(0), main_uri.as_str(), text);
            let offset = text.find("test_file_manager").expect("module offset") as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: main_uri.clone(),
                        },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup.value.contains("use test_file_manager"));
            assert!(markup.value.contains("Some doccomment  \nBlablabla"));
        });
    }

    #[test]
    fn hover_refreshes_grouped_module_doc_comments_after_disk_changes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let root = temp_test_dir("hover-grouped-module-doc-refresh");
            let main_path = root.join("main.fdn");
            let imported_path = root.join("test_file_manager.fdn");
            let main_text = "use test_file_manager.{StorageManager}\n";

            std::fs::write(&main_path, main_text).expect("write main module");
            std::fs::write(
                &imported_path,
                "#> Initial module docs\n\nobject StorageManager {}\n",
            )
            .expect("write imported module");

            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();
            let main_uri = path_url(&main_path);
            backend.refresh(&main_uri, 1, main_text).await;

            let file = SourceFile::new(FileId(0), main_uri.as_str(), main_text);
            let offset = main_text.find("test_file_manager").expect("module offset") as u32;
            let pos = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let initial = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: main_uri.clone(),
                        },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("initial hover result")
            .expect("initial hover contents");
            let HoverContents::Markup(initial_markup) = initial.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(initial_markup.value.contains("Initial module docs"));

            std::fs::write(
                &imported_path,
                "#> Updated module docs\n\nobject StorageManager {}\n",
            )
            .expect("rewrite imported module with updated docs");

            let updated = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: main_uri.clone(),
                        },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("updated hover result")
            .expect("updated hover contents");
            let HoverContents::Markup(updated_markup) = updated.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(updated_markup.value.contains("Updated module docs"));
            assert!(!updated_markup.value.contains("Initial module docs"));

            std::fs::write(&imported_path, "object StorageManager {}\n")
                .expect("rewrite imported module without docs");

            let removed = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: main_uri.clone(),
                        },
                        position: pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("removed hover result")
            .expect("removed hover contents");
            let HoverContents::Markup(removed_markup) = removed.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(!removed_markup.value.contains("Updated module docs"));

            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn completion_inserts_only_stdlib_module_suffix_inside_std_imports() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///stdlib_completion.fdn").expect("document uri");
            let text = "use std.coll\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset =
                text.find("std.coll").expect("import offset") as u32 + "std.coll".len() as u32;
            let position =
                convert::span_to_range(&file, Span::new(FileId(0), offset, offset)).start;

            let response = LanguageServer::completion(
                backend,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(CompletionContext {
                        trigger_kind: CompletionTriggerKind::INVOKED,
                        trigger_character: None,
                    }),
                },
            )
            .await
            .expect("completion result")
            .expect("completion items");

            let CompletionResponse::Array(items) = response else {
                panic!("expected completion array");
            };

            let item = items
                .into_iter()
                .find(|item| item.label == "std.collections")
                .expect("stdlib module completion");
            assert_eq!(item.insert_text.as_deref(), Some("collections"));
        });
    }

    #[test]
    fn completion_resolves_string_members_for_call_results_on_manual_invocation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///call_result_completion.fdn").expect("document uri");
            let text = "action main {\n    var value = input().trim()\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("trim").expect("trim offset") as u32 + 1;
            let position =
                convert::span_to_range(&file, Span::new(FileId(0), offset, offset)).start;

            let response = LanguageServer::completion(
                backend,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(CompletionContext {
                        trigger_kind: CompletionTriggerKind::INVOKED,
                        trigger_character: None,
                    }),
                },
            )
            .await
            .expect("completion result")
            .expect("completion items");

            let CompletionResponse::Array(items) = response else {
                panic!("expected completion array");
            };

            let labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
            assert!(labels.contains(&"trim".to_string()));
            assert!(labels.contains(&"trimStart".to_string()));
        });
    }

    #[test]
    fn hover_resolves_container_type_annotations() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///type_hover.fdn").expect("document uri");
            let text = "action main {\n    var tasks oftype list oftype string = []\n    var tags oftype hashset oftype string = hashset()\n    var lookup oftype map oftype (string, integer) = {}\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);

            let list_offset = text.find("list oftype string").expect("list offset") as u32;
            let list_pos = convert::span_to_range(
                &file,
                Span::new(FileId(0), list_offset, list_offset + 1),
            )
            .start;
            let list_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: list_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("list hover result")
            .expect("list hover contents");

            let HoverContents::Markup(list_markup) = list_hover.contents else {
                panic!("expected markdown hover contents for list");
            };
            assert!(list_markup.value.contains("list oftype T"));

            let map_offset = text.find("map oftype").expect("map offset") as u32;
            let map_pos = convert::span_to_range(
                &file,
                Span::new(FileId(0), map_offset, map_offset + 1),
            )
            .start;
            let map_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: map_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("map hover result")
            .expect("map hover contents");

            let HoverContents::Markup(map_markup) = map_hover.contents else {
                panic!("expected markdown hover contents for map");
            };
            assert!(map_markup.value.contains("map oftype (K, V)"));

            let hashset_offset = text.find("hashset oftype string").expect("hashset offset") as u32;
            let hashset_pos = convert::span_to_range(
                &file,
                Span::new(FileId(0), hashset_offset, hashset_offset + 1),
            )
            .start;
            let hashset_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: hashset_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hashset hover result")
            .expect("hashset hover contents");

            let HoverContents::Markup(hashset_markup) = hashset_hover.contents else {
                panic!("expected markdown hover contents for hashset");
            };
            assert!(hashset_markup.value.contains("hashset oftype T"));
        });
    }

    #[test]
    fn completion_resolves_hashset_members_for_typed_receivers() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///hashset_receiver_completion.fdn").expect("document uri");
            let text = "action main {\n    var tags oftype hashset oftype string = hashset()\n    tags.\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("tags.").expect("tags dot offset") as u32 + "tags.".len() as u32;
            let position = convert::span_to_range(&file, Span::new(FileId(0), offset, offset)).start;

            let response = LanguageServer::completion(
                backend,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(CompletionContext {
                        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                        trigger_character: Some(".".to_string()),
                    }),
                },
            )
            .await
            .expect("completion result")
            .expect("completion items");

            let CompletionResponse::Array(items) = response else {
                panic!("expected completion array");
            };

            let contains = items
                .iter()
                .find(|item| item.label == "contains")
                .expect("hashset contains completion");
            let documentation = contains.documentation.as_ref().expect("contains doc");
            let Documentation::MarkupContent(markup) = documentation else {
                panic!("expected markdown documentation for hashset member");
            };
            assert!(markup
                .value
                .contains("hashset.contains(value oftype string) -> boolean"));
            assert!(items.iter().any(|item| item.label == "insert"));
            assert!(items.iter().any(|item| item.label == "isEmpty"));
        });
    }

    #[test]
    fn hover_reports_typed_receiver_method_signatures_for_builtin_collections() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///receiver_signature_hover.fdn").expect("document uri");
            let text = "action main {\n    var tasks oftype hashset oftype string = hashset()\n    var values oftype list oftype integer = []\n    var lookup oftype dict oftype (string, integer) = {}\n    tasks.contains\n    values.remove\n    lookup.get\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);

            for (needle, expected) in [
                (
                    "contains",
                    "hashset.contains(value oftype string) -> boolean",
                ),
                (
                    "remove",
                    "list.remove(index oftype integer) -> integer",
                ),
                (
                    "get",
                    "dict.get(key oftype string) -> integer",
                ),
            ] {
                let offset = text.find(needle).expect("member offset") as u32;
                let position = convert::span_to_range(
                    &file,
                    Span::new(FileId(0), offset, offset + 1),
                )
                .start;

                let hover = LanguageServer::hover(
                    backend,
                    HoverParams {
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: uri.clone() },
                            position,
                        },
                        work_done_progress_params: Default::default(),
                    },
                )
                .await
                .expect("hover result")
                .expect("hover contents");

                let HoverContents::Markup(markup) = hover.contents else {
                    panic!("expected markdown hover contents");
                };
                assert!(markup.value.contains(expected), "hover was: {}", markup.value);
            }
        });
    }

    #[test]
    fn completion_resolves_hashset_members_for_dot_trigger_before_doc_updates() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///hashset_trigger_completion.fdn").expect("document uri");
            let text = "action main {\n    var tags oftype hashset oftype string = hashset()\n    tags\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("tags\n").expect("tags offset") as u32 + "tags".len() as u32;
            let position = convert::span_to_range(&file, Span::new(FileId(0), offset, offset)).start;

            let response = LanguageServer::completion(
                backend,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(CompletionContext {
                        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                        trigger_character: Some(".".to_string()),
                    }),
                },
            )
            .await
            .expect("completion result")
            .expect("completion items");

            let CompletionResponse::Array(items) = response else {
                panic!("expected completion array");
            };

            assert!(items.iter().any(|item| item.label == "contains"));
            assert!(items.iter().any(|item| item.label == "insert"));
            assert!(items.iter().all(|item| item.label != "FILE_PATH"));
        });
    }

    #[test]
    fn completion_resolves_field_chain_members_during_trailing_dot_parse_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///hashset_trailing_dot_parse_error.fdn").expect("document uri");
            let text = "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n\n    action loadData returns boolean {\n        if not this.tasks. {\n            return true\n        }\n        return false\n    }\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("this.tasks.").expect("this.tasks offset") as u32
                + "this.tasks.".len() as u32;
            let position = convert::span_to_range(&file, Span::new(FileId(0), offset, offset)).start;

            let response = LanguageServer::completion(
                backend,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(CompletionContext {
                        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                        trigger_character: Some(".".to_string()),
                    }),
                },
            )
            .await
            .expect("completion result")
            .expect("completion items");

            let CompletionResponse::Array(items) = response else {
                panic!("expected completion array");
            };

            assert!(items.iter().any(|item| item.label == "contains"));
            assert!(items.iter().any(|item| item.label == "isEmpty"));
            assert!(items.iter().all(|item| item.label != "FILE_PATH"));
        });
    }

    #[test]
    fn completion_resolves_hashset_members_through_imported_object_fields() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let storage_uri = Url::parse("file:///storage_manager.fdn").expect("storage uri");
            let storage_text = "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n}\n";
            backend
                .store
                .insert(storage_uri.clone(), background_doc(&storage_uri, storage_text));

            let main_uri = Url::parse("file:///storage_main.fdn").expect("main uri");
            let main_text = "use storage_manager as storage_mod\n\naction main {\n    var storage = storage_mod.StorageManager()\n    storage.tasks.\n}\n";
            let analysis = analysis::analyze(main_text, main_uri.as_str());
            let mut doc = Document {
                version: 1,
                text: main_text.to_string(),
                diagnostics: analysis.diagnostics.clone(),
                semantic_tokens: analysis.semantic_tokens,
                symbol_table: analysis.symbol_table,
                identifier_spans: analysis.identifier_spans,
                imports: HashMap::from([("storage_mod".to_string(), storage_uri.clone())]),
                direct_imports: HashMap::new(),
                wildcard_imports: vec![],
                stdlib_imports: HashMap::new(),
                stdlib_direct_imports: HashMap::new(),
                stdlib_call_sites: analysis.stdlib_call_sites.clone(),
                inlay_hint_sites: analysis.inlay_hint_sites,
                member_access_sites: analysis.member_access_sites,
            };

            for site in &analysis.imported_constructor_call_sites {
                let ret_type = imported_constructor_type_name(
                    &backend.store,
                    &doc.imports,
                    &doc.direct_imports,
                    site,
                )
                .expect("imported constructor type");
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, &ret_type, true);
            }

            backend.store.insert(main_uri.clone(), doc);

            let file = SourceFile::new(FileId(0), main_uri.as_str(), main_text);
            let offset = main_text.find("storage.tasks.").expect("storage.tasks offset") as u32
                + "storage.tasks.".len() as u32;
            let position = convert::span_to_range(&file, Span::new(FileId(0), offset, offset)).start;

            let response = LanguageServer::completion(
                backend,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: main_uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(CompletionContext {
                        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                        trigger_character: Some(".".to_string()),
                    }),
                },
            )
            .await
            .expect("completion result")
            .expect("completion items");

            let CompletionResponse::Array(items) = response else {
                panic!("expected completion array");
            };

            assert!(items.iter().any(|item| item.label == "contains"));
            assert!(items.iter().any(|item| item.label == "insert"));
        });
    }

    #[test]
    fn hover_resolves_hashset_members_through_local_object_fields() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///hashset_nested_hover.fdn").expect("document uri");
            let text = "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n}\n\naction main {\n    var storage = StorageManager()\n    storage.tasks.isEmpty()\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("isEmpty").expect("isEmpty offset") as u32;
            let position =
                convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup
                .value
                .contains("hashset.isEmpty() -> boolean"));
        });
    }

    #[test]
    fn hover_resolves_object_fields_through_local_object_fields() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///field_nested_hover.fdn").expect("document uri");
            let text = "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n}\n\naction main {\n    var storage = StorageManager()\n    storage.tasks.isEmpty()\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.find("tasks.isEmpty").expect("tasks offset") as u32;
            let position =
                convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup.value.contains("StorageManager.tasks: hashset oftype string"));
        });
    }

    #[test]
    fn hover_resolves_hashset_members_through_imported_object_fields() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let storage_uri = Url::parse("file:///storage_manager_hover.fdn").expect("storage uri");
            let storage_text =
                "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n}\n";
            backend
                .store
                .insert(storage_uri.clone(), background_doc(&storage_uri, storage_text));

            let main_uri = Url::parse("file:///storage_main_hover.fdn").expect("main uri");
            let main_text = "use storage_manager_hover as storage_mod\n\naction main {\n    var storage = storage_mod.StorageManager()\n    storage.tasks.isEmpty()\n}\n";
            let analysis = analysis::analyze(main_text, main_uri.as_str());
            let mut doc = Document {
                version: 1,
                text: main_text.to_string(),
                diagnostics: analysis.diagnostics.clone(),
                semantic_tokens: analysis.semantic_tokens,
                symbol_table: analysis.symbol_table,
                identifier_spans: analysis.identifier_spans,
                imports: HashMap::from([("storage_mod".to_string(), storage_uri.clone())]),
                direct_imports: HashMap::new(),
                wildcard_imports: vec![],
                stdlib_imports: HashMap::new(),
                stdlib_direct_imports: HashMap::new(),
                stdlib_call_sites: analysis.stdlib_call_sites.clone(),
                inlay_hint_sites: analysis.inlay_hint_sites,
                member_access_sites: analysis.member_access_sites,
            };

            for site in &analysis.imported_constructor_call_sites {
                let ret_type = imported_constructor_type_name(
                    &backend.store,
                    &doc.imports,
                    &doc.direct_imports,
                    site,
                )
                .expect("imported constructor type");
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, &ret_type, true);
            }

            backend.store.insert(main_uri.clone(), doc);

            let file = SourceFile::new(FileId(0), main_uri.as_str(), main_text);
            let offset = main_text.find("isEmpty").expect("isEmpty offset") as u32;
            let position =
                convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: main_uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup
                .value
                .contains("hashset.isEmpty() -> boolean"));
        });
    }

    #[test]
    fn hover_resolves_object_fields_through_imported_object_fields() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let storage_uri =
                Url::parse("file:///storage_manager_field_hover.fdn").expect("storage uri");
            let storage_text =
                "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n}\n";
            backend
                .store
                .insert(storage_uri.clone(), background_doc(&storage_uri, storage_text));

            let main_uri = Url::parse("file:///storage_main_field_hover.fdn").expect("main uri");
            let main_text = "use storage_manager_field_hover as storage_mod\n\naction main {\n    var storage = storage_mod.StorageManager()\n    storage.tasks.isEmpty()\n}\n";
            let analysis = analysis::analyze(main_text, main_uri.as_str());
            let mut doc = Document {
                version: 1,
                text: main_text.to_string(),
                diagnostics: analysis.diagnostics.clone(),
                semantic_tokens: analysis.semantic_tokens,
                symbol_table: analysis.symbol_table,
                identifier_spans: analysis.identifier_spans,
                imports: HashMap::from([("storage_mod".to_string(), storage_uri.clone())]),
                direct_imports: HashMap::new(),
                wildcard_imports: vec![],
                stdlib_imports: HashMap::new(),
                stdlib_direct_imports: HashMap::new(),
                stdlib_call_sites: analysis.stdlib_call_sites.clone(),
                inlay_hint_sites: analysis.inlay_hint_sites,
                member_access_sites: analysis.member_access_sites,
            };

            for site in &analysis.imported_constructor_call_sites {
                let ret_type = imported_constructor_type_name(
                    &backend.store,
                    &doc.imports,
                    &doc.direct_imports,
                    site,
                )
                .expect("imported constructor type");
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, &ret_type, true);
            }

            backend.store.insert(main_uri.clone(), doc);

            let file = SourceFile::new(FileId(0), main_uri.as_str(), main_text);
            let offset = main_text.find("tasks.isEmpty").expect("tasks offset") as u32;
            let position =
                convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: main_uri.clone(),
                        },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup.value.contains("StorageManager.tasks: hashset oftype string"));
        });
    }

    #[test]
    fn imported_object_field_hashset_method_typos_report_diagnostics_recursively() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let storage_uri = Url::parse("file:///storage_manager_diag.fdn").expect("storage uri");
            let storage_text = "object StorageManager {\n    var tasks oftype hashset oftype string = hashset()\n}\n";
            backend
                .store
                .insert(storage_uri.clone(), background_doc(&storage_uri, storage_text));

            let main_uri = Url::parse("file:///storage_diag_main.fdn").expect("main uri");
            let main_text = "use storage_manager_diag as storage_mod\n\naction main {\n    var storage = storage_mod.StorageManager()\n    for task in storage.tasks.toaddsdsfList() {\n        print(task)\n    }\n}\n";
            let analysis = analysis::analyze(main_text, main_uri.as_str());
            let mut doc = Document {
                version: 1,
                text: main_text.to_string(),
                diagnostics: analysis.diagnostics.clone(),
                semantic_tokens: analysis.semantic_tokens,
                symbol_table: analysis.symbol_table,
                identifier_spans: analysis.identifier_spans,
                imports: HashMap::from([("storage_mod".to_string(), storage_uri.clone())]),
                direct_imports: HashMap::new(),
                wildcard_imports: vec![],
                stdlib_imports: HashMap::new(),
                stdlib_direct_imports: HashMap::new(),
                stdlib_call_sites: analysis.stdlib_call_sites.clone(),
                inlay_hint_sites: analysis.inlay_hint_sites,
                member_access_sites: analysis.member_access_sites,
            };

            for site in &analysis.imported_constructor_call_sites {
                let ret_type = imported_constructor_type_name(
                    &backend.store,
                    &doc.imports,
                    &doc.direct_imports,
                    site,
                )
                .expect("imported constructor type");
                patch_var_inferred_type(&mut doc, &site.var_name, site.decl_span, &ret_type, true);
            }

            let diagnostics = backend.check_receiver_chain_method_diagnostics(
                main_text,
                &main_uri,
                &doc.symbol_table,
                &analysis.receiver_chain_method_call_sites,
            );

            let diag = diagnostics
                .iter()
                .find(|diag| diag.message.contains("has no field or method `toaddsdsfList`"))
                .expect("missing recursive imported-field method diagnostic");
            assert_eq!(diag.code, Some(NumberOrString::String("E0204".into())));
        });
    }

    #[test]
    fn hover_resolves_object_field_and_method_declarations() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///object_member_hover.fdn").expect("document uri");
            let text = r#"object StorageManager {
    var tasks oftype list oftype string

    action _saveData returns nothing {}
    action _loadData returns nothing {}
}
"#;
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);

            let tasks_offset = text.find("tasks oftype").expect("tasks offset") as u32;
            let tasks_pos =
                convert::span_to_range(&file, Span::new(FileId(0), tasks_offset, tasks_offset + 1))
                    .start;
            let tasks_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: tasks_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("tasks hover result")
            .expect("tasks hover contents");

            let HoverContents::Markup(tasks_markup) = tasks_hover.contents else {
                panic!("expected markdown hover contents for tasks");
            };
            assert!(
                tasks_markup
                    .value
                    .contains("StorageManager.tasks: list oftype string")
            );

            let save_offset = text.find("_saveData").expect("save offset") as u32;
            let save_pos =
                convert::span_to_range(&file, Span::new(FileId(0), save_offset, save_offset + 1))
                    .start;
            let save_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: save_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("save hover result")
            .expect("save hover contents");

            let HoverContents::Markup(save_markup) = save_hover.contents else {
                panic!("expected markdown hover contents for _saveData");
            };
            assert!(save_markup.value.contains("action _saveData"));
        });
    }

    #[test]
    fn hover_appends_hash_gt_doc_comments_to_symbol_docs() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///doc_comment_hover.fdn").expect("document uri");
            let text = "#> Returns a friendly greeting.\naction greet returns string {\n    return \"hi\"\n}\n\naction main {\n    greet()\n}\n";
            backend.refresh(&uri, 1, text).await;
            let file = SourceFile::new(FileId(0), uri.as_str(), text);
            let offset = text.rfind("greet()").expect("greet call") as u32;
            let position = convert::span_to_range(&file, Span::new(FileId(0), offset, offset + 1)).start;

            let hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("hover result")
            .expect("hover contents");

            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover contents");
            };
            assert!(markup.value.contains("action greet -> string"));
            assert!(markup.value.contains("Returns a friendly greeting."));
        });
    }

    #[test]
    fn hover_resolves_identifiers_and_methods_inside_string_interpolation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (service, _socket) = LspService::new(FidanLsp::new);
            let backend = service.inner();

            let uri = Url::parse("file:///interp_hover.fdn").expect("document uri");
            let text =
                "action main {\n    var name = \"Ada\"\n    print(\"Hello {name.upper()}\")\n}\n";
            backend.refresh(&uri, 1, text).await;

            let file = SourceFile::new(FileId(0), uri.as_str(), text);

            let name_offset = text.find("{name.upper()}").expect("interp name offset") as u32 + 1;
            let name_pos =
                convert::span_to_range(&file, Span::new(FileId(0), name_offset, name_offset + 1))
                    .start;
            let name_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: name_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("name hover result")
            .expect("name hover contents");

            let HoverContents::Markup(name_markup) = name_hover.contents else {
                panic!("expected markdown hover contents for interpolation identifier");
            };
            assert!(name_markup.value.contains("var name -> string"));

            let upper_offset = text.find("upper()").expect("interp method offset") as u32;
            let upper_pos =
                convert::span_to_range(&file, Span::new(FileId(0), upper_offset, upper_offset + 1))
                    .start;
            let upper_hover = LanguageServer::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: upper_pos,
                    },
                    work_done_progress_params: Default::default(),
                },
            )
            .await
            .expect("method hover result")
            .expect("method hover contents");

            let HoverContents::Markup(upper_markup) = upper_hover.contents else {
                panic!("expected markdown hover contents for interpolation method");
            };
            assert!(upper_markup.value.contains("string.upper() -> string"));
        });
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

    #[test]
    fn organize_imports_rewrites_grouped_duplicate_member_against_direct_import() {
        let text = "use std.io.print\nuse std.io.{print, readFile}\n\naction main {\n    print(readFile(\"demo.txt\"))\n}\n";
        let diagnostics = vec![Diagnostic {
            range: Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 1,
                    character: 28,
                },
            },
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("W1007".to_string())),
            source: Some("fidan".to_string()),
            message: "duplicate import `print`".to_string(),
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        }];

        let edits =
            build_remove_unused_imports_edits_for_text("file:///demo.fdn", text, &diagnostics);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "use std.io.{readFile}");
        assert_eq!(edits[0].range.start.line, 1);
        assert_eq!(edits[0].range.end.line, 1);
    }
}
