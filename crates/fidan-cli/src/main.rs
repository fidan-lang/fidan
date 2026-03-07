#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::{CompileOptions, EmitKind, ExecutionMode, SandboxPolicy, TraceMode};
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
        /// Replay a previous run using a saved bundle ID (e.g. `a1b2c3d4`)
        /// or an explicit path to a `.bundle` file
        #[arg(long)]
        replay: Option<String>,
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
        /// Enable zero-config sandbox: deny all file, env, net, and spawn by default
        #[arg(long)]
        sandbox: bool,
        /// Allow file-system reads from path prefix (repeatable; `*` = allow all)
        #[arg(long, value_delimiter = ',')]
        allow_read: Vec<String>,
        /// Allow file-system writes to path prefix (repeatable; `*` = allow all)
        #[arg(long, value_delimiter = ',')]
        allow_write: Vec<String>,
        /// Allow environment variable access (`getEnv`, `setEnv`, `args`, `cwd`)
        #[arg(long)]
        allow_env: bool,
        /// Allow network access (reserved for a future `std.net` module)
        #[arg(long)]
        allow_net: bool,
        /// Allow subprocess spawn (reserved for a future `std.process` module)
        #[arg(long)]
        allow_spawn: bool,
        /// Wall-time limit in seconds when `--sandbox` is active (0 = no limit)
        #[arg(long)]
        time_limit: Option<u64>,
        /// Memory limit in MB when `--sandbox` is active (0 = no limit)
        #[arg(long)]
        mem_limit: Option<u64>,
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
    /// Profile a Fidan source file: call counts, time per action, hot-path hints
    Profile {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Write profiling data to a file in JSON format
        #[arg(long)]
        profile_out: Option<PathBuf>,
        /// Suppress specific diagnostic codes (comma-separated)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
    },
    /// Run `test { ... }` blocks in a Fidan source file
    Test {
        file: PathBuf,
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
    },
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
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
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
    /// Explain what one or more source lines do (static analysis, offline, zero AI)
    ExplainLine {
        /// Path to the .fdn source file
        file: PathBuf,
        /// First line to explain (1-based)
        #[arg(long)]
        line: usize,
        /// Last line to explain, inclusive (defaults to --line)
        #[arg(long)]
        end_line: Option<usize>,
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

// ── fidan explain-line ────────────────────────────────────────────────────────────────
//
// Static analysis report for one or more source lines.
// Uses the AST + typeck `expr_types` map — fully offline, zero AI.

fn run_explain_line(file: PathBuf, line_start: usize, line_end: usize) -> Result<()> {
    use fidan_ast::{BinOp, Expr, Item, Stmt};
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    if line_start == 0 {
        bail!("--line is 1-based; 0 is not a valid line number");
    }
    let line_end = line_end.max(line_start);

    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;
    let source_name = file.display().to_string();
    let source_map = Arc::new(SourceMap::new());
    let interner = Arc::new(SymbolInterner::new());
    let f = source_map.add_file(&*source_name, &*src);
    let (tokens, _) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
    let (module, parse_diags) = fidan_parser::parse(&tokens, f.id, Arc::clone(&interner));
    if !parse_diags.is_empty() {
        for d in &parse_diags {
            fidan_diagnostics::render_to_stderr(d, &source_map);
        }
        bail!("parse errors prevent line explanation");
    }
    let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));

    // ── Local helpers ──────────────────────────────────────────────────────
    fn offset_line(src: &str, offset: usize) -> usize {
        src[..offset.min(src.len())]
            .chars()
            .filter(|&c| c == '\n')
            .count()
            + 1
    }

    fn span_overlaps(src: &str, span: fidan_source::Span, lo: usize, hi: usize) -> bool {
        let s = offset_line(src, span.start as usize);
        let e = offset_line(src, span.end.saturating_sub(1) as usize);
        s <= hi && e >= lo
    }

    fn type_name(ty: &fidan_typeck::FidanType) -> String {
        ty.to_string()
    }

    // Collect all Expr::Ident names (and their types) reachable from an ExprId.
    fn collect_reads(
        eid: fidan_ast::ExprId,
        module: &fidan_ast::Module,
        interner: &SymbolInterner,
        typed: &fidan_typeck::TypedModule,
        out: &mut Vec<(String, Option<String>)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let expr = module.arena.get_expr(eid);
        match expr {
            Expr::Ident { name, .. } => {
                let s = interner.resolve(*name).to_string();
                if seen.insert(s.clone()) {
                    let ty_s = typed.expr_types.get(&eid).map(|t| type_name(t));
                    out.push((s, ty_s));
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                collect_reads(*lhs, module, interner, typed, out, seen);
                collect_reads(*rhs, module, interner, typed, out, seen);
            }
            Expr::Unary { operand, .. } => {
                collect_reads(*operand, module, interner, typed, out, seen);
            }
            Expr::NullCoalesce { lhs, rhs, .. } => {
                collect_reads(*lhs, module, interner, typed, out, seen);
                collect_reads(*rhs, module, interner, typed, out, seen);
            }
            Expr::Call { callee, args, .. } => {
                collect_reads(*callee, module, interner, typed, out, seen);
                for a in args {
                    collect_reads(a.value, module, interner, typed, out, seen);
                }
            }
            Expr::Field { object, .. } => {
                collect_reads(*object, module, interner, typed, out, seen);
            }
            Expr::Index { object, index, .. } => {
                collect_reads(*object, module, interner, typed, out, seen);
                collect_reads(*index, module, interner, typed, out, seen);
            }
            Expr::Assign { value, .. } | Expr::CompoundAssign { value, .. } => {
                collect_reads(*value, module, interner, typed, out, seen);
            }
            Expr::Ternary {
                condition,
                then_val,
                else_val,
                ..
            } => {
                collect_reads(*condition, module, interner, typed, out, seen);
                collect_reads(*then_val, module, interner, typed, out, seen);
                collect_reads(*else_val, module, interner, typed, out, seen);
            }
            Expr::List { elements, .. } => {
                for e in elements {
                    collect_reads(*e, module, interner, typed, out, seen);
                }
            }
            Expr::Tuple { elements, .. } => {
                for e in elements {
                    collect_reads(*e, module, interner, typed, out, seen);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    collect_reads(*k, module, interner, typed, out, seen);
                    collect_reads(*v, module, interner, typed, out, seen);
                }
            }
            Expr::StringInterp { parts, .. } => {
                for p in parts {
                    if let fidan_ast::InterpPart::Expr(e) = p {
                        collect_reads(*e, module, interner, typed, out, seen);
                    }
                }
            }
            Expr::Spawn { expr, .. } | Expr::Await { expr, .. } => {
                collect_reads(*expr, module, interner, typed, out, seen);
            }
            Expr::Slice {
                target,
                start,
                end,
                step,
                ..
            } => {
                collect_reads(*target, module, interner, typed, out, seen);
                if let Some(s) = start {
                    collect_reads(*s, module, interner, typed, out, seen);
                }
                if let Some(e) = end {
                    collect_reads(*e, module, interner, typed, out, seen);
                }
                if let Some(s) = step {
                    collect_reads(*s, module, interner, typed, out, seen);
                }
            }
            _ => {}
        }
    }

    // Collect names of variables written by a statement.
    fn collect_writes(
        stmt: &Stmt,
        module: &fidan_ast::Module,
        interner: &SymbolInterner,
    ) -> Vec<String> {
        let mut out = Vec::new();
        match stmt {
            Stmt::VarDecl { name, .. }
            | Stmt::For { binding: name, .. }
            | Stmt::ParallelFor { binding: name, .. } => {
                out.push(interner.resolve(*name).to_string());
            }
            Stmt::Destructure { bindings, .. } => {
                for b in bindings {
                    out.push(interner.resolve(*b).to_string());
                }
            }
            Stmt::Assign { target, .. } => {
                // Walk target to find the root ident name(s).
                fn extract_target(
                    eid: fidan_ast::ExprId,
                    module: &fidan_ast::Module,
                    interner: &SymbolInterner,
                    out: &mut Vec<String>,
                ) {
                    match module.arena.get_expr(eid) {
                        Expr::Ident { name, .. } => out.push(interner.resolve(*name).to_string()),
                        Expr::Field { object, .. } | Expr::Index { object, .. } => {
                            extract_target(*object, module, interner, out)
                        }
                        Expr::CompoundAssign { target, .. } | Expr::Assign { target, .. } => {
                            extract_target(*target, module, interner, out)
                        }
                        _ => {}
                    }
                }
                extract_target(*target, module, interner, &mut out);
            }
            Stmt::Expr { expr, .. } => match module.arena.get_expr(*expr) {
                Expr::Assign { target, .. } | Expr::CompoundAssign { target, .. } => {
                    fn extract_target(
                        eid: fidan_ast::ExprId,
                        module: &fidan_ast::Module,
                        interner: &SymbolInterner,
                        out: &mut Vec<String>,
                    ) {
                        match module.arena.get_expr(eid) {
                            Expr::Ident { name, .. } => {
                                out.push(interner.resolve(*name).to_string())
                            }
                            Expr::Field { object, .. } | Expr::Index { object, .. } => {
                                extract_target(*object, module, interner, out)
                            }
                            _ => {}
                        }
                    }
                    extract_target(*target, module, interner, &mut out);
                }
                _ => {}
            },
            _ => {}
        }
        out
    }

    // Binary-op risks.
    fn binary_risks(eid: fidan_ast::ExprId, module: &fidan_ast::Module) -> Vec<String> {
        let mut out = Vec::new();
        fn scan(
            eid: fidan_ast::ExprId,
            module: &fidan_ast::Module,
            out: &mut Vec<String>,
            seen_div: &mut bool,
            seen_idx: &mut bool,
        ) {
            match module.arena.get_expr(eid) {
                Expr::Binary { op, lhs, rhs, .. } => {
                    match op {
                        BinOp::Div | BinOp::Rem => {
                            if !*seen_div {
                                out.push("division or modulo by zero".to_string());
                                *seen_div = true;
                            }
                        }
                        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Pow => {
                            out.push("integer overflow on very large values".to_string());
                        }
                        _ => {}
                    }
                    scan(*lhs, module, out, seen_div, seen_idx);
                    scan(*rhs, module, out, seen_div, seen_idx);
                }
                Expr::Index { object, index, .. } => {
                    if !*seen_idx {
                        out.push("index out of bounds".to_string());
                        *seen_idx = true;
                    }
                    scan(*object, module, out, seen_div, seen_idx);
                    scan(*index, module, out, seen_div, seen_idx);
                }
                Expr::Call { callee, args, .. } => {
                    scan(*callee, module, out, seen_div, seen_idx);
                    for a in args {
                        scan(a.value, module, out, seen_div, seen_idx);
                    }
                }
                Expr::Assign { value, .. } | Expr::CompoundAssign { value, .. } => {
                    scan(*value, module, out, seen_div, seen_idx);
                }
                _ => {}
            }
        }
        scan(eid, module, &mut out, &mut false, &mut false);
        out.dedup();
        out
    }

    // Plain-English description of an expression.
    fn describe_expr(
        eid: fidan_ast::ExprId,
        module: &fidan_ast::Module,
        interner: &SymbolInterner,
        typed: &fidan_typeck::TypedModule,
        depth: usize,
    ) -> String {
        let expr = module.arena.get_expr(eid);
        match expr {
            Expr::IntLit { value, .. } => format!("integer literal `{value}`"),
            Expr::FloatLit { value, .. } => format!("float literal `{value}`"),
            Expr::StrLit { value, .. } => format!("string literal `\"{value}\"`"),
            Expr::BoolLit { value, .. } => format!("boolean `{value}`"),
            Expr::Nothing { .. } => "`nothing`".to_string(),
            Expr::Ident { name, .. } => {
                let s = interner.resolve(*name);
                if let Some(ty) = typed.expr_types.get(&eid) {
                    format!("`{s}` ({ty})")
                } else {
                    format!("`{s}`")
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let op_s = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Rem => "%",
                    BinOp::Pow => "**",
                    BinOp::Eq => "==",
                    BinOp::NotEq => "!=",
                    BinOp::Lt => "<",
                    BinOp::LtEq => "<=",
                    BinOp::Gt => ">",
                    BinOp::GtEq => ">=",
                    BinOp::And => "and",
                    BinOp::Or => "or",
                    BinOp::Range => "..",
                    BinOp::RangeInclusive => "...",
                    _ => "op",
                };
                if depth < 2 {
                    let l = describe_expr(*lhs, module, interner, typed, depth + 1);
                    let r = describe_expr(*rhs, module, interner, typed, depth + 1);
                    format!("{l} {op_s} {r}")
                } else {
                    format!("(binary `{op_s}`)")
                }
            }
            Expr::Unary { op, operand, .. } => {
                let op_s = match op {
                    fidan_ast::UnOp::Neg => "-",
                    fidan_ast::UnOp::Not => "not ",
                    fidan_ast::UnOp::Pos => "+",
                };
                let inner = describe_expr(*operand, module, interner, typed, depth + 1);
                format!("{op_s}{inner}")
            }
            Expr::Call { callee, args, .. } => {
                let callee_s = describe_expr(*callee, module, interner, typed, depth + 1);
                if args.is_empty() {
                    format!("call to `{callee_s}`")
                } else {
                    format!(
                        "call to `{callee_s}` with {} argument{}",
                        args.len(),
                        if args.len() == 1 { "" } else { "s" }
                    )
                }
            }
            Expr::Field { object, field, .. } => {
                let obj = describe_expr(*object, module, interner, typed, depth + 1);
                let f = interner.resolve(*field);
                format!("{obj}.{f}")
            }
            Expr::Index { object, index, .. } => {
                let obj = describe_expr(*object, module, interner, typed, depth + 1);
                let idx = describe_expr(*index, module, interner, typed, depth + 1);
                format!("{obj}[{idx}]")
            }
            Expr::StringInterp { .. } => "string interpolation".to_string(),
            Expr::List { elements, .. } => format!(
                "list literal ({} element{})",
                elements.len(),
                if elements.len() == 1 { "" } else { "s" }
            ),
            Expr::Dict { entries, .. } => format!(
                "dict literal ({} entr{})",
                entries.len(),
                if entries.len() == 1 { "y" } else { "ies" }
            ),
            Expr::Tuple { elements, .. } => format!(
                "tuple ({} element{})",
                elements.len(),
                if elements.len() == 1 { "" } else { "s" }
            ),
            Expr::Ternary { condition, .. } => {
                let c = describe_expr(*condition, module, interner, typed, depth + 1);
                format!("conditional expression (condition: {c})")
            }
            Expr::Spawn { expr, .. } => {
                let inner = describe_expr(*expr, module, interner, typed, depth + 1);
                format!("spawns async task: {inner}")
            }
            Expr::Await { expr, .. } => {
                let inner = describe_expr(*expr, module, interner, typed, depth + 1);
                format!("awaits result of: {inner}")
            }
            _ => "(expression)".to_string(),
        }
    }

    // Plain-English description of a statement.
    fn describe_stmt(
        stmt: &Stmt,
        module: &fidan_ast::Module,
        interner: &SymbolInterner,
        typed: &fidan_typeck::TypedModule,
    ) -> (String, Option<String>) {
        // (what, ty)
        match stmt {
            Stmt::VarDecl {
                name,
                init,
                is_const,
                ..
            } => {
                let n = interner.resolve(*name);
                let kind = if *is_const { "constant" } else { "variable" };
                let init_s = init
                    .map(|e| {
                        let d = describe_expr(e, module, interner, typed, 0);
                        format!(" = {d}")
                    })
                    .unwrap_or_default();
                let ty_s = init.and_then(|e| typed.expr_types.get(&e)).map(type_name);
                (format!("declares {kind} `{n}`{init_s}"), ty_s)
            }
            Stmt::Destructure {
                bindings, value, ..
            } => {
                let names: Vec<String> = bindings
                    .iter()
                    .map(|b| interner.resolve(*b).to_string())
                    .collect();
                let inner = describe_expr(*value, module, interner, typed, 0);
                (
                    format!("unpacks `({})` from {inner}", names.join(", ")),
                    None,
                )
            }
            Stmt::Assign { target, value, .. } => {
                let tgt = describe_expr(*target, module, interner, typed, 0);
                let val = describe_expr(*value, module, interner, typed, 0);
                let ty_s = typed.expr_types.get(value).map(type_name);
                (format!("sets `{tgt}` to {val}"), ty_s)
            }
            Stmt::Expr { expr, .. } => match module.arena.get_expr(*expr) {
                Expr::Assign { target, value, .. } => {
                    let tgt = describe_expr(*target, module, interner, typed, 0);
                    let val = describe_expr(*value, module, interner, typed, 0);
                    let ty_s = typed.expr_types.get(value).map(type_name);
                    (format!("sets `{tgt}` to {val}"), ty_s)
                }
                Expr::CompoundAssign {
                    op, target, value, ..
                } => {
                    let tgt = describe_expr(*target, module, interner, typed, 0);
                    let val = describe_expr(*value, module, interner, typed, 0);
                    let op_s = match op {
                        BinOp::Add => "+=",
                        BinOp::Sub => "-=",
                        BinOp::Mul => "*=",
                        BinOp::Div => "/=",
                        _ => "op=",
                    };
                    (
                        format!("applies `{op_s}` — updates `{tgt}` using {val}"),
                        None,
                    )
                }
                _ => {
                    let d = describe_expr(*expr, module, interner, typed, 0);
                    let ty_s = typed.expr_types.get(expr).map(type_name);
                    (d, ty_s)
                }
            },
            Stmt::Return { value, .. } => {
                let val_s = value
                    .map(|e| format!(" {}", describe_expr(e, module, interner, typed, 0)))
                    .unwrap_or_default();
                let ty_s = value.and_then(|e| typed.expr_types.get(&e)).map(type_name);
                (format!("returns{val_s}"), ty_s)
            }
            Stmt::If {
                condition,
                else_body,
                ..
            } => {
                let cond = describe_expr(*condition, module, interner, typed, 0);
                let has_else = else_body.is_some();
                (
                    format!(
                        "conditional branch on {cond}{}",
                        if has_else { " (has else branch)" } else { "" }
                    ),
                    None,
                )
            }
            Stmt::For {
                binding, iterable, ..
            } => {
                let b = interner.resolve(*binding);
                let iter = describe_expr(*iterable, module, interner, typed, 0);
                (
                    format!("iterates over {iter}, binding each element to `{b}`"),
                    None,
                )
            }
            Stmt::ParallelFor {
                binding, iterable, ..
            } => {
                let b = interner.resolve(*binding);
                let iter = describe_expr(*iterable, module, interner, typed, 0);
                (
                    format!("parallel-iterates over {iter}, binding each element to `{b}`"),
                    None,
                )
            }
            Stmt::While { condition, .. } => {
                let cond = describe_expr(*condition, module, interner, typed, 0);
                (format!("loops while {cond} is true"), None)
            }
            Stmt::Attempt {
                catches, finally, ..
            } => {
                let nc = catches.len();
                let has_fin = finally.is_some();
                (
                    format!(
                        "attempt/catch block ({nc} catch clause{}{}))",
                        if nc == 1 { "" } else { "s" },
                        if has_fin { ", with finally" } else { "" }
                    ),
                    None,
                )
            }
            Stmt::ConcurrentBlock {
                is_parallel, tasks, ..
            } => {
                let kind = if *is_parallel {
                    "parallel"
                } else {
                    "concurrent"
                };
                (
                    format!(
                        "{kind} block with {} task{}",
                        tasks.len(),
                        if tasks.len() == 1 { "" } else { "s" }
                    ),
                    None,
                )
            }
            Stmt::Panic { value, .. } => {
                let val = describe_expr(*value, module, interner, typed, 0);
                (format!("panics with {val}"), None)
            }
            Stmt::Break { .. } => ("breaks out of the enclosing loop".to_string(), None),
            Stmt::Continue { .. } => ("skips to the next loop iteration".to_string(), None),
            Stmt::Check {
                scrutinee, arms, ..
            } => {
                let scr = describe_expr(*scrutinee, module, interner, typed, 0);
                (
                    format!(
                        "pattern-matches on {scr} ({} arm{})",
                        arms.len(),
                        if arms.len() == 1 { "" } else { "s" }
                    ),
                    None,
                )
            }
            Stmt::Error { .. } => ("(parse error placeholder)".to_string(), None),
        }
    }

    // ── Walk the AST and collect explanations ──────────────────────────────
    struct Expl {
        line_range: String,
        source_text: String,
        context: String,
        what: String,
        ty: Option<String>,
        reads: Vec<(String, Option<String>)>,
        writes: Vec<String>,
        risks: Vec<String>,
    }

    let all_src_lines: Vec<&str> = src.lines().collect();

    fn extract_source_text(all_lines: &[&str], lo: usize, hi: usize) -> String {
        let s = lo.saturating_sub(1);
        let e = hi.min(all_lines.len());
        if s >= e {
            return String::new();
        }
        all_lines[s..e]
            .iter()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn process_stmt(
        stmt: &Stmt,
        module: &fidan_ast::Module,
        interner: &SymbolInterner,
        typed: &fidan_typeck::TypedModule,
        src: &str,
        all_src_lines: &[&str],
        line_start: usize,
        line_end: usize,
        context: &str,
        results: &mut Vec<Expl>,
    ) {
        let span = match stmt {
            Stmt::VarDecl { span, .. }
            | Stmt::Destructure { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::If { span, .. }
            | Stmt::Check { span, .. }
            | Stmt::For { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Attempt { span, .. }
            | Stmt::ParallelFor { span, .. }
            | Stmt::ConcurrentBlock { span, .. }
            | Stmt::Panic { span, .. }
            | Stmt::Error { span } => *span,
        };

        if !span_overlaps(src, span, line_start, line_end) {
            // Check recursively into compound stmts.
            let children: Vec<fidan_ast::StmtId> = match stmt {
                Stmt::If {
                    then_body,
                    else_ifs,
                    else_body,
                    ..
                } => {
                    let mut v: Vec<_> = then_body.clone();
                    for ei in else_ifs {
                        v.extend_from_slice(&ei.body);
                    }
                    if let Some(eb) = else_body {
                        v.extend_from_slice(eb);
                    }
                    v
                }
                Stmt::For { body, .. }
                | Stmt::ParallelFor { body, .. }
                | Stmt::While { body, .. } => body.clone(),
                Stmt::Attempt {
                    body,
                    catches,
                    otherwise,
                    finally,
                    ..
                } => {
                    let mut v: Vec<_> = body.clone();
                    for c in catches {
                        v.extend_from_slice(&c.body);
                    }
                    if let Some(o) = otherwise {
                        v.extend_from_slice(o);
                    }
                    if let Some(f) = finally {
                        v.extend_from_slice(f);
                    }
                    v
                }
                Stmt::ConcurrentBlock { tasks, .. } => {
                    tasks.iter().flat_map(|t| t.body.clone()).collect()
                }
                Stmt::Check { arms, .. } => arms.iter().flat_map(|a| a.body.clone()).collect(),
                _ => vec![],
            };
            for sid in children {
                let child = module.arena.get_stmt(sid);
                process_stmt(
                    child,
                    module,
                    interner,
                    typed,
                    src,
                    all_src_lines,
                    line_start,
                    line_end,
                    context,
                    results,
                );
            }
            return;
        }

        let stmt_lo = offset_line(src, span.start as usize);
        let stmt_hi = offset_line(src, span.end.saturating_sub(1) as usize);
        let source_text = extract_source_text(all_src_lines, stmt_lo, stmt_hi);
        let (what, ty) = describe_stmt(stmt, module, interner, typed);

        // Collect reads from all expressions in this stmt.
        let expr_ids_in_stmt: Vec<fidan_ast::ExprId> = match stmt {
            Stmt::VarDecl { init, .. } => init.iter().copied().collect(),
            Stmt::Destructure { value, .. }
            | Stmt::For {
                iterable: value, ..
            }
            | Stmt::ParallelFor {
                iterable: value, ..
            }
            | Stmt::While {
                condition: value, ..
            }
            | Stmt::If {
                condition: value, ..
            }
            | Stmt::Panic { value, .. } => vec![*value],
            Stmt::Assign { target, value, .. } => vec![*target, *value],
            Stmt::Expr { expr, .. } => vec![*expr],
            Stmt::Return { value, .. } => value.iter().copied().collect(),
            Stmt::Check { scrutinee, .. } => vec![*scrutinee],
            _ => vec![],
        };
        let mut reads: Vec<(String, Option<String>)> = Vec::new();
        let mut seen_reads = std::collections::HashSet::new();
        for eid in &expr_ids_in_stmt {
            collect_reads(*eid, module, interner, typed, &mut reads, &mut seen_reads);
        }

        // Remove from reads any names that are also writes (they're declared here).
        let writes = collect_writes(stmt, module, interner);
        reads.retain(|(name, _)| !writes.contains(name));

        // Collect risks from expression operators.
        let risks: Vec<String> = expr_ids_in_stmt
            .iter()
            .flat_map(|&eid| binary_risks(eid, module))
            .collect::<std::collections::BTreeSet<String>>()
            .into_iter()
            .collect();

        let line_range = if stmt_lo == stmt_hi {
            format!("line {stmt_lo}")
        } else {
            format!("lines {stmt_lo}–{stmt_hi}")
        };

        results.push(Expl {
            line_range,
            source_text: source_text.chars().take(120).collect(),
            context: context.to_string(),
            what,
            ty,
            reads,
            writes,
            risks,
        });
    }

    let mut results: Vec<Expl> = Vec::new();

    // Walk top-level items.
    for &iid in &module.items {
        let item = module.arena.get_item(iid);
        match item {
            Item::ActionDecl {
                name, body, params, ..
            }
            | Item::ExtensionAction {
                name, body, params, ..
            } => {
                let fn_name = interner.resolve(*name);
                let action_span = match item {
                    Item::ActionDecl { span, .. } | Item::ExtensionAction { span, .. } => *span,
                    _ => unreachable!(),
                };
                if span_overlaps(&src, action_span, line_start, line_end) {
                    // Walk body stmts.
                    let ctx = format!("in action `{fn_name}`");
                    for &sid in body {
                        let stmt = module.arena.get_stmt(sid);
                        process_stmt(
                            stmt,
                            &module,
                            &interner,
                            &typed,
                            &src,
                            &all_src_lines,
                            line_start,
                            line_end,
                            &ctx,
                            &mut results,
                        );
                    }
                    // If the action signature line itself is targeted, describe the declaration.
                    let sig_lo = offset_line(&src, action_span.start as usize);
                    if sig_lo >= line_start && sig_lo <= line_end && results.is_empty() {
                        let param_list: Vec<String> = params
                            .iter()
                            .map(|p| {
                                let pn = interner.resolve(p.name);
                                format!("`{pn}`")
                            })
                            .collect();
                        let what = if param_list.is_empty() {
                            format!("declares action `{fn_name}` with no parameters")
                        } else {
                            format!(
                                "declares action `{fn_name}` with parameters: {}",
                                param_list.join(", ")
                            )
                        };
                        results.push(Expl {
                            line_range: format!("line {sig_lo}"),
                            source_text: all_src_lines
                                .get(sig_lo.saturating_sub(1))
                                .unwrap_or(&"")
                                .chars()
                                .take(120)
                                .collect(),
                            context: "at module level".to_string(),
                            what,
                            ty: None,
                            reads: vec![],
                            writes: vec![],
                            risks: vec![],
                        });
                    }
                }
            }
            Item::Stmt(sid) => {
                let stmt = module.arena.get_stmt(*sid);
                process_stmt(
                    stmt,
                    &module,
                    &interner,
                    &typed,
                    &src,
                    &all_src_lines,
                    line_start,
                    line_end,
                    "at module level",
                    &mut results,
                );
            }
            Item::ExprStmt(eid) => {
                let expr_span = module.arena.get_expr(*eid).span();
                if span_overlaps(&src, expr_span, line_start, line_end) {
                    let (what, ty) = (
                        describe_expr(*eid, &module, &interner, &typed, 0),
                        typed.expr_types.get(eid).map(type_name),
                    );
                    let stmt_lo = offset_line(&src, expr_span.start as usize);
                    let stmt_hi = offset_line(&src, expr_span.end.saturating_sub(1) as usize);
                    let mut reads = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    collect_reads(*eid, &module, &interner, &typed, &mut reads, &mut seen);
                    let risks = binary_risks(*eid, &module);
                    results.push(Expl {
                        line_range: if stmt_lo == stmt_hi {
                            format!("line {stmt_lo}")
                        } else {
                            format!("lines {stmt_lo}–{stmt_hi}")
                        },
                        source_text: extract_source_text(&all_src_lines, stmt_lo, stmt_hi)
                            .chars()
                            .take(120)
                            .collect(),
                        context: "at module level".to_string(),
                        what,
                        ty,
                        reads,
                        writes: vec![],
                        risks,
                    });
                }
            }
            Item::VarDecl {
                name,
                init,
                is_const,
                span,
                ..
            } => {
                if span_overlaps(&src, *span, line_start, line_end) {
                    let n = interner.resolve(*name);
                    let kind = if *is_const { "constant" } else { "variable" };
                    let init_s = init
                        .map(|e| format!(" = {}", describe_expr(e, &module, &interner, &typed, 0)))
                        .unwrap_or_default();
                    let ty_s = init.and_then(|e| typed.expr_types.get(&e)).map(type_name);
                    let stmt_lo = offset_line(&src, span.start as usize);
                    let mut reads = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    if let Some(e) = init {
                        collect_reads(*e, &module, &interner, &typed, &mut reads, &mut seen);
                    }
                    results.push(Expl {
                        line_range: format!("line {stmt_lo}"),
                        source_text: all_src_lines
                            .get(stmt_lo.saturating_sub(1))
                            .unwrap_or(&"")
                            .chars()
                            .take(120)
                            .collect(),
                        context: "at module level".to_string(),
                        what: format!("declares module-level {kind} `{n}`{init_s}"),
                        ty: ty_s,
                        reads,
                        writes: vec![n.to_string()],
                        risks: vec![],
                    });
                }
            }
            Item::Assign {
                target,
                value,
                span,
            } => {
                if span_overlaps(&src, *span, line_start, line_end) {
                    let tgt = describe_expr(*target, &module, &interner, &typed, 0);
                    let val = describe_expr(*value, &module, &interner, &typed, 0);
                    let ty_s = typed.expr_types.get(value).map(type_name);
                    let stmt_lo = offset_line(&src, span.start as usize);
                    let mut reads = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    collect_reads(*value, &module, &interner, &typed, &mut reads, &mut seen);
                    results.push(Expl {
                        line_range: format!("line {stmt_lo}"),
                        source_text: all_src_lines
                            .get(stmt_lo.saturating_sub(1))
                            .unwrap_or(&"")
                            .chars()
                            .take(120)
                            .collect(),
                        context: "at module level".to_string(),
                        what: format!("top-level assignment: sets `{tgt}` to {val}"),
                        ty: ty_s,
                        reads,
                        writes: vec![tgt],
                        risks: vec![],
                    });
                }
            }
            Item::Destructure {
                bindings,
                value,
                span,
            } => {
                if span_overlaps(&src, *span, line_start, line_end) {
                    let names: Vec<String> = bindings
                        .iter()
                        .map(|b| interner.resolve(*b).to_string())
                        .collect();
                    let inner = describe_expr(*value, &module, &interner, &typed, 0);
                    let stmt_lo = offset_line(&src, span.start as usize);
                    let mut reads = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    collect_reads(*value, &module, &interner, &typed, &mut reads, &mut seen);
                    results.push(Expl {
                        line_range: format!("line {stmt_lo}"),
                        source_text: all_src_lines
                            .get(stmt_lo.saturating_sub(1))
                            .unwrap_or(&"")
                            .chars()
                            .take(120)
                            .collect(),
                        context: "at module level".to_string(),
                        what: format!("unpacks `({})` from {inner}", names.join(", ")),
                        ty: None,
                        reads,
                        writes: names,
                        risks: vec![],
                    });
                }
            }
            Item::Use {
                path,
                alias,
                re_export,
                grouped,
                span,
            } => {
                if span_overlaps(&src, *span, line_start, line_end) {
                    let path_s: Vec<String> = path
                        .iter()
                        .map(|s| interner.resolve(*s).to_string())
                        .collect();
                    let path_str = path_s.join(".");
                    let alias_s = alias.map(|a| format!(" as `{}`", interner.resolve(a)));
                    let import_kind = if *re_export {
                        "re-exports"
                    } else if *grouped {
                        "imports (flat)"
                    } else {
                        "imports namespace"
                    };
                    let stmt_lo = offset_line(&src, span.start as usize);
                    results.push(Expl {
                        line_range: format!("line {stmt_lo}"),
                        source_text: all_src_lines
                            .get(stmt_lo.saturating_sub(1))
                            .unwrap_or(&"")
                            .chars()
                            .take(120)
                            .collect(),
                        context: "at module level".to_string(),
                        what: format!(
                            "{import_kind} `{path_str}`{}",
                            alias_s.as_deref().unwrap_or("")
                        ),
                        ty: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
            }
            Item::ObjectDecl {
                name,
                parent,
                fields,
                methods,
                span,
            } => {
                let obj_name = interner.resolve(*name);
                if span_overlaps(&src, *span, line_start, line_end) {
                    // If the cursor is on a method, recurse into it.
                    for &mid in methods {
                        let method_item = module.arena.get_item(mid);
                        match method_item {
                            Item::ActionDecl {
                                name: mname,
                                body,
                                params,
                                span: mspan,
                                ..
                            }
                            | Item::ExtensionAction {
                                name: mname,
                                body,
                                params,
                                span: mspan,
                                ..
                            } => {
                                if span_overlaps(&src, *mspan, line_start, line_end) {
                                    let mn = interner.resolve(*mname);
                                    let ctx = format!("method `{mn}` on object `{obj_name}`");
                                    for &sid in body {
                                        let stmt = module.arena.get_stmt(sid);
                                        process_stmt(
                                            stmt,
                                            &module,
                                            &interner,
                                            &typed,
                                            &src,
                                            &all_src_lines,
                                            line_start,
                                            line_end,
                                            &ctx,
                                            &mut results,
                                        );
                                    }
                                    // Signature line with no body match → describe the method.
                                    let sig_lo = offset_line(&src, mspan.start as usize);
                                    if sig_lo >= line_start
                                        && sig_lo <= line_end
                                        && results.is_empty()
                                    {
                                        let param_list: Vec<String> = params
                                            .iter()
                                            .map(|p| {
                                                let pn = interner.resolve(p.name);
                                                format!("`{pn}`")
                                            })
                                            .collect();
                                        let what = if param_list.is_empty() {
                                            format!(
                                                "declares method `{mn}` on object `{obj_name}` with no parameters"
                                            )
                                        } else {
                                            format!(
                                                "declares method `{mn}` on object `{obj_name}` with parameters: {}",
                                                param_list.join(", ")
                                            )
                                        };
                                        results.push(Expl {
                                            line_range: format!("line {sig_lo}"),
                                            source_text: all_src_lines
                                                .get(sig_lo.saturating_sub(1))
                                                .unwrap_or(&"")
                                                .chars()
                                                .take(120)
                                                .collect(),
                                            context: format!("in object `{obj_name}`"),
                                            what,
                                            ty: None,
                                            reads: vec![],
                                            writes: vec![],
                                            risks: vec![],
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // If no methods matched, describe the object declaration itself,
                    // but only if the target line is the `object Foo {` header or a field line.
                    if results.is_empty() {
                        let obj_lo = offset_line(&src, span.start as usize);
                        let obj_hi = offset_line(&src, span.end.saturating_sub(1) as usize);
                        // Check field lines.
                        for field in fields {
                            let flo = offset_line(&src, field.span.start as usize);
                            if flo >= line_start && flo <= line_end {
                                let fn_ = interner.resolve(field.name);
                                let certain = if field.certain { "certain" } else { "optional" };
                                let init_s = field
                                    .default
                                    .map(|e| {
                                        format!(
                                            " = {}",
                                            describe_expr(e, &module, &interner, &typed, 0)
                                        )
                                    })
                                    .unwrap_or_default();
                                results.push(Expl {
                                    line_range: format!("line {flo}"),
                                    source_text: all_src_lines
                                        .get(flo.saturating_sub(1))
                                        .unwrap_or(&"")
                                        .chars()
                                        .take(120)
                                        .collect(),
                                    context: format!("field of object `{obj_name}`"),
                                    what: format!("declares {certain} field `{fn_}`{init_s}"),
                                    ty: None,
                                    reads: vec![],
                                    writes: vec![],
                                    risks: vec![],
                                });
                            }
                        }
                        // Header line.
                        if results.is_empty() && obj_lo >= line_start && obj_lo <= line_end {
                            let parent_s = parent
                                .as_ref()
                                .map(|p| {
                                    let seg: Vec<String> = p
                                        .iter()
                                        .map(|s| interner.resolve(*s).to_string())
                                        .collect();
                                    format!(" extends `{}`", seg.join("."))
                                })
                                .unwrap_or_default();
                            results.push(Expl {
                                line_range: format!("lines {obj_lo}–{obj_hi}"),
                                source_text: all_src_lines
                                    .get(obj_lo.saturating_sub(1))
                                    .unwrap_or(&"")
                                    .chars().take(120).collect(),
                                context: "at module level".to_string(),
                                what: format!(
                                    "declares object type `{obj_name}`{parent_s} with {} field{} and {} method{}",
                                    fields.len(),
                                    if fields.len() == 1 { "" } else { "s" },
                                    methods.len(),
                                    if methods.len() == 1 { "" } else { "s" },
                                ),
                                ty: None,
                                reads: vec![],
                                writes: vec![],
                                risks: vec![],
                            });
                        }
                    }
                }
            }
            Item::TestDecl {
                name: test_name,
                body,
                span,
            } => {
                if span_overlaps(&src, *span, line_start, line_end) {
                    let ctx = format!("in test `{test_name}`");
                    for &sid in body {
                        let stmt = module.arena.get_stmt(sid);
                        process_stmt(
                            stmt,
                            &module,
                            &interner,
                            &typed,
                            &src,
                            &all_src_lines,
                            line_start,
                            line_end,
                            &ctx,
                            &mut results,
                        );
                    }
                    if results.is_empty() {
                        let hdr_lo = offset_line(&src, span.start as usize);
                        let hdr_hi = offset_line(&src, span.end.saturating_sub(1) as usize);
                        if hdr_lo >= line_start && hdr_lo <= line_end {
                            results.push(Expl {
                                line_range: format!("lines {hdr_lo}–{hdr_hi}"),
                                source_text: all_src_lines
                                    .get(hdr_lo.saturating_sub(1))
                                    .unwrap_or(&"")
                                    .chars()
                                    .take(120)
                                    .collect(),
                                context: "at module level".to_string(),
                                what: format!("declares test block `{test_name}`"),
                                ty: None,
                                reads: vec![],
                                writes: vec![],
                                risks: vec![],
                            });
                        }
                    }
                }
            }
            Item::EnumDecl {
                name,
                variants,
                span,
            } => {
                let decl_lo = offset_line(&src, span.start as usize);
                if span_overlaps(&src, *span, line_start, line_end) {
                    let n = interner.resolve(*name);
                    let var_names: Vec<String> = variants
                        .iter()
                        .map(|&v| interner.resolve(v).to_string())
                        .collect();
                    results.push(Expl {
                        line_range: format!("line {decl_lo}"),
                        source_text: all_src_lines
                            .get(decl_lo.saturating_sub(1))
                            .unwrap_or(&"")
                            .chars()
                            .take(120)
                            .collect(),
                        context: "at module level".to_string(),
                        what: format!(
                            "declares enum `{}` with variants: {}",
                            n,
                            var_names.join(", ")
                        ),
                        ty: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
            }
        }
    }

    // ── Render output ──────────────────────────────────────────────────────
    let range_desc = if line_start == line_end {
        format!("line {line_start}")
    } else {
        format!("lines {line_start}–{line_end}")
    };

    if results.is_empty() {
        println!("  (no statements found on {range_desc} in `{source_name}`)");
        return Ok(());
    }

    let color = {
        use std::io::IsTerminal;
        std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
    };
    let (bold, dim, cyan, green, yellow, reset) = if color {
        (
            "\x1b[1m",
            "\x1b[2m",
            "\x1b[1;36m",
            "\x1b[1;32m",
            "\x1b[1;33m",
            "\x1b[0m",
        )
    } else {
        ("", "", "", "", "", "")
    };

    for expl in &results {
        println!();
        println!(
            "{cyan}{bold}{}{reset}  {dim}({}){reset}",
            expl.line_range, expl.context
        );
        println!("{dim}{}{reset}", expl.source_text);
        println!();
        println!("  {bold}what it does:{reset}  {}", expl.what);
        if let Some(ty) = &expl.ty {
            println!("  {bold}type:{reset}          {green}{ty}{reset}");
        }
        if !expl.reads.is_empty() {
            let reads_s: Vec<String> = expl
                .reads
                .iter()
                .map(|(name, ty)| {
                    if let Some(t) = ty {
                        format!("{name} ({t})")
                    } else {
                        name.clone()
                    }
                })
                .collect();
            println!("  {bold}reads:{reset}         {}", reads_s.join(", "));
        }
        if !expl.writes.is_empty() {
            println!(
                "  {bold}writes:{reset}        {yellow}{}{reset}",
                expl.writes.join(", ")
            );
        }
        if !expl.risks.is_empty() {
            println!("  {bold}could go wrong:{reset}");
            for r in &expl.risks {
                println!("    • {r}");
            }
        }
    }
    println!();
    Ok(())
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
            replay,
            suppress,
            sandbox,
            allow_read,
            allow_write,
            allow_env,
            allow_net,
            allow_spawn,
            time_limit,
            mem_limit,
        } => {
            let emit_kinds = parse_emit(&emit)?;
            let trace_mode = parse_trace(&trace)?;
            let replay_inputs = match replay {
                Some(ref id_or_path) => load_replay_bundle(id_or_path)?,
                None => vec![],
            };
            let sandbox_policy = if sandbox {
                let mut policy = SandboxPolicy::default();
                let mut read_all = false;
                for p in &allow_read {
                    if p == "*" {
                        policy = policy.with_allow_read_all();
                        read_all = true;
                        break;
                    }
                }
                if !read_all {
                    for p in &allow_read {
                        policy = policy.with_allow_read_prefix(p);
                    }
                }
                let mut write_all = false;
                for p in &allow_write {
                    if p == "*" {
                        policy = policy.with_allow_write_all();
                        write_all = true;
                        break;
                    }
                }
                if !write_all {
                    for p in &allow_write {
                        policy = policy.with_allow_write_prefix(p);
                    }
                }
                if allow_env {
                    policy = policy.with_allow_env();
                }
                if allow_net {
                    policy = policy.with_allow_net();
                }
                if allow_spawn {
                    policy = policy.with_allow_spawn();
                }
                if let Some(t) = time_limit {
                    policy = policy.with_time_limit(t);
                }
                if let Some(m) = mem_limit {
                    policy = policy.with_mem_limit(m);
                }
                Some(policy)
            } else {
                None
            };
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
                replay_inputs,
                suppress,
                sandbox: sandbox_policy,
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
        Command::Profile {
            file,
            profile_out,
            suppress,
        } => {
            let opts = CompileOptions {
                input: file,
                output: profile_out,
                mode: ExecutionMode::Profile,
                suppress,
                ..Default::default()
            };
            run_pipeline(opts)
        }
        Command::Test { file, suppress } => {
            let opts = CompileOptions {
                input: file,
                mode: ExecutionMode::Test,
                suppress,
                ..Default::default()
            };
            run_pipeline(opts)
        }
        Command::Check {
            file,
            max_errors,
            strict,
            suppress,
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
                suppress,
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
        Command::ExplainLine {
            file,
            line,
            end_line,
        } => run_explain_line(file, line, end_line.unwrap_or(line)),
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

fn emit_mir_safety_diags(
    mir: &fidan_mir::MirProgram,
    interner: &fidan_lexer::SymbolInterner,
    strict_mode: bool,
    suppress: &[String],
) -> usize {
    let mut errs = 0;

    // ── E0401: parallel data-race check ──────────────────────────────────────
    for diag in fidan_passes::check_parallel_races(mir, interner) {
        if !is_suppressed("E0401", suppress) {
            render_message_to_stderr(
                Severity::Error,
                fidan_diagnostics::diag_code!("E0401"),
                &format!("data race on `{}`: {}", diag.var_name, diag.context),
            );
        }
        errs += 1;
    }

    // ── W1004: unawaited Pending check ───────────────────────────────────────
    for diag in fidan_passes::check_unawaited_pending(mir, interner) {
        if is_suppressed("W1004", suppress) {
            continue;
        }
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
        if !is_suppressed("W2006", suppress) {
            render_message_to_stderr(
                sev,
                fidan_diagnostics::diag_code!("W2006"),
                &format!(
                    "in `{}`: {} — this will panic at runtime",
                    diag.fn_name, diag.context
                ),
            );
        }
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
        if is_suppressed(diag.code, suppress) {
            if strict_mode && diag.code == "W5001" {
                errs += 1;
            }
            continue;
        }
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

// ── Replay bundle helpers ─────────────────────────────────────────────────────
//
// Bundle format (plain text, one stdin line per line):
//   fidan-replay-v1\n
//   <line0>\n
//   <line1>\n
//   …
//
// Bundles are stored in ~/.fidan/replays/<id>.bundle where `id` is 8 lowercase
// hex digits derived from a hash of the source path + current Unix timestamp.

fn replay_dir() -> std::path::PathBuf {
    dirs_or_home().join(".fidan").join("replays")
}

fn dirs_or_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn save_replay_bundle(source: &std::path::Path, lines: &[String]) -> Result<String> {
    use std::io::Write;

    // Derive a content-addressed ID using FNV-1a: same script + same captured
    // inputs always produce the same bundle ID, regardless of system or time.
    // This means re-running with identical inputs overwrites the previous bundle
    // (which is fine — they are identical).
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a 64-bit offset basis
    let fnv_prime: u64 = 0x00000100000001b3;
    for byte in source.to_string_lossy().as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(fnv_prime);
    }
    for line in lines {
        for byte in line.as_bytes() {
            h ^= *byte as u64;
            h = h.wrapping_mul(fnv_prime);
        }
        h ^= b'\n' as u64;
        h = h.wrapping_mul(fnv_prime);
    }
    let id = format!("{:08x}", h & 0xffffffff);

    let dir = replay_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create replay directory {:?}", dir))?;
    let path = dir.join(format!("{id}.bundle"));
    let mut f =
        std::fs::File::create(&path).with_context(|| format!("cannot create bundle {:?}", path))?;
    writeln!(f, "fidan-replay-v1")?;
    for line in lines {
        writeln!(f, "{line}")?;
    }
    Ok(id)
}

fn load_replay_bundle(id_or_path: &str) -> Result<Vec<String>> {
    let path: std::path::PathBuf = if std::path::Path::new(id_or_path).exists() {
        std::path::PathBuf::from(id_or_path)
    } else {
        replay_dir().join(format!("{id_or_path}.bundle"))
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read replay bundle {:?}", path))?;
    let mut lines = content.lines();
    match lines.next() {
        Some("fidan-replay-v1") => {}
        _ => bail!("unrecognised replay bundle format in {:?}", path),
    }
    Ok(lines.map(str::to_string).collect())
}

fn run_pipeline(mut opts: CompileOptions) -> Result<()> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let is_stdin = opts.input.as_os_str() == "-";

    // ── Extension check (skipped for stdin) ───────────────────────────────────
    if !is_stdin
        && opts.input.extension().and_then(|e| e.to_str()) != Some("fdn")
        && !is_suppressed("W2001", &opts.suppress)
    {
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
        if !is_suppressed(diag.code.as_str(), &opts.suppress) {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
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
        if !is_suppressed(diag.code.as_str(), &opts.suppress) {
            fidan_diagnostics::render_to_stderr(diag, &source_map);
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
                        if !is_suppressed(d.code.as_str(), &opts.suppress) {
                            fidan_diagnostics::render_to_stderr(d, &source_map);
                        }
                    }
                    let (imp_module, imp_parse_diags) =
                        fidan_parser::parse(&imp_tokens, imp_file.id, Arc::clone(&interner));
                    for d in &imp_parse_diags {
                        if !is_suppressed(d.code.as_str(), &opts.suppress) {
                            fidan_diagnostics::render_to_stderr(d, &source_map);
                        }
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
                            if !is_suppressed(d.code.as_str(), &opts.suppress) {
                                fidan_diagnostics::render_to_stderr(d, &source_map);
                            }
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
            if is_suppressed(diag.code.as_str(), &opts.suppress) {
                if diag.severity == fidan_diagnostics::Severity::Error {
                    error_count += 1;
                }
                continue;
            }
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
                    error_count +=
                        emit_mir_safety_diags(&mir, &interner, opts.strict_mode, &opts.suppress);
                    if error_count == 0 {
                        // ── Optimisation passes (Phase 6) ─────────────────────
                        fidan_passes::run_all(&mut mir);
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
        ExecutionMode::Profile => {
            if error_count == 0 {
                if let Some(ref hir) = merged_hir {
                    let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                    // Safety analysis warns about potential issues — profile run
                    // continues regardless (warnings don’t block profiling).
                    emit_mir_safety_diags(&mir, &interner, false, &opts.suppress);
                    fidan_passes::run_all(&mut mir);
                    let prog_name = opts
                        .input
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?")
                        .to_string();
                    let (result, report) = fidan_interp::run_mir_with_profile(
                        mir,
                        Arc::clone(&interner),
                        Arc::clone(&source_map),
                        &prog_name,
                    );
                    if let Err(ref err) = result {
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
                                    &format!("could not write profile output: {e}"),
                                ),
                            }
                        }
                    }
                    if result.is_err() {
                        std::process::exit(1);
                    }
                }
            }
        }
        ExecutionMode::Test => {
            if error_count == 0 {
                if let Some(ref hir) = merged_hir {
                    let mut mir = fidan_mir::lower_program(hir, &interner, &[]);
                    // ── MIR safety analysis (E0401, W1004) ───────────────────
                    error_count +=
                        emit_mir_safety_diags(&mir, &interner, opts.strict_mode, &opts.suppress);
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
        emit_mir_safety_diags(&mir, &interner, false, &[]);
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
