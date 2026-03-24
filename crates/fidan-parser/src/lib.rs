//! `fidan-parser` — Recursive-descent parser + Pratt expression parser.

mod parser;
mod pratt;
mod recovery;

pub use parser::Parser;

use fidan_ast::Module;
use fidan_diagnostics::Diagnostic;
use fidan_lexer::{SymbolInterner, Token};
use fidan_source::FileId;
use std::sync::Arc;

/// Parse a flat token stream into a [`Module`] AST.
///
/// Returns the completed module and any diagnostics produced during parsing.
/// Even when diagnostics are present the module is valid — `Expr::Error` /
/// `Stmt::Error` placeholders are inserted so downstream passes can continue.
pub fn parse(
    tokens: &[Token],
    file_id: FileId,
    interner: Arc<SymbolInterner>,
) -> (Module, Vec<Diagnostic>) {
    let mut p = Parser::new(tokens, file_id, interner);
    p.parse_module();
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_diagnostics::Severity;
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    /// Lex `src` then parse it. Returns (module, error_diagnostics).
    fn parse_src(src: &str) -> (Module, Vec<Diagnostic>) {
        let file = SourceFile::new(FileId(0), "<test>", src);
        let interner = Arc::new(SymbolInterner::new());
        let (tokens, _lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        parse(&tokens, FileId(0), interner)
    }

    fn errors(diags: &[Diagnostic]) -> Vec<&str> {
        diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message.as_str())
            .collect()
    }

    // ── Variable declarations ─────────────────────────────────────────────────

    #[test]
    fn var_integer() {
        let (_, diags) = parse_src("var x = 42");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn var_with_type_annotation() {
        let (_, diags) = parse_src("var x oftype integer = 10");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn var_nothing() {
        let (_, diags) = parse_src("var x = nothing");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Action declarations ───────────────────────────────────────────────────

    #[test]
    fn action_no_params() {
        let (_, diags) = parse_src("action greet { print(\"hello\") }");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn action_with_params_and_return() {
        let (_, diags) = parse_src(
            r#"action add with (a oftype integer, b oftype integer) returns integer {
                return a + b
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn parallel_action() {
        let (_, diags) = parse_src(
            r#"parallel action fetch returns string {
                return "data"
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn extern_action_without_body_parses() {
        let (_, diags) = parse_src(
            r#"@extern("self", symbol = "native_add")
            action nativeAdd with (a oftype integer, b oftype integer) returns integer"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn decorator_positional_after_named_is_rejected() {
        let (_, diags) = parse_src(
            r#"@extern("self", symbol = "native_add", "oops")
            action nativeAdd returns integer"#,
        );
        assert!(
            errors(&diags)
                .iter()
                .any(|msg| msg.contains("positional arguments must come before named arguments")),
            "expected positional-after-named error, got {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn decorator_duplicate_named_arg_is_rejected() {
        let (_, diags) = parse_src(
            r#"@extern("self", symbol = "a", symbol = "b")
            action nativeAdd returns integer"#,
        );
        assert!(
            errors(&diags)
                .iter()
                .any(|msg| msg.contains("duplicate named argument")),
            "expected duplicate named arg error, got {:?}",
            errors(&diags)
        );
    }

    // ── Object declarations ───────────────────────────────────────────────────

    #[test]
    fn object_simple() {
        let (_, diags) = parse_src(
            r#"object Point {
                var x oftype float
                var y oftype float
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn object_extends() {
        let (_, diags) = parse_src(
            r#"object Animal { var name oftype string }
            object Dog extends Animal { var breed oftype string }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Control flow ──────────────────────────────────────────────────────────

    #[test]
    fn if_otherwise_else() {
        let (_, diags) = parse_src(
            r#"if x > 0 {
                print("positive")
            } otherwise when x < 0 {
                print("negative")
            } otherwise {
                print("zero")
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn while_loop() {
        let (_, diags) = parse_src(
            r#"var i = 0
            while i < 10 {
                i = i + 1
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn for_loop() {
        let (_, diags) = parse_src("for item in items { print(item) }");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn attempt_catch() {
        let (_, diags) = parse_src(
            r#"attempt {
                panic("oops")
            } catch e {
                print(e)
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn attempt_rescue_alias() {
        let (_, diags) = parse_src(
            r#"attempt {
                panic("oops")
            } rescue e {
                print(e)
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Expressions ───────────────────────────────────────────────────────────

    #[test]
    fn arithmetic_precedence() {
        let (_, diags) = parse_src("var r = 1 + 2 * 3 - 4 / 2");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn null_coalesce() {
        let (_, diags) = parse_src("var r = nothing ?? 42");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn ternary_expression() {
        let (_, diags) = parse_src("var r = 1 if x > 0 else 0");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn list_literal() {
        let (_, diags) = parse_src("var xs = [1, 2, 3]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn string_interpolation() {
        let (_, diags) = parse_src(r#"var msg = "hello {name}!""#);
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Concurrency syntax ────────────────────────────────────────────────────

    #[test]
    fn spawn_await() {
        let (_, diags) = parse_src(
            r#"var h = spawn compute()
            var r = await h"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn parallel_block() {
        let (_, diags) = parse_src(
            r#"parallel {
                task A { print("a") }
                task B { print("b") }
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn concurrent_block() {
        let (_, diags) = parse_src(
            r#"concurrent {
                task X { print("x") }
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Error recovery ────────────────────────────────────────────────────────

    #[test]
    fn error_recovery_does_not_panic() {
        // Malformed input — must produce diagnostics but never panic.
        let (_, diags) = parse_src("var 123");
        assert!(!diags.is_empty(), "expected at least one diagnostic");
    }

    #[test]
    fn error_recovery_continues_after_bad_token() {
        // Second declaration should still be parsed despite the first being broken.
        let (module, _diags) = parse_src("var @@@ = 1\nvar y = 2");
        // At least one item should have been recovered.
        assert!(!module.items.is_empty());
    }

    // ── List comprehensions ───────────────────────────────────────────────────

    #[test]
    fn list_comprehension_simple() {
        let (_, diags) = parse_src("var xs = [x for x in items]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn list_comprehension_with_filter() {
        let (_, diags) = parse_src("var evens = [x for x in nums if x % 2 == 0]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn list_comprehension_with_transform() {
        let (_, diags) = parse_src("var squares = [x * x for x in [1, 2, 3, 4, 5]]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Dict literals & comprehensions ────────────────────────────────────────

    #[test]
    fn dict_literal() {
        let (_, diags) = parse_src(r#"var d = {"a": 1, "b": 2, "c": 3}"#);
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn dict_comprehension() {
        let (_, diags) = parse_src("var d = {x: x * 2 for x in [1, 2, 3]}");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Slice expressions ─────────────────────────────────────────────────────

    #[test]
    fn slice_full_range() {
        let (_, diags) = parse_src("var s = xs[1..4]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn slice_from_start() {
        let (_, diags) = parse_src("var s = xs[..3]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn slice_to_end() {
        let (_, diags) = parse_src("var s = xs[2..]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn slice_with_step() {
        let (_, diags) = parse_src("var s = xs[0..10 step 2]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn slice_negative_index() {
        let (_, diags) = parse_src("var last = xs[-1]");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Test blocks ───────────────────────────────────────────────────────────

    #[test]
    fn test_block_empty() {
        let (_, diags) = parse_src(r#"test "empty" {}"#);
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn test_block_with_assert() {
        let (_, diags) = parse_src(
            r#"test "arithmetic" {
                assert(1 + 1 == 2)
                assert_eq(10 - 3, 7)
                assert_ne("a", "b")
            }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn multiple_test_blocks() {
        let (module, diags) = parse_src(
            r#"test "first" { assert(true) }
            test "second" { assert(1 == 1) }
            test "third" { assert_eq(2 + 2, 4) }"#,
        );
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
        // Three test-decl items should be in the module.
        assert_eq!(module.items.len(), 3);
    }

    #[test]
    fn test_block_missing_name_is_recovered() {
        // Should produce an error but not panic.
        let (_, diags) = parse_src("test { assert(true) }");
        assert!(
            !diags.is_empty(),
            "expected a diagnostic for missing test name"
        );
    }

    // ── `use` declarations ───────────────────────────────────────────────────

    #[test]
    fn use_simple() {
        let (_, diags) = parse_src("use std.io");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn use_grouped() {
        let (_, diags) = parse_src("use std.io.{print, readFile}");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn use_alias() {
        let (_, diags) = parse_src("use std.math as math");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn use_keyword_named_module() {
        let (_, diags) = parse_src("use std.parallel");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn use_grouped_from_keyword_named_module() {
        let (_, diags) = parse_src("use std.parallel.{parallelMap, parallelReduce}");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn use_grouped_operator_named_export() {
        let (_, diags) = parse_src("use std.math.{pow}");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn field_call_operator_named_export() {
        let (_, diags) = parse_src("var x = math.pow(2, 3)");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn field_call_keyword_named_export() {
        let (_, diags) = parse_src("var x = regex.test(\"a\", \"a\")");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    // ── Range expressions ────────────────────────────────────────────────────

    #[test]
    fn exclusive_range() {
        let (_, diags) = parse_src("for i in 0..10 { print(i) }");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }

    #[test]
    fn inclusive_range() {
        let (_, diags) = parse_src("for i in 1...5 { print(i) }");
        assert!(
            errors(&diags).is_empty(),
            "unexpected errors: {:?}",
            errors(&diags)
        );
    }
}
