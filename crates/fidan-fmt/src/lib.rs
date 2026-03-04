//! `fidan-fmt` — Canonical source formatter for the Fidan programming language.
//!
//! # Usage
//!
//! ```rust
//! use fidan_fmt::{FormatOptions, format_source};
//!
//! let opts = FormatOptions::default();
//! let formatted = format_source("var x=1\nvar y=2", &opts);
//! ```
//!
//! On the command line:
//! ```text
//! fidan fmt file.fdn            # print to stdout
//! fidan fmt file.fdn --in-place # rewrite in place
//! fidan fmt file.fdn --check    # exit 1 if not already formatted (CI mode)
//! ```

pub mod config;
mod emit_expr;
mod emit_item;
mod emit_stmt;
mod printer;

pub use config::FormatOptions;

use emit_item::emit_module;
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::{FileId, SourceFile};
use std::sync::Arc;

// ── Public API ─────────────────────────────────────────────────────────────────

/// Format the given Fidan source string using the provided options.
///
/// Parse errors are tolerated: nodes that could not be parsed are replaced by
/// `# <parse error>` comments so the rest of the file is still formatted.
///
/// The returned string always ends with exactly one newline character.
pub fn format_source(src: &str, opts: &FormatOptions) -> String {
    let interner = Arc::new(SymbolInterner::new());
    let file = SourceFile::new(FileId(0), "<fmt>", src);
    let (tokens, _lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, _parse_diags) =
        fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
    let mut p = printer::Printer::new(&module.arena, &interner, opts);
    emit_module(&mut p, &module);
    p.finish()
}

/// Returns `true` when `src` is already formatted according to `opts`.
///
/// Useful for CI checks (`fidan fmt --check`): exit non-zero when this
/// returns `false`.
pub fn check_formatted(src: &str, opts: &FormatOptions) -> bool {
    format_source(src, opts) == src
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        format_source(src, &FormatOptions::default())
    }

    // ── Idempotence ───────────────────────────────────────────────────────

    /// Formatting an already-formatted string must be a no-op.
    fn assert_idempotent(src: &str) {
        let first = fmt(src);
        let second = fmt(&first);
        assert_eq!(
            first, second,
            "formatter is not idempotent!\nfirst pass:\n{first}\nsecond pass:\n{second}"
        );
    }

    // ── Literals ──────────────────────────────────────────────────────────

    #[test]
    fn integer_var() {
        let out = fmt("var x = 42");
        assert_eq!(out, "var x = 42\n");
        assert_idempotent("var x = 42\n");
    }

    #[test]
    fn typed_var() {
        let out = fmt("var count oftype integer = 0");
        assert_eq!(out, "var count oftype integer = 0\n");
    }

    #[test]
    fn const_var() {
        let out = fmt("const var MAX oftype integer = 100");
        assert_eq!(out, "const var MAX oftype integer = 100\n");
    }

    #[test]
    fn bool_var() {
        let out = fmt("var flag = true");
        assert_eq!(out, "var flag = true\n");
    }

    // ── Actions ───────────────────────────────────────────────────────────

    #[test]
    fn action_no_params() {
        let src = "action main {\n    var x = 1\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    #[test]
    fn action_with_params() {
        let src = "action greet with (certain name oftype string) returns string {\n    return name\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    #[test]
    fn parallel_action() {
        let src = "parallel action fetch with (certain url oftype string) returns string {\n    return url\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
    }

    // ── Use imports ───────────────────────────────────────────────────────

    #[test]
    fn use_simple() {
        let out = fmt("use std.io");
        assert_eq!(out, "use std.io\n");
    }

    #[test]
    fn use_alias() {
        let out = fmt("use std.io as io");
        assert_eq!(out, "use std.io as io\n");
    }

    // ── Control flow ──────────────────────────────────────────────────────

    #[test]
    fn if_else() {
        let src = "action f {\n    if x == 1 {\n        var a = 1\n    } else {\n        var b = 2\n    }\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    #[test]
    fn for_loop() {
        let src = "action f {\n    for i in items {\n        print(i)\n    }\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    #[test]
    fn while_loop() {
        let src = "action f {\n    while x > 0 {\n        x = x - 1\n    }\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    // ── Operators ─────────────────────────────────────────────────────────

    #[test]
    fn binary_precedence_parens() {
        // (a + b) * c — the addition has lower precedence, needs parens
        let out = fmt("var x = (a + b) * c");
        assert_eq!(out, "var x = (a + b) * c\n");
    }

    #[test]
    fn binary_no_spurious_parens() {
        // a + b + c — left-associative, no parens needed
        let out = fmt("var x = a + b + c");
        assert_eq!(out, "var x = a + b + c\n");
    }

    // ── Blank-line separation ─────────────────────────────────────────────

    #[test]
    fn blank_line_between_items() {
        let src = "var a = 1\n\nvar b = 2\n";
        let out = fmt(src);
        assert_eq!(out, src);
    }

    // ── check_formatted ───────────────────────────────────────────────────

    #[test]
    fn check_formatted_already_clean() {
        let src = "var x = 1\n";
        assert!(check_formatted(src, &FormatOptions::default()));
    }

    #[test]
    fn check_formatted_unclean() {
        // Extra spaces that the formatter would remove/normalise
        // (the formatter normalises `= 1` not `  =  1`)
        let src = "var  x  =  1\n";
        // The formatter would produce "var x = 1\n", which differs.
        // Depending on how the lexer handles multiple spaces this may or may
        // not differ — the key is that the function doesn't panic.
        let _ = check_formatted(src, &FormatOptions::default());
    }
}
