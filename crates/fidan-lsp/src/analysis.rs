//! Lightweight analysis pass: lex + parse + type-check a source text and
//! collect LSP diagnostics, semantic tokens, and the per-document symbol table.

use crate::convert::span_to_range;
use crate::{semantic, symbols};
use fidan_ast::{Expr, Item, Module};
use fidan_diagnostics::{Diagnostic as FidanDiag, Severity};
use fidan_lexer::{Lexer, SymbolInterner, TokenKind};
use fidan_source::{FileId, SourceFile, Span};
use fidan_typeck::CrossModuleCallSite;
use std::sync::Arc;
use tower_lsp::lsp_types::{self as lsp, DiagnosticSeverity, SemanticToken};

// ── Inlay hint site ───────────────────────────────────────────────────────────

/// A position in the source where the LSP should show a synthetic label.
#[derive(Debug, Clone)]
pub struct InlayHintSite {
    /// Byte offset in the source at which to insert the label
    /// (placed immediately *after* the relevant identifier).
    pub byte_offset: u32,
    /// Text to display, e.g. `": integer"`.
    pub label: String,
    /// `true` → type annotation, `false` → parameter name.
    pub is_type_hint: bool,
}

/// Output of a single analysis run.
pub struct AnalysisResult {
    pub diagnostics: Vec<lsp::Diagnostic>,
    pub semantic_tokens: Vec<SemanticToken>,
    /// Every identifier token: (span, resolved name). Used for hover / go-to-def.
    pub identifier_spans: Vec<(Span, String)>,
    /// Per-document symbol table built from declarations. Used for hover / completion.
    pub symbol_table: symbols::SymbolTable,
    /// File-path imports declared in this document: `(alias_name, file_path_string)`.
    /// E.g. `use "test.fdn" as module` → `("module", "test.fdn")`.
    pub imports: Vec<(String, String)>,
    /// Stdlib imports: `(alias_name, module_name)`.
    /// E.g. `use std.io` → `("io", "io")`; `use std.math as m` → `("m", "math")`.
    pub stdlib_imports: Vec<(String, String)>,
    /// Non-call member accesses where the target type has a cross-module parent.
    pub cross_module_field_accesses: Vec<(String, String, Span)>,
    /// Method call sites on cross-module receivers, with inferred arg types.
    pub cross_module_call_sites: Vec<CrossModuleCallSite>,
    /// Top-level `var x = recv.method()` where `method` resolved to Dynamic (cross-module).
    /// Stored as `(var_name, receiver_type_name, method_name)` so the server can patch
    /// the symbol-table entry after loading cross-module docs.
    pub dynamic_var_call_sites: Vec<(String, String, String)>,
    /// Top-level `var x = std_alias.member()` calls where the alias resolves to `std.<module>`.
    /// Stored as `(var_name, module_name, member_name)` so the server can patch the
    /// symbol-table entry using shared stdlib metadata.
    pub stdlib_var_call_sites: Vec<(String, String, String)>,
    /// Positions where the editor should display synthetic type labels.
    pub inlay_hint_sites: Vec<InlayHintSite>,
}

/// Lex, parse and type-check `text`, returning all diagnostics as LSP
/// `Diagnostic` objects and a full set of semantic tokens for the document.
///
/// The `uri` string is used as the "file name" inside `SourceFile` so that
/// diagnostics printed to stderr (if any) show a meaningful path.
pub fn analyze(text: &str, uri_str: &str) -> AnalysisResult {
    let file = SourceFile::new(FileId(0), uri_str, text);
    let interner = Arc::new(SymbolInterner::new());

    // ── Lex ──────────────────────────────────────────────────────────────────
    let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();

    // ── Parse ─────────────────────────────────────────────────────────────────
    let (module, parse_diags) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));

    // ── Identifier-span index (for hover / go-to-def positional lookup) ────────
    let identifier_spans: Vec<(Span, String)> = tokens
        .iter()
        .filter_map(|tok| match &tok.kind {
            TokenKind::Ident(sym) => Some((tok.span, interner.resolve(*sym).to_string())),
            TokenKind::Shared => Some((tok.span, "Shared".to_string())),
            TokenKind::Pending => Some((tok.span, "Pending".to_string())),
            TokenKind::Weak => Some((tok.span, "WeakShared".to_string())),
            _ => None,
        })
        .collect();

    // ── Type-check (full — needed to build hover/completion symbol table) ────
    let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    let symbol_table = symbols::build(&module, &typed, &interner);

    // ── File-path import extraction ─────────────────────────────────────
    let imports = extract_imports(&module, &interner);

    // ── Stdlib import extraction (`use std.<module>`) ────────────────────
    let stdlib_imports = extract_stdlib_imports(&module, &interner);

    // ── Dynamic var call sites (cross-module method return type patching) ──
    let dynamic_var_call_sites = extract_dynamic_var_calls(&module, &typed, &interner);
    let stdlib_var_call_sites = extract_stdlib_var_call_sites(&module, &interner, &stdlib_imports);

    // ── Inlay hints (untyped var declarations) ────────────────────────────────
    let inlay_hint_sites =
        collect_inlay_hints(&module, &interner, &symbol_table, &identifier_spans);

    // Consume typed fields now (after all borrows of `typed` are done).
    let typeck_diags = typed.diagnostics;
    let cross_module_field_accesses = typed.cross_module_field_accesses;
    let cross_module_call_sites = typed.cross_module_call_sites;

    // ── Diagnostics ───────────────────────────────────────────────────────────
    let diagnostics = lex_diags
        .into_iter()
        .chain(parse_diags)
        .chain(typeck_diags)
        .map(|d| fidan_to_lsp(&d, &file))
        .collect();

    // ── Semantic tokens ───────────────────────────────────────────────────────
    let semantic_tokens = semantic::compute(&tokens, &file, &interner);

    AnalysisResult {
        diagnostics,
        semantic_tokens,
        identifier_spans,
        symbol_table,
        imports,
        stdlib_imports,
        cross_module_field_accesses,
        cross_module_call_sites,
        dynamic_var_call_sites,
        stdlib_var_call_sites,
        inlay_hint_sites,
    }
}

/// Extract `(alias, file_path)` pairs from `use "file.fdn" as alias` items.
fn extract_imports(module: &Module, interner: &SymbolInterner) -> Vec<(String, String)> {
    module
        .items
        .iter()
        .filter_map(|&iid| {
            if let Item::Use { path, alias, .. } = module.arena.get_item(iid) {
                if path.len() != 1 {
                    return None;
                }
                let path_str = interner.resolve(path[0]).to_string();
                // Only treat as a file-path import if it looks like a path.
                let is_file = path_str.ends_with(".fdn")
                    || path_str.starts_with("./")
                    || path_str.starts_with("../")
                    || path_str.starts_with('/');
                if !is_file {
                    return None;
                }
                let alias_str = alias
                    .map(|a| interner.resolve(a).to_string())
                    .unwrap_or_else(|| {
                        std::path::Path::new(&path_str)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(&path_str)
                            .to_string()
                    });
                Some((alias_str, path_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract `(alias, module_name)` pairs from `use std.<module>` items (namespace imports only).
/// Grouped/destructured imports (`use std.io.{fn}`) are excluded — those inject free names.
fn extract_stdlib_imports(module: &Module, interner: &SymbolInterner) -> Vec<(String, String)> {
    module
        .items
        .iter()
        .filter_map(|&iid| {
            if let Item::Use {
                path,
                alias,
                grouped,
                ..
            } = module.arena.get_item(iid)
            {
                // Need at least `std` + one module segment.
                if path.len() < 2 {
                    return None;
                }
                // Must start with `std`.
                if interner.resolve(path[0]).as_ref() != "std" {
                    return None;
                }
                // Skip grouped imports — they flatten names into scope, not a namespace.
                if *grouped {
                    return None;
                }
                // Module name is the second segment (e.g. "io", "math").
                let module_name = interner.resolve(path[1]).to_string();
                // Alias: explicit `as name` or implicit last segment.
                let alias_str = alias
                    .map(|a| interner.resolve(a).to_string())
                    .unwrap_or_else(|| module_name.clone());
                Some((alias_str, module_name))
            } else {
                None
            }
        })
        .collect()
}

/// Collect top-level `var x = recv.method()` sites where the call's return type
/// resolved to `Dynamic` (cross-module receiver).  The server uses these to
/// retrospectively patch `x`'s symbol-table entry once the imported doc is loaded.
fn extract_dynamic_var_calls(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                name,
                init: Some(init_eid),
                ..
            } => maybe_push_dynamic_var_call(module, typed, interner, *name, *init_eid, &mut out),
            Item::ActionDecl { body, .. }
            | Item::ExtensionAction { body, .. }
            | Item::TestDecl { body, .. } => {
                collect_stmt_dynamic_var_calls(module, body, typed, interner, &mut out)
            }
            Item::Stmt(stmt_id) => {
                collect_stmt_dynamic_var_calls(module, &[*stmt_id], typed, interner, &mut out)
            }
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl { body, .. } = module.arena.get_item(mid) {
                        collect_stmt_dynamic_var_calls(module, body, typed, interner, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn maybe_push_dynamic_var_call(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    var_name_sym: fidan_lexer::Symbol,
    init_eid: fidan_ast::ExprId,
    out: &mut Vec<(String, String, String)>,
) {
    if !matches!(
        typed.expr_types.get(&init_eid),
        Some(fidan_typeck::FidanType::Dynamic) | None
    ) {
        return;
    }
    if let Expr::Call { callee, .. } = module.arena.get_expr(init_eid)
        && let Expr::Field { object, field, .. } = module.arena.get_expr(*callee)
        && let Some(fidan_typeck::FidanType::Object(obj_sym)) = typed.expr_types.get(object)
    {
        out.push((
            interner.resolve(var_name_sym).to_string(),
            interner.resolve(*obj_sym).to_string(),
            interner.resolve(*field).to_string(),
        ));
    }
}

fn extract_stdlib_var_call_sites(
    module: &Module,
    interner: &SymbolInterner,
    stdlib_imports: &[(String, String)],
) -> Vec<(String, String, String)> {
    let stdlib_aliases: std::collections::HashMap<&str, &str> = stdlib_imports
        .iter()
        .map(|(alias, module_name)| (alias.as_str(), module_name.as_str()))
        .collect();
    let mut out = Vec::new();

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                name,
                init: Some(init_eid),
                ..
            } => maybe_push_stdlib_var_call(
                module,
                interner,
                &stdlib_aliases,
                *name,
                *init_eid,
                &mut out,
            ),
            Item::ActionDecl { body, .. }
            | Item::ExtensionAction { body, .. }
            | Item::TestDecl { body, .. } => {
                collect_stmt_stdlib_var_calls(module, body, interner, &stdlib_aliases, &mut out)
            }
            Item::Stmt(stmt_id) => collect_stmt_stdlib_var_calls(
                module,
                &[*stmt_id],
                interner,
                &stdlib_aliases,
                &mut out,
            ),
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl { body, .. } = module.arena.get_item(mid) {
                        collect_stmt_stdlib_var_calls(
                            module,
                            body,
                            interner,
                            &stdlib_aliases,
                            &mut out,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    out
}

fn collect_stmt_dynamic_var_calls(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    out: &mut Vec<(String, String, String)>,
) {
    for &sid in stmts {
        match module.arena.get_stmt(sid) {
            fidan_ast::Stmt::VarDecl {
                name,
                init: Some(init_eid),
                ..
            } => {
                if !matches!(
                    typed.expr_types.get(init_eid),
                    Some(fidan_typeck::FidanType::Dynamic) | None
                ) {
                    continue;
                }
                if let Expr::Call { callee, .. } = module.arena.get_expr(*init_eid)
                    && let Expr::Field { object, field, .. } = module.arena.get_expr(*callee)
                    && let Some(fidan_typeck::FidanType::Object(obj_sym)) =
                        typed.expr_types.get(object)
                {
                    out.push((
                        interner.resolve(*name).to_string(),
                        interner.resolve(*obj_sym).to_string(),
                        interner.resolve(*field).to_string(),
                    ));
                }
            }
            fidan_ast::Stmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_stmt_dynamic_var_calls(module, then_body, typed, interner, out);
                for else_if in else_ifs {
                    collect_stmt_dynamic_var_calls(module, &else_if.body, typed, interner, out);
                }
                if let Some(else_body) = else_body {
                    collect_stmt_dynamic_var_calls(module, else_body, typed, interner, out);
                }
            }
            fidan_ast::Stmt::Check { arms, .. } => {
                for arm in arms {
                    collect_stmt_dynamic_var_calls(module, &arm.body, typed, interner, out);
                }
            }
            fidan_ast::Stmt::For { body, .. }
            | fidan_ast::Stmt::While { body, .. }
            | fidan_ast::Stmt::ParallelFor { body, .. } => {
                collect_stmt_dynamic_var_calls(module, body, typed, interner, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_dynamic_var_calls(module, body, typed, interner, out);
                for catch in catches {
                    collect_stmt_dynamic_var_calls(module, &catch.body, typed, interner, out);
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_dynamic_var_calls(module, otherwise, typed, interner, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_dynamic_var_calls(module, finally, typed, interner, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_dynamic_var_calls(module, &task.body, typed, interner, out);
                }
            }
            _ => {}
        }
    }
}

fn collect_stmt_stdlib_var_calls(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    interner: &SymbolInterner,
    stdlib_aliases: &std::collections::HashMap<&str, &str>,
    out: &mut Vec<(String, String, String)>,
) {
    for &sid in stmts {
        match module.arena.get_stmt(sid) {
            fidan_ast::Stmt::VarDecl {
                name,
                init: Some(init_eid),
                ..
            } => {
                maybe_push_stdlib_var_call(module, interner, stdlib_aliases, *name, *init_eid, out)
            }
            fidan_ast::Stmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_stmt_stdlib_var_calls(module, then_body, interner, stdlib_aliases, out);
                for else_if in else_ifs {
                    collect_stmt_stdlib_var_calls(
                        module,
                        &else_if.body,
                        interner,
                        stdlib_aliases,
                        out,
                    );
                }
                if let Some(else_body) = else_body {
                    collect_stmt_stdlib_var_calls(module, else_body, interner, stdlib_aliases, out);
                }
            }
            fidan_ast::Stmt::Check { arms, .. } => {
                for arm in arms {
                    collect_stmt_stdlib_var_calls(module, &arm.body, interner, stdlib_aliases, out);
                }
            }
            fidan_ast::Stmt::For { body, .. }
            | fidan_ast::Stmt::While { body, .. }
            | fidan_ast::Stmt::ParallelFor { body, .. } => {
                collect_stmt_stdlib_var_calls(module, body, interner, stdlib_aliases, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_stdlib_var_calls(module, body, interner, stdlib_aliases, out);
                for catch in catches {
                    collect_stmt_stdlib_var_calls(
                        module,
                        &catch.body,
                        interner,
                        stdlib_aliases,
                        out,
                    );
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_stdlib_var_calls(module, otherwise, interner, stdlib_aliases, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_stdlib_var_calls(module, finally, interner, stdlib_aliases, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_stdlib_var_calls(
                        module,
                        &task.body,
                        interner,
                        stdlib_aliases,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

fn maybe_push_stdlib_var_call(
    module: &Module,
    interner: &SymbolInterner,
    stdlib_aliases: &std::collections::HashMap<&str, &str>,
    var_name_sym: fidan_lexer::Symbol,
    init_eid: fidan_ast::ExprId,
    out: &mut Vec<(String, String, String)>,
) {
    if let Expr::Call { callee, .. } = module.arena.get_expr(init_eid)
        && let Expr::Field { object, field, .. } = module.arena.get_expr(*callee)
        && let Expr::Ident {
            name: recv_name, ..
        } = module.arena.get_expr(*object)
        && let Some(module_name) = stdlib_aliases.get(interner.resolve(*recv_name).as_ref())
    {
        out.push((
            interner.resolve(var_name_sym).to_string(),
            (*module_name).to_string(),
            interner.resolve(*field).to_string(),
        ));
    }
}

fn fidan_to_lsp(d: &FidanDiag, file: &SourceFile) -> lsp::Diagnostic {
    // Encode machine-applicable suggestions as JSON in `data` so the
    // `code_action` handler can offer quick-fix actions without re-running
    // the full analysis pipeline.
    let data: Option<serde_json::Value> = if d.suggestions.is_empty() {
        None
    } else {
        let fixes: Vec<serde_json::Value> = d
            .suggestions
            .iter()
            .filter_map(|s| {
                let edit = s.edit.as_ref()?;
                Some(serde_json::json!({
                    "message": s.message,
                    "start":   edit.span.start,
                    "end":     edit.span.end,
                    "replacement": edit.replacement,
                }))
            })
            .collect();
        if fixes.is_empty() {
            None
        } else {
            Some(serde_json::json!(fixes))
        }
    };

    lsp::Diagnostic {
        range: span_to_range(file, d.span),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Note => DiagnosticSeverity::INFORMATION,
        }),
        code: Some(lsp::NumberOrString::String(d.code.clone())),
        source: Some("fidan".to_string()),
        message: d.message.clone(),
        related_information: None,
        tags: None,
        code_description: None,
        data,
    }
}

/// Collect inlay hint sites from a module.
///
/// Currently emits a `": type"` label after the name of every `var`/`const var`
/// declaration that has **no explicit type annotation** but whose type was
/// successfully inferred during the type-check pass.
fn collect_inlay_hints(
    module: &Module,
    interner: &SymbolInterner,
    symbol_table: &symbols::SymbolTable,
    identifier_spans: &[(Span, String)],
) -> Vec<InlayHintSite> {
    let mut hints = Vec::new();
    for &iid in &module.items {
        if let Item::VarDecl { name, ty: None, .. } = module.arena.get_item(iid) {
            let name_str = interner.resolve(*name).to_string();
            // Only emit if the symbol table resolved a concrete type.
            let entry = match symbol_table.get(&name_str) {
                Some(e) => e,
                None => continue,
            };
            // Extract type from hover detail: `"```fidan\nvar x: integer\n```"`.
            // Grab the part after the `:` on the middle line.
            let type_label = extract_type_from_detail(&entry.detail);
            if type_label == "?" {
                continue; // unresolved — don't clutter the editor
            }
            // Find the identifier token span for the variable name in the source.
            // Use the first occurrence that matches the declared name exactly.
            if let Some((span, _)) = identifier_spans.iter().find(|(_, n)| n == &name_str) {
                hints.push(InlayHintSite {
                    byte_offset: span.end,
                    label: format!(" -> {}", type_label),
                    is_type_hint: true,
                });
            }
        }
    }
    hints
}

/// Extract the type string from a hover detail like `"```fidan\nvar x: integer\n```"`.
fn extract_type_from_detail(detail: &str) -> &str {
    // The detail for variables looks like:  `\`\`\`fidan\nvar x -> type\n\`\`\``
    // We want everything after the last `->` on the declaration line, trimmed.
    for line in detail.lines() {
        if let Some(colon_pos) = line.rfind("->") {
            let candidate = line[colon_pos + 2..].trim();
            if !candidate.is_empty() && !candidate.contains('`') {
                return candidate;
            }
        }
    }
    "?"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn workspace_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn assert_file_analyzes_without_errors(rel_path: &str) {
        let path = workspace_root().join(rel_path);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let uri = format!("file:///{}", path.display().to_string().replace('\\', "/"));
        let result = analyze(&src, &uri);
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|diag| diag.severity == Some(DiagnosticSeverity::ERROR))
            .collect();
        assert!(
            errors.is_empty(),
            "expected no analysis errors for {} but got: {errors:#?}",
            rel_path
        );
    }

    #[test]
    fn analyze_recent_feature_surface_without_errors() {
        let src = r#"use std.async
use std.collections as collections
use std.regex

enum Result {
    Ok(string)
    Err(integer, dynamic)
}

@extern("kernel32", symbol = "Beep")
action beep with (certain freq oftype integer, certain ms oftype integer) returns nothing

action work with (optional name oftype dynamic = r"{guest}") returns dynamic {
    return name
}

action main {
    var raw = r"\n {literal}"
    var grouped = collections.groupBy([1, 1, 2])
    var parts = collections.chunk([1, 2, 3, 4], 2)
    var windows = collections.window([1, 2, 3], 2)
    var rows = collections.enumerate([10, 20])
    concurrent {
        task reader {
            print(raw)
        }
        task writer {
            print(await async.gather([spawn work("Ada")]))
        }
    }
}
"#;

        let result = analyze(src, "file:///feature_surface.fdn");
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|diag| diag.severity == Some(DiagnosticSeverity::ERROR))
            .collect();
        assert!(
            errors.is_empty(),
            "expected no analysis errors, got: {errors:#?}"
        );
        assert!(
            result
                .stdlib_imports
                .iter()
                .any(|(alias, module)| alias == "async" && module == "async")
        );
        assert!(
            result
                .stdlib_imports
                .iter()
                .any(|(alias, module)| alias == "collections" && module == "collections")
        );
        assert!(
            result
                .stdlib_imports
                .iter()
                .any(|(alias, module)| alias == "regex" && module == "regex")
        );
        assert!(
            result
                .stdlib_var_call_sites
                .iter()
                .any(|(var, module, member)| var == "rows"
                    && module == "collections"
                    && member == "enumerate")
        );
    }

    #[test]
    fn analyze_records_stdlib_namespace_call_sites_for_lsp_type_patching() {
        let src = r#"use std.env

var argv = env.args()
"#;

        let result = analyze(src, "file:///stdlib_type_patch.fdn");
        assert!(
            result
                .stdlib_var_call_sites
                .iter()
                .any(|(var, module, member)| var == "argv" && module == "env" && member == "args"),
            "expected env.args() to be recorded for stdlib type patching"
        );
    }

    #[test]
    fn analyze_indexes_builtin_wrapper_tokens_for_hover() {
        let src = "var counter oftype Shared oftype integer = Shared(0)\nvar weak oftype WeakShared\nvar pending oftype Pending oftype string\n";
        let result = analyze(src, "file:///builtin_wrappers.fdn");
        assert!(
            result
                .identifier_spans
                .iter()
                .any(|(_, name)| name == "Shared")
        );
        assert!(
            result
                .identifier_spans
                .iter()
                .any(|(_, name)| name == "WeakShared")
        );
        assert!(
            result
                .identifier_spans
                .iter()
                .any(|(_, name)| name == "Pending")
        );
    }

    #[test]
    fn analyze_current_feature_examples_without_errors() {
        for rel_path in [
            "test/examples/check_val.fdn",
            "test/examples/async_demo.fdn",
            "test/examples/concurrency_showcase.fdn",
            "test/examples/parallel_demo.fdn",
            "test/examples/trace_demo.fdn",
            "test/examples/spawn_method_test.fdn",
        ] {
            assert_file_analyzes_without_errors(rel_path);
        }
    }
}
