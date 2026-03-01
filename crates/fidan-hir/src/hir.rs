// fidan-hir/src/hir.rs
//
// High-Level Intermediate Representation.
//
// HIR is structurally close to the source AST but:
//   • Every expression carries its concrete `FidanType` (no Unknown / inference vars).
//   • All synonym tokens have been resolved to canonical forms.
//   • Ternary `val if cond else fb` is desugared to `HirExprKind::IfExpr`.
//   • `is not` patterns are represented as `BinOp::NotEq` (already done by parser).
//   • All type annotations are filled in (no `TypeExpr` — only `FidanType`).
//   • String interpolation parts carry typed HirExprs.
//
// HIR is still tree-shaped and NOT in SSA form; that is MIR's job.

use fidan_ast::{BinOp, UnOp};
use fidan_lexer::Symbol;
use fidan_source::Span;
use fidan_typeck::FidanType;

// ── Module ─────────────────────────────────────────────────────────────────────

/// The HIR representation of a source module.
#[derive(Debug)]
pub struct HirModule {
    /// Object / class definitions.
    pub objects: Vec<HirObject>,
    /// All action declarations (top-level functions + extension actions).
    pub functions: Vec<HirFunction>,
    /// Top-level variable declarations (module-scoped globals).
    pub globals: Vec<HirGlobal>,
    /// Top-level executable statements (printed / run in order).
    pub init_stmts: Vec<HirStmt>,
}

// ── Globals ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct HirGlobal {
    pub name: Symbol,
    pub ty: FidanType,
    pub init: Option<HirExpr>,
    pub is_const: bool,
    pub span: Span,
}

// ── Objects ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct HirObject {
    pub name: Symbol,
    pub parent: Option<Symbol>,
    pub fields: Vec<HirField>,
    /// Methods defined directly inside this object block.
    pub methods: Vec<HirFunction>,
    pub span: Span,
}

#[derive(Debug)]
pub struct HirField {
    pub name: Symbol,
    pub ty: FidanType,
    pub required: bool,
    pub default: Option<HirExpr>,
    pub span: Span,
}

// ── Functions ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct HirFunction {
    pub name: Symbol,
    /// For extension actions: the object type this action extends.
    pub extends: Option<Symbol>,
    pub params: Vec<HirParam>,
    pub return_ty: FidanType,
    pub body: Vec<HirStmt>,
    pub is_parallel: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct HirParam {
    pub name: Symbol,
    pub ty: FidanType,
    pub required: bool,
    pub default: Option<HirExpr>,
    pub span: Span,
}

// ── Statements ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum HirStmt {
    /// `var name oftype T = expr`  /  `const var name = expr`
    VarDecl {
        name: Symbol,
        ty: FidanType,
        init: Option<HirExpr>,
        is_const: bool,
        span: Span,
    },
    /// `var (a, b) = expr` — tuple destructuring.
    Destructure {
        bindings: Vec<Symbol>,
        binding_tys: Vec<FidanType>,
        value: HirExpr,
        span: Span,
    },
    /// `set name = expr` / `set name.field = expr`
    Assign {
        target: HirExpr,
        value: HirExpr,
        span: Span,
    },
    /// Bare expression used as a statement (e.g. `print("hi")`).
    Expr(HirExpr),
    Return {
        value: Option<HirExpr>,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    /// `if cond { ... } otherwise when cond2 { ... } otherwise { ... }`
    If {
        condition: HirExpr,
        then_body: Vec<HirStmt>,
        else_ifs: Vec<HirElseIf>,
        else_body: Option<Vec<HirStmt>>,
        span: Span,
    },
    /// `check scrutinee { pattern => stmts ... }`
    Check {
        scrutinee: HirExpr,
        arms: Vec<HirCheckArm>,
        span: Span,
    },
    /// Sequential `for item in collection { ... }`
    For {
        binding: Symbol,
        binding_ty: FidanType,
        iterable: HirExpr,
        body: Vec<HirStmt>,
        span: Span,
    },
    While {
        condition: HirExpr,
        body: Vec<HirStmt>,
        span: Span,
    },
    /// `attempt { ... } catch err { ... } finally { ... }`
    Attempt {
        body: Vec<HirStmt>,
        catches: Vec<HirCatchClause>,
        otherwise: Option<Vec<HirStmt>>,
        finally: Option<Vec<HirStmt>>,
        span: Span,
    },
    /// `panic expr` / `throw expr`
    Panic {
        value: HirExpr,
        span: Span,
    },
    /// `parallel for item in collection { ... }`
    ParallelFor {
        binding: Symbol,
        binding_ty: FidanType,
        iterable: HirExpr,
        body: Vec<HirStmt>,
        span: Span,
    },
    /// `concurrent { task ... }` or `parallel { task ... task ... }`
    ConcurrentBlock {
        is_parallel: bool,
        tasks: Vec<HirTask>,
        span: Span,
    },
    /// Error-recovery placeholder.
    Error {
        span: Span,
    },
}

#[derive(Debug)]
pub struct HirElseIf {
    pub condition: HirExpr,
    pub body: Vec<HirStmt>,
    pub span: Span,
}

#[derive(Debug)]
pub struct HirCheckArm {
    pub pattern: HirExpr,
    pub body: Vec<HirStmt>,
    pub span: Span,
}

#[derive(Debug)]
pub struct HirCatchClause {
    pub binding: Option<Symbol>,
    pub ty: FidanType,
    pub body: Vec<HirStmt>,
    pub span: Span,
}

#[derive(Debug)]
pub struct HirTask {
    pub name: Option<Symbol>,
    pub body: Vec<HirStmt>,
    pub span: Span,
}

// ── Expressions ───────────────────────────────────────────────────────────────

/// A HIR expression: an expression kind plus its inferred type and source span.
#[derive(Debug)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: FidanType,
    pub span: Span,
}

#[derive(Debug)]
pub enum HirExprKind {
    // ── Literals ──────────────────────────────────────────────────────────────
    IntLit(i64),
    FloatLit(f64),
    StrLit(String),
    BoolLit(bool),
    Nothing,

    // ── Variables / names ─────────────────────────────────────────────────────
    Var(Symbol),
    This,
    Parent,

    // ── Operators ─────────────────────────────────────────────────────────────
    Binary {
        op: BinOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
    },
    /// Assignment-as-expression: `target = value`.  Lowered separately from `BinOp::Eq` (==).
    Assign {
        target: Box<HirExpr>,
        value: Box<HirExpr>,
    },
    Unary {
        op: UnOp,
        operand: Box<HirExpr>,
    },
    NullCoalesce {
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
    },

    // ── Desugared ternary: `then_val if cond else else_val` ──────────────────
    IfExpr {
        condition: Box<HirExpr>,
        then_val: Box<HirExpr>,
        else_val: Box<HirExpr>,
    },

    // ── Calls ─────────────────────────────────────────────────────────────────
    Call {
        callee: Box<HirExpr>,
        args: Vec<HirArg>,
    },

    // ── Member access / indexing ──────────────────────────────────────────────
    Field {
        object: Box<HirExpr>,
        field: Symbol,
    },
    Index {
        object: Box<HirExpr>,
        index: Box<HirExpr>,
    },

    // ── Collections ───────────────────────────────────────────────────────────
    List(Vec<HirExpr>),
    Dict(Vec<(HirExpr, HirExpr)>),
    Tuple(Vec<HirExpr>),

    // ── String interpolation ──────────────────────────────────────────────────
    StringInterp(Vec<HirInterpPart>),

    // ── Concurrency ───────────────────────────────────────────────────────────
    Spawn(Box<HirExpr>),
    Await(Box<HirExpr>),

    // ── Check expression ──────────────────────────────────────────────────────
    CheckExpr {
        scrutinee: Box<HirExpr>,
        arms: Vec<HirCheckExprArm>,
    },

    // ── Error recovery ────────────────────────────────────────────────────────
    Error,
}

#[derive(Debug)]
pub struct HirArg {
    pub name: Option<Symbol>,
    pub value: HirExpr,
    pub span: Span,
}

#[derive(Debug)]
pub enum HirInterpPart {
    Literal(String),
    Expr(HirExpr),
}

#[derive(Debug)]
pub struct HirCheckExprArm {
    pub pattern: HirExpr,
    pub body: Vec<HirStmt>,
    pub span: Span,
}
