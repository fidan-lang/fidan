use crate::stmt::TypeExpr;
use crate::{ItemId, StmtId};
use fidan_lexer::Symbol;
use fidan_source::Span;

/// Top-level items (declarations).
#[derive(Debug, Clone)]
pub enum Item {
    /// `var name oftype T = expr`  /  `const var name oftype T = expr` at module scope
    VarDecl {
        name: fidan_lexer::Symbol,
        ty: Option<crate::stmt::TypeExpr>,
        init: Option<crate::ExprId>,
        /// `true` when declared with `const var`.
        is_const: bool,
        span: fidan_source::Span,
    },
    /// A top-level expression statement, e.g. `print("Hello")` or `main()`
    ExprStmt(crate::ExprId),
    /// A top-level assignment, e.g. `x = 1` or `obj.field = val`
    Assign {
        target: crate::ExprId,
        value: crate::ExprId,
        span: fidan_source::Span,
    },
    /// `var (a, b) = expr` — tuple destructuring at module scope
    Destructure {
        bindings: Vec<Symbol>,
        value: crate::ExprId,
        span: fidan_source::Span,
    },
    /// `object Name extends Parent { fields... methods... }`
    ObjectDecl {
        name: Symbol,
        parent: Option<Symbol>,
        fields: Vec<FieldDecl>,
        methods: Vec<ItemId>,
        span: Span,
    },
    /// `action Name with params returns T { body }`
    ActionDecl {
        name: Symbol,
        params: Vec<Param>,
        return_ty: Option<TypeExpr>,
        body: Vec<StmtId>,
        decorators: Vec<Decorator>,
        is_parallel: bool,
        span: Span,
    },
    /// `action Name extends ObjectName with params { body }`
    ExtensionAction {
        name: Symbol,
        extends: Symbol,
        params: Vec<Param>,
        return_ty: Option<TypeExpr>,
        body: Vec<StmtId>,
        decorators: Vec<Decorator>,
        is_parallel: bool,
        span: Span,
    },
    /// `use std.io` import / `export use std.io` re-export
    Use {
        path: Vec<Symbol>,
        alias: Option<Symbol>,
        re_export: bool,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: Symbol,
    pub ty: TypeExpr,
    pub required: bool,
    pub default: Option<crate::ExprId>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Symbol,
    pub ty: TypeExpr,
    pub required: bool,
    pub default: Option<crate::ExprId>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Decorator {
    pub name: Symbol,
    pub args: Vec<crate::ExprId>,
    pub span: Span,
}
