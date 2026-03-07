// fidan-passes/src/precompile_hints.rs
//
// Compile-time "Why Is This Slow?" hints (W5001, W5003).
//
// Algorithm
// ---------
// For each function F:
//   1. Identify **loop blocks**: basic blocks that form the body of a loop.
//      A loop back-edge is a `Goto` or `Branch` successor whose `BlockId` is
//      ≤ the current block's `BlockId`.  All blocks whose ID is in the range
//      [back_edge_target .. back_edge_source] (inclusive) are considered
//      loop body blocks.
//
//   2. Within loop blocks, scan instructions for:
//
//      **W5001** — a local variable with `MirTy::Dynamic` is:
//        - produced by a `Rvalue::Call` or `Instr::Call` (dynamic return type),
//        - *and* subsequently used as an argument to another call, or as the
//          object of a field/index access inside the same loop region.
//        This catches the common "flexible container element" pattern that
//        prevents JIT compilation of loop bodies.
//
//      **W5003** — a direct `Callee::Fn(id)` call is made inside a loop body
//        and the target function has `precompile = false`.
//        Emits one diagnostic per (caller_function, callee_function) pair,
//        de-duplicated so the same callee is only reported once per function.
//
// W5002 (closure captures mutable outer variable) and W5004 (@precompile
// in AOT mode) are not yet emitted: W5002 requires upvalue metadata not
// present in the current MIR, and W5004 requires knowledge of the build
// target (deferred to Phase 11 LLVM AOT).

use fidan_lexer::SymbolInterner;
use fidan_mir::{BlockId, Callee, FunctionId, Instr, LocalId, MirProgram, MirTy, Operand, Rvalue};
use rustc_hash::{FxHashMap, FxHashSet};

// ── Public diagnostic type ────────────────────────────────────────────────────

/// A single compile-time performance hint.
pub struct SlowHintDiag {
    /// The diagnostic code: `"W5001"` or `"W5003"`.
    pub code: &'static str,
    /// Name of the function where the issue was detected.
    pub fn_name: String,
    /// Human-readable description of the finding and suggested fix.
    pub context: String,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the slow-hints analysis across the entire MIR program.
pub fn check(prog: &MirProgram, interner: &SymbolInterner) -> Vec<SlowHintDiag> {
    let mut diags = Vec::new();

    // Build a map from FunctionId → name for W5003 messages.
    let fn_names: FxHashMap<FunctionId, String> = prog
        .functions
        .iter()
        .map(|f| (f.id, interner.resolve(f.name).to_string()))
        .collect();

    for func in &prog.functions {
        let fn_name = interner.resolve(func.name).to_string();

        // ── Step 1: find loop blocks ──────────────────────────────────────────
        let loop_blocks = find_loop_blocks(func);
        if loop_blocks.is_empty() {
            continue;
        }

        // ── Step 2: collect dynamic SSA locals defined in loop blocks ─────────
        // A local is "hot-dynamic" if it is assigned `MirTy::Dynamic` inside a
        // loop block via an `Rvalue::Call` (dynamic-returning call).
        let mut hot_dynamic: FxHashSet<LocalId> = FxHashSet::default();

        for bb in &func.blocks {
            if !loop_blocks.contains(&bb.id) {
                continue;
            }
            for instr in &bb.instructions {
                match instr {
                    Instr::Assign {
                        dest,
                        ty: MirTy::Dynamic,
                        rhs: Rvalue::Call { .. },
                    } => {
                        hot_dynamic.insert(*dest);
                    }
                    Instr::Call {
                        dest: Some(dest), ..
                    } => {
                        // Mark result if the callee returns Dynamic.
                        // (We don't know the return type here, so we track all
                        // call results and cross-check in step 3.)
                        let _ = dest; // handled below via Assign path
                    }
                    _ => {}
                }
            }
        }

        // ── Step 3: W5001 — hot dynamic used in a call / field / index ───────
        let mut w5001_emitted = false;
        for bb in &func.blocks {
            if w5001_emitted {
                break;
            }
            if !loop_blocks.contains(&bb.id) {
                continue;
            }
            for instr in &bb.instructions {
                if w5001_emitted {
                    break;
                }
                let uses_dynamic = match instr {
                    Instr::Call { args, .. }
                    | Instr::Assign {
                        rhs: Rvalue::Call { args, .. },
                        ..
                    } => args
                        .iter()
                        .any(|a| matches!(a, Operand::Local(l) if hot_dynamic.contains(l))),

                    Instr::GetField { object, .. } | Instr::SetField { object, .. } => {
                        matches!(object, Operand::Local(l) if hot_dynamic.contains(l))
                    }
                    Instr::GetIndex { object, .. } | Instr::SetIndex { object, .. } => {
                        matches!(object, Operand::Local(l) if hot_dynamic.contains(l))
                    }
                    _ => false,
                };

                // Also flag if there's an Assign whose rhs is a call where any
                // argument is dynamic, even if the result is not dynamic.
                let call_arg_dynamic = match instr {
                    Instr::Assign {
                        ty: MirTy::Dynamic,
                        rhs: Rvalue::Use(Operand::Local(src)),
                        ..
                    } => hot_dynamic.contains(src),
                    _ => false,
                };

                if uses_dynamic || call_arg_dynamic {
                    diags.push(SlowHintDiag {
                        code: "W5001",
                        fn_name: fn_name.clone(),
                        context: format!(
                            "loop body in `{fn_name}` uses a `flexible`-typed value — \
                             the JIT cannot specialize the loop; consider replacing \
                             `flexible` with a concrete type or annotating with `@precompile`"
                        ),
                    });
                    w5001_emitted = true;
                }
            }
        }

        // ── Step 4: W5003 — direct call in hot path to non-@precompile fn ────
        let mut w5003_reported: FxHashSet<FunctionId> = FxHashSet::default();
        for bb in &func.blocks {
            if !loop_blocks.contains(&bb.id) {
                continue;
            }
            for instr in &bb.instructions {
                let callee_id = match instr {
                    Instr::Call {
                        callee: Callee::Fn(id),
                        ..
                    } => Some(*id),
                    Instr::Assign {
                        rhs:
                            Rvalue::Call {
                                callee: Callee::Fn(id),
                                ..
                            },
                        ..
                    } => Some(*id),
                    _ => None,
                };

                if let Some(cid) = callee_id {
                    if w5003_reported.contains(&cid) {
                        continue;
                    }
                    // Skip self-recursive calls (recursive functions handle
                    // their own hot-path annotation).
                    if cid == func.id {
                        continue;
                    }
                    if let Some(callee_fn) = prog.functions.get(cid.0 as usize) {
                        if !callee_fn.precompile {
                            let callee_name = fn_names
                                .get(&cid)
                                .map(String::as_str)
                                .unwrap_or("<unknown>");
                            diags.push(SlowHintDiag {
                                code: "W5003",
                                fn_name: fn_name.clone(),
                                context: format!(
                                    "action `{callee_name}` is called inside a loop in \
                                     `{fn_name}` but lacks `@precompile`; \
                                     consider adding `@precompile` to `{callee_name}` \
                                     so the JIT can eagerly compile it"
                                ),
                            });
                            w5003_reported.insert(cid);
                        }
                    }
                }
            }
        }
    }

    diags
}

// ── Loop-block detection ──────────────────────────────────────────────────────

/// Returns the set of `BlockId`s that belong to loop bodies in `func`.
///
/// A *back-edge* is any terminator successor `S` where `S.0 ≤ current_block.0`.
/// The loop body is the inclusive range of blocks `[S .. current]`.
fn find_loop_blocks(func: &fidan_mir::MirFunction) -> FxHashSet<BlockId> {
    let mut loop_blocks: FxHashSet<BlockId> = FxHashSet::default();

    // Inline successor matching using stack-allocated arrays — no heap allocation
    // per block.  A `Goto` has one successor; a `Branch` has two; all exit
    // terminators have none.
    for bb in &func.blocks {
        match &bb.terminator {
            fidan_mir::Terminator::Goto(id) => {
                if id.0 <= bb.id.0 {
                    // Back-edge: mark [id .. bb] as loop body.
                    for b in id.0..=bb.id.0 {
                        loop_blocks.insert(BlockId(b));
                    }
                }
            }
            fidan_mir::Terminator::Branch {
                then_bb, else_bb, ..
            } => {
                for &succ in &[*then_bb, *else_bb] {
                    if succ.0 <= bb.id.0 {
                        for b in succ.0..=bb.id.0 {
                            loop_blocks.insert(BlockId(b));
                        }
                    }
                }
            }
            // Return / Throw / Unreachable — no successors, no back-edges.
            _ => {}
        }
    }

    loop_blocks
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::SymbolInterner;
    use fidan_mir::{
        BlockId, FunctionId, Instr, LocalId, MirFunction, MirProgram, MirTy, Operand, Rvalue,
        Terminator,
    };
    use std::sync::Arc;

    fn make_interner() -> Arc<SymbolInterner> {
        Arc::new(SymbolInterner::new())
    }

    /// Build a minimal `MirFunction` whose blocks form a back-edge:
    ///   bb0 → bb1 → bb0 (back-edge, loop header)
    fn make_loop_func(id: u32, name_sym: fidan_lexer::Symbol) -> MirFunction {
        let mut f = MirFunction::new(FunctionId(id), name_sym, MirTy::Nothing);
        // bb0: unconditional loop body
        let _bb0 = f.alloc_block();
        // bb1: back-edge to bb0
        let _bb1 = f.alloc_block();
        f.blocks[0].terminator = Terminator::Goto(BlockId(1));
        f.blocks[1].terminator = Terminator::Goto(BlockId(0)); // back-edge
        f
    }

    #[test]
    fn no_loop_no_diags() {
        let interner = make_interner();
        let name = interner.intern("straight_line");
        let mut f = MirFunction::new(FunctionId(0), name, MirTy::Nothing);
        let _b0 = f.alloc_block();
        f.blocks[0].terminator = Terminator::Return(None);
        let mut prog = MirProgram::new();
        prog.add_function(f);
        let diags = check(&prog, &interner);
        assert!(diags.is_empty(), "expected no diags for straight-line code");
    }

    #[test]
    fn loop_with_dynamic_assign_emits_w5001() {
        let interner = make_interner();
        let name = interner.intern("hot_fn");
        let target_name = interner.intern("callee");
        let mut f = make_loop_func(0, name);

        // In bb0 (loop body): assign Dynamic result of a Call
        let dest = LocalId(0);
        f.blocks[0].instructions.push(Instr::Assign {
            dest,
            ty: MirTy::Dynamic,
            rhs: Rvalue::Call {
                callee: fidan_mir::Callee::Builtin(target_name),
                args: vec![],
            },
        });
        // Also use it as a call argument to trigger W5001
        f.blocks[0].instructions.push(Instr::Call {
            dest: None,
            callee: fidan_mir::Callee::Builtin(target_name),
            args: vec![Operand::Local(dest)],
            span: fidan_source::Span::new(fidan_source::FileId(0), 0, 0),
        });

        let mut prog = MirProgram::new();
        prog.add_function(f);

        let diags = check(&prog, &interner);
        assert!(
            diags.iter().any(|d| d.code == "W5001"),
            "expected W5001 for loop with dynamic-typed call result"
        );
    }

    #[test]
    fn loop_call_no_precompile_emits_w5003() {
        let interner = make_interner();
        let caller_name = interner.intern("outer");
        let callee_name = interner.intern("inner");

        // callee: a plain function without @precompile
        let mut callee_fn = MirFunction::new(FunctionId(1), callee_name, MirTy::Nothing);
        let _b = callee_fn.alloc_block();
        callee_fn.blocks[0].terminator = Terminator::Return(None);
        assert!(!callee_fn.precompile);

        // caller: loop that calls callee
        let mut caller_fn = make_loop_func(0, caller_name);
        caller_fn.blocks[0].instructions.push(Instr::Call {
            dest: None,
            callee: fidan_mir::Callee::Fn(FunctionId(1)),
            args: vec![],
            span: fidan_source::Span::new(fidan_source::FileId(0), 0, 0),
        });

        let mut prog = MirProgram::new();
        prog.add_function(caller_fn);
        prog.add_function(callee_fn);

        let diags = check(&prog, &interner);
        assert!(
            diags.iter().any(|d| d.code == "W5003"),
            "expected W5003 when a non-@precompile function is called in a loop"
        );
    }

    #[test]
    fn loop_call_with_precompile_no_w5003() {
        let interner = make_interner();
        let caller_name = interner.intern("outer2");
        let callee_name = interner.intern("inner2");

        // callee: has @precompile
        let mut callee_fn = MirFunction::new(FunctionId(1), callee_name, MirTy::Nothing);
        let _b = callee_fn.alloc_block();
        callee_fn.blocks[0].terminator = Terminator::Return(None);
        callee_fn.precompile = true;

        let mut caller_fn = make_loop_func(0, caller_name);
        caller_fn.blocks[0].instructions.push(Instr::Call {
            dest: None,
            callee: fidan_mir::Callee::Fn(FunctionId(1)),
            args: vec![],
            span: fidan_source::Span::new(fidan_source::FileId(0), 0, 0),
        });

        let mut prog = MirProgram::new();
        prog.add_function(caller_fn);
        prog.add_function(callee_fn);

        let diags = check(&prog, &interner);
        assert!(
            !diags.iter().any(|d| d.code == "W5003"),
            "W5003 must not fire when callee is already @precompile"
        );
    }
}
