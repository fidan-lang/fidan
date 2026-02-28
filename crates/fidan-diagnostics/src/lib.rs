//! `fidan-diagnostics` — Diagnostic types, rendering, and fix engine.

mod codes;
mod diagnostic;
mod fix_engine;
mod label;
mod render;
mod suggestion;

pub use codes::{CODES, DiagnosticCode, lookup as lookup_code, title as code_title};
pub use diagnostic::{Diagnostic, DiagnosticKind, Severity};
pub use fix_engine::FixEngine;
pub use label::Label;
pub use render::{render_message_to_stderr, render_to_stderr};
pub use suggestion::{Confidence, SourceEdit, Suggestion};
