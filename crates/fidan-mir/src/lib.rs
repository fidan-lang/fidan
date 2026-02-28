//! `fidan-mir` — SSA/CFG Mid-Level IR types and HIR→MIR lowering.

mod mir;
mod lower;
mod display;

pub use mir::{MirProgram, MirFunction, BasicBlock, BlockId, LocalId, Operand, Instruction, Terminator};
pub use lower::lower_program;
