//! `fidan-interp` — AST-walking interpreter (Phase 5 bootstrap).
//!
//! Evaluates a parsed, type-checked `Module` directly.
//! When HIR/MIR lowering is complete (Phase 6+), this will be replaced by
//! a proper SSA/MIR walker.

mod builtins;
mod env;
mod frame;
mod interp;
mod mir_interp;

pub use interp::{ReplState, RunError, new_repl_state, run, run_repl_line};
pub use mir_interp::run_mir;
