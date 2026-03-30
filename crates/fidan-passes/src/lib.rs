//! `fidan-passes` — MIR optimisation and analysis passes.

use fidan_mir::MirProgram;

/// Common interface for all MIR mutation passes.
pub trait Pass {
    fn run(&self, _p: &mut MirProgram) {}
}

mod constant_folding;
mod copy_propagation;
mod dead_code;
pub mod escape_analysis;
mod inlining;
pub mod null_safety;
pub mod parallel_check;
pub mod precompile_hints;
pub mod unawaited_pending;
mod unreachable_pruning;

pub use constant_folding::ConstantFolding;
pub use copy_propagation::CopyPropagation;
pub use dead_code::DeadCodeElimination;
pub use escape_analysis::EscapeInfo;
pub use inlining::Inlining;
pub use null_safety::NullSafetyDiag;
pub use parallel_check::ParallelRaceDiag;
pub use precompile_hints::SlowHintDiag;
pub use unawaited_pending::UnawaitedPendingDiag;
pub use unreachable_pruning::UnreachablePruning;

/// Check for data races in true parallel constructs (`parallel` / `parallel for`) (E0401).
pub fn check_parallel_races(
    prog: &MirProgram,
    interner: &fidan_lexer::SymbolInterner,
) -> Vec<ParallelRaceDiag> {
    parallel_check::check(prog, interner)
}

/// Check for `spawn` expressions whose `Pending` results are never `await`ed (W1004).
pub fn check_unawaited_pending(
    prog: &MirProgram,
    interner: &fidan_lexer::SymbolInterner,
) -> Vec<UnawaitedPendingDiag> {
    unawaited_pending::check(prog, interner)
}

/// Check for definitely-nothing values used in non-null-safe contexts (W2006).
pub fn check_null_safety(
    prog: &MirProgram,
    interner: &fidan_lexer::SymbolInterner,
) -> Vec<NullSafetyDiag> {
    null_safety::check(prog, interner)
}

/// Emit compile-time "Why Is This Slow?" hints (W5001, W5003).
pub fn check_slow_hints(
    prog: &MirProgram,
    interner: &fidan_lexer::SymbolInterner,
) -> Vec<SlowHintDiag> {
    precompile_hints::check(prog, interner)
}

/// Run escape analysis on all functions in `program`.
///
/// Returns one `EscapeInfo` per function (indexed by `FunctionId.0`).
/// The analysis identifies locals that provably do not outlive the current
/// stack frame; these are candidates for clone-elision and future stack
/// allocation.
pub fn run_escape_analysis(program: &MirProgram) -> Vec<EscapeInfo> {
    escape_analysis::analyze(program)
}

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

/// Run an optimisation pipeline that preserves user call boundaries for rich traces.
///
/// This intentionally skips inlining so `--trace full` can show the real source-level
/// call chain instead of a flattened optimized stack. We still keep the cheap cleanup
/// passes so debug runs stay reasonably tidy and fast.
pub fn run_preserving_call_frames(program: &mut MirProgram) {
    ConstantFolding.run(program);
    CopyPropagation.run(program);
    DeadCodeElimination.run(program);
    UnreachablePruning.run(program);
}
