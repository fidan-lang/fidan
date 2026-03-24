//! `fidan-hir` — High-Level IR types and AST→HIR lowering.

mod hir;
mod lower;

pub use hir::{
    CustomDecorator, DecoratorArg, HirArg, HirCatchClause, HirCheckArm, HirCheckExprArm, HirElseIf,
    HirEnum, HirExpr, HirExprKind, HirExternAbi, HirExternDecl, HirField, HirFunction, HirGlobal,
    HirInterpPart, HirModule, HirObject, HirParam, HirStmt, HirTask, HirTestDecl, HirUseDecl,
};
pub use lower::{lower_module, merge_module};

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    /// Parse + typecheck + lower `src` to HIR.  Returns the module and interner.
    fn lower(src: &str) -> (HirModule, Arc<SymbolInterner>) {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<test>", src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, _) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        let hir = lower_module(&module, &typed, &interner);
        (hir, interner)
    }

    // ── 1. Empty module ───────────────────────────────────────────────────────

    #[test]
    fn empty_module_produces_empty_hir() {
        let (hir, _) = lower("");
        assert!(hir.objects.is_empty(), "no objects");
        assert!(hir.functions.is_empty(), "no functions");
        assert!(hir.init_stmts.is_empty(), "no init stmts");
        assert!(hir.tests.is_empty(), "no tests");
        assert!(hir.use_decls.is_empty(), "no use decls");
    }

    // ── 2. Action lowering ────────────────────────────────────────────────────

    #[test]
    fn action_with_params_produces_hir_function() {
        let (hir, _) =
            lower("action greet with (name oftype string, count oftype integer) { return name }");
        assert_eq!(hir.functions.len(), 1, "exactly one function");
        let f = &hir.functions[0];
        assert_eq!(f.params.len(), 2, "two params");
        assert!(!f.is_parallel, "not parallel");
        assert!(!f.precompile, "no @precompile");
    }

    #[test]
    fn parallel_action_sets_is_parallel() {
        let (hir, _) = lower("parallel action worker with () { }");
        assert_eq!(hir.functions.len(), 1);
        assert!(hir.functions[0].is_parallel, "@parallel flag propagated");
    }

    #[test]
    fn concurrent_and_parallel_blocks_preserve_block_kind() {
        let (hir, _) =
            lower("concurrent { task { print(\"a\") } } parallel { task { print(\"b\") } }");
        assert_eq!(hir.init_stmts.len(), 2);
        match &hir.init_stmts[0] {
            HirStmt::ConcurrentBlock { is_parallel, .. } => assert!(!is_parallel),
            other => panic!("expected concurrent block, got {other:?}"),
        }
        match &hir.init_stmts[1] {
            HirStmt::ConcurrentBlock { is_parallel, .. } => assert!(*is_parallel),
            other => panic!("expected parallel block, got {other:?}"),
        }
    }

    // ── 3. Object lowering ────────────────────────────────────────────────────

    #[test]
    fn object_with_fields_produces_hir_object() {
        let (hir, _) = lower("object Point {\n var x oftype integer\n var y oftype integer\n}");
        assert_eq!(hir.objects.len(), 1, "one object");
        let obj = &hir.objects[0];
        assert_eq!(obj.fields.len(), 2, "two fields");
        assert!(obj.methods.is_empty(), "no inline methods");
        assert!(obj.parent.is_none(), "no parent");
    }

    #[test]
    fn object_with_method_captures_method_in_hir() {
        let src = r#"
object Counter {
    var value oftype integer
    action increment with () {
        set this.value = this.value + 1
    }
}
"#;
        let (hir, _) = lower(src);
        assert_eq!(hir.objects.len(), 1);
        let obj = &hir.objects[0];
        assert_eq!(obj.fields.len(), 1, "one field");
        assert_eq!(obj.methods.len(), 1, "one inline method");
    }

    // ── 4. Test block collection ──────────────────────────────────────────────

    #[test]
    fn test_block_lands_in_tests_vec() {
        let (hir, _) = lower(r#"test "addition works" { var x = 1 + 2 }"#);
        assert_eq!(hir.tests.len(), 1, "one test decl");
        assert_eq!(hir.tests[0].name, "addition works");
        assert!(!hir.tests[0].body.is_empty(), "test body has stmts");
        // Test block must NOT appear as an init_stmt.
        assert!(
            hir.init_stmts.is_empty(),
            "test block not duplicated into init_stmts",
        );
    }

    // ── 5. Top-level var → init_stmt ─────────────────────────────────────────

    #[test]
    fn top_level_var_goes_into_init_stmts() {
        let (hir, _) = lower("var answer = 42");
        assert_eq!(hir.init_stmts.len(), 1, "one init stmt");
        assert!(
            matches!(hir.init_stmts[0], HirStmt::VarDecl { .. }),
            "stmt is VarDecl",
        );
    }

    // ── 6. For loop in init stmts ─────────────────────────────────────────────

    #[test]
    fn for_loop_in_init_produces_hir_for() {
        let (hir, _) = lower("var items = [1, 2, 3]\nfor x in items { }");
        // Two init stmts: the var decl + the for loop.
        assert_eq!(hir.init_stmts.len(), 2);
        assert!(
            matches!(hir.init_stmts[1], HirStmt::For { .. }),
            "second stmt is For",
        );
    }

    // ── 7. Multiple top-level actions ─────────────────────────────────────────

    #[test]
    fn multiple_actions_all_registered() {
        let (hir, _) = lower(
            "action add with (a oftype integer, b oftype integer) { return a + b }\n\
             action sub with (a oftype integer, b oftype integer) { return a - b }",
        );
        assert_eq!(hir.functions.len(), 2, "two functions");
    }
}
