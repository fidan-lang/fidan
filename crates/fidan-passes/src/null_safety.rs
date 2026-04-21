// fidan-passes/src/null_safety.rs
//
// Null-safety static analysis (W2006).
//
// Algorithm
// ---------
// For each function F in the MIR program:
//   1. Single SSA forward scan: build `definitely_nothing: HashSet<LocalId>` —
//      locals that are unconditionally assigned the literal `nothing`, or are
//      SSA copies of such a local.
//   2. Propagate through `Rvalue::NullCoalesce`: the result is NOT nothing (lhs
//      is nothing → result is rhs, which is assumed non-nothing).
//   3. Check each instruction for dangerous uses of definitely-nothing locals:
//      - Binary arithmetic / bitwise on a nothing operand (comparison and logical
//        ops are excluded — they are valid null-guard patterns: `x if is not nothing`).//      - Method call where the receiver is definitely nothing.
//      - Field read/write where the object is definitely nothing.
//      - Index read/write where the collection is definitely nothing.
//      - Direct function call (`Callee::Fn`) where an argument matching a
//        `certain = true` parameter is definitely nothing.
//
// Conservative design
// -------------------
// Only locals that are *unconditionally* nothing (assigned `nothing` directly,
// or SSA-copied from such a local) are flagged.  Phi-node results and
// `LoadGlobal` results are treated as "unknown" and never flagged — this keeps
// the false-positive rate at zero while still catching the most common cases
// such as forgetting to initialise a variable before use.

use fidan_ast::BinOp;
use fidan_lexer::SymbolInterner;
use fidan_mir::{Callee, FunctionId, Instr, LocalId, MirLit, MirProgram, Operand, Rvalue};
use std::collections::HashSet;

/// Single null safety diagnostic.
pub struct NullSafetyDiag {
    /// Name of the function where the issue was found.
    pub fn_name: String,
    /// Human-readable description of the dangerous use.
    pub context: String,
}

/// Run the null-safety check across the entire program.
pub fn check(prog: &MirProgram, interner: &SymbolInterner) -> Vec<NullSafetyDiag> {
    let mut diags = Vec::new();

    for func in &prog.functions {
        let fn_name = interner.resolve(func.name).to_string();

        // ── Pass 1: build definitely-nothing set ──────────────────────────────
        let mut def_nothing: HashSet<LocalId> = HashSet::new();
        // Also track locals that are the result of NullCoalesce (definitely NOT nothing).
        let mut def_value: HashSet<LocalId> = HashSet::new();

        for bb in &func.blocks {
            for instr in &bb.instructions {
                if let Instr::Assign { dest, rhs, .. } = instr {
                    match rhs {
                        // Literal nothing  →  dest is definitely nothing.
                        Rvalue::Literal(MirLit::Nothing) => {
                            def_nothing.insert(*dest);
                        }
                        // Copy of a nothing local  →  dest is also nothing.
                        Rvalue::Use(Operand::Local(src)) if def_nothing.contains(src) => {
                            def_nothing.insert(*dest);
                        }
                        // Const-nothing via Use  →  dest is definitely nothing.
                        Rvalue::Use(Operand::Const(MirLit::Nothing)) => {
                            def_nothing.insert(*dest);
                        }
                        // NullCoalesce result:  if lhs is nothing but rhs isn't,
                        // the result is rhs — NOT nothing.  Track this to avoid
                        // false positives on the downstream uses of `dest`.
                        Rvalue::NullCoalesce { .. } => {
                            def_value.insert(*dest);
                        }
                        _ => {
                            // Any other rvalue → we assume it might be non-nothing.
                        }
                    }
                }
            }
        }

        // ── Pass 2: check dangerous uses ──────────────────────────────────────
        for bb in &func.blocks {
            for instr in &bb.instructions {
                match instr {
                    // ── Arithmetic / bitwise on nothing ───────────────────────
                    // Comparison and logical ops (==, !=, <, <=, >, >=, and, or)
                    // are valid null-guard patterns and must NOT be flagged.
                    Instr::Assign {
                        rhs: Rvalue::Binary { op, lhs, rhs },
                        ..
                    } => {
                        let is_safe_op = matches!(
                            op,
                            BinOp::Eq
                                | BinOp::NotEq
                                | BinOp::Lt
                                | BinOp::LtEq
                                | BinOp::Gt
                                | BinOp::GtEq
                                | BinOp::And
                                | BinOp::Or
                        );
                        if !is_safe_op
                            && (is_def_nothing(lhs, &def_nothing, &def_value)
                                || is_def_nothing(rhs, &def_nothing, &def_value))
                        {
                            diags.push(NullSafetyDiag {
                                fn_name: fn_name.clone(),
                                context: "binary operation with a `nothing` operand".into(),
                            });
                        }
                    }
                    Instr::Assign {
                        rhs: Rvalue::Unary { operand, .. },
                        ..
                    } if is_def_nothing(operand, &def_nothing, &def_value) => {
                        diags.push(NullSafetyDiag {
                            fn_name: fn_name.clone(),
                            context: "unary operation on `nothing`".into(),
                        });
                    }

                    // ── Method call on nothing receiver ───────────────────────
                    Instr::Call {
                        callee: Callee::Method { receiver, .. },
                        ..
                    } if is_def_nothing(receiver, &def_nothing, &def_value) => {
                        diags.push(NullSafetyDiag {
                            fn_name: fn_name.clone(),
                            context: "method call on `nothing`".into(),
                        });
                    }

                    // ── Direct function call: check `certain` params ──────────
                    Instr::Call {
                        callee: Callee::Fn(fn_id),
                        args,
                        ..
                    } => {
                        check_certain_args(
                            *fn_id,
                            args,
                            prog,
                            interner,
                            &def_nothing,
                            &def_value,
                            &fn_name,
                            &mut diags,
                        );
                    }

                    // ── Field access on nothing ───────────────────────────────
                    Instr::GetField { object, field, .. }
                        if is_def_nothing(object, &def_nothing, &def_value) =>
                    {
                        let fname = interner.resolve(*field);
                        diags.push(NullSafetyDiag {
                            fn_name: fn_name.clone(),
                            context: format!("field read `.{fname}` on `nothing`"),
                        });
                    }
                    Instr::SetField { object, field, .. }
                        if is_def_nothing(object, &def_nothing, &def_value) =>
                    {
                        let fname = interner.resolve(*field);
                        diags.push(NullSafetyDiag {
                            fn_name: fn_name.clone(),
                            context: format!("field write `.{fname}` on `nothing`"),
                        });
                    }

                    // ── Index access on nothing ───────────────────────────────
                    Instr::GetIndex { object, .. }
                        if is_def_nothing(object, &def_nothing, &def_value) =>
                    {
                        diags.push(NullSafetyDiag {
                            fn_name: fn_name.clone(),
                            context: "index read on `nothing`".into(),
                        });
                    }
                    Instr::SetIndex { object, .. }
                        if is_def_nothing(object, &def_nothing, &def_value) =>
                    {
                        diags.push(NullSafetyDiag {
                            fn_name: fn_name.clone(),
                            context: "index write on `nothing`".into(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    diags
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `true` when `op` is unconditionally `nothing` and has NOT been
/// saved by a null-coalesce expression.
fn is_def_nothing(
    op: &Operand,
    def_nothing: &HashSet<LocalId>,
    def_value: &HashSet<LocalId>,
) -> bool {
    match op {
        Operand::Const(MirLit::Nothing) => true,
        Operand::Local(l) => def_nothing.contains(l) && !def_value.contains(l),
        _ => false,
    }
}

/// Check whether any argument passed to a direct callee corresponds to a
/// `certain = true` parameter and is definitely `nothing`.
#[allow(clippy::too_many_arguments)]
fn check_certain_args(
    fn_id: FunctionId,
    args: &[Operand],
    prog: &MirProgram,
    interner: &SymbolInterner,
    def_nothing: &HashSet<LocalId>,
    def_value: &HashSet<LocalId>,
    caller_name: &str,
    diags: &mut Vec<NullSafetyDiag>,
) {
    let callee = prog.function(fn_id);
    let callee_name = interner.resolve(callee.name);

    for (i, param) in callee.params.iter().enumerate() {
        if !param.certain {
            continue;
        }
        if let Some(arg) = args.get(i)
            && is_def_nothing(arg, def_nothing, def_value)
        {
            let pname = interner.resolve(param.name);
            diags.push(NullSafetyDiag {
                fn_name: caller_name.to_string(),
                context: format!(
                    "passing `nothing` as `{pname}` in call to `{callee_name}` \
                         — parameter is `oftype` (non-nullable)"
                ),
            });
        }
    }
}
