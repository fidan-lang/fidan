use crate::install::ResolvedToolchain;
use crate::{CompileOptions, OptLevel};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LLVM_BACKEND_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlvmCompileRequest {
    pub protocol_version: u32,
    pub input: PathBuf,
    pub output: PathBuf,
    pub opt_level: SerializableOptLevel,
    pub emit_obj: bool,
    pub extra_lib_dirs: Vec<PathBuf>,
    pub link_dynamic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlvmCompileResponse {
    pub protocol_version: u32,
    pub success: bool,
    pub output: Option<PathBuf>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializableOptLevel {
    O0,
    O1,
    O2,
    O3,
    Os,
    Oz,
}

impl From<OptLevel> for SerializableOptLevel {
    fn from(value: OptLevel) -> Self {
        match value {
            OptLevel::O0 => Self::O0,
            OptLevel::O1 => Self::O1,
            OptLevel::O2 => Self::O2,
            OptLevel::O3 => Self::O3,
            OptLevel::Os => Self::Os,
            OptLevel::Oz => Self::Oz,
        }
    }
}

pub fn invoke_llvm_helper(
    toolchain: &ResolvedToolchain,
    opts: &CompileOptions,
    output: PathBuf,
) -> Result<PathBuf> {
    let helper = &toolchain.helper_path;
    if !helper.is_file() {
        bail!(
            "installed LLVM helper is missing at `{}` — reinstall with `fidan toolchain add llvm --version {}`",
            helper.display(),
            toolchain.metadata.toolchain_version
        );
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "fidan-llvm-helper-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    std::fs::create_dir_all(&temp_dir)
        .context("failed to create temporary LLVM helper directory")?;

    let request_path = temp_dir.join("request.json");
    let response_path = temp_dir.join("response.json");
    let request = LlvmCompileRequest {
        protocol_version: LLVM_BACKEND_PROTOCOL_VERSION,
        input: opts.input.clone(),
        output: output.clone(),
        opt_level: opts.opt_level.into(),
        emit_obj: opts.emit.contains(&crate::EmitKind::Obj),
        extra_lib_dirs: opts.extra_lib_dirs.clone(),
        link_dynamic: opts.link_dynamic,
    };
    let request_bytes =
        serde_json::to_vec_pretty(&request).context("failed to serialize LLVM compile request")?;
    std::fs::write(&request_path, request_bytes)
        .context("failed to write LLVM compile request file")?;

    let status = std::process::Command::new(helper)
        .arg("compile")
        .arg("--request")
        .arg(&request_path)
        .arg("--response")
        .arg(&response_path)
        .status()
        .with_context(|| format!("failed to launch LLVM helper `{}`", helper.display()))?;

    if !status.success() {
        bail!(
            "LLVM helper exited with status {} — reinstall the toolchain or use `--backend cranelift`",
            status
        );
    }

    let response_bytes =
        std::fs::read(&response_path).context("LLVM helper did not produce a response file")?;
    let response: LlvmCompileResponse =
        serde_json::from_slice(&response_bytes).context("failed to parse LLVM helper response")?;
    if response.protocol_version != LLVM_BACKEND_PROTOCOL_VERSION {
        bail!(
            "LLVM helper protocol mismatch (helper={}, cli={})",
            response.protocol_version,
            LLVM_BACKEND_PROTOCOL_VERSION
        );
    }
    if !response.success {
        bail!(
            "LLVM backend failed{}",
            response
                .message
                .as_deref()
                .map(|msg| format!(": {msg}"))
                .unwrap_or_default()
        );
    }

    Ok(response.output.unwrap_or(output))
}
