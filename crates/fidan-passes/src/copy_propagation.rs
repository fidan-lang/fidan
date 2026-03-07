// fidan-passes/src/copy_propagation.rs
//
// Copy propagation: replace `Use(local)` operands with the value they copy
// when that copy was a simple assignment (`dest = Use(src)`).

use fidan_mir::{Callee, Instr, LocalId, MirProgram, MirStringPart, Operand, Rvalue, Terminator};
use rustc_hash::FxHashMap;

pub struct CopyPropagation;

impl crate::Pass for CopyPropagation {
    fn run(&self, prog: &mut MirProgram) {
        for func in &mut prog.functions {
            // Build copy map: local -> what it copies from.
            // We only forward one level (chains are resolved by running the pass
            // twice, but a single pass already handles most cases).
            let mut copy_map: FxHashMap<LocalId, Operand> = FxHashMap::default();
            for bb in &func.blocks {
                for instr in &bb.instructions {
                    if let Instr::Assign {
                        dest,
                        rhs: Rvalue::Use(src),
                        ..
                    } = instr
                    {
                        copy_map.insert(*dest, src.clone());
                    }
                }
            }
            if copy_map.is_empty() {
                continue;
            }

            // Resolve operand transitively (up to 8 hops to break cycles safely).
            let resolve = |op: &Operand| -> Operand {
                let mut cur = op.clone();
                for _ in 0..8 {
                    if let Operand::Local(l) = &cur {
                        if let Some(next) = copy_map.get(l) {
                            cur = next.clone();
                            continue;
                        }
                    }
                    break;
                }
                cur
            };

            // Substitute in all instructions and terminators.
            for bb in &mut func.blocks {
                for phi in &mut bb.phis {
                    for (_, op) in &mut phi.operands {
                        *op = resolve(op);
                    }
                }
                for instr in &mut bb.instructions {
                    subst_instr(instr, &resolve);
                }
                subst_terminator(&mut bb.terminator, &resolve);
            }
        }
    }
}

fn subst_op(op: &mut Operand, resolve: &impl Fn(&Operand) -> Operand) {
    *op = resolve(op);
}

fn subst_rvalue(rv: &mut Rvalue, resolve: &impl Fn(&Operand) -> Operand) {
    match rv {
        Rvalue::Use(op) => subst_op(op, resolve),
        Rvalue::Binary { lhs, rhs, .. } => {
            subst_op(lhs, resolve);
            subst_op(rhs, resolve);
        }
        Rvalue::Unary { operand, .. } => subst_op(operand, resolve),
        Rvalue::NullCoalesce { lhs, rhs } => {
            subst_op(lhs, resolve);
            subst_op(rhs, resolve);
        }
        Rvalue::Call { callee, args, .. } => {
            subst_callee(callee, resolve);
            for a in args {
                subst_op(a, resolve);
            }
        }
        Rvalue::Construct { fields, .. } => {
            for (_, v) in fields {
                subst_op(v, resolve);
            }
        }
        Rvalue::List(elems) => {
            for e in elems {
                subst_op(e, resolve);
            }
        }
        Rvalue::Dict(pairs) => {
            for (k, v) in pairs {
                subst_op(k, resolve);
                subst_op(v, resolve);
            }
        }
        Rvalue::Tuple(elems) => {
            for e in elems {
                subst_op(e, resolve);
            }
        }
        Rvalue::StringInterp(parts) => {
            for p in parts {
                if let MirStringPart::Operand(op) = p {
                    subst_op(op, resolve);
                }
            }
        }
        Rvalue::Literal(_) | Rvalue::CatchException => {}
        Rvalue::MakeClosure { captures, .. } => {
            for c in captures {
                subst_op(c, resolve);
            }
        }
        Rvalue::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            subst_op(target, resolve);
            if let Some(s) = start {
                subst_op(s, resolve);
            }
            if let Some(e) = end {
                subst_op(e, resolve);
            }
            if let Some(st) = step {
                subst_op(st, resolve);
            }
        }
        Rvalue::ConstructEnum { payload, .. } => {
            for p in payload {
                subst_op(p, resolve);
            }
        }
        Rvalue::EnumTagCheck { value, .. } => subst_op(value, resolve),
        Rvalue::EnumPayload { value, .. } => subst_op(value, resolve),
    }
}

fn subst_callee(callee: &mut Callee, resolve: &impl Fn(&Operand) -> Operand) {
    match callee {
        Callee::Method { receiver, .. } => subst_op(receiver, resolve),
        Callee::Dynamic(op) => subst_op(op, resolve),
        Callee::Fn(_) | Callee::Builtin(_) => {}
    }
}

fn subst_instr(instr: &mut Instr, resolve: &impl Fn(&Operand) -> Operand) {
    match instr {
        Instr::Assign { rhs, .. } => subst_rvalue(rhs, resolve),
        Instr::Call {
            callee,
            args,
            dest: _,
            span: _,
        } => {
            subst_callee(callee, resolve);
            for a in args {
                subst_op(a, resolve);
            }
        }
        Instr::SetField { object, value, .. } => {
            subst_op(object, resolve);
            subst_op(value, resolve);
        }
        Instr::GetField { object, .. } => subst_op(object, resolve),
        Instr::GetIndex { object, index, .. } => {
            subst_op(object, resolve);
            subst_op(index, resolve);
        }
        Instr::SetIndex {
            object,
            index,
            value,
        } => {
            subst_op(object, resolve);
            subst_op(index, resolve);
            subst_op(value, resolve);
        }
        Instr::AwaitPending { handle, .. } => subst_op(handle, resolve),
        Instr::SpawnConcurrent { args, .. }
        | Instr::SpawnParallel { args, .. }
        | Instr::SpawnExpr { args, .. }
        | Instr::SpawnDynamic { args, .. } => {
            for a in args {
                subst_op(a, resolve);
            }
        }
        Instr::JoinAll { .. } | Instr::ParallelIter { .. } => {}
        Instr::Drop { .. } | Instr::Nop | Instr::PushCatch(_) | Instr::PopCatch => {}
        Instr::CertainCheck { operand, .. } => subst_op(operand, resolve),
        Instr::LoadGlobal { .. } => {}
        Instr::StoreGlobal { value, .. } => subst_op(value, resolve),
    }
}

fn subst_terminator(term: &mut Terminator, resolve: &impl Fn(&Operand) -> Operand) {
    match term {
        Terminator::Return(Some(op)) => subst_op(op, resolve),
        Terminator::Branch { cond, .. } => subst_op(cond, resolve),
        Terminator::Throw { value } => subst_op(value, resolve),
        Terminator::Return(None) | Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}
