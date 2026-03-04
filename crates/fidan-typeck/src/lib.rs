//! `fidan-typeck` — Symbol tables, type inference, type checking, parallel safety.
//!
//! # Entry point
//! ```no_run
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

mod scope;
mod types;
mod infer;
mod check;
mod parallel_check;

pub use types::FidanType;
pub use check::{TypeChecker, ObjectInfo, ActionInfo, ParamInfo};

use fidan_ast::{ExprId, Module};
use fidan_diagnostics::Diagnostic;
use fidan_lexer::{Symbol, SymbolInterner};
use rustc_hash::FxHashMap;
use std::sync::Arc;

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
    /// Top-level action signatures (name → signature).
    pub actions: FxHashMap<Symbol, ActionInfo>,
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
        assert!(check_errors(
            r#"var x = 5
            if x > 0 { print("pos") } otherwise { print("neg") }"#
        )
        .is_empty());
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
        assert!(check_errors(
            r#"test "math" {
                assert(1 + 1 == 2)
                assert_eq(10 - 3, 7)
            }"#
        )
        .is_empty());
    }

    #[test]
    fn assert_builtins_are_registered() {
        // All three assert builtins must be callable with no type errors.
        assert!(check_errors(
            r#"test "assertions" {
                assert(true)
                assert_eq(1, 1)
                assert_ne("a", "b")
            }"#
        )
        .is_empty());
    }

    #[test]
    fn object_declaration_is_clean() {
        assert!(check_errors(
            r#"object Point {
                var x oftype float
                var y oftype float
            }"#
        )
        .is_empty());
    }

    // ── Parallel-safety diagnostics ───────────────────────────────────────────

    #[test]
    fn parallel_action_is_clean() {
        assert!(check_errors(
            r#"parallel action compute returns integer {
                var n = 42
                return n
            }"#
        )
        .is_empty());
    }
}
