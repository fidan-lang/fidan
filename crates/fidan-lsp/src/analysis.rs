//! Lightweight analysis pass: lex + parse + type-check a source text and
//! collect LSP diagnostics, semantic tokens, and the per-document symbol table.

use crate::convert::span_to_range;
use crate::{semantic, symbols};
use fidan_ast::{Item, Module};
use fidan_diagnostics::{Diagnostic as FidanDiag, Severity};
use fidan_lexer::{Lexer, SymbolInterner, TokenKind};
use fidan_source::{FileId, SourceFile, Span};
use std::sync::Arc;
use tower_lsp::lsp_types::{self as lsp, DiagnosticSeverity, SemanticToken};
use fidan_typeck::CrossModuleCallSite;

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
    /// Non-call member accesses where the target type has a cross-module parent.
    pub cross_module_field_accesses: Vec<(String, String, Span)>,
    /// Method call sites on cross-module receivers, with inferred arg types.
    pub cross_module_call_sites: Vec<CrossModuleCallSite>,
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
        .filter_map(|tok| {
            if let TokenKind::Ident(sym) = &tok.kind {
                Some((tok.span, interner.resolve(*sym).to_string()))
            } else {
                None
            }
        })
        .collect();

    // ── Type-check (full — needed to build hover/completion symbol table) ────
    let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    let symbol_table = symbols::build(&module, &typed, &interner);
    let typeck_diags = typed.diagnostics;

    // ── File-path import extraction ─────────────────────────────────────
    let imports = extract_imports(&module, &interner);

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
        cross_module_field_accesses: typed.cross_module_field_accesses,
        cross_module_call_sites: typed.cross_module_call_sites,
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

fn fidan_to_lsp(d: &FidanDiag, file: &SourceFile) -> lsp::Diagnostic {
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
        data: None,
    }
}
