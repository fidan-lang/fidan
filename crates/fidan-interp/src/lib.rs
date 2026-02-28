//! `fidan-interp` — MIR tree-walking interpreter.

mod interp;
mod frame;
mod builtins;

pub use interp::Interpreter;
