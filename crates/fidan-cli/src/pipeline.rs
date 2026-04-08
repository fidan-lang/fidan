use crate::imports::{collect_file_import_paths, filter_hir_module, pre_register_hir_into_tc};
use crate::last_error;
use crate::replay::save_replay_bundle;
use anyhow::{Context, Result, bail};
use fidan_diagnostics::{Diagnostic, Severity, render_message_to_stderr};
use fidan_driver::dal::validate_package_name;
use fidan_driver::{CompileOptions, EmitKind, ExecutionMode, TraceMode};
use fidan_runtime::push_program_args;
use std::path::PathBuf;

// ── Hot Reload ────────────────────────────────────────────────────────────────

/// Run a Fidan program and re-run it whenever any watched source file changes.
///
/// On each file-save event the full pipeline is re-run from scratch (lex →
/// parse → typecheck → HIR/MIR → interpret).  This is clean and correct
/// because all pipeline state is freshly constructed per run.
pub(crate) fn run_with_reload(opts: CompileOptions) -> Result<()> {
    use notify::{Event, RecursiveMode, Watcher, recommended_watcher};
    use std::collections::HashSet;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let watch_path = opts.input.clone();

    let entry_dir = watch_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = recommended_watcher(tx)?;

    // Watch the entry-point directory recursively so subdirectory imports are
    // covered without any extra bookkeeping.
    watcher.watch(&entry_dir, RecursiveMode::Recursive)?;

    // Track extra directories (outside the entry dir) that are already watched,
    // so we only call watcher.watch() once per directory.
    let mut extra_watched: HashSet<std::path::PathBuf> = HashSet::new();

    /// Quick lex+parse pass that transitively resolves every `use` statement
    /// and returns the canonical parent directories of all imported `.fdn`
    /// files that live *outside* `skip_dir`.
    fn collect_external_import_dirs(
        entry: &std::path::Path,
        skip_dir: &std::path::Path,
    ) -> HashSet<std::path::PathBuf> {
        use fidan_lexer::{Lexer, SymbolInterner};
        use fidan_source::SourceMap;
        use std::collections::VecDeque;
        use std::sync::Arc;

        let interner = Arc::new(SymbolInterner::new());
        let mut dirs: HashSet<std::path::PathBuf> = HashSet::new();
        let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
        let mut queue: VecDeque<std::path::PathBuf> = VecDeque::new();
        queue.push_back(entry.to_path_buf());

        while let Some(path) = queue.pop_front() {
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !visited.insert(canon) {
                continue;
            }
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let name = path.display().to_string();
            let sm = Arc::new(SourceMap::new());
            let file = sm.add_file(&*name, &*src);
            let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
            let (module, _) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
            let base = path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let (imports, _) = collect_file_import_paths(&module, &interner, &base);
            for (imp_path, _, _) in imports {
                if let Some(parent) = imp_path.parent() {
                    let canon_parent = parent
                        .canonicalize()
                        .unwrap_or_else(|_| parent.to_path_buf());
                    // Only register dirs that are outside the entry dir.
                    if !canon_parent.starts_with(skip_dir) {
                        dirs.insert(canon_parent);
                    }
                }
                queue.push_back(imp_path);
            }
        }
        dirs
    }

    // Helper: ensure we are watching `dir` (idempotent).
    let entry_dir_canon = entry_dir
        .canonicalize()
        .unwrap_or_else(|_| entry_dir.clone());
    let watch_extra = |watcher: &mut dyn Watcher,
                       extra: HashSet<std::path::PathBuf>,
                       watched: &mut HashSet<std::path::PathBuf>| {
        for dir in extra {
            if watched.insert(dir.clone()) {
                if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                    eprintln!(
                        "\x1b[1;31m[↻ reload] cannot watch {}: {e}\x1b[0m",
                        dir.display()
                    );
                } else {
                    eprintln!("\x1b[2m[↻ reload] also watching {}\x1b[0m", dir.display());
                }
            }
        }
    };

    // Seed external-import watchers based on what the entry file currently imports.
    let initial_extra = collect_external_import_dirs(&watch_path, &entry_dir_canon);
    watch_extra(&mut watcher, initial_extra, &mut extra_watched);

    eprintln!(
        "\x1b[2m[↻ reload] watching {} — Ctrl+C to stop\x1b[0m",
        entry_dir.display()
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
                    "\x1b[2m[↻ reload] {} changed — re-running\x1b[0m",
                    changed.join(", ")
                );

                // Re-collect external import dirs in case imports changed.
                let new_extra = collect_external_import_dirs(&watch_path, &entry_dir_canon);
                watch_extra(&mut watcher, new_extra, &mut extra_watched);

                // Re-run — errors are printed but do not stop the watcher.
                let _ = run_pipeline(opts.clone());
            }
            Ok(Err(e)) => eprintln!("\x1b[1;31m[↻ reload] watcher error: {e}\x1b[0m"),
            Err(_) => break, // channel closed
        }
    }
    Ok(())
}

pub(crate) fn run_new(
    project_name: &str,
    parent_dir: Option<&PathBuf>,
    package: bool,
) -> Result<()> {
    let base = parent_dir
        .cloned()
        .unwrap_or_else(|| std::env::current_dir().expect("cannot read cwd"));
    let project_dir = base.join(project_name);

    ensure_new_project_dir(&project_dir)?;

    if package {
        scaffold_dal_package(&project_dir, project_name)?;
        render_message_to_stderr(
            Severity::Note,
            "",
            &format!(
                "created Dal package `{}` — next: cd {} && fidan dal package",
                project_name, project_name
            ),
        );
    } else {
        scaffold_standard_project(&project_dir, project_name)?;
        render_message_to_stderr(
            Severity::Note,
            "",
            &format!(
                "created project `{}` — run: fidan run {}/main.fdn",
                project_name, project_name
            ),
        );
    }

    Ok(())
}

fn ensure_new_project_dir(project_dir: &std::path::Path) -> Result<()> {
    if project_dir.exists() {
        let mut entries = std::fs::read_dir(project_dir)
            .with_context(|| format!("cannot read directory {:?}", project_dir))?;
        if entries.next().is_some() {
            bail!(
                "directory {:?} already exists and is not empty",
                project_dir
            );
        }
        return Ok(());
    }

    std::fs::create_dir_all(project_dir)
        .with_context(|| format!("cannot create directory {:?}", project_dir))?;
    Ok(())
}

fn scaffold_standard_project(project_dir: &std::path::Path, project_name: &str) -> Result<()> {
    let main_src = format!(
        concat!(
            "# {name}\n",
            "# Entry point — run with: fidan run main.fdn\n",
            "\n",
            "action main {{\n",
            "    print(\"Hello from {name}!\")\n",
            "}}\n",
            "\n",
            "main()\n"
        ),
        name = project_name
    );
    std::fs::write(project_dir.join("main.fdn"), &main_src)
        .with_context(|| "cannot write main.fdn")?;
    Ok(())
}

fn scaffold_dal_package(project_dir: &std::path::Path, project_name: &str) -> Result<()> {
    validate_package_name(project_name)
        .with_context(|| "Dal package names must be lowercase, use digits or single hyphens, and start/end with an alphanumeric character")?;

    std::fs::create_dir_all(project_dir.join("src"))
        .with_context(|| format!("cannot create {:?}", project_dir.join("src")))?;

    let manifest = format!(
        concat!(
            "[package]\n",
            "name = \"{name}\"\n",
            "version = \"0.1.0\"\n",
            "readme = \"README.md\"\n"
        ),
        name = project_name
    );
    std::fs::write(project_dir.join("dal.toml"), manifest)
        .with_context(|| "cannot write dal.toml")?;

    let readme = format!(
        concat!(
            "# {name}\n\n",
            "A Dal package for Fidan.\n\n",
            "## Package structure\n\n",
            "- `dal.toml` package manifest\n",
            "- `src/init.fdn` package entry module\n\n",
            "Build locally with `fidan dal package`.\n"
        ),
        name = project_name
    );
    std::fs::write(project_dir.join("README.md"), readme)
        .with_context(|| "cannot write README.md")?;

    let init_src = format!(
        concat!(
            "# {name}\n",
            "# Dal package entry module\n\n",
            "action greet returns string {{\n",
            "    return \"Hello from {name}!\"\n",
            "}}\n"
        ),
        name = project_name
    );
    std::fs::write(project_dir.join("src").join("init.fdn"), init_src)
        .with_context(|| "cannot write src/init.fdn")?;

    Ok(())
}

pub(crate) fn run_fmt(
    file: PathBuf,
    in_place: bool,
    check: bool,
    indent_width: Option<usize>,
    max_line_len: Option<usize>,
) -> Result<()> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;

    let source_map = Arc::new(SourceMap::new());
    let interner = Arc::new(SymbolInterner::new());
    let source_name = file.display().to_string();
    let source_file = source_map.add_file(&*source_name, &*src);
    let (tokens, lex_diags) = Lexer::new(&source_file, Arc::clone(&interner)).tokenise();
    let (_, parse_diags) = fidan_parser::parse(&tokens, source_file.id, Arc::clone(&interner));

    let has_syntax_errors = lex_diags
        .iter()
        .chain(parse_diags.iter())
        .any(|diag| diag.severity == Severity::Error);
    if has_syntax_errors {
        for diag in lex_diags.iter().chain(parse_diags.iter()) {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
        }
        bail!(
            "refusing to format {} because it contains syntax errors",
            file.display()
        );
    }

    let opts = fidan_fmt::resolve_format_options_for_path(Some(&file), indent_width, max_line_len)?;

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

// ── Shared trace renderer ────────────────────────────────────────────────────────────

/// Print the call-stack trace for a runtime error according to `mode`.
/// Does nothing when `mode` is `None` or the trace is empty.
pub(crate) fn render_trace_to_stderr(trace: &[fidan_interp::TraceFrame], mode: TraceMode) {
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
            if mode == TraceMode::Full
                && let Some(ref loc) = frame.location
            {
                eprintln!("         at {loc}");
            }
        }
        if matches!(mode, TraceMode::Short) && trace.len() > 5 {
            eprintln!("    ... {} more frames omitted", trace.len() - 5);
        }
    }
}

// ── MIR safety analysis ──────────────────────────────────────────────────────────

/// Returns `true` if `code` appears in the `suppress` list (case-insensitive).
/// Short-circuits immediately when `suppress` is empty (the common case).
#[inline]
fn is_suppressed(code: &str, suppress: &[String]) -> bool {
    !suppress.is_empty()
        && !code.is_empty()
        && suppress.iter().any(|s| s.eq_ignore_ascii_case(code))
}

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

#[derive(Debug)]
pub(crate) struct DiagnosticBudget {
    max_errors: Option<usize>,
    error_count: usize,
    stopped_early: bool,
}

impl DiagnosticBudget {
    pub(crate) fn new(max_errors: Option<usize>) -> Self {
        Self {
            max_errors,
            error_count: 0,
            stopped_early: false,
        }
    }

    pub(crate) fn error_count(&self) -> usize {
        self.error_count
    }

    fn stopped_early(&self) -> bool {
        self.stopped_early
    }

    fn errors_remaining(&self) -> bool {
        self.max_errors.is_none_or(|limit| self.error_count < limit)
    }

    pub(crate) fn would_block_further_errors(&self) -> bool {
        !self.errors_remaining()
    }

    fn count_error(&mut self) -> bool {
        if !self.errors_remaining() {
            self.stopped_early = true;
            return false;
        }
        self.error_count += 1;
        true
    }

    pub(crate) fn render_diag(
        &mut self,
        diag: &Diagnostic,
        source_map: &std::sync::Arc<fidan_source::SourceMap>,
        suppress: &[String],
    ) {
        if diag.severity == fidan_diagnostics::Severity::Error && !self.count_error() {
            return;
        }
        if !is_suppressed(diag.code.as_str(), suppress) {
            last_error::record(diag.code.as_str(), &diag.message);
            fidan_diagnostics::render_to_stderr(diag, source_map);
        }
    }

    fn render_message(&mut self, severity: Severity, code: impl std::fmt::Display, message: &str) {
        if severity == Severity::Error && !self.count_error() {
            return;
        }
        let code_s = code.to_string();
        last_error::record(&code_s, message);
        render_message_to_stderr(severity, code_s, message);
    }
}

pub(crate) fn emit_mir_safety_diags(
    mir: &fidan_mir::MirProgram,
    interner: &fidan_lexer::SymbolInterner,
    strict_mode: bool,
    suppress: &[String],
    budget: &mut DiagnosticBudget,
) {
    if budget.would_block_further_errors() {
        return;
    }

    // ── E0401: parallel data-race check ──────────────────────────────────────
    for diag in fidan_passes::check_parallel_races(mir, interner) {
        if !is_suppressed("E0401", suppress) {
            budget.render_message(
                Severity::Error,
                fidan_diagnostics::diag_code!("E0401"),
                &format!("data race on `{}`: {}", diag.var_name, diag.context),
            );
        }
        if budget.would_block_further_errors() {
            return;
        }
    }

    // ── W1004: unawaited Pending check ───────────────────────────────────────
    for diag in fidan_passes::check_unawaited_pending(mir, interner) {
        if is_suppressed("W1004", suppress) {
            continue;
        }
        let pl = if diag.count == 1 { "" } else { "s" };
        let message = format!(
            "action `{}` contains {} unawaited `spawn` expression{} \
                 — result{} silently discarded; use `await` or `var _ = spawn \u{2026}` to suppress",
            diag.fn_name,
            diag.count,
            pl,
            if diag.count == 1 { " is" } else { "s are" },
        );
        last_error::record("W1004", &message);
        render_message_to_stderr(
            Severity::Warning,
            fidan_diagnostics::diag_code!("W1004"),
            &message,
        );
    }

    // ── W2006: null-safety check ──────────────────────────────────────────────
    for diag in fidan_passes::check_null_safety(mir, interner) {
        let sev = if strict_mode {
            Severity::Error
        } else {
            Severity::Warning
        };
        if !is_suppressed("W2006", suppress) {
            budget.render_message(
                sev,
                fidan_diagnostics::diag_code!("W2006"),
                &format!(
                    "in `{}`: {} — this will panic at runtime",
                    diag.fn_name, diag.context
                ),
            );
        }
        if strict_mode && budget.would_block_further_errors() {
            return;
        }
    }

    // ── W5001 / W5003: compile-time slow hints ────────────────────────────────
    for diag in fidan_passes::check_slow_hints(mir, interner) {
        let code = match diag.code {
            "W5001" => fidan_diagnostics::diag_code!("W5001"),
            "W5003" => fidan_diagnostics::diag_code!("W5003"),
            _ => continue,
        };
        if is_suppressed(diag.code, suppress) {
            if strict_mode && diag.code == "W5001" {
                let _ = budget.count_error();
            }
            continue;
        }
        let sev = if strict_mode && diag.code == "W5001" {
            Severity::Error
        } else {
            Severity::Warning
        };
        budget.render_message(sev, code, &diag.context);
        if strict_mode && diag.code == "W5001" && budget.would_block_further_errors() {
            return;
        }
    }
}

pub(crate) fn run_pipeline(mut opts: CompileOptions) -> Result<()> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let is_stdin = opts.input.as_os_str() == "-";

    // ── Extension check (skipped for stdin) ───────────────────────────────────
    if !is_stdin
        && opts.input.extension().and_then(|e| e.to_str()) != Some("fdn")
        && !is_suppressed("W2001", &opts.suppress)
    {
        let message = format!(
            "file '{}' does not have the '.fdn' extension",
            opts.input.display()
        );
        last_error::record("W2001", &message);
        render_message_to_stderr(
            Severity::Warning,
            fidan_diagnostics::diag_code!("W2001"),
            &message,
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

    let mut budget = DiagnosticBudget::new(opts.max_errors);

    // ── Lex ────────────────────────────────────────────────────────────────────
    let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    for diag in &lex_diags {
        budget.render_diag(diag, &source_map, &opts.suppress);
        if budget.would_block_further_errors() {
            break;
        }
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
        budget.render_diag(diag, &source_map, &opts.suppress);
        if budget.would_block_further_errors() {
            break;
        }
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
    let mut error_count: usize = budget.error_count();

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

    if !is_stdin {
        for message in fidan_driver::detect_import_cycles(&opts.input) {
            budget.render_message(Severity::Error, "", &message);
            error_count = budget.error_count();
            if budget.would_block_further_errors() {
                break;
            }
        }
    }

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
            let diag = fidan_diagnostics::Diagnostic::error(
                fidan_diagnostics::diag_code!("E0106"),
                format!("module `{name}` not found"),
                span,
            );
            budget.render_diag(&diag, &source_map, &opts.suppress);
            if budget.would_block_further_errors() {
                break;
            }
        }
        let mut queue: VecDeque<QueueItem> = main_paths
            .into_iter()
            .map(|(p, _, filter)| (p, true, filter)) // direct imports always exposed
            .collect();

        // Track canonical paths to break import cycles.
        let mut loaded: HashSet<std::path::PathBuf> = HashSet::new();
        if !is_stdin && let Ok(canon) = opts.input.canonicalize() {
            loaded.insert(canon);
        }

        while let Some((import_path, expose, filter)) = queue.pop_front() {
            if budget.would_block_further_errors() {
                break;
            }
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
                        budget.render_diag(d, &source_map, &opts.suppress);
                        if budget.would_block_further_errors() {
                            break;
                        }
                    }
                    let (imp_module, imp_parse_diags) =
                        fidan_parser::parse(&imp_tokens, imp_file.id, Arc::clone(&interner));
                    for d in &imp_parse_diags {
                        budget.render_diag(d, &source_map, &opts.suppress);
                        if budget.would_block_further_errors() {
                            break;
                        }
                    }
                    let imp_lex_err = imp_lex_diags
                        .iter()
                        .any(|d| d.severity == fidan_diagnostics::Severity::Error);
                    let imp_parse_err = imp_parse_diags
                        .iter()
                        .any(|d| d.severity == fidan_diagnostics::Severity::Error);
                    error_count = budget.error_count();

                    // Enqueue transitive imports (they use their own internal resolution,
                    // no name filter needed — the filter only applies at the call site).
                    let imp_base = import_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    let (sub_paths, sub_unresolved) =
                        collect_file_import_paths(&imp_module, &interner, &imp_base);
                    for (name, span) in sub_unresolved {
                        let diag = fidan_diagnostics::Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0106"),
                            format!("module `{name}` not found"),
                            span,
                        );
                        budget.render_diag(&diag, &source_map, &opts.suppress);
                        if budget.would_block_further_errors() {
                            break;
                        }
                    }
                    for (sub, sub_re_export, sub_filter) in sub_paths {
                        queue.push_back((sub, expose && sub_re_export, sub_filter));
                    }

                    // Typeck + HIR-lower the imported module (only if lex/parse clean).
                    if !imp_lex_err && !imp_parse_err && !budget.would_block_further_errors() {
                        let imp_tm =
                            fidan_typeck::typecheck_full(&imp_module, Arc::clone(&interner));
                        for d in &imp_tm.diagnostics {
                            budget.render_diag(d, &source_map, &opts.suppress);
                            if budget.would_block_further_errors() {
                                break;
                            }
                        }
                        let imp_tc_err = imp_tm
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == fidan_diagnostics::Severity::Error);
                        error_count = budget.error_count();
                        if !imp_tc_err && !budget.would_block_further_errors() {
                            let imp_hir = fidan_hir::lower_module(&imp_module, &imp_tm, &interner);
                            // Always push so private transitive deps are compiled into MIR
                            // (their functions may be called by the importing module).
                            // `expose` only controls whether names appear in the outer typeck.
                            hirs.push((imp_hir, filter, expose));
                        }
                    }
                }
                Err(e) => {
                    budget.render_message(
                        Severity::Error,
                        "",
                        &format!("cannot load import `{}`: {e}", import_path.display()),
                    );
                    error_count = budget.error_count();
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
            if is_suppressed(diag.code.as_str(), &opts.suppress) {
                if diag.severity == fidan_diagnostics::Severity::Error {
                    let _ = budget.count_error();
                    error_count = budget.error_count();
                }
                continue;
            }
            if opts.strict_mode
                && diag.severity == fidan_diagnostics::Severity::Warning
                && is_strict_escalated(&diag.code)
            {
                budget.render_message(Severity::Error, diag.code.as_str(), &diag.message);
                error_count = budget.error_count();
            } else {
                budget.render_diag(diag, &source_map, &opts.suppress);
                error_count = budget.error_count();
            }
            if budget.would_block_further_errors() {
                break;
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
        let mut footer = match opts.mode {
            ExecutionMode::Check => format!("found {error_count} error{s} in `{source_name}`"),
            ExecutionMode::Interpret => {
                format!("could not run `{source_name}` — {error_count} error{s}")
            }
            _ => format!("could not compile `{source_name}` — {error_count} error{s}"),
        };
        if budget.stopped_early() {
            footer.push_str(" (stopped early)");
        }
        render_message_to_stderr(Severity::Note, "", &footer);
        if opts.mode != ExecutionMode::Check {
            eprintln!(
                "         run `fidan check` to list all errors, or `--max-errors N` to stop early"
            );
        }
    }
    match opts.mode {
        ExecutionMode::Interpret => {
            if error_count == 0
                && let Some(ref hir) = merged_hir
            {
                let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                // ── MIR safety analysis (E0401, W1004) ───────────────────────
                emit_mir_safety_diags(
                    &mir,
                    &interner,
                    opts.strict_mode,
                    &opts.suppress,
                    &mut budget,
                );
                error_count = budget.error_count();
                if error_count == 0 {
                    // ── Optimisation passes (Phase 6) ─────────────────────
                    if opts.trace == TraceMode::Full {
                        fidan_passes::run_preserving_call_frames(&mut mir);
                    } else {
                        fidan_passes::run_all(&mut mir);
                    }
                    let mut program_argv = vec![opts.input.display().to_string()];
                    program_argv.extend(opts.program_args.iter().cloned());
                    let _program_args_guard = push_program_args(program_argv);
                    let replay_inputs = std::mem::take(&mut opts.replay_inputs);
                    let sandbox = opts.sandbox.take();
                    let (result, captured) = fidan_interp::run_mir_with_replay(
                        mir,
                        Arc::clone(&interner),
                        Arc::clone(&source_map),
                        opts.jit_threshold,
                        replay_inputs,
                        sandbox,
                    );
                    if let Err(err) = result {
                        last_error::record(err.code, &err.message);
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
                        // Save replay bundle if any stdin was captured.
                        if !captured.is_empty() {
                            match save_replay_bundle(&opts.input, &captured) {
                                Ok(id) => render_message_to_stderr(
                                    Severity::Note,
                                    "replay",
                                    &format!(
                                        "inputs saved — reproduce with: fidan run {} --replay {id}",
                                        opts.input.display()
                                    ),
                                ),
                                Err(e) => render_message_to_stderr(
                                    Severity::Note,
                                    "replay",
                                    &format!("could not save replay bundle: {e}"),
                                ),
                            }
                        }
                        std::process::exit(1);
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
            if error_count == 0
                && let Some(ref hir) = merged_hir
            {
                let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                emit_mir_safety_diags(
                    &mir,
                    &interner,
                    opts.strict_mode,
                    &opts.suppress,
                    &mut budget,
                );
                error_count = budget.error_count();
                if error_count == 0 {
                    fidan_passes::run_all(&mut mir);
                    let session = fidan_driver::Session::new();
                    if let Err(e) =
                        fidan_driver::compile(&session, mir, Arc::clone(&interner), &opts)
                    {
                        error_count += 1;
                        render_message_to_stderr(
                            Severity::Error,
                            "",
                            &format!("AOT compilation failed: {e:#}"),
                        );
                    }
                }
            }
        }
        ExecutionMode::Profile => {
            if error_count == 0
                && let Some(ref hir) = merged_hir
            {
                let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                // Safety analysis warns about potential issues — profile run
                // continues regardless (warnings don't block profiling).
                emit_mir_safety_diags(&mir, &interner, false, &opts.suppress, &mut budget);
                fidan_passes::run_all(&mut mir);
                let prog_name = opts
                    .input
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                let _program_args_guard = push_program_args(vec![opts.input.display().to_string()]);
                let (result, report) = fidan_interp::run_mir_with_profile(
                    mir,
                    Arc::clone(&interner),
                    Arc::clone(&source_map),
                    &prog_name,
                );
                if let Err(ref err) = result {
                    last_error::record(err.code, &err.message);
                    render_message_to_stderr(Severity::Error, err.code, &err.message);
                    if !err.trace.is_empty() {
                        render_trace_to_stderr(&err.trace, TraceMode::Short);
                    }
                }
                if let Some(ref rep) = report {
                    print!("{}", rep.render());
                    if let Some(ref out_path) = opts.output {
                        match std::fs::write(out_path, rep.render_json()) {
                            Ok(()) => render_message_to_stderr(
                                Severity::Note,
                                "profile",
                                &format!("JSON written to {}", out_path.display()),
                            ),
                            Err(e) => render_message_to_stderr(
                                Severity::Error,
                                fidan_diagnostics::diag_code!("R0001"),
                                &{
                                    let message = format!("could not write profile output: {e}");
                                    last_error::record("R0001", &message);
                                    message
                                },
                            ),
                        }
                    }
                }
                if result.is_err() {
                    std::process::exit(1);
                }
            }
        }
        ExecutionMode::Test => {
            if error_count == 0
                && let Some(ref hir) = merged_hir
            {
                let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                // ── MIR safety analysis (E0401, W1004) ───────────────────
                emit_mir_safety_diags(
                    &mir,
                    &interner,
                    opts.strict_mode,
                    &opts.suppress,
                    &mut budget,
                );
                error_count = budget.error_count();
                if error_count == 0 {
                    fidan_passes::run_all(&mut mir);
                    let test_count = mir.test_functions.len();
                    if test_count == 0 {
                        render_message_to_stderr(Severity::Note, "", "no test blocks found");
                    } else {
                        let _program_args_guard =
                            push_program_args(vec![opts.input.display().to_string()]);
                        match fidan_interp::run_tests(
                            mir,
                            Arc::clone(&interner),
                            Arc::clone(&source_map),
                        ) {
                            (Err(err), _) => {
                                // Initialisation (top-level code) crashed before tests ran.
                                last_error::record(err.code, &err.message);
                                render_message_to_stderr(
                                    Severity::Error,
                                    err.code,
                                    &format!("program initialisation failed: {}", err.message),
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

    if error_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn run_new_scaffolds_standard_project() -> Result<()> {
        let sandbox = make_temp_dir("fidan_new_standard");
        run_new("hello-app", Some(&sandbox), false)?;
        let project_dir = sandbox.join("hello-app");
        assert!(project_dir.join("main.fdn").is_file());
        assert!(!project_dir.join("dal.toml").exists());
        fs::remove_dir_all(&sandbox).ok();
        Ok(())
    }

    #[test]
    fn run_new_scaffolds_dal_package_project() -> Result<()> {
        let sandbox = make_temp_dir("fidan_new_package");
        run_new("hello-package", Some(&sandbox), true)?;
        let project_dir = sandbox.join("hello-package");
        assert!(project_dir.join("dal.toml").is_file());
        assert!(project_dir.join("README.md").is_file());
        assert!(project_dir.join("src").join("init.fdn").is_file());
        assert!(!project_dir.join("main.fdn").exists());
        fs::remove_dir_all(&sandbox).ok();
        Ok(())
    }

    #[test]
    fn run_new_rejects_invalid_dal_package_names() {
        let sandbox = make_temp_dir("fidan_new_invalid_package");
        let err = run_new("HelloPkg", Some(&sandbox), true)
            .expect_err("expected invalid package name error");
        assert!(
            err.to_string()
                .contains("Dal package names must be lowercase")
        );
        fs::remove_dir_all(&sandbox).ok();
    }

    fn make_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
        fs::create_dir_all(&dir).expect("failed to create temp test dir");
        dir
    }
}
