//! Per-document model stored in the [`DocumentStore`].

use tower_lsp::lsp_types::Diagnostic;

/// A single open document tracked by the language server.
#[derive(Debug, Clone)]
#[allow(dead_code)] // version and diagnostics will be read by hover/completion (P1)
pub struct Document {
    /// LSP document version counter sent by the editor on every change.
    pub version: i32,
    /// Full text of the document as last reported by the editor.
    pub text: String,
    /// Most recently computed diagnostics (errors / warnings).
    pub diagnostics: Vec<Diagnostic>,
}
