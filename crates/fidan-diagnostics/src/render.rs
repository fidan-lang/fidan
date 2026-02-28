use crate::{Diagnostic, Severity};
use fidan_source::SourceMap;

// ─────────────────────────────────────────────────────────────────────────────
// Fidan Diagnostic Renderer
//
// Span-anchored format:
//
//   error[E0101]: undefined name `greting`
//     --> test.fdn:2:7
//      |
//    1 | var greeting = "Hello"
//    2 | print(greting)
//      |       ^^^^^^^ unknown name
//      |
//   help: did you mean `greeting`?
//      |
//    2 | print(greeting)
//      |       +++++++
//      |
//
// Cause-chain (one level per cause, labelled):
//
//   caused by (1/2):
//     error[E0201]: …
//       --> …
//       …
//
// Spanless pipeline badge:
//
//    ◆  note  unimplemented  interpreter not yet implemented (Phase 5)
// ─────────────────────────────────────────────────────────────────────────────

// ── helpers ──────────────────────────────────────────────────────────────────

/// Convert a byte offset into a 1-based `(line, column)` pair.
fn byte_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(src.len());
    let before = &src[..clamped];
    let line = before.chars().filter(|c| *c == '\n').count() + 1;
    let col = before.rfind('\n').map_or(clamped, |n| clamped - n - 1) + 1;
    (line, col)
}

fn is_color_enabled() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

// ── spanless badge renderer ───────────────────────────────────────────────────

/// Render a spanless pipeline-level message to stderr.
///
/// Used for phase stubs, extension warnings, LSP stubs, etc. — messages that
/// have no source location and use the badge layout:
/// ` ◆  note  code  message`
pub fn render_message_to_stderr(severity: Severity, code: &str, message: &str) {
    if is_color_enabled() {
        let (sym, sev_color) = match severity {
            Severity::Error => ("✖", "\x1b[1;31m"),
            Severity::Warning => ("⚠", "\x1b[1;33m"),
            Severity::Note => ("◆", "\x1b[1;36m"),
        };
        let sev_str = severity.to_string();
        let reset = "\x1b[0m";
        let dim = "\x1b[2m";
        if code.is_empty() {
            eprintln!(" {sev_color}{sym}  {sev_str}{reset}  {message}");
        } else {
            eprintln!(" {sev_color}{sym}  {sev_str}{reset}  {dim}{code}{reset}  {message}");
        }
    } else {
        let sev_str = severity.to_string();
        if code.is_empty() {
            eprintln!("{sev_str}  {message}");
        } else {
            eprintln!("{sev_str}  {code}  {message}");
        }
    }
}

// ── span-anchored renderer ────────────────────────────────────────────────────

/// Render a span-anchored diagnostic to stderr.
pub fn render_to_stderr(diag: &Diagnostic, source_map: &SourceMap) {
    render_one(diag, source_map, 0);
}

fn render_one(diag: &Diagnostic, source_map: &SourceMap, depth: usize) {
    let file = source_map.get(diag.span.file);
    let name: &str = &file.name;
    let src: &str = &file.src;

    let (line, col) = byte_to_line_col(src, diag.span.start as usize);
    let span_len = (diag.span.end as usize)
        .saturating_sub(diag.span.start as usize)
        .max(1);

    // Indentation for cause-chain nesting.
    let dp = "  ".repeat(depth);

    let color = is_color_enabled();
    let (hdr_c, ctx_c, plus_c, reset, bold, dim) = if color {
        let h = match diag.severity {
            Severity::Error => "\x1b[1;31m",   // bold red
            Severity::Warning => "\x1b[1;33m", // bold yellow
            Severity::Note => "\x1b[1;36m",    // bold cyan
        };
        (h, "\x1b[2m", "\x1b[1;32m", "\x1b[0m", "\x1b[1m", "\x1b[2m")
    } else {
        ("", "", "", "", "", "")
    };

    // ── Header: error[E0101]: message ────────────────────────────────────────
    let kind_label = match diag.severity {
        Severity::Error if !diag.code.is_empty() => format!("error[{}]", diag.code),
        Severity::Warning if !diag.code.is_empty() => format!("warning[{}]", diag.code),
        Severity::Error => "error".to_string(),
        Severity::Warning => "warning".to_string(),
        Severity::Note => "note".to_string(),
    };
    eprintln!("{dp}{hdr_c}{bold}{kind_label}{reset}: {}", diag.message);

    // ── Location ─────────────────────────────────────────────────────────────
    eprintln!("{dp}  {dim}-->{reset} {name}:{line}:{col}");

    // ── Source snippet with context window ───────────────────────────────────
    let all_lines: Vec<&str> = src.lines().collect();
    let total = all_lines.len();

    if line > 0 && line <= total {
        // Show 1 line before and 1 line after the error line (if they exist).
        let ctx_start = if line > 1 { line - 1 } else { line };
        let ctx_end = (line + 1).min(total);

        // Gutter width = digits in the largest line number shown.
        let gutter_w = ctx_end.to_string().len();
        let g = " ".repeat(gutter_w); // blank gutter for separator lines

        // Optional inline label after the underline carets.
        let label_msg: Option<&str> = diag
            .labels
            .first()
            .filter(|l| !l.message.is_empty())
            .map(|l| l.message.as_str());

        eprintln!("{dp}  {g} |");
        for ln in ctx_start..=ctx_end {
            if ln == 0 || ln > total {
                continue;
            }
            let src_line = all_lines[ln - 1];
            let ln_s = format!("{:>width$}", ln, width = gutter_w);

            if ln == line {
                // Primary error line — full brightness.
                eprintln!("{dp}  {ln_s} | {src_line}");

                // Underline: ^ for errors, ~ for warnings/notes.
                let caret = if diag.severity == Severity::Error {
                    '^'
                } else {
                    '~'
                };
                let uline = format!(
                    "{}{}",
                    " ".repeat(col.saturating_sub(1)),
                    caret.to_string().repeat(span_len),
                );

                if let Some(lmsg) = label_msg {
                    eprintln!("{dp}  {g} | {hdr_c}{uline}  {lmsg}{reset}");
                } else {
                    eprintln!("{dp}  {g} | {hdr_c}{uline}{reset}");
                }
            } else {
                // Context line — dimmed.
                eprintln!("{dp}  {ctx_c}{ln_s} | {src_line}{reset}");
            }
        }
        eprintln!("{dp}  {g} |");
    }

    // ── Notes ─────────────────────────────────────────────────────────────────
    for note in &diag.notes {
        eprintln!("{dp}  {dim}note:{reset} {note}");
    }

    // ── Help + fix-it patch ───────────────────────────────────────────────────
    //
    // When a suggestion carries a `SourceEdit`, we show the patched line with
    // `++++` characters highlighting exactly what will be inserted/replaced:
    //
    //   help: did you mean `greeting`?
    //      |
    //    2 | print(greeting)
    //      |       +++++++
    //      |
    for sug in &diag.suggestions {
        eprintln!("{dp}  {dim}help:{reset} {}", sug.message);

        if let Some(edit) = &sug.edit {
            let (edit_ln, edit_col) = byte_to_line_col(src, edit.span.start as usize);
            let edit_raw_len = (edit.span.end as usize).saturating_sub(edit.span.start as usize);

            if edit_ln > 0 && edit_ln <= all_lines.len() {
                let src_line = all_lines[edit_ln - 1];
                let col0 = edit_col.saturating_sub(1); // 0-based column
                let col0c = col0.min(src_line.len()); // clamped
                let end0c = (col0 + edit_raw_len).min(src_line.len());

                let patched = format!(
                    "{}{}{}",
                    &src_line[..col0c],
                    &edit.replacement,
                    &src_line[end0c..],
                );

                let gw = edit_ln.to_string().len();
                let gp = " ".repeat(gw);
                let ln_s = format!("{:>width$}", edit_ln, width = gw);
                let plus = format!(
                    "{}{}",
                    " ".repeat(col0c),
                    "+".repeat(edit.replacement.len().max(1)),
                );

                eprintln!("{dp}  {gp} |");
                eprintln!("{dp}  {ln_s} | {patched}");
                eprintln!("{dp}  {gp} | {plus_c}{plus}{reset}");
                eprintln!("{dp}  {gp} |");
            }
        }
    }

    // ── Cause chain ───────────────────────────────────────────────────────────
    //
    // Each cause is labelled with its position and rendered one indent level
    // deeper — giving a "traceback" feel where each hop in the error path is
    // visible with its own span and evidence.
    if !diag.cause_chain.is_empty() {
        eprintln!("{dp}");
        for (i, cause) in diag.cause_chain.iter().enumerate() {
            let n = i + 1;
            let total_c = diag.cause_chain.len();
            eprintln!("{dp}  {dim}caused by ({n}/{total_c}):{reset}");
            render_one(cause, source_map, depth + 1);
        }
    }
}
