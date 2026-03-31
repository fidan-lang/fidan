pub use fidan_stdlib::SandboxPolicy;
use std::path::PathBuf;

/// How much of the runtime call stack to print on an uncaught panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TraceMode {
    /// No stack trace (default).
    #[default]
    None,
    /// Show up to 5 innermost frames.
    Short,
    /// Show every frame.
    Full,
    /// Print all frames on a single line.
    Compact,
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub mode: ExecutionMode,
    pub emit: Vec<EmitKind>,
    /// Stack-trace verbosity for uncaught runtime panics.
    pub trace: TraceMode,
    /// Stop reporting errors after this many (None = no limit).
    pub max_errors: Option<usize>,
    /// Call-count threshold before the JIT compiles a hot function (0 = off).
    pub jit_threshold: u32,
    /// Treat select warnings (unused vars, null safety, deprecated, unknown
    /// decorator) as hard errors.  Mirrors `-Werror` in C compilers.
    pub strict_mode: bool,
    /// Pre-loaded stdin lines for a replay run.  Empty = normal execution;
    /// non-empty = replay every `input()` call from this list in order.
    pub replay_inputs: Vec<String>,
    /// Script-facing argv for interpreted execution. When empty, interpreted
    /// programs see just the script path as argv[0].
    pub program_args: Vec<String>,
    /// Diagnostic codes to silence (e.g. `["W5003", "W1004"]`).
    /// The diagnostic is still compiled and counted for errors — only its
    /// rendered output is suppressed.
    pub suppress: Vec<String>,
    /// Zero-config sandbox policy for `fidan run --sandbox`.
    /// `None` = no sandboxing (default).
    pub sandbox: Option<SandboxPolicy>,
    /// Optimisation level for AOT compilation.
    pub opt_level: OptLevel,
    /// Additional library search directories for the system linker.
    pub extra_lib_dirs: Vec<std::path::PathBuf>,
    /// Link the Fidan runtime dynamically (`libfidan_runtime.so` / `.dll`) instead
    /// of embedding `libfidan_runtime.a` into the binary.  Corresponds to
    /// `fidan build --link-runtime dynamic`.
    pub link_dynamic: bool,
    /// Link-time optimization mode for AOT compilation.
    pub lto: LtoMode,
    /// Post-link stripping mode for the produced native binary.
    pub strip: StripMode,
    /// AOT codegen backend selection policy.
    pub backend: Backend,
    /// Optional AOT target CPU hint.
    /// `None` = backend default; `"native"` = host-tuned when supported.
    pub target_cpu: Option<String>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            output: None,
            mode: ExecutionMode::Interpret,
            emit: vec![],
            trace: TraceMode::None,
            max_errors: None,
            jit_threshold: 500,
            strict_mode: false,
            replay_inputs: vec![],
            program_args: vec![],
            suppress: vec![],
            sandbox: None,
            opt_level: OptLevel::O2,
            extra_lib_dirs: vec![],
            link_dynamic: false,
            lto: LtoMode::Off,
            strip: StripMode::Off,
            backend: Backend::Auto,
            target_cpu: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Interpret,
    Build,
    Check,
    Test,
    /// `fidan profile` — run with interpreter timing hooks, then print report.
    Profile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitKind {
    Tokens,
    Ast,
    Hir,
    Mir,
    /// Keep the intermediate object file (`.o` / `.obj`) alongside the binary.
    Obj,
}

/// Which AOT codegen backend to use for `fidan build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Prefer a compatible installed LLVM toolchain; otherwise fall back to Cranelift.
    #[default]
    Auto,
    /// Pure-Rust Cranelift backend — no system LLVM required.
    Cranelift,
    /// LLVM backend — higher-quality code, requires LLVM to be installed.
    Llvm,
}

/// Optimisation level for AOT compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    O0,
    O1,
    #[default]
    O2,
    O3,
    Os,
    Oz,
}

/// Link-time optimization mode for AOT builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LtoMode {
    #[default]
    Off,
    Full,
}

/// Post-link stripping mode for AOT builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StripMode {
    #[default]
    Off,
    /// Remove non-essential symbols while preserving a loadable binary.
    Symbols,
    /// Remove as much symbol/debug information as the platform tool allows.
    All,
}
