//! `fidan-hir` — High-Level IR types and AST→HIR lowering.

mod hir;
mod lower;

pub use hir::{HirModule, HirFunction, HirExpr, HirStmt};
pub use lower::lower_module;
