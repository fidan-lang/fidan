//! Position/Range conversions between Fidan's byte-offset `Span` and LSP `Position`/`Range`.

use fidan_source::{SourceFile, Span};
use tower_lsp::lsp_types::{Position, Range};

/// Convert a Fidan [`Span`] to an LSP [`Range`] using the line-start table in
/// [`SourceFile`].
///
/// Positions are 0-based in LSP, but `SourceFile::line_col()` returns 1-based
/// (line, col) pairs — we subtract 1 from each.
pub fn span_to_range(file: &SourceFile, span: Span) -> Range {
    let (sl, sc) = file.line_col(span.start);
    let (el, ec) = file.line_col(span.end);
    Range {
        start: Position {
            line: sl.saturating_sub(1),
            character: sc.saturating_sub(1),
        },
        end: Position {
            line: el.saturating_sub(1),
            character: ec.saturating_sub(1),
        },
    }
}

/// Build an LSP [`Range`] that covers the entire `text` of a document —
/// used to produce a single whole-document `TextEdit` from the formatter.
pub fn whole_document_range(text: &str) -> Range {
    // Split on '\n' so that a trailing newline produces a correct final
    // empty-string segment.
    let lines: Vec<&str> = text.split('\n').collect();
    let last_line = lines.len().saturating_sub(1) as u32;
    // The last segment from split('\n') gives the characters after the final
    // newline (could be 0 for a file that ends with \n).
    let last_char = lines.last().map(|l| l.len() as u32).unwrap_or(0);
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: last_line,
            character: last_char,
        },
    }
}
