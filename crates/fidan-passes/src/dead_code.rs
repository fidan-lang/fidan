// fidan-passes/src/dead_code.rs
//
// Dead-code elimination: remove pure `Assign` instructions whose `dest` is
// never subsequently read, and strip all `Nop`s.

use fidan_mir::{
    Callee, Instr, LocalId, MirFunction, MirProgram, MirStringPart, Operand, Rvalue, Terminator,
};
use rustc_hash::FxHashMap;

pub struct DeadCodeElimination;

impl crate::Pass for DeadCodeElimination {
    fn run(&self, prog: &mut MirProgram) {
        for func in &mut prog.functions {
            // Count all reads of each local.
            let use_count = count_uses(func);

            for bb in &mut func.blocks {
                bb.instructions.retain(|instr| {
                    match instr {
                        // Remove pure assignments whose result is never read.
                        Instr::Assign { dest, rhs, .. } => {
                            if use_count.get(dest).copied().unwrap_or(0) == 0 && is_pure_rvalue(rhs)
                            {
                                return false;
                            }
                            true
                        }
                        // Always strip no-ops.
                        Instr::Nop => false,
                        _ => true,
                    }
                });
            }
        }
    }
}

/// Count how many times each local is read (appears as an operand, not a dest).
fn count_uses(func: &MirFunction) -> FxHashMap<LocalId, usize> {
    let mut uses: FxHashMap<LocalId, usize> = FxHashMap::default();
    let mut add = |op: &Operand| {
        if let Operand::Local(l) = op {
            *uses.entry(*l).or_insert(0) += 1;
        }
    };
    for bb in &func.blocks {
        for phi in &bb.phis {
            for (_, op) in &phi.operands {
                add(op);
            }
        }
        for instr in &bb.instructions {
            count_instr_reads(instr, &mut add);
        }
        match &bb.terminator {
            Terminator::Return(Some(op)) => add(op),
            Terminator::Branch { cond, .. } => add(cond),
            Terminator::Throw { value } => add(value),
            _ => {}
        }
    }
    uses
}

fn count_instr_reads(instr: &Instr, add: &mut impl FnMut(&Operand)) {
    match instr {
        Instr::Assign { rhs, .. } => count_rvalue_reads(rhs, add),
        Instr::Call { callee, args, .. } => {
            match callee {
                Callee::Method { receiver, .. } => add(receiver),
                Callee::Dynamic(op) => add(op),
                Callee::Fn(_) | Callee::Builtin(_) => {}
            }
            for a in args {
                add(a);
            }
        }
        Instr::SetField { object, value, .. } => {
            add(object);
            add(value);
        }
        Instr::GetField { object, .. } => add(object),
        Instr::GetIndex { object, index, .. } => {
            add(object);
            add(index);
        }
        Instr::SetIndex {
            object,
            index,
            value,
        } => {
            add(object);
            add(index);
            add(value);
        }
        Instr::AwaitPending { handle, .. } => add(handle),
        Instr::SpawnConcurrent { args, .. }
        | Instr::SpawnParallel { args, .. }
        | Instr::SpawnExpr { args, .. }
        | Instr::SpawnDynamic { args, .. } => {
            for a in args {
                add(a);
            }
        }
        Instr::ParallelIter {
            collection,
            closure_args,
            ..
        } => {
            add(collection);
            for a in closure_args {
                add(a);
            }
        }
        Instr::Drop { local } => add(&Operand::Local(*local)),
        Instr::JoinAll { handles } => {
            for h in handles {
                add(&Operand::Local(*h));
            }
        }
        Instr::Nop | Instr::PushCatch(_) | Instr::PopCatch => {}
        Instr::CertainCheck { operand, .. } => add(operand),
        Instr::LoadGlobal { .. } => {}
        Instr::StoreGlobal { value, .. } => add(value),
    }
}

fn count_rvalue_reads(rv: &Rvalue, add: &mut impl FnMut(&Operand)) {
    match rv {
        Rvalue::Use(op) => add(op),
        Rvalue::Binary { lhs, rhs, .. } => {
            add(lhs);
            add(rhs);
        }
        Rvalue::Unary { operand, .. } => add(operand),
        Rvalue::NullCoalesce { lhs, rhs } => {
            add(lhs);
            add(rhs);
        }
        Rvalue::Call { callee, args, .. } => {
            match callee {
                Callee::Method { receiver, .. } => add(receiver),
                Callee::Dynamic(op) => add(op),
                Callee::Fn(_) | Callee::Builtin(_) => {}
            }
            for a in args {
                add(a);
            }
        }
        Rvalue::Construct { fields, .. } => {
            for (_, v) in fields {
                add(v);
            }
        }
        Rvalue::List(elems) => {
            for e in elems {
                add(e);
            }
        }
        Rvalue::Dict(pairs) => {
            for (k, v) in pairs {
                add(k);
                add(v);
            }
        }
        Rvalue::Tuple(elems) => {
            for e in elems {
                add(e);
            }
        }
        Rvalue::StringInterp(parts) => {
            for p in parts {
                if let MirStringPart::Operand(op) = p {
                    add(op);
                }
            }
        }
        Rvalue::Literal(_) | Rvalue::CatchException => {}
        Rvalue::MakeClosure { captures, .. } => {
            for c in captures {
                add(c);
            }
        }
        Rvalue::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            add(target);
            if let Some(s) = start {
                add(s);
            }
            if let Some(e) = end {
                add(e);
            }
            if let Some(st) = step {
                add(st);
            }
        }
    }
}

/// Returns `true` if evaluating this `Rvalue` has no observable side effects.
fn is_pure_rvalue(rv: &Rvalue) -> bool {
    matches!(
        rv,
        Rvalue::Use(_)
            | Rvalue::Binary { .. }
            | Rvalue::Unary { .. }
            | Rvalue::NullCoalesce { .. }
            | Rvalue::List(_)
            | Rvalue::Dict(_)
            | Rvalue::Tuple(_)
            | Rvalue::StringInterp(_)
            | Rvalue::Literal(_)
            | Rvalue::MakeClosure { .. }
            | Rvalue::CatchException
            | Rvalue::Slice { .. }
    )
}
