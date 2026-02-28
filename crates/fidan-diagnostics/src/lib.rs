//! `fidan-diagnostics` — Diagnostic types, rendering, and fix engine.

mod diagnostic;
mod fix_engine;
mod label;
mod render;

pub use diagnostic::{Diagnostic, DiagnosticKind, Severity};
pub use fix_engine::FixEngine;
pub use label::Label;
pub use render::render_to_stderr;
