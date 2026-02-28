use crate::{Expr, Stmt, Item, ExprId, StmtId, ItemId};

/// Visitor trait for AST traversal. Override only the methods you need.
pub trait AstVisitor {
    fn visit_expr(&mut self, _id: ExprId, _expr: &Expr) {}
    fn visit_stmt(&mut self, _id: StmtId, _stmt: &Stmt) {}
    fn visit_item(&mut self, _id: ItemId, _item: &Item) {}
}
