use crate::ExprId;
use crate::stmt::CheckArm;
use fidan_lexer::Symbol;
use fidan_source::Span;
use serde::{Deserialize, Serialize};

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
    /// Tuple literal: `(a, b, c)`
    Tuple {
        elements: Vec<ExprId>,
        span: Span,
    },

    // Check-expression: `check x { pattern => expr, ... }`
    Check {
        scrutinee: ExprId,
        arms: Vec<CheckArm>,
        span: Span,
    },

    /// Slice expression: `target[start..end]`, `target[..end]`, `target[start..]`,
    /// `target[..]`, any of the above with `step N`.
    /// `inclusive` means `...` (inclusive end bound).
    /// `start`, `end`, and `step` are all optional.
    Slice {
        target: ExprId,
        start: Option<ExprId>,
        end: Option<ExprId>,
        inclusive: bool,
        step: Option<ExprId>,
        span: Span,
    },

    /// List comprehension: `[element for binding in iterable]`
    /// or `[element for binding in iterable if filter]`.
    ListComp {
        element: ExprId,
        binding: Symbol,
        iterable: ExprId,
        filter: Option<ExprId>,
        span: Span,
    },

    /// Dict comprehension: `{key: value for binding in iterable}`
    /// or `{key: value for binding in iterable if filter}`.
    DictComp {
        key: ExprId,
        value: ExprId,
        binding: Symbol,
        iterable: ExprId,
        filter: Option<ExprId>,
        span: Span,
    },

    /// Inline anonymous action expression: `action with (params) { body }`.
    /// Used as a first-class value (e.g. passed to `forEach` or `Shared.update`).
    Lambda {
        params: Vec<crate::item::Param>,
        return_ty: Option<crate::TypeExpr>,
        body: Vec<crate::StmtId>,
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
            Expr::Tuple { span, .. } => *span,
            Expr::Check { span, .. } => *span,
            Expr::Slice { span, .. } => *span,
            Expr::ListComp { span, .. } => *span,
            Expr::DictComp { span, .. } => *span,
            Expr::Lambda { span, .. } => *span,
            Expr::Error { span } => *span,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<Symbol>, // named arg when present; None for positional
    pub value: ExprId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum InterpPart {
    Literal(String),
    Expr(ExprId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnOp {
    Pos,
    Neg,
    Not,
}
