use fidan_source::Span;

#[derive(Debug, Clone)]
pub struct Label {
    pub span:    Span,
    pub message: String,
    pub primary: bool,
}

impl Label {
    pub fn primary(span: Span, message: impl Into<String>) -> Self {
        Self { span, message: message.into(), primary: true }
    }
    pub fn secondary(span: Span, message: impl Into<String>) -> Self {
        Self { span, message: message.into(), primary: false }
    }
}
