use crate::pipeline::{emit_mir_safety_diags, render_trace_to_stderr};
use anyhow::Result;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::TraceMode;

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
    error_history: &mut Vec<String>,
) -> bool {
    use fidan_diagnostics::render_to_stderr;
    use fidan_lexer::Lexer;
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let ps = Arc::new(SourceMap::new());
    let pf = ps.add_file("<repl>", plain_src);

    let (pt, ld) = Lexer::new(&pf, Arc::clone(interner)).tokenise();
    if !ld.is_empty() {
        for d in &ld {
            render_to_stderr(d, &ps);
            error_history.push(format!("[{}]: {}", d.code, d.message));
        }
        return true;
    }

    let (pm, pd) = fidan_parser::parse(&pt, pf.id, Arc::clone(interner));
    if !pd.is_empty() {
        for d in &pd {
            render_to_stderr(d, &ps);
            error_history.push(format!("[{}]: {}", d.code, d.message));
        }
        return true;
    }

    let mut tc = fidan_typeck::TypeChecker::new(Arc::clone(interner), boot_fid);
    tc.set_repl(true);
    tc.check_module(&pm);
    let ty = tc.finish_typed();
    if !ty.diagnostics.is_empty() {
        for d in &ty.diagnostics {
            render_to_stderr(d, &ps);
            error_history.push(format!("[{}]: {}", d.code, d.message));
        }
        return true;
    }
    false
}

pub(crate) fn run_repl(trace_mode: TraceMode) -> Result<()> {
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
        if !lex_diags.is_empty() {
            let used_plain = echo_sym_opt.is_some()
                && render_plain_diagnostics(&plain_source, &interner, boot_fid, &mut error_history);
            if !used_plain {
                for d in &lex_diags {
                    fidan_diagnostics::render_to_stderr(d, &exec_smap);
                    error_history.push(format!("[{}]: {}", d.code, d.message));
                }
            }
            continue;
        }

        let (full_module, parse_diags) =
            fidan_parser::parse(&exec_toks, exec_file.id, Arc::clone(&interner));
        if !parse_diags.is_empty() {
            let used_plain = echo_sym_opt.is_some()
                && render_plain_diagnostics(&plain_source, &interner, boot_fid, &mut error_history);
            if !used_plain {
                for d in &parse_diags {
                    fidan_diagnostics::render_to_stderr(d, &exec_smap);
                    error_history.push(format!("[{}]: {}", d.code, d.message));
                }
            }
            continue;
        }

        // ── Fresh type-check (consumes exec_tc to produce TypedModule) ──────
        let mut exec_tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), boot_fid);
        exec_tc.set_repl(true);
        exec_tc.check_module(&full_module);
        let typed = exec_tc.finish_typed();
        if !typed.diagnostics.is_empty() {
            let used_plain = echo_sym_opt.is_some()
                && render_plain_diagnostics(&plain_source, &interner, boot_fid, &mut error_history);
            if !used_plain {
                for d in &typed.diagnostics {
                    fidan_diagnostics::render_to_stderr(d, &exec_smap);
                    error_history.push(format!("[{}]: {}", d.code, d.message));
                }
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
