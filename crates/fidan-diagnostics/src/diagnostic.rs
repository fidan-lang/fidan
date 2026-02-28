use fidan_source::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity { Error, Warning, Note }

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code:     String,
    pub message:  String,
    pub span:     Span,
    pub labels:   Vec<crate::Label>,
}

impl Diagnostic {
    pub fn error(code: impl Into<String>, message: impl Into<String>, span: Span) -> Self {
        Self { severity: Severity::Error, code: code.into(), message: message.into(), span, labels: vec![] }
    }
    pub fn warning(code: impl Into<String>, message: impl Into<String>, span: Span) -> Self {
        Self { severity: Severity::Warning, code: code.into(), message: message.into(), span, labels: vec![] }
    }
    pub fn with_label(mut self, label: crate::Label) -> Self {
        self.labels.push(label);
        self
    }
}

/// Shorthand for a collection of diagnostics.
pub type DiagnosticKind = Diagnostic;
