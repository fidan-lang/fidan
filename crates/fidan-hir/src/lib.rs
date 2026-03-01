//! `fidan-hir` — High-Level IR types and AST→HIR lowering.

mod hir;
mod lower;

pub use hir::{
    HirArg, HirCatchClause, HirCheckArm, HirCheckExprArm, HirElseIf, HirExpr, HirExprKind,
    HirField, HirFunction, HirGlobal, HirInterpPart, HirModule, HirObject, HirParam, HirStmt,
    HirTask,
};
pub use lower::lower_module;
