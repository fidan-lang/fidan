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
    Repl {
        /// Print the call stack on uncaught panics: none | short | full | compact
        #[arg(long, default_value = "short")]
        trace: String,
    },
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
    // Pre-warm Rayon's global thread pool so the first `parallel for` in
    // user code pays no OS thread-spawn latency.
    rayon::ThreadPoolBuilder::new().build_global().ok();
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
        Command::Repl { trace } => {
            let trace_mode = parse_trace(&trace)?;
            run_repl(trace_mode)
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
fn emit_mir_safety_diags(
    mir: &fidan_mir::MirProgram,
    interner: &fidan_lexer::SymbolInterner,
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
    std::collections::VecDeque<(std::path::PathBuf, bool)>,
    Vec<(String, fidan_source::Span)>,
) {
    let mut paths = std::collections::VecDeque::new();
    let mut unresolved: Vec<(String, fidan_source::Span)> = Vec::new();
    for &item_id in &module.items {
        let item = module.arena.get_item(item_id);
        if let fidan_ast::Item::Use {
            path,
            re_export,
            span,
            ..
        } = item
        {
            if path.is_empty() {
                continue;
            }
            let first = interner.resolve(path[0]);

            // ── Explicit file-path import (string with ./ ../ / or .fdn) ───
            if path.len() == 1
                && (first.starts_with("./")
                    || first.starts_with("../")
                    || first.starts_with('/')
                    || first.ends_with(".fdn"))
            {
                paths.push_back((base_dir.join(&*first), *re_export));
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
            if let Some(resolved) = find_relative(base_dir, &segments) {
                paths.push_back((resolved, *re_export));
            } else {
                unresolved.push((segments.join("."), *span));
            }
        }
    }
    (paths, unresolved)
}

/// Pre-register functions, objects, and globals from `hir` into `tc` so the
/// main file's type-checker sees imported symbols as known bindings.
fn pre_register_hir_into_tc(tc: &mut fidan_typeck::TypeChecker, hir: &fidan_hir::HirModule) {
    use fidan_typeck::{ActionInfo, ParamInfo};

    for func in &hir.functions {
        let info = ActionInfo {
            params: func
                .params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name,
                    ty: p.ty.clone(),
                    required: p.required,
                    has_default: p.default.is_some(),
                })
                .collect(),
            return_ty: func.return_ty.clone(),
            span: func.span,
        };
        tc.pre_register_action(func.name, info);
    }

    for obj in &hir.objects {
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
                            required: p.required,
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
            tc.pre_register_global(*name, ty.clone(), *is_const);
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

    let imported_hirs: Vec<fidan_hir::HirModule> = {
        use fidan_lexer::Lexer;
        use std::collections::{HashSet, VecDeque};

        let mut hirs: Vec<fidan_hir::HirModule> = Vec::new();
        // Queue carries `(path, expose)` where `expose = true` means the file's
        // symbols are visible in the top-level program:
        //   • Direct imports of `main` are always exposed (expose = true).
        //   • Transitive sub-imports inherit exposure only when the parent used
        //     `export use`; plain `use` keeps them private to that file.
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
        let mut queue: VecDeque<(std::path::PathBuf, bool)> = main_paths
            .into_iter()
            .map(|(p, _)| (p, true)) // direct imports of main are always exposed
            .collect();

        // Track canonical paths to break cycles.
        let mut loaded: HashSet<std::path::PathBuf> = HashSet::new();
        if !is_stdin {
            if let Ok(canon) = opts.input.canonicalize() {
                loaded.insert(canon);
            }
        }

        while let Some((import_path, expose)) = queue.pop_front() {
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

                    // Enqueue transitive imports from this imported file.
                    // A sub-import is exposed to the top-level program only if the
                    // current file is itself exposed AND the sub-import was declared
                    // with `export use`.  Plain `use` keeps sub-imports private.
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
                    for (sub, sub_re_export) in sub_paths {
                        queue.push_back((sub, expose && sub_re_export));
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
                            // Only merge into the program if this import is exposed.
                            // Private imports (plain `use` in a library file) are compiled
                            // for correctness checking but their symbols stay local.
                            if expose {
                                hirs.push(imp_hir);
                            }
                        }
                    }
                }
                Err(e) => {
                    error_count += 1;
                    eprintln!("error: cannot load import `{}`: {e}", import_path.display());
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
        for hir in &imported_hirs {
            pre_register_hir_into_tc(&mut tc, hir);
        }
        tc.check_module(&module);
        let tm = tc.finish_typed();
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

    // ── Merge HIR: base module + all imported HIR modules ─────────────────────
    //
    // Compute once; all --emit and run blocks share the merged result without
    // re-calling lower_module.
    let merged_hir: Option<fidan_hir::HirModule> = typed_module.as_ref().map(|tm| {
        let base = fidan_hir::lower_module(&module, tm, &interner);
        imported_hirs
            .into_iter()
            .fold(base, |acc, imp| fidan_hir::merge_module(acc, imp))
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
            let mir = fidan_mir::lower_program(hir, &interner);
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
                    let mut mir = fidan_mir::lower_program(hir, &interner);
                    // ── MIR safety analysis (E0401, W1004) ───────────────────────
                    error_count += emit_mir_safety_diags(&mir, &interner);
                    if error_count == 0 {
                        // ── Optimisation passes (Phase 6) ─────────────────────
                        fidan_passes::run_all(&mut mir);
                        let result = fidan_interp::run_mir(
                            mir,
                            Arc::clone(&interner),
                            Arc::clone(&source_map),
                        );
                        if let Err(err) = result {
                            render_message_to_stderr(Severity::Error, err.code, &err.message);
                            if !err.trace.is_empty() && opts.trace != TraceMode::None {
                                render_trace_to_stderr(&err.trace, opts.trace);
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
                    let mut mir = fidan_mir::lower_program(hir, &interner);
                    // ── MIR safety analysis (E0401, W1004) ───────────────────
                    error_count += emit_mir_safety_diags(&mir, &interner);
                    if error_count == 0 {
                        fidan_passes::run_all(&mut mir);
                        match fidan_interp::run_mir(
                            mir,
                            Arc::clone(&interner),
                            Arc::clone(&source_map),
                        ) {
                            Ok(()) => {
                                eprintln!("\x1b[1;32mtest passed\x1b[0m");
                            }
                            Err(err) => {
                                let msg = err.message.trim_start_matches("assertion failed: ");
                                eprintln!("\x1b[1;31mtest failed\x1b[0m: {}", msg);
                                if !err.trace.is_empty() && opts.trace != TraceMode::None {
                                    render_trace_to_stderr(&err.trace, opts.trace);
                                }
                                std::process::exit(1);
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
            error_history.push(format!("[{}]: {}", diag.code, diag.message));
        }

        let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));

        for diag in &parse_diags {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
            error_history.push(format!("[{}]: {}", diag.code, diag.message));
        }

        if lex_diags.is_empty() && parse_diags.is_empty() {
            tc.check_module(&module);
            let type_diags = tc.drain_diags();
            if type_diags.is_empty() {
                match fidan_interp::run_repl_line(&mut repl_state, module) {
                    Ok(Some(echo)) => println!("{echo}"),
                    Ok(None) => {}
                    Err(e) => {
                        render_message_to_stderr(Severity::Error, e.code, &e.message);
                        render_trace_to_stderr(&e.trace, trace_mode);
                        error_history.push(e.message);
                    }
                }
            } else {
                for diag in &type_diags {
                    fidan_diagnostics::render_to_stderr(diag, &source_map);
                    error_history.push(format!("[{}]: {}", diag.code, diag.message));
                }
            }
        }
    }

    println!();
    println!("Bye! 👋");
    Ok(())
}
