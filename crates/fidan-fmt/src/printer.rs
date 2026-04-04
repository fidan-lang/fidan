//! Low-level output buffer used by all emitters.

use crate::comments::{FmtComment, normalize_comment_lines};
use crate::config::FormatOptions;
use fidan_ast::AstArena;
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::{SourceFile, Span};
use std::sync::Arc;

/// Accumulates formatted text with indentation and blank-line management.
pub struct Printer<'a> {
    out: String,
    /// Current indent depth *in spaces*.
    indent: usize,
    /// `true` immediately after a newline — the indent has not been written yet.
    at_sol: bool,
    pub arena: &'a AstArena,
    pub interner: &'a SymbolInterner,
    pub opts: &'a FormatOptions,
    source: &'a SourceFile,
    comments: Vec<FmtComment>,
    next_comment: usize,
}

impl<'a> Printer<'a> {
    pub fn new(
        arena: &'a AstArena,
        interner: &'a SymbolInterner,
        opts: &'a FormatOptions,
        source: &'a SourceFile,
        comments: Vec<FmtComment>,
    ) -> Self {
        Self {
            out: String::with_capacity(4096),
            indent: 0,
            at_sol: true,
            arena,
            interner,
            opts,
            source,
            comments,
            next_comment: 0,
        }
    }

    // ── Symbol resolution ──────────────────────────────────────────────────

    /// Resolve a `Symbol` to an owned `String`.
    /// We return `String` intentionally so callers can pass it to `w()` without
    /// a simultaneous immutable + mutable borrow of `self`.
    pub fn sym_s(&self, sym: Symbol) -> Arc<str> {
        self.interner.resolve(sym)
    }

    /// Create a scratch printer that shares arena/interner/config/source state
    /// but starts with no buffered output or pending comments.
    pub fn scratch(&self) -> Self {
        Self::new(
            self.arena,
            self.interner,
            self.opts,
            self.source,
            Vec::new(),
        )
    }

    // ── Output primitives ──────────────────────────────────────────────────

    /// Write a string fragment at the current position.
    /// If we are at the start of a line, the indent is written first.
    pub fn w(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.at_sol {
            let spaces = self.indent;
            for _ in 0..spaces {
                self.out.push(' ');
            }
            self.at_sol = false;
        }
        self.out.push_str(s);
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    pub fn source_slice(&self, span: Span) -> &str {
        &self.source.src[span.start as usize..span.end as usize]
    }

    /// End the current line (write `\n`).
    /// Trailing spaces on the current line are trimmed before the newline.
    pub fn nl(&mut self) {
        // Trim trailing spaces (happens when a blank line is emitted while at_sol=false)
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        self.at_sol = true;
    }

    /// Insert a blank separator line.
    ///
    /// Safe to call when already at start-of-line.
    pub fn blank(&mut self) {
        if !self.at_sol {
            self.nl();
        }
        // Push a bare newline — no indent (that would be trailing whitespace).
        self.out.push('\n');
        // Still at start-of-line after the blank.
        self.at_sol = true;
    }

    pub fn emit_comments_before(&mut self, offset: u32) {
        while self.next_comment < self.comments.len()
            && self.comments[self.next_comment].start < offset
        {
            let comment = self.comments[self.next_comment].clone();
            self.emit_comment_standalone(&comment);
            self.next_comment += 1;
        }
    }

    pub fn emit_trailing_comments_for(&mut self, span_end: u32) {
        let owner_offset = span_end.saturating_sub(1);
        let owner_line = self.source.line_col(owner_offset).0;
        while self.next_comment < self.comments.len() {
            let comment = self.comments[self.next_comment].clone();
            let comment_line = self.source.line_col(comment.start).0;
            if !comment.inline || comment_line != owner_line {
                break;
            }
            if self.at_sol {
                self.w("");
            }
            self.w("  ");
            self.emit_comment_inline(&comment);
            self.next_comment += 1;
        }
    }

    pub fn emit_remaining_comments(&mut self) {
        while self.next_comment < self.comments.len() {
            let comment = self.comments[self.next_comment].clone();
            self.emit_comment_standalone(&comment);
            self.next_comment += 1;
        }
    }

    fn emit_comment_standalone(&mut self, comment: &FmtComment) {
        if !self.at_sol {
            self.nl();
        }
        let lines = normalize_comment_lines(&comment.text);
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                self.nl();
            }
            if !line.is_empty() {
                self.w(line);
            }
        }
        self.nl();
    }

    fn emit_comment_inline(&mut self, comment: &FmtComment) {
        let lines = normalize_comment_lines(&comment.text);
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                self.nl();
            }
            if !line.is_empty() {
                self.w(line);
            }
        }
    }

    // ── Indentation ────────────────────────────────────────────────────────

    pub fn indent_in(&mut self) {
        self.indent += self.opts.indent_width;
    }

    pub fn indent_out(&mut self) {
        self.indent = self.indent.saturating_sub(self.opts.indent_width);
    }

    // ── Finalise ───────────────────────────────────────────────────────────

    /// Consume the printer and return the completed formatted string.
    /// The output is trimmed of trailing blank lines and ends with exactly
    /// one newline character.
    pub fn finish(mut self) -> String {
        // Trim trailing whitespace / blank lines.
        let trimmed = self.out.trim_end_matches([' ', '\n']);
        self.out = trimmed.to_string();
        self.out.push('\n');
        self.out
    }
}
