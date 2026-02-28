use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::{CompileOptions, EmitKind, ExecutionMode};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name    = "fidan",
    version,
    about   = "The Fidan language compiler and toolchain",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a Fidan source file using the interpreter
    Run {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Emit intermediate representation: tokens | ast | hir | mir
        #[arg(long, value_delimiter = ',')]
        emit: Vec<String>,
    },
    /// Compile a Fidan source file to a native binary
    Build {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Output binary path
        #[arg(short, long, default_value = "out")]
        output: PathBuf,
        /// Enable release optimisations (requires LLVM)
        #[arg(long)]
        release: bool,
        /// Emit intermediate representation: tokens | ast | hir | mir
        #[arg(long, value_delimiter = ',')]
        emit: Vec<String>,
    },
    /// Run `test { ... }` blocks in a Fidan source file
    Test { file: PathBuf },
    /// Start the language server (LSP)
    Lsp,
}

fn parse_emit(raw: &[String]) -> Result<Vec<EmitKind>> {
    raw.iter()
        .map(|s| match s.trim().to_lowercase().as_str() {
            "tokens" => Ok(EmitKind::Tokens),
            "ast" => Ok(EmitKind::Ast),
            "hir" => Ok(EmitKind::Hir),
            "mir" => Ok(EmitKind::Mir),
            other => bail!(
                "unknown --emit target {:?}  (valid: tokens, ast, hir, mir)",
                other
            ),
        })
        .collect()
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run { file, emit } => {
            let emit_kinds = parse_emit(&emit)?;
            let opts = CompileOptions {
                input: file,
                output: None,
                mode: ExecutionMode::Interpret,
                emit: emit_kinds,
            };
            run_pipeline(opts)
        }
        Command::Build {
            file, output, emit, ..
        } => {
            let emit_kinds = parse_emit(&emit)?;
            let opts = CompileOptions {
                input: file,
                output: Some(output),
                mode: ExecutionMode::Build,
                emit: emit_kinds,
            };
            run_pipeline(opts)
        }
        Command::Test { file } => {
            let opts = CompileOptions {
                input: file,
                output: None,
                mode: ExecutionMode::Test,
                emit: vec![],
            };
            run_pipeline(opts)
        }
        Command::Lsp => {
            render_message_to_stderr(
                Severity::Note,
                "unimplemented",
                "LSP server not yet implemented (Phase 10)",
            );
            Ok(())
        }
    }
}

fn run_pipeline(opts: CompileOptions) -> Result<()> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    // ── Extension check ────────────────────────────────────────────────────────
    if opts.input.extension().and_then(|e| e.to_str()) != Some("fdn") {
        render_message_to_stderr(
            Severity::Warning,
            "W001",
            &format!(
                "file '{}' does not have the '.fdn' extension",
                opts.input.display()
            ),
        );
    }

    // ── Load source ────────────────────────────────────────────────────────────
    let src = std::fs::read_to_string(&opts.input)
        .with_context(|| format!("cannot read {:?}", opts.input))?;

    let source_map = Arc::new(SourceMap::new());
    let file = source_map.add_file(opts.input.display().to_string().as_str(), src.as_str());

    let interner = Arc::new(SymbolInterner::new());

    // ── Lex ────────────────────────────────────────────────────────────────────
    let tokens = Lexer::new(&file, Arc::clone(&interner)).tokenise();

    // ── --emit tokens ──────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Tokens) {
        println!("=== tokens: {} ===", opts.input.display());
        for tok in &tokens {
            println!("  {:?}", tok);
        }
    }

    // ── Remaining stages (not yet implemented) ─────────────────────────────────
    if opts.emit.contains(&EmitKind::Ast)
        || opts.emit.contains(&EmitKind::Hir)
        || opts.emit.contains(&EmitKind::Mir)
    {
        render_message_to_stderr(
            Severity::Note,
            "unimplemented",
            "--emit ast/hir/mir not yet implemented (Phase 2+)",
        );
    }

    match opts.mode {
        ExecutionMode::Interpret => {
            if !opts.emit.contains(&EmitKind::Tokens) {
                render_message_to_stderr(
                    Severity::Note,
                    "unimplemented",
                    "interpreter not yet implemented (Phase 5)",
                );
            }
        }
        ExecutionMode::Build => {
            if !opts.emit.iter().any(|e| *e == EmitKind::Tokens) {
                render_message_to_stderr(
                    Severity::Note,
                    "unimplemented",
                    "AOT backend not yet implemented (Phase 8/11)",
                );
            }
        }
        ExecutionMode::Test => {
            render_message_to_stderr(
                Severity::Note,
                "unimplemented",
                "test runner not yet implemented (Phase 7)",
            );
        }
    }

    Ok(())
}
