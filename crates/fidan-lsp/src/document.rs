//! Per-document model stored in the [`DocumentStore`].

use crate::symbols::SymbolTable;
use fidan_source::Span;
use std::collections::HashMap;
use tower_lsp::lsp_types::{Diagnostic, SemanticToken, Url};

/// A single open document tracked by the language server.
#[derive(Debug, Clone)]
#[allow(dead_code)] // version and diagnostics used by future hover/refactor handlers
pub struct Document {
    /// LSP document version counter sent by the editor on every change.
    pub version: i32,
    /// Full text of the document as last reported by the editor.
    pub text: String,
    /// Most recently computed diagnostics (errors / warnings).
    pub diagnostics: Vec<Diagnostic>,
    /// Most recently computed semantic tokens (delta-encoded).
    pub semantic_tokens: Vec<SemanticToken>,
    /// Per-document symbol table: declarations → (kind, span, hover markdown).
    pub symbol_table: SymbolTable,
    /// Every identifier token's span + resolved name, used for position lookup.
    pub identifier_spans: Vec<(Span, String)>,
    /// Namespace alias → absolute file URL for `use "file.fdn" as alias` imports.
    pub imports: HashMap<String, Url>,
}
