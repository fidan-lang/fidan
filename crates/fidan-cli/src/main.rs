use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::{CompileOptions, EmitKind, ExecutionMode, TraceMode};
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
        /// Print the call stack on uncaught panics: none | short | full | compact
        #[arg(long, default_value = "none")]
        trace: String,
        /// Stop after this many errors (0 = no limit)
        #[arg(long, default_value = "0")]
        max_errors: usize,
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
    /// Check a Fidan source file for errors without running it
    Check {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Stop after this many errors (0 = no limit)
        #[arg(long, default_value = "0")]
        max_errors: usize,
    },
    /// Apply high-confidence fix suggestions to a Fidan source file
    Fix {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Print the proposed changes without writing to the file
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the description of a diagnostic code (e.g. `E0101`, `W2001`)
    Explain {
        /// Diagnostic code
        code: String,
    },
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

fn parse_trace(raw: &str) -> Result<TraceMode> {
    match raw.trim().to_lowercase().as_str() {
        "none" | "" => Ok(TraceMode::None),
        "short" => Ok(TraceMode::Short),
        "full" => Ok(TraceMode::Full),
        "compact" => Ok(TraceMode::Compact),
        other => bail!(
            "unknown --trace mode {:?}  (valid: none, short, full, compact)",
            other
        ),
    }
}

fn run_explain(code: &str) {
    let entry = match fidan_diagnostics::lookup_code(code) {
        Some(e) => e,
        None => {
            eprintln!("  unknown diagnostic code `{code}`");
            eprintln!("  run `fidan explain` without arguments to list all codes");
            return;
        }
    };

    // Header line – mirrors the style used in error output
    let prefix = if code.starts_with('W') {
        "warning"
    } else if code.starts_with('R') {
        "runtime"
    } else {
        "error"
    };
    println!("{prefix}[{code}] — {}", entry.title);
    println!("category: {}", entry.category);
    println!();

    match fidan_diagnostics::explain(fidan_diagnostics::DiagCode(entry.code)) {
        Some(text) => print!("{text}"),
        None => println!("  (no extended explanation is available for this code yet)"),
    }
}

fn run_fix(file: PathBuf, dry_run: bool) -> Result<()> {
    use fidan_diagnostics::{Confidence, render_to_stderr};
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;
    let source_name = file.display().to_string();
    let source_map = Arc::new(SourceMap::new());
    let interner = Arc::new(SymbolInterner::new());
    let f = source_map.add_file(source_name.as_str(), src.as_str());
    let (tokens, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
    for d in &lex_diags {
        render_to_stderr(d, &source_map);
    }
    let (module, parse_diags) = fidan_parser::parse(&tokens, f.id, Arc::clone(&interner));
    for d in &parse_diags {
        render_to_stderr(d, &source_map);
    }
    let type_diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
    for d in &type_diags {
        render_to_stderr(d, &source_map);
    }

    // Collect all High-confidence machine-applicable edits.
    let mut edits: Vec<(u32, u32, String)> = vec![]; // (lo, hi, replacement)
    for diag in type_diags
        .iter()
        .chain(parse_diags.iter())
        .chain(lex_diags.iter())
    {
        for sug in &diag.suggestions {
            if sug.confidence == Confidence::High {
                if let Some(edit) = &sug.edit {
                    edits.push((edit.span.start, edit.span.end, edit.replacement.clone()));
                }
            }
        }
    }

    if edits.is_empty() {
        render_message_to_stderr(Severity::Note, "", "no high-confidence fixes available");
        return Ok(());
    }

    // Sort descending by byte offset — apply back-to-front to preserve earlier offsets.
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    edits.dedup_by_key(|e| e.0);

    let src_bytes = src.as_bytes();
    let mut patched = src.clone();
    for (lo, hi, replacement) in &edits {
        let lo = *lo as usize;
        let hi = (*hi as usize).min(patched.len());
        if dry_run {
            let line_start = src_bytes[..lo]
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let line_end = src_bytes[hi..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| hi + p)
                .unwrap_or(src.len());
            println!("\x1b[31m- {}\x1b[0m", &src[line_start..line_end]);
            let new_line = format!(
                "{}{}{}",
                &src[line_start..lo],
                replacement,
                &src[hi..line_end]
            );
            println!("\x1b[32m+ {}\x1b[0m", new_line);
        } else {
            patched.replace_range(lo..hi, replacement);
        }
    }

    if !dry_run {
        std::fs::write(&file, &patched).with_context(|| format!("cannot write {:?}", file))?;
        render_message_to_stderr(
            Severity::Note,
            "",
            &format!("applied {} fix(es) to {source_name}", edits.len()),
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    ensure_utf8_console();
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            file,
            emit,
            trace,
            max_errors,
        } => {
            let emit_kinds = parse_emit(&emit)?;
            let trace_mode = parse_trace(&trace)?;
            let opts = CompileOptions {
                input: file,
                output: None,
                mode: ExecutionMode::Interpret,
                emit: emit_kinds,
                trace: trace_mode,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
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
                ..Default::default()
            };
            run_pipeline(opts)
        }
        Command::Test { file } => {
            let opts = CompileOptions {
                input: file,
                mode: ExecutionMode::Test,
                ..Default::default()
            };
            run_pipeline(opts)
        }
        Command::Check { file, max_errors } => {
            let opts = CompileOptions {
                input: file,
                mode: ExecutionMode::Check,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                ..Default::default()
            };
            run_pipeline(opts)
        }
        Command::Fix { file, dry_run } => run_fix(file, dry_run),
        Command::Explain { code } => {
            run_explain(&code);
            Ok(())
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
            fidan_diagnostics::diag_code!("W2001"),
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
    let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    for diag in &lex_diags {
        fidan_diagnostics::render_to_stderr(diag, &source_map);
    }

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
        fidan_ast::print_module(&module, &interner);
        println!("\n=== ast counts: {source_name} ===");
        println!("  items: {}", module.items.len());
        println!("  exprs: {}", module.arena.exprs.len());
        println!("  stmts: {}", module.arena.stmts.len());
        println!("  items_arena: {}", module.arena.items.len());
    }
    // ── Type-check ────────────────────────────────────
    // Only proceed if parse is clean.
    let mut error_count: usize = lex_diags
        .iter()
        .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
        .count()
        + parse_diags
            .iter()
            .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
            .count();

    // Always run the full typed path so HIR/MIR emit has type information.
    let typed_module = if lex_diags.is_empty() && parse_diags.is_empty() {
        let tm = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        for diag in &tm.diagnostics {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
        }
        error_count += tm
            .diagnostics
            .iter()
            .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
            .count();
        Some(tm)
    } else {
        None
    };

    // ── --emit hir ─────────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Hir) {
        if let Some(ref tm) = typed_module {
            let hir = fidan_hir::lower_module(&module, tm);
            println!("=== hir: {source_name} ===");
            println!("  objects:    {}", hir.objects.len());
            println!("  functions:  {}", hir.functions.len());
            println!("  globals:    {}", hir.globals.len());
            println!("  init_stmts: {}", hir.init_stmts.len());
            for obj in &hir.objects {
                let name = interner.resolve(obj.name);
                let parent = obj.parent.map(|p| interner.resolve(p).to_string()).unwrap_or_default();
                if parent.is_empty() {
                    println!("  object {name}");
                } else {
                    println!("  object {name} extends {parent}");
                }
                for f in &obj.fields {
                    let fname = interner.resolve(f.name);
                    println!("    field {fname}: {:?}", f.ty);
                }
                for m in &obj.methods {
                    let mname = interner.resolve(m.name);
                    println!("    method {mname}() -> {:?}  ({} stmts)", m.return_ty, m.body.len());
                }
            }
            for func in &hir.functions {
                let fname = interner.resolve(func.name);
                let ext = func.extends.map(|e| format!(" extends {}", interner.resolve(e))).unwrap_or_default();
                println!("  action {fname}{ext}() -> {:?}  ({} stmts)", func.return_ty, func.body.len());
            }
            for g in &hir.globals {
                let gname = interner.resolve(g.name);
                println!("  global {}{gname}: {:?}", if g.is_const { "const " } else { "" }, g.ty);
            }
        } else {
            eprintln!("  (HIR not available — parse or lex errors present)");
        }
    }

    // ── --emit mir ─────────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Mir) {
        if let Some(ref tm) = typed_module {
            let hir = fidan_hir::lower_module(&module, tm);
            let mir = fidan_mir::lower_program(&hir, &interner);
            println!("=== mir: {source_name} ===");
            println!("  functions: {}", mir.functions.len());
            fidan_mir::print_program(&mir);
        } else {
            eprintln!("  (MIR not available — parse or lex errors present)");
        }
    }

    // ── Multi-error footer ───────────────────────────────
    if error_count > 0 {
        let s = if error_count == 1 { "" } else { "s" };
        let footer = match opts.mode {
            ExecutionMode::Check => format!("found {error_count} error{s} in `{source_name}`"),
            ExecutionMode::Interpret => {
                format!("could not run `{source_name}` — {error_count} error{s}")
            }
            _ => format!("could not compile `{source_name}` — {error_count} error{s}"),
        };
        render_message_to_stderr(Severity::Note, "", &footer);
        if opts.mode != ExecutionMode::Check {
            eprintln!(
                "         run `fidan check` to list all errors, or `--max-errors N` to stop early"
            );
        }
    }
    match opts.mode {
        ExecutionMode::Interpret => {
            if error_count == 0 {
                // ── MIR pipeline (Phase 6) ────────────────────────────────────
                let result = if let Some(ref tm) = typed_module {
                    let hir = fidan_hir::lower_module(&module, tm);
                    let mut mir = fidan_mir::lower_program(&hir, &interner);
                    fidan_passes::run_all(&mut mir);
                    fidan_interp::run_mir(mir, Arc::clone(&interner))
                } else {
                    // Fallback: should never happen since error_count == 0 implies typed_module.
                    Ok(())
                };
                if let Err(err) = result {
                    render_message_to_stderr(
                        Severity::Error,
                        fidan_diagnostics::diag_code!("R0001"),
                        &err.message,
                    );
                    if !err.trace.is_empty() && opts.trace != TraceMode::None {
                        let frames: &[String] = match opts.trace {
                            TraceMode::Short => &err.trace[..err.trace.len().min(5)],
                            _ => &err.trace,
                        };
                        if opts.trace == TraceMode::Compact {
                            eprintln!("  stack: {}", frames.join(" ← "));
                        } else {
                            eprintln!("  stack trace (innermost first):");
                            for (i, frame) in frames.iter().enumerate() {
                                if i == 0 {
                                    eprintln!("    #{i}  {frame}  ← panicked here");
                                } else if let Some(outer) = frames.get(i + 1) {
                                    eprintln!("    #{i}  {frame}  ← called by {outer}");
                                } else {
                                    eprintln!("    #{i}  {frame}");
                                }
                            }
                        }
                    }
                }
            }
        }
        ExecutionMode::Check => {
            // Parse + typecheck already ran above; non-zero exit if errors found.
            if error_count > 0 {
                std::process::exit(1);
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

/// Interactive lex + parse + typecheck + interpret loop.
///
/// Each line is treated as a self-contained Fidan snippet.  The persistent
/// [`TypeChecker`] accumulates symbol definitions across lines so names defined
/// on line N are visible on line N+1.  The interpreter runs after every clean
/// type-check so side effects (print, etc.) are visible immediately.
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
    let prompt = "ƒ>  ";

    // Persist the interner so symbol IDs are stable across REPL lines.
    let interner = Arc::new(SymbolInterner::new());

    // A stable boot file gives the TypeChecker a FileId for its internal
    // dummy spans (built-in registrations).  Because each per-line SourceMap
    // also starts its counter at 0, all spans always resolve correctly.
    let boot_map = Arc::new(SourceMap::new());
    let boot_file = boot_map.add_file("<repl>", "");
    let boot_fid = boot_file.id;

    // ONE persistent TypeChecker for the whole session.  Symbol definitions
    // accumulate across lines, so `greeting` defined on line 1 is visible on
    // line 2 — exactly how a real REPL should behave.  :reset replaces it.
    let mut tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
    tc.set_repl(true);

    // ONE persistent interpreter state, same rationale: variables and actions
    // defined on earlier lines must be visible on later lines.
    let mut repl_state = fidan_interp::new_repl_state(Arc::clone(&interner));

    let mut line_no: u32 = 0;
    // Rolling buffer of recent runtime errors for `:last [--full]`.
    let mut error_history: Vec<String> = Vec::new();

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
                    tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
                    tc.set_repl(true);
                    repl_state = fidan_interp::new_repl_state(Arc::clone(&interner));
                    println!("  (session state cleared)");
                    continue;
                }

                "help" => {
                    println!("  :help               show this message");
                    println!("  :exit / :quit / :q  leave the REPL");
                    println!("  :clear / :cls       clear the terminal (also Ctrl+L)");
                    println!("  :reset              clear the session state");
                    println!("  :ast  <snippet>     show the parsed AST node counts (debug)");
                    println!("  :type <expr>        print the inferred type of an expression");
                    println!(
                        "  :last [--full]      show last runtime error (--full shows all recent)"
                    );
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
                    let (toks, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
                    for d in &lex_diags {
                        fidan_diagnostics::render_to_stderr(d, &smap);
                    }
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

                // ── :type <expr>  ────────────────────────────────────────
                "type" => {
                    if cmd_arg.is_empty() {
                        eprintln!("  usage: :type <expr>");
                        continue;
                    }
                    line_no += 1;
                    let sname = format!("<repl:{line_no}>");
                    let smap = Arc::new(SourceMap::new());
                    let f = smap.add_file(sname.as_str(), cmd_arg);
                    let (toks, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
                    for d in &lex_diags {
                        fidan_diagnostics::render_to_stderr(d, &smap);
                    }
                    let (m, parse_diags) = fidan_parser::parse(&toks, f.id, Arc::clone(&interner));
                    for d in &parse_diags {
                        fidan_diagnostics::render_to_stderr(d, &smap);
                    }
                    if lex_diags.is_empty() && parse_diags.is_empty() {
                        match tc.infer_snippet_type(&m) {
                            Some(ty_name) => println!("  : {ty_name}"),
                            None => eprintln!("  (snippet has no bare expression to infer)"),
                        }
                        let _ = tc.drain_diags(); // discard type errors — :type is query-only
                    }
                    continue;
                }

                // ── :last [--full]  ────────────────────────────────────────
                "last" => {
                    if error_history.is_empty() {
                        println!("  (no errors recorded this session)");
                    } else if cmd_arg == "--full" {
                        for (i, msg) in error_history.iter().enumerate().rev() {
                            println!("  [{}]  {}", i + 1, msg);
                        }
                    } else {
                        println!("  {}", error_history.last().unwrap());
                        if error_history.len() > 1 {
                            println!(
                                "  ({} total — :last --full to see all)",
                                error_history.len()
                            );
                        }
                    }
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
        let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        for diag in &lex_diags {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
        }

        let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));

        for diag in &parse_diags {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
        }

        if lex_diags.is_empty() && parse_diags.is_empty() {
            tc.check_module(&module);
            let type_diags = tc.drain_diags();
            if type_diags.is_empty() {
                match fidan_interp::run_repl_line(&mut repl_state, &module) {
                    Ok(Some(echo)) => println!("{echo}"),
                    Ok(None) => {}
                    Err(msg) => {
                        render_message_to_stderr(
                            Severity::Error,
                            fidan_diagnostics::diag_code!("R0001"),
                            &msg,
                        );
                        error_history.push(msg);
                    }
                }
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
