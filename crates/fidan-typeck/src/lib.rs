//! `fidan-typeck` — Symbol tables, type inference, type checking, parallel safety.

mod scope;
mod types;
mod infer;
mod check;
mod parallel_check;

pub use types::FidanType;
pub use scope::TypeChecker;
