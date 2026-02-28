use crate::ExprId;
use crate::stmt::CheckArm;
use fidan_lexer::Symbol;
use fidan_source::Span;

/// All expressions in the Fidan AST.
#[derive(Debug, Clone)]
pub enum Expr {
    // Literals
    IntLit {
        value: i64,
        span: Span,
    },
    FloatLit {
        value: f64,
        span: Span,
    },
    StrLit {
        value: String,
        span: Span,
    },
    BoolLit {
        value: bool,
        span: Span,
    },
    Nothing {
        span: Span,
    },

    // Names
    Ident {
        name: Symbol,
        span: Span,
    },
    This {
        span: Span,
    },
    Parent {
        span: Span,
    },

    // Operators
    Binary {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
    },
    Unary {
        op: UnOp,
        operand: ExprId,
        span: Span,
    },
    NullCoalesce {
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
    },

    // Compound
    Call {
        callee: ExprId,
        args: Vec<Arg>,
        span: Span,
    },
    Field {
        object: ExprId,
        field: Symbol,
        span: Span,
    },
    Index {
        object: ExprId,
        index: ExprId,
        span: Span,
    },

    // Assignment
    Assign {
        target: ExprId,
        value: ExprId,
        span: Span,
    },
    CompoundAssign {
        op: BinOp,
        target: ExprId,
        value: ExprId,
        span: Span,
    },

    // String interpolation
    StringInterp {
        parts: Vec<InterpPart>,
        span: Span,
    },

    // Spawn / await
    Spawn {
        expr: ExprId,
        span: Span,
    },
    Await {
        expr: ExprId,
        span: Span,
    },

    // Ternary: `then_val if condition else else_val`
    Ternary {
        condition: ExprId,
        then_val: ExprId,
        else_val: ExprId,
        span: Span,
    },

    // Collection literals
    List {
        elements: Vec<ExprId>,
        span: Span,
    },
    Dict {
        entries: Vec<(ExprId, ExprId)>,
        span: Span,
    },

    // Check-expression: `check x { pattern => expr, ... }`
    Check {
        scrutinee: ExprId,
        arms: Vec<CheckArm>,
        span: Span,
    },

    // Error recovery placeholder
    Error {
        span: Span,
    },
}

impl Expr {
    /// Return the source span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::IntLit { span, .. } => *span,
            Expr::FloatLit { span, .. } => *span,
            Expr::StrLit { span, .. } => *span,
            Expr::BoolLit { span, .. } => *span,
            Expr::Nothing { span } => *span,
            Expr::Ident { span, .. } => *span,
            Expr::This { span } => *span,
            Expr::Parent { span } => *span,
            Expr::Binary { span, .. } => *span,
            Expr::Unary { span, .. } => *span,
            Expr::NullCoalesce { span, .. } => *span,
            Expr::Call { span, .. } => *span,
            Expr::Field { span, .. } => *span,
            Expr::Index { span, .. } => *span,
            Expr::Assign { span, .. } => *span,
            Expr::CompoundAssign { span, .. } => *span,
            Expr::StringInterp { span, .. } => *span,
            Expr::Spawn { span, .. } => *span,
            Expr::Await { span, .. } => *span,
            Expr::Ternary { span, .. } => *span,
            Expr::List { span, .. } => *span,
            Expr::Dict { span, .. } => *span,
            Expr::Check { span, .. } => *span,
            Expr::Error { span } => *span,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<Symbol>, // `name:` for named args; None for positional
    pub value: ExprId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum InterpPart {
    Literal(String),
    Expr(ExprId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    BitXor,         // `^`
    BitAnd,         // `&`
    BitOr,          // `|`
    Shl,            // `<<`
    Shr,            // `>>`
    Range,          // `..`  exclusive (start..end)
    RangeInclusive, // `...` inclusive (start...end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}
