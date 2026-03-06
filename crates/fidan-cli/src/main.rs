#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
        /// JIT compilation threshold: compile a function after this many calls (0 = off)
        #[arg(long, default_value = "500")]
        jit_threshold: u32,
        /// Treat select warnings (W1001–W1003, W2004–W2006) as errors
        #[arg(long)]
        strict: bool,
        /// Watch source files and re-run automatically on change
        #[arg(long)]
        reload: bool,
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
    Repl {
        /// Print the call stack on uncaught panics: none | short | full | compact
        #[arg(long, default_value = "short")]
        trace: String,
    },
    /// Start the language server (LSP)
    Lsp {
        /// Communicate over stdin/stdout.
        /// This flag is accepted for compatibility with LSP clients (e.g. VS Code's
        /// vscode-languageclient) that automatically append `--stdio` to the server
        /// process arguments when `TransportKind.stdio` is configured.  The Fidan
        /// LSP server always uses stdio, so the flag is a no-op.
        #[arg(long)]
        stdio: bool,
    },
    /// Format a Fidan source file
    Format {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Rewrite the file in place instead of printing to stdout
        #[arg(long)]
        in_place: bool,
        /// Exit 1 if the file is not already formatted (useful in CI)
        #[arg(long)]
        check: bool,
        /// Number of spaces per indent level (default: 4)
        #[arg(long)]
        indent_width: Option<usize>,
        /// Soft line-length limit (default: 100)
        #[arg(long)]
        max_line_len: Option<usize>,
    },
    /// Check a Fidan source file for errors without running it
    Check {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Stop after this many errors (0 = no limit)
        #[arg(long, default_value = "0")]
        max_errors: usize,
        /// Treat select warnings (W1001–W1003, W2004–W2006) as errors
        #[arg(long)]
        strict: bool,
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
    /// Scaffold a new Fidan project in a new directory
    New {
        /// Name of the project (also the directory name)
        project_name: String,
        /// Output directory (default: current directory)
        #[arg(short, long)]
        dir: Option<PathBuf>,
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

// ── Hot Reload ────────────────────────────────────────────────────────────────

/// Run a Fidan program and re-run it whenever any watched source file changes.
///
/// On each file-save event the full pipeline is re-run from scratch (lex →
/// parse → typecheck → HIR/MIR → interpret).  This is clean and correct
/// because all pipeline state is freshly constructed per run.
fn run_with_reload(opts: CompileOptions) -> Result<()> {
    use notify::{Event, RecursiveMode, Watcher, recommended_watcher};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let watch_path = opts.input.clone();

    // Collect the initial set of files to watch.  After Phase 7 the import
    // system is in place, so we watch the entry point + all `.fdn` siblings
    // in the same directory as a pragmatic approximation.
    let watch_dir = watch_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = recommended_watcher(tx)?;
    watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

    eprintln!(
        "\x1b[2m[reload] watching {} — Ctrl+C to stop\x1b[0m",
        watch_dir.display()
    );

    // Run once immediately on startup.
    run_pipeline(opts.clone())?;

    // Debounce: ignore events that arrive within 100 ms of the last one.
    let debounce = Duration::from_millis(100);
    let mut last_event = Instant::now() - debounce;

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                // Only react to write/create/remove events on `.fdn` files.
                let is_fdn = event
                    .paths
                    .iter()
                    .any(|p| p.extension().and_then(|e| e.to_str()) == Some("fdn"));
                if !is_fdn {
                    continue;
                }
                // Drain any queued events to debounce.
                while rx.try_recv().is_ok() {}
                if last_event.elapsed() < debounce {
                    continue;
                }
                last_event = Instant::now();

                // Print a brief diff summary.
                let changed: Vec<String> = event
                    .paths
                    .iter()
                    .filter_map(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                eprintln!(
                    "\x1b[2m[reload] {} changed — re-running\x1b[0m",
                    changed.join(", ")
                );

                // Re-run — errors are printed but do not stop the watcher.
                let _ = run_pipeline(opts.clone());
            }
            Ok(Err(e)) => eprintln!("[reload] watcher error: {e}"),
            Err(_) => break, // channel closed
        }
    }
    Ok(())
}

fn run_new(project_name: &str, parent_dir: Option<&PathBuf>) -> Result<()> {
    let base = parent_dir
        .cloned()
        .unwrap_or_else(|| std::env::current_dir().expect("cannot read cwd"));
    let project_dir = base.join(project_name);

    if project_dir.exists() {
        bail!("directory {:?} already exists", project_dir);
    }

    std::fs::create_dir_all(&project_dir)
        .with_context(|| format!("cannot create directory {:?}", project_dir))?;

    // main.fdn — boilerplate entry point
    let main_src = format!(
        concat!(
            "# {name}\n",
            "# Entry point — run with: fidan run main.fdn\n",
            "\n",
            "action main() {{\n",
            "    print(\"Hello from {name}!\")\n",
            "}}\n",
            "\n",
            "main()\n"
        ),
        name = project_name
    );
    std::fs::write(project_dir.join("main.fdn"), &main_src)
        .with_context(|| "cannot write main.fdn")?;

    render_message_to_stderr(
        Severity::Note,
        "",
        &format!(
            "created project `{}` — run: fidan run {}/main.fdn",
            project_name, project_name
        ),
    );
    Ok(())
}

fn run_fmt(
    file: PathBuf,
    in_place: bool,
    check: bool,
    indent_width: Option<usize>,
    max_line_len: Option<usize>,
) -> Result<()> {
    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;

    let mut opts = fidan_fmt::FormatOptions::default();
    if let Some(w) = indent_width {
        opts.indent_width = w;
    }
    if let Some(l) = max_line_len {
        opts.max_line_len = l;
    }

    let formatted = fidan_fmt::format_source(&src, &opts);

    if check {
        if formatted == src {
            // Already formatted — exit 0 (success).
            return Ok(());
        }
        render_message_to_stderr(
            Severity::Error,
            "fmt",
            &format!(
                "{} is not formatted — run `fidan format {}` to fix",
                file.display(),
                file.display()
            ),
        );
        std::process::exit(1);
    }

    if in_place {
        if formatted != src {
            std::fs::write(&file, &formatted)
                .with_context(|| format!("cannot write {:?}", file))?;
            render_message_to_stderr(Severity::Note, "", &format!("formatted {}", file.display()));
        }
    } else {
        print!("{formatted}");
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
            jit_threshold,
            strict,
            reload,
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
                jit_threshold,
                strict_mode: strict,
            };
            if reload {
                run_with_reload(opts)
            } else {
                run_pipeline(opts)
            }
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
        Command::Check {
            file,
            max_errors,
            strict,
        } => {
            let opts = CompileOptions {
                input: file,
                mode: ExecutionMode::Check,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                strict_mode: strict,
                ..Default::default()
            };
            run_pipeline(opts)
        }
        Command::Fix { file, dry_run } => run_fix(file, dry_run),
        Command::Format {
            file,
            in_place,
            check,
            indent_width,
            max_line_len,
        } => run_fmt(file, in_place, check, indent_width, max_line_len),
        Command::Explain { code } => {
            run_explain(&code);
            Ok(())
        }
        Command::Repl { trace } => {
            let trace_mode = parse_trace(&trace)?;
            run_repl(trace_mode)
        }
        Command::Lsp { .. } => {
            fidan_lsp::run();
            Ok(())
        }
        Command::New { project_name, dir } => run_new(&project_name, dir.as_ref()),
    }
}

// ── Shared trace renderer ────────────────────────────────────────────────────────────

/// Print the call-stack trace for a runtime error according to `mode`.
/// Does nothing when `mode` is `None` or the trace is empty.
fn render_trace_to_stderr(trace: &[fidan_interp::TraceFrame], mode: TraceMode) {
    if trace.is_empty() || mode == TraceMode::None {
        return;
    }
    if mode == TraceMode::Compact {
        let names: Vec<String> = trace
            .iter()
            .map(|f| f.label.split('(').next().unwrap_or(&f.label).to_string())
            .collect();
        eprintln!("  stack: {}", names.join(" ← "));
    } else {
        let frames = match mode {
            TraceMode::Short => &trace[..trace.len().min(5)],
            _ => trace,
        };
        eprintln!("  stack trace (innermost first):");
        for (i, frame) in frames.iter().enumerate() {
            let display = match mode {
                TraceMode::Short => frame
                    .label
                    .split('(')
                    .next()
                    .unwrap_or(&frame.label)
                    .to_string(),
                _ => frame.label.clone(),
            };
            let relation = if i == 0 {
                "  ← panicked here".to_string()
            } else if let Some(caller) = frames.get(i + 1) {
                let caller_name = caller.label.split('(').next().unwrap_or(&caller.label);
                format!("  ← called by {caller_name}")
            } else {
                String::new()
            };
            eprintln!("    #{i}  {display}{relation}");
            if mode == TraceMode::Full {
                if let Some(ref loc) = frame.location {
                    eprintln!("         at {loc}");
                }
            }
        }
        if matches!(mode, TraceMode::Short) && trace.len() > 5 {
            eprintln!("    ... {} more frames omitted", trace.len() - 5);
        }
    }
}

// ── MIR safety analysis ──────────────────────────────────────────────────────────

/// Run the MIR-level safety passes (E0401 parallel races, W1004 unawaited Pending)
/// and render their diagnostics to stderr.
///
/// Returns the number of **errors** found (warnings are rendered but not counted
/// as blocking errors).
/// Returns `true` for warning codes that `--strict` escalates to hard errors.
fn is_strict_escalated(code: &str) -> bool {
    matches!(
        code,
        "W1001" | "W1002" | "W1003" | "W2004" | "W2005" | "W2006" | "W5001"
    )
}

fn emit_mir_safety_diags(
    mir: &fidan_mir::MirProgram,
    interner: &fidan_lexer::SymbolInterner,
    strict_mode: bool,
) -> usize {
    let mut errs = 0;

    // ── E0401: parallel data-race check ──────────────────────────────────────
    for diag in fidan_passes::check_parallel_races(mir, interner) {
        render_message_to_stderr(
            Severity::Error,
            fidan_diagnostics::diag_code!("E0401"),
            &format!("data race on `{}`: {}", diag.var_name, diag.context),
        );
        errs += 1;
    }

    // ── W1004: unawaited Pending check ───────────────────────────────────────
    for diag in fidan_passes::check_unawaited_pending(mir, interner) {
        let pl = if diag.count == 1 { "" } else { "s" };
        render_message_to_stderr(
            Severity::Warning,
            fidan_diagnostics::diag_code!("W1004"),
            &format!(
                "action `{}` contains {} unawaited `spawn` expression{} \
                 — result{} silently discarded; use `await` or `var _ = spawn \u{2026}` to suppress",
                diag.fn_name,
                diag.count,
                pl,
                if diag.count == 1 { " is" } else { "s are" },
            ),
        );
    }

    // ── W2006: null-safety check ──────────────────────────────────────────────
    for diag in fidan_passes::check_null_safety(mir, interner) {
        let sev = if strict_mode {
            Severity::Error
        } else {
            Severity::Warning
        };
        render_message_to_stderr(
            sev,
            fidan_diagnostics::diag_code!("W2006"),
            &format!(
                "in `{}`: {} — this will panic at runtime",
                diag.fn_name, diag.context
            ),
        );
        if strict_mode {
            errs += 1;
        }
    }

    // ── W5001 / W5003: compile-time slow hints ────────────────────────────────
    for diag in fidan_passes::check_slow_hints(mir, interner) {
        let code = match diag.code {
            "W5001" => fidan_diagnostics::diag_code!("W5001"),
            "W5003" => fidan_diagnostics::diag_code!("W5003"),
            _ => continue,
        };
        let sev = if strict_mode && diag.code == "W5001" {
            Severity::Error
        } else {
            Severity::Warning
        };
        render_message_to_stderr(sev, code, &diag.context);
        if strict_mode && diag.code == "W5001" {
            errs += 1;
        }
    }

    errs
}

// ── File-import helpers ──────────────────────────────────────────────────────────

/// The single stdlib root prefix.  Every stdlib import starts with `std`
/// (`use std.io`, `use std.math`, …), so only that one token needs to be
/// excluded from file-based resolution.  If a user writes bare `use math`,
/// `find_relative` will simply find nothing — no magic silent swallow.
const STDLIB_MODULES: &[&str] = &["std"];

/// Resolve a dot-path user import relative to `base_dir` (the directory of
/// the importing file), mirroring Python's package layout:
///
/// ```text
/// use mymod            →  {base_dir}/mymod.fdn
///                      OR {base_dir}/mymod/init.fdn
///
/// use mymod.utils      →  {base_dir}/mymod/utils.fdn
///                      OR {base_dir}/mymod/utils/init.fdn
/// ```
///
/// The user chooses the folder name — no magic directory is required.
fn find_relative(base_dir: &std::path::Path, segments: &[String]) -> Option<std::path::PathBuf> {
    // Build the directory prefix from all but the last segment.
    // e.g. ["mymod", "utils"] → prefix = base_dir/mymod, leaf = "utils"
    // e.g. ["mymod"]          → prefix = base_dir,        leaf = "mymod"
    let (dir_parts, leaf) = segments.split_at(segments.len().saturating_sub(1));

    let mut dir = base_dir.to_path_buf();
    for part in dir_parts {
        dir.push(part);
    }

    let leaf = leaf.first().map(|s| s.as_str()).unwrap_or("");

    // Try `{dir}/{leaf}.fdn`
    let flat = dir.join(format!("{leaf}.fdn"));
    if flat.exists() {
        return Some(flat);
    }
    // Try `{dir}/{leaf}/init.fdn`
    let init = dir.join(leaf).join("init.fdn");
    if init.exists() {
        return Some(init);
    }
    None
}

/// Returns `(resolved_path, re_export)` pairs for every file-import in `module`.
///
/// - `use "./path"` / `use "../path"` / `use "/abs/path"` — explicit file path
/// - `use mymod` / `use mymod.sub` — resolved relative to the importing file's
///   directory (Python-style): `mymod.fdn` or `mymod/init.fdn`
///
/// The `re_export` flag mirrors the `export use` keyword: when `true` the
/// imported file's symbols should be re-exposed to the grandparent importer.
///
/// Stdlib names (`io`, `math`, etc.) are skipped — the MIR lowerer handles those.
///
/// Returns `(resolved, unresolved)` where `unresolved` holds `(dotted_name, span)`
/// for every user import whose file could not be found on disk.
fn collect_file_import_paths(
    module: &fidan_ast::Module,
    interner: &fidan_lexer::SymbolInterner,
    base_dir: &std::path::Path,
) -> (
    std::collections::VecDeque<(
        std::path::PathBuf,
        bool,
        Option<std::collections::HashSet<String>>,
    )>,
    Vec<(String, fidan_source::Span)>,
) {
    // Three import modes, encoded in `Option<HashSet<String>>`:
    //
    //   None               = Namespace  (`use mod` / `use mod.sub`): HirModule is merged into
    //                         MIR for dispatch, but nothing is registered flat in typeck.
    //                         Call as `mod.fn()` only.
    //
    //   Some(empty set)    = Wildcard   (file-path imports: `use "./utils.fdn"`): everything
    //                         from the module is registered flat in typeck.  Backward-compat.
    //
    //   Some(non-empty)    = Flat       (`use mod.{name}`): only the listed names are registered
    //                         flat in typeck; HIR is filtered before merging into MIR.
    //
    // Priority when the same path is imported multiple times: Wildcard > Namespace > Flat.

    // Enum used only inside this function to compute the filter before writing to path_map.
    enum Mode {
        Wildcard,
        Namespace,
        Flat(String),
    }

    let mut path_map: Vec<(
        std::path::PathBuf,
        bool,
        Option<std::collections::HashSet<String>>,
    )> = Vec::new();
    let mut unresolved: Vec<(String, fidan_source::Span)> = Vec::new();

    let mut add = |resolved: std::path::PathBuf, re_export: bool, mode: Mode| {
        if let Some(entry) = path_map.iter_mut().find(|(p, _, _)| *p == resolved) {
            entry.1 |= re_export;
            // If already a wildcard (Some(empty)), it can never be downgraded.
            if entry.2.as_ref().map_or(false, |s| s.is_empty()) {
                return;
            }
            match mode {
                // Upgrade to wildcard.
                Mode::Wildcard => entry.2 = Some(std::collections::HashSet::new()),
                // Namespace wins over existing flat (Some(names) → None).
                Mode::Namespace => entry.2 = None,
                // Accumulate flat name — but only if currently flat (Some).
                // If currently namespace (None), do nothing (namespace wins).
                Mode::Flat(name) => {
                    if let Some(ref mut set) = entry.2 {
                        set.insert(name);
                    }
                }
            }
        } else {
            let filter = match mode {
                Mode::Wildcard => Some(std::collections::HashSet::new()),
                Mode::Namespace => None,
                Mode::Flat(name) => {
                    let mut s = std::collections::HashSet::new();
                    s.insert(name);
                    Some(s)
                }
            };
            path_map.push((resolved, re_export, filter));
        }
    };

    for &item_id in &module.items {
        let item = module.arena.get_item(item_id);
        if let fidan_ast::Item::Use {
            path,
            alias: item_alias,
            re_export,
            grouped,
            span,
            ..
        } = item
        {
            if path.is_empty() {
                continue;
            }
            let first = interner.resolve(path[0]);

            // ── Explicit file-path import (string with ./ ../ / or .fdn) ───
            // Wildcard when no alias (all symbols exposed flat).
            // Namespace when alias given: `use "./f.fdn" as ns` → `ns.fn()` only.
            if path.len() == 1
                && (first.starts_with("./")
                    || first.starts_with("../")
                    || first.starts_with('/')
                    || first.ends_with(".fdn"))
            {
                let mode = if item_alias.is_some() {
                    Mode::Namespace
                } else {
                    Mode::Wildcard
                };
                add(base_dir.join(&*first), *re_export, mode);
                continue;
            }

            // ── Stdlib import — handled by MIR lowerer, skip ───────────────
            if STDLIB_MODULES.contains(&&*first) {
                continue;
            }

            // ── User package import — resolve relative to base_dir ─────────
            let segments: Vec<String> = path
                .iter()
                .map(|&s| interner.resolve(s).to_string())
                .collect();

            if *grouped {
                // Flat import: `use mod.{name}` — the last path segment is a
                // specific name to import flat.  Resolve the prefix as the file.
                if segments.len() >= 2 {
                    let prefix = &segments[..segments.len() - 1];
                    let specific_name = segments.last().unwrap().clone();
                    if let Some(resolved) = find_relative(base_dir, prefix) {
                        add(resolved, *re_export, Mode::Flat(specific_name));
                    } else if let Some(resolved) = find_relative(base_dir, &segments) {
                        // Edge: the full path happens to be a file — namespace.
                        add(resolved, *re_export, Mode::Namespace);
                    } else {
                        unresolved.push((segments.join("."), *span));
                    }
                } else {
                    // Single-segment grouped edge case — treat as namespace.
                    if let Some(resolved) = find_relative(base_dir, &segments) {
                        add(resolved, *re_export, Mode::Namespace);
                    } else {
                        unresolved.push((segments.join("."), *span));
                    }
                }
            } else {
                // Namespace import: `use mod` / `use mod.submod` — resolve the
                // full path; the last segment becomes the namespace alias.
                if let Some(resolved) = find_relative(base_dir, &segments) {
                    add(resolved, *re_export, Mode::Namespace);
                } else {
                    unresolved.push((segments.join("."), *span));
                }
            }
        }
    }

    (path_map.into_iter().collect(), unresolved)
}

/// Pre-register functions, objects, and globals from `hir` into `tc` so the
/// main file's type-checker sees imported symbols as known bindings.
///
/// `filter` — when `Some`, only names in the set are registered (flat/grouped
/// import, e.g. `use mod.{name}`).  When `None` (namespace import, e.g.
/// `use mod`), nothing is registered flat — the namespace variable itself is
/// already bound by `check_item` so calls like `mod.fn()` type-check correctly
/// via dynamic dispatch on `FidanType::Dynamic`.
fn pre_register_hir_into_tc(
    tc: &mut fidan_typeck::TypeChecker,
    hir: &fidan_hir::HirModule,
    filter: Option<&std::collections::HashSet<String>>,
    interner: &fidan_lexer::SymbolInterner,
) {
    use fidan_typeck::{ActionInfo, ParamInfo};

    let visible = |sym: fidan_lexer::Symbol| -> bool {
        // None              → namespace import: nothing registered flat.
        // Some(empty set)   → wildcard (file-path): everything registered flat.
        // Some(non-empty)   → flat import: only listed names registered.
        match filter {
            None => false,
            Some(f) if f.is_empty() => true,
            Some(f) => f.contains(interner.resolve(sym).as_ref()),
        }
    };

    for func in &hir.functions {
        if !visible(func.name) {
            continue;
        }
        let info = ActionInfo {
            params: func
                .params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name,
                    ty: p.ty.clone(),
                    certain: p.certain,
                    optional: p.optional,
                    has_default: p.default.is_some(),
                })
                .collect(),
            return_ty: func.return_ty.clone(),
            span: func.span,
        };
        tc.pre_register_action(func.name, info);
    }

    for obj in &hir.objects {
        if !visible(obj.name) {
            continue;
        }
        tc.pre_register_object_data(
            obj.name,
            obj.parent,
            obj.span,
            obj.fields.iter().map(|f| (f.name, f.ty.clone())),
            obj.methods.iter().map(|m| {
                let ai = ActionInfo {
                    params: m
                        .params
                        .iter()
                        .map(|p| ParamInfo {
                            name: p.name,
                            ty: p.ty.clone(),
                            certain: p.certain,
                            optional: p.optional,
                            has_default: p.default.is_some(),
                        })
                        .collect(),
                    return_ty: m.return_ty.clone(),
                    span: m.span,
                };
                (m.name, ai)
            }),
        );
    }

    for glob in &hir.globals {
        if !visible(glob.name) {
            continue;
        }
        tc.pre_register_global(glob.name, glob.ty.clone(), glob.is_const);
    }

    // Top-level variable declarations live in init_stmts (HirGlobal is unused by the
    // current HIR lowerer — all top-level vars, including `const var`, become VarDecl
    // init statements).  Scan the first level to pre-register any such declarations.
    for stmt in &hir.init_stmts {
        if let fidan_hir::HirStmt::VarDecl {
            name, ty, is_const, ..
        } = stmt
        {
            if visible(*name) {
                tc.pre_register_global(*name, ty.clone(), *is_const);
            }
        }
    }

    // Re-exported stdlib namespaces: if the imported file declared `export use
    // std.X`, expose the binding in the caller's type-checker so accesses like
    // `X.fn()` don't produce false E0101 errors.
    for decl in &hir.use_decls {
        if !decl.re_export {
            continue;
        }
        if decl.module_path.len() >= 2 {
            if let Some(names) = &decl.specific_names {
                // `export use std.io.readFile` — register the free-function name.
                for name in names {
                    tc.pre_register_namespace(name);
                }
            } else {
                // `export use std.io` — register the namespace alias.
                let alias = decl
                    .alias
                    .as_deref()
                    .unwrap_or(decl.module_path[1].as_str());
                tc.pre_register_namespace(alias);
            }
        } else if decl.module_path.len() == 1 && decl.specific_names.is_none() {
            // `export use mymod` — user-module re-export.  Register the namespace
            // alias so the importer's typechecker allows `mymod.fn()` calls.
            let alias = decl
                .alias
                .as_deref()
                .unwrap_or(decl.module_path[0].as_str());
            tc.pre_register_namespace(alias);
        }
    }
}

/// Filter a HIR module to only the named functions, objects, and globals.
///
/// Used for flat/grouped imports (`use mod.{name}`) so that only the requested
/// symbols end up in the merged MIR — preventing unnamed symbols from being
/// callable without a namespace prefix.
/// Top-level init statements (side-effects) and use_decls are kept intact.
fn filter_hir_module(
    mut hir: fidan_hir::HirModule,
    names: &std::collections::HashSet<String>,
    interner: &fidan_lexer::SymbolInterner,
) -> fidan_hir::HirModule {
    hir.functions
        .retain(|f| names.contains(interner.resolve(f.name).as_ref()));
    hir.objects
        .retain(|o| names.contains(interner.resolve(o.name).as_ref()));
    hir.globals
        .retain(|g| names.contains(interner.resolve(g.name).as_ref()));
    // Keep init_stmts as-is: top-level side-effects (e.g. print("IMPORTED"))
    // should execute even for selective imports, matching Python semantics.
    hir
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

    // ── File-path import loading ───────────────────────────────────────────────
    //
    // Before type-checking the main file, collect `use "./other.fdn"` imports,
    // run them through the full pipeline (lex → parse → typeck → HIR lower), and
    // pre-register their exported symbols into the main file's TypeChecker so
    // the main file's checker sees imported functions/objects/globals as known.
    let base_dir: std::path::PathBuf = if is_stdin {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    } else {
        opts.input
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    };

    let imported_hirs: Vec<(
        fidan_hir::HirModule,
        Option<std::collections::HashSet<String>>,
        bool, // expose_to_typeck
    )> = {
        use fidan_lexer::Lexer;
        use std::collections::{HashSet, VecDeque};

        //   expose_to_typeck = true  → symbols visible in the top-level program
        //   filter = None             → namespace import: call as `mod.fn()` only
        //   filter = Some(empty)      → wildcard: everything flat
        //   filter = Some(names)      → flat import: only listed names flat
        type QueueItem = (std::path::PathBuf, bool, Option<HashSet<String>>);

        let mut hirs: Vec<(
            fidan_hir::HirModule,
            Option<HashSet<String>>,
            bool, // expose_to_typeck
        )> = Vec::new();
        let (main_paths, main_unresolved) =
            collect_file_import_paths(&module, &interner, &base_dir);
        for (name, span) in main_unresolved {
            error_count += 1;
            let diag = fidan_diagnostics::Diagnostic::error(
                fidan_diagnostics::diag_code!("E0106"),
                format!("module `{name}` not found"),
                span,
            );
            fidan_diagnostics::render_to_stderr(&diag, &source_map);
        }
        let mut queue: VecDeque<QueueItem> = main_paths
            .into_iter()
            .map(|(p, _, filter)| (p, true, filter)) // direct imports always exposed
            .collect();

        // Track canonical paths to break import cycles.
        let mut loaded: HashSet<std::path::PathBuf> = HashSet::new();
        if !is_stdin {
            if let Ok(canon) = opts.input.canonicalize() {
                loaded.insert(canon);
            }
        }

        while let Some((import_path, expose, filter)) = queue.pop_front() {
            let canon = import_path
                .canonicalize()
                .unwrap_or_else(|_| import_path.clone());
            if !loaded.insert(canon) {
                continue; // already loaded or cycle detected
            }

            match std::fs::read_to_string(&import_path) {
                Ok(imp_src) => {
                    let imp_name = import_path.display().to_string();
                    let imp_file = source_map.add_file(imp_name.as_str(), imp_src.as_str());
                    let (imp_tokens, imp_lex_diags) =
                        Lexer::new(&imp_file, Arc::clone(&interner)).tokenise();
                    for d in &imp_lex_diags {
                        fidan_diagnostics::render_to_stderr(d, &source_map);
                    }
                    let (imp_module, imp_parse_diags) =
                        fidan_parser::parse(&imp_tokens, imp_file.id, Arc::clone(&interner));
                    for d in &imp_parse_diags {
                        fidan_diagnostics::render_to_stderr(d, &source_map);
                    }
                    let imp_lex_err = imp_lex_diags
                        .iter()
                        .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
                        .count();
                    let imp_parse_err = imp_parse_diags
                        .iter()
                        .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
                        .count();
                    error_count += imp_lex_err + imp_parse_err;

                    // Enqueue transitive imports (they use their own internal resolution,
                    // no name filter needed — the filter only applies at the call site).
                    let imp_base = import_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    let (sub_paths, sub_unresolved) =
                        collect_file_import_paths(&imp_module, &interner, &imp_base);
                    for (name, span) in sub_unresolved {
                        error_count += 1;
                        let diag = fidan_diagnostics::Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0106"),
                            format!("module `{name}` not found"),
                            span,
                        );
                        fidan_diagnostics::render_to_stderr(&diag, &source_map);
                    }
                    for (sub, sub_re_export, sub_filter) in sub_paths {
                        queue.push_back((sub, expose && sub_re_export, sub_filter));
                    }

                    // Typeck + HIR-lower the imported module (only if lex/parse clean).
                    if imp_lex_err == 0 && imp_parse_err == 0 {
                        let imp_tm =
                            fidan_typeck::typecheck_full(&imp_module, Arc::clone(&interner));
                        for d in &imp_tm.diagnostics {
                            fidan_diagnostics::render_to_stderr(d, &source_map);
                        }
                        let imp_tc_err = imp_tm
                            .diagnostics
                            .iter()
                            .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
                            .count();
                        error_count += imp_tc_err;
                        if imp_tc_err == 0 {
                            let imp_hir = fidan_hir::lower_module(&imp_module, &imp_tm, &interner);
                            // Always push so private transitive deps are compiled into MIR
                            // (their functions may be called by the importing module).
                            // `expose` only controls whether names appear in the outer typeck.
                            hirs.push((imp_hir, filter, expose));
                        }
                    }
                }
                Err(e) => {
                    error_count += 1;
                    render_message_to_stderr(
                        Severity::Error,
                        "",
                        &format!("cannot load import `{}`: {e}", import_path.display()),
                    );
                }
            }
        }
        hirs
    };

    // Always run the full typed path so HIR/MIR emit has type information.
    // Pre-register imported symbols before checking the main file so the checker
    // does not emit false "undefined identifier" errors for cross-file references.
    let typed_module = if lex_diags.is_empty() && parse_diags.is_empty() {
        let mut tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), file.id);
        for (hir, filter, expose_tc) in &imported_hirs {
            if *expose_tc {
                pre_register_hir_into_tc(&mut tc, hir, filter.as_ref(), &interner);
            }
        }
        tc.check_module(&module);
        let tm = tc.finish_typed();
        for diag in &tm.diagnostics {
            if opts.strict_mode
                && diag.severity == fidan_diagnostics::Severity::Warning
                && is_strict_escalated(&diag.code)
            {
                render_message_to_stderr(Severity::Error, diag.code.as_str(), &diag.message);
                error_count += 1;
            } else {
                fidan_diagnostics::render_to_stderr(diag, &source_map);
                if diag.severity == fidan_diagnostics::Severity::Error {
                    error_count += 1;
                }
            }
        }
        Some(tm)
    } else {
        None
    };

    // ── Merge HIR: base module + all imported HIR modules ─────────────────────
    //
    // Compute once; all --emit and run blocks share the merged result without
    // re-calling lower_module.
    let merged_hir: Option<fidan_hir::HirModule> = typed_module.as_ref().map(|tm| {
        let base = fidan_hir::lower_module(&module, tm, &interner);
        imported_hirs
            .into_iter()
            .fold(base, |acc, (imp, filter, _expose_tc)| {
                // None (namespace) or Some(empty) (wildcard): merge the full HIR so that
                // all functions exist in MIR (namespace dispatch / flat exposure).
                // Some(non-empty names): flat import — strip everything except those names.
                let filtered = if let Some(names) = filter.as_ref().filter(|f| !f.is_empty()) {
                    filter_hir_module(imp, names, &interner)
                } else {
                    imp
                };
                fidan_hir::merge_module(acc, filtered)
            })
    });

    // ── --emit hir ─────────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Hir) {
        if let Some(ref hir) = merged_hir {
            println!("=== hir: {source_name} ===");
            println!("  objects:    {}", hir.objects.len());
            println!("  functions:  {}", hir.functions.len());
            println!("  globals:    {}", hir.globals.len());
            println!("  init_stmts: {}", hir.init_stmts.len());
            for obj in &hir.objects {
                let name = interner.resolve(obj.name);
                let parent = obj
                    .parent
                    .map(|p| interner.resolve(p).to_string())
                    .unwrap_or_default();
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
                    println!(
                        "    method {mname}() -> {:?}  ({} stmts)",
                        m.return_ty,
                        m.body.len()
                    );
                }
            }
            for func in &hir.functions {
                let fname = interner.resolve(func.name);
                let ext = func
                    .extends
                    .map(|e| format!(" extends {}", interner.resolve(e)))
                    .unwrap_or_default();
                println!(
                    "  action {fname}{ext}() -> {:?}  ({} stmts)",
                    func.return_ty,
                    func.body.len()
                );
            }
            for g in &hir.globals {
                let gname = interner.resolve(g.name);
                println!(
                    "  global {}{gname}: {:?}",
                    if g.is_const { "const " } else { "" },
                    g.ty
                );
            }
        } else {
            eprintln!("  (HIR not available — parse or lex errors present)");
        }
    }

    // ── --emit mir ─────────────────────────────────────────────────────────────
    if opts.emit.contains(&EmitKind::Mir) {
        if let Some(ref hir) = merged_hir {
            let mir = fidan_mir::lower_program(hir, &interner, &[]);
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
                if let Some(ref hir) = merged_hir {
                    let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                    // ── MIR safety analysis (E0401, W1004) ───────────────────────
                    error_count += emit_mir_safety_diags(&mir, &interner, opts.strict_mode);
                    if error_count == 0 {
                        // ── Optimisation passes (Phase 6) ─────────────────────
                        fidan_passes::run_all(&mut mir);
                        let result = fidan_interp::run_mir_with_jit(
                            mir,
                            Arc::clone(&interner),
                            Arc::clone(&source_map),
                            opts.jit_threshold,
                        );
                        if let Err(err) = result {
                            render_message_to_stderr(Severity::Error, err.code, &err.message);
                            if !err.trace.is_empty() && opts.trace != TraceMode::None {
                                render_trace_to_stderr(&err.trace, opts.trace);
                            }
                            if opts.trace != TraceMode::Full {
                                render_message_to_stderr(
                                    Severity::Note,
                                    "",
                                    "run with `--trace full` for more details",
                                );
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
                    "AOT backend not yet implemented (Phase 11 – LLVM)",
                );
            }
        }
        ExecutionMode::Test => {
            if error_count == 0 {
                if let Some(ref hir) = merged_hir {
                    let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                    // ── MIR safety analysis (E0401, W1004) ───────────────────
                    error_count += emit_mir_safety_diags(&mir, &interner, opts.strict_mode);
                    if error_count == 0 {
                        fidan_passes::run_all(&mut mir);
                        let test_count = mir.test_functions.len();
                        if test_count == 0 {
                            render_message_to_stderr(Severity::Note, "", "no test blocks found");
                        } else {
                            match fidan_interp::run_tests(
                                mir,
                                Arc::clone(&interner),
                                Arc::clone(&source_map),
                            ) {
                                (Err(err), _) => {
                                    // Initialisation (top-level code) crashed before tests ran.
                                    eprintln!(
                                        "\x1b[1;31merror\x1b[0m: program initialisation failed: {}",
                                        err.message
                                    );
                                    if !err.trace.is_empty() && opts.trace != TraceMode::None {
                                        render_trace_to_stderr(&err.trace, opts.trace);
                                    }
                                    std::process::exit(1);
                                }
                                (Ok(()), results) => {
                                    let mut passed = 0usize;
                                    let mut failed = 0usize;
                                    for r in &results {
                                        if r.passed {
                                            passed += 1;
                                            eprintln!("  \x1b[1;32m✓\x1b[0m {}", r.name);
                                        } else {
                                            failed += 1;
                                            let msg = r.message.as_deref().unwrap_or("failed");
                                            let msg = msg.trim_start_matches("assertion failed: ");
                                            eprintln!("  \x1b[1;31m✗\x1b[0m {} — {}", r.name, msg);
                                        }
                                    }
                                    eprintln!();
                                    if failed == 0 {
                                        eprintln!(
                                            "\x1b[1;32m{} test{} passed\x1b[0m",
                                            passed,
                                            if passed == 1 { "" } else { "s" }
                                        );
                                    } else {
                                        eprintln!(
                                            "\x1b[1;32m{} passed\x1b[0m, \x1b[1;31m{} failed\x1b[0m",
                                            passed, failed
                                        );
                                        std::process::exit(1);
                                    }
                                }
                            }
                        }
                    }
                }
            }
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

// ── REPL helpers ─────────────────────────────────────────────────────────────────────

/// Count the net change in brace depth for one REPL input line.
///
/// `{` adds 1, `}` subtracts 1.  Content inside `"..."` double-quoted strings
/// (including `{expr}` interpolation regions) and line comments starting with
/// `#` is ignored so that e.g. `print("open: {")` does not accidentally trigger
/// multiline continuation.
///
/// Single-quoted strings (`'...'`) are also handled for completeness.
fn count_brace_delta(line: &str) -> i32 {
    let mut delta: i32 = 0;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            // Line comments: everything to the end of the line is ignored.
            '#' => break,

            // Double-quoted string literal — skip until the closing `"`.
            '"' => {
                while let Some(sc) = chars.next() {
                    match sc {
                        '\\' => {
                            chars.next();
                        } // backslash-escaped character
                        '"' => break, // end of string literal
                        _ => {}
                    }
                }
            }

            // Single-quoted string — same treatment.
            '\'' => {
                while let Some(sc) = chars.next() {
                    match sc {
                        '\\' => {
                            chars.next();
                        }
                        '\'' => break,
                        _ => {}
                    }
                }
            }

            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

// ── REPL ─────────────────────────────────────────────────────────────────────────────

/// Interactive lex + parse + typecheck + interpret loop.
///
/// Each line is treated as a self-contained Fidan snippet.  The persistent
/// [`TypeChecker`] accumulates symbol definitions across lines so names defined
/// on line N are visible on line N+1.  The interpreter runs after every clean
/// type-check so side effects (print, etc.) are visible immediately.
fn run_repl(trace_mode: TraceMode) -> Result<()> {
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
    let mut rl = rustyline::Editor::<ReplHelper, rustyline::history::DefaultHistory>::new()?;
    rl.set_helper(Some(ReplHelper));

    let prompt_main = "ƒ>  ";
    let prompt_cont = "...  ";

    // Persist the interner so symbol IDs are stable across REPL lines.
    let interner = Arc::new(SymbolInterner::new());

    let boot_map = Arc::new(SourceMap::new());
    let boot_file = boot_map.add_file("<repl>", "");
    let boot_fid = boot_file.id;

    // Persistent TypeChecker kept in sync after each successful line.
    // Used exclusively for :type queries — not for compilation.
    let mut tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
    tc.set_repl(true);

    // MIR-backed REPL state: accumulated source, global snapshot, and init cursor.
    let mut mir_repl_state = fidan_interp::MirReplState::new();

    // ── Multiline state ────────────────────────────────────────────────────
    let mut open_braces: i32 = 0;
    let mut pending_input = String::new();

    let mut line_no: u32 = 0;
    let mut error_history: Vec<String> = Vec::new();

    loop {
        let prompt = if open_braces > 0 {
            prompt_cont
        } else {
            prompt_main
        };
        let line = match rl.readline(prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => {
                if open_braces > 0 {
                    open_braces = 0;
                    pending_input.clear();
                    println!("  (input cancelled)");
                }
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let _ = rl.add_history_entry(trimmed);

        // ── :cancel — abort multiline input (valid at any nesting depth) ──
        if trimmed == ":cancel" {
            if open_braces > 0 || !pending_input.is_empty() {
                open_braces = 0;
                pending_input.clear();
                println!("  (multiline input cancelled)");
            } else {
                eprintln!("  (nothing to cancel — not inside a multiline block)");
            }
            continue;
        }

        // ── Colon commands (only when not in a multiline block) ────────────
        if open_braces == 0 {
            if let Some(cmd) = trimmed.strip_prefix(':') {
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
                        mir_repl_state = fidan_interp::MirReplState::new();
                        open_braces = 0;
                        pending_input.clear();
                        println!("  (session state cleared)");
                        continue;
                    }

                    "help" => {
                        println!("  :help               show this message");
                        println!("  :exit / :quit / :q  leave the REPL");
                        println!("  :clear / :cls       clear the terminal (also Ctrl+L)");
                        println!("  :reset              clear the session state");
                        println!("  :cancel             abort a multiline block input");
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
                        let (m, ast_diags) =
                            fidan_parser::parse(&toks, f.id, Arc::clone(&interner));
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
                        let (m, parse_diags) =
                            fidan_parser::parse(&toks, f.id, Arc::clone(&interner));
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
        } // end `if open_braces == 0`

        // ── Multiline brace counting (string-literal + comment aware) ─────
        open_braces = (open_braces + count_brace_delta(trimmed)).max(0);
        if !pending_input.is_empty() {
            pending_input.push('\n');
        }
        pending_input.push_str(trimmed);
        if open_braces > 0 {
            continue; // wait for closing braces
        }

        // ── Complete input ready — compile full accumulated source ──────────
        let complete_input = std::mem::take(&mut pending_input);

        // ── Auto-echo: mini-parse just the new input to see if its last item
        //   is a bare expression.  If so we wrap it as
        //   `var __repl_echo__ set <expr>` so the value is preserved in a
        //   global after execution and can be displayed without calling print().
        let echo_sym = interner.intern("__repl_echo__");
        let (echo_sym_opt, candidate_source) = {
            use fidan_ast::Item;
            let mini_smap = Arc::new(SourceMap::new());
            let mini_file = mini_smap.add_file("<echo-check>", complete_input.as_str());
            let (mini_toks, _) = Lexer::new(&mini_file, Arc::clone(&interner)).tokenise();
            let (mini_mod, _) =
                fidan_parser::parse(&mini_toks, mini_file.id, Arc::clone(&interner));
            let last = mini_mod.items.last().map(|id| mini_mod.arena.get_item(*id));
            if let Some(Item::ExprStmt(expr_id)) = last {
                let accumulated = &mir_repl_state.accumulated_source;
                let wrapped = if mini_mod.items.len() == 1 {
                    // Entire input is the bare expression — wrap it directly.
                    if accumulated.is_empty() {
                        format!("var __repl_echo__ set {}\n", complete_input.trim())
                    } else {
                        format!(
                            "{}\nvar __repl_echo__ set {}\n",
                            accumulated,
                            complete_input.trim()
                        )
                    }
                } else {
                    // Multiple items: use the expr span to find the split point.
                    let span = mini_mod.arena.get_expr(*expr_id).span();
                    let lo = span.start as usize;
                    let hi = span.end as usize;
                    let expr_text = &complete_input[lo..hi.min(complete_input.len())];
                    let prefix = &complete_input[..lo];
                    if accumulated.is_empty() {
                        format!("{}var __repl_echo__ set {}\n", prefix, expr_text)
                    } else {
                        format!(
                            "{}\n{}var __repl_echo__ set {}\n",
                            accumulated, prefix, expr_text
                        )
                    }
                };
                (Some(echo_sym), wrapped)
            } else {
                let mut s = mir_repl_state.accumulated_source.clone();
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&complete_input);
                s.push('\n');
                (None, s)
            }
        };

        // ── Lex + parse the full candidate source ──────────────────────────
        let exec_smap = Arc::new(SourceMap::new());
        let exec_file = exec_smap.add_file("<repl>", &*candidate_source);
        let (exec_toks, lex_diags) = Lexer::new(&exec_file, Arc::clone(&interner)).tokenise();
        for d in &lex_diags {
            fidan_diagnostics::render_to_stderr(d, &exec_smap);
            error_history.push(format!("[{}]: {}", d.code, d.message));
        }
        if !lex_diags.is_empty() {
            continue;
        }

        let (full_module, parse_diags) =
            fidan_parser::parse(&exec_toks, exec_file.id, Arc::clone(&interner));
        for d in &parse_diags {
            fidan_diagnostics::render_to_stderr(d, &exec_smap);
            error_history.push(format!("[{}]: {}", d.code, d.message));
        }
        if !parse_diags.is_empty() {
            continue;
        }

        // ── Fresh type-check (consumes exec_tc to produce TypedModule) ──────
        let mut exec_tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
        exec_tc.set_repl(true);
        exec_tc.check_module(&full_module);
        let typed = exec_tc.finish_typed();
        if !typed.diagnostics.is_empty() {
            for d in &typed.diagnostics {
                fidan_diagnostics::render_to_stderr(d, &exec_smap);
                error_history.push(format!("[{}]: {}", d.code, d.message));
            }
            continue;
        }

        // ── HIR → MIR → optimisation passes ───────────────────────────────
        let hir = fidan_hir::lower_module(&full_module, &typed, &interner);
        let mut mir =
            fidan_mir::lower_program(&hir, &interner, &mir_repl_state.persistent_global_names);
        // Run MIR safety diagnostics (W2006 null-safety, W1004 unawaited, etc.).
        emit_mir_safety_diags(&mir, &interner, false);
        fidan_passes::run_all(&mut mir);

        // ── Execute the new delta on the MIR machine ───────────────────────
        match fidan_interp::run_mir_repl_line(
            &mut mir_repl_state,
            mir,
            Arc::clone(&interner),
            Arc::clone(&exec_smap),
            500,
            echo_sym_opt,
        ) {
            Ok(Some(val)) => {
                // Suppress Nothing values (e.g. from print() calls that happen
                // to be the last expression — the side effect already printed).
                if !matches!(val, fidan_interp::FidanValue::Nothing) {
                    println!("{}", fidan_interp::display_value(&val));
                }
            }
            Ok(None) => {}
            Err(e) => {
                render_message_to_stderr(Severity::Error, e.code, &e.message);
                render_trace_to_stderr(&e.trace, trace_mode);
                if trace_mode != TraceMode::Full {
                    render_message_to_stderr(
                        Severity::Note,
                        "",
                        "run with `--trace full` for more details",
                    );
                }
                error_history.push(e.message.clone());
                // Don't commit: REPL state remains unchanged on runtime error.
                continue;
            }
        }

        // ── Commit: persist source and refresh :type TypeChecker ────────────
        mir_repl_state.accumulated_source = candidate_source;

        // Rebuild the persistent tc from the full module so :type queries
        // can see all newly-defined names with correct type information.
        tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
        tc.set_repl(true);
        tc.check_module(&full_module);
        let _ = tc.drain_diags();
    }

    println!();
    println!("Bye! 👋");
    Ok(())
}
