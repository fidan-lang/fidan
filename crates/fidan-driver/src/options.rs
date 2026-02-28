use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub input:  PathBuf,
    pub output: Option<PathBuf>,
    pub mode:   ExecutionMode,
    pub emit:   Vec<EmitKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode { Interpret, Build, Test }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitKind { Tokens, Ast, Hir, Mir }
