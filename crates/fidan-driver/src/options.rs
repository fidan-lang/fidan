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
    /// Diagnostic codes to silence (e.g. `["W5003", "W1004"]`).
    /// The diagnostic is still compiled and counted for errors — only its
    /// rendered output is suppressed.
    pub suppress: Vec<String>,
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
            suppress: vec![],
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
}
