//! `fidan-typeck` — Symbol tables, type inference, type checking, parallel safety.
//!
//! # Entry point
//! ```ignore
//! let diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
//! // or, for HIR lowering:
//! let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
//! ```

mod scope;
mod types;
mod infer;
mod check;
mod parallel_check;

pub use types::FidanType;
pub use check::{TypeChecker, ObjectInfo, ActionInfo, ParamInfo};

use fidan_ast::{ExprId, Module};
use fidan_diagnostics::Diagnostic;
use fidan_lexer::{Symbol, SymbolInterner};
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Full type-information produced after a successful type-checking pass.
///
/// Carries everything HIR lowering needs to annotate every node with a
/// concrete type, plus the object/action registry for layout information.
#[derive(Debug)]
pub struct TypedModule {
    /// All diagnostics (errors + warnings) emitted during type-checking.
    pub diagnostics: Vec<Diagnostic>,
    /// Type of every expression, keyed by `ExprId`.
    pub expr_types: FxHashMap<ExprId, FidanType>,
    /// Class/object registry: layout (fields, methods, parent) per class name.
    pub objects: FxHashMap<Symbol, ObjectInfo>,
    /// Top-level action signatures (name → signature).
    pub actions: FxHashMap<Symbol, ActionInfo>,
}

/// Run all type-checking passes over `module` and return the resulting diagnostics.
///
/// Zero diagnostics means the module is well-typed.
pub fn typecheck(module: &Module, interner: Arc<SymbolInterner>) -> Vec<Diagnostic> {
    let mut tc = TypeChecker::new(interner, module.file);
    tc.check_module(module);
    tc.finish()
}

/// Run all type-checking passes and return the full `TypedModule` (type map + diagnostics).
///
/// Use this instead of `typecheck` when you need type annotations on HIR nodes.
pub fn typecheck_full(module: &Module, interner: Arc<SymbolInterner>) -> TypedModule {
    let mut tc = TypeChecker::new(interner, module.file);
    tc.check_module(module);
    tc.finish_typed()
}
