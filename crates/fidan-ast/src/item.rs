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
        /// Parent type path. A single element `[Foo]` means a local `extends Foo`.
        /// Multiple elements `[module, Foo]` mean a qualified `extends module.Foo`.
        parent: Option<Vec<Symbol>>,
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
        /// `true` when the import was written with curly braces, e.g. `use mod.{name}`.
        /// This indicates a *flat* import where `name` is accessible directly.
        /// `false` means a *namespace* import, e.g. `use mod` or `use mod.submod`,
        /// where the last path segment becomes a namespace variable.
        grouped: bool,
        span: Span,
    },
    /// A top-level statement (for, while, if, check, attempt, etc.)
    Stmt(StmtId),
    /// `test "name" { body }` — a named test block.
    ///
    /// Only executed when the program is run with `fidan test`.
    /// The body may call `assert(cond)` and `assert_eq(a, b)`.
    TestDecl {
        /// Human-readable test name (from the string literal).
        name: String,
        /// Body statements (type-checked like a regular action body).
        body: Vec<StmtId>,
        span: Span,
    },
    /// `enum Name { Variant1, Variant2(Type, ...) }` — enumeration type with optional payloads.
    EnumDecl {
        name: Symbol,
        variants: Vec<EnumVariantDef>,
        span: Span,
    },
}

/// One variant inside an `enum` declaration.
#[derive(Debug, Clone)]
pub struct EnumVariantDef {
    pub name: Symbol,
    /// Empty for unit variants; holds payload field types for data-carrying variants.
    pub payload_types: Vec<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: Symbol,
    pub ty: TypeExpr,
    pub has_type_annotation: bool,
    pub is_const: bool,
    pub certain: bool,
    pub default: Option<crate::ExprId>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Symbol,
    pub ty: TypeExpr,
    pub certain: bool,
    /// `true` when the `optional` keyword was written — the param may be omitted at the call site.
    pub optional: bool,
    pub default: Option<crate::ExprId>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Decorator {
    pub name: Symbol,
    pub args: Vec<crate::Arg>,
    pub span: Span,
}
