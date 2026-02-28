use fidan_source::Span;
use crate::ExprId;
use fidan_lexer::Symbol;

/// All expressions in the Fidan AST.
#[derive(Debug, Clone)]
pub enum Expr {
    // Literals
    IntLit   { value: i64,    span: Span },
    FloatLit { value: f64,    span: Span },
    StrLit   { value: String, span: Span },
    BoolLit  { value: bool,   span: Span },
    Nothing  { span: Span },

    // Names
    Ident    { name: Symbol, span: Span },
    This     { span: Span },
    Parent   { span: Span },

    // Operators
    Binary   { op: BinOp, lhs: ExprId, rhs: ExprId, span: Span },
    Unary    { op: UnOp,  operand: ExprId,           span: Span },
    NullCoalesce { lhs: ExprId, rhs: ExprId,         span: Span },

    // Compound
    Call  {
        callee:    ExprId,
        args:      Vec<Arg>,
        span:      Span,
    },
    Field { object: ExprId, field: Symbol, span: Span },
    Index { object: ExprId, index: ExprId, span: Span },

    // Assignment
    Assign { target: ExprId, value: ExprId, span: Span },
    CompoundAssign { op: BinOp, target: ExprId, value: ExprId, span: Span },

    // String interpolation
    StringInterp { parts: Vec<InterpPart>, span: Span },

    // Spawn / await
    Spawn { expr: ExprId, span: Span },
    Await { expr: ExprId, span: Span },
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name:  Option<Symbol>,   // `name:` for named args; None for positional
    pub value: ExprId,
    pub span:  Span,
}

#[derive(Debug, Clone)]
pub enum InterpPart {
    Literal(String),
    Expr(ExprId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem, Pow,
    Eq, NotEq, Lt, LtEq, Gt, GtEq,
    And, Or,
    Range,       // `..`
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}
