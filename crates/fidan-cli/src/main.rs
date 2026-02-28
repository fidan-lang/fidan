use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::{CompileOptions, EmitKind, ExecutionMode};
use std::path::PathBuf;

// On Windows, PowerShell's default code page is CP1252 which corrupts the UTF-8
// box-drawing characters emitted by ariadne.  Switch both the input and output
// console code pages to UTF-8 (65001) as early as possible.
#[cfg(target_os = "windows")]
fn ensure_utf8_console() {
    unsafe extern "system" {
        fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
        fn SetConsoleCP(wCodePageID: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}
#[cfg(not(target_os = "windows"))]
fn ensure_utf8_console() {}

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
    /// Run a Fidan source file (pass `-` to read from stdin)
    Run {
        /// Path to the .fdn source file, or `-` to read from stdin
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
    /// Start an interactive REPL (lex + parse + typecheck each line)
    Repl,
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
    ensure_utf8_console();
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
        Command::Repl => run_repl(),
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

    let is_stdin = opts.input.as_os_str() == "-";

    // ── Extension check (skipped for stdin) ───────────────────────────────────
    if !is_stdin && opts.input.extension().and_then(|e| e.to_str()) != Some("fdn") {
        render_message_to_stderr(
            Severity::Warning,
            "W2001",
            &format!(
                "file '{}' does not have the '.fdn' extension",
                opts.input.display()
            ),
        );
    }

    // ── Load source ────────────────────────────────────────────────────────────
    let (source_name, src) = if is_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("cannot read from stdin")?;
        ("<stdin>".to_string(), buf)
    } else {
        let s = std::fs::read_to_string(&opts.input)
            .with_context(|| format!("cannot read {:?}", opts.input))?;
        (opts.input.display().to_string(), s)
    };

    let source_map = Arc::new(SourceMap::new());
    let file = source_map.add_file(source_name.as_str(), src.as_str());

    let interner = Arc::new(SymbolInterner::new());

    // ── Lex ────────────────────────────────────────────────────────────────────
    let tokens = Lexer::new(&file, Arc::clone(&interner)).tokenise();

    // ── --emit tokens ──────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Tokens) {
        println!("=== tokens: {source_name} ===");
        for tok in &tokens {
            println!("  {:?}", tok);
        }
    }

    // ── Parse ──────────────────────────────────────────────────────────────────
    let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));

    // Surface parse diagnostics via the diagnostics renderer
    for diag in &parse_diags {
        fidan_diagnostics::render_to_stderr(diag, &source_map);
    }

    // ── --emit ast ─────────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Ast) {
        println!("=== ast: {source_name} ===");
        println!("  items: {}", module.items.len());
        println!("  exprs: {}", module.arena.exprs.len());
        println!("  stmts: {}", module.arena.stmts.len());
        println!("  items_arena: {}", module.arena.items.len());
    }
    // ── Type-check ────────────────────────────────────
    // Only proceed if parse is clean.
    let mut error_count: usize = parse_diags
        .iter()
        .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
        .count();

    if parse_diags.is_empty() {
        let type_diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
        for diag in &type_diags {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
        }
        error_count += type_diags
            .iter()
            .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
            .count();
    }

    // ── Multi-error footer ───────────────────────────────
    if error_count > 0 {
        let s = if error_count == 1 { "" } else { "s" };
        render_message_to_stderr(
            Severity::Note,
            "",
            &format!("could not compile `{source_name}` — {error_count} error{s}"),
        );
        eprintln!(
            "         run `fidan check` to list all errors, or `--max-errors N` to stop early"
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

// ── REPL helper ─────────────────────────────────────────────────────────────────────
//
// Implementing the `Helper` bundle lets us colour the prompt via `Highlighter`
// while rustyline calculates cursor position from the *plain* prompt string.
// This avoids the over-wide cursor that appears when ANSI codes are embedded
// directly in the prompt string (even with \x01/\x02 guards on Windows).

struct ReplHelper;

impl rustyline::Helper for ReplHelper {}

impl rustyline::completion::Completer for ReplHelper {
    type Candidate = rustyline::completion::Pair;
}

impl rustyline::hint::Hinter for ReplHelper {
    type Hint = String;
}

impl rustyline::validate::Validator for ReplHelper {}

impl rustyline::highlight::Highlighter for ReplHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> std::borrow::Cow<'b, str> {
        // Wrap the plain prompt in bold-cyan colour codes for display only.
        // rustyline never passes this string through its width logic.
        std::borrow::Cow::Owned(format!("\x1b[1;36m{prompt}\x1b[0m"))
    }

    fn highlight_char(
        &self,
        _line: &str,
        _pos: usize,
        _kind: rustyline::highlight::CmdKind,
    ) -> bool {
        false
    }
}

// ── REPL ─────────────────────────────────────────────────────────────────────────────

/// Interactive lex + parse + typecheck loop.
///
/// Each line is treated as a self-contained Fidan snippet.  The interpreter
/// (Phase 5) will be wired here once available — for now this REPL is useful
/// for exploring the parser and type-checker interactively.
fn run_repl() -> Result<()> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use rustyline::error::ReadlineError;
    use std::sync::Arc;

    // ── Banner ─────────────────────────────────────────────────────────────
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    println!(
        "Fidan {} ({}) on {}/{}",
        env!("CARGO_PKG_VERSION"),
        profile,
        os,
        arch
    );
    println!(
        "Type :help for commands. Ctrl+C cancels a line, Ctrl+L clears screen, Ctrl+D to exit."
    );
    println!();

    // ── rustyline editor ───────────────────────────────────────────────────
    // Using Editor<ReplHelper> so the Highlighter colours the prompt while
    // rustyline measures cursor position from the plain string only.
    let mut rl = rustyline::Editor::<ReplHelper, rustyline::history::DefaultHistory>::new()?;
    rl.set_helper(Some(ReplHelper));
    // Ctrl+L is bound to ClearScreen by rustyline's default Emacs keymap.

    // Plain string — no escape codes.  ReplHelper::highlight_prompt wraps it
    // in colour for display; rustyline uses this for all width arithmetic.
    let prompt = " ƒ>  ";

    // Persist the interner so symbol IDs are stable across REPL lines.
    let interner = Arc::new(SymbolInterner::new());
    let mut line_no: u32 = 0;

    loop {
        let line = match rl.readline(prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C — cancel the current line and re-prompt (do not exit).
                continue;
            }
            Err(ReadlineError::Eof) => break, // Ctrl+D / Ctrl+Z+Enter
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Push to history so up/down arrows work.
        let _ = rl.add_history_entry(trimmed);

        // ── Colon commands ────────────────────────────────────────
        if let Some(cmd) = trimmed.strip_prefix(':') {
            // Split on first space: ":ast var x = 1" → ("ast", "var x = 1")
            let (cmd_word, cmd_arg) = cmd
                .trim()
                .split_once(' ')
                .map(|(w, a)| (w, a.trim()))
                .unwrap_or((cmd.trim(), ""));

            match cmd_word {
                "exit" | "quit" | "q" => break,

                "reset" => {
                    println!("  (session state cleared)");
                    // True eval-state reset happens in Phase 5.
                    continue;
                }

                "help" => {
                    println!("  :help               show this message");
                    println!("  :exit / :quit / :q  leave the REPL");
                    println!("  :clear / :cls       clear the terminal (also Ctrl+L)");
                    println!("  :reset              clear the session state");
                    println!("  :ast  <snippet>     show the parsed AST node counts (debug)");
                    println!("  :type <expr>        print the inferred type (Phase 5)");
                    println!("  :last [--full]      show the last error's cause chain (Phase 5)");
                    continue;
                }

                // ── :clear / :cls ─────────────────────────────────────────
                "clear" | "cls" => {
                    // ANSI: erase display + move cursor to top-left.
                    print!("\x1b[2J\x1b[1;1H");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    continue;
                }

                // ── :ast <snippet> ────────────────────────────────────────
                "ast" => {
                    if cmd_arg.is_empty() {
                        eprintln!("  usage: :ast <snippet>");
                        continue;
                    }
                    line_no += 1;
                    let sname = format!("<repl:{line_no}>");
                    let smap = Arc::new(SourceMap::new());
                    let f = smap.add_file(sname.as_str(), cmd_arg);
                    let toks = Lexer::new(&f, Arc::clone(&interner)).tokenise();
                    let (m, ast_diags) = fidan_parser::parse(&toks, f.id, Arc::clone(&interner));
                    for d in &ast_diags {
                        fidan_diagnostics::render_to_stderr(d, &smap);
                    }
                    if ast_diags.is_empty() {
                        println!("  items : {}", m.items.len());
                        println!("  exprs : {}", m.arena.exprs.len());
                        println!("  stmts : {}", m.arena.stmts.len());
                    }
                    continue;
                }

                // ── :type <expr>  (Phase 5) ───────────────────────────────
                "type" => {
                    eprintln!(
                        "  :type — full type inference in the REPL is not yet implemented (Phase 5)"
                    );
                    continue;
                }

                // ── :last [--full]  (Phase 5) ─────────────────────────────
                "last" => {
                    eprintln!("  :last — error history is not yet implemented (Phase 5)");
                    continue;
                }

                other => {
                    eprintln!("  unknown command `:{other}`. Type :help for a list.");
                    continue;
                }
            }
        }

        // ── Lex + parse + typecheck ───────────────────────────────
        line_no += 1;
        let source_name = format!("<repl:{line_no}>");
        let source_map = Arc::new(SourceMap::new());
        let file = source_map.add_file(source_name.as_str(), trimmed);
        let tokens = Lexer::new(&file, Arc::clone(&interner)).tokenise();

        let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));

        for diag in &parse_diags {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
        }

        if parse_diags.is_empty() {
            let type_diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
            if type_diags.is_empty() {
                println!("  ok");
            } else {
                for diag in &type_diags {
                    fidan_diagnostics::render_to_stderr(diag, &source_map);
                }
            }
        }
    }

    println!();
    println!("Bye! 👋");
    Ok(())
}
