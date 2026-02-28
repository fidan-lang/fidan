use crate::{Diagnostic, Severity};
use fidan_source::SourceMap;

// ─────────────────────────────────────────────────────────────────────────────
// Fidan Diagnostics renderer
//
// Error format (colour terminal):
//
//   FidanError: undefined name 'greting'
//     --> test.fdn:2:7
//
//     2 │ print(greting)
//       │       ~~~~~~~
//
//     note: did you mean 'greeting'?
//
// Cause-chain (Python traceback style, each level indented):
//
//   FidanError: type mismatch
//     --> test.fdn:5:3
//     ...
//
//     caused by:
//       FidanError: undefined name 'bar'
//         --> test.fdn:2:7
//         ...
//
// Spanless pipeline messages:
//
//    ◆  note  unimplemented  interpreter not yet implemented (Phase 5)
// ─────────────────────────────────────────────────────────────────────────────

// ── internal helpers ──────────────────────────────────────────────────────────

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

// ── spanless message renderer ─────────────────────────────────────────────────

/// Render a single **spanless** diagnostic message to stderr.
///
/// Used for pipeline-level messages that are not anchored to a source span
/// (e.g. "LSP not implemented", ".fdn extension warning", phase stubs).
/// Render a single **spanless** pipeline-level message to stderr.
///
/// These have no source location (e.g. phase stubs, extension warnings).
/// Badge format:  ◆  note  code  message
pub fn render_message_to_stderr(severity: Severity, code: &str, message: &str) {
    if is_color_enabled() {
        let (sym, sev_color) = match severity {
            Severity::Error => ("✗", "\x1b[1;31m"),
            Severity::Warning => ("▲", "\x1b[1;33m"),
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

/// Render a span-anchored `Diagnostic` to stderr.
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

    // Indentation grows with each level of the cause chain.
    let pad = "  ".repeat(depth);

    let color = is_color_enabled();
    let (hdr_c, under_c, reset, bold, dim) = if color {
        let h = match diag.severity {
            Severity::Error => "\x1b[1;31m",   // bold red
            Severity::Warning => "\x1b[1;33m", // bold yellow
            Severity::Note => "\x1b[1;36m",    // bold cyan
        };
        (h, "\x1b[33m", "\x1b[0m", "\x1b[1m", "\x1b[2m")
    } else {
        ("", "", "", "", "")
    };

    // "FidanError" / "FidanWarning" / "note"
    let kind_label = match diag.severity {
        Severity::Error => "FidanError",
        Severity::Warning => "FidanWarning",
        Severity::Note => "note",
    };

    // ── header ────────────────────────────────────────────────────────────────
    eprintln!("{pad}{hdr_c}{bold}{kind_label}{reset}: {}", diag.message);

    // ── location ──────────────────────────────────────────────────────────────
    eprintln!("{pad}  {dim}-->{reset} {name}:{line}:{col}");
    eprintln!("{pad}");

    // ── source snippet with underline ─────────────────────────────────────────
    let lines: Vec<&str> = src.lines().collect();
    if line > 0 && line <= lines.len() {
        let src_line = lines[line - 1];
        let line_no_str = line.to_string();
        let gutter_pad = " ".repeat(line_no_str.len());

        let under_col = col.saturating_sub(1); // 0-based column
        let underline = format!("{}{}", " ".repeat(under_col), "~".repeat(span_len));

        // First labelled span message (if any) goes inline after the underline.
        let label_msg: Option<&str> = diag
            .labels
            .first()
            .filter(|l| !l.message.is_empty())
            .map(|l| l.message.as_str());

        eprintln!("{pad}  {gutter_pad} {dim}│{reset}");
        eprintln!("{pad}  {line_no_str} {dim}│{reset} {src_line}");
        if let Some(lmsg) = label_msg {
            eprintln!(
                "{pad}  {gutter_pad} {dim}│{reset} {under_c}{underline}{reset}  {hdr_c}{lmsg}{reset}"
            );
        } else {
            eprintln!("{pad}  {gutter_pad} {dim}│{reset} {under_c}{underline}{reset}");
        }
        eprintln!("{pad}");
    }

    // ── notes ─────────────────────────────────────────────────────────────────
    for note in &diag.notes {
        eprintln!("{pad}  {dim}note:{reset} {note}");
    }

    // ── help / suggestions ────────────────────────────────────────────────────
    for sug in &diag.suggestions {
        if let Some(edit) = &sug.edit {
            eprintln!(
                "{pad}  {dim}help:{reset} {} (replace with `{}`)",
                sug.message, edit.replacement
            );
        } else {
            eprintln!("{pad}  {dim}help:{reset} {}", sug.message);
        }
    }

    if (!diag.notes.is_empty() || !diag.suggestions.is_empty()) && diag.cause_chain.is_empty() {
        eprintln!("{pad}");
    }

    // ── cause chain ───────────────────────────────────────────────────────────
    //
    // Rendered like a Python traceback, each cause indented one level deeper.
    if !diag.cause_chain.is_empty() {
        eprintln!("{pad}");
        eprintln!("{pad}  {dim}caused by:{reset}");
        for cause in &diag.cause_chain {
            render_one(cause, source_map, depth + 1);
        }
    }
}
