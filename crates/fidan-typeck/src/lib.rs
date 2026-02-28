//! `fidan-typeck` — Symbol tables, type inference, type checking, parallel safety.
//!
//! # Entry point
//! ```ignore
//! let diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
//! ```

mod scope;
mod types;
mod infer;
mod check;
mod parallel_check;

pub use types::FidanType;
pub use check::TypeChecker;

use fidan_ast::Module;
use fidan_diagnostics::Diagnostic;
use fidan_lexer::SymbolInterner;
use std::sync::Arc;

/// Run all type-checking passes over `module` and return the resulting diagnostics.
///
/// Zero diagnostics means the module is well-typed.
pub fn typecheck(module: &Module, interner: Arc<SymbolInterner>) -> Vec<Diagnostic> {
    let mut tc = TypeChecker::new(interner, module.file);
    tc.check_module(module);
    tc.finish()
}
