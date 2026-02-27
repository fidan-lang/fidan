# Fidan – Complete Implementation Architecture

> Language: Rust  
> Status: Planning  
> Date: 2026-02-27

---

## Table of Contents

1. [The Big Picture](#1-the-big-picture)
2. [Cargo Workspace Layout](#2-cargo-workspace-layout)
3. [Stage 0 – Source Management](#3-stage-0--source-management-fidan-source)
4. [Stage 1 – Lexer / Tokenizer](#4-stage-1--lexer--tokenizer-fidan-lexer)
5. [Stage 2 – AST Definition](#5-stage-2--ast-definition-fidan-ast)
6. [Stage 3 – Parser](#6-stage-3--parser-fidan-parser)
7. [Stage 4 – Semantic Analysis & Type System](#7-stage-4--semantic-analysis--type-system-fidan-typeck)
8. [Stage 5 – HIR (High-Level IR)](#8-stage-5--hir-fidan-hir)
9. [Stage 6 – MIR (Mid-Level IR / SSA)](#9-stage-6--mir-fidan-mir)
10. [Stage 7 – Optimization Passes](#10-stage-7--optimization-passes-fidan-passes)
11. [Stage 8 – Diagnostic System](#11-stage-8--diagnostic-system-fidan-diagnostics)
12. [Stage 9 – Runtime & Value Model](#12-stage-9--runtime--value-model-fidan-runtime)
13. [Stage 10 – Interpreter Backend](#13-stage-10--interpreter-backend-fidan-interp)
14. [Stage 11 – Codegen Backend (Cranelift)](#14-stage-11--codegen-backend-fidan-codegen-cranelift)
15. [Stage 12 – Standard Library](#15-stage-12--standard-library-fidan-stdlib)
16. [Stage 13 – Driver & Compilation Pipeline](#16-stage-13--driver--compilation-pipeline-fidan-driver)
17. [Stage 14 – CLI](#17-stage-14--cli-fidan-cli)
18. [Stage 15 – Language Server (LSP)](#18-stage-15--language-server-fidan-lsp)
19. [Key Technical Decisions & Rationale](#19-key-technical-decisions--rationale)
20. [Implementation Phases (Milestones)](#20-implementation-phases-milestones)
21. [Pitfalls & Pre-planned Mitigations](#21-pitfalls--pre-planned-mitigations)

---

## 1. The Big Picture

Fidan's compilation pipeline is a classic multi-stage lowering pipeline. The same source travels
through every stage regardless of the chosen execution mode. The mode only affects which
**backend** is invoked at the end:

```
Source Text
    │
    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           Front-end (always runs)                               │
│                                                                                 │
│  SourceFile  →  Lexer  →  Token Stream  →  Parser  →  AST                      │
│                                                   ↓                             │
│                              Semantic Analysis (fidan-typeck)                   │
│                           Symbol Resolution │ Type Inference │ Null Safety      │
│                                                   ↓                             │
│                               HIR  (high-level, typed, desugared)               │
│                                                   ↓                             │
│                               MIR  (SSA / 3-address, control-flow graph)        │
│                                                   ↓                             │
│                           Optimization Passes (fidan-passes)                    │
└─────────────────────────┬──────────────────────┬───────────────────────────────┘
                          │                      │
            Interpreter   │     Precompile JIT   │     Full AOT
                          │                      │
              ┌───────────┴──┐  ┌────────────────┴──┐  ┌─────────────────────┐
              │  fidan-interp│  │cranelift (JIT ABI)│  │cranelift / LLVM     │
              │  MIR walker  │  │hot functions only │  │full native binary   │
              └──────────────┘  └───────────────────┘  └─────────────────────┘
                          │                      │                 │
                          └──────────────────────┴─────────────────┘
                                                 │
                                        fidan-runtime (always present)
                                 GC │ Object model │ Stdlib │ Concurrency
```

The **same MIR** feeds all three backends. No code duplication in the compiler, and no
behavioral divergence between modes.

---

## 2. Cargo Workspace Layout

```
fidan/
├── Cargo.toml                   ← workspace root
├── ARCHITECTURE.md
├── LICENSE.md
├── scripts/
├── TEST/
└── crates/
    ├── fidan-source/            ← SourceFile, Span, SourceMap, FileId
    ├── fidan-lexer/             ← Tokenizer, Token, SynonymMap
    ├── fidan-ast/               ← All AST node types, arena allocator
    ├── fidan-parser/            ← Recursive-descent parser + Pratt expressions
    ├── fidan-typeck/            ← Symbol tables, type inference, type checking
    ├── fidan-hir/               ← HIR types + AST→HIR lowering
    ├── fidan-mir/               ← MIR types (SSA/CFG) + HIR→MIR lowering
    ├── fidan-passes/            ← Optimization passes operating on MIR
    ├── fidan-diagnostics/       ← Diagnostic types, rendering, fix engine
    ├── fidan-runtime/           ← Value types, GC, object model, task scheduler
    ├── fidan-interp/            ← MIR interpreter
    ├── fidan-codegen-cranelift/ ← Cranelift backend (Precompile JIT + AOT)
    ├── fidan-stdlib/            ← Standard library (Rust implementations)
    ├── fidan-driver/            ← Pipeline orchestration, Session, CompileOptions
    ├── fidan-lsp/               ← Language Server Protocol server
    └── fidan-cli/               ← Main binary: `fidan` command
```

**Dependency rules (strict, enforced by Cargo):**

```
fidan-source        (no fidan deps)
fidan-lexer         → fidan-source
fidan-ast           → fidan-source
fidan-parser        → fidan-lexer, fidan-ast
fidan-diagnostics   → fidan-source
fidan-typeck        → fidan-ast, fidan-diagnostics
fidan-hir           → fidan-ast, fidan-typeck
fidan-mir           → fidan-hir
fidan-passes        → fidan-mir
fidan-runtime       → fidan-source (for string interning)
fidan-interp        → fidan-mir, fidan-runtime, fidan-stdlib
fidan-codegen-cranelift → fidan-mir, fidan-runtime
fidan-stdlib        → fidan-runtime
fidan-driver        → all of the above, fidan-diagnostics
fidan-lsp           → fidan-driver
fidan-cli           → fidan-driver
```

The strict layering prevents circular dependencies and makes unit-testing each stage trivial.

---

## 3. Stage 0 – Source Management (`fidan-source`)

> Purpose: Give every byte of source text a stable, addressable identity.

### SourceMap

```rust
pub struct SourceMap {
    files: Vec<Arc<SourceFile>>,
}

pub struct SourceFile {
    pub id:      FileId,         // u32 newtype
    pub path:    PathBuf,
    pub src:     Arc<String>,    // the full text
    pub lines:   Vec<u32>,       // byte offset of each line start (for O(log n) line lookup)
}

/// A half-open byte range within a single SourceFile.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub lo:   u32,
    pub hi:   u32,
}

impl Span {
    pub fn to_location(&self, sm: &SourceMap) -> Location { ... } // (line, col)
    pub fn merge(self, other: Span) -> Span { ... }
    pub fn dummy() -> Span { ... }
}
```

**Notes:**
- All spans are byte-based (not char-based) for efficiency.
- Lines vec enables O(log n) `byte → (line, col)` lookup via binary search.
- `Span::dummy()` is used for compiler-synthesized nodes with no source location.

---

## 4. Stage 1 – Lexer / Tokenizer (`fidan-lexer`)

> Purpose: Transform raw source text into a flat stream of `Token`s with spans,  
> normalizing synonyms to canonical token types along the way.

### Token Taxonomy

```rust
pub enum TokenKind {
    // ── Literals ─────────────────────────────────────────────
    LitInt(i64),
    LitFloat(f64),
    LitString(Arc<String>),     // raw content, interpolation parsed later
    LitBool(bool),
    Nothing,                    // the `nothing` literal / type

    // ── Keywords ─────────────────────────────────────────────
    Var, Action, Object, Extends, Returns,
    With, This, Parent,
    Required, Optional, Default,
    Oftype,

    // ── Control Flow ─────────────────────────────────────────
    If, Otherwise, When, Else, // `otherwise when` is TWO tokens → parsed as ElseIf
    Attempt, Try, Catch, Finally,
    Return, Panic, Throw,

    // ── Canonical Operators (Post-synonym normalization) ──────
    Assign,           // `set` | `=`
    Eq,               // `==` | `is` | `equals`
    NotEq,            // `!=` | `notequals` | `isnot`
    Gt,               // `greaterthan` | `>`
    Lt,               // `lessthan`    | `<`
    GtEq,             // `greaterthanorequal` | `>=`
    LtEq,             // `lessthanorequal`    | `<=`
    And,              // `and` | `&&`
    Or,               // `or`  | `||`
    Not,              // `not` | `!`
    NullCoalesce,     // `??`
    Plus, Minus, Star, Slash, Percent, Caret,

    // ── Delimiters ────────────────────────────────────────────
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    Comma,            // `,` | `also` (both normalized to Comma in parameter lists)
    Dot, Colon, DoubleColon, Arrow, FatArrow, Semicolon,
    Hash,             // start of single-line comment (consumed, not emitted)
    At,               // decorator prefix

    // ── Identifiers ───────────────────────────────────────────
    Ident(Symbol),    // interned string

    // ── Special ───────────────────────────────────────────────
    Eof,
    Unknown(char),    // error recovery token
}

pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
```

### Synonym Map

A `SynonymMap` is a static compile-time table (using `phf` crate for perfect hashing):

| Written form | Canonical `TokenKind` |
|---|---|
| `set` (in assignment context) | `Assign` |
| `is`, `equals`, `==` | `Eq` |
| `!=`, `notequals`, `isnot` | `NotEq` |
| `greaterthan`, `>` | `Gt` |
| `lessthan`, `<` | `Lt` |
| `and`, `&&` | `And` |
| `or`, `\|\|` | `Or` |
| `not`, `!` | `Not` |
| `also` | `Comma` |
| `try` | `Attempt` |
| `throw` | `Panic` |
| `else if` | `Otherwise` `When` (two tokens, parser handles) |

**Important:** Synonyms are resolved at lex time so the parser only ever sees canonical tokens.
The original source span is preserved so error messages reference the exact written form.

### Context-sensitive tokens

`set` is ambiguous: `var x set 10` (assignment) vs. a hypothetical type named `set`.
The lexer always emits it as `Assign`. The parser is responsible for determining meaning from
context. This keeps the lexer context-free.

Similarly `is` in `person is not nothing` tokenizes `is` → `Eq`, then `not` → `Not`.
The expression `a is not b` thus tokenizes to `a Eq Not b` and the parser rewrites this
compound to `a NotEq b` during a normalization pass (see Parser section).

### String Interpolation

String interpolation `"Hello {name}, you are {age} years old"` is handled in a two-step process:

1. Lexer: emits a `LitString` with the raw content (including `{...}` placeholders).
2. Parser: `"parse_string_interpolation"` splits the raw string at `{` / `}` boundaries,
   recursively parsing each embedded expression as a full expression. Produces an
   `Expr::StringInterp` AST node containing alternating `Expr::Lit(string)` and `Expr::X`
   fragments.

This keeps the lexer simple and places interpolation parsing where it belongs: in the parser,
which already knows about expressions.

### Comment Handling

```
# single-line: consume until \n
#/ multi-line: depth counter; #/ increments, /# decrements; stops when depth == 0
```

The lexer tracks `nest_depth: u32`. Comments are consumed entirely and not emitted as tokens.
Their spans are recorded in a `CommentStore` attached to `SourceFile` so the formatter can
reproduce them.

### Symbol Interning

All identifiers go through a global `SymbolInterner` (a `DashMap<Arc<String>, SymbolId>`).
This means **equality of identifiers is O(1)** everywhere downstream (just compare `SymbolId`,
a `u32`). The interner is stored in the `Session`.

---

## 5. Stage 2 – AST Definition (`fidan-ast`)

> Purpose: A faithful, lossless structural representation of parsed Fidan source.

### Design Principles

- **No semantic content** – the AST contains exactly what was written. Types may be absent
  where not annotated. Names are unresolved strings/symbols.
- **Arena-allocated** – all nodes live in a `typed_arena::Arena<T>` owned by the `Module`.
  This gives:
  - `O(1)` allocation with no individual heap fragmentation
  - Trivial deallocation (drop the arena, all nodes go away)
  - `Copy`-able node references (just `&'ast NodeType`)
- **Fully spanned** – every node carries a `Span`.

### Arena Strategy

```rust
pub struct AstArena<'ast> {
    exprs:  typed_arena::Arena<Expr<'ast>>,
    stmts:  typed_arena::Arena<Stmt<'ast>>,
    items:  typed_arena::Arena<Item<'ast>>,
    params: typed_arena::Arena<Param<'ast>>,
    // etc.
}

// References into the arena are just thin pointers with a lifetime
pub type ExprRef<'ast> = &'ast Expr<'ast>;
```

### Core Node Types

```rust
// ── Top-level items ────────────────────────────────────────────────────────────

pub struct Module<'ast> {
    pub items: Vec<ItemRef<'ast>>,
    pub span:  Span,
}

pub enum Item<'ast> {
    VarDecl(VarDecl<'ast>),
    ActionDecl(ActionDecl<'ast>),
    ObjectDecl(ObjectDecl<'ast>),
    ExprStmt(ExprRef<'ast>),   // top-level expressions like `print("Hello")`
}

// ── Variable declaration ───────────────────────────────────────────────────────

pub struct VarDecl<'ast> {
    pub name:        Symbol,
    pub ty:          Option<TypeRef<'ast>>,     // `oftype T`
    pub init:        Option<ExprRef<'ast>>,     // `set expr`  or  `= expr`
    pub decorators:  Vec<Decorator>,
    pub span:        Span,
}

// ── Action (function / method) ─────────────────────────────────────────────────

pub struct ActionDecl<'ast> {
    pub name:        Symbol,
    pub extends:     Option<Symbol>,            // `extends TypeName`
    pub params:      Vec<Param<'ast>>,
    pub return_ty:   Option<TypeRef<'ast>>,     // `returns T`
    pub body:        Block<'ast>,
    pub decorators:  Vec<Decorator>,
    pub span:        Span,
}

pub struct Param<'ast> {
    pub name:       Symbol,
    pub ty:         Option<TypeRef<'ast>>,
    pub default:    Option<ExprRef<'ast>>,      // `= expr` or `default expr`
    pub required:   bool,                        // `required` keyword
    pub optional:   bool,                        // `optional` keyword
    pub span:       Span,
}

// ── Object declaration ─────────────────────────────────────────────────────────

pub struct ObjectDecl<'ast> {
    pub name:       Symbol,
    pub extends:    Option<Symbol>,
    pub body:       Vec<ItemRef<'ast>>,         // fields and action members
    pub decorators: Vec<Decorator>,
    pub span:       Span,
}

// ── Statements ─────────────────────────────────────────────────────────────────

pub enum Stmt<'ast> {
    VarDecl(VarDecl<'ast>),
    Assign   { target: ExprRef<'ast>, value: ExprRef<'ast>, span: Span },
    If(IfStmt<'ast>),
    Attempt(AttemptStmt<'ast>),
    Return   { value: Option<ExprRef<'ast>>, span: Span },
    Panic    { value: ExprRef<'ast>, span: Span },       // `panic` / `throw`
    ExprStmt(ExprRef<'ast>),
    Block(Block<'ast>),
}

pub struct IfStmt<'ast> {
    pub condition:  ExprRef<'ast>,
    pub then_block: Block<'ast>,
    pub else_ifs:   Vec<(ExprRef<'ast>, Block<'ast>)>,   // `otherwise when` / `else if`
    pub else_block: Option<Block<'ast>>,
    pub span:       Span,
}

pub struct AttemptStmt<'ast> {
    pub body:       Block<'ast>,
    pub catch:      Option<CatchClause<'ast>>,
    pub otherwise:  Option<Block<'ast>>,   // runs only if NO error was thrown
    pub finally:    Option<Block<'ast>>,
    pub span:       Span,
}

pub struct CatchClause<'ast> {
    pub binding: Symbol,                   // `catch error { ... }`
    pub body:    Block<'ast>,
    pub span:    Span,
}

pub struct Block<'ast> {
    pub stmts: Vec<StmtRef<'ast>>,
    pub span:  Span,
}

// ── Expressions ────────────────────────────────────────────────────────────────

pub enum Expr<'ast> {
    Lit      (Lit, Span),
    Ident    (Symbol, Span),
    This     (Span),
    Parent   (Span),
    Nothing  (Span),
    Binary   { op: BinOp, lhs: ExprRef<'ast>, rhs: ExprRef<'ast>, span: Span },
    Unary    { op: UnOp,  operand: ExprRef<'ast>, span: Span },
    Call(CallExpr<'ast>),
    Member   { object: ExprRef<'ast>, field: Symbol, span: Span },
    Index    { object: ExprRef<'ast>, index: ExprRef<'ast>, span: Span },
    NullCoalesce { lhs: ExprRef<'ast>, rhs: ExprRef<'ast>, span: Span },
    Ternary  { condition: ExprRef<'ast>, then: ExprRef<'ast>, otherwise: ExprRef<'ast>, span: Span },
    StringInterp { parts: Vec<StringPart<'ast>>, span: Span },
    List     { elements: Vec<ExprRef<'ast>>, span: Span },
    Dict     { entries: Vec<(ExprRef<'ast>, ExprRef<'ast>)>, span: Span },
}

pub struct CallExpr<'ast> {
    pub callee:       ExprRef<'ast>,
    pub positional:   Vec<ExprRef<'ast>>,
    pub named:        Vec<(Symbol, ExprRef<'ast>)>,    // `name set value`
    pub span:         Span,
}

pub enum StringPart<'ast> {
    Literal(Arc<String>),
    Interpolated(ExprRef<'ast>),
}

// ── Types ──────────────────────────────────────────────────────────────────────

pub enum Type<'ast> {
    Named    (Symbol, Span),           // `integer`, `string`, `Person`, ...
    Generic  (Symbol, Vec<TypeRef<'ast>>, Span),   // future: `list oftype integer`
    Nothing  (Span),
    Dynamic  (Span),
}

// ── Decorators ─────────────────────────────────────────────────────────────────

pub struct Decorator {
    pub name: Symbol,
    pub args: Vec<Lit>,    // decorators can have simple literal arguments for MVP
    pub span: Span,
}
```

---

## 6. Stage 3 – Parser (`fidan-parser`)

> Purpose: Transform a flat token stream into an `ast::Module`.

### Approach: Hand-written Recursive Descent + Pratt Expressions

A hand-written parser gives:
- Best possible error recovery
- Easy to extend without fighting a grammar DSL
- Natural place to implement synonym normalization edge-cases

**Pratt parsing** for expressions handles operator precedence cleanly. Each operator has a
`left_binding_power` and `right_binding_power`.

### Precedence Table (ascending)

| Precedence | Operators |
|---|---|
| 1 | `??` (null-coalesce) |
| 2 | `or` |
| 3 | `and` |
| 4 | `not` (prefix) |
| 5 | `== != < > <= >=` |
| 6 | `+ -` |
| 7 | `* / %` |
| 8 | `^` (power) |
| 9 | Unary `-` |
| 10 | `.` (member access), `()` (call), `[]` (index) |

**Ternary** `value if condition else fallback` is handled as an infix operator on `condition`
with associativity rules:
- `if` triggers ternary parsing: parse the left side, see `if`, then parse `condition`, expect
  `else`, parse `fallback`.
- Precedence: lower than everything except `??`.

### Error Recovery

The parser maintains a **synchronization set**: after an error, it discards tokens until it
reaches a `}`, `;`, a statement-starting keyword (`var`, `action`, `object`, `if`, `attempt`,
`return`, etc.) or `Eof`. This lets parsing continue and collect multiple errors in one pass.

Every error produces a `Diagnostic` and the parser inserts an `Expr::Error(span)` or
`Stmt::Error(span)` placeholder node so downstream stages can keep running.

### Parsing `otherwise when` / `else if`

```
if_stmt := 'if' expr block
           ('otherwise' 'when' expr block)*
           ('otherwise' | 'else') block)?
```

Both `otherwise when` and `else if` produce the same `IfStmt::else_ifs` list entry.

### Parsing Named Arguments

```
call_expr := expr '(' arg_list ')'
arg_list  := (positional_arg (',' | 'also') arg_list)?
           | (named_arg (',' | 'also') arg_list)?

positional_arg := expr                    (not followed by 'set' / '=')
named_arg      := ident ('set' | '=') expr
```

**Positional-before-named rule** is enforced in the semantic analysis phase, not the parser.
The parser simply collects all arguments, tagging them as positional or named.

### Parsing Extension Actions

```
action_decl := decorator* 'action' ident ('extends' ident)? ('with' '(' params ')')? ('returns' type)? block
```

When `extends TypeName` appears, the parser records it in `ActionDecl::extends`. Semantic
analysis determines whether it's a valid type and how to bind it.

### Parsing `is not`

The token stream `a is not b` reads: `Ident(a) Eq Not Ident(b)`.  
A post-parse token normalization step in the parser recognizes the pattern `Eq Not` adjacent
in the token stream (holding a combined span) and rewrites it to `NotEq`. This happens before
the Pratt loop processes the operators.

---

## 7. Stage 4 – Semantic Analysis & Type System (`fidan-typeck`)

> Purpose: Resolve names, infer types, check types, enforce language rules. Produces a
> fully-typed, resolved AST (or flags all errors).

### Component Breakdown

```
fidan-typeck
├── symbol_table.rs     ← Scoped symbol resolution
├── type_engine.rs      ← Type inference (constraint-based, bidirectional)
├── type_checker.rs     ← Type compatibility, coercions, error generation
├── null_safety.rs      ← Flow-sensitive nothing-analysis
├── extension_action.rs ← Extension action resolution
├── argument_check.rs   ← Positional-before-named, required params
├── decorator_check.rs  ← Validate and record decorators
└── control_flow.rs     ← Definite assignment, unreachable code
```

### Symbol Table

```rust
pub struct SymbolTable<'tcx> {
    scopes: Vec<Scope<'tcx>>,   // stack; current scope is last
}

pub struct Scope<'tcx> {
    symbols: HashMap<Symbol, SymbolInfo<'tcx>>,
    kind:    ScopeKind,         // Module | Function | Block | Object | Param
}

pub struct SymbolInfo<'tcx> {
    pub kind:       SymbolKind,     // Var, Action, Object, Param, Field
    pub ty:         TypeId,         // resolved type
    pub span:       Span,           // declaration site
    pub is_mutable: bool,
    pub initialized: Tristate,      // Yes | No | Maybe (for control-flow analysis)
}
```

### Type Representation

```rust
pub enum Ty {
    // Primitives
    Integer, Float, String, Boolean, Nothing, Dynamic,
    // Composite
    List   (Box<Ty>),
    Dict   (Box<Ty>, Box<Ty>),              // key type, value type
    // User types
    Object (ObjectTypeId),                  // resolved object declaration
    // Action types (for first-class functions, future)
    Action { params: Vec<Ty>, ret: Box<Ty> },
    // The "nullable wrapper" is implicit: ALL types can hold Nothing.
    // Nothing is NOT a separate wrapper type; every Ty can be assigned Nothing.
    // Type errors arise only when operating on a potentially-nothing value without a check.
    
    // Inference variable (used during inference, should be eliminated after)
    InferVar(InferVarId),
    
    // Unresolved (placeholder after parse errors)
    Error,
}
```

### Type Inference Strategy

**Bidirectional type inference** (similar to Hindley-Milner but simpler, appropriate for
Fidan's mostly-annotated style):

1. **Check mode**: When a type annotation is present, propagate it inward ("this expression
   should have type T").
2. **Infer mode**: When no annotation, synthesize a type from the expression and unify upward.
3. **Unification**: When two types must match, produce a `TypeConstraint`. Constraints are
   solved at the end of each function scope.

Key rules:
- `var x set 10` → `x` has type `Integer` (inferred from literal).
- `var x oftype integer` → `x` has type `Integer`, initialized to `Nothing`.
- Assignments: RHS type must be compatible with LHS type. `Nothing` is compatible with any type.
- Binary operators: define type signatures, e.g., `+` requires both sides `Integer` or `Float`,
  result is the same. String `+` is also allowed (concatenation).
- Return type annotation: body's last expression / all `return` expressions must match.
- If no `returns` annotation: inferred from body (for completeness; encouraged to annotate).

### Null Safety

When a value of type T is accessed (field, method, operator), the type checker performs
**flow-sensitive analysis**:

- Before a `??` or `if value != nothing` guard, the value is "possibly-nothing" → warn on
  direct dereference.
- After a guard (inside the then-branch) the value is narrowed to "definitely not nothing".
- This is tracked in `null_safety.rs` using a `NullState` map per basic block.

The analysis produces **warnings**, not errors, by default (configurable via CLI flags).

### Extension Action Resolution

```
action greet extends Person with (optional person oftype Person) { ... }
```

Two registrations made:
1. **Method registration** on `PersonType`: method `greet(optional person: Person) -> nothing`.
   When called as `john.greet()`: `this` is bound to `john`, `person` defaults to `Nothing`.
2. **Free function registration** in the enclosing scope: `greet(person: Person) -> nothing`.
   When called as `greet(jennifer)`: `this` is implicitly bound to the value of `person`
   (i.e., `this === person` when called as a free function).

The `this` binding rule for free-function call: the first (and only) extension parameter
shadows `this` inside the body. This means `person ?? this` → in the free function case,
`person` is `jennifer` and `this` is also `jennifer`, so the expression evaluates to `jennifer`.
In the method case, `this` is the receiver and `person` is `nothing`, so `person ?? this`
evaluates to `this` (the receiver). This is clean and consistent.

### Argument Checking

- Positional-before-named: detect any named arg that precedes a positional arg → error.
- `required` params: error if not supplied at call site.
- `optional` params: default to `Nothing` if not supplied.
- Default values: evaluated at call site (not at definition time, unlike Python's default trap).

---

## 8. Stage 5 – HIR (`fidan-hir`)

> Purpose: A desugared, fully-typed representation. Every synonym is gone, every implicit
> form is explicit. This is the last "human-readable" IR.

### What HIR adds over AST

| AST feature | HIR equivalent |
|---|---|
| Synonym tokens | Fully canonical (only one form exists) |
| Missing type annotations | All types explicit (inferred types filled in) |
| `nothing` implicit init | Explicit `= nothing` assignment |
| `parent.x()` | Explicit vtable target resolved |
| Extension action duality | Two separate entries in HIR |
| `is not` compound | Single `NotEq` node |
| `also` as param sep | `Comma` |
| `attempt/try`, `panic/throw` | Canonical `attempt`, `throw` |
| String interpolation | `StringInterp(parts)` node, parts are typed |
| Ternary `val if cond else fb` | `IfExpr { cond, then, else }` node |
| `value ?? fallback` | `NullCoalesce { value, fallback }` node (preserved, for codegen) |

HIR is still tree-shaped and close to source. It is NOT in SSA form.

### HIR Lowering

AST → HIR lowering (`ast_to_hir.rs`) is a straightforward structural transformation that:
1. Walks every AST node.
2. Looks up resolved types from `fidan-typeck`'s output (`TypedAst`).
3. Emits HIR nodes with fully-annotated types.
4. Desugars the above table.

---

## 9. Stage 6 – MIR (`fidan-mir`)

> Purpose: A flat, explicit, SSA-form control-flow graph, suitable for optimization and
> multiple codegen backends.

### Why SSA?

Static Single Assignment form means each variable is assigned exactly once. This enables:
- Trivial data-flow analysis (no aliasing of variable names)
- Dead code elimination
- Constant propagation
- Future: more advanced optimizations (GVN, LICM, etc.)

### MIR Structure

```rust
pub struct MirFunction {
    pub name:           Symbol,
    pub params:         Vec<MirParam>,
    pub return_ty:      MirTy,
    pub basic_blocks:   Vec<BasicBlock>,
    // start block is always index 0
}

pub struct BasicBlock {
    pub id:           BlockId,
    pub phis:         Vec<PhiNode>,
    pub instructions: Vec<Instr>,
    pub terminator:   Terminator,
}

pub struct PhiNode {
    pub result: LocalId,
    pub ty:     MirTy,
    pub operands: Vec<(BlockId, Operand)>,
}

pub enum Instr {
    Assign      { dest: LocalId, ty: MirTy, rhs: Rvalue },
    Call        { dest: Option<LocalId>, callee: Callee, args: Vec<Operand>, span: Span },
    NullCheck   { scrutinee: Operand, span: Span },  // inserted by null-safety pass
    SetField    { object: Operand, field: Symbol, value: Operand },
    GetField    { dest: LocalId, object: Operand, field: Symbol },
    Drop        { local: LocalId },           // explicit lifetime end for GC
}

pub enum Terminator {
    Return(Option<Operand>),
    Goto(BlockId),
    Branch   { cond: Operand, then: BlockId, else_: BlockId },
    Throw    { value: Operand },
    Unreachable,
}

pub enum Rvalue {
    Use(Operand),
    Binary { op: BinOp, lhs: Operand, rhs: Operand },
    Unary  { op: UnOp, operand: Operand },
    NullCoalesce { lhs: Operand, rhs: Operand },
    Call   { callee: Callee, args: Vec<Operand> },
    Construct { ty: ObjectTypeId, fields: Vec<(Symbol, Operand)> },
    List   (Vec<Operand>),
    Dict   (Vec<(Operand, Operand)>),
    StringInterp(Vec<StringPart>),   // stays as-is; runtime handles formatting
    Literal(Lit),
    Nothing,                         // nothing literal
}

pub enum Operand {
    Local(LocalId),
    Const(Lit),
    Nothing,
    Global(GlobalId),
}

pub enum Callee {
    Fn      (FunctionId),
    Method  { receiver: Operand, method: Symbol },
    Dynamic (Operand),    // function passed as value (future)
}
```

### HIR → MIR Lowering

This is the most algorithmically complex lowering step. It implements:

1. **Block splitting**: Every `if`, `attempt`, loop, etc. creates new basic blocks.
2. **SSA construction**: Use Braun et al.'s "Simple and Efficient Construction of SSA Form"
   algorithm. This avoids the two-pass approach (no need to scan for all definitions first).
3. **Exception handling**: `attempt/catch` is lowered to:
   - A "landing pad" basic block that receives the thrown value.
   - The `throw` instruction unwinds to the nearest landing pad.
   - `finally` blocks are duplicated on all exit paths (or implemented via cleanup blocks).
4. **`this` binding**: In extension actions, `this` is given its own `LocalId` and wired
   appropriately by the call-site lowering.
5. **`parent.method()` calls**: Lowered to `Callee::Fn(resolved_parent_method_id)` with
   the receiver passed explicitly.

---

## 10. Stage 7 – Optimization Passes (`fidan-passes`)

> Purpose: Transform MIR to be faster/smaller without changing semantics.

Passes run on the MIR `program` (all functions). Each pass is a `MirPass` trait:

```rust
pub trait MirPass {
    fn name(&self) -> &'static str;
    fn run(&mut self, function: &mut MirFunction, ctx: &PassContext);
}
```

### MVP Pass Set

| Pass | What it does |
|---|---|
| `ConstantFolding` | Evaluate `2 + 3` → `5` at compile time |
| `DeadCodeElimination` | Remove instructions whose results are never used |
| `CopyPropagation` | Replace `x = y; use(x)` with `use(y)` |
| `InliningPass` | Inline small functions (heuristic: < N instructions) |
| `NullCoalesceSimplification` | `x ?? nothing` → `x`; `nothing ?? y` → `y` |
| `UnreachablePruning` | Remove blocks after `Unreachable` terminators |

### Later Passes (Post-MVP)

- Global Value Numbering (GVN) – deduplicates redundant computations
- Loop-Invariant Code Motion (LICM)
- Escape Analysis – determine if objects can be stack-allocated
- Trait Devirtualization (when objects are concretely known)

---

## 11. Stage 8 – Diagnostic System (`fidan-diagnostics`)

> Purpose: Produce explainable, actionable error messages. This is a first-class feature of
> Fidan, not an afterthought.

### Philosophy

Every diagnostic answers these questions:
1. **What** went wrong (the primary message).
2. **Where** it went wrong (primary span with label).
3. **Why** it went wrong (secondary spans, notes).
4. **How** to fix it (suggestion with code).
5. **Cause chain**: if this error was triggered by another error upstream, show that chain.

### Diagnostic Structure

```rust
pub struct Diagnostic {
    pub severity:     Severity,          // Error | Warning | Info | Hint
    pub code:         DiagnosticCode,    // E001..E999, W001..W999
    pub message:      String,
    pub primary:      Label,             // main span + message
    pub secondary:    Vec<Label>,
    pub notes:        Vec<String>,
    pub suggestions:  Vec<Suggestion>,
    pub cause_chain:  Vec<Box<Diagnostic>>,
}

pub struct Label {
    pub span:    Span,
    pub message: String,
    pub style:   LabelStyle,    // Primary | Secondary
}

pub struct Suggestion {
    pub message:   String,
    pub edits:     Vec<SourceEdit>,    // span → replacement text
    pub confidence: Confidence,        // Definite | Likely | Possible
}

pub struct SourceEdit {
    pub span:        Span,
    pub replacement: String,
}

pub enum DiagnosticCode {
    // Type errors: E1xx
    TypeMismatch,         // E101
    UndefinedVariable,    // E102
    UndefinedField,       // E103
    // etc.
    // Null errors: E2xx  
    PossibleNothingDeref, // E201 (warning)
    // Argument errors: E3xx
    MissingRequired,      // E301
    PositionalAfterNamed, // E302
    // etc.
}
```

### Rendering Backend

Use the [`ariadne`](https://crates.io/crates/ariadne) crate. It produces beautiful ANSI-colored
terminal output with source context and arrows:

```
Error[E101]: type mismatch: expected `integer`, found `string`
  --> src/main.fdn:14:13
   │
14 │     var age set "hello"
   │             ^^^ ^^^^^^^ this has type `string`
   │             │
   │             expected `integer` here
   │
   = note: variable `age` was declared with `oftype integer`
  help: try converting the string to an integer
   │
14 │     var age set toInteger("hello")
   │             ^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

### Rule-Based Fix Suggestions

A `FixEngine` holds a table of `FixRule`s:

```rust
pub struct FixRule {
    pub trigger: DiagnosticCode,
    pub apply:   Box<dyn Fn(&Diagnostic, &SourceMap) -> Option<Suggestion>>,
}
```

Examples:
- `UndefinedVariable("foo")` → search symbol table for names with edit-distance ≤ 2 → suggest "Did you mean `{similar_name}`?"
- `TypeMismatch { expected: String, found: Integer }` → suggest `toString(x)` wrapper.
- `MissingRequired("name")` → suggest adding `name set "..."` at call site.
- `PositionalAfterNamed` → show the corrected argument order.

Edit distance: use `strsim` crate (Jaro-Winkler or Levenshtein).

---

## 12. Stage 9 – Runtime & Value Model (`fidan-runtime`)

> Purpose: Define how Fidan values exist at runtime, how objects are allocated, and how
> concurrency works.

### Value Representation

```rust
// The universal Fidan value type in interpreted / mixed mode.
// In AOT mode, this is replaced by typed native values, but the GC still
// tracks heap objects via a uniform header.

pub enum FidanValue {
    Integer  (i64),
    Float    (f64),
    Boolean  (bool),
    Nothing,
    String   (GcRef<FidanString>),
    List     (GcRef<FidanList>),
    Dict     (GcRef<FidanDict>),
    Object   (GcRef<FidanObject>),
    Function (FunctionId),            // first-class function reference
}

impl FidanValue {
    pub fn type_name(&self) -> &'static str { ... }
    pub fn is_nothing(&self) -> bool { matches!(self, Self::Nothing) }
    pub fn truthy(&self) -> bool { ... }   // for boolean coercions
}
```

For **NaN-boxing** (a future optimization): pack the entire `FidanValue` into 8 bytes using
IEEE 754 NaN payloads. Deferred to post-MVP (complicates GC interaction).

### Heap & GC

**Phase 1 (MVP): Reference Counting with Cycle Collection**

```rust
pub struct GcRef<T>(Rc<GcCell<T>>);   // single-threaded interpreter
// or
pub struct GcRef<T>(Arc<Mutex<T>>);   // for concurrent execution
```

- All heap objects (`FidanString`, `FidanList`, `FidanDict`, `FidanObject`) are reference-counted.
- A periodic `CycleCollector` (Bacon-Rajan algorithm) handles reference cycles (common in
  object graphs).
- The `Drop` impl on `GcRef` decrements the reference count; when it hits zero, the object is
  freed; if it's a tracked cycle candidate, it's added to the cycle collector's trial buffer.

**Phase 2 (Post-MVP): Generational GC or Incremental Mark-Sweep**

A proper tracing GC would give better tail latency for long-running programs. Design the GC
interface behind a trait so it can be swapped:

```rust
pub trait GcBackend {
    fn alloc<T: GcTraceable>(&self, val: T) -> GcRef<T>;
    fn collect(&self);
    fn add_root(&self, ptr: GcRootPtr);
    fn remove_root(&self, ptr: GcRootPtr);
}
```

### Object Model

```rust
pub struct FidanClass {
    pub name:    Symbol,
    pub parent:  Option<Arc<FidanClass>>,
    pub fields:  Vec<FieldDef>,           // (name, ty, index into FidanObject.fields)
    pub methods: HashMap<Symbol, FunctionId>,
}

pub struct FidanObject {
    pub class:  Arc<FidanClass>,
    pub fields: Vec<FidanValue>,          // indexed by FieldDef.index
}

impl FidanObject {
    pub fn get_field(&self, name: Symbol) -> &FidanValue { ... }
    pub fn set_field(&mut self, name: Symbol, value: FidanValue) { ... }
    pub fn find_method(&self, name: Symbol) -> Option<FunctionId> {
        // Walk up class hierarchy
        let mut cls = &self.class;
        loop {
            if let Some(id) = cls.methods.get(&name) { return Some(*id); }
            cls = cls.parent.as_ref()?;
        }
    }
}
```

### Concurrency Model

**Structured Concurrency** means every task has a parent scope. Tasks cannot escape their
enclosing block (no fire-and-forget).

```fidan
concurrent {
    task A { ... }
    task B { ... }
}
# execution continues here only after both A and B complete (or one fails)
```

**Implementation (Phase 1):** Cooperative task scheduler using green threads.

Rust crate: [`corosensei`](https://crates.io/crates/corosensei) (safe, stackful coroutines) OR
a simple single-threaded event loop with yield points.

**Implementation (Phase 2):** Multi-threaded work-stealing scheduler (similar to Tokio's
runtime but without async/await syntax surfacing to the user). Fidan's concurrency primitives
map to a runtime-managed task graph.

**Data safety:** Objects passed between tasks are cloned (value semantics) or wrapped in an
explicit `Shared<T>` type (like Rust's `Arc<Mutex<T>>`). Direct shared mutable access without
`Shared` is a compile-time error.

---

## 13. Stage 10 – Interpreter Backend (`fidan-interp`)

> Purpose: Execute MIR directly, as fast as possible, without compilation.

### Design: MIR Walker with Value Stack

```rust
pub struct Interpreter {
    pub session:  Arc<Session>,
    pub runtime:  Arc<Runtime>,       // GC, stdlib, task scheduler
    call_stack:   Vec<CallFrame>,
}

pub struct CallFrame {
    pub function: Arc<MirFunction>,
    pub block:    BlockId,
    pub instr:    usize,
    pub locals:   Vec<FidanValue>,    // indexed by LocalId
    pub this_val: Option<FidanValue>, // `this` binding
}
```

Execution loop:

```
loop {
    let frame = call_stack.last_mut();
    let instr = frame.current_instr();
    match instr {
        Instr::Assign { dest, rhs } => {
            let val = eval_rvalue(rhs, frame);
            frame.locals[dest] = val;
        }
        Instr::Call { dest, callee, args } => {
            push new CallFrame
        }
        Terminator::Return(op) => {
            pop CallFrame, set result
        }
        Terminator::Throw(val) => {
            unwind call_stack until landing pad found
        }
        // ...
    }
}
```

### Hot Reload

For hot reload (interpreter only):
1. Watch source files with `notify` crate.
2. On modification: re-parse and re-analyze the changed module.
3. Update the `MirProgram` with new function bodies.
4. Running tasks that are between instructions safely pick up the new function body at the
   next call boundary.

### `@precompile` in Interpreter Mode

When a `@precompile`-annotated function is first called:
1. Detect it.
2. Pass its MIR to `fidan-codegen-cranelift` for JIT compilation.
3. Replace the `FunctionId` entry in the runtime's function table with a pointer to the
   compiled code.
4. Subsequent calls use the compiled version directly.

This is the **Interpreter + Precompile** mode described in the spec.

---

## 14. Stage 11 – Codegen Backend (`fidan-codegen-cranelift`)

> Purpose: Compile MIR to native machine code via Cranelift.

### Why Cranelift?

- Pure Rust (no LLVM dependency overhead, no C++ ABI)
- Fast compilation (suitable for both JIT and AOT)
- Actively maintained (used in Wasmtime, Rust's `cg_clif` backend)
- Good enough optimization for Fidan's needs at launch

LLVM can be added as an optional secondary backend later for maximum optimization on release
builds, but is NOT required for MVP.

### MIR → Cranelift IR Mapping

```
MirFunction           → cranelift::ir::Function
BasicBlock            → cranelift::ir::Block
LocalId (SSA value)   → cranelift::ir::Value
FidanValue (in interp)→ typed Cranelift values (i64, f64, pointer, etc.)
```

When compiling in **AOT mode**, types are known at compile time (from typeck), so all
`FidanValue` enums are replaced with their native Cranelift types:

| `FidanValue` | Cranelift type |
|---|---|
| `Integer(i64)` | `I64` |
| `Float(f64)` | `F64` |
| `Boolean(bool)` | `I8` |
| `String(GcRef<_>)` | `I64` (pointer) |
| `List(GcRef<_>)` | `I64` (pointer) |
| `Object(GcRef<_>)` | `I64` (pointer) |
| `Nothing` | `I64` (0 = null pointer, special-cased) |

**Dynamic dispatch** (for `dynamic` typed variables): use a tagged union struct in memory
(two `I64` words: tag + payload). Generated helper functions in the runtime handle dispatch.

### JIT Mode (Precompile)

```rust
use cranelift_jit::{JITBuilder, JITModule};

pub struct JitCompiler {
    module: JITModule,
}

impl JitCompiler {
    pub fn compile_function(&mut self, mir_fn: &MirFunction) -> *const u8 { ... }
}
```

The compiled function pointer is stored in the interpreter's function table and called via
a safe trampoline that handles the ABI boundary between the interpreter's `FidanValue`
stack and the compiled function's native calling convention.

### AOT Mode

```rust
use cranelift_object::{ObjectBuilder, ObjectModule};

pub struct AotCompiler {
    module: ObjectModule,
}

impl AotCompiler {
    pub fn compile_program(&mut self, program: &MirProgram) -> Vec<u8> { /* ELF/PE/Mach-O */ }
}
```

The output object file is linked with:
- `fidan-runtime` (compiled as a static `.a` library)
- `fidan-stdlib` (compiled as a static `.a` library)
- System libraries (libc, pthreads)

Using the system linker (`cc -o output main.o libfidan_runtime.a`) invoked via `std::process::Command`.

### Platform ABI

For the AOT calling convention, define a **Fidan ABI** that is consistent across platforms:
- First argument: always the return slot (pointer to FidanValue) if the return type is > 8 bytes
- `this` is always the first parameter for methods
- Primitives are passed in registers; heap types are passed as pointers
- Tail call optimization: supported for direct recursive calls (mark with `@tailcall` decorator, or automatic detection)

---

## 15. Stage 12 – Standard Library (`fidan-stdlib`)

> Purpose: Provide the essential batteries that users expect.

All stdlib modules are implemented in Rust for Phase 1. As the Fidan language matures, high-level
wrappers can be written in Fidan itself (bootstrapping).

The stdlib is organized into a `std` namespace:

### Module Plan

| Module | Contents | Rust crate |
|---|---|---|
| `std.io` | File, stdin/stdout/stderr, Path, Directory | `std::fs`, `std::io` |
| `std.net` | TcpSocket, HttpClient, HttpServer | `tokio` / `hyper` |
| `std.collections` | Set, Queue, Deque, BTreeMap | `std::collections` |
| `std.math` | sin, cos, sqrt, floor, ceil, abs, min, max, random | `std::f64` |
| `std.string` | split, join, trim, replace, contains, startsWith, endsWith, toUpper, toLower | Rust String methods |
| `std.concurrent` | Task, Channel, Mutex, Barrier | custom runtime |
| `std.debug` | assert, assertEq, inspect, profile | custom |
| `std.test` | describe/it test blocks, expect(...).to... matchers | custom |
| `std.cli` | Argument parsing, colored output, progress bars | `clap`, `indicatif` |
| `std.time` | DateTime, Duration, sleep | `chrono` |
| `std.json` | parse, stringify, path queries | `serde_json` |
| `std.env` | Environment variables, platform info | `std::env` |

### Builtin Functions

These are injected into every module's scope without import:

- `print(...)` – write to stdout with newline
- `input(prompt)` – read line from stdin
- `len(collection)` – length of string/list/dict
- `range(start, stop, step?)` – integer range (lazy)
- `toString(x)` – convert any value to string
- `toInteger(x)` – parse/convert to integer
- `toFloat(x)` – parse/convert to float

---

## 16. Stage 13 – Driver & Compilation Pipeline (`fidan-driver`)

> Purpose: Orchestrate the full pipeline, manage sessions, and expose a clean API to the CLI
> and LSP.

### Session

```rust
pub struct Session {
    pub interner:    SymbolInterner,
    pub source_map:  SourceMap,
    pub config:      CompileConfig,
    pub diagnostics: DiagnosticBag,    // accumulates all diagnostics
}

pub struct CompileConfig {
    pub mode:          ExecutionMode,   // Interpret | InterpretPrecompile | Aot
    pub opt_level:     OptLevel,        // None | Size | Speed
    pub target:        target_lexicon::Triple,
    pub emit:          Vec<EmitKind>,   // Mir | Hir | Asm | Object | Binary
    pub stdlib_path:   PathBuf,
    pub warn_as_error: bool,
}
```

### Pipeline Function

```rust
pub fn compile(session: &mut Session, input: &Path) -> Result<CompileOutput, ()> {
    // 1. Load source
    let file = session.source_map.load_file(input)?;

    // 2. Lex
    let tokens = fidan_lexer::tokenize(&file, session)?;

    // 3. Parse
    let ast = fidan_parser::parse(&tokens, &file, session)?;
    if session.has_errors() { return Err(()); }

    // 4. Type check
    let typed_ast = fidan_typeck::check(&ast, session)?;
    if session.has_errors() { return Err(()); }

    // 5. Lower to HIR
    let hir = fidan_hir::lower(&typed_ast, session);

    // 6. Lower to MIR
    let mut mir = fidan_mir::lower(&hir, session);

    // 7. Optimize
    fidan_passes::run_passes(&mut mir, &session.config);

    // 8. Backend dispatch
    match session.config.mode {
        ExecutionMode::Interpret => {
            let interp = fidan_interp::Interpreter::new(session, mir);
            interp.run_main()
        }
        ExecutionMode::Aot => {
            let obj = fidan_codegen_cranelift::compile_aot(&mir, &session.config);
            link_and_emit(obj, &session.config)
        }
    }
}
```

---

## 17. Stage 14 – CLI (`fidan-cli`)

> Purpose: User-facing `fidan` binary.

```
fidan run <file.fdn>               # interpret (default mode)
fidan run --precompile <file.fdn>  # interpreter + @precompile JIT
fidan build <file.fdn> [-o out]    # AOT compile to binary
fidan build --emit mir <file.fdn>  # dump MIR as text
fidan build --emit hir <file.fdn>  # dump HIR as text
fidan check <file.fdn>             # typecheck only, no execution
fidan fmt <file.fdn> [--in-place]  # format source code
fidan test [pattern]               # run test blocks
fidan repl                         # interactive REPL
fidan new <project-name>           # scaffold a new project
```

**Implementation:** Use `clap` crate with derive macros for argument parsing.

### REPL

The REPL maintains a persistent `Session` and `Interpreter`. Each line is parsed as a
statement or expression. Expressions' results are printed. The symbol table persists across
lines. Hot-patches the interpreter's environment on each entry.

---

## 18. Stage 15 – Language Server (`fidan-lsp`)

> Purpose: IDE integration (VS Code, Neovim, etc.) via the Language Server Protocol.

**Crate:** `tower-lsp` (Rust async LSP framework built on `tower` and `tokio`).

### Features (prioritized)

| Feature | Priority | Notes |
|---|---|---|
| Diagnostics (errors, warnings) | P0 | Reuse `fidan-diagnostics` |
| Go to definition | P0 | |
| Hover (type info) | P0 | |
| Completion | P1 | Identifier, field, method |
| Inline hints (types) | P1 | Show inferred types |
| Semantic highlighting | P1 | |
| Rename symbol | P2 | |
| Find all references | P2 | |
| Code actions / quick fixes | P2 | Surface fix suggestions from `fidan-diagnostics` |
| Format on save | P1 | |
| Signature help | P1 | |

The LSP server uses the same `fidan-driver` pipeline but in **incremental mode**: only
re-analyze changed files/functions. Future: use `salsa` crate for demand-driven incremental
compilation.

---

## 19. Key Technical Decisions & Rationale

### 1. Cranelift over LLVM (initial)

**Decision:** Use Cranelift for both JIT and AOT in MVP.  
**Rationale:** Cranelift is pure Rust, compiles significantly faster than LLVM, has a cleaner
API, and produces sufficiently optimized code for most use cases. LLVM can be added as an
opt-in backend for release builds later.

### 2. Arena Allocation for AST

**Decision:** All AST nodes are arena-allocated.  
**Rationale:** Avoids complex lifetime management, gives `O(1)` allocation, trivial deallocation,
and allows `Copy` node references throughout the codebase.

### 3. Reference Counting with Cycle Collection (not Tracing GC)

**Decision:** Start with Rust `Rc`/`Arc` plus periodic cycle collection.  
**Rationale:** Simpler to implement, predictable performance, no stop-the-world pauses.
Tracing GC can be introduced later behind the `GcBackend` trait.

### 4. Bidirectional Type Inference (not Full HM)

**Decision:** Use bidirectional inference, not full Hindley-Milner.  
**Rationale:** Bidirectional inference handles Fidan's mostly-annotated style well, is easier
to implement, and produces better error messages than constraint-solving unification algorithms.

### 5. Lexer-time Synonym Normalization

**Decision:** Map synonyms to canonical tokens in the lexer.  
**Rationale:** Keeps the parser simple and consistent. Error messages can still reference the
original written form because spans are preserved.

### 6. `nothing` as a Value, not a Separate Type

**Decision:** `nothing` is a value every type can hold; there is no `Maybe<T>` or `Option<T>`
wrapper in the type system.  
**Rationale:** This is the spec's intention ("all types are nullable"). Flow-sensitive null
safety analysis provides safety without forcing the user to unwrap values. This is closer to
Kotlin's approach than Rust's.

### 7. Green Threads for Concurrency (Phase 1)

**Decision:** Use stackful green threads for the concurrency runtime initially.  
**Rationale:** They are transparent to the user (no `async/await` keyword leakage), work
naturally with existing code, and map cleanly to Fidan's structured concurrency model.
Async futures can be added as a lower-level optimization later.

### 8. String Interpolation as Parser Concern, not Lexer

**Decision:** Lexer emits raw string content; parser splits and recursively parses interpolated
expressions.  
**Rationale:** Interpolated expressions can be arbitrarily complex. Parsing them in the lexer
would require the lexer to embed a mini-parser, which is messy and error-prone.

---

## 20. Implementation Phases (Milestones)

### Phase 0 – Skeleton (1–2 weeks)
**Goal:** Cargo workspace compiles. Each crate exists. Integration test harness exists.

- [ ] Set up Cargo workspace with all 14 crates (initially empty)
- [ ] `fidan-source`: `SourceFile`, `Span`, `SourceMap`, `SymbolInterner`
- [ ] Integration test: load `TEST/test.fdn` and print its contents
- [ ] CI setup (GitHub Actions: `cargo test`, `cargo clippy`, `cargo fmt --check`)

### Phase 1 – Lexer (1–2 weeks)
**Goal:** Tokenize `TEST/test.fdn` correctly.

- [ ] Implement all `TokenKind` variants
- [ ] Synonym normalization table (`phf` map)
- [ ] `#` and `#/ ... /#` (nested) comment handling
- [ ] `CommentStore` for formatter round-trip
- [ ] Span tracking
- [ ] Symbol interning integration
- [ ] Unit tests: every token type, all synonyms, nested comments, string with interpolation

### Phase 2 – AST + Parser (2–3 weeks)
**Goal:** Parse `TEST/test.fdn` to AST and pretty-print it back.

- [ ] All AST node types with arena allocation
- [ ] Recursive descent parser: items, statements
- [ ] Pratt expression parser with full precedence table
- [ ] Ternary and null-coalesce parsing
- [ ] Named argument call parsing
- [ ] Extension action declaration parsing
- [ ] `otherwise when` / `else if` parsing
- [ ] `attempt/catch/otherwise/finally` parsing
- [ ] String interpolation parsing
- [ ] Error recovery (synchronization set)
- [ ] AST pretty-printer (used by `fidan build --emit ast`)
- [ ] Round-trip test: parse `test.fdn`, pretty-print, re-parse, compare ASTs

### Phase 3 – Semantic Analysis (3–4 weeks)
**Goal:** Typecheck `test.fdn`; report all type errors on a buggy version.

- [ ] Symbol table with scope stack
- [ ] `object` type registration and field/method resolution
- [ ] Inheritance chain resolution (`extends`)
- [ ] Type inference for var declarations
- [ ] Type inference for expressions (binary ops, calls, member access)
- [ ] Type checking (assignments, return types, argument types)
- [ ] `this` and `parent` binding
- [ ] Extension action dual-registration
- [ ] Named/positional argument order enforcement
- [ ] `required` / `optional` parameter checking
- [ ] Null safety analysis (flow-sensitive, as warnings)
- [ ] Decorator validation (`@precompile`, etc.)

### Phase 4 – Diagnostics (1–2 weeks)
**Goal:** Error messages that make users say "wow".

- [ ] Full `Diagnostic` / `Label` / `Suggestion` types
- [ ] `ariadne` rendering integration
- [ ] `FixEngine` with rules for all E1xx, E2xx, E3xx codes
- [ ] Edit-distance suggestions for undefined names
- [ ] Test every error code produces a beautiful message

### Phase 5 – HIR + MIR + Tree-walking Interpreter (3–4 weeks)
**Goal:** `fidan run TEST/test.fdn` works end-to-end.

- [ ] HIR types and AST→HIR lowering
- [ ] MIR types (BasicBlock, Phi, SSA locals)  
- [ ] HIR→MIR lowering with Braun SSA construction
- [ ] Exception handling lowering (landing pads)
- [ ] MIR text dump (`--emit mir`)
- [ ] MIR interpreter with call stack, frame locals, GC
- [ ] `fidan-runtime`: `FidanValue`, `FidanObject`, `FidanClass`, ref-counted GC
- [ ] Builtin functions: `print`, `input`, `len`, `toString`, etc.
- [ ] Structured concurrency (single-threaded task scheduler first)
- [ ] Run `TEST/test.fdn` fully and verify output

### Phase 6 – Optimization Passes (1 week)
**Goal:** MIR is faster after passes.

- [ ] `ConstantFolding`, `DeadCodeElimination`, `CopyPropagation`, `UnreachablePruning`
- [ ] Benchmark: run `scripts/performance_bm.sh` equivalent in Fidan, measure improvement

### Phase 7 – Standard Library Core (2–3 weeks)
**Goal:** `std.io`, `std.string`, `std.math`, `std.collections`, `std.test` implemented.

- [ ] Module import system (`use std.io`)
- [ ] All listed stdlib modules (Rust implementation, Fidan-callable via FFI)
- [ ] `fidan test` command works, runs test blocks

### Phase 8 – AOT Backend (3–4 weeks)
**Goal:** `fidan build test.fdn -o test` produces a working binary.

- [ ] Cranelift `ObjectModule` setup
- [ ] MIR → Cranelift IR translation for all instruction types
- [ ] Runtime library (`fidan-runtime`) compiled as static library
- [ ] System linker invocation
- [ ] GC roots in compiled code (stack maps OR conservative scanning)
- [ ] Test: compiled binary matches interpreter output

### Phase 9 – Precompile JIT (2 weeks)
**Goal:** `@precompile` decorator accelerates functions in interpreter mode.

- [ ] Cranelift `JITModule` setup
- [ ] JIT compilation on first call to `@precompile` function
- [ ] ABI trampoline between interpreter stack and native calling convention
- [ ] Auto-detection of hot paths (call counter threshold)
- [ ] Benchmark: `@precompile` on a tight loop vs. without

### Phase 10 – CLI Polish & LSP (2–3 weeks)
**Goal:** Usable development experience.

- [ ] All `fidan` subcommands working
- [ ] REPL with history and multi-line input
- [ ] LSP server: diagnostics, hover, go-to-def, completion
- [ ] VS Code extension skeleton (JSON grammar + LSP client)
- [ ] Formatter (`fidan fmt`)

---

## 21. Pitfalls & Pre-planned Mitigations

| Pitfall | Mitigation |
|---|---|
| **Arena lifetime hell in Rust** | Use index-based references (`ExprId(u32)`) instead of raw `&'ast` references if lifetime inference becomes too complex. The arena is still used for storage; lookups go through an index. |
| **`is not` expression parsing** | Token-pair normalization in the parser (documented above). Test exhaustively: `a is not b`, `a is not nothing`, `not a is b`. |
| **Default param evaluation (Python trap)** | Default values are stored as `Expr` in the AST and re-evaluated at each call site during interpretation. Never evaluated once at definition time. |
| **Recursive object references causing ref-count cycles** | Bacon-Rajan cycle collector runs periodically. Objects with no external references but internal cycles are collected. |
| **JIT ABI mismatch between interpreter values and compiled code** | Define a clear `FidanABI` spec. All JIT functions receive and return tagged `FidanValue` structs. Trampolines handle boxing/unboxing. Tests verify ABI correctness for every type. |
| **`this` in free-function call of extension actions** | Clearly specified: `this === person` (the extension parameter) in free-function context. Implemented as a single consistent rule in MIR lowering. |
| **Exception unwind crossing compiled frames** | In AOT mode, use Dwarf unwinding (like Rust's panics). In interpreter mode, use an explicit unwind loop. In mixed mode (interpreter calling compiled), the ABI trampoline must also be a landing pad candidate. This is complex; handle it in Phase 9, not Phase 5. |
| **Concurrency + GC interaction** | In Phase 1, all concurrency is cooperative single-threaded (no real parallelism). The GC never runs concurrently with mutators in this phase. Multi-threaded GC is a Phase 2+ concern. |
| **String interpolation with complex nested expressions** | Recursively call the full expression parser. Limit nesting depth (`MAX_INTERP_DEPTH = 16`) to prevent pathological cases. Report a clean error if exceeded. |
| **`dynamic` type in AOT mode** | All `dynamic`-typed values are lowered to a 2-word tagged union in memory. Dispatch is handled by a runtime helper. This works but is slower; warn users that `dynamic` opts out of AOT type optimizations. |
| **Bootstrapping: stdlib written in Fidan calling Fidan** | Keep the stdlib in Rust until the compiler is self-hosting. Define a clear FFI surface (`@extern(rust)` decorator) that Fidan code can call into Rust. Bootstrap incrementally. |
| **Symbol `set` ambiguity** | `set` is always `Assign` from the lexer. `var x set 10` parses as `VarDecl { init: Assign(10) }`. A future collection type named `Set` uses `Set` (capitalized) as a type name, never as a keyword. Lowercase `set` is permanently reserved as `Assign`. |

---

*This document is the ground truth for the Fidan implementation. It should be updated as
decisions change. All architectural changes should be reflected here before code is written.*
