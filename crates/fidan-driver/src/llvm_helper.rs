use crate::install::ResolvedToolchain;
use crate::{CompileOptions, LtoMode, OptLevel, StripMode};
use anyhow::{Context, Result, bail};
use fidan_mir::MirProgram;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const LLVM_BACKEND_PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlvmCompileRequest {
    pub protocol_version: u32,
    pub input: PathBuf,
    pub output: PathBuf,
    pub runtime_dir: PathBuf,
    pub payload: LlvmBackendPayload,
    pub opt_level: SerializableOptLevel,
    pub lto: SerializableLtoMode,
    pub strip: SerializableStripMode,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializableLtoMode {
    Off,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializableStripMode {
    Off,
    Symbols,
    All,
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

impl From<LtoMode> for SerializableLtoMode {
    fn from(value: LtoMode) -> Self {
        match value {
            LtoMode::Off => Self::Off,
            LtoMode::Full => Self::Full,
        }
    }
}

impl From<StripMode> for SerializableStripMode {
    fn from(value: StripMode) -> Self {
        match value {
            StripMode::Off => Self::Off,
            StripMode::Symbols => Self::Symbols,
            StripMode::All => Self::All,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlvmBackendPayload {
    pub program: MirProgram,
    pub symbols: Vec<String>,
}

pub fn invoke_llvm_helper(
    toolchain: &ResolvedToolchain,
    program: &MirProgram,
    symbols: Vec<String>,
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

    let runtime_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.to_path_buf()))
        .context("failed to resolve the running Fidan installation directory")?;

    let request = LlvmCompileRequest {
        protocol_version: LLVM_BACKEND_PROTOCOL_VERSION,
        input: opts.input.clone(),
        output: output.clone(),
        runtime_dir,
        payload: LlvmBackendPayload {
            program: program.clone(),
            symbols,
        },
        opt_level: opts.opt_level.into(),
        lto: opts.lto.into(),
        strip: opts.strip.into(),
        emit_obj: opts.emit.contains(&crate::EmitKind::Obj),
        extra_lib_dirs: opts.extra_lib_dirs.clone(),
        link_dynamic: opts.link_dynamic,
    };
    let request_bytes =
        serde_json::to_vec(&request).context("failed to serialize LLVM compile request")?;

    let mut command = Command::new(helper);
    configure_helper_environment(&mut command, toolchain);

    let mut child = command
        .arg("compile")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to launch LLVM helper `{}`", helper.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        stdin
            .write_all(&request_bytes)
            .context("failed to send LLVM compile request to helper")?;
    }

    let output_result = child
        .wait_with_output()
        .context("failed while waiting for LLVM helper to finish")?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr)
            .trim()
            .to_string();
        bail!(
            "LLVM helper exited with status {}{}",
            output_result.status,
            if stderr.is_empty() {
                " — reinstall the toolchain or use `--backend cranelift`".to_string()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let response: LlvmCompileResponse = serde_json::from_slice(&output_result.stdout)
        .context("failed to parse LLVM helper response")?;
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

fn configure_helper_environment(command: &mut Command, toolchain: &ResolvedToolchain) {
    let llvm_bin = toolchain.root.join("llvm").join("bin");

    if llvm_bin.is_dir() {
        prepend_env_path(command, "PATH", &llvm_bin);
    }
}

fn prepend_env_path(command: &mut Command, key: &str, value: &std::path::Path) {
    let existing = std::env::var_os(key).unwrap_or_default();
    let mut paths = vec![value.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env(key, joined);
    }
}
