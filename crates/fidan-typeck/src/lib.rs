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
    fn extern_native_rejects_more_than_four_params() {
        let errors = check_errors(
            r#"@extern("self")
            action tooWide with (
                a oftype integer,
                b oftype integer,
                c oftype integer,
                d oftype integer,
                e oftype integer
            ) returns integer"#,
        );
        assert!(
            errors
                .iter()
                .any(|msg| msg.contains("supports at most 4 parameters")),
            "expected native arity limit error, got {errors:?}"
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
}
