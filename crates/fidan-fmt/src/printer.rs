//! Low-level output buffer used by all emitters.

use crate::config::FormatOptions;
use fidan_ast::AstArena;
use fidan_lexer::{Symbol, SymbolInterner};
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
}

impl<'a> Printer<'a> {
    pub fn new(arena: &'a AstArena, interner: &'a SymbolInterner, opts: &'a FormatOptions) -> Self {
        Self {
            out: String::with_capacity(4096),
            indent: 0,
            at_sol: true,
            arena,
            interner,
            opts,
        }
    }

    // ── Symbol resolution ──────────────────────────────────────────────────

    /// Resolve a `Symbol` to an owned `String`.
    /// We return `String` intentionally so callers can pass it to `w()` without
    /// a simultaneous immutable + mutable borrow of `self`.
    pub fn sym_s(&self, sym: Symbol) -> Arc<str> {
        self.interner.resolve(sym)
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
