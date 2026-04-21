//! `fidan-driver` — Compilation pipeline orchestration.

mod ai_analysis;
pub mod dal;
mod frontend;
pub mod install;
mod llvm_helper;
mod options;
mod pipeline;
pub mod progress;
mod session;
pub mod terminal;

pub use ai_analysis::{
    AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisCommand,
    AiAnalysisHelperCommand, AiAnalysisHelperRequest, AiAnalysisHelperResponse,
    AiAnalysisHelperResult, AiAnalysisRequest, AiAnalysisResponse, AiAnalysisResult, AiCallGraph,
    AiCallNode, AiDependency, AiDeterministicExplainLine, AiDiagnosticSummary, AiExplainContext,
    AiFixHunk, AiFixMode, AiFixResult, AiModuleOutline, AiOutlineItem, AiProjectSummary,
    AiRuntimeTrace, AiStructuredExplanation, AiSymbolInfo, AiSymbolRef, AiTraceStep, AiTypeMap,
    AiTypedBinding,
};
pub use frontend::{
    FrontendOutput, ImportFilter, ResolvedImport, UnresolvedImport, collect_file_import_paths,
    compile_file_to_mir, compile_source_to_mir, detect_import_cycles, filter_hir_module,
    pre_register_hir_into_tc,
};
pub use install::{
    ActiveVersionMetadata, EffectiveBackend, InstallEntry, InstallsMetadata, ResolvedToolchain,
    ToolchainExecCommand, ToolchainMetadata, is_valid_exec_namespace, resolve_fidan_home,
    resolve_install_root,
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
