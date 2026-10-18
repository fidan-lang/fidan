//! Preserve known Nothing flows in the existing boxed MIR representation.
//! Source types remain nullable; scalar AOT ABIs must not erase the Nothing tag.
use crate::Pass;
use fidan_mir::{
    Callee, FunctionId, GlobalId, Instr, LocalId, MirLit, MirProgram, MirTy, Operand, Rvalue,
    Terminator,
};
use rustc_hash::{FxHashMap, FxHashSet};
pub struct NullableRepresentation;
fn scalar(ty: &MirTy) -> bool {
    matches!(
        ty,
        MirTy::Integer | MirTy::Float | MirTy::Boolean | MirTy::Handle
    )
}
fn nullable(operand: &Operand, locals: &FxHashSet<LocalId>) -> bool {
    matches!(operand, Operand::Const(MirLit::Nothing))
        || matches!(operand,Operand::Local(local) if locals.contains(local))
}
impl Pass for NullableRepresentation {
    fn run(&self, program: &mut MirProgram) {
        let mut locals: FxHashMap<FunctionId, FxHashSet<LocalId>> = FxHashMap::default();
        let mut returns = FxHashSet::default();
        let mut globals: FxHashSet<GlobalId> = FxHashSet::default();
        loop {
            let mut changed = false;
            for function in &mut program.functions {
                let known = locals.entry(function.id).or_default();
                for block in &mut function.blocks {
                    for phi in &mut block.phis {
                        if phi
                            .operands
                            .iter()
                            .any(|(_, operand)| nullable(operand, known))
                        {
                            changed |= known.insert(phi.result);
                            if scalar(&phi.ty) {
                                phi.ty = MirTy::Dynamic;
                            }
                        }
                    }
                    for instruction in &mut block.instructions {
                        match instruction {
                            Instr::Assign { dest, ty, rhs } => {
                                let may_be_nothing = match rhs {
                                    Rvalue::Use(operand) => nullable(operand, known),
                                    Rvalue::Literal(MirLit::Nothing) => true,
                                    Rvalue::Call {
                                        callee: Callee::Fn(id),
                                        ..
                                    } => returns.contains(id),
                                    _ => false,
                                };
                                if may_be_nothing {
                                    changed |= known.insert(*dest);
                                    if scalar(ty) {
                                        *ty = MirTy::Dynamic;
                                    }
                                }
                            }
                            Instr::Call {
                                dest: Some(dest),
                                result_ty,
                                callee: Callee::Fn(id),
                                ..
                            } if returns.contains(id) => {
                                changed |= known.insert(*dest);
                                if result_ty.as_ref().is_some_and(scalar) {
                                    *result_ty = Some(MirTy::Dynamic);
                                }
                            }
                            Instr::StoreGlobal { global, value } if nullable(value, known) => {
                                changed |= globals.insert(*global);
                            }
                            Instr::LoadGlobal { dest, global } if globals.contains(global) => {
                                changed |= known.insert(*dest);
                            }
                            _ => {}
                        }
                    }
                    if let Terminator::Return(Some(operand)) = &block.terminator
                        && nullable(operand, known)
                        && scalar(&function.return_ty)
                    {
                        function.return_ty = MirTy::Dynamic;
                        changed |= returns.insert(function.id);
                    }
                }
            }
            for (index, global) in program.globals.iter_mut().enumerate() {
                if globals.contains(&GlobalId(index as u32)) && scalar(&global.ty) {
                    global.ty = MirTy::Dynamic;
                }
            }
            if !changed {
                break;
            }
        }
    }
}
