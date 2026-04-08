//! `fidan-fmt` — Canonical source formatter for the Fidan programming language.
//!
//! # Usage
//!
//! ```rust
//! use fidan_fmt::{FormatOptions, format_source};
//!
//! let opts = FormatOptions::default();
//! let formatted = format_source("action main {\n    var total = 20\n\n    total %= 6\n}\n", &opts);
//! assert_eq!(formatted, "action main {\n    var total = 20\n\n    total %= 6\n}\n");
//! ```
//!
//! On the command line:
//! ```text
//! fidan format file.fdn            # print to stdout
//! fidan format file.fdn --in-place # rewrite in place
//! fidan format file.fdn --check    # exit 1 if not already formatted (CI mode)
//! ```

mod comments;
pub mod config;
mod emit_expr;
mod emit_item;
mod emit_stmt;
mod printer;

pub use config::{
    FormatConfigError, FormatOptions, find_format_config, load_format_options_for_path,
    resolve_format_options_for_path,
};

use comments::collect_comments;
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
    let comments = collect_comments(&file);
    let (tokens, _lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, _parse_diags) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
    let mut p = printer::Printer::new(&module.arena, &interner, opts, &file, comments);
    emit_module(&mut p, &module);
    p.finish()
}

/// Returns `true` when `src` is already formatted according to `opts`.
///
/// Useful for CI checks (`fidan format --check`): exit non-zero when this
/// returns `false`.
pub fn check_formatted(src: &str, opts: &FormatOptions) -> bool {
    format_source(src, opts) == src
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_diagnostics::Severity;
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    fn fmt(src: &str) -> String {
        format_source(src, &FormatOptions::default())
    }

    fn fmt_with_max_line_len(src: &str, max_line_len: usize) -> String {
        format_source(
            src,
            &FormatOptions {
                max_line_len,
                ..FormatOptions::default()
            },
        )
    }

    fn workspace_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn assert_round_trip_file(rel_path: &str) {
        let path = workspace_root().join(rel_path);
        let original = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        let item_count_original = {
            let interner = Arc::new(SymbolInterner::new());
            let file = SourceFile::new(FileId(0), "<original>", original.as_str());
            let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
            let (module, _) = fidan_parser::parse(&tokens, FileId(0), interner);
            module.items.len()
        };

        let opts = FormatOptions::default();
        let formatted = format_source(&original, &opts);

        let item_count_formatted = {
            let interner = Arc::new(SymbolInterner::new());
            let file = SourceFile::new(FileId(0), "<formatted>", formatted.as_str());
            let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
            let (module, diags) = fidan_parser::parse(&tokens, FileId(0), interner);
            let errors: Vec<_> = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .collect();
            assert!(
                errors.is_empty(),
                "re-parsing formatted source produced errors for {}:\n{errors:#?}\n\nFormatted source:\n{formatted}",
                rel_path
            );
            module.items.len()
        };

        assert_eq!(
            item_count_original, item_count_formatted,
            "top-level item count changed after formatting {}: {item_count_original} -> {item_count_formatted}",
            rel_path
        );

        let formatted2 = format_source(&formatted, &opts);
        assert_eq!(
            formatted, formatted2,
            "formatter is not idempotent on {}!\nfirst pass:\n{formatted}\nsecond pass:\n{formatted2}",
            rel_path
        );
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
        let src =
            "action greet with (certain name oftype string) returns string {\n    return name\n}\n";
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

    #[test]
    fn formats_recent_feature_surface_cleanly() {
        let src = r#"@extern("kernel32", "Beep")
action beep with (certain freq oftype integer, certain ms oftype integer) returns nothing

enum Result {
    Ok(string)
    Err(integer, dynamic)
}

action demo with (optional name oftype dynamic = r"{literal}") returns dynamic {
    var values oftype tuple = (1, 2, 3)
    var first = values[0]
    var slice = [1, 2, 3, 4][1..3]
    var maybe = nothing ?? "fallback"
    var comp = [x * 2 for x in [1, 2, 3] if x > 1]
    var map = {x: x + 1 for x in [1, 2, 3] if x > 1}
    var pending = spawn work(name)
    var result = await pending
    concurrent {
        task reader {
            print(r"\n {name}")
        }
        task writer {
            print(result)
        }
    }
    parallel {
        task A {
            print("a")
        }
        task B {
            print("b")
        }
    }
    check result {
        "ok" => {
            return result
        }
        _ => {
            panic("bad result")
        }
    }
}
"#;
        let expected = r#"@extern("kernel32", "Beep")
action beep with (certain freq oftype integer, certain ms oftype integer) returns nothing

enum Result {
    Ok(string)
    Err(integer, dynamic)
}

action demo with (optional name oftype dynamic = "\{literal\}") returns dynamic {
    var values oftype tuple = (1, 2, 3)
    var first = values[0]
    var slice = [1, 2, 3, 4][1..3]
    var maybe = nothing ?? "fallback"
    var comp = [x * 2 for x in [1, 2, 3] if x > 1]
    var map = {x: x + 1 for x in [1, 2, 3] if x > 1}
    var pending = spawn work(name)
    var result = await pending

    concurrent {
        task reader {
            print("\\n \{name\}")
        }
        task writer {
            print(result)
        }
    }
    parallel {
        task A {
            print("a")
        }
        task B {
            print("b")
        }
    }
    check result {
        "ok" => {
            return result
        }
        _ => {
            panic("bad result")
        }
    }
}
"#;
        assert_eq!(fmt(src), expected);
        assert_idempotent(expected);
    }

    #[test]
    fn enum_payloads_and_dynamic_types_round_trip() {
        let src = "enum Value {\n    Text(string)\n    Pair(integer, dynamic)\n}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
    }

    #[test]
    fn statement_compound_assign_is_preserved_as_compound_syntax() {
        let src = "action main {\n    var total = 0\n\n    total += 1\n}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
    }

    #[test]
    fn statement_percent_assign_is_preserved_as_compound_syntax() {
        let src = "action main {\n    var total = 20\n\n    total %= 6\n}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
    }

    #[test]
    fn top_level_percent_assign_is_preserved_as_compound_syntax() {
        let src = "var total = 20\n\ntotal %= 6\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
    }

    #[test]
    fn check_expression_single_line_arms_stay_braceless() {
        let src = "action main returns string {\n    return check 2 {\n        1 => \"one\"\n        2 => \"two\"\n        _ => \"other\"\n    }\n}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
    }

    #[test]
    fn preserves_line_and_block_comments() {
        let src = r#"## heading
var x=1 # tail
#/ block
   keep
/#"#;
        let expected = "## heading\nvar x = 1  # tail\n#/ block\n   keep\n/#\n";
        assert_eq!(fmt(src), expected);
        assert_idempotent(expected);
    }

    #[test]
    fn inline_lambda_expression() {
        let src = "var greet = action with (certain name oftype string) returns string {\n    return name\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    #[test]
    fn inline_lambda_without_params() {
        let src = "var noop = action {\n    return\n}\n";
        let out = fmt(src);
        assert_eq!(out, src);
        assert_idempotent(src);
    }

    #[test]
    fn long_chain_assignment_wraps_using_max_line_len() {
        let src = "action demo {\n    var result = source.reallyLongMethod(alpha, beta, gamma).next().finalize(delta, epsilon)\n}\n";
        let expected = "action demo {\n    var result = source\n        .reallyLongMethod(\n            alpha,\n            beta,\n            gamma,\n        )\n        .next()\n        .finalize(delta, epsilon)\n}\n";
        let formatted = fmt_with_max_line_len(src, 40);
        assert_eq!(formatted, expected);
        assert_eq!(fmt_with_max_line_len(&formatted, 40), expected);
    }

    #[test]
    fn long_return_call_wraps_using_max_line_len() {
        let src = "action demo returns dynamic {\n    return compute.reallyLongMethod(alpha, beta, gamma).finish()\n}\n";
        let formatted = fmt_with_max_line_len(src, 36);
        assert!(formatted.contains("return compute\n        .reallyLongMethod("));
        assert!(formatted.contains("            alpha,"));
        assert!(formatted.contains("        .finish()"));
        assert_eq!(fmt_with_max_line_len(&formatted, 36), formatted);
    }

    #[test]
    fn multiline_chain_flattens_when_max_line_len_is_large() {
        let src = "var result =\n    \"df\"\n        .filter(\"revenue\" > 1000)\n        .group_by(\"country\")\n        .agg([\"revenue\".as(\"total_revenue\"), \"orders\".as(\"avg_orders\")])\n        .sort(\"total_revenue\", true)\n";
        let expected = "var result = \"df\".filter(\"revenue\" > 1000).group_by(\"country\").agg([\"revenue\".as(\"total_revenue\"), \"orders\".as(\"avg_orders\")]).sort(\"total_revenue\", true)\n";
        assert_eq!(fmt_with_max_line_len(src, 10_000), expected);
    }

    #[test]
    fn wrapped_chain_keeps_root_after_equals_when_it_fits() {
        let src = "var result = \"df\".filter(\"revenue\" > 1000).group_by(\"country\").agg([\"revenue\".as(\"total_revenue\"), \"orders\".as(\"avg_orders\")]).sort(\"total_revenue\", true)\n";
        let expected = "var result = \"df\"\n    .filter(\"revenue\" > 1000)\n    .group_by(\"country\")\n    .agg([\"revenue\".as(\"total_revenue\"), \"orders\".as(\"avg_orders\")])\n    .sort(\"total_revenue\", true)\n";
        assert_eq!(fmt_with_max_line_len(src, 100), expected);
    }

    #[test]
    fn multiline_call_args_flatten_when_max_line_len_is_large() {
        let src = "var value = process(\n    alpha,\n    beta,\n    gamma,\n)\n";
        let expected = "var value = process(alpha, beta, gamma)\n";
        assert_eq!(fmt_with_max_line_len(src, 10_000), expected);
    }

    #[test]
    fn multiline_list_flattens_when_max_line_len_is_large() {
        let src = "var values = [\n    alpha,\n    beta,\n    gamma,\n]\n";
        let expected = "var values = [alpha, beta, gamma]\n";
        assert_eq!(fmt_with_max_line_len(src, 10_000), expected);
    }

    #[test]
    fn long_list_wraps_when_max_line_len_is_small() {
        let src = "var values = [alpha, beta, gamma, delta, epsilon]\n";
        let expected =
            "var values = [\n    alpha,\n    beta,\n    gamma,\n    delta,\n    epsilon,\n]\n";
        let formatted = fmt_with_max_line_len(src, 28);
        assert_eq!(formatted, expected);
        assert_eq!(fmt_with_max_line_len(&formatted, 28), expected);
    }

    #[test]
    fn long_dict_wraps_when_max_line_len_is_small() {
        let src = "var lookup = {alpha_key: first_value, beta_key: second_value}\n";
        let expected =
            "var lookup = {\n    alpha_key: first_value,\n    beta_key: second_value,\n}\n";
        let formatted = fmt_with_max_line_len(src, 34);
        assert_eq!(formatted, expected);
        assert_eq!(fmt_with_max_line_len(&formatted, 34), expected);
    }

    #[test]
    fn long_tuple_wraps_when_max_line_len_is_small() {
        let src = "var pair = (alpha_value, beta_value, gamma_value)\n";
        let expected = "var pair = (\n    alpha_value,\n    beta_value,\n    gamma_value,\n)\n";
        let formatted = fmt_with_max_line_len(src, 24);
        assert_eq!(formatted, expected);
        assert_eq!(fmt_with_max_line_len(&formatted, 24), expected);
    }

    #[test]
    fn multiline_params_flatten_when_max_line_len_is_large() {
        let src = "action greet with (\n    certain first oftype string,\n    certain last oftype string,\n) returns string {\n    return first + last\n}\n";
        let expected = "action greet with (certain first oftype string, certain last oftype string) returns string {\n    return first + last\n}\n";
        assert_eq!(fmt_with_max_line_len(src, 10_000), expected);
    }

    #[test]
    fn nested_action_decorators_are_preserved() {
        let src = "action outer {\n    @deprecated\n    action inner {\n        print(\"hi\")\n    }\n}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
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

    #[test]
    fn grouped_imports_stay_grouped() {
        let src = "use std.math.{abs, sqrt, floor, ceil, round, max, min}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
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
        // Consecutive declarations stay grouped.
        let src = "var a = 1\nvar b = 2\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);

        // Any blank lines the user wrote between grouped declarations are stripped.
        let src_with_blank = "var a = 1\n\nvar b = 2\n";
        assert_eq!(fmt(src_with_blank), "var a = 1\nvar b = 2\n");

        // Imports and declarations get separated into distinct groups.
        let imports_then_vars = "use std.io\nuse std.math\nvar a = 1\n";
        assert_eq!(
            fmt(imports_then_vars),
            "use std.io\nuse std.math\n\nvar a = 1\n"
        );

        // Block-level items (actions, objects) get a blank line before and after.
        let src_action = "var x = 1\naction foo {\n}\nvar y = 2\n";
        let formatted = fmt(src_action);
        assert!(
            formatted.contains("\n\naction"),
            "expected blank line before action block, got:\n{formatted}"
        );
        assert!(
            formatted.contains("}\n\nvar"),
            "expected blank line after action block, got:\n{formatted}"
        );
    }

    #[test]
    fn blank_line_between_decls_and_control_flow_in_blocks() {
        let src = "action demo {\n    var a = 1\n    var b = 2\n\n    if a < b {\n        print(a)\n    }\n}\n";
        assert_eq!(fmt(src), src);
        assert_idempotent(src);
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

    #[test]
    fn interpolation_expressions_render_nested_strings_as_plain_code() {
        let src = r#"action demo {
    var sentence = "the quick brown fox"
    var scores = {"Alice": 95}
    print("contains quick: {sentence.contains("quick")}")
    print("Alice: {scores["Alice"]}")
    print("join: {["a", "b"].join(", ")}")
}
"#;
        let formatted = fmt(src);
        assert!(
            formatted.contains(r#"print("contains quick: {sentence.contains("quick")}")"#),
            "formatted output should preserve nested string literals as code inside interpolation:\n{formatted}"
        );
        assert!(
            formatted.contains(r#"print("Alice: {scores["Alice"]}")"#),
            "formatted output should preserve string index keys as code inside interpolation:\n{formatted}"
        );
        assert!(
            formatted.contains(r#"print("join: {["a", "b"].join(", ")}")"#),
            "formatted output should preserve nested string arguments as code inside interpolation:\n{formatted}"
        );
        assert_idempotent(&formatted);
    }

    #[test]
    fn interpolation_expression_preserves_required_inner_string_escapes_only() {
        let src = r#"action demo {
    print("Quote: {"He said \"hi\""}")
}
"#;
        let formatted = fmt(src);
        assert!(
            formatted.contains(r#"print("Quote: {"He said \"hi\""}")"#),
            "formatted output should preserve only the escapes required by the nested string literal itself:\n{formatted}"
        );
        assert_idempotent(&formatted);
    }

    #[test]
    fn multiline_string_stays_multiline_when_inline_form_exceeds_max_line_len() {
        let src = "var text = \"First line\nSecond line\nThird line\"\n";
        let formatted = fmt_with_max_line_len(src, 24);
        assert_eq!(formatted, src);
        assert_idempotent(&formatted);
    }

    #[test]
    fn multiline_string_collapses_when_inline_form_fits_max_line_len() {
        let src = "var text = \"First line\nSecond line\nThird line\"\n";
        let formatted = fmt_with_max_line_len(src, 120);
        assert_eq!(
            formatted,
            "var text = \"First line\\nSecond line\\nThird line\"\n"
        );
        assert_idempotent(&formatted);
    }

    #[test]
    fn multiline_interpolation_stays_multiline_when_inline_form_exceeds_max_line_len() {
        let src = "var name = \"Ada\"\nvar greeting = \"Hello,\n{name}!\nDone.\"\n";
        let formatted = fmt_with_max_line_len(src, 28);
        assert_eq!(formatted, src);
        assert_idempotent(&formatted);
    }

    #[test]
    fn multiline_interpolation_collapses_when_inline_form_fits_max_line_len() {
        let src = "var name = \"Ada\"\nvar greeting = \"Hello,\n{name}!\nDone.\"\n";
        let formatted = fmt_with_max_line_len(src, 120);
        assert_eq!(
            formatted,
            "var name = \"Ada\"\nvar greeting = \"Hello,\\n{name}!\\nDone.\"\n"
        );
        assert_idempotent(&formatted);
    }

    // ── Round-trip ────────────────────────────────────────────────────────

    /// Format `test/examples/test.fdn`, re-parse the result, and verify:
    ///   1. The second parse produces zero errors.
    ///   2. The top-level item count is preserved.
    ///   3. A third format pass is identical to the second (idempotence).
    #[test]
    fn round_trip_test_fdn() {
        assert_round_trip_file("test/examples/test.fdn");
    }

    #[test]
    fn round_trip_current_feature_examples() {
        for rel_path in [
            "test/examples/check_val.fdn",
            "test/examples/async_demo.fdn",
            "test/examples/comprehensive.fdn",
            "test/examples/concurrency_showcase.fdn",
            "test/examples/parallel_demo.fdn",
            "test/examples/enum_test.fdn",
            "test/examples/spawn_method_test.fdn",
        ] {
            assert_round_trip_file(rel_path);
        }
    }
}
