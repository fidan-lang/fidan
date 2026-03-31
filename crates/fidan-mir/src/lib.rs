//! `fidan-mir` — SSA/CFG Mid-Level IR types and HIR→MIR lowering.

mod analysis;
mod display;
mod lower;
mod mir;

pub use analysis::{collect_effective_local_types, collect_may_throw_functions};
pub use display::print_program;
pub use lower::lower_program;
pub use mir::{
    BasicBlock, BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirEnumInfo, MirExternAbi,
    MirExternDecl, MirFunction, MirGlobal, MirLit, MirObjectInfo, MirParam, MirProgram,
    MirStringPart, MirTy, MirUseDecl, Operand, PhiNode, Rvalue, Terminator,
};

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    /// Full pipeline: source → tokens → AST → TypedModule → HIR → MIR.
    fn lower(src: &str) -> MirProgram {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<test>", src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, _) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        let hir = fidan_hir::lower_module(&module, &typed, &interner);
        lower_program(&hir, &interner, &[])
    }

    // ── 1. Empty program has only the init function ───────────────────────────

    #[test]
    fn empty_program_has_single_init_fn() {
        let mir = lower("");
        // Exactly one function: FunctionId(0), the top-level init function.
        assert_eq!(mir.functions.len(), 1, "init fn only");
        assert!(mir.objects.is_empty(), "no objects");
        assert!(mir.globals.is_empty(), "no globals");
        assert!(mir.test_functions.is_empty(), "no test fns");
    }

    // ── 2. Top-level action adds a MIR function ───────────────────────────────

    #[test]
    fn action_declaration_adds_mir_function() {
        let mir = lower("action greet with (name oftype string) { return name }");
        // init fn (0) + greet fn (1)
        assert_eq!(mir.functions.len(), 2, "init + greet");
    }

    // ── 3. Test block lands in test_functions ─────────────────────────────────

    #[test]
    fn test_block_registered_in_test_functions() {
        let mir = lower(r#"test "addition" { var x = 1 + 2 }"#);
        assert_eq!(mir.test_functions.len(), 1, "one test function");
        assert_eq!(mir.test_functions[0].0, "addition", "correct test name");
    }

    // ── 4. Multiple test blocks all registered ────────────────────────────────

    #[test]
    fn multiple_test_blocks_all_registered() {
        let mir = lower(r#"test "t1" { } test "t2" { } test "t3" { }"#);
        assert_eq!(mir.test_functions.len(), 3, "three test functions");
    }

    // ── 5. Top-level var registers a global ───────────────────────────────────

    #[test]
    fn top_level_var_creates_mir_global() {
        let mir = lower("var answer = 42");
        assert_eq!(mir.globals.len(), 1, "one global");
    }

    // ── 6. Object type registers MIR object metadata ─────────────────────────

    #[test]
    fn object_declaration_registers_mir_object() {
        let mir = lower("object Point {\n var x oftype integer\n var y oftype integer\n}");
        assert_eq!(mir.objects.len(), 1, "one object");
        assert_eq!(mir.objects[0].field_names.len(), 2, "two fields");
    }

    // ── 7. Two actions → three functions total (init + 2) ────────────────────

    #[test]
    fn two_actions_produce_three_mir_functions() {
        let mir = lower(
            "action add with (a oftype integer, b oftype integer) { return a + b }\n\
             action sub with (a oftype integer, b oftype integer) { return a - b }",
        );
        assert_eq!(mir.functions.len(), 3, "init + add + sub");
    }

    #[test]
    fn concurrent_block_lowers_to_spawn_concurrent() {
        let mir = lower("concurrent { task { print(\"a\") } }");
        let init = &mir.functions[0];
        assert!(
            init.blocks
                .iter()
                .flat_map(|bb| &bb.instructions)
                .any(|instr| matches!(instr, Instr::SpawnConcurrent { .. })),
            "expected SpawnConcurrent in init function"
        );
        assert!(
            !init
                .blocks
                .iter()
                .flat_map(|bb| &bb.instructions)
                .any(|instr| matches!(instr, Instr::SpawnParallel { .. })),
            "concurrent block must not lower to SpawnParallel"
        );
    }

    #[test]
    fn parallel_block_lowers_to_spawn_parallel() {
        let mir = lower("parallel { task { print(\"a\") } }");
        let init = &mir.functions[0];
        assert!(
            init.blocks
                .iter()
                .flat_map(|bb| &bb.instructions)
                .any(|instr| matches!(instr, Instr::SpawnParallel { .. })),
            "expected SpawnParallel in init function"
        );
        assert!(
            !init
                .blocks
                .iter()
                .flat_map(|bb| &bb.instructions)
                .any(|instr| matches!(instr, Instr::SpawnConcurrent { .. })),
            "parallel block must not lower to SpawnConcurrent"
        );
    }

    #[test]
    fn pure_direct_functions_are_marked_non_throwing() {
        let mir = lower(
            "action step with (x oftype integer) returns integer { return x + 1 }\n\
             action main returns integer { return step(41) }",
        );
        let throw_map = collect_may_throw_functions(&mir);
        assert_eq!(throw_map.get(&FunctionId(1)), Some(&false));
        assert_eq!(throw_map.get(&FunctionId(2)), Some(&false));
    }

    #[test]
    fn throwing_functions_are_marked_throwing() {
        let mir = lower(
            "action boom { panic(\"nope\") }\n\
             action main { boom() }",
        );
        let throw_map = collect_may_throw_functions(&mir);
        assert_eq!(throw_map.get(&FunctionId(1)), Some(&true));
        assert_eq!(throw_map.get(&FunctionId(2)), Some(&true));
    }

    #[test]
    fn namespace_method_calls_use_namespace_literals() {
        let mir = lower(
            "use std.math\n\
             action main returns float { return math.sqrt(9) }",
        );
        let main = &mir.functions[1];
        assert!(
            main.blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instr| matches!(
                    instr,
                    Instr::Call {
                        callee: Callee::Method {
                            receiver: Operand::Const(MirLit::Namespace(namespace)),
                            ..
                        },
                        ..
                    } if namespace == "math"
                )),
            "expected stdlib namespace method call to lower as a namespace literal receiver"
        );
    }
}
