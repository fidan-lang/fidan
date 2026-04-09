use fidan_lexer::Symbol;
use fidan_stdlib::{StdlibValueKind, infer_receiver_method, infer_stdlib_method};
use std::collections::HashMap;

use crate::{
    Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirLit, MirProgram, MirTy, Operand,
    Rvalue, Terminator,
};

pub fn collect_effective_local_types(
    function: &MirFunction,
    program: &MirProgram,
    mut resolve_symbol: impl FnMut(Symbol) -> Option<String>,
) -> HashMap<u32, MirTy> {
    fn mark_pointer_like_local(map: &mut HashMap<u32, MirTy>, operand: &Operand) {
        if let Operand::Local(local) = operand
            && matches!(map.get(&local.0), Some(MirTy::Error))
        {
            map.insert(local.0, MirTy::Dynamic);
        }
    }

    let mut map = HashMap::new();
    let global_ns_map = build_global_namespace_map(program, &mut resolve_symbol);
    let mut namespace_locals: HashMap<LocalId, String> = HashMap::new();

    for param in &function.params {
        map.insert(param.local.0, param.ty.clone());
    }

    for block in &function.blocks {
        namespace_locals.clear();
        for phi in &block.phis {
            map.entry(phi.result.0).or_insert_with(|| phi.ty.clone());
        }

        for instruction in &block.instructions {
            match instruction {
                Instr::GetField { object, .. }
                | Instr::SetField { object, .. }
                | Instr::GetIndex { object, .. }
                | Instr::SetIndex { object, .. } => mark_pointer_like_local(&mut map, object),
                Instr::Call {
                    callee: Callee::Method { receiver, .. },
                    ..
                } => mark_pointer_like_local(&mut map, receiver),
                Instr::SpawnDynamic {
                    method: Some(_),
                    args,
                    ..
                } => {
                    if let Some(receiver) = args.first() {
                        mark_pointer_like_local(&mut map, receiver);
                    }
                }
                _ => {}
            }

            match instruction {
                Instr::Assign { dest, ty, rhs } => {
                    let effective_ty = if matches!(ty, MirTy::Dynamic | MirTy::Error) {
                        infer_rvalue_type(
                            rhs,
                            program,
                            &map,
                            &namespace_locals,
                            &mut resolve_symbol,
                        )
                    } else {
                        ty.clone()
                    };
                    map.insert(dest.0, effective_ty);
                    namespace_locals.remove(dest);
                    match rhs {
                        Rvalue::Literal(MirLit::Namespace(namespace)) => {
                            namespace_locals.insert(*dest, namespace.clone());
                        }
                        Rvalue::Use(Operand::Local(source)) => {
                            if let Some(namespace) = namespace_locals.get(source).cloned() {
                                namespace_locals.insert(*dest, namespace);
                            }
                        }
                        _ => {}
                    }
                }
                Instr::Call {
                    dest: Some(dest),
                    result_ty,
                    callee,
                    args,
                    ..
                } => {
                    let inferred_ty = result_ty
                        .clone()
                        .filter(|ty| !matches!(ty, MirTy::Dynamic | MirTy::Error))
                        .or_else(|| {
                            infer_call_result_ty(
                                callee,
                                program,
                                args,
                                &map,
                                &namespace_locals,
                                &mut resolve_symbol,
                            )
                        })
                        .unwrap_or(MirTy::Dynamic);
                    map.insert(dest.0, inferred_ty);
                    namespace_locals.remove(dest);
                }
                Instr::GetField { dest, .. } | Instr::GetIndex { dest, .. } => {
                    map.insert(dest.0, MirTy::Dynamic);
                    namespace_locals.remove(dest);
                }
                Instr::AwaitPending { dest, .. } | Instr::SpawnExpr { dest, .. } => {
                    map.entry(dest.0).or_insert(MirTy::Dynamic);
                    namespace_locals.remove(dest);
                }
                Instr::SpawnDynamic { dest, .. } => {
                    map.entry(dest.0)
                        .or_insert(MirTy::Pending(Box::new(MirTy::Dynamic)));
                    namespace_locals.remove(dest);
                }
                Instr::SpawnConcurrent { handle, .. } | Instr::SpawnParallel { handle, .. } => {
                    map.entry(handle.0)
                        .or_insert(MirTy::Pending(Box::new(MirTy::Dynamic)));
                }
                Instr::LoadGlobal { dest, global } => {
                    let global_ty = if global_ns_map.contains_key(global) {
                        MirTy::Dynamic
                    } else {
                        program
                            .globals
                            .get(global.0 as usize)
                            .map(|entry| entry.ty.clone())
                            .unwrap_or(MirTy::Dynamic)
                    };
                    map.entry(dest.0).or_insert(global_ty);
                    if let Some(namespace) = global_ns_map.get(global) {
                        namespace_locals.insert(*dest, namespace.clone());
                    } else {
                        namespace_locals.remove(dest);
                    }
                }
                _ => {}
            }
        }
    }

    loop {
        let mut changed = false;
        for block in &function.blocks {
            for phi in &block.phis {
                if is_primitive_scalar(map.get(&phi.result.0).unwrap_or(&MirTy::Dynamic)) {
                    continue;
                }
                let inferred = if is_primitive_scalar(&phi.ty) {
                    Some(phi.ty.clone())
                } else {
                    phi.operands.iter().find_map(|(_, operand)| match operand {
                        Operand::Local(local) => map
                            .get(&local.0)
                            .filter(|ty| is_primitive_scalar(ty))
                            .cloned(),
                        Operand::Const(MirLit::Float(_)) => Some(MirTy::Float),
                        Operand::Const(MirLit::Int(_)) => Some(MirTy::Integer),
                        Operand::Const(MirLit::Bool(_)) => Some(MirTy::Boolean),
                        _ => None,
                    })
                };
                if let Some(ty) = inferred {
                    let changed_here = map.get(&phi.result.0) != Some(&ty);
                    if changed_here {
                        map.insert(phi.result.0, ty);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    map
}

pub fn collect_may_throw_functions(program: &MirProgram) -> HashMap<FunctionId, bool> {
    let mut map: HashMap<FunctionId, bool> = program
        .functions
        .iter()
        .map(|function| (function.id, true))
        .collect();

    loop {
        let mut changed = false;
        for function in &program.functions {
            let may_throw = function_may_throw(function, &map);
            if map.insert(function.id, may_throw) != Some(may_throw) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    map
}

fn build_global_namespace_map(
    program: &MirProgram,
    resolve_symbol: &mut impl FnMut(Symbol) -> Option<String>,
) -> HashMap<GlobalId, String> {
    let mut map = HashMap::new();
    for (index, global) in program.globals.iter().enumerate() {
        let Some(global_name) = resolve_symbol(global.name) else {
            continue;
        };
        for decl in &program.use_decls {
            if decl.is_stdlib
                && decl.specific_names.is_none()
                && global_name.as_str() == decl.alias.as_str()
            {
                map.insert(GlobalId(index as u32), decl.module.clone());
            }
        }
    }
    map
}

fn infer_call_result_ty(
    callee: &Callee,
    program: &MirProgram,
    args: &[Operand],
    map: &HashMap<u32, MirTy>,
    namespace_locals: &HashMap<LocalId, String>,
    resolve_symbol: &mut impl FnMut(Symbol) -> Option<String>,
) -> Option<MirTy> {
    match callee {
        Callee::Fn(fn_id) => {
            let return_ty = effective_return_ty(&program.functions[fn_id.0 as usize]);
            if matches!(return_ty, MirTy::Nothing | MirTy::Error) {
                None
            } else {
                Some(return_ty)
            }
        }
        Callee::Method { receiver, method } => {
            let method_name = resolve_symbol(*method)?;
            let arg_kinds = args
                .iter()
                .map(|arg| operand_stdlib_kind(arg, map))
                .collect::<Vec<_>>();
            stdlib_namespace(receiver, namespace_locals)
                .and_then(|namespace| {
                    infer_stdlib_method(namespace.as_str(), method_name.as_str(), &arg_kinds)
                        .map(|info| stdlib_kind_to_mir_ty(info.return_kind))
                })
                .or_else(|| match receiver {
                    Operand::Local(recv) => map.get(&recv.0).and_then(|receiver_ty| {
                        infer_receiver_method(
                            mir_ty_to_stdlib_kind(receiver_ty.clone()),
                            method_name.as_str(),
                            &arg_kinds,
                        )
                        .map(|info| stdlib_kind_to_mir_ty(info.return_kind))
                    }),
                    _ => None,
                })
        }
        _ => None,
    }
}

fn stdlib_namespace(
    receiver: &Operand,
    namespace_locals: &HashMap<LocalId, String>,
) -> Option<String> {
    match receiver {
        Operand::Local(local) => namespace_locals.get(local).cloned(),
        Operand::Const(MirLit::Namespace(namespace)) => Some(namespace.clone()),
        _ => None,
    }
}

fn infer_rvalue_type(
    rhs: &Rvalue,
    program: &MirProgram,
    map: &HashMap<u32, MirTy>,
    namespace_locals: &HashMap<LocalId, String>,
    resolve_symbol: &mut impl FnMut(Symbol) -> Option<String>,
) -> MirTy {
    use fidan_ast::BinOp::*;

    match rhs {
        Rvalue::Binary { op, lhs, rhs } => {
            let lhs_ty = infer_operand_type(lhs, map);
            let rhs_ty = infer_operand_type(rhs, map);
            match op {
                Eq | NotEq | Lt | LtEq | Gt | GtEq => MirTy::Boolean,
                Add | Sub | Mul | Div | Rem | Pow
                    if matches!(lhs_ty, MirTy::Float) || matches!(rhs_ty, MirTy::Float) =>
                {
                    MirTy::Float
                }
                Add | Sub | Mul | Div | Rem | Pow
                    if matches!(lhs_ty, MirTy::Integer) && matches!(rhs_ty, MirTy::Integer) =>
                {
                    MirTy::Integer
                }
                Add | Sub | Mul | Div | Rem | Pow => MirTy::Dynamic,
                And | Or => MirTy::Boolean,
                BitXor | BitAnd | BitOr | Shl | Shr
                    if matches!(lhs_ty, MirTy::Integer) && matches!(rhs_ty, MirTy::Integer) =>
                {
                    MirTy::Integer
                }
                BitXor | BitAnd | BitOr | Shl | Shr => MirTy::Dynamic,
                Range | RangeInclusive => MirTy::Dynamic,
            }
        }
        Rvalue::Unary { op, operand } => {
            let operand_ty = infer_operand_type(operand, map);
            match op {
                fidan_ast::UnOp::Not => MirTy::Boolean,
                fidan_ast::UnOp::Neg | fidan_ast::UnOp::Pos
                    if matches!(operand_ty, MirTy::Error | MirTy::Dynamic) =>
                {
                    MirTy::Dynamic
                }
                fidan_ast::UnOp::Neg | fidan_ast::UnOp::Pos => operand_ty,
            }
        }
        Rvalue::Literal(MirLit::Int(_)) => MirTy::Integer,
        Rvalue::Literal(MirLit::Float(_)) => MirTy::Float,
        Rvalue::Literal(MirLit::Bool(_)) => MirTy::Boolean,
        Rvalue::Literal(MirLit::Str(_)) => MirTy::String,
        Rvalue::Use(operand) => infer_operand_type(operand, map),
        Rvalue::Call { callee, args } => {
            infer_call_result_ty(callee, program, args, map, namespace_locals, resolve_symbol)
                .unwrap_or(MirTy::Dynamic)
        }
        _ => MirTy::Dynamic,
    }
}

fn infer_operand_type(operand: &Operand, local_types: &HashMap<u32, MirTy>) -> MirTy {
    match operand {
        Operand::Local(local) => local_types.get(&local.0).cloned().unwrap_or(MirTy::Error),
        Operand::Const(MirLit::Int(_)) => MirTy::Integer,
        Operand::Const(MirLit::Float(_)) => MirTy::Float,
        Operand::Const(MirLit::Bool(_)) => MirTy::Boolean,
        Operand::Const(MirLit::Str(_)) => MirTy::String,
        Operand::Const(MirLit::Nothing) => MirTy::Nothing,
        _ => MirTy::Error,
    }
}

fn operand_stdlib_kind(operand: &Operand, local_types: &HashMap<u32, MirTy>) -> StdlibValueKind {
    match operand {
        Operand::Local(local) => local_types
            .get(&local.0)
            .cloned()
            .map(mir_ty_to_stdlib_kind)
            .unwrap_or(StdlibValueKind::Dynamic),
        Operand::Const(MirLit::Int(_)) => StdlibValueKind::Integer,
        Operand::Const(MirLit::Float(_)) => StdlibValueKind::Float,
        Operand::Const(MirLit::Bool(_)) => StdlibValueKind::Boolean,
        Operand::Const(MirLit::Str(_)) => StdlibValueKind::String,
        Operand::Const(MirLit::Nothing) => StdlibValueKind::Nothing,
        _ => StdlibValueKind::Dynamic,
    }
}

fn mir_ty_to_stdlib_kind(ty: MirTy) -> StdlibValueKind {
    match ty {
        MirTy::Integer => StdlibValueKind::Integer,
        MirTy::Float => StdlibValueKind::Float,
        MirTy::Boolean => StdlibValueKind::Boolean,
        MirTy::String => StdlibValueKind::String,
        MirTy::List(_) => StdlibValueKind::List,
        MirTy::Dict(_, _) => StdlibValueKind::Dict,
        MirTy::HashSet(_) => StdlibValueKind::HashSet,
        MirTy::Nothing => StdlibValueKind::Nothing,
        _ => StdlibValueKind::Dynamic,
    }
}

fn stdlib_kind_to_mir_ty(kind: StdlibValueKind) -> MirTy {
    match kind {
        StdlibValueKind::Integer => MirTy::Integer,
        StdlibValueKind::Float => MirTy::Float,
        StdlibValueKind::Boolean => MirTy::Boolean,
        StdlibValueKind::String => MirTy::String,
        StdlibValueKind::List => MirTy::List(Box::new(MirTy::Dynamic)),
        StdlibValueKind::Dict => MirTy::Dict(Box::new(MirTy::Dynamic), Box::new(MirTy::Dynamic)),
        StdlibValueKind::HashSet => MirTy::HashSet(Box::new(MirTy::Dynamic)),
        StdlibValueKind::Nothing => MirTy::Nothing,
        StdlibValueKind::Dynamic => MirTy::Dynamic,
    }
}

fn effective_return_ty(function: &MirFunction) -> MirTy {
    match &function.return_ty {
        MirTy::Nothing | MirTy::Error => {
            let has_value_return = function
                .blocks
                .iter()
                .any(|block| matches!(block.terminator, crate::Terminator::Return(Some(_))));
            if has_value_return {
                MirTy::Dynamic
            } else {
                function.return_ty.clone()
            }
        }
        other => other.clone(),
    }
}

fn is_primitive_scalar(ty: &MirTy) -> bool {
    matches!(ty, MirTy::Integer | MirTy::Float | MirTy::Boolean)
}

fn function_may_throw(function: &MirFunction, throw_map: &HashMap<FunctionId, bool>) -> bool {
    for block in &function.blocks {
        for instruction in &block.instructions {
            match instruction {
                Instr::Assign {
                    rhs: Rvalue::Call { callee, .. },
                    ..
                }
                | Instr::Call { callee, .. } => {
                    if callee_may_throw(callee, throw_map) {
                        return true;
                    }
                }
                Instr::GetField { .. }
                | Instr::SetField { .. }
                | Instr::GetIndex { .. }
                | Instr::SetIndex { .. }
                | Instr::AwaitPending { .. }
                | Instr::PushCatch(_)
                | Instr::PopCatch => {
                    return true;
                }
                _ => {}
            }
        }

        if matches!(block.terminator, Terminator::Throw { .. }) {
            return true;
        }
    }

    false
}

fn callee_may_throw(callee: &Callee, throw_map: &HashMap<FunctionId, bool>) -> bool {
    match callee {
        Callee::Fn(function_id) => throw_map.get(function_id).copied().unwrap_or(true),
        _ => true,
    }
}
