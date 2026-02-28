//! `fidan-diagnostics` — Diagnostic types, rendering, and fix engine.

mod codes;
mod diagnostic;
mod fix_engine;
mod label;
mod render;
mod suggestion;

pub use codes::{
    CODES, DiagCode, DiagnosticCode, assert_code, lookup as lookup_code, title as code_title,
};

/// Validate a diagnostic code string at **compile time** and return a [`DiagCode`].
///
/// ```rust,ignore
/// Diagnostic::error(diag_code!("E0101"), message, span);
/// ```
///
/// Passing a code not present in `CODES` is a **compile error**.
#[macro_export]
macro_rules! diag_code {
    ($code:literal) => {{
        const _VALIDATED: $crate::DiagCode = $crate::DiagCode::new($code);
        _VALIDATED
    }};
}
pub use diagnostic::{Diagnostic, DiagnosticKind, Severity};
pub use fix_engine::FixEngine;
pub use label::Label;
pub use render::{render_message_to_stderr, render_to_stderr};
pub use suggestion::{Confidence, SourceEdit, Suggestion};
