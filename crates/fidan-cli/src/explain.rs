use crate::last_error;
use anyhow::{Context, Result, bail};
use fidan_driver::{
    AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AiAnalysisHelperCommand, AiAnalysisHelperRequest,
    AiAnalysisHelperResponse, AiAnalysisHelperResult, AiStructuredExplanation,
};
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub(crate) struct ExplainArgs {
    pub target: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub diagnostic: Option<String>,
    pub last_error: bool,
    pub ai: Option<String>,
}

fn source_line_count(file: &PathBuf) -> Result<usize> {
    let src = std::fs::read_to_string(file).with_context(|| format!("cannot read {:?}", file))?;
    Ok(src.lines().count().max(1))
}

fn parse_diagnostic_code(code: &str) -> Result<String> {
    let normalized = code.trim().to_uppercase();
    let bytes = normalized.as_bytes();
    if bytes.len() == 5
        && matches!(bytes[0], b'E' | b'W' | b'R')
        && bytes[1..].iter().all(|b| b.is_ascii_digit())
    {
        Ok(normalized)
    } else {
        bail!(
            "diagnostic code must look like E0101, W1005, or R0001; got `{}`",
            code.trim()
        )
    }
}

enum ParsedTarget {
    PlainPath(PathBuf),
    PathWithRange(PathBuf, usize, usize),
    InvalidRangeSuffix,
}

fn parse_target_with_optional_range(raw: &str) -> ParsedTarget {
    fn parse_line_number(s: &str) -> Option<usize> {
        let n = s.parse::<usize>().ok()?;
        (n > 0).then_some(n)
    }

    fn is_valid_path_prefix(path: &str) -> bool {
        if path.is_empty() || path.ends_with(':') {
            return false;
        }

        match path.find(':') {
            None => true,
            Some(1) => {
                path.as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic())
                    && path[2..].find(':').is_none()
            }
            Some(_) => false,
        }
    }

    fn parse_range(raw: &str) -> Option<(PathBuf, usize, usize)> {
        // path:start:end
        {
            let mut parts = raw.rsplitn(3, ':');
            let end_s = parts.next();
            let start_s = parts.next();
            let path = parts.next();

            if let (Some(end_s), Some(start_s), Some(path)) = (end_s, start_s, path)
                && is_valid_path_prefix(path)
                && !start_s.is_empty()
                && !end_s.is_empty()
                && !start_s.contains(':')
                && !start_s.contains('-')
                && !end_s.contains(':')
                && !end_s.contains('-')
                && let (Some(start), Some(end)) =
                    (parse_line_number(start_s), parse_line_number(end_s))
                && start <= end
            {
                return Some((PathBuf::from(path), start, end));
            }
        }

        // path:start-end
        if let Some((path, tail)) = raw.rsplit_once(':')
            && is_valid_path_prefix(path)
        {
            if let Some((start_s, end_s)) = tail.split_once('-') {
                if !start_s.is_empty()
                    && !end_s.is_empty()
                    && !start_s.contains(':')
                    && !start_s.contains('-')
                    && !end_s.contains(':')
                    && !end_s.contains('-')
                    && let (Some(start), Some(end)) =
                        (parse_line_number(start_s), parse_line_number(end_s))
                    && start <= end
                {
                    return Some((PathBuf::from(path), start, end));
                }
            } else if !tail.is_empty()
                && !tail.contains(':')
                && !tail.contains('-')
                && let Some(line) = parse_line_number(tail)
            {
                return Some((PathBuf::from(path), line, line));
            }
        }

        None
    }

    fn looks_like_range_attempt(raw: &str) -> bool {
        if !raw.contains(':') {
            return false;
        }

        let tail = match raw.rsplit_once(':') {
            Some((_, tail)) => tail,
            None => return false,
        };

        if tail.is_empty() {
            return true;
        }

        tail.chars()
            .all(|c| c.is_ascii_digit() || c == ':' || c == '-')
    }

    if let Some((file, start, end)) = parse_range(raw) {
        return ParsedTarget::PathWithRange(file, start, end);
    }

    if looks_like_range_attempt(raw) {
        return ParsedTarget::InvalidRangeSuffix;
    }

    ParsedTarget::PlainPath(PathBuf::from(raw))
}

pub(crate) fn run_explain_command(args: ExplainArgs) -> Result<()> {
    let ExplainArgs {
        target,
        line,
        end_line,
        diagnostic,
        last_error,
        ai,
    } = args;

    if diagnostic.is_some() {
        if target.is_some() || line.is_some() || end_line.is_some() || ai.is_some() || last_error {
            bail!(
                "`--diagnostic` cannot be combined with a file target, `--line`, `--end-line`, `--ai`, or `--last-error`"
            );
        }
        let code = parse_diagnostic_code(diagnostic.as_deref().unwrap())?;
        return run_explain_diagnostic(&code);
    }

    if last_error {
        if target.is_some() || line.is_some() || end_line.is_some() || ai.is_some() {
            bail!(
                "`--last-error` cannot be combined with a file target, `--line`, `--end-line`, or `--ai`"
            );
        }
        let record = last_error::load()?;
        println!(
            "last recorded diagnostic: {} — {}",
            record.code, record.message
        );
        println!();
        return run_explain_diagnostic(&record.code);
    }

    let target = target.context(
        "expected a source file target, `--diagnostic CODE`, or `--last-error`\n\nexamples:\n  fidan explain app.fdn --line 42\n  fidan explain app.fdn:42-45\n  fidan explain --diagnostic E0101\n  fidan explain --last-error",
    )?;

    let (file, line, end_line) = match parse_target_with_optional_range(&target) {
        ParsedTarget::PathWithRange(file, alias_start, alias_end) => {
            if line.is_some() || end_line.is_some() {
                bail!("`path:line-range` cannot be combined with `--line` or `--end-line`");
            }
            (file, Some(alias_start), Some(alias_end))
        }
        ParsedTarget::InvalidRangeSuffix => {
            bail!(
                "invalid line range suffix in `{}`\n\nexpected one of:\n  path:LINE\n  path:START-END\n  path:START:END",
                target
            );
        }
        ParsedTarget::PlainPath(file) => (file, line, end_line),
    };
    let total_lines = source_line_count(&file)?;
    let line_start = line.unwrap_or(1);
    let line_end = end_line.unwrap_or_else(|| {
        if line.is_some() {
            line_start
        } else {
            total_lines
        }
    });
    if let Some(prompt) = ai {
        let prompt = prompt.trim().to_string();
        return run_explain_ai(
            file,
            line_start,
            line_end,
            if prompt.is_empty() {
                None
            } else {
                Some(prompt.as_str())
            },
        );
    }
    run_explain_line(file, line_start, line_end)
}

fn run_explain_ai(
    file: PathBuf,
    line_start: usize,
    line_end: usize,
    prompt: Option<&str>,
) -> Result<()> {
    let toolchain = crate::toolchain::ensure_ai_toolchain_installed()?;

    if !toolchain.helper_path.is_file() {
        bail!(
            "installed ai-analysis helper is missing at `{}` — reinstall with `fidan toolchain add ai-analysis --version {}`",
            toolchain.helper_path.display(),
            toolchain.metadata.toolchain_version
        );
    }

    let fidan_path = std::env::current_exe()
        .context("failed to resolve the running Fidan executable for ai-analysis")?;

    let file_display = file.display().to_string();
    let request = AiAnalysisHelperRequest {
        protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        command: AiAnalysisHelperCommand::Explain {
            file,
            line_start: Some(line_start),
            line_end: Some(line_end),
            prompt: prompt.map(ToOwned::to_owned),
            fidan_path: Some(fidan_path),
        },
    };
    let request_bytes =
        serde_json::to_vec(&request).context("failed to serialize ai-analysis helper request")?;

    let mut child = Command::new(&toolchain.helper_path)
        .arg("analyze")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch ai-analysis helper `{}`",
                toolchain.helper_path.display()
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        stdin
            .write_all(&request_bytes)
            .context("failed to send ai-analysis helper request")?;
    }

    let spinner = if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(
            indicatif::ProgressStyle::with_template("  {spinner:.cyan}  {msg}")
                .expect("valid spinner template")
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(70));
        pb.set_message("Analyzing code with AI…");
        pb
    } else {
        indicatif::ProgressBar::hidden()
    };

    let output = child
        .wait_with_output()
        .context("failed while waiting for ai-analysis helper to finish")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "ai-analysis helper exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let response: AiAnalysisHelperResponse = serde_json::from_slice(&output.stdout)
        .context("failed to parse ai-analysis helper response")?;
    if response.protocol_version != AI_ANALYSIS_HELPER_PROTOCOL_VERSION {
        bail!(
            "ai-analysis helper protocol mismatch (helper={}, cli={})",
            response.protocol_version,
            AI_ANALYSIS_HELPER_PROTOCOL_VERSION
        );
    }
    if !response.success {
        bail!(
            "ai-analysis helper failed{}",
            response
                .error
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        );
    }
    let result = response
        .result
        .context("ai-analysis helper returned no result")?;
    let AiAnalysisHelperResult::Explain(explanation) = result else {
        bail!("ai-analysis helper returned an unexpected result kind for an explain request");
    };
    render_ai_explanation(&explanation, &file_display, line_start, line_end);
    Ok(())
}

// ── Rich terminal rendering ──────────────────────────────────────────────────

fn term_width() -> usize {
    crossterm::terminal::size()
        .map(|(w, _)| (w as usize).clamp(40, 120))
        .unwrap_or(80)
}

fn use_color() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// Render inline markdown-like spans (backtick code, `**bold**`, URLs) using ANSI codes.
/// Writes the rendered text into `out`.  If `color` is false, strips markers and returns plain.
fn render_inline_md(text: &str, color: bool) -> String {
    if !color {
        // Just strip any **bold** markers and return as-is.
        return text.replace("**", "");
    }

    let mut out = String::with_capacity(text.len() + 64);
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // `code` span
        if chars[i] == '`'
            && let Some(end) = chars[i + 1..].iter().position(|&c| c == '`')
        {
            let code: String = chars[i + 1..i + 1 + end].iter().collect();
            out.push_str("\x1b[33m`");
            out.push_str(&code);
            out.push_str("`\x1b[0m");
            i += end + 2;
            continue;
        }
        // **bold** span
        if i + 1 < len
            && chars[i] == '*'
            && chars[i + 1] == '*'
            && let Some(end_off) = {
                let tail = &chars[i + 2..];
                tail.windows(2).position(|w| w[0] == '*' && w[1] == '*')
            }
        {
            let bold_text: String = chars[i + 2..i + 2 + end_off].iter().collect();
            out.push_str("\x1b[1m");
            out.push_str(&bold_text);
            out.push_str("\x1b[22m");
            i += end_off + 4;
            continue;
        }
        // [text](url) link
        if chars[i] == '['
            && let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']')
        {
            let link_text: String = chars[i + 1..i + 1 + close_bracket].iter().collect();
            let after_bracket = i + 1 + close_bracket + 1;
            if after_bracket < len
                && chars[after_bracket] == '('
                && let Some(close_paren) = chars[after_bracket + 1..].iter().position(|&c| c == ')')
            {
                let url: String = chars[after_bracket + 1..after_bracket + 1 + close_paren]
                    .iter()
                    .collect();
                // OSC 8 hyperlink
                out.push_str(&format!(
                    "\x1b]8;;{url}\x1b\\\x1b[34;4m{link_text}\x1b[24;39m\x1b]8;;\x1b\\"
                ));
                i += 2 + close_bracket + 2 + close_paren + 1;
                continue;
            }
        }
        // bare https?:// URL
        if chars[i] == 'h'
            && chars[i..]
                .iter()
                .collect::<String>()
                .starts_with("https://")
            || chars[i..].iter().collect::<String>().starts_with("http://")
        {
            let rest: String = chars[i..].iter().collect();
            let url_end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\''))
                .unwrap_or(rest.len());
            let url = &rest[..url_end];
            out.push_str(&format!(
                "\x1b]8;;{url}\x1b\\\x1b[34;4m{url}\x1b[24;39m\x1b]8;;\x1b\\"
            ));
            i += url_end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Word-wrap text to `max_width`, prepending `indent` to each line.
/// ANSI escape sequences are not counted towards the width.
fn word_wrap(text: &str, indent: &str, max_width: usize) -> String {
    let visible_width = |s: &str| -> usize {
        let mut w = 0;
        let mut in_esc = false;
        for c in s.chars() {
            if in_esc {
                if c.is_ascii_alphabetic() {
                    in_esc = false;
                }
            } else if c == '\x1b' {
                in_esc = true;
            } else {
                w += 1;
            }
        }
        w
    };

    let indent_len = indent.len();
    let usable = max_width.saturating_sub(indent_len);
    let mut result = String::new();

    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            result.push('\n');
            continue;
        }
        let mut line_buf = String::new();
        let mut line_vis = 0usize;
        for word in paragraph.split_whitespace() {
            let word_vis = visible_width(word);
            if line_vis == 0 {
                line_buf.push_str(word);
                line_vis = word_vis;
            } else if line_vis + 1 + word_vis <= usable {
                line_buf.push(' ');
                line_buf.push_str(word);
                line_vis += 1 + word_vis;
            } else {
                result.push_str(indent);
                result.push_str(&line_buf);
                result.push('\n');
                line_buf = word.to_owned();
                line_vis = word_vis;
            }
        }
        if !line_buf.is_empty() {
            result.push_str(indent);
            result.push_str(&line_buf);
            result.push('\n');
        }
    }
    result
}

fn render_ai_explanation(
    explanation: &AiStructuredExplanation,
    file_display: &str,
    line_start: usize,
    line_end: usize,
) {
    let color = use_color();
    let width = if color { term_width() } else { 80 };

    if color {
        render_ai_explanation_rich(explanation, width, file_display, line_start, line_end);
    } else {
        render_ai_explanation_plain(explanation);
    }
}

fn render_ai_explanation_rich(
    explanation: &AiStructuredExplanation,
    width: usize,
    file_display: &str,
    line_start: usize,
    line_end: usize,
) {
    let provider = explanation.provider.as_deref().unwrap_or("");
    let model = explanation.model.as_deref().unwrap_or("");
    let via = match (provider.is_empty(), model.is_empty()) {
        (true, _) => String::new(),
        (false, true) => format!(" via \x1b[96m{provider}\x1b[0m"),
        (false, false) => format!(" via \x1b[96m{provider}\x1b[0m (\x1b[35m{model}\x1b[0m)"),
    };

    // ── top box ──────────────────────────────────────────────────────────────
    let title = " ◆  AI analysis ";
    // Visible widths: "╭─" (2) + title + "─" fill + via + " ─╮" (3)
    let title_vis = title.chars().count(); // visible width (chars, not bytes)
    let via_vis = match (provider.is_empty(), model.is_empty()) {
        (true, _) => 0,
        (false, true) => format!(" via {}", provider).chars().count(),
        (false, false) => format!(" via {} ({})", provider, model).chars().count(),
    };
    // inner = width - 4  (for ╭─ ... ─╮)
    let inner = width.saturating_sub(4);
    let fill_len = inner.saturating_sub(title_vis + via_vis + 1).max(1);
    let top_fill = "─".repeat(fill_len);
    println!(
        "\x1b[2m╭─\x1b[0m\x1b[1;37m{title}\x1b[0m\x1b[2m{top_fill}\x1b[0m{via}\x1b[2m ─╮\x1b[0m"
    );
    let subtitle = if line_start < line_end {
        format!(
            " {} (lines {}\u{2013}{})",
            file_display, line_start, line_end
        )
    } else {
        format!(" {} (line {})", file_display, line_start)
    };
    let subtitle_vis = subtitle.chars().count();
    let subtitle_pad = " ".repeat(width.saturating_sub(subtitle_vis + 2));
    println!("\x1b[2m│\x1b[0m\x1b[2m{subtitle}\x1b[0m{subtitle_pad}\x1b[2m│\x1b[0m");
    let bottom_bar = "─".repeat(width.saturating_sub(2));
    println!("\x1b[2m╰{bottom_bar}╯\x1b[0m");
    println!();

    let sections: &[(&str, &str)] = &[
        ("Summary", &explanation.summary),
        ("Input/output behavior", &explanation.input_output_behavior),
        ("Dependencies", &explanation.dependencies),
        ("Possible edge cases", &explanation.possible_edge_cases),
        (
            "Why a certain pattern is used",
            &explanation.why_pattern_is_used,
        ),
        ("Related symbols", &explanation.related_symbols),
        ("Underlying behaviour", &explanation.underlying_behaviour),
    ];

    let sep = "─".repeat(width.saturating_sub(2));

    for (title, body) in sections {
        if body.trim().is_empty() {
            continue;
        }
        // Section header: "  ■ Title"
        println!("  \x1b[1;36m■\x1b[0m \x1b[1;37m{title}\x1b[0m");
        println!("\x1b[2m {sep}\x1b[0m");
        let rendered_body = render_inline_md(body, true);
        let wrapped = word_wrap(&rendered_body, "  ", width);
        print!("{wrapped}");
        println!();
    }
}

fn render_ai_explanation_plain(explanation: &AiStructuredExplanation) {
    if explanation.provider.is_some() || explanation.model.is_some() {
        println!(
            "AI analysis{}{}",
            explanation
                .provider
                .as_deref()
                .map(|value| format!(" via {value}"))
                .unwrap_or_default(),
            explanation
                .model
                .as_deref()
                .map(|value| format!(" ({value})"))
                .unwrap_or_default()
        );
        println!();
    }

    fn print_section(title: &str, body: &str) {
        println!("# {title}");
        println!("{body}");
        println!();
    }

    print_section("summary", &explanation.summary);
    print_section("input/output behavior", &explanation.input_output_behavior);
    print_section("dependencies", &explanation.dependencies);
    print_section("possible edge cases", &explanation.possible_edge_cases);
    print_section(
        "why a certain pattern is used",
        &explanation.why_pattern_is_used,
    );
    print_section("related symbols", &explanation.related_symbols);
    print_section("underlying behaviour", &explanation.underlying_behaviour);
}

fn run_explain_diagnostic(code: &str) -> Result<()> {
    let entry = match fidan_diagnostics::lookup_code(code) {
        Some(e) => e,
        None => {
            bail!("unknown diagnostic code `{code}`");
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
    Ok(())
}

// ── fidan explain ───────────────────────────────────────────────────────────────────
//
// Static analysis report for one or more source lines.
// Uses the AST + typeck `expr_types` map — fully offline, zero AI.

pub(crate) fn run_explain_line(file: PathBuf, line_start: usize, line_end: usize) -> Result<()> {
    use fidan_ast::{BinOp, Expr, Item, Stmt};
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    if line_start == 0 {
        bail!("--line is 1-based; 0 is not a valid line number");
    }
    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;
    let total_lines = src.lines().count().max(1);
    let line_start = line_start.min(total_lines);
    let line_end = line_end.max(line_start).min(total_lines);
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
                    let ty_s = typed.expr_types.get(&eid).map(type_name);
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
                        BinOp::Div | BinOp::Rem if !*seen_div => {
                            out.push("division or modulo by zero".to_string());
                            *seen_div = true;
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
        fn describe_call_target(
            callee: &Expr,
            args: &[fidan_ast::Arg],
            module: &fidan_ast::Module,
            interner: &SymbolInterner,
            typed: &fidan_typeck::TypedModule,
            depth: usize,
        ) -> String {
            let arg_phrase = if args.is_empty() {
                "with no arguments".to_string()
            } else if args.len() == 1 {
                format!(
                    "passing {}",
                    describe_expr(args[0].value, module, interner, typed, depth + 1)
                )
            } else {
                format!("with {} arguments", args.len())
            };

            match callee {
                Expr::Parent { .. } => format!("calls the parent constructor, {arg_phrase}"),
                Expr::Ident { name, .. } => {
                    let name_s = interner.resolve(*name);
                    if args.is_empty() {
                        format!("calls `{name_s}`")
                    } else {
                        format!("calls `{name_s}`, {arg_phrase}")
                    }
                }
                Expr::Field { object, field, .. } => {
                    let obj = describe_expr(*object, module, interner, typed, depth + 1);
                    let field_s = interner.resolve(*field);
                    if args.is_empty() {
                        format!("calls method `{field_s}` on {obj}")
                    } else {
                        format!("calls method `{field_s}` on {obj}, {arg_phrase}")
                    }
                }
                _ => {
                    let callee_s = describe_expr_by_ref(callee, module, interner, typed, depth + 1);
                    if args.is_empty() {
                        format!("calls {callee_s}")
                    } else {
                        format!("calls {callee_s}, {arg_phrase}")
                    }
                }
            }
        }

        fn describe_expr_by_ref(
            expr: &Expr,
            module: &fidan_ast::Module,
            interner: &SymbolInterner,
            typed: &fidan_typeck::TypedModule,
            depth: usize,
        ) -> String {
            match expr {
                Expr::IntLit { value, .. } => format!("integer literal `{value}`"),
                Expr::FloatLit { value, .. } => format!("float literal `{value}`"),
                Expr::StrLit { value, .. } => format!("string literal `\"{value}\"`"),
                Expr::BoolLit { value, .. } => format!("boolean `{value}`"),
                Expr::Nothing { .. } => "`nothing`".to_string(),
                Expr::Ident { name, .. } => {
                    let s = interner.resolve(*name);
                    format!("`{s}`")
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
                    let callee_expr = module.arena.get_expr(*callee);
                    describe_call_target(callee_expr, args, module, interner, typed, depth)
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
                Expr::StringInterp { .. } => {
                    "builds a string using embedded expressions".to_string()
                }
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
                Expr::This { .. } => "the current object (`this`)".to_string(),
                Expr::Parent { .. } => "the parent constructor (`parent`)".to_string(),
                _ => "(expression)".to_string(),
            }
        }

        let expr = module.arena.get_expr(eid);
        describe_expr_by_ref(expr, module, interner, typed, depth)
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
            Stmt::ActionDecl { name, params, .. } => (
                format!(
                    "declares nested action `{}` with {} parameter{}",
                    interner.resolve(*name),
                    params.len(),
                    if params.len() == 1 { "" } else { "s" }
                ),
                Some("action".to_string()),
            ),
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

    struct StmtExplainCtx<'a> {
        module: &'a fidan_ast::Module,
        interner: &'a SymbolInterner,
        typed: &'a fidan_typeck::TypedModule,
        src: &'a str,
        all_src_lines: &'a [&'a str],
        line_start: usize,
        line_end: usize,
    }

    fn process_stmt(stmt: &Stmt, context: &str, ctx: &StmtExplainCtx<'_>, results: &mut Vec<Expl>) {
        let span = match stmt {
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
        };

        if !span_overlaps(ctx.src, span, ctx.line_start, ctx.line_end) {
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
                | Stmt::While { body, .. }
                | Stmt::ActionDecl { body, .. } => body.clone(),
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
                let child = ctx.module.arena.get_stmt(sid);
                process_stmt(child, context, ctx, results);
            }
            return;
        }

        let stmt_lo = offset_line(ctx.src, span.start as usize);
        let stmt_hi = offset_line(ctx.src, span.end.saturating_sub(1) as usize);
        let source_text = extract_source_text(ctx.all_src_lines, stmt_lo, stmt_hi);
        let (what, ty) = describe_stmt(stmt, ctx.module, ctx.interner, ctx.typed);

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
            collect_reads(
                *eid,
                ctx.module,
                ctx.interner,
                ctx.typed,
                &mut reads,
                &mut seen_reads,
            );
        }

        // Remove from reads any names that are also writes (they're declared here).
        let writes = collect_writes(stmt, ctx.module, ctx.interner);
        reads.retain(|(name, _)| !writes.contains(name));

        // Collect risks from expression operators.
        let risks: Vec<String> = expr_ids_in_stmt
            .iter()
            .flat_map(|&eid| binary_risks(eid, ctx.module))
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
    let stmt_ctx = StmtExplainCtx {
        module: &module,
        interner: &interner,
        typed: &typed,
        src: &src,
        all_src_lines: &all_src_lines,
        line_start,
        line_end,
    };

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
                        process_stmt(stmt, &ctx, &stmt_ctx, &mut results);
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
                process_stmt(stmt, "at module level", &stmt_ctx, &mut results);
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
                            } if span_overlaps(&src, *mspan, line_start, line_end) => {
                                let mn = interner.resolve(*mname);
                                let ctx = format!("method `{mn}` on object `{obj_name}`");
                                for &sid in body {
                                    let stmt = module.arena.get_stmt(sid);
                                    process_stmt(stmt, &ctx, &stmt_ctx, &mut results);
                                }
                                // Signature line with no body match → describe the method.
                                let sig_lo = offset_line(&src, mspan.start as usize);
                                if sig_lo >= line_start && sig_lo <= line_end && results.is_empty()
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
                        process_stmt(stmt, &ctx, &stmt_ctx, &mut results);
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
                        .map(|v| interner.resolve(v.name).to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_driver::AiStructuredExplanation;

    fn smoke_explanation() -> AiStructuredExplanation {
        AiStructuredExplanation {
            provider: Some("openai-compatible".to_string()),
            model: Some("docker.io/ai/gemma3:latest".to_string()),
            summary: "The code defines an action named `middle` that takes an integer as input \
                and calls the `inner` action, constructing a string with interpolation using the \
                input integer. The `inner` action builds interpolated strings and can potentially \
                panic."
                .to_string(),
            input_output_behavior: "The `middle` action receives an integer as input. It then \
                calls the `inner` action, passing a string built by interpolating the input \
                integer into the string 'iteration {count}'. The `inner` action's output is not \
                explicitly defined in the provided context, but it is known to build interpolated \
                strings and potentially panic."
                .to_string(),
            dependencies: "The `middle` action depends on the `inner` action.".to_string(),
            possible_edge_cases: "The `inner` action could panic if the interpolation fails or \
                if the input integer is invalid. The exact behavior of `inner` is not fully \
                defined in the provided context, so potential panics or other errors within \
                `inner` are possible."
                .to_string(),
            why_pattern_is_used: "The `with` pattern is used to define the input parameter of \
                the `middle` action, specifying that it expects an integer. The `inner` action \
                uses the interpolation pattern (`iteration {count}`) to construct a string \
                dynamically based on the input value, which is a common Fidan pattern for \
                building dynamic strings."
                .to_string(),
            related_symbols: "The `inner` action is a related symbol, as `middle` calls it. \
                `inner` is defined at line 1 in the same file and accepts a **string** argument \
                via `msg`. See also: https://example.com/fidan-docs/actions"
                .to_string(),
            underlying_behaviour: "The `middle` action's behaviour is determined by the call to \
                `inner`. The `inner` action builds a string using interpolation, which could lead \
                to a panic if the interpolation fails. The call chain is: `outer` → `middle(42)` \
                → `inner(\"iteration 42\")` → `panic(\"something went wrong: iteration 42\")`."
                .to_string(),
        }
    }

    /// Run with `cargo test -p fidan-cli render_rich -- --nocapture` to see the output.
    #[test]
    fn render_rich_smoke() {
        let explanation = smoke_explanation();
        println!(); // blank line before output in test runner
        render_ai_explanation_rich(&explanation, 90, "example.rs", 1, 10);
    }

    /// Run with `cargo test -p fidan-cli render_plain -- --nocapture` to see plain fallback.
    #[test]
    fn render_plain_smoke() {
        let explanation = smoke_explanation();
        println!();
        render_ai_explanation_plain(&explanation);
    }

    #[test]
    fn inline_md_code_spans() {
        let out = render_inline_md("use `middle` and `inner` together", true);
        assert!(out.contains("\x1b[33m`middle`\x1b[0m"));
        assert!(out.contains("\x1b[33m`inner`\x1b[0m"));
    }

    #[test]
    fn inline_md_bold() {
        let out = render_inline_md("a **strong** word", true);
        assert!(out.contains("\x1b[1mstrong\x1b[22m"));
    }

    #[test]
    fn inline_md_no_color_strips_markers() {
        let out = render_inline_md("use `code` and **bold** here", false);
        assert!(!out.contains('\x1b'));
        assert!(out.contains("code"));
        assert!(out.contains("bold"));
        // backticks are kept in plain mode (no terminal, no highlight)
        assert!(!out.contains('*'));
    }

    #[test]
    fn word_wrap_respects_width() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let wrapped = word_wrap(text, "  ", 30);
        for line in wrapped.lines() {
            assert!(line.len() <= 30, "line too long: {line:?}");
        }
    }
}
