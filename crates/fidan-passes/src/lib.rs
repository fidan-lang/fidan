//! `fidan-passes` — MIR optimisation passes.

use fidan_mir::MirProgram;

/// Common interface for all MIR passes.
pub trait Pass {
    fn run(&self, _p: &mut MirProgram) {}
}

mod constant_folding;
mod copy_propagation;
mod dead_code;
mod inlining;
mod unreachable_pruning;

pub use constant_folding::ConstantFolding;
pub use copy_propagation::CopyPropagation;
pub use dead_code::DeadCodeElimination;
pub use inlining::Inlining;
pub use unreachable_pruning::UnreachablePruning;

/// Run all optimisation passes in the standard order.
///
/// Pass ordering rationale:
///   1. `ConstantFolding` — fold literals and strength-reduce identities first
///      so that inlining reveals more constant call arguments.
///   2. `Inlining` — replace small direct calls with their bodies;
///      newly inlined code may contain more foldable constants.
///   3. `ConstantFolding` (second run) — fold constants exposed by inlining.
///   4. `CopyPropagation` — forward copies produced by the inliner.
///   5. `DeadCodeElimination` — remove parameter temporaries and other dead
///      locals left behind after inlining + propagation.
///   6. `UnreachablePruning` — strip blocks after unconditional returns.
pub fn run_all(program: &mut MirProgram) {
    ConstantFolding.run(program);
    Inlining.run(program);
    ConstantFolding.run(program); // second run: fold constants exposed by inlining
    CopyPropagation.run(program);
    DeadCodeElimination.run(program);
    UnreachablePruning.run(program);
}
