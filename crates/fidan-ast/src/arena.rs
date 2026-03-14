/// An opaque index into the `AstArena` expression pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

/// An opaque index into the `AstArena` statement pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StmtId(pub u32);

/// An opaque index into the `AstArena` item pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemId(pub u32);

/// Central arena for all AST nodes. Lives for the duration of a compilation session.
#[derive(Debug, Default)]
pub struct AstArena {
    pub exprs: Vec<crate::Expr>,
    pub stmts: Vec<crate::Stmt>,
    pub items: Vec<crate::Item>,
}

impl AstArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc_expr(&mut self, e: crate::Expr) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(e);
        id
    }

    pub fn alloc_stmt(&mut self, s: crate::Stmt) -> StmtId {
        let id = StmtId(self.stmts.len() as u32);
        self.stmts.push(s);
        id
    }

    pub fn alloc_item(&mut self, i: crate::Item) -> ItemId {
        let id = ItemId(self.items.len() as u32);
        self.items.push(i);
        id
    }

    pub fn get_expr(&self, id: ExprId) -> &crate::Expr {
        &self.exprs[id.0 as usize]
    }
    pub fn get_stmt(&self, id: StmtId) -> &crate::Stmt {
        &self.stmts[id.0 as usize]
    }
    pub fn get_item(&self, id: ItemId) -> &crate::Item {
        &self.items[id.0 as usize]
    }
}
