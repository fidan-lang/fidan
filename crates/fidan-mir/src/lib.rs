//! `fidan-mir` — SSA/CFG Mid-Level IR types and HIR→MIR lowering.

mod display;
mod lower;
mod mir;

pub use display::print_program;
pub use lower::lower_program;
pub use mir::{
    BasicBlock, BlockId, Callee, FunctionId, Instr, LocalId, MirFunction, MirLit, MirParam,
    MirProgram, MirStringPart, MirTy, Operand, PhiNode, Rvalue, Terminator,
};
