//! Lightweight analysis pass: lex + parse a source text and collect LSP diagnostics.

use crate::convert::span_to_range;
use fidan_diagnostics::{Diagnostic as FidanDiag, Severity};
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::{FileId, SourceFile};
use std::sync::Arc;
use tower_lsp::lsp_types::{self as lsp, DiagnosticSeverity};

/// Output of a single analysis run.
pub struct AnalysisResult {
    pub diagnostics: Vec<lsp::Diagnostic>,
}

/// Lex and parse `text`, returning all lex + parse diagnostics as LSP
/// `Diagnostic` objects.
///
/// The `uri` string is used as the "file name" inside `SourceFile` so that
/// diagnostics printed to stderr (if any) show a meaningful path.
pub fn analyze(text: &str, uri_str: &str) -> AnalysisResult {
    let file = SourceFile::new(FileId(0), uri_str, text);
    let interner = Arc::new(SymbolInterner::new());
    let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (_module, parse_diags) = fidan_parser::parse(&tokens, FileId(0), interner);

    let diagnostics = lex_diags
        .into_iter()
        .chain(parse_diags)
        .map(|d| fidan_to_lsp(&d, &file))
        .collect();

    AnalysisResult { diagnostics }
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
