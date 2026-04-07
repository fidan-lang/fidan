//! `fidan-typeck` — Symbol tables, type inference, type checking, parallel safety.
//!
//! # Entry point
//! ```rust,ignore
//! use std::sync::Arc;
//! use fidan_lexer::SymbolInterner;
//!
//! let module: fidan_ast::Module = unimplemented!();
//! let interner = Arc::new(SymbolInterner::new());
//!
//! // Lightweight: returns only diagnostics.
//! let diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
//!
//! // Full: returns type map + diagnostics for HIR lowering.
//! let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
//! ```

mod check;
mod infer;
mod parallel_check;
mod scope;
mod types;

pub use check::{ActionInfo, EnumInfo, ObjectInfo, ParamInfo, TypeChecker};
pub use types::FidanType;

use fidan_ast::{ExprId, Module};
use fidan_diagnostics::Diagnostic;
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::Span;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// A method call whose signature couldn't be verified locally because the
/// method lives in a cross-module parent class.  The LSP validates argument
/// types against the cross-document method signature at analysis time.
#[derive(Debug, Clone)]
pub struct CrossModuleCallSite {
    /// Resolved name of the receiver type, e.g. `"TRex"`.
    pub receiver_ty: String,
    /// Name of the method being called, e.g. `"roar"`.
    pub method_name: String,
    /// Inferred argument types in call order, e.g. `["string"]`.
    pub arg_tys: Vec<String>,
    /// Span of the whole call expression (for diagnostic placement).
    pub span: Span,
}

/// Full type-information produced after a successful type-checking pass.
///
/// Carries everything HIR lowering needs to annotate every node with a
/// concrete type, plus the object/action registry for layout information.
#[derive(Debug)]
pub struct TypedModule {
    /// All diagnostics (errors + warnings) emitted during type-checking.
    pub diagnostics: Vec<Diagnostic>,
    /// Type of every expression, keyed by `ExprId`.
    pub expr_types: FxHashMap<ExprId, FidanType>,
    /// Class/object registry: layout (fields, methods, parent) per class name.
    pub objects: FxHashMap<Symbol, ObjectInfo>,
    /// Enum registry: variants per enum name.
    pub enums: FxHashMap<Symbol, EnumInfo>,
    /// Top-level action signatures (name → signature).
    pub actions: FxHashMap<Symbol, ActionInfo>,
    /// Non-call field / method accesses on types with cross-module parents.
    /// The LSP validates these against cross-document symbol tables.
    pub cross_module_field_accesses: Vec<(String, String, Span)>,
    /// Method call sites where the callee is in a cross-module parent class.
    /// The LSP validates argument types against the cross-document signature.
    pub cross_module_call_sites: Vec<CrossModuleCallSite>,
}

/// Run all type-checking passes over `module` and return the resulting diagnostics.
///
/// Zero diagnostics means the module is well-typed.
pub fn typecheck(module: &Module, interner: Arc<SymbolInterner>) -> Vec<Diagnostic> {
    let mut tc = TypeChecker::new(interner, module.file);
    tc.check_module(module);
    tc.finish()
}

/// Run all type-checking passes and return the full `TypedModule` (type map + diagnostics).
///
/// Use this instead of `typecheck` when you need type annotations on HIR nodes.
pub fn typecheck_full(module: &Module, interner: Arc<SymbolInterner>) -> TypedModule {
    let mut tc = TypeChecker::new(interner, module.file);
    tc.check_module(module);
    tc.finish_typed()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_diagnostics::Severity;
    use fidan_lexer::Lexer;
    use fidan_source::{FileId, SourceFile};

    /// Parse + typecheck `src`, returning only error diagnostics.
    fn check_errors(src: &str) -> Vec<String> {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<test>", src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, _) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        let diags = typecheck(&module, interner);
        diags
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect()
    }

    fn check_warning_codes(src: &str) -> Vec<String> {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<test>", src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, _) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        let diags = typecheck(&module, interner);
        diags
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.code.to_string())
            .collect()
    }

    // ── Well-typed programs produce no errors ─────────────────────────────────

    #[test]
    fn integer_var_is_clean() {
        assert!(check_errors("var x = 42").is_empty());
    }

    #[test]
    fn string_var_is_clean() {
        assert!(check_errors(r#"var s = "hello""#).is_empty());
    }

    #[test]
    fn boolean_var_is_clean() {
        assert!(check_errors("var b = true").is_empty());
    }

    #[test]
    fn action_no_params_is_clean() {
        assert!(check_errors(r#"action greet { print("hi") }"#).is_empty());
    }

    #[test]
    fn action_with_params_is_clean() {
        assert!(check_errors(
            r#"action add with (certain a oftype integer, certain b oftype integer) returns integer {
                return a + b
            }"#
        )
        .is_empty());
    }

    #[test]
    fn explicit_nothing_passed_to_certain_param_is_error() {
        let errors = check_errors(
            r#"action approx_equal with (
                certain a oftype float,
                certain b oftype float,
                optional rel_tol oftype float = 0.0000001,
                optional abs_tol oftype float = 0.0001
            ) returns boolean {
                return true
            }

            approx_equal(nothing, nothing)"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("certain parameter `a` cannot receive `nothing`")),
            "expected certain-param nothing error for `a`, got {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("certain parameter `b` cannot receive `nothing`")),
            "expected certain-param nothing error for `b`, got {errors:?}"
        );
    }

    #[test]
    fn const_nothing_passed_to_certain_param_is_error() {
        let errors = check_errors(
            r#"action approx_equal with (certain a oftype float, certain b oftype float) returns boolean {
                return true
            }

            const var x = nothing
            approx_equal(x, x)"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("certain parameter `a` cannot receive `nothing`")),
            "expected certain-param const-nothing error for `a`, got {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("certain parameter `b` cannot receive `nothing`")),
            "expected certain-param const-nothing error for `b`, got {errors:?}"
        );
    }

    #[test]
    fn user_action_argument_type_mismatch_is_error() {
        let errors = check_errors(
            r#"action add with (certain a oftype integer, certain b oftype integer) returns integer {
                return a + b
            }

            add("one", 2)"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("argument `a` expects type `integer`, found `string`")),
            "expected user-action type mismatch, got {errors:?}"
        );
    }

    #[test]
    fn object_constructor_argument_type_mismatch_is_error() {
        let errors = check_errors(
            r#"object Point {
                new with (certain x oftype integer, certain y oftype integer) {
                    var z = x + y
                }
            }

            var p = Point("bad", 2)"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("argument `x` expects type `integer`, found `string`")),
            "expected constructor type mismatch, got {errors:?}"
        );
    }

    #[test]
    fn parent_constructor_calls_remain_valid() {
        let errors = check_errors(
            r#"object Animal {
                var species oftype string

                new with (certain species oftype string) {
                    this.species = species
                }
            }

            object Dog extends Animal {
                new with (certain name oftype string) {
                    parent("Dog")
                }
            }

            var dog = Dog("Fido")"#,
        );
        assert!(
            errors.is_empty(),
            "expected parent constructor call to remain valid, got {errors:?}"
        );
    }

    #[test]
    fn explicit_constructor_new_call_remains_valid() {
        let errors = check_errors(
            r#"object Dog {
                new with (certain name oftype string) {
                    print(name)
                }
            }

            var dog = Dog.new("Fido")"#,
        );
        assert!(
            errors.is_empty(),
            "expected explicit constructor call to remain valid, got {errors:?}"
        );
    }

    #[test]
    fn enum_unit_variants_remain_accessible_via_type_path() {
        let errors = check_errors(
            r#"enum Direction {
                North
                South
            }

            var direction = Direction.North
            var same = direction == Direction.South"#,
        );
        assert!(
            errors.is_empty(),
            "expected enum unit variants to remain accessible, got {errors:?}"
        );
    }

    #[test]
    fn object_field_with_unknown_type_is_error() {
        let errors = check_errors(
            r#"object Broken {
                var value oftype MissingType
            }"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("undefined type `MissingType`")),
            "expected unknown object field type error, got {errors:?}"
        );
    }

    #[test]
    fn invalid_string_method_is_error() {
        let errors = check_errors(r#"var result = "somestring".filter()"#);
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("type `string` has no method `filter`")),
            "expected invalid string method error, got {errors:?}"
        );
    }

    #[test]
    fn invalid_integer_method_is_error() {
        let errors = check_errors("var x = 2.nonexistent()");
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("type `integer` has no method `nonexistent`")),
            "expected invalid integer method error, got {errors:?}"
        );
    }

    #[test]
    fn invalid_nothing_method_is_error() {
        let errors = check_errors("print().print()");
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("type `nothing` has no method `print`")),
            "expected invalid nothing method error, got {errors:?}"
        );
    }

    #[test]
    fn valid_string_method_remains_clean() {
        assert!(check_errors(r#"var size = "hello".len()"#).is_empty());
    }

    #[test]
    fn integer_literal_is_not_callable() {
        let errors = check_errors("var x = 1()");
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("type `integer` is not callable")),
            "expected integer not callable error, got {errors:?}"
        );
    }

    #[test]
    fn nothing_is_not_callable() {
        let errors = check_errors("nothing()()()");
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("type `nothing` is not callable")),
            "expected nothing not callable error, got {errors:?}"
        );
    }

    #[test]
    fn builtin_return_value_is_not_callable_twice() {
        let errors = check_errors("print()()");
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("type `nothing` is not callable")),
            "expected builtin return value not callable error, got {errors:?}"
        );
    }

    #[test]
    fn unimported_stdlib_free_functions_still_error() {
        let errors = check_errors(
            r#"sqrt(4)
            readFile("x")"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("undefined name `sqrt`")),
            "expected undefined sqrt error, got {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("undefined name `readFile`")),
            "expected undefined readFile error, got {errors:?}"
        );
    }

    #[test]
    fn imported_stdlib_free_function_keeps_return_metadata() {
        assert!(
            check_errors(
                r#"use std.math.sqrt
            var root = sqrt(4)"#,
            )
            .is_empty()
        );
    }

    #[test]
    fn if_otherwise_is_clean() {
        assert!(
            check_errors(
                r#"var x = 5
            if x > 0 { print("pos") } otherwise { print("neg") }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn for_loop_is_clean() {
        assert!(check_errors("for i in [1, 2, 3] { print(i) }").is_empty());
    }

    #[test]
    fn list_comprehension_is_clean() {
        assert!(check_errors("var evens = [x for x in [1, 2, 3, 4] if x % 2 == 0]").is_empty());
    }

    #[test]
    fn test_block_is_clean() {
        assert!(
            check_errors(
                r#"test "math" {
                assert(1 + 1 == 2)
                assert_eq(10 - 3, 7)
            }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn assert_builtins_are_registered() {
        // All three assert builtins must be callable with no type errors.
        assert!(
            check_errors(
                r#"test "assertions" {
                assert(true)
                assert_eq(1, 1)
                assert_ne("a", "b")
            }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn object_declaration_is_clean() {
        assert!(
            check_errors(
                r#"object Point {
                var x oftype float
                var y oftype float
            }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn extern_native_action_is_clean() {
        assert!(
            check_errors(
                r#"@extern("self", symbol = "native_add")
                action nativeAdd with (a oftype integer, b oftype integer) returns integer"#
            )
            .is_empty()
        );
    }

    #[test]
    fn extern_fidan_requires_unsafe() {
        let errors = check_errors(
            r#"@extern("self", abi = "fidan")
            action echo with (text oftype string) returns string"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("requires the @unsafe decorator")),
            "expected missing @unsafe error, got {errors:?}"
        );
    }

    #[test]
    fn extern_native_rejects_string_params() {
        let errors = check_errors(
            r#"@extern("self")
            action bad with (text oftype string) returns integer"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("unsupported type `string`")),
            "expected unsupported native type error, got {errors:?}"
        );
    }

    #[test]
    fn extern_body_is_rejected() {
        let errors = check_errors(
            r#"@extern("self")
            action bad returns integer {
                return 1
            }"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("must omit their body")),
            "expected bodyless extern error, got {errors:?}"
        );
    }

    #[test]
    fn extern_native_accepts_more_than_four_params() {
        assert!(
            check_errors(
                r#"@extern("self")
            action wide with (
                a oftype integer,
                b oftype integer,
                c oftype integer,
                d oftype integer,
                e oftype integer,
                f oftype integer
            ) returns integer"#
            )
            .is_empty()
        );
    }

    #[test]
    fn extern_invalid_abi_value_is_rejected() {
        let errors = check_errors(
            r#"@extern("self", abi = "mystery")
            action bad with (a oftype integer) returns integer"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("must be either \"native\" or \"fidan\"")),
            "expected invalid abi error, got {errors:?}"
        );
    }

    #[test]
    fn extern_link_must_be_string_literal() {
        let errors = check_errors(
            r#"@extern("self", link = 123)
            action bad with (a oftype integer) returns integer"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("`link` must be a string literal")),
            "expected invalid link literal error, got {errors:?}"
        );
    }

    #[test]
    fn nested_action_decorators_are_clean() {
        assert!(
            check_errors(
                r#"action decorate with (target oftype dynamic, label oftype string) {
            }

            action main {
                @precompile
                @decorate("local")
                action helper with (certain value oftype integer) returns integer {
                    return value + 1
                }

                assert_eq(helper(4), 5)
            }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn nested_extern_action_is_clean() {
        assert!(
            check_errors(
                r#"action main {
                @extern("self", symbol = "native_add")
                action nativeAdd with (a oftype integer, b oftype integer) returns integer
            }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn nested_deprecated_action_warns_at_call_site() {
        let warnings = check_warning_codes(
            r#"action main {
                @deprecated
                action old_helper returns integer {
                    return 1
                }

                old_helper()
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W2005"),
            "expected W2005, got {warnings:?}"
        );
    }

    #[test]
    fn builtin_type_name_cannot_be_shadowed_by_var() {
        let errors = check_errors("const var integer -> integer set 1");
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("reserved builtin name `integer`")),
            "expected reserved builtin var error, got {errors:?}"
        );
    }

    #[test]
    fn builtin_type_name_cannot_be_used_as_param() {
        let errors = check_errors(
            r#"action bad with (integer oftype integer) returns integer {
                return integer
            }"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("reserved builtin name `integer`")),
            "expected reserved builtin param error, got {errors:?}"
        );
    }

    #[test]
    fn builtin_type_name_cannot_be_used_as_loop_binding() {
        let errors = check_errors(
            r#"for integer in [1, 2, 3] {
                print(integer)
            }"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("reserved builtin name `integer`")),
            "expected reserved builtin loop-binding error, got {errors:?}"
        );
    }

    #[test]
    fn handle_var_shadowing_remains_legal() {
        assert!(check_errors("var handle = 1").is_empty());
    }

    // ── Parallel-safety diagnostics ───────────────────────────────────────────

    #[test]
    fn parallel_action_is_clean() {
        assert!(
            check_errors(
                r#"parallel action compute returns integer {
                var n = 42
                return n
            }"#
            )
            .is_empty()
        );
    }

    #[test]
    fn warns_on_statement_after_return() {
        let warnings = check_warning_codes(
            r#"action sum returns integer {
                return 1
                print("dead")
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_statement_after_panic() {
        let warnings = check_warning_codes(
            r#"action blow_up {
                panic("boom")
                print("dead")
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_after_fully_terminating_if_chain() {
        let warnings = check_warning_codes(
            r#"action choose with (certain flag oftype boolean) returns integer {
                if flag {
                    return 1
                } otherwise {
                    return 2
                }
                print("dead")
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_if_false_branch_body() {
        let warnings = check_warning_codes(
            r#"action main {
                if false {
                    print("dead")
                }
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_else_after_if_true() {
        let warnings = check_warning_codes(
            r#"action main {
                if true {
                    print("live")
                } otherwise {
                    print("dead")
                }
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_while_false_body() {
        let warnings = check_warning_codes(
            r#"action main {
                while false {
                    print("dead")
                }
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_const_false_identifier_condition() {
        let warnings = check_warning_codes(
            r#"action main {
                const var x = false
                if x {
                    print("dead")
                }
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_constant_comparison_condition() {
        let warnings = check_warning_codes(
            r#"action main {
                if 1 > 2 {
                    print("dead")
                }
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }

    #[test]
    fn warns_on_const_numeric_comparison_condition() {
        let warnings = check_warning_codes(
            r#"action main {
                const var limit = 1
                if limit > 2 {
                    print("dead")
                }
            }"#,
        );
        assert!(
            warnings.iter().any(|code| code == "W1006"),
            "expected W1006, got {warnings:?}"
        );
    }
}
