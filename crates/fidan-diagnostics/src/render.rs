use crate::{Diagnostic, Severity};
use fidan_source::SourceMap;

// ─────────────────────────────────────────────────────────────────────────────
// Fidan Diagnostic Renderer
//
// Span-anchored format (TTY / color):
//
//  ╭─ ✖ error[E0101] ────────────────────────────────────────────────────────╮
//  │  undefined name `greting`                                               │
//  │  test.fdn:2:7                                                           │
//  ╰─────────────────────────────────────────────────────────────────────────╯
//     |
//   1 | var greeting = "Hello"
//   2 | print(greting)
//     |       ^^^^^^^ unknown name
//     |
//  help: did you mean `greeting`?
//     |
//   2 | print(greeting)
//     |       +++++++
//     |
//
// Non-TTY (piped/redirected) falls back to the flat rustc-style:
//
//   error[E0101]: undefined name `greting`
//     --> test.fdn:2:7
//      …
//
// Cause-chain (one level per cause, labelled):
//
//   caused by (1/2):
//     ╭─ ✖ error[E0201] ──…
//     …
//
// Spanless pipeline badge:
//
//  ╭─ ◆ note ────────────────────────────────────╮
//  │  interpreter not yet implemented (Phase 5)  │
//  ╰─────────────────────────────────────────────╯
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

/// Detect the current terminal width, falling back to 80 columns.
/// Result is clamped to [50, 120] so boxes never grow absurdly wide.
/// `$COLUMNS` overrides the OS query (handy in CI or scripts).
fn terminal_width() -> usize {
    if let Ok(s) = std::env::var("COLUMNS")
        && let Ok(n) = s.trim().parse::<usize>()
    {
        return n.clamp(50, 120);
    }

    #[cfg(unix)]
    {
        #[repr(C)]
        struct Winsize {
            ws_row: u16,
            ws_col: u16,
            _ws_xpixel: u16,
            _ws_ypixel: u16,
        }
        // TIOCGWINSZ ioctl number (platform-specific).
        #[cfg(target_os = "macos")]
        const TIOCGWINSZ: i32 = 0x40087468_u32 as i32;
        #[cfg(not(target_os = "macos"))]
        const TIOCGWINSZ: i32 = 0x5413;
        unsafe extern "C" {
            fn ioctl(fd: i32, request: i32, ...) -> i32;
        }
        use std::os::unix::io::AsRawFd;
        let fd = std::io::stderr().as_raw_fd();
        let mut ws = Winsize {
            ws_row: 0,
            ws_col: 0,
            _ws_xpixel: 0,
            _ws_ypixel: 0,
        };
        let ret = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut Winsize) };
        if ret == 0 && ws.ws_col >= 50 {
            return (ws.ws_col as usize).min(120);
        }
    }

    #[cfg(windows)]
    {
        #[repr(C)]
        struct Coord {
            x: i16,
            y: i16,
        }
        #[repr(C)]
        struct SmallRect {
            left: i16,
            top: i16,
            right: i16,
            bottom: i16,
        }
        #[repr(C)]
        struct ConsoleScreenBufferInfo {
            dw_size: Coord,
            dw_cursor_position: Coord,
            w_attributes: u16,
            sr_window: SmallRect,
            dw_maximum_window_size: Coord,
        }
        unsafe extern "system" {
            fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
            fn GetConsoleScreenBufferInfo(
                hConsoleOutput: *mut core::ffi::c_void,
                lpConsoleScreenBufferInfo: *mut ConsoleScreenBufferInfo,
            ) -> i32;
        }
        // STD_ERROR_HANDLE = -12i32 as u32
        const STD_ERROR_HANDLE: u32 = 0xFFFFFFF4;
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if !handle.is_null() && handle as isize != -1 {
            let mut info = std::mem::MaybeUninit::<ConsoleScreenBufferInfo>::uninit();
            let ret = unsafe { GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) };
            if ret != 0 {
                let info = unsafe { info.assume_init() };
                let w = (info.sr_window.right - info.sr_window.left + 1) as usize;
                if w >= 50 {
                    return w.min(120);
                }
            }
        }
    }

    80 // fallback
}

/// Split `text` into lines of at most `width` visible characters, breaking at
/// word boundaries.  Words longer than `width` are kept on a line of their own
/// rather than being truncated.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if current_len == 0 {
            current.push_str(word);
            current_len = wlen;
        } else if current_len + 1 + wlen <= width {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + wlen;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = wlen;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn wrap_preserving_layout(text: &str, width: usize) -> Vec<String> {
    let expanded = text.replace('\t', "    ");
    let mut lines = Vec::new();

    for raw_line in expanded.split('\n') {
        if raw_line.chars().count() <= width {
            lines.push(raw_line.to_string());
        } else {
            lines.extend(word_wrap(raw_line.trim(), width));
        }
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

// ── spanless badge renderer ───────────────────────────────────────────────────

/// Render a spanless pipeline-level message to stderr.
///
/// When color/Unicode output is enabled the message is wrapped in a bordered
/// box using box-drawing characters:
///
/// ```text
///  ╭─ ✖ error ──────────────────────────────────────────────────────────────╮
///  │  W2001  file 'test.js' does not have the '.fdn' extension              │
///  ╰────────────────────────────────────────────────────────────────────────╯
/// ```
pub fn render_message_to_stderr(severity: Severity, code: impl std::fmt::Display, message: &str) {
    let code_s = code.to_string();
    if is_color_enabled() {
        let (sym, sev_color) = match severity {
            Severity::Error => ("✖", "\x1b[1;31m"),
            Severity::Warning => ("⚠", "\x1b[1;33m"),
            Severity::Note => ("◆", "\x1b[1;36m"),
        };
        let sev_str = severity.to_string();
        let reset = "\x1b[0m";
        let bold = "\x1b[1m";

        // ── Box layout — adapts to the current terminal width ────────────────
        // Top:    " ╭─ {sym} {sev_str} ─...─╮"   1+1+2+title_vis+1+dashes+1 = w
        // Body:   " │  [{code}  ]{message}{pad}  │"  1+1+2+body_vis+pad+2+1 = w
        // Bottom: " ╰─...─╯"                       1+1+(w-3)+1 = w
        let w = terminal_width() - 1;
        let cw = w - 7; // usable content width inside │  …  │

        // Title (sym + sev_str): sym is 1 terminal column, space, sev_str chars.
        let title_vis = 1 + 1 + sev_str.chars().count();
        let dashes_top = w.saturating_sub(6 + title_vis);
        eprintln!(
            " {sev_color}╭─ {sym} {sev_str} {}╮{reset}",
            "─".repeat(dashes_top)
        );

        // Body: optional bold "code  " prefix, then word-wrapped message.
        let (prefix_styled, prefix_vis) = if code_s.is_empty() {
            (String::new(), 0usize)
        } else {
            (
                format!("{bold}{code_s}{reset}  "),
                code_s.chars().count() + 2,
            )
        };
        let text_width = cw.saturating_sub(prefix_vis).max(cw / 2);
        let wrapped = wrap_preserving_layout(message, text_width);
        for (i, chunk) in wrapped.iter().enumerate() {
            let content = if i == 0 {
                format!("{prefix_styled}{chunk}")
            } else {
                format!("{}{chunk}", " ".repeat(prefix_vis))
            };
            let content_vis = prefix_vis + chunk.chars().count();
            let pad = cw.saturating_sub(content_vis.min(cw));
            eprintln!(
                " {sev_color}│{reset}  {}{}  {sev_color}│{reset}",
                content,
                " ".repeat(pad)
            );
        }

        // Bottom border.
        eprintln!(" {sev_color}╰{}╯{reset}", "─".repeat(w - 3));
    } else {
        let sev_str = severity.to_string();
        if code_s.is_empty() {
            eprintln!("{sev_str}  {message}");
        } else {
            eprintln!("{sev_str}  {code_s}  {message}");
        }
    }
}

// ── backtrace renderer ──────────────────────────────────────────────────────

/// Render a compiler-crash stack trace to stderr, keeping only the frames
/// that belong to Fidan compiler code (function names starting with `fidan`).
///
/// Intended to be called from the global panic hook immediately after
/// [`render_message_to_stderr`] has displayed the crash box.
pub fn render_backtrace_to_stderr(bt: &std::backtrace::Backtrace) {
    let raw = bt.to_string();
    let raw = raw.trim();
    if raw.is_empty() || matches!(raw, "disabled" | "unsupported") {
        return;
    }

    // ── Parse frames ─────────────────────────────────────────────────────────
    // Each frame spans one or two lines:
    //   "   N: fn_name"
    //   "             at file:line"   ← optional location continuation
    struct Frame {
        func: String,
        location: Option<String>,
    }

    let mut frames: Vec<Frame> = Vec::new();
    let mut pending: Option<String> = None;

    for line in raw.lines() {
        let t = line.trim_start();
        if t.is_empty() {
            continue;
        }
        // Frame-start: an all-digit prefix before the first ':'
        if let Some(colon) = t.find(':') {
            let prefix = &t[..colon];
            if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
                if let Some(func) = pending.take() {
                    frames.push(Frame {
                        func,
                        location: None,
                    });
                }
                pending = Some(t[colon + 1..].trim().to_string());
                continue;
            }
        }
        // Location continuation: "at <path>"
        if let Some(stripped) = t.strip_prefix("at ")
            && let Some(func) = pending.take()
        {
            frames.push(Frame {
                func,
                location: Some(stripped.trim().to_string()),
            });
            continue;
        }
    }
    if let Some(func) = pending {
        frames.push(Frame {
            func,
            location: None,
        });
    }

    // ── Filter to Fidan frames only ───────────────────────────────────────────
    // Also drop the panic-hook closure itself (fidan::main::closure$N) since
    // it is implementation noise, not a meaningful compiler frame.
    let fidan: Vec<&Frame> = frames
        .iter()
        .filter(|f| {
            let lower = f.func.to_lowercase();
            lower.starts_with("fidan") && !lower.contains("closure")
        })
        .collect();

    if fidan.is_empty() {
        return;
    }

    // ── Render ────────────────────────────────────────────────────────────────
    if is_color_enabled() {
        let dim = "\x1b[2m";
        let bold = "\x1b[1m";
        let reset = "\x1b[0m";
        eprintln!();
        eprintln!("   {dim}stack trace{reset}");
        for (i, frame) in fidan.iter().enumerate() {
            eprintln!("   {dim}{:>3}.{reset} {bold}{}{reset}", i + 1, frame.func);
            if let Some(loc) = &frame.location {
                eprintln!("        {dim}at {loc}{reset}");
            }
        }
        eprintln!();
    } else {
        eprintln!("\n   stack trace:");
        for (i, frame) in fidan.iter().enumerate() {
            eprintln!("   {:>3}. {}", i + 1, frame.func);
            if let Some(loc) = &frame.location {
                eprintln!("        at {loc}");
            }
        }
        eprintln!();
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

    // ── Header box (TTY) or flat line (non-TTY) ──────────────────────────────
    let kind_label = match diag.severity {
        Severity::Error if !diag.code.is_empty() => format!("error[{}]", diag.code),
        Severity::Warning if !diag.code.is_empty() => format!("warning[{}]", diag.code),
        Severity::Error => "error".to_string(),
        Severity::Warning => "warning".to_string(),
        Severity::Note => "note".to_string(),
    };
    if color {
        let sym = match diag.severity {
            Severity::Error => "\u{2716}",   // ✖
            Severity::Warning => "\u{26a0}", // ⚠
            Severity::Note => "\u{25c6}",    // ◆
        };
        let w = terminal_width() - 1;
        let cw = w - 7; // usable content width inside │  …  │
        // Top: " ╭─ {sym} {kind_label} ─{dashes}╮"
        let title_vis = 1 + 1 + kind_label.chars().count();
        let dashes_top = w.saturating_sub(6 + title_vis);
        eprintln!(
            "{dp} {hdr_c}\u{256d}\u{2500} {sym} {kind_label} {}\u{256e}{reset}",
            "\u{2500}".repeat(dashes_top)
        );
        // Message body line - word-wrapped so long messages don't overflow the box
        let wrapped_msg = word_wrap(&diag.message, cw);
        for (i, chunk) in wrapped_msg.iter().enumerate() {
            let chunk_vis = chunk.chars().count();
            let chunk_pad = cw.saturating_sub(chunk_vis.min(cw));
            if i == 0 {
                eprintln!(
                    "{dp} {hdr_c}\u{2502}{reset}  {bold}{chunk}{reset}{}  {hdr_c}\u{2502}{reset}",
                    " ".repeat(chunk_pad)
                );
            } else {
                eprintln!(
                    "{dp} {hdr_c}\u{2502}{reset}  {chunk}{}  {hdr_c}\u{2502}{reset}",
                    " ".repeat(chunk_pad)
                );
            }
        }
        // Location line (dimmed)
        let loc_str = format!("{name}:{line}:{col}");
        let loc_chars: String = loc_str.chars().take(cw).collect();
        let loc_vis = loc_chars.chars().count();
        let loc_pad = cw.saturating_sub(loc_vis);
        eprintln!(
            "{dp} {hdr_c}\u{2502}{reset}  {dim}{loc_chars}{reset}{}  {hdr_c}\u{2502}{reset}",
            " ".repeat(loc_pad)
        );
        // Bottom border
        eprintln!(
            "{dp} {hdr_c}\u{2570}{}\u{256f}{reset}",
            "\u{2500}".repeat(w - 3)
        );
    } else {
        // Non-TTY: keep the flat rustc-style format
        eprintln!("{dp}{kind_label}: {}", diag.message);
        eprintln!("{dp}  --> {name}:{line}:{col}");
    }

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

        // Optional inline label — only from a *primary* label on this line.
        let label_msg: Option<&str> = diag
            .labels
            .iter()
            .find(|l| l.primary && !l.message.is_empty())
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

    // ── Secondary labels (e.g. "first declared here") ─────────────────────────
    //
    // Each secondary label with a distinct span gets its own mini-snippet so
    // the user can see both locations at a glance.
    for label in diag
        .labels
        .iter()
        .filter(|l| !l.primary && !l.message.is_empty())
    {
        // Resolve the source for this label's file (may differ from primary).
        let lfile = source_map.get(label.span.file);
        let lsrc: &str = &lfile.src;
        let lname: &str = &lfile.name;
        let (lline, lcol) = byte_to_line_col(lsrc, label.span.start as usize);
        let lspan_len = (label.span.end as usize)
            .saturating_sub(label.span.start as usize)
            .max(1);
        let llines: Vec<&str> = lsrc.lines().collect();
        let ltotal = llines.len();

        if lline > 0 && lline <= ltotal {
            eprintln!("{dp}  {dim}note:{reset} {}", label.message);
            eprintln!("{dp}  {dim}-->{reset} {lname}:{lline}:{lcol}");

            let lctx_start = if lline > 1 { lline - 1 } else { lline };
            let lctx_end = (lline + 1).min(ltotal);
            let lgutter_w = lctx_end.to_string().len();
            let lg = " ".repeat(lgutter_w);

            eprintln!("{dp}  {lg} |");
            for ln in lctx_start..=lctx_end {
                if ln == 0 || ln > ltotal {
                    continue;
                }
                let src_line = llines[ln - 1];
                let ln_s = format!("{:>width$}", ln, width = lgutter_w);
                if ln == lline {
                    eprintln!("{dp}  {ln_s} | {src_line}");
                    let uline = format!(
                        "{}{}",
                        " ".repeat(lcol.saturating_sub(1)),
                        "~".repeat(lspan_len),
                    );
                    eprintln!("{dp}  {lg} | {dim}{uline}{reset}");
                } else {
                    eprintln!("{dp}  {ctx_c}{ln_s} | {src_line}{reset}");
                }
            }
            eprintln!("{dp}  {lg} |");
        }
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
