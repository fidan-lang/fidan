//! `fidan-passes` — MIR optimisation passes.

use fidan_mir::MirProgram;

/// Common interface for all MIR passes.
pub trait Pass { fn run(&self, _p: &mut MirProgram) {} }

mod constant_folding;
mod dead_code;
mod copy_propagation;
mod unreachable_pruning;

pub use constant_folding::ConstantFolding;
pub use dead_code::DeadCodeElimination;
pub use copy_propagation::CopyPropagation;
pub use unreachable_pruning::UnreachablePruning;

/// Run all optimisation passes in the standard order.
pub fn run_all(program: &mut MirProgram) {
    ConstantFolding.run(program);
    CopyPropagation.run(program);
    DeadCodeElimination.run(program);
    UnreachablePruning.run(program);
}
