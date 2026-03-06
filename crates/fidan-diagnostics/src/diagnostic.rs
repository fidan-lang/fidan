use crate::{codes::DiagCode, suggestion::Suggestion};
use fidan_source::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Note => write!(f, "note"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub span: Span,
    pub labels: Vec<crate::Label>,
    /// Plain-text notes rendered after the main message (e.g. "did you mean 'foo'?").
    pub notes: Vec<String>,
    /// Structured fix suggestions rendered as `help:` lines.
    pub suggestions: Vec<Suggestion>,
    /// Optional chain of upstream diagnostics that caused this one.
    ///
    /// Each entry is rendered as an indented sub-block so the user can trace
    /// exactly how a symptom relates to its root cause.
    pub cause_chain: Vec<Diagnostic>,
}

impl Diagnostic {
    pub fn error(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            code: code.0.to_owned(),
            message: message.into(),
            span,
            labels: vec![],
            notes: vec![],
            suggestions: vec![],
            cause_chain: vec![],
        }
    }

    pub fn warning(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.0.to_owned(),
            message: message.into(),
            span,
            labels: vec![],
            notes: vec![],
            suggestions: vec![],
            cause_chain: vec![],
        }
    }

    pub fn note(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Note,
            code: code.0.to_owned(),
            message: message.into(),
            span,
            labels: vec![],
            notes: vec![],
            suggestions: vec![],
            cause_chain: vec![],
        }
    }

    pub fn with_label(mut self, label: crate::Label) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }

    /// Attach this diagnostic as the cause of the current one.
    pub fn with_cause(mut self, cause: Diagnostic) -> Self {
        self.cause_chain.push(cause);
        self
    }

    /// Push a note in-place (for use after construction).
    pub fn add_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Push a suggestion in-place.
    pub fn add_suggestion(&mut self, s: Suggestion) {
        self.suggestions.push(s);
    }

    /// Push a cause diagnostic in-place.
    pub fn add_cause(&mut self, cause: Diagnostic) {
        self.cause_chain.push(cause);
    }
}

/// Shorthand alias kept for backwards compatibility.
pub type DiagnosticKind = Diagnostic;
