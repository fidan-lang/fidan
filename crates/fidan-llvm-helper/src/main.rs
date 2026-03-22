use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fidan_codegen_llvm::{CompileRequest as LlvmBackendRequest, compile_request};
use fidan_driver::{
    LLVM_BACKEND_PROTOCOL_VERSION, LlvmCompileRequest, LlvmCompileResponse, SerializableLtoMode,
    SerializableOptLevel, SerializableStripMode,
};
use std::io::{Read, Write};
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
        request: Option<PathBuf>,
        #[arg(long)]
        response: Option<PathBuf>,
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
        Command::Compile { request, response } => {
            handle_compile(request.as_ref(), response.as_ref())
        }
    }
}

fn handle_compile(request_path: Option<&PathBuf>, response_path: Option<&PathBuf>) -> Result<()> {
    match (request_path, response_path) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => anyhow::bail!(
            "`compile` expects either both --request/--response paths or neither (stdin/stdout mode)"
        ),
    }

    let request_bytes = match request_path {
        Some(request_path) => std::fs::read(request_path)
            .with_context(|| format!("failed to read `{}`", request_path.display()))?,
        None => {
            let mut request_bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut request_bytes)
                .context("failed to read LLVM request from stdin")?;
            request_bytes
        }
    };
    let request: LlvmCompileRequest =
        serde_json::from_slice(&request_bytes).context("failed to parse LLVM compile request")?;

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
        let backend_request = to_backend_request(request);
        let helper_path =
            std::env::current_exe().context("failed to resolve current LLVM helper executable")?;
        match compile_request(&helper_path, &backend_request) {
            Ok(output) => LlvmCompileResponse {
                protocol_version: LLVM_BACKEND_PROTOCOL_VERSION,
                success: true,
                output: Some(output),
                message: None,
            },
            Err(error) => LlvmCompileResponse {
                protocol_version: LLVM_BACKEND_PROTOCOL_VERSION,
                success: false,
                output: None,
                message: Some(format!("{error:#}")),
            },
        }
    };

    let response_bytes =
        serde_json::to_vec(&response).context("failed to serialize LLVM response")?;
    match response_path {
        Some(response_path) => {
            if let Some(parent) = response_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create `{}`", parent.display()))?;
            }
            std::fs::write(response_path, response_bytes)
                .with_context(|| format!("failed to write `{}`", response_path.display()))?;
        }
        None => {
            std::io::stdout()
                .write_all(&response_bytes)
                .context("failed to write LLVM response to stdout")?;
        }
    }
    Ok(())
}

fn to_backend_request(value: LlvmCompileRequest) -> LlvmBackendRequest {
    LlvmBackendRequest {
        input: value.input,
        output: value.output,
        runtime_dir: value.runtime_dir,
        payload: fidan_codegen_llvm::BackendPayload {
            program: value.payload.program,
            symbols: value.payload.symbols,
        },
        opt_level: match value.opt_level {
            SerializableOptLevel::O0 => fidan_codegen_llvm::OptLevel::O0,
            SerializableOptLevel::O1 => fidan_codegen_llvm::OptLevel::O1,
            SerializableOptLevel::O2 => fidan_codegen_llvm::OptLevel::O2,
            SerializableOptLevel::O3 => fidan_codegen_llvm::OptLevel::O3,
            SerializableOptLevel::Os => fidan_codegen_llvm::OptLevel::Os,
            SerializableOptLevel::Oz => fidan_codegen_llvm::OptLevel::Oz,
        },
        lto: match value.lto {
            SerializableLtoMode::Off => fidan_codegen_llvm::LtoMode::Off,
            SerializableLtoMode::Full => fidan_codegen_llvm::LtoMode::Full,
        },
        strip: match value.strip {
            SerializableStripMode::Off => fidan_codegen_llvm::StripMode::Off,
            SerializableStripMode::Symbols => fidan_codegen_llvm::StripMode::Symbols,
            SerializableStripMode::All => fidan_codegen_llvm::StripMode::All,
        },
        emit_obj: value.emit_obj,
        extra_lib_dirs: value.extra_lib_dirs,
        link_dynamic: value.link_dynamic,
    }
}
