//! `fidan-diagnostics` — Diagnostic types, rendering, and fix engine.

mod diagnostic;
mod label;
mod render;
mod fix_engine;

pub use diagnostic::{Diagnostic, DiagnosticKind, Severity};
pub use label::Label;
pub use fix_engine::FixEngine;
