//! `fidan-driver` — Compilation pipeline orchestration.

mod options;
mod pipeline;
mod session;

pub use options::{
    Backend, CompileOptions, EmitKind, ExecutionMode, OptLevel, SandboxPolicy, TraceMode,
};
pub use pipeline::compile;
pub use session::Session;
