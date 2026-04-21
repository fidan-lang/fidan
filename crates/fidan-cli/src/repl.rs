use crate::last_error;
use crate::pipeline::{DiagnosticBudget, emit_mir_safety_diags, render_trace_to_stderr};
use anyhow::Result;
use crossterm::{cursor, execute, terminal};
use fidan_ast::{Item, Module, Stmt};
use fidan_diagnostics::{Diagnostic, Severity, render_message_to_stderr};
use fidan_driver::TraceMode;
use fidan_source::Span;

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
        if crate::terminal::stdout_supports_color() {
            std::borrow::Cow::Owned(format!("\x1b[1;36m{prompt}\x1b[0m"))
        } else {
            std::borrow::Cow::Borrowed(prompt)
        }
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

fn normalize_max_errors_per_input(max_errors_per_input: usize) -> Option<usize> {
    if max_errors_per_input == 0 {
        None
    } else {
        Some(max_errors_per_input)
    }
}

fn stmt_span(stmt: &Stmt) -> Span {
    match stmt {
        Stmt::VarDecl { span, .. }
        | Stmt::Destructure { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Expr { span, .. }
        | Stmt::ActionDecl { span, .. }
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
    }
}

fn item_span(module: &Module, item: &Item) -> Span {
    match item {
        Item::VarDecl { span, .. }
        | Item::Assign { span, .. }
        | Item::Destructure { span, .. }
        | Item::ObjectDecl { span, .. }
        | Item::ActionDecl { span, .. }
        | Item::ExtensionAction { span, .. }
        | Item::Use { span, .. }
        | Item::TestDecl { span, .. }
        | Item::EnumDecl { span, .. } => *span,
        Item::ExprStmt(expr_id) => module.arena.get_expr(*expr_id).span(),
        Item::Stmt(stmt_id) => stmt_span(module.arena.get_stmt(*stmt_id)),
    }
}

struct ReplChunkRewrite {
    chunk: String,
    echo_last_expr: bool,
    transformed: bool,
}

fn rewrite_repl_chunk(
    module: &Module,
    input: &str,
    synthetic_exec_counter: &mut u32,
) -> ReplChunkRewrite {
    let mut chunk = String::new();
    let mut transformed = false;
    let last_index = module.items.len().saturating_sub(1);
    let mut echo_last_expr = false;

    for (index, item_id) in module.items.iter().enumerate() {
        let item = module.arena.get_item(*item_id);
        let span = item_span(module, item);
        let raw = &input[span.start as usize..span.end as usize];

        match item {
            // Keep declarations at top-level so globals/imports persist across REPL inputs.
            // Wrap control-flow statements into synthetic actions so the REPL does not need
            // to delta-execute through old top-level control-flow blocks.
            Item::Stmt(_) => {
                transformed = true;
                *synthetic_exec_counter += 1;
                let action_name = format!("__repl_exec_{}__", *synthetic_exec_counter);
                chunk.push_str("action ");
                chunk.push_str(&action_name);
                chunk.push_str(" {\n");
                chunk.push_str(raw.trim());
                chunk.push_str("\n}\n");
                chunk.push_str(&action_name);
                chunk.push_str("()\n");
            }
            // Preserve auto-echo for a trailing bare expression.
            Item::ExprStmt(_) if index == last_index => {
                echo_last_expr = true;
                chunk.push_str("var __repl_echo__ set ");
                chunk.push_str(raw.trim());
                chunk.push('\n');
            }
            _ => {
                chunk.push_str(raw.trim());
                chunk.push('\n');
            }
        }
    }

    ReplChunkRewrite {
        chunk,
        echo_last_expr,
        transformed,
    }
}

fn render_repl_diagnostics(
    diags: &[Diagnostic],
    source_map: &std::sync::Arc<fidan_source::SourceMap>,
    budget: &mut DiagnosticBudget,
    error_history: Option<&mut Vec<String>>,
) -> bool {
    let mut rendered_any = false;
    let mut error_history = error_history;

    for diag in diags {
        if budget.would_block_further_errors() {
            break;
        }
        budget.render_diag(diag, source_map, &[]);
        rendered_any = true;
        last_error::record(diag.code.as_str(), &diag.message);
        if let Some(history) = error_history.as_deref_mut() {
            history.push(format!("[{}]: {}", diag.code, diag.message));
        }
    }

    rendered_any
}

// ── REPL ─────────────────────────────────────────────────────────────────────────────

/// Interactive lex + parse + typecheck + interpret loop.
///
/// Each line is treated as a self-contained Fidan snippet.  The persistent
/// [`TypeChecker`] accumulates symbol definitions across lines so names defined
/// on line N are visible on line N+1.  The interpreter runs after every clean
/// type-check so side effects (print, etc.) are visible immediately.
/// Re-run the full lex → parse → typecheck pipeline on `plain_src` and
/// render any resulting diagnostics to stderr, adding each to `error_history`.
/// Returns `true` if at least one diagnostic was emitted.
///
/// Used by the REPL to produce user-visible errors that don't mention the
/// internal `__repl_echo__` wrapper variable.
fn render_plain_diagnostics(
    plain_src: &str,
    interner: &std::sync::Arc<fidan_lexer::SymbolInterner>,
    boot_fid: fidan_source::FileId,
    max_errors_per_input: Option<usize>,
    error_history: &mut Vec<String>,
) -> bool {
    use fidan_lexer::Lexer;
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let ps = Arc::new(SourceMap::new());
    let pf = ps.add_file("<repl>", plain_src);
    let mut budget = DiagnosticBudget::new(max_errors_per_input);

    let (pt, ld) = Lexer::new(&pf, Arc::clone(interner)).tokenise();
    if !ld.is_empty() {
        return render_repl_diagnostics(&ld, &ps, &mut budget, Some(error_history));
    }

    let (pm, pd) = fidan_parser::parse(&pt, pf.id, Arc::clone(interner));
    if !pd.is_empty() {
        return render_repl_diagnostics(&pd, &ps, &mut budget, Some(error_history));
    }

    let mut tc = fidan_typeck::TypeChecker::new(Arc::clone(interner), boot_fid);
    tc.set_repl(true);
    tc.check_module(&pm);
    let ty = tc.finish_typed();
    if !ty.diagnostics.is_empty() {
        return render_repl_diagnostics(&ty.diagnostics, &ps, &mut budget, Some(error_history));
    }
    false
}

pub(crate) fn run_repl(trace_mode: TraceMode, max_errors_per_input: usize) -> Result<()> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use rustyline::error::ReadlineError;
    use rustyline::{At, Cmd, KeyCode, KeyEvent, Modifiers, Movement, Word};
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
    rl.bind_sequence(
        KeyEvent(KeyCode::Delete, Modifiers::CTRL),
        Cmd::Kill(Movement::ForwardWord(1, At::AfterEnd, Word::Emacs)),
    );
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
    let max_errors_per_input = normalize_max_errors_per_input(max_errors_per_input);
    let mut synthetic_exec_counter: u32 = 0;

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
        if open_braces == 0
            && let Some(cmd) = trimmed.strip_prefix(':')
        {
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
                    let _ = execute!(
                        std::io::stdout(),
                        terminal::Clear(terminal::ClearType::All),
                        cursor::MoveTo(0, 0)
                    );
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
                    let mut diag_budget = DiagnosticBudget::new(max_errors_per_input);
                    let (toks, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
                    if render_repl_diagnostics(&lex_diags, &smap, &mut diag_budget, None) {
                        continue;
                    }
                    let (m, ast_diags) = fidan_parser::parse(&toks, f.id, Arc::clone(&interner));
                    if render_repl_diagnostics(&ast_diags, &smap, &mut diag_budget, None) {
                        continue;
                    }
                    println!("  items : {}", m.items.len());
                    println!("  exprs : {}", m.arena.exprs.len());
                    println!("  stmts : {}", m.arena.stmts.len());
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
                    let mut diag_budget = DiagnosticBudget::new(max_errors_per_input);
                    let (toks, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
                    if render_repl_diagnostics(&lex_diags, &smap, &mut diag_budget, None) {
                        continue;
                    }
                    let (m, parse_diags) = fidan_parser::parse(&toks, f.id, Arc::clone(&interner));
                    if render_repl_diagnostics(&parse_diags, &smap, &mut diag_budget, None) {
                        continue;
                    }
                    match tc.infer_snippet_type(&m) {
                        Some(ty_name) => println!("  : {ty_name}"),
                        None => eprintln!("  (snippet has no bare expression to infer)"),
                    }
                    let _ = tc.drain_diags(); // discard type errors — :type is query-only
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

        // Plain source (without any echo-wrap) kept for user-visible error
        // messages; spans in diagnostics from the wrapped candidate source
        // would otherwise expose the internal `__repl_echo__` variable.
        let plain_source = {
            let mut s = mir_repl_state.accumulated_source.clone();
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&complete_input);
            s.push('\n');
            s
        };

        // ── Auto-echo: mini-parse just the new input to see if its last item
        //   is a bare expression.  If so we wrap it as
        //   `var __repl_echo__ set <expr>` so the value is preserved in a
        //   global after execution and can be displayed without calling print().
        let echo_sym = interner.intern("__repl_echo__");
        let (echo_sym_opt, candidate_source, transformed_input) = {
            let mini_smap = Arc::new(SourceMap::new());
            let mini_file = mini_smap.add_file("<echo-check>", complete_input.as_str());
            let (mini_toks, _) = Lexer::new(&mini_file, Arc::clone(&interner)).tokenise();
            let (mini_mod, _) =
                fidan_parser::parse(&mini_toks, mini_file.id, Arc::clone(&interner));
            let rewritten =
                rewrite_repl_chunk(&mini_mod, &complete_input, &mut synthetic_exec_counter);
            let mut s = mir_repl_state.accumulated_source.clone();
            if !s.is_empty() && !rewritten.chunk.is_empty() {
                s.push('\n');
            }
            s.push_str(&rewritten.chunk);
            (
                rewritten.echo_last_expr.then_some(echo_sym),
                s,
                rewritten.transformed,
            )
        };

        // ── Lex + parse the full candidate source ──────────────────────────
        let exec_smap = Arc::new(SourceMap::new());
        let exec_file = exec_smap.add_file("<repl>", &*candidate_source);
        let mut diag_budget = DiagnosticBudget::new(max_errors_per_input);
        let (exec_toks, lex_diags) = Lexer::new(&exec_file, Arc::clone(&interner)).tokenise();
        if !lex_diags.is_empty() {
            let used_plain = (echo_sym_opt.is_some() || transformed_input)
                && render_plain_diagnostics(
                    &plain_source,
                    &interner,
                    boot_fid,
                    max_errors_per_input,
                    &mut error_history,
                );
            if !used_plain {
                let _ = render_repl_diagnostics(
                    &lex_diags,
                    &exec_smap,
                    &mut diag_budget,
                    Some(&mut error_history),
                );
            }
            continue;
        }

        let (full_module, parse_diags) =
            fidan_parser::parse(&exec_toks, exec_file.id, Arc::clone(&interner));
        if !parse_diags.is_empty() {
            let used_plain = (echo_sym_opt.is_some() || transformed_input)
                && render_plain_diagnostics(
                    &plain_source,
                    &interner,
                    boot_fid,
                    max_errors_per_input,
                    &mut error_history,
                );
            if !used_plain {
                let _ = render_repl_diagnostics(
                    &parse_diags,
                    &exec_smap,
                    &mut diag_budget,
                    Some(&mut error_history),
                );
            }
            continue;
        }

        // ── Fresh type-check (consumes exec_tc to produce TypedModule) ──────
        let mut exec_tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
        exec_tc.set_repl(true);
        exec_tc.check_module(&full_module);
        let typed = exec_tc.finish_typed();
        if !typed.diagnostics.is_empty() {
            let used_plain = (echo_sym_opt.is_some() || transformed_input)
                && render_plain_diagnostics(
                    &plain_source,
                    &interner,
                    boot_fid,
                    max_errors_per_input,
                    &mut error_history,
                );
            if !used_plain {
                let _ = render_repl_diagnostics(
                    &typed.diagnostics,
                    &exec_smap,
                    &mut diag_budget,
                    Some(&mut error_history),
                );
            }
            continue;
        }

        // ── HIR → MIR → optimisation passes ───────────────────────────────
        let hir = fidan_hir::lower_module(&full_module, &typed, &interner);
        let mut mir =
            fidan_mir::lower_program(&hir, &interner, &mir_repl_state.persistent_global_names);
        // Run MIR safety diagnostics (W2006 null-safety, W1004 unawaited, etc.).
        emit_mir_safety_diags(&mir, &interner, false, &[], &mut diag_budget);
        if diag_budget.error_count() > 0 {
            continue;
        }
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
                last_error::record(e.code, &e.message);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_max_errors_per_input_zero_is_unlimited() {
        assert_eq!(normalize_max_errors_per_input(0), None);
    }

    #[test]
    fn normalize_max_errors_per_input_positive_is_preserved() {
        assert_eq!(normalize_max_errors_per_input(1), Some(1));
        assert_eq!(normalize_max_errors_per_input(3), Some(3));
    }
}
