use crate::model::BackendPayload;
use anyhow::{Result, bail};
use fidan_lexer::Symbol;
use fidan_mir::{
    FunctionId, GlobalId, MirFunction, MirGlobal, MirProgram, MirTy, Terminator,
    collect_effective_local_types, collect_may_throw_functions,
};
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
        collect_effective_local_types(function, &self.payload.program, |symbol| {
            self.payload.symbols.get(symbol.0 as usize).cloned()
        })
    }

    pub fn build_function_throw_map(&self) -> HashMap<FunctionId, bool> {
        collect_may_throw_functions(&self.payload.program)
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

#[cfg(test)]
mod tests {
    use super::{BackendContext, mangle_fn};
    use crate::model::BackendPayload;
    use fidan_lexer::Symbol;
    use fidan_mir::{
        BasicBlock, BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirGlobal,
        MirLit, MirProgram, MirTy, MirUseDecl, Operand, Rvalue, Terminator,
    };
    use fidan_source::Span;

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

    #[test]
    fn infers_stdlib_method_call_result_types() {
        let payload = BackendPayload {
            program: MirProgram {
                functions: vec![MirFunction {
                    id: FunctionId(0),
                    name: Symbol(0),
                    params: vec![],
                    return_ty: MirTy::Nothing,
                    blocks: vec![BasicBlock {
                        id: BlockId(0),
                        phis: vec![],
                        instructions: vec![
                            Instr::LoadGlobal {
                                dest: LocalId(0),
                                global: GlobalId(0),
                            },
                            Instr::Assign {
                                dest: LocalId(1),
                                ty: MirTy::Dynamic,
                                rhs: Rvalue::Binary {
                                    op: fidan_ast::BinOp::Add,
                                    lhs: Operand::Const(MirLit::Int(2)),
                                    rhs: Operand::Const(MirLit::Int(3)),
                                },
                            },
                            Instr::Call {
                                dest: Some(LocalId(2)),
                                result_ty: None,
                                callee: Callee::Method {
                                    receiver: Operand::Local(LocalId(0)),
                                    method: Symbol(2),
                                },
                                args: vec![Operand::Local(LocalId(1))],
                                span: Span::default(),
                            },
                        ],
                        terminator: Terminator::Return(None),
                    }],
                    local_count: 3,
                    precompile: false,
                    extern_decl: None,
                    custom_decorators: vec![],
                }],
                globals: vec![MirGlobal {
                    name: Symbol(1),
                    ty: MirTy::Dynamic,
                }],
                use_decls: vec![MirUseDecl {
                    module: "math".to_owned(),
                    alias: "math".to_owned(),
                    specific_names: None,
                    re_export: false,
                    is_stdlib: true,
                }],
                ..MirProgram::default()
            },
            symbols: vec!["main".to_owned(), "math".to_owned(), "sqrt".to_owned()],
        };
        let backend = BackendContext::new(&payload);
        let function = &backend.program().functions[0];
        let local_types = backend.build_local_type_map(function);

        assert_eq!(local_types.get(&2), Some(&MirTy::Float));
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
