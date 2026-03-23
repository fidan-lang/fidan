//! `fidan-driver` — Compilation pipeline orchestration.

mod frontend;
pub mod install;
mod llvm_helper;
mod options;
mod pipeline;
pub mod progress;
mod session;

pub use frontend::{
    FrontendOutput, ImportFilter, ResolvedImport, UnresolvedImport, collect_file_import_paths,
    compile_file_to_mir, compile_source_to_mir, filter_hir_module, pre_register_hir_into_tc,
};
pub use install::{
    ActiveVersionMetadata, EffectiveBackend, InstallEntry, InstallsMetadata, ResolvedToolchain,
    ToolchainMetadata, resolve_fidan_home, resolve_install_root,
};
pub use llvm_helper::{
    LLVM_BACKEND_PROTOCOL_VERSION, LlvmBackendPayload, LlvmCompileRequest, LlvmCompileResponse,
    SerializableLtoMode, SerializableOptLevel, SerializableStripMode,
};
pub use options::{
    Backend, CompileOptions, EmitKind, ExecutionMode, LtoMode, OptLevel, SandboxPolicy, StripMode,
    TraceMode,
};
pub use pipeline::compile;
pub use session::Session;
