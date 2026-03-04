//! `fidan-interp` — MIR interpreter (Phase 6).
//!
//! Executes a compiled `MirProgram` by walking its SSA/CFG representation.
//! The former AST-walking bootstrap interpreter (Phase 5) has been removed;
//! all execution paths now go through the MIR machine.

mod bootstrap;
mod builtins;
mod mir_interp;

pub use mir_interp::{
    MirMachine, MirReplState, RunError, TestResult, TraceFrame, run_mir, run_mir_repl_line,
    run_mir_with_jit, run_tests,
};
