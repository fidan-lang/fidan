use crate::model::BackendPayload;
use anyhow::{Result, bail};
use fidan_lexer::Symbol;
use fidan_mir::{
    Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirGlobal, MirLit, MirProgram,
    MirTy, Operand, Rvalue, Terminator,
};
use fidan_stdlib::{StdlibValueKind, infer_receiver_method, infer_stdlib_method};
use std::collections::HashMap;
use std::env;

pub struct BackendContext<'a> {
    payload: &'a BackendPayload,
}

impl<'a> BackendContext<'a> {
    pub fn new(payload: &'a BackendPayload) -> Self {
        Self { payload }
    }

    pub fn program(&self) -> &'a MirProgram {
        &self.payload.program
    }

    pub fn program_target_triple(&self) -> String {
        let os = match env::consts::OS {
            "windows" => "pc-windows-msvc",
            "macos" => "apple-darwin",
            "linux" => "unknown-linux-gnu",
            _ => "unknown-linux-gnu",
        };
        let arch = env::consts::ARCH;
        format!("{arch}-{os}")
    }

    pub fn symbol_name(&self, symbol: Symbol) -> Result<&'a str> {
        if let Some(entry) = self.payload.symbols.get(symbol.0 as usize) {
            Ok(entry.as_str())
        } else {
            bail!("MIR payload references missing symbol id {}", symbol.0)
        }
    }

    pub fn function(&self, function_id: FunctionId) -> Result<&'a MirFunction> {
        self.payload
            .program
            .functions
            .get(function_id.0 as usize)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MIR payload references missing function id {}",
                    function_id.0
                )
            })
    }

    pub fn global(&self, global_id: GlobalId) -> Result<&'a MirGlobal> {
        self.payload
            .program
            .globals
            .get(global_id.0 as usize)
            .ok_or_else(|| {
                anyhow::anyhow!("MIR payload references missing global id {}", global_id.0)
            })
    }

    pub fn init_function(&self) -> Option<&'a MirFunction> {
        self.payload.program.functions.first()
    }

    pub fn main_function(&self) -> Option<&'a MirFunction> {
        self.payload.program.functions.iter().find(|function| {
            self.symbol_name(function.name)
                .map(|name| name == "main")
                .unwrap_or(false)
        })
    }

    pub fn mangled_function_name(&self, function: &MirFunction) -> Result<String> {
        let name = self.symbol_name(function.name)?;
        Ok(mangle_fn(name, function.id.0))
    }

    pub fn effective_return_ty(&self, function: &MirFunction) -> MirTy {
        match &function.return_ty {
            MirTy::Nothing | MirTy::Error => {
                let has_value_return = function
                    .blocks
                    .iter()
                    .any(|block| matches!(block.terminator, Terminator::Return(Some(_))));
                if has_value_return {
                    MirTy::Dynamic
                } else {
                    function.return_ty.clone()
                }
            }
            other => other.clone(),
        }
    }

    pub fn build_local_type_map(&self, function: &MirFunction) -> HashMap<u32, MirTy> {
        let mut map = HashMap::new();
        let global_ns_map = self.build_global_namespace_map();
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
                    Instr::Assign { dest, ty, rhs } => {
                        let effective_ty = if matches!(ty, MirTy::Dynamic | MirTy::Error) {
                            self.infer_rvalue_type(rhs, &map, &namespace_locals)
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
                                self.infer_call_result_ty(callee, args, &map, &namespace_locals)
                            })
                            .unwrap_or(MirTy::Dynamic);
                        map.insert(dest.0, inferred_ty);
                    }
                    Instr::GetField { dest, .. } | Instr::GetIndex { dest, .. } => {
                        map.insert(dest.0, MirTy::Dynamic);
                        namespace_locals.remove(dest);
                    }
                    Instr::LoadGlobal { dest, global } => {
                        map.entry(dest.0).or_insert(MirTy::Dynamic);
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

        map
    }

    fn build_global_namespace_map(&self) -> HashMap<GlobalId, String> {
        let mut global_ns_map = HashMap::new();
        for (index, global) in self.payload.program.globals.iter().enumerate() {
            let Ok(global_name) = self.symbol_name(global.name) else {
                continue;
            };
            for decl in &self.payload.program.use_decls {
                if decl.is_stdlib
                    && decl.specific_names.is_none()
                    && global_name == decl.alias.as_str()
                {
                    global_ns_map.insert(GlobalId(index as u32), decl.module.clone());
                }
            }
        }
        global_ns_map
    }

    fn infer_call_result_ty(
        &self,
        callee: &Callee,
        args: &[Operand],
        local_types: &HashMap<u32, MirTy>,
        namespace_locals: &HashMap<LocalId, String>,
    ) -> Option<MirTy> {
        match callee {
            Callee::Method {
                receiver: Operand::Local(receiver),
                method,
            } => {
                let method_name = self.symbol_name(*method).ok()?;
                let arg_kinds = args
                    .iter()
                    .map(|arg| operand_stdlib_kind(arg, local_types))
                    .collect::<Vec<_>>();

                namespace_locals
                    .get(receiver)
                    .and_then(|namespace| {
                        infer_stdlib_method(namespace.as_str(), method_name, &arg_kinds)
                            .map(|info| stdlib_kind_to_mir_ty(info.return_kind))
                    })
                    .or_else(|| {
                        local_types.get(&receiver.0).and_then(|receiver_ty| {
                            infer_receiver_method(
                                mir_ty_to_stdlib_kind(receiver_ty.clone()),
                                method_name,
                                &arg_kinds,
                            )
                            .map(|info| stdlib_kind_to_mir_ty(info.return_kind))
                        })
                    })
            }
            _ => None,
        }
    }

    fn infer_rvalue_type(
        &self,
        rhs: &Rvalue,
        local_types: &HashMap<u32, MirTy>,
        namespace_locals: &HashMap<LocalId, String>,
    ) -> MirTy {
        use fidan_ast::BinOp::*;

        match rhs {
            Rvalue::Binary { op, lhs, rhs } => {
                let lhs_ty = infer_operand_type(lhs, local_types);
                let rhs_ty = infer_operand_type(rhs, local_types);
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
                let operand_ty = infer_operand_type(operand, local_types);
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
            Rvalue::Use(operand) => infer_operand_type(operand, local_types),
            Rvalue::Call { callee, args } => self
                .infer_call_result_ty(callee, args, local_types, namespace_locals)
                .unwrap_or(MirTy::Dynamic),
            _ => MirTy::Dynamic,
        }
    }
}

pub fn mangle_fn(name: &str, id: u32) -> String {
    if name == "main" {
        return "fdn_main".to_owned();
    }
    if name == "__init__" || id == 0 {
        return "fdn_init".to_owned();
    }
    format!("fdn_{name}_{id}")
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
        StdlibValueKind::Nothing => MirTy::Nothing,
        StdlibValueKind::Dynamic => MirTy::Dynamic,
    }
}

#[cfg(test)]
mod tests {
    use super::{BackendContext, mangle_fn};
    use crate::model::BackendPayload;
    use fidan_lexer::Symbol;
    use fidan_mir::{
        BasicBlock, BlockId, FunctionId, Instr, LocalId, MirFunction, MirLit, MirProgram, MirTy,
        Operand, Rvalue, Terminator,
    };

    #[test]
    fn mangles_main_and_init_consistently() {
        assert_eq!(mangle_fn("main", 4), "fdn_main");
        assert_eq!(mangle_fn("__init__", 12), "fdn_init");
        assert_eq!(mangle_fn("worker", 7), "fdn_worker_7");
    }

    #[test]
    fn promotes_nothing_return_when_function_returns_a_value() {
        let payload = sample_payload(sample_function_with_value_return());
        let backend = BackendContext::new(&payload);
        let function = &backend.program().functions[0];
        assert_eq!(backend.effective_return_ty(function), MirTy::Dynamic);
    }

    #[test]
    fn infers_scalar_local_types_from_literals_and_binary_ops() {
        let payload = sample_payload(sample_inference_function());
        let backend = BackendContext::new(&payload);
        let function = &backend.program().functions[0];
        let local_types = backend.build_local_type_map(function);

        assert_eq!(local_types.get(&0), Some(&MirTy::Integer));
        assert_eq!(local_types.get(&1), Some(&MirTy::Integer));
        assert_eq!(local_types.get(&2), Some(&MirTy::Boolean));
    }

    fn sample_payload(function: MirFunction) -> BackendPayload {
        BackendPayload {
            program: MirProgram {
                functions: vec![function],
                ..MirProgram::default()
            },
            symbols: vec!["main".to_owned()],
        }
    }

    fn sample_function_with_value_return() -> MirFunction {
        MirFunction {
            id: FunctionId(0),
            name: Symbol(0),
            params: vec![],
            return_ty: MirTy::Nothing,
            blocks: vec![BasicBlock {
                id: BlockId(0),
                phis: vec![],
                instructions: vec![],
                terminator: Terminator::Return(Some(Operand::Const(MirLit::Int(1)))),
            }],
            local_count: 0,
            precompile: false,
            extern_decl: None,
            custom_decorators: vec![],
        }
    }

    fn sample_inference_function() -> MirFunction {
        MirFunction {
            id: FunctionId(0),
            name: Symbol(0),
            params: vec![],
            return_ty: MirTy::Nothing,
            blocks: vec![BasicBlock {
                id: BlockId(0),
                phis: vec![],
                instructions: vec![
                    Instr::Assign {
                        dest: LocalId(0),
                        ty: MirTy::Dynamic,
                        rhs: Rvalue::Literal(MirLit::Int(41)),
                    },
                    Instr::Assign {
                        dest: LocalId(1),
                        ty: MirTy::Dynamic,
                        rhs: Rvalue::Binary {
                            op: fidan_ast::BinOp::Add,
                            lhs: Operand::Local(LocalId(0)),
                            rhs: Operand::Const(MirLit::Int(1)),
                        },
                    },
                    Instr::Assign {
                        dest: LocalId(2),
                        ty: MirTy::Dynamic,
                        rhs: Rvalue::Unary {
                            op: fidan_ast::UnOp::Not,
                            operand: Operand::Const(MirLit::Bool(false)),
                        },
                    },
                ],
                terminator: Terminator::Return(None),
            }],
            local_count: 3,
            precompile: false,
            extern_decl: None,
            custom_decorators: vec![],
        }
    }
}
