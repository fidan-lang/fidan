//! `fidan-driver` — Compilation pipeline orchestration.

pub mod install;
mod llvm_helper;
mod options;
mod pipeline;
mod session;

pub use install::{
    ActiveVersionMetadata, EffectiveBackend, InstallEntry, InstallsMetadata, ResolvedToolchain,
    ToolchainMetadata, resolve_fidan_home, resolve_install_root,
};
pub use llvm_helper::LLVM_BACKEND_PROTOCOL_VERSION;
pub use options::{
    Backend, CompileOptions, EmitKind, ExecutionMode, OptLevel, SandboxPolicy, TraceMode,
};
pub use pipeline::compile;
pub use session::Session;
