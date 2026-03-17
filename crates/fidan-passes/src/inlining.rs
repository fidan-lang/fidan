// fidan-passes/src/inlining.rs
//
// Function inlining: replace calls to small, single-block actions with
// inlined copies of their bodies, enabling further folding/DCE.
//
// Inlining criteria (all must hold):
//   1. Callee has exactly 1 basic block.
//   2. ≤ INLINE_THRESHOLD non-Nop instructions in that block.
//   3. Terminator is `Return(_)`.
//   4. No recursive self-calls, no spawn, no exception handlers.
//
// After inlining, follow-up passes (ConstantFolding, CopyPropagation, DCE)
// clean up the resulting literals and dead temporaries.

use fidan_mir::{
    Callee, FunctionId, Instr, LocalId, MirFunction, MirLit, MirProgram, MirStringPart, MirTy,
    Operand, Rvalue, Terminator,
};
use rustc_hash::FxHashMap;

pub struct Inlining;

/// Maximum instructions in a callee's single block for it to be inlined.
const INLINE_THRESHOLD: usize = 15;

impl crate::Pass for Inlining {
    fn run(&self, prog: &mut MirProgram) {
        // Determine which function IDs are eligible to be inlined.
        let n = prog.functions.len();
        let inlinable: Vec<bool> = (0..n)
            .map(|i| is_inlinable(&prog.functions[i], FunctionId(i as u32)))
            .collect();

        // Collect all call sites across all (non-candidate) callers.
        // Each entry: (caller_idx, bb_idx, instr_idx, callee_fid).
        // Sorted descending so higher indices are processed first — this way
        // earlier indices in the same block remain valid after insertion.
        let mut sites: Vec<(usize, usize, usize, FunctionId)> = Vec::new();
        for (ci, func) in prog.functions.iter().enumerate() {
            for (bi, bb) in func.blocks.iter().enumerate() {
                for (ii, instr) in bb.instructions.iter().enumerate() {
                    if let Some(fid) = call_target(instr)
                        && inlinable.get(fid.0 as usize).copied().unwrap_or(false)
                    {
                        sites.push((ci, bi, ii, fid));
                    }
                }
            }
        }

        // Process in descending (bb_idx, instr_idx) so indices stay valid.
        sites.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));

        for (caller_idx, bb_idx, instr_idx, callee_fid) in sites {
            // Skip self-recursion (shouldn't happen — is_inlinable filters it — but be safe).
            if caller_idx == callee_fid.0 as usize {
                continue;
            }

            // Extract callee data without holding a borrow on prog.functions.
            let callee_data = extract_callee(&prog.functions[callee_fid.0 as usize]);

            // Extract call args and dest from the caller's instruction.
            let (dest_local, args) =
                match &prog.functions[caller_idx].blocks[bb_idx].instructions[instr_idx] {
                    Instr::Assign {
                        dest,
                        rhs: Rvalue::Call { args, .. },
                        ..
                    } => (Some(*dest), args.clone()),
                    Instr::Call { dest, args, .. } => (*dest, args.clone()),
                    _ => continue,
                };

            do_inline(
                &mut prog.functions[caller_idx],
                bb_idx,
                instr_idx,
                dest_local,
                args,
                callee_data,
            );
        }
    }
}

// ── Callee eligibility ─────────────────────────────────────────────────────

fn is_inlinable(func: &MirFunction, self_id: FunctionId) -> bool {
    if func.blocks.len() != 1 {
        return false;
    }
    let bb = &func.blocks[0];
    let count = bb
        .instructions
        .iter()
        .filter(|i| !matches!(i, Instr::Nop))
        .count();
    if count > INLINE_THRESHOLD {
        return false;
    }
    if !matches!(bb.terminator, Terminator::Return(_)) {
        return false;
    }
    for instr in &bb.instructions {
        match instr {
            Instr::Call {
                callee: Callee::Fn(id),
                ..
            } if *id == self_id => return false,
            Instr::Assign {
                rhs:
                    Rvalue::Call {
                        callee: Callee::Fn(id),
                        ..
                    },
                ..
            } if *id == self_id => return false,
            Instr::SpawnParallel { .. }
            | Instr::SpawnConcurrent { .. }
            | Instr::SpawnExpr { .. }
            | Instr::SpawnDynamic { .. }
            | Instr::PushCatch(_)
            | Instr::PopCatch => return false,
            _ => {}
        }
    }
    true
}

/// Extract the direct `Fn` callee id from a call instruction, if any.
fn call_target(instr: &Instr) -> Option<FunctionId> {
    match instr {
        Instr::Assign {
            rhs:
                Rvalue::Call {
                    callee: Callee::Fn(fid),
                    ..
                },
            ..
        } => Some(*fid),
        Instr::Call {
            callee: Callee::Fn(fid),
            ..
        } => Some(*fid),
        _ => None,
    }
}

// ── Callee data extraction ─────────────────────────────────────────────────

struct CalleeData {
    /// LocalId of each parameter (in order).
    param_locals: Vec<LocalId>,
    /// Default value for each parameter (None = no default).
    param_defaults: Vec<Option<MirLit>>,
    /// Total local count (for offset calculation).
    local_count: u32,
    /// Return type (for the return-value assignment in the caller).
    return_ty: MirTy,
    /// Cloned instructions from the single block (excludes the terminator).
    instructions: Vec<Instr>,
    /// Cloned terminator (should be `Return(_)`).
    terminator: Terminator,
}

fn extract_callee(func: &MirFunction) -> CalleeData {
    let bb = &func.blocks[0];
    CalleeData {
        param_locals: func.params.iter().map(|p| p.local).collect(),
        param_defaults: func.params.iter().map(|p| p.default.clone()).collect(),
        local_count: func.local_count,
        return_ty: func.return_ty.clone(),
        instructions: bb.instructions.clone(),
        terminator: bb.terminator.clone(),
    }
}

// ── Inlining ───────────────────────────────────────────────────────────────

fn do_inline(
    caller: &mut MirFunction,
    bb_idx: usize,
    instr_idx: usize,
    dest_local: Option<LocalId>,
    call_args: Vec<Operand>,
    callee: CalleeData,
) {
    let offset = caller.local_count;

    // Map each callee parameter local → the corresponding call argument.
    // If an arg is missing or is the Nothing literal, substitute the param's
    // compile-time default (if any) so optional-with-default params work
    // correctly even after inlining.
    let mut param_map: FxHashMap<LocalId, Operand> = FxHashMap::default();
    for (i, &param_local) in callee.param_locals.iter().enumerate() {
        let arg = call_args.get(i).cloned();
        let resolved = match arg {
            Some(Operand::Const(MirLit::Nothing)) | None => {
                // Use default if available, otherwise Nothing.
                callee
                    .param_defaults
                    .get(i)
                    .and_then(|d| d.as_ref())
                    .map(|lit| Operand::Const(lit.clone()))
                    .or_else(|| call_args.get(i).cloned())
                    .unwrap_or(Operand::Const(MirLit::Nothing))
            }
            Some(op) => op,
        };
        param_map.insert(param_local, resolved);
    }

    // Remap and clone callee instructions.
    let remapped: Vec<Instr> = callee
        .instructions
        .iter()
        .map(|i| remap_instr(i, &param_map, offset))
        .collect();
    let n_body = remapped.len();

    // Remap the return value.
    let return_op: Option<Operand> = match &callee.terminator {
        Terminator::Return(Some(op)) => Some(remap_op(op, &param_map, offset)),
        _ => None,
    };

    // Grow caller's local counter.
    caller.local_count += callee.local_count;

    // Remove the original call instruction.
    caller.blocks[bb_idx].instructions.remove(instr_idx);

    // Insert remapped body at the same position.
    for (i, instr) in remapped.into_iter().enumerate() {
        caller.blocks[bb_idx]
            .instructions
            .insert(instr_idx + i, instr);
    }

    // If there's a dest and a return value, append an assignment.
    if let (Some(dest), Some(ret_op)) = (dest_local, return_op) {
        caller.blocks[bb_idx].instructions.insert(
            instr_idx + n_body,
            Instr::Assign {
                dest,
                ty: callee.return_ty,
                rhs: Rvalue::Use(ret_op),
            },
        );
    }
}

// ── Local remapping ────────────────────────────────────────────────────────

fn remap_op(op: &Operand, param_map: &FxHashMap<LocalId, Operand>, offset: u32) -> Operand {
    match op {
        Operand::Local(l) => {
            if let Some(mapped) = param_map.get(l) {
                mapped.clone()
            } else {
                Operand::Local(LocalId(l.0 + offset))
            }
        }
        Operand::Const(_) => op.clone(),
    }
}

fn remap_lit(l: LocalId, offset: u32) -> LocalId {
    LocalId(l.0 + offset)
}

/// Clone + remap a single instruction: both operands (reads) and dest locals (writes).
fn remap_instr(instr: &Instr, param_map: &FxHashMap<LocalId, Operand>, offset: u32) -> Instr {
    let r = |op: &Operand| remap_op(op, param_map, offset);
    let d = |l: LocalId| remap_lit(l, offset);

    match instr {
        Instr::Assign { dest, ty, rhs } => Instr::Assign {
            dest: d(*dest),
            ty: ty.clone(),
            rhs: remap_rvalue(rhs, &r),
        },
        Instr::Call {
            dest,
            result_ty,
            callee,
            args,
            span,
        } => Instr::Call {
            dest: dest.map(d),
            result_ty: result_ty.clone(),
            callee: remap_callee(callee, &r),
            args: args.iter().map(&r).collect(),
            span: *span,
        },
        Instr::SetField {
            object,
            field,
            value,
        } => Instr::SetField {
            object: r(object),
            field: *field,
            value: r(value),
        },
        Instr::GetField {
            dest,
            object,
            field,
        } => Instr::GetField {
            dest: d(*dest),
            object: r(object),
            field: *field,
        },
        Instr::GetIndex {
            dest,
            object,
            index,
        } => Instr::GetIndex {
            dest: d(*dest),
            object: r(object),
            index: r(index),
        },
        Instr::SetIndex {
            object,
            index,
            value,
        } => Instr::SetIndex {
            object: r(object),
            index: r(index),
            value: r(value),
        },
        Instr::Drop { local } => Instr::Drop { local: d(*local) },
        Instr::LoadGlobal { dest, global } => Instr::LoadGlobal {
            dest: d(*dest),
            global: *global,
        },
        Instr::StoreGlobal { global, value } => Instr::StoreGlobal {
            global: *global,
            value: r(value),
        },
        Instr::AwaitPending { dest, handle } => Instr::AwaitPending {
            dest: d(*dest),
            handle: r(handle),
        },
        // Spawn / concurrent: should not appear (screened by is_inlinable), but remap safely.
        Instr::SpawnConcurrent {
            handle,
            task_fn,
            args,
        } => Instr::SpawnConcurrent {
            handle: d(*handle),
            task_fn: *task_fn,
            args: args.iter().map(&r).collect(),
        },
        Instr::SpawnParallel {
            handle,
            task_fn,
            args,
        } => Instr::SpawnParallel {
            handle: d(*handle),
            task_fn: *task_fn,
            args: args.iter().map(&r).collect(),
        },
        Instr::SpawnExpr {
            dest,
            task_fn,
            args,
        } => Instr::SpawnExpr {
            dest: d(*dest),
            task_fn: *task_fn,
            args: args.iter().map(&r).collect(),
        },
        Instr::SpawnDynamic { dest, method, args } => Instr::SpawnDynamic {
            dest: d(*dest),
            method: *method,
            args: args.iter().map(&r).collect(),
        },
        Instr::JoinAll { handles } => Instr::JoinAll {
            handles: handles.iter().map(|&h| d(h)).collect(),
        },
        Instr::ParallelIter {
            collection,
            body_fn,
            closure_args,
        } => Instr::ParallelIter {
            collection: r(collection),
            body_fn: *body_fn,
            closure_args: closure_args.iter().map(&r).collect(),
        },
        Instr::PushCatch(bid) => Instr::PushCatch(*bid),
        Instr::PopCatch => Instr::PopCatch,
        Instr::CertainCheck { operand, name } => Instr::CertainCheck {
            operand: r(operand),
            name: *name,
        },
        Instr::Nop => Instr::Nop,
    }
}

fn remap_rvalue(rv: &Rvalue, r: &impl Fn(&Operand) -> Operand) -> Rvalue {
    match rv {
        Rvalue::Use(op) => Rvalue::Use(r(op)),
        Rvalue::Binary { op, lhs, rhs } => Rvalue::Binary {
            op: *op,
            lhs: r(lhs),
            rhs: r(rhs),
        },
        Rvalue::Unary { op, operand } => Rvalue::Unary {
            op: *op,
            operand: r(operand),
        },
        Rvalue::NullCoalesce { lhs, rhs } => Rvalue::NullCoalesce {
            lhs: r(lhs),
            rhs: r(rhs),
        },
        Rvalue::Call { callee, args } => Rvalue::Call {
            callee: remap_callee(callee, r),
            args: args.iter().map(r).collect(),
        },
        Rvalue::Construct { ty, fields } => Rvalue::Construct {
            ty: *ty,
            fields: fields.iter().map(|(f, v)| (*f, r(v))).collect(),
        },
        Rvalue::List(elems) => Rvalue::List(elems.iter().map(r).collect()),
        Rvalue::Dict(pairs) => Rvalue::Dict(pairs.iter().map(|(k, v)| (r(k), r(v))).collect()),
        Rvalue::Tuple(elems) => Rvalue::Tuple(elems.iter().map(r).collect()),
        Rvalue::StringInterp(parts) => Rvalue::StringInterp(
            parts
                .iter()
                .map(|p| match p {
                    MirStringPart::Literal(s) => MirStringPart::Literal(s.clone()),
                    MirStringPart::Operand(op) => MirStringPart::Operand(r(op)),
                })
                .collect(),
        ),
        Rvalue::Literal(lit) => Rvalue::Literal(lit.clone()),
        Rvalue::CatchException => Rvalue::CatchException,
        Rvalue::MakeClosure { fn_id, captures } => Rvalue::MakeClosure {
            fn_id: *fn_id,
            captures: captures.iter().map(r).collect(),
        },
        Rvalue::Slice {
            target,
            start,
            end,
            inclusive,
            step,
        } => Rvalue::Slice {
            target: r(target),
            start: start.as_ref().map(r),
            end: end.as_ref().map(r),
            inclusive: *inclusive,
            step: step.as_ref().map(r),
        },
        Rvalue::ConstructEnum { tag, payload } => Rvalue::ConstructEnum {
            tag: *tag,
            payload: payload.iter().map(r).collect(),
        },
        Rvalue::EnumTagCheck {
            value,
            expected_tag,
        } => Rvalue::EnumTagCheck {
            value: r(value),
            expected_tag: *expected_tag,
        },
        Rvalue::EnumPayload { value, index } => Rvalue::EnumPayload {
            value: r(value),
            index: *index,
        },
    }
}

fn remap_callee(callee: &Callee, r: &impl Fn(&Operand) -> Operand) -> Callee {
    match callee {
        Callee::Fn(fid) => Callee::Fn(*fid),
        Callee::Builtin(sym) => Callee::Builtin(*sym),
        Callee::Method { receiver, method } => Callee::Method {
            receiver: r(receiver),
            method: *method,
        },
        Callee::Dynamic(op) => Callee::Dynamic(r(op)),
    }
}
