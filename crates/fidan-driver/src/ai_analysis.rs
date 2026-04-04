use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const AI_ANALYSIS_PROTOCOL_VERSION: u32 = 1;
pub const AI_ANALYSIS_HELPER_PROTOCOL_VERSION: u32 = 1;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAnalysisHelperRequest {
    pub protocol_version: u32,
    pub command: AiAnalysisHelperCommand,
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
