use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "fidan", version, about = "The Fidan language compiler and toolchain")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a Fidan source file using the interpreter
    Run {
        /// Path to the .fdn source file
        file: std::path::PathBuf,
        /// Emit intermediate representation (tokens | ast | hir | mir)
        #[arg(long)]
        emit: Option<String>,
    },
    /// Compile a Fidan source file to a native binary
    Build {
        /// Path to the .fdn source file
        file: std::path::PathBuf,
        /// Output binary path
        #[arg(short, long, default_value = "out")]
        output: std::path::PathBuf,
        /// Enable release optimisations (requires LLVM)
        #[arg(long)]
        release: bool,
    },
    /// Run tests in a Fidan source file
    Test {
        file: std::path::PathBuf,
    },
    /// Start the language server
    Lsp,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run { file, emit } => {
            eprintln!("[fidan] run: {:?}  emit={:?}", file, emit);
            eprintln!("Interpreter not yet implemented — Phase 5.");
        }
        Command::Build { file, output, release } => {
            eprintln!("[fidan] build: {:?} -> {:?}  release={}", file, output, release);
            eprintln!("AOT backend not yet implemented — Phase 8/11.");
        }
        Command::Test { file } => {
            eprintln!("[fidan] test: {:?}", file);
            eprintln!("Test runner not yet implemented — Phase 7.");
        }
        Command::Lsp => {
            eprintln!("[fidan] lsp: not yet implemented — Phase 10.");
        }
    }

    Ok(())
}
