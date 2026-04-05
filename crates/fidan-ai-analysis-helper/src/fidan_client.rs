use anyhow::{Context, Result, bail};
use fidan_driver::{
    AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisCommand, AiAnalysisRequest, AiAnalysisResponse,
    AiAnalysisResult, AiCallGraph, AiExplainContext, AiRuntimeTrace, AiTypeMap,
    resolve_install_root,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn request_explain_context(
    explicit_fidan: Option<&Path>,
    file: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<AiExplainContext> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::ExplainContext {
            file: file.to_path_buf(),
            line_start,
            line_end,
        },
    };
    match invoke(explicit_fidan, &request)? {
        AiAnalysisResult::ExplainContext(context) => Ok(context),
        _ => bail!("fidan returned an unexpected ai-analysis result kind"),
    }
}

pub fn request_module_outline(
    explicit_fidan: Option<&Path>,
    file: &Path,
) -> Result<serde_json::Value> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::ModuleOutline {
            file: file.to_path_buf(),
        },
    };
    serde_json::to_value(invoke(explicit_fidan, &request)?)
        .context("failed to serialize module outline response")
}

pub fn request_project_summary(
    explicit_fidan: Option<&Path>,
    entry: &Path,
) -> Result<serde_json::Value> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::ProjectSummary {
            entry: entry.to_path_buf(),
        },
    };
    serde_json::to_value(invoke(explicit_fidan, &request)?)
        .context("failed to serialize project summary response")
}

pub fn request_symbol_info(
    explicit_fidan: Option<&Path>,
    file: &Path,
    symbol: &str,
) -> Result<serde_json::Value> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::SymbolInfo {
            file: file.to_path_buf(),
            symbol: symbol.to_string(),
        },
    };
    serde_json::to_value(invoke(explicit_fidan, &request)?)
        .context("failed to serialize symbol info response")
}

// Phase D: these are available for MCP tool wiring when ready.
#[allow(dead_code)]
pub fn request_call_graph(explicit_fidan: Option<&Path>, file: &Path) -> Result<AiCallGraph> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::CallGraph {
            file: file.to_path_buf(),
        },
    };
    match invoke(explicit_fidan, &request)? {
        AiAnalysisResult::CallGraph(graph) => Ok(graph),
        _ => bail!("fidan returned an unexpected ai-analysis result kind"),
    }
}

#[allow(dead_code)]
pub fn request_type_map(explicit_fidan: Option<&Path>, file: &Path) -> Result<AiTypeMap> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::TypeMap {
            file: file.to_path_buf(),
        },
    };
    match invoke(explicit_fidan, &request)? {
        AiAnalysisResult::TypeMap(map) => Ok(map),
        _ => bail!("fidan returned an unexpected ai-analysis result kind"),
    }
}

#[allow(dead_code)]
pub fn request_runtime_trace(
    explicit_fidan: Option<&Path>,
    file: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<AiRuntimeTrace> {
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::RuntimeTrace {
            file: file.to_path_buf(),
            line_start,
            line_end,
        },
    };
    match invoke(explicit_fidan, &request)? {
        AiAnalysisResult::RuntimeTrace(trace) => Ok(trace),
        _ => bail!("fidan returned an unexpected ai-analysis result kind"),
    }
}

fn invoke(explicit_fidan: Option<&Path>, request: &AiAnalysisRequest) -> Result<AiAnalysisResult> {
    let fidan = resolve_fidan_path(explicit_fidan)?;
    let request_bytes =
        serde_json::to_vec(request).context("failed to serialize ai-analysis request")?;
    let mut child = Command::new(&fidan)
        .arg("__ai-analysis")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to launch `{}`", fidan.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&request_bytes)
            .context("failed to send ai-analysis request to fidan")?;
    }

    let output = child
        .wait_with_output()
        .context("failed while waiting for fidan ai-analysis response")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "fidan ai-analysis command exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let response: AiAnalysisResponse = serde_json::from_slice(&output.stdout)
        .context("failed to parse fidan ai-analysis response")?;
    if response.protocol_version != AI_ANALYSIS_PROTOCOL_VERSION {
        bail!(
            "ai-analysis protocol mismatch (fidan={}, helper={})",
            response.protocol_version,
            AI_ANALYSIS_PROTOCOL_VERSION
        );
    }
    if !response.success {
        bail!(
            "fidan ai-analysis request failed{}",
            response
                .error
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        );
    }
    response
        .result
        .context("fidan ai-analysis response was missing a result")
}

fn resolve_fidan_path(explicit_fidan: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit_fidan {
        return Ok(path.to_path_buf());
    }
    if let Ok(path) = std::env::var("FIDAN_EXE")
        && !path.trim().is_empty()
    {
        return Ok(PathBuf::from(path));
    }

    let root = resolve_install_root()?;
    let exe = if cfg!(windows) { "fidan.exe" } else { "fidan" };
    Ok(root.join("current").join(exe))
}
