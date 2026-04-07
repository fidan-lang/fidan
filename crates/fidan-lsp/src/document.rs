//! Per-document model stored in the [`DocumentStore`].

use crate::analysis::InlayHintSite;
use crate::analysis::MemberAccessSite;
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
    /// Namespace alias → absolute file URL for imports that bind a module namespace.
    pub imports: HashMap<String, Url>,
    /// Flat imported symbol → (absolute file URL, exported symbol name).
    pub direct_imports: HashMap<String, (Url, String)>,
    /// Ordered wildcard file imports from `use "file.fdn"` declarations.
    pub wildcard_imports: Vec<Url>,
    /// Stdlib module alias → canonical module name. E.g. `use std.io` → `"io" → "io"`;
    /// `use std.math as m` → `"m" → "math"`.
    pub stdlib_imports: HashMap<String, String>,
    /// Inlay hint positions computed during the last analysis pass.
    pub inlay_hint_sites: Vec<InlayHintSite>,
    /// Typed member-access spans for hover on literal/computed receivers.
    pub member_access_sites: Vec<MemberAccessSite>,
}
