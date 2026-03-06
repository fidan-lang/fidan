//! `fidan-driver` — Compilation pipeline orchestration.

mod options;
mod pipeline;
mod session;

pub use options::{CompileOptions, EmitKind, ExecutionMode, SandboxPolicy, TraceMode};
pub use pipeline::compile;
pub use session::Session;
