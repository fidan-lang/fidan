use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fidan_driver::{LLVM_BACKEND_PROTOCOL_VERSION, LlvmCompileRequest, LlvmCompileResponse};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "fidan-llvm-helper",
    about = "LLVM helper process for Fidan toolchains"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a request emitted by the Fidan CLI
    Compile {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        response: PathBuf,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fidan-llvm-helper: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Compile { request, response } => handle_compile(&request, &response),
    }
}

fn handle_compile(request_path: &PathBuf, response_path: &PathBuf) -> Result<()> {
    let request_bytes = std::fs::read(request_path)
        .with_context(|| format!("failed to read `{}`", request_path.display()))?;
    let request: LlvmCompileRequest = serde_json::from_slice(&request_bytes)
        .with_context(|| format!("failed to parse `{}`", request_path.display()))?;

    let response = if request.protocol_version != LLVM_BACKEND_PROTOCOL_VERSION {
        LlvmCompileResponse {
            protocol_version: LLVM_BACKEND_PROTOCOL_VERSION,
            success: false,
            output: None,
            message: Some(format!(
                "LLVM helper protocol mismatch (request={}, helper={})",
                request.protocol_version, LLVM_BACKEND_PROTOCOL_VERSION
            )),
        }
    } else {
        LlvmCompileResponse {
            protocol_version: LLVM_BACKEND_PROTOCOL_VERSION,
            success: false,
            output: None,
            message: Some(
                "LLVM backend is not implemented yet in this Fidan toolchain build".to_string(),
            ),
        }
    };

    if let Some(parent) = response_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let response_bytes =
        serde_json::to_vec_pretty(&response).context("failed to serialize LLVM response")?;
    std::fs::write(response_path, response_bytes)
        .with_context(|| format!("failed to write `{}`", response_path.display()))?;
    Ok(())
}
