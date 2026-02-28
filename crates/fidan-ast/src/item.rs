use fidan_source::Span;
use crate::{StmtId, ItemId};
use crate::stmt::TypeExpr;
use fidan_lexer::Symbol;

/// Top-level items (declarations).
#[derive(Debug, Clone)]
pub enum Item {
    /// `object Name extends Parent { fields... methods... }`
    ObjectDecl {
        name:    Symbol,
        parent:  Option<Symbol>,
        fields:  Vec<FieldDecl>,
        methods: Vec<ItemId>,
        span:    Span,
    },
    /// `action Name with params returns T { body }`
    ActionDecl {
        name:       Symbol,
        params:     Vec<Param>,
        return_ty:  Option<TypeExpr>,
        body:       Vec<StmtId>,
        decorators: Vec<Decorator>,
        is_parallel: bool,
        span:       Span,
    },
    /// `action Name extends ObjectName with params { body }`
    ExtensionAction {
        name:       Symbol,
        extends:    Symbol,
        params:     Vec<Param>,
        return_ty:  Option<TypeExpr>,
        body:       Vec<StmtId>,
        decorators: Vec<Decorator>,
        is_parallel: bool,
        span:       Span,
    },
    /// `use std.io` import
    Use {
        path: Vec<Symbol>,
        alias: Option<Symbol>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name:     Symbol,
    pub ty:       TypeExpr,
    pub required: bool,
    pub default:  Option<crate::ExprId>,
    pub span:     Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name:     Symbol,
    pub ty:       TypeExpr,
    pub required: bool,
    pub default:  Option<crate::ExprId>,
    pub span:     Span,
}

#[derive(Debug, Clone)]
pub struct Decorator {
    pub name: Symbol,
    pub args: Vec<crate::ExprId>,
    pub span: Span,
}
