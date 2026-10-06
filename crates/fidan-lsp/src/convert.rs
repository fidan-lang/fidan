//! Position/Range conversions between Fidan's byte-offset `Span` and LSP `Position`/`Range`.

use fidan_source::{SourceFile, Span};
use tower_lsp::lsp_types::{Position, Range};

/// Convert a Fidan [`Span`] to an LSP [`Range`] using the line-start table in
/// [`SourceFile`].
///
/// LSP columns count UTF-16 code units; source spans count UTF-8 bytes.
pub fn span_to_range(file: &SourceFile, span: Span) -> Range {
    Range {
        start: offset_to_lsp_pos(file, span.start),
        end: offset_to_lsp_pos(file, span.end),
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
    let last_char = lines
        .last()
        .map(|l| l.encode_utf16().count() as u32)
        .unwrap_or(0);
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

/// Convert a byte offset to an LSP `Position` (0-based line, UTF-16 character).
pub(crate) fn offset_to_lsp_pos(file: &SourceFile, offset: u32) -> Position {
    let off = offset as usize;
    let line = match file.line_starts.binary_search(&(off as u32)) {
        Ok(l) => l,
        Err(l) => l.saturating_sub(1),
    };
    let line_start = file.line_starts[line] as usize;
    let col_bytes = off.saturating_sub(line_start);
    // Convert the byte column to UTF-16 code units.
    let line_text = file.src.get(line_start..).unwrap_or("");
    let mut utf16_col = 0u32;
    let mut remaining = col_bytes;
    for ch in line_text.chars() {
        if remaining == 0 {
            break;
        }
        let byte_len = ch.len_utf8();
        if remaining < byte_len {
            break;
        }
        remaining -= byte_len;
        utf16_col += ch.len_utf16() as u32;
    }
    Position {
        line: line as u32,
        character: utf16_col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_source::FileId;

    #[test]
    fn unicode_spans_and_document_edits_use_utf16_columns() {
        let source = "é🌱x\n中🌱z";
        let file = SourceFile::new(FileId(0), "unicode.fdn", source);
        let offset = source.find('x').unwrap() as u32;
        let range = span_to_range(&file, Span::new(file.id, offset, offset + 1));
        assert_eq!(range.start, Position::new(0, 3));
        assert_eq!(range.end, Position::new(0, 4));
        assert_eq!(whole_document_range(source).end, Position::new(1, 4));
        assert_eq!(whole_document_range("🌱\n").end, Position::new(1, 0));
        assert_eq!(whole_document_range("").end, Position::new(0, 0));
    }
}
