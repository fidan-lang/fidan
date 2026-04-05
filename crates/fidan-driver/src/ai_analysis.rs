use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const AI_ANALYSIS_PROTOCOL_VERSION: u32 = 1;
pub const AI_ANALYSIS_HELPER_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAnalysisRequest {
    pub protocol_version: u32,
    pub command: AiAnalysisCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiAnalysisCommand {
    ExplainContext {
        file: PathBuf,
        line_start: Option<usize>,
        line_end: Option<usize>,
    },
    ModuleOutline {
        file: PathBuf,
    },
    ProjectSummary {
        entry: PathBuf,
    },
    SymbolInfo {
        file: PathBuf,
        symbol: String,
    },
    CallGraph {
        file: PathBuf,
    },
    TypeMap {
        file: PathBuf,
    },
    RuntimeTrace {
        file: PathBuf,
        line_start: Option<usize>,
        line_end: Option<usize>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAnalysisResponse {
    pub protocol_version: u32,
    pub success: bool,
    pub result: Option<AiAnalysisResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiAnalysisResult {
    ExplainContext(AiExplainContext),
    ModuleOutline(AiModuleOutline),
    ProjectSummary(AiProjectSummary),
    SymbolInfo(AiSymbolInfo),
    CallGraph(AiCallGraph),
    TypeMap(AiTypeMap),
    RuntimeTrace(AiRuntimeTrace),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiExplainContext {
    pub file: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub total_lines: usize,
    pub selected_source: String,
    pub deterministic_lines: Vec<AiDeterministicExplainLine>,
    pub module_outline: Vec<AiOutlineItem>,
    pub dependencies: Vec<AiDependency>,
    pub related_symbols: Vec<AiSymbolRef>,
    pub diagnostics: Vec<AiDiagnosticSummary>,
    pub call_graph: Vec<AiCallNode>,
    pub type_map: Vec<AiTypedBinding>,
    pub runtime_trace: Option<AiRuntimeTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiDeterministicExplainLine {
    pub line: usize,
    pub source: String,
    pub what_it_does: String,
    pub inferred_type: Option<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiOutlineItem {
    pub kind: String,
    pub name: String,
    pub line: usize,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiDependency {
    pub path: String,
    pub alias: Option<String>,
    pub is_re_export: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSymbolRef {
    pub name: String,
    pub kind: String,
    pub file: PathBuf,
    pub line: usize,
    pub snippet: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiDiagnosticSummary {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiModuleOutline {
    pub file: PathBuf,
    pub items: Vec<AiOutlineItem>,
    pub dependencies: Vec<AiDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProjectSummary {
    pub entry: PathBuf,
    pub file_count: usize,
    pub files: Vec<PathBuf>,
    pub top_level_items: Vec<AiOutlineItem>,
    pub dependencies: Vec<AiDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSymbolInfo {
    pub file: PathBuf,
    pub symbol: String,
    pub matches: Vec<AiSymbolRef>,
}

/// A single node in the call graph — one action/method and its callees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCallNode {
    /// Fully-qualified caller name (e.g. `"MyObject::my_method"` or `"greet"`).
    pub caller: String,
    /// Names of actions/methods called inside this one, deduplicated.
    pub callees: Vec<String>,
    /// Source line where the action is declared.
    pub line: usize,
    /// True if the action appears to call itself (directly recursive).
    pub is_recursive: bool,
}

/// Static call graph for the whole module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCallGraph {
    pub nodes: Vec<AiCallNode>,
}

/// One typed binding (variable, constant, or parameter) discovered statically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTypedBinding {
    /// Variable / parameter name.
    pub name: String,
    /// Inferred type string (e.g. `"integer"`, `"string"`, `"bool"`).
    pub inferred_type: String,
    /// Source line of the declaration.
    pub line: usize,
    /// `"var"`, `"const"`, or `"param"`.
    pub kind: String,
}

/// Type map for the whole module (or a selected range).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTypeMap {
    pub bindings: Vec<AiTypedBinding>,
}

/// A single step in a static execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTraceStep {
    /// Category: `"assign"`, `"call"`, `"return"`, `"branch"`,
    /// `"loop"`, `"concurrent"`, `"panic"`, `"import"`, or `"other"`.
    pub kind: String,
    /// Human-readable description of what this step does.
    pub description: String,
    /// Source line, if known.
    pub line: Option<usize>,
    /// Static value hint (e.g. `"42"` for an integer literal), if determinable.
    pub value: Option<String>,
}

/// A static trace simulating execution order for the selected range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRuntimeTrace {
    pub steps: Vec<AiTraceStep>,
    /// True when the step limit (250) was reached before visiting all statements.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAnalysisHelperRequest {
    pub protocol_version: u32,
    pub command: AiAnalysisHelperCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiFixMode {
    Diagnostics,
    Improve,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiAnalysisHelperCommand {
    Explain {
        file: PathBuf,
        line_start: Option<usize>,
        line_end: Option<usize>,
        prompt: Option<String>,
        fidan_path: Option<PathBuf>,
    },
    Fix {
        /// Path of the source file (for display only — source is passed inline).
        file: PathBuf,
        /// Source content after deterministic high-confidence fixes have been applied.
        source: String,
        /// Remaining diagnostics that could not be auto-fixed.
        diagnostics: Vec<AiDiagnosticSummary>,
        /// Optional compiler-backed context derived from the patched source.
        #[serde(default)]
        explain_context: Box<Option<AiExplainContext>>,
        /// Whether the AI should resolve compiler diagnostics or perform general improvements.
        mode: AiFixMode,
        /// Optional steering prompt from the user.
        prompt: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAnalysisHelperResponse {
    pub protocol_version: u32,
    pub success: bool,
    pub result: Option<AiAnalysisHelperResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiAnalysisHelperResult {
    Explain(AiStructuredExplanation),
    Fix(AiFixResult),
}

/// One source-text hunk returned by the AI fixer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiFixHunk {
    /// Line number of the first line to replace (1-based, inclusive).
    pub line_start: usize,
    /// Line number of the last line to replace (1-based, inclusive).
    pub line_end: usize,
    /// Exact original text (used to verify the hunk is still applicable).
    pub old_text: String,
    /// Replacement text (the minimal fix).
    pub new_text: String,
    /// One-sentence rationale explaining what diagnostic this resolves.
    pub reason: String,
}

/// Full fix result returned by the AI analysis helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiFixResult {
    /// High-level summary of all changes made.
    pub summary: String,
    /// Individual text hunks to apply (in any order; `fix.rs` sorts them).
    pub hunks: Vec<AiFixHunk>,
    /// Model identifier returned by the provider, if available.
    pub model: Option<String>,
    /// Provider name (`"openai-compatible"`, `"anthropic"`, etc.).
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiStructuredExplanation {
    pub summary: String,
    pub input_output_behavior: String,
    pub dependencies: String,
    pub possible_edge_cases: String,
    pub why_pattern_is_used: String,
    pub related_symbols: String,
    pub underlying_behaviour: String,
    pub model: Option<String>,
    pub provider: Option<String>,
}
