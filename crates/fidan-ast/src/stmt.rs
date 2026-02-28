use fidan_source::Span;
use crate::{ExprId, StmtId};
use fidan_lexer::Symbol;

/// All statements in the Fidan AST.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `var name oftype T = expr`
    VarDecl {
        name:   Symbol,
        ty:     Option<TypeExpr>,
        init:   Option<ExprId>,
        span:   Span,
    },
    /// `set name = expr` / `set name.field = expr`
    Assign {
        target: ExprId,
        value:  ExprId,
        span:   Span,
    },
    Expr { expr: ExprId, span: Span },
    Return { value: Option<ExprId>, span: Span },
    Break  { span: Span },
    Continue { span: Span },
    If {
        condition:  ExprId,
        then_body:  Vec<StmtId>,
        else_ifs:   Vec<ElseIf>,
        else_body:  Option<Vec<StmtId>>,
        span:       Span,
    },
    When {
        scrutinee: ExprId,
        arms:      Vec<WhenArm>,
        span:      Span,
    },
    For {
        binding:    Symbol,
        iterable:   ExprId,
        body:       Vec<StmtId>,
        span:       Span,
    },
    While {
        condition: ExprId,
        body:      Vec<StmtId>,
        span:      Span,
    },
    Attempt {
        body:      Vec<StmtId>,
        catches:   Vec<CatchClause>,
        otherwise: Option<Vec<StmtId>>,
        finally:   Option<Vec<StmtId>>,
        span:      Span,
    },
    /// `parallel for item in collection { ... }`
    ParallelFor {
        binding:  Symbol,
        iterable: ExprId,
        body:     Vec<StmtId>,
        span:     Span,
    },
    /// `concurrent { task ... task ... }` or `parallel { task ... task ... }`
    ConcurrentBlock {
        is_parallel: bool,
        tasks:       Vec<Task>,
        span:        Span,
    },
    /// `panic expr` / `throw expr`
    Panic { value: ExprId, span: Span },
    /// Error recovery placeholder inserted when the parser cannot produce a valid statement.
    Error { span: Span },
}

#[derive(Debug, Clone)]
pub struct ElseIf {
    pub condition: ExprId,
    pub body:      Vec<StmtId>,
    pub span:      Span,
}

#[derive(Debug, Clone)]
pub struct WhenArm {
    pub pattern: ExprId,
    pub body:    Vec<StmtId>,
    pub span:    Span,
}

#[derive(Debug, Clone)]
pub struct CatchClause {
    pub binding: Option<Symbol>,
    pub ty:      Option<TypeExpr>,
    pub body:    Vec<StmtId>,
    pub span:    Span,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub name: Option<Symbol>,
    pub body: Vec<StmtId>,
    pub span: Span,
}

/// A syntactic type expression (unresolved).
#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named   { name: Symbol, span: Span },
    Oftype  { base: Box<TypeExpr>, param: Box<TypeExpr>, span: Span },
    Dynamic { span: Span },
    Nothing { span: Span },
}

impl TypeExpr {
    /// Byte offset of the last character of this type expression.
    pub fn span_end(&self) -> u32 {
        match self {
            TypeExpr::Named   { span, .. } => span.end,
            TypeExpr::Oftype  { span, .. } => span.end,
            TypeExpr::Dynamic { span }     => span.end,
            TypeExpr::Nothing { span }     => span.end,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named   { span, .. } => *span,
            TypeExpr::Oftype  { span, .. } => *span,
            TypeExpr::Dynamic { span }     => *span,
            TypeExpr::Nothing { span }     => *span,
        }
    }
}
