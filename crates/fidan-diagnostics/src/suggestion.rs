use fidan_source::Span;

/// Confidence level of a fix suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Safe: applying this edit will almost certainly fix the problem.
    High,
    /// Plausible: likely the right fix, but the programmer should verify.
    Medium,
    /// Speculative: best guess based on proximity / pattern matching.
    Low,
}

/// A concrete source-level edit that could fix the diagnostic.
#[derive(Debug, Clone)]
pub struct SourceEdit {
    /// The region of source text to replace.
    pub span: Span,
    /// The replacement text.  Empty string = deletion.
    pub replacement: String,
}

/// A human-readable suggestion, optionally backed by a `SourceEdit`.
///
/// Suggestions are rendered at the bottom of an ariadne report as `help:` lines.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// Short human-readable message, e.g. `"did you mean 'foo'?"`.
    pub message: String,
    /// Machine-applicable edit, if one is available.
    pub edit: Option<SourceEdit>,
    /// How confident the fix engine is that this suggestion is correct.
    pub confidence: Confidence,
}

impl Suggestion {
    /// A textual hint with no associated source edit.
    pub fn hint(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            edit: None,
            confidence: Confidence::Medium,
        }
    }

    /// A hint + machine-applicable edit.
    pub fn fix(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
        confidence: Confidence,
    ) -> Self {
        Self {
            message: message.into(),
            edit: Some(SourceEdit {
                span,
                replacement: replacement.into(),
            }),
            confidence,
        }
    }
}
