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

/// An `enum Name { Variant, ... }` declaration in the HIR.
#[derive(Debug)]
pub struct HirEnum {
    pub name: Symbol,
    /// Each entry is `(variant_name, payload_arity)`. Arity 0 = unit variant.
    pub variants: Vec<(Symbol, usize)>,
    pub span: Span,
}

/// The HIR representation of a source module.
#[derive(Debug)]
pub struct HirModule {
    /// Object / class definitions.
    pub objects: Vec<HirObject>,
    /// Enum type declarations.
    pub enums: Vec<HirEnum>,
    /// All action declarations (top-level functions + extension actions).
    pub functions: Vec<HirFunction>,
    /// Top-level variable declarations (module-scoped globals).
    pub globals: Vec<HirGlobal>,
    /// Top-level executable statements (printed / run in order).
    pub init_stmts: Vec<HirStmt>,
    /// Import declarations (`use std.io`, `use std.math.{sin}`, …).
    pub use_decls: Vec<HirUseDecl>,
    /// Named test blocks (`test "name" { … }`), only executed by `fidan test`.
    pub tests: Vec<HirTestDecl>,
}

// ── Test declarations ──────────────────────────────────────────────────────────

/// A single `test "name" { … }` block in the HIR.
#[derive(Debug)]
pub struct HirTestDecl {
    /// Human-readable test name.
    pub name: String,
    /// Body statements, type-checked like a regular action body.
    pub body: Vec<HirStmt>,
    pub span: Span,
}

// ── Use declarations ───────────────────────────────────────────────────────────

/// A single `use` import in the HIR.
///
/// Examples:
/// - `use std.io`                  → `module_path=["std","io"]`, `alias=None`, `specific_names=None`
/// - `use std.io as myio`          → `alias=Some("myio")`
/// - `use std.io.{readFile,print}` → `specific_names=Some(["readFile","print"])`
/// - `export use std.io`           → same as above but `re_export=true`
#[derive(Debug, Clone)]
pub struct HirUseDecl {
    /// Full module path segments, e.g. `["std", "io"]`.
    pub module_path: Vec<String>,
    /// Optional alias for the module namespace.
    pub alias: Option<String>,
    /// If `Some`, only these specific names are imported (destructured import).
    pub specific_names: Option<Vec<String>>,
    /// `true` when declared as `export use` — the import is re-published to any
    /// file that imports this module, so its stdlib namespace becomes visible there.
    pub re_export: bool,
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
    pub is_const: bool,
    pub certain: bool,
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
    /// `true` when the `@precompile` decorator was applied — the JIT should
    /// compile this function eagerly before the first call.
    pub precompile: bool,
    /// Built-in foreign-function metadata for `@extern`.
    pub extern_decl: Option<HirExternDecl>,
    /// User-defined (custom) decorators applied to this function.
    pub custom_decorators: Vec<CustomDecorator>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirExternAbi {
    Native,
    Fidan,
}

#[derive(Debug, Clone)]
pub struct HirExternDecl {
    pub lib: String,
    pub symbol: String,
    pub link: Option<String>,
    pub abi: HirExternAbi,
}

/// A compile-time literal argument to a user-defined decorator.
#[derive(Debug, Clone)]
pub enum DecoratorArg {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

/// A user-defined decorator application on a function.
///
/// At program startup the runtime calls `decorator_action(fn_name, arg1, arg2, ...)`
/// for every `CustomDecorator` entry on a function, in declaration order.
#[derive(Debug, Clone)]
pub struct CustomDecorator {
    /// The name of the decorator action (resolved at HIR-lowering time).
    pub name: Symbol,
    /// Compile-time literal arguments beyond the implicit `fn_name: string` first arg.
    pub args: Vec<DecoratorArg>,
}

#[derive(Debug)]
pub struct HirParam {
    pub name: Symbol,
    pub ty: FidanType,
    pub certain: bool,
    /// `true` when the `optional` keyword was written — the param may be omitted at the call site.
    pub optional: bool,
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
    /// Slice: `target[start..end]`, `target[start..]`, `target[..end]`, `target[..]`,
    /// any of the above optionally followed by `step N`.
    /// `inclusive` means `...` (inclusive upper bound).
    Slice {
        target: Box<HirExpr>,
        start: Option<Box<HirExpr>>,
        end: Option<Box<HirExpr>>,
        inclusive: bool,
        step: Option<Box<HirExpr>>,
    },

    // ── Collections ───────────────────────────────────────────────────────────
    List(Vec<HirExpr>),
    Dict(Vec<(HirExpr, HirExpr)>),
    Tuple(Vec<HirExpr>),
    // ── Comprehensions ────────────────────────────────────────────
    /// `[element for binding in iterable]` / `[... if filter]`
    ListComp {
        element: Box<HirExpr>,
        binding: Symbol,
        iterable: Box<HirExpr>,
        filter: Option<Box<HirExpr>>,
    },
    /// `{key: value for binding in iterable}` / `{... if filter}`
    DictComp {
        key: Box<HirExpr>,
        value: Box<HirExpr>,
        binding: Symbol,
        iterable: Box<HirExpr>,
        filter: Option<Box<HirExpr>>,
    },
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

    // ── Inline lambda ─────────────────────────────────────────────────────────
    /// `action with (params) { body }` — anonymous action used as a value.
    Lambda {
        params: Vec<HirParam>,
        body: Vec<HirStmt>,
        return_ty: FidanType,
        precompile: bool,
        extern_decl: Option<HirExternDecl>,
    },

    // ── Enum destructure pattern (used exclusively in `check` arm patterns) ──
    /// `Enum.Variant(binding1, binding2)` — introduces new local bindings from payload.
    /// Emitted by the HIR lowerer when it detects an enum constructor call in a
    /// `check` arm pattern position.  Not a valid general expression.
    EnumDestructure {
        /// The enum type name (e.g. `Result`).
        enum_sym: Symbol,
        /// The variant being matched (e.g. `Ok`).
        tag: Symbol,
        /// Binding names introduced into the arm body scope.
        bindings: Vec<Symbol>,
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
