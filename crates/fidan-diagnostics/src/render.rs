use crate::{Diagnostic, Severity};
use ariadne::{CharSet, Color, Config, Label, Report, ReportKind, sources};
use fidan_source::SourceMap;

// ── Spanless message renderer ─────────────────────────────────────────────────

/// Render a single **spanless** diagnostic message to stderr.
///
/// Use this for messages that are not anchored to a specific source location
/// (e.g. CLI warnings, pipeline stub notices, file-level conditions).
///
/// **⚠ Placeholder format** — the current output (`warning[W001]: …`) intentionally
/// reuses Rust-style formatting as a stopgap. Phase 4 will replace this with
/// Fidan's own branded visual identity (custom badges, cause-chain display,
/// stdout/stderr separation, NLP explanations). See PROGRESS.md §Phase 4.
///
/// Colour is suppressed when `NO_COLOR` is set or stderr is not a terminal.
pub fn render_message_to_stderr(severity: Severity, code: &str, message: &str) {
    use std::io::IsTerminal;
    let use_color = std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal();

    if use_color {
        // severity label colour
        let color = match severity {
            Severity::Error => "\x1b[1;31m",   // bold red
            Severity::Warning => "\x1b[1;33m", // bold yellow
            Severity::Note => "\x1b[1;36m",    // bold cyan
        };
        let bold = "\x1b[1m";
        let reset = "\x1b[0m";

        if code.is_empty() {
            eprintln!("{color}{severity}{reset}{bold}: {message}{reset}");
        } else {
            eprintln!("{color}{severity}[{code}]{reset}{bold}: {message}{reset}");
        }
    } else {
        // Plain fallback — no ANSI, identical information
        if code.is_empty() {
            eprintln!("{severity}: {message}");
        } else {
            eprintln!("{severity}[{code}]: {message}");
        }
    }
}

/// Render a single diagnostic to **stderr** using ariadne.
///
/// `source_map` is used to look up the source text and file name from the
/// `FileId` stored in the diagnostic's span.
pub fn render_to_stderr(diag: &Diagnostic, source_map: &SourceMap) {
    let file = source_map.get(diag.span.file);
    let name: String = file.name.to_string();
    let src: String = file.src.to_string();

    let kind = match diag.severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note => ReportKind::Advice,
    };

    // Use Unicode box-drawing characters only when writing directly to a TTY
    // (a real terminal that supports UTF-8).  When stderr is a pipe — e.g. when
    // the user runs `2>&1` in PowerShell — UTF-8 box chars get garbled by the
    // shell's code-page.  Falling back to ASCII keeps the output readable.
    use std::io::IsTerminal;
    let char_set = if std::io::stderr().is_terminal() {
        CharSet::Unicode
    } else {
        CharSet::Ascii
    };
    let cfg = Config::default().with_char_set(char_set);

    // In ariadne 0.6 the primary span (file, range) is passed directly to build().
    let primary_range = diag.span.start as usize..diag.span.end as usize;
    let mut b = Report::build(kind, (name.clone(), primary_range.clone()))
        .with_config(cfg)
        .with_message(&diag.message);

    if !diag.code.is_empty() {
        b = b.with_code(&diag.code);
    }

    let primary_color = match diag.severity {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Note => Color::BrightBlue,
    };

    if diag.labels.is_empty() {
        // Emit an implicit primary label so ariadne has something to underline.
        b = b.with_label(Label::new((name.clone(), primary_range)).with_color(primary_color));
    } else {
        for lbl in &diag.labels {
            let color = if lbl.primary {
                primary_color
            } else {
                Color::Blue
            };
            let range = lbl.span.start as usize..lbl.span.end as usize;
            let mut al = Label::new((name.clone(), range)).with_color(color);
            if !lbl.message.is_empty() {
                al = al.with_message(&lbl.message);
            }
            b = b.with_label(al);
        }
    }

    b.finish()
        .eprint(sources([(name, src)]))
        .expect("failed to render diagnostic");
}
