//! `fidan-driver` — Compilation pipeline orchestration.

mod session;
mod pipeline;
mod options;

pub use session::Session;
pub use pipeline::compile;
pub use options::{CompileOptions, EmitKind, ExecutionMode};
