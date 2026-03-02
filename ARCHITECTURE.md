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
14. [Stage 11 – Codegen Backends (Cranelift JIT + LLVM AOT)](#14-stage-11--codegen-backends)
15. [Stage 12 – Standard Library](#15-stage-12--standard-library-fidan-stdlib)
16. [Stage 13 – Driver & Compilation Pipeline](#16-stage-13--driver--compilation-pipeline-fidan-driver)
17. [Stage 14 – CLI](#17-stage-14--cli-fidan-cli)
18. [Stage 15 – Language Server (LSP)](#18-stage-15--language-server-fidan-lsp)
19. [Key Technical Decisions & Rationale](#19-key-technical-decisions--rationale)
20. [Implementation Phases (Milestones)](#20-implementation-phases-milestones)
21. [Pitfalls & Pre-planned Mitigations](#21-pitfalls--pre-planned-mitigations)
22. [Differentiating Features Roadmap](#22-differentiating-features-roadmap)

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
              │  fidan-interp│  │cranelift (JIT ABI)│  │LLVM -O3 / LTO / PGO │
              │  MIR walker  │  │hot functions only │  │full native binary   │
              └──────────────┘  └───────────────────┘  └─────────────────────┘
                          │                      │                 │
                          └──────────────────────┴─────────────────┘
                                                 │
                                        fidan-runtime (always present)
                                 Memory │ Object model │ Stdlib │ Concurrency
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
├── test/
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
    ├── fidan-runtime/           ← Value types, memory model (owned/COW/ARC), object model, task scheduler
    ├── fidan-interp/            ← MIR interpreter
    ├── fidan-codegen-cranelift/ ← Cranelift backend — JIT only (`@precompile`, interpreter hot paths)
    ├── fidan-codegen-llvm/      ← LLVM backend — AOT only (`fidan build`, release binaries)
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
fidan-codegen-cranelift → fidan-mir, fidan-runtime          ← JIT only
fidan-codegen-llvm      → fidan-mir, fidan-runtime          ← AOT only; optional feature flag, requires LLVM
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

    // ── Concurrency & Parallelism ─────────────────────────────
    Concurrent,       // `concurrent` block — cooperative I/O-bound tasks
    Parallel,         // `parallel` block OR `parallel action` modifier OR `parallel for`
    Task,             // named task inside a concurrent/parallel block
    Spawn,            // `spawn expr` — explicit non-blocking parallel call → Pending oftype T
    Await,            // `await expr` — wait for a `Pending oftype T` to resolve
    Shared,           // `Shared oftype T` — built-in synchronized wrapper type
    For,              // `for item in collection` (also used by `parallel for`)
    In,               // `in` keyword for for-loops and parallel for

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
    Dot, Colon, DoubleColon, Arrow, FatArrow,
    Semicolon,        // `;` | `sep` — inline statement separator
    Newline,          // primary statement terminator (emitted by lexer after a logical line)
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
| `spawn` (at call site) | `Spawn` |
| `await` | `Await` |
| `stop`, `break` | `Break` |
| `sep`, `;` | `Semicolon` (inline statement separator) |

**Important:** Synonyms are resolved at lex time so the parser only ever sees canonical tokens.
The original source span is preserved so error messages reference the exact written form.

### Context-sensitive tokens

`set` is ambiguous: `var x set 10` (assignment) vs. a hypothetical type named `set`.
The lexer always emits it as `Assign`. The parser is responsible for determining meaning from
context. This keeps the lexer context-free.

Similarly `is` in `person is not nothing` tokenizes `is` → `Eq`, then `not` → `Not`.
The expression `a is not b` thus tokenizes to `a Eq Not b` and the parser rewrites this
compound to `a NotEq b` (`is not` in this context is the same as `isnot`) during a normalization pass (see Parser section).

### String Interpolation

String interpolation `"Hello {name}, you are {age} years old"` is handled in a two-step process:

1. Lexer: emits a `LitString` with the raw content (including `{...}` placeholders).
2. Parser: `"parse_string_interpolation"` splits the raw string at `{` / `}` boundaries,
   recursively parsing each embedded expression as a full expression. Produces an
   `Expr::StringInterp` AST node containing alternating `Expr::Lit(string)` and `Expr::X`
   fragments.

This keeps the lexer simple and places interpolation parsing where it belongs: in the parser,
which already knows about expressions.

### Statement Termination

Fidan uses **newlines as the primary statement terminator**. There are no mandatory semicolons.
The lexer implements the same rule as Go and Swift:

**A `Newline` token is emitted as a statement terminator when the last non-whitespace
token on the line is any of:**
- A literal (`integer`, `float`, `string`, `boolean`, `nothing`)
- An identifier or `Ident`
- A closing delimiter: `)`, `}`, `]`
- A postfix keyword: `return`, `break`, `continue`

**A newline is NOT emitted (logical line continues) when the last token is:**
- A binary operator (`+`, `-`, `*`, `/`, `and`, `or`, `greaterthan`, etc.)
- A comma `,` or `also`
- An opening delimiter: `(`, `{`, `[`
- The `then` or `with` keyword

This means multi-line expressions work naturally:
```fidan
var result set someFunction(
    arg1,
    arg2
)

var sum set 1 +
    2 +
    3
```

**Inline statement separation** (rare): use `;` or `sep` to put multiple
statements on one line. All three emit the same `Semicolon` token.
```fidan
var x set 1 stop var y set 2 stop print(x + y)
var x set 1; var y set 2; print(x + y)   # identical
```

The parser treats `Newline` and `Semicolon` identically as statement separators.

### Parentheses Rule

> **Parentheses in Fidan mean exactly one thing: wrapping a list of items or grouping a
> sub-expression. They are NEVER written around control-flow conditions or `for` bindings.**

| Position | Parens required? | Example |
|---|---|---|
| Action parameter list | **Yes** | `action foo with (required x -> integer)` |
| Lambda parameter list | **Yes** | `(x -> integer) returns integer => x * 2` |
| Call / constructor argument list | **Yes** | `add(1, 2)`, `Dog("Rex")` |
| Grouping inside an expression | **Yes** | `(a + b) * c` |
| `if` condition | **No — forbidden** | `if score > 90 {` ✅  `if (score > 90) {` ❌ |
| `while` condition | **No — forbidden** | `while count < 5 {` ✅  `while (count < 5) {` ❌ |
| `for` binding | **No — forbidden** | `for item in list {` ✅  `for (item in list) {` ❌ |

This keeps control-flow readable as plain English prose and gives parentheses a single,
unambiguous meaning in the grammar.

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

### Execution Model (decided 2026-02-28)

Fidan uses **top-to-bottom, declaration-hoisted execution** — like a JavaScript module, not
like a language that requires a mandatory `main` entry point.

**Rules:**
1. **Pass 1 – Hoist declarations:** All `object` and `action` definitions in the module are
   registered first, regardless of their textual position. This means forward references
   between actions are always valid.
2. **Pass 2 – Execute statements:** Top-level `var` declarations and bare expression
   statements (`print(...)`, `main()`, etc.) are executed in source order.

There is **no required `main` action**. If a programmer wants a clean entry point, the
convention is to define `action main { ... }` and call `main()` as the last line of the file.
This is a convention enforced by style, not by the compiler.

**AOT (`fidan build`):** The driver synthesizes a Rust `fn main()` (in the generated
binary's runtime stub) that runs the module's Pass 2 statement list in order. The user
never writes or sees this entry point — it is purely a linker-level detail.

**Interpreter / JIT:** Mirrors the same two-pass structure. The type checker already
implements this (register-in-pass-1, check-in-pass-2), so the interpreter follows the
same pattern naturally.

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
    pub is_parallel: bool,                      // `parallel action foo(...)` modifier
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
    Assign      { target: ExprRef<'ast>, value: ExprRef<'ast>, span: Span },
    If(IfStmt<'ast>),
    Attempt(AttemptStmt<'ast>),
    Concurrent(ConcurrentBlock<'ast>),          // `concurrent { task A {...} task B {...} }`
    Parallel(ParallelBlock<'ast>),              // `parallel { task A {...} task B {...} }`
    ParallelFor(ParallelForStmt<'ast>),         // `parallel for item in collection { ... }`
    For(ForStmt<'ast>),                         // `for item in collection { ... }`
    Return      { value: Option<ExprRef<'ast>>, span: Span },
    Panic       { value: ExprRef<'ast>, span: Span },   // `panic` / `throw`
    ExprStmt(ExprRef<'ast>),
    Block(Block<'ast>),
}

// ── Concurrency / Parallelism nodes ───────────────────────────────────────────

/// A named or anonymous task inside a `concurrent` or `parallel` block.
pub struct Task<'ast> {
    pub name:  Option<Symbol>,    // `task loadData { ... }` — name is optional
    pub body:  Block<'ast>,
    pub span:  Span,
}

/// `concurrent { task A {...} task B {...} }`
/// Tasks run cooperatively (may share one thread). Good for I/O-bound work.
/// All tasks must complete (or one fail) before the block exits.
pub struct ConcurrentBlock<'ast> {
    pub tasks: Vec<Task<'ast>>,
    pub span:  Span,
}

/// `parallel { task A {...} task B {...} }`
/// Tasks run on separate OS threads / thread pool workers simultaneously.
/// Good for CPU-bound work. All tasks join before the block exits.
pub struct ParallelBlock<'ast> {
    pub tasks: Vec<Task<'ast>>,
    pub span:  Span,
}

/// `parallel for item in collection { body }`
/// Runs each iteration simultaneously across the thread pool.
/// The loop body must satisfy the parallel capture safety rules.
pub struct ParallelForStmt<'ast> {
    pub binding:    Symbol,
    pub ty:         Option<TypeRef<'ast>>,
    pub iterable:   ExprRef<'ast>,
    pub body:       Block<'ast>,
    pub span:       Span,
}

/// `for item in collection { body }` (sequential)
pub struct ForStmt<'ast> {
    pub binding:    Symbol,
    pub ty:         Option<TypeRef<'ast>>,
    pub iterable:   ExprRef<'ast>,
    pub body:       Block<'ast>,
    pub span:       Span,
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
    /// `spawn crunch(data)` — non-blocking parallel call, evaluates to `Pending oftype T`
    Spawn    { call: ExprRef<'ast>, span: Span },
    /// `await pendingValue` — block until `Pending oftype T` resolves, yields T
    Await    { value: ExprRef<'ast>, span: Span },
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

// GENERIC TYPE SYNTAX IN FIDAN:
//
// Fidan does NOT use angle brackets `<>` for generic type parameters.
// The `oftype` keyword is used consistently for all type annotation contexts:
//
//   Single param:  `list oftype integer`
//                  `Shared oftype integer`
//                  `Pending oftype string`
//
//   Multi-param:   `dictionary oftype (string, integer)`
//                  (parentheses disambiguate from comma-separated parameters)
//
// In a variable declaration this produces a readable "double oftype":
//   `var items oftype list oftype integer`
// This reads naturally as "items of type list of type integer".
//
// `<>` syntax appears ONLY in Rust implementation code inside this document,
// never in Fidan source syntax.

pub enum Type<'ast> {
    Named    (Symbol, Span),
    // `list oftype integer`  →  Generic("list", [Named("integer")])
    // `dictionary oftype (string, integer)`  →  Generic("dictionary", [Named("string"), Named("integer")])
    // `Shared oftype integer`  →  Generic("Shared", [Named("integer")])
    // `Pending oftype string`  →  Generic("Pending", [Named("string")])
    Generic  (Symbol, Vec<TypeRef<'ast>>, Span),
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
├── parallel_check.rs   ← Capture safety analysis for parallel blocks and actions
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

### Parallel Capture Safety (`parallel_check.rs`)

This is the **most critical safety rule in Fidan's concurrency model**. When the type checker
enters a `parallel` block, a `parallel action` body, or a `parallel for` body, it activates a
`ParallelContext`. The rules enforced inside a `ParallelContext`:

| Captured variable | Usage in parallel context | Verdict |
|---|---|---|
| Immutable (`var x set 10`, never reassigned) | Read-only | ✅ Allowed |
| Mutable, captured by **one** task, not read by others | Read + write | ✅ Allowed (no sharing) |
| Mutable, captured by **multiple** tasks | Any mutation | ❌ Error E4xx |
| `Shared oftype T` | Read + write via `.get()` / `.update()` | ✅ Allowed |
| `Shared oftype T` | Direct field access bypassing API | ❌ Error |
| Object passed by value | Implicitly cloned | ✅ Allowed |
| Object passed with `move` keyword on task | Ownership transfer, not accessible in parent | ✅ Allowed |

The analysis in `parallel_check.rs`:
1. Builds a **capture set** for each task: the set of variables from enclosing scopes referenced.
2. Finds the **mutation set** for each task: variables that are assigned inside the task.
3. Checks for **intersection**: if variable `x` is in the mutation set of task A AND the
   capture set of task B (or vice versa), that is a data race → `E401: data race on variable 'x'`.
4. Suggests wrapping in `Shared oftype T` as the fix.

```
# Compile-time error example:
var counter = 0
parallel {
    task A { counter = counter + 1 }   # mutation
    task B { counter = counter + 1 }   # mutation of same var
}
# E401: data race: `counter` is mutated by both task A and task B
# help: wrap in `Shared`: var counter oftype Shared oftype integer = Shared(0)
#       (or let type be inferred): var counter = Shared(0)
#       then use:                  counter.update(x => x + 1)
```

```
# OK example:
var counter = Shared(0)
parallel {
    task A { counter.update(x => x + 1) }
    task B { counter.update(x => x + 1) }
}
print(counter.get())   # 2
```

**For `parallel for`** loops: the body is a single task logically. The only safety check is
that each iteration does not mutate a variable shared with other iterations (i.e., no
loop-carried dependency on mutable state). Mutations local to the iteration body are always safe.

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
    Assign          { dest: LocalId, ty: MirTy, rhs: Rvalue },
    Call            { dest: Option<LocalId>, callee: Callee, args: Vec<Operand>, span: Span },
    NullCheck       { scrutinee: Operand, span: Span },  // inserted by null-safety pass
    SetField        { object: Operand, field: Symbol, value: Operand },
    GetField        { dest: LocalId, object: Operand, field: Symbol },
    Drop            { local: LocalId },           // explicit scope-end: owned value is destroyed here

    // ── Concurrency ────────────────────────────────────────────────────
    /// Spawn a function as a cooperative green-thread task (for `concurrent` blocks)
    SpawnConcurrent { handle: LocalId, task_fn: FunctionId, args: Vec<Operand> },
    /// Spawn a function onto the OS thread pool (for `parallel` blocks and `parallel action`)
    SpawnParallel   { handle: LocalId, task_fn: FunctionId, args: Vec<Operand> },
    /// Wait for ALL given join handles before proceeding (end of a concurrent/parallel block)
    JoinAll         { handles: Vec<LocalId> },
    SpawnExpr       { dest: LocalId, task_fn: FunctionId, args: Vec<Operand> },  // `spawn expr` → `Pending oftype T` handle
    /// `await pending` → blocks current task until the `Pending oftype T` resolves, stores result
    AwaitPending    { dest: LocalId, handle: Operand },
    /// `parallel for` — distributes iterations over the thread pool via Rayon
    /// `body_fn` receives a single element and returns nothing; captures are passed as `closure_args`
    ParallelIter    { collection: Operand, body_fn: FunctionId, closure_args: Vec<Operand> },
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
4. **Concurrency lowering**:
   - `concurrent { task A {...} task B {...} }` → each task body is lifted to its own
     synthetic `MirFunction`. Then: `SpawnConcurrent` for each, `JoinAll` at block end.
   - `parallel { task A {...} task B {...} }` → same structure but uses `SpawnParallel`.
   - `parallel for item in collection { body }` → body lifted to a synthetic `MirFunction`
     receiving `item` as its first parameter; lowered to `ParallelIter`.
   - `spawn expr` → `SpawnExpr`, result is `Pending oftype T`.
   - `await pending` → `AwaitPending`.
   - Captured immutable variables are passed as `closure_args`. `Shared oftype T` values
     are passed as `Arc<Mutex<FidanValue>>` pointers (Rust). This is enforced by `parallel_check.rs` before
     lowering, so no unsafe sharing reaches the MIR level.
5. **`this` binding**: In extension actions, `this` is given its own `LocalId` and wired
   appropriately by the call-site lowering.
6. **`parent.method()` calls**: Lowered to `Callee::Fn(resolved_parent_method_id)` with
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
// In AOT mode, this is replaced by typed native LLVM values.
//
// OwnedRef<T>  — interpreter-internal Rc<RefCell<T>>. The Fidan type checker
//               guarantees only ONE Fidan-level owner exists; the Rc is an
//               implementation convenience (the interpreter's own data structures
//               may hold temporary Rust references during a single evaluation step).
//               Never exposed to user code. In AOT, lowered to Box<T> or alloca.
//
// SharedRef<T> — Arc<Mutex<T>>. Used ONLY for `Shared oftype T` values.

pub enum FidanValue {
    Integer  (i64),
    Float    (f64),
    Boolean  (bool),
    Nothing,
    String   (OwnedRef<FidanString>),
    List     (OwnedRef<FidanList>),
    Dict     (OwnedRef<FidanDict>),
    Object   (OwnedRef<FidanObject>),
    Shared   (SharedRef<FidanValue>),    // `Shared oftype T` — explicit ARC
    Function (FunctionId),               // first-class function reference
}

impl FidanValue {
    pub fn type_name(&self) -> &'static str { ... }
    pub fn is_nothing(&self) -> bool { matches!(self, Self::Nothing) }
    pub fn truthy(&self) -> bool { ... }   // for boolean coercions
}
```

For **NaN-boxing** (a future optimization): pack the entire `FidanValue` into 8 bytes using
IEEE 754 NaN payloads. Deferred to post-MVP.

### Memory Model

Fidan uses **move-by-default ownership** with Copy-on-Write collections and selective ARC.
There is no garbage collector. Memory is freed **deterministically at scope exit**.
The user never calls `free()` or thinks about lifetimes — the compiler handles everything.

#### Three-tier model

| Tier | Types | Mechanism | Cost |
|---|---|---|---|
| **1. Primitives** | `integer`, `float`, `boolean`, `nothing` | Always copied (stack) | Zero |
| **2. Owned values** | `string`, `list`, `dict`, user `object` types | Move semantics + COW | Zero in common path |
| **3. Explicit shared** | `Shared oftype T` | ARC (`Arc<Mutex<T>>`) | Only where user opts in |

#### Tier 2 — Move semantics in detail

Every heap value has **exactly one owner** at every point in the program.

```fidan
var a set Person(name: "Alice")   # a owns the Person
var b set a                        # ownership MOVES to b; a is now invalid
print(b.name)                      # fine
print(a.name)                      # compile error: a was moved
```

When a function call moves the value and the caller also uses it after — the compiler
automatically inserts a **clone** (explicit clone never required by the user):

```fidan
var a set Person(name: "Alice")
someFunction(a)    # compiler sees a is used below → inserts clone automatically
print(a.name)      # still valid; a was cloned into someFunction, original kept here
```

The compiler emits a **hint** (not an error) when it inserts an implicit clone of a
large object (list, dict, deep object graph), so the user can optimize if they care.

#### Copy-on-Write for collections

`list`, `dict`, and `string` use COW internally:
- Passing a collection to a function is a **cheap pointer copy** regardless of size.
- The **physical data is only duplicated at the moment of first mutation** inside the callee.
- Read-only operations (iteration, length, get) never trigger a copy.
- This means passing a 10-million-element list to a read-only function costs effectively zero.

```fidan
var data set list(1, 2, 3, 4, 5)   # one allocation
var count set data.length           # no copy; COW read — free
data.append(6)                      # mutates; COW copy happens here only
```

#### Deterministic drop

When an owned value's scope ends, its memory is freed immediately — no pause, no background
collector, no reference counter decrement chain.

```rust
// Runtime representation of owned heap objects
pub struct OwnedBox<T> {
    data: *mut T,     // raw heap pointer; freed in Drop
}
impl<T> Drop for OwnedBox<T> {
    fn drop(&mut self) { unsafe { dealloc(self.data) } }
}
```

#### Tier 3 — `Shared oftype T` (explicit ARC)

When the programmer explicitly writes `Shared oftype T`, they are opting into multiple
ownership — typically because the value crosses thread boundaries in a `parallel` block.

```rust
// Runtime representation of Shared
pub struct Shared<T>(Arc<Mutex<T>>);
```

- ARC overhead (atomic increment/decrement) exists **only** for `Shared` values.
- For data that is intentionally shared across threads, the synchronization cost already
  dominates — ARC overhead is negligible in that context.
- **Cycle prevention:** `Shared` values that need back-references use `WeakShared oftype T`
  (a non-owning reference, implemented as `Weak<Mutex<T>>`). The compiler warns when a
  `Shared` graph contains a statically-detectable ownership cycle.
- There is **no cycle collector** anywhere in the runtime.

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

### Concurrency & Parallelism Model

Fidan makes a **hard, explicit distinction** between two concepts that most languages conflate:

| | `concurrent` | `parallel` |
|---|---|---|
| **What it means** | Multiple tasks making progress | Multiple tasks executing simultaneously |
| **CPU usage** | Possibly one core (cooperative) | Multiple cores (true multi-threading) |
| **Best for** | I/O-bound work (network, file, UI) | CPU-bound work (computation, data processing) |
| **Data safety** | Relaxed (one thread at a time) | Strict (compile-time race checking) |
| **Runtime mechanism** | Green threads (cooperative yield) | OS threads via Rayon thread pool |
| **Keyword** | `concurrent` | `parallel` |

Both forms are **structured**: every task has a parent scope. Tasks cannot escape their
enclosing block. The block exits only after all tasks complete (or one fails).

---

#### Form 1: `concurrent` block (I/O-bound structured concurrency)

```fidan
concurrent {
    task fetchUserData  { var data = std.net.get("https://api.example.com/user") }
    task fetchConfig    { var cfg  = std.io.readFile("config.json") }
}
# resumes here only after both tasks complete (or one fails)
```

**Runtime:** Cooperative green-thread scheduler. Yield points occur at I/O boundaries.
All concurrent tasks may run on a single OS thread. No data-race checking needed because
tasks cannot execute simultaneously.

---

#### Form 2: `parallel` block (CPU-bound structured parallelism)

```fidan
parallel {
    task processChunk1 { var r1 = crunch(data[0..500]) }
    task processChunk2 { var r2 = crunch(data[500..1000]) }
}
```

Each task runs on a **separate OS thread** from Rayon's global thread pool. Tasks truly
execute simultaneously. **Compile-time capture safety** (enforced by `parallel_check.rs`)
guarantees no data races.

---

#### Form 3: `parallel action` modifier (thread-pool dispatch)

Marks an action as intended for parallel execution. When **called normally**, it blocks the
calling thread until the result is ready (transparent to the caller). When called via `spawn`,
it returns immediately with a `Pending oftype T`.

```fidan
parallel action crunch(data oftype list) returns list {
    return data.map(x => x * x)
}

# Blocking call (runs on thread pool, caller waits):
var result = crunch(myData)

# Non-blocking spawn (runs on thread pool, caller continues immediately):
var pending = spawn crunch(myData)
# ... do other work ...
var result = await pending
```

**Important:** `parallel action` is not just a decorator — it is a modifier that changes the
action's type. The return type of a `parallel action foo() returns T` is `T` (when called
blocking) or `Pending oftype T` (when called via `spawn`). The type checker knows this distinction.

---

#### Form 4: `parallel for` (data parallelism)

```fidan
var results = list()
parallel for item in largeCollection {
    var processed = expensiveTransform(item)
    results.append(processed)    # ERROR: E401 — `results` mutated from parallel context
}

# Correct pattern: use parallelMap from stdlib instead
var results = largeCollection.parallelMap(item => expensiveTransform(item))
```

`parallel for` with a mutable shared accumulator is a classic data race. `parallelMap`
from `std.collections` is the idiomatic solution — it returns a new list with transformed
elements, with no shared mutable state.

For side-effect-only parallel iteration (e.g., writing to independent files):

```fidan
parallel for item in files {
    std.io.writeFile(item.path, item.content)   # no shared state, safe
}
```

---

#### `Shared oftype T` — explicit synchronized state

The only way to safely **share mutable state** across parallel tasks.

**Fidan syntax:**
```fidan
# Type is inferred from the initial value (preferred style):
var counter = Shared(0)
var results = Shared(list())

# Explicit type annotation (both oftype keywords are deliberate and readable):
var counter oftype Shared oftype integer = Shared(0)
var results oftype Shared oftype list = Shared(list())

parallel {
    task A {
        counter.update(x => x + 1)
        results.update(r => r.append("A done"))
    }
    task B {
        counter.update(x => x + 1)
        results.update(r => r.append("B done"))
    }
}

print(counter.get())    # 2
print(results.get())    # ["A done", "B done"] (order may vary)
```

`Shared oftype T` API (surface-level Fidan):
- `Shared(initialValue)` — create; type parameter inferred
- `.get() returns T` — read (acquires lock, copies value, releases)
- `.update(transform oftype action(T) returns T)` — atomic read-modify-write
- `.withLock(action(T) returns nothing)` — hold lock for entire block (for complex mutations)

**Rust implementation:** `Arc<Mutex<FidanValue>>` (the `<>` here is Rust, not Fidan syntax).

---

#### `Pending oftype T` — non-blocking parallel handle

**Fidan syntax:**
```fidan
# Type inferred from the return type of the spawned action:
var p1 = spawn crunch(data1)   # p1 has type: Pending oftype list
var p2 = spawn crunch(data2)

# Explicit annotation (both oftype keywords deliberate):
var p1 oftype Pending oftype list = spawn crunch(data1)

# ... do other sequential work while tasks run ...

var r1 = await p1    # blocks until p1 resolves, result has type: list
var r2 = await p2
```

If a `spawn`ed task throws an error and nobody `await`s it, the runtime issues a **warning**
at the point where the `Pending oftype T` is dropped without being awaited (similar to Rust's
`#[must_use]` for `Result`). Fidan enforces this with a W3xx warning class.

---

#### Implementation Plan

**Phase 1 — `concurrent` only (single-threaded cooperative):**
- Rust crate: [`corosensei`](https://crates.io/crates/corosensei) (safe, stackful coroutines)
- All concurrent tasks run on a single OS thread, scheduled cooperatively
- Yield at I/O boundaries automatically (the runtime inserts yield points at stdlib I/O calls)
- No `parallel` keyword support yet — produces a clear error: "parallel is not yet supported"
- No data-race checking needed in this phase (everything is sequential under the hood)

**Phase 2 — `parallel` blocks, `parallel action`, `parallel for`:**
- Rust crate: [`rayon`](https://crates.io/crates/rayon) for the thread pool
- `SpawnParallel` / `JoinAll` MIR instructions map to `rayon::spawn` + `rayon::join`
- `ParallelIter` maps to `rayon::iter::ParallelIterator` (`.par_iter()` under the hood)
- `Shared oftype T` implemented as `Arc<Mutex<FidanValue>>`
- `WeakShared oftype T` implemented as `Weak<Mutex<FidanValue>>` for back-references
- `Pending oftype T` implemented as a wrapper around `std::thread::JoinHandle<FidanValue>`
  or a Rayon future
- Owned values that do NOT cross thread boundaries remain `OwnedBox<T>` — no ARC cost
- Values that DO cross boundaries must be typed `Shared` at the call site — the type
  checker enforces this; passing a non-Shared owned value into a `parallel` block
  **moves** it into the block (one owner, the task)

---

**No GIL. No undefined behavior. No need for the user to understand async/await internals.**
The user writes `concurrent` or `parallel` and gets the right behavior.

---

## 13. Stage 10 – Interpreter Backend (`fidan-interp`)

> Purpose: Execute MIR directly, as fast as possible, without compilation.

### Design: MIR Walker with Value Stack

```rust
pub struct Interpreter {
    pub session:  Arc<Session>,
    pub runtime:  Arc<Runtime>,       // memory model (owned/ARC ops), stdlib, task scheduler
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

## 14. Stage 11 – Codegen Backends

> Two separate crates. Same MIR input. Different performance/latency trade-offs.

```
                   MIR (optimized)
                       │
          ┌──────────────┴────────────────┐
          │                               │
  fidan-codegen-cranelift         fidan-codegen-llvm
  JIT mode only                   AOT mode only
  Purpose: low latency             Purpose: maximum performance
  Used by: `fidan run`             Used by: `fidan build`
           `@precompile`                    release binaries
           auto hot-path JIT               any `--release` flag
  Compilation speed: ~ms           Optimization quality: -O3
  Code quality: ~85% of LLVM       + vectorization (SIMD)
                                   + LTO (whole-program analysis)
                                   + PGO (profile-guided)
                                   + monomorphization
```

### 14a. `fidan-codegen-cranelift` — JIT Only

**Why Cranelift for JIT:**
- Compilation latency is measured in **milliseconds** — acceptable at runtime
- Pure Rust, no C++ toolchain dependency
- Actively maintained (Wasmtime, `cg_clif`)
- Code quality is good, not optimal — perfectly acceptable for `@precompile` 
  hot paths that just need to beat the MIR interpreter

**What it is NOT:** Cranelift is never used to produce release binaries in the final architecture.
During Phase 8 of development it temporarily also handles AOT as a stepping stone (for
correctness validation before the LLVM backend is ready), but that is a transitional state.

#### MIR → Cranelift IR Mapping

```
MirFunction           → cranelift::ir::Function
BasicBlock            → cranelift::ir::Block
LocalId (SSA value)   → cranelift::ir::Value
```

Types passed between the MIR interpreter and JIT-compiled functions use the **Fidan JIT ABI**:

| `FidanValue` variant | JIT ABI type |
|---|---|
| `Integer(i64)` | `I64` |
| `Float(f64)` | `F64` |
| `Boolean(bool)` | `I8` |
| Heap types (String, List, Object) | `I64` (pointer into Fidan heap — owned or `Shared`) |
| `Nothing` | `I64` value `0` |

#### JIT Compilation Path

```rust
use cranelift_jit::{JITBuilder, JITModule};

pub struct JitCompiler {
    module: JITModule,
}

impl JitCompiler {
    /// Called when a @precompile function is first invoked, or when the
    /// call-count threshold is crossed for automatic hot-path detection.
    pub fn compile_function(&mut self, mir_fn: &MirFunction) -> *const u8 { ... }
}
```

The compiled function pointer replaces the interpreter's dispatch entry for that `FunctionId`.
Subsequent calls use the native code directly. A safe trampoline handles the ABI boundary
between the interpreter's `Vec<FidanValue>` argument list and the native calling convention.

#### JIT Compilation Strategy: Lazy by Default, Eager by Annotation

**Decision:** The JIT is **lazy** (compile-on-first-hot-call) by default.  
`@precompile` is the user-directed **eager** escape hatch that forces compilation at program start.

**Why lazy:**
- Startup latency: eager JIT compiles every reachable function before any code runs. For
  programs where only 20% of functions are ever called, 80% of JIT budget is wasted.
- Dead code is never compiled. Error handlers, rarely-triggered branches, imported-but-unused
  utility functions all have zero cost.
- Tiered compilation is only possible when the cold path (interpreter) runs first and
  generates the call-frequency data needed to decide what to compile.
- `@precompile` gives back eagerness exactly where the user knows it is needed — the right
  division of labour between compiler and programmer.

**Call-counter model:**
```
Per-function call counter in MirMachine (u32, resets at u32::MAX)
    │
    ├── count < JIT_THRESHOLD (default: 500)  →  interpret via MirMachine
    │
    └── count >= JIT_THRESHOLD                →  compile with Cranelift JIT
                                                  store native ptr in dispatch table
                                                  replace MirMachine dispatch entry
                                                  subsequent calls → native code directly
```

`@precompile` pre-sets the counter to `JIT_THRESHOLD` so the function is compiled on its
very first call, before any interpreter warmup. The threshold is tunable via
`--jit-threshold N` for benchmarking and experimentation.

---

### 14b. `fidan-codegen-llvm` — AOT Only

**Why LLVM for AOT:**
- `fidan build` compiles **once**, runs **forever** — latency doesn't matter, quality does
- LLVM -O3 is the industry standard for production native code quality
- Auto-vectorization (SIMD for free on eligible loops)
- Link-Time Optimization (LTO): whole-program analysis across all functions
- Profile-Guided Optimization (PGO): instrument → profile → recompile with real data
- Monomorphization eliminates all boxing for generic types (see below)
- Same backend used by Rust, Swift, Clang — proven, stable, battle-tested

**Rust crate:** [`inkwell`](https://crates.io/crates/inkwell) — safe Rust bindings for LLVM.

#### MIR → LLVM IR Mapping

```
MirFunction           → llvm::Function
BasicBlock            → llvm::BasicBlock
LocalId (SSA value)   → llvm::Value*  (LLVM is also in SSA form — direct 1:1 match)
PhiNode               → llvm::PHINode*
```

In AOT mode with full type information, all values are **unboxed to native LLVM types**:

| `FidanValue` | Unboxed LLVM type |
|---|---|
| `Integer(i64)` | `i64` |
| `Float(f64)` | `double` |
| `Boolean(bool)` | `i1` |
| `String` | `%FidanStr*` (pointer to owned heap struct) |
| `List oftype integer` | `%FidanList_i64*` (monomorphized — stores raw `i64[]`) |
| `List oftype T` (generic) | `%FidanList*` (boxed, only if T unknown at compile time) |
| `Nothing` | `i64` value `0` |
| `dynamic` | `%FidanTaggedUnion` (2× `i64`: tag + payload) |

#### Monomorphization

This is the single most impactful feature for C++-competitive performance.

When the type checker knows the concrete type parameter at a call site:
```fidan
var ints oftype list oftype integer = list()
ints.append(1)      # T is statically `integer` here
```

The codegen generates a **specialized** LLVM function `list_append_integer` that operates
directly on `i64[]` — no boxing, no `FidanValue` enum, no heap allocation for the element.

Process:
1. During MIR→LLVM lowering, collect all **concrete instantiations** of generic functions
   (tracked by `fidan-typeck`'s monomorphization collector).
2. For each unique concrete instantiation, emit a separate specialized LLVM function.
3. Call sites use the specialized function directly.
4. The generic (boxed) version is emitted only if `dynamic` types require it.

This is exactly how C++ templates work, and how Rust generics work. It eliminates the
primary boxing overhead for all generic stdlib types.

#### LLVM Optimization Pipeline

```rust
// Applied in order for `fidan build --release`:
pass_manager.add_inline_pass();                  // inline small functions
pass_manager.add_promote_memory_to_register();   // mem2reg (uses LLVM's own SSA)
pass_manager.add_gvn_pass();                     // global value numbering
pass_manager.add_loop_vectorize_pass();          // auto SIMD
pass_manager.add_slp_vectorize_pass();           // superword-level parallelism
pass_manager.add_dead_store_elimination_pass();
pass_manager.add_aggressive_dead_code_elimination_pass();
pass_manager.run_on_module(&module);
// Then: LTO via llvm-lto or linker plugin
```

#### Escape Analysis (stack allocation)

Before LLVM codegen, a MIR pass (`EscapeAnalysis`) checks each object allocation:
- If an owned value never escapes its creating function (never stored in a field, never
  returned, never passed to a function that stores it) → it is **stack-allocated**.
- Stack allocation = no heap allocator call, no `OwnedBox` overhead, no drop bookkeeping.
- For small, short-lived objects (the vast majority) this is a massive win.

Implemented in `fidan-passes` as a pre-LLVM MIR pass. Produces an `Allocation` annotation:
```rust
pub enum AllocationKind { Stack, HeapOwned, HeapShared }
```
The LLVM codegen respects this: stack-allocated objects use `alloca`, heap-owned use
`OwnedBox<T>`, heap-shared use `Arc<Mutex<T>>`.

#### PGO (Profile-Guided Optimization)

```
fidan build --instrument program.fdn -o program_instrumented
./program_instrumented < real_workload.txt
fidan build --use-profile program.fdn -o program_optimized
```

LLVM's PGO instruments branch frequencies and function call counts, then uses the real-world
profile to:
- Reorder basic blocks for better branch prediction
- Inline hot call sites more aggressively
- Prioritize vectorization of hot loops

#### LTO (Link-Time Optimization)

Enabled with `fidan build --lto`. Passes LLVM bitcode to the linker stage. This allows:
- Inlining across module boundaries (e.g., inlining stdlib functions into user code)
- Whole-program dead code elimination
- Cross-module constant propagation

#### AOT Object File and Linking

```rust
pub struct AotCompiler {
    context:  inkwell::context::Context,
    module:   inkwell::module::Module<'ctx>,
}

impl AotCompiler {
    pub fn compile_program(&mut self, program: &MirProgram) -> Vec<u8> {
        // ... emit LLVM IR, run pass manager, emit object file bytes
    }
}
```

Output is linked with:
- `fidan-runtime` (precompiled static `.a`)
- `fidan-stdlib` (precompiled static `.a`)
- System libraries

Linker invocation: `cc -o output main.o libfidan_runtime.a libfidan_stdlib.a`
On Windows: `link.exe` or `lld` is used instead.

#### Platform ABI

- `this` is always the first parameter for methods
- Primitives passed in registers; heap objects as pointers
- Tail call optimization: automatic for direct self-recursion; `@tailcall` for explicit opt-in
- Return values > 8 bytes: sret (pointer to caller-allocated return slot)
- Exception unwind: DWARF on Linux/macOS, SEH on Windows (LLVM handles this natively)

---

### Performance Roadmap

**Honest assessment of where Fidan lands vs C++ at each phase:**

| Phase | Mode | vs. C++ single-thread | vs. C++ parallel |
|---|---|---|---|
| MVP (interpreter) | `fidan run` | ~2–10% | ~20–50% (GIL-free) |
| Phase 9 (Cranelift JIT) | `@precompile` hot paths | ~50–70% | ~80–100% |
| Phase 8 (Cranelift AOT) | `fidan build` | ~75–85% | 100–120% |
| Phase 11 (LLVM AOT, no mono) | `fidan build --release` | ~85–95% | 110–130% |
| Phase 11+ (LLVM + monomorphization + escape analysis) | `fidan build --release` | **95–110%** | **120–150%** |
| Phase 11+ (+ PGO + LTO) | `fidan build --release --pgo` | **100–120%** | **130–200%** |

**Notes on beating C++:**
- Single-threaded compute: competitive but not reliably faster. C++ has decades of LLVM/GCC
  hand-tuning. Fidan can match it with LLVM -O3 + monomorphization + escape analysis.
- Parallel workloads: Fidan can **genuinely exceed** C++ because parallelism is first-class
  syntax — users actually use it. C++ requires TBB/OpenMP which almost nobody writes in
  practice. `parallelMap` on a 16-core machine beats hand-written C++ that is still sequential
  because the developer didn't bother with TBB.
- Bootstrapping the compiler to Fidan does **not** affect user program performance — the
  runtime stays in Rust regardless of whether the compiler itself is written in Fidan.
- The `dynamic` type permanently opts out of monomorphization and some AOT optimizations.
  Users who want peak performance should use typed variables.

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
| `std.collections` | Set, Queue, Deque, BTreeMap, `.parallelMap()`, `.parallelFilter()` | `std::collections` + `rayon` |
| `std.math` | sin, cos, sqrt, floor, ceil, abs, min, max, random | `std::f64` |
| `std.string` | split, join, trim, replace, contains, startsWith, endsWith, toUpper, toLower | Rust String methods |
| `std.concurrent` | Task, Channel (async/IO-bound), cooperative scheduler helpers | `corosensei` |
| `std.parallel` | `Shared oftype T`, `Pending oftype T`, `parallelMap`, `parallelFilter`, `parallelFor` | `rayon` |
| `std.debug` | assert, assertEq, inspect, profile | custom |
| `std.test` | describe/it test blocks, expect(...).to... matchers | custom |
| `std.cli` | Argument parsing, colored output, progress bars | `clap`, `indicatif` |
| `std.time` | DateTime, Duration, wait | `chrono` |
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

> **Current implementation:** The REPL uses the direct AST-walking interpreter (`fidan-interp`)
> as a bootstrap shortcut — it provides stateful, line-by-line execution without requiring a
> full MIR re-compilation on each input.  This is intentional and correct for now.
>
> **Planned migration (Phase 10):** The REPL will be migrated to the MIR pipeline, giving it
> the same execution semantics as `fidan run`.  The approach is an incremental MIR append model:
> each new line is lowered to MIR and merged into the persistent `MirProgram`, after which only
> the newly-emitted basic blocks are executed.  The AST-walking interpreter (`interp.rs`) will
> be retired once this migration is complete.

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

### 1. Cranelift for JIT, LLVM for AOT

**Decision:** Cranelift is the **JIT-only** backend (`@precompile`, interpreter hot paths).
LLVM is the **AOT-only** backend (`fidan build`, release binaries). These are not alternatives
or fallbacks — they are complementary tools with distinct roles.

**Rationale:**  
Cranelift's strength is **compilation speed** — it can JIT-compile a function in milliseconds,
which is the only metric that matters at runtime. Its code quality (~85% of LLVM -O3) is more
than sufficient for hot paths that just need to beat the MIR interpreter.  
LLVM's strength is **code quality** — -O3, auto-vectorization, LTO, PGO, and the resulting
binary runs forever without recompilation. Latency is acceptable because AOT compilation
happens once.  
This split is the same model used by: Firefox (Baseline JIT → IonMonkey/LLVM), Java HotSpot
(profiling tier → C2), Julia (all LLVM but same reasoning applies). It is proven.

### 2. Arena Allocation for AST

**Decision:** All AST nodes are arena-allocated.  
**Rationale:** Avoids complex lifetime management, gives `O(1)` allocation, trivial deallocation,
and allows `Copy` node references throughout the codebase.

### 3. Move-by-Default Ownership with COW and Selective ARC (no GC)

**Decision:** No garbage collector of any kind. Memory is managed via move semantics,
Copy-on-Write for collections, and explicit `Shared oftype T` (ARC) only where the user
opts into shared ownership.  
**Rationale:** A GC — even a well-tuned one — cannot reach C++-level single-threaded
performance because it introduces allocation overhead, heap fragmentation, and unpredictable
collection pauses. Move semantics with COW gives deterministic, zero-overhead memory
management that the user never has to think about. ARC cost is pay-as-you-go: you pay it
exactly and only where you wrote `Shared`. Cycles (the traditional ARC failure case) are
handled by `WeakShared` back-references — no cycle collector needed.  
**Trade-off acknowledged:** The compiler must perform ownership inference and insert implicit
clones. This is non-trivial to implement correctly. A clone-too-eagerly compiler is correct
but slow; a clone-too-rarely compiler is wrong. The test suite must have extensive tests
for clone elision. The compiler should also emit hints when large implicit clones are inserted
so the user can understand the performance model.

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

### 7. Two-tier Concurrency Model (`concurrent` vs `parallel`)

**Decision:** Use `concurrent` for cooperative I/O-bound tasks (green threads, possibly
single-threaded) and `parallel` for true multi-core CPU-bound work (Rayon thread pool).
These are two distinct keywords with distinct semantics, not aliases.

**Rationale:** Conflating concurrency and parallelism is the single biggest source of
confusion in languages like Python (the GIL vs asyncio vs threads mess). Fidan makes the
distinction explicit in the syntax. A user who writes `concurrent` gets cooperative scheduling
and does not need to think about data races. A user who writes `parallel` opts into
full multi-core execution and the compiler enforces data safety via `parallel_check.rs`.
Green threads (`corosensei`) handle the cooperative case without surfacing async/await.
Rayon handles the parallel case without requiring users to manage thread lifetimes.

### 8. String Interpolation as Parser Concern, not Lexer

**Decision:** Lexer emits raw string content; parser splits and recursively parses interpolated
expressions.  
**Rationale:** Interpolated expressions can be arbitrarily complex. Parsing them in the lexer
would require the lexer to embed a mini-parser, which is messy and error-prone.

### 9. Lazy JIT with User-Directed Eager Escape Hatch

**Decision:** The Cranelift JIT is **lazy by default**. A function is compiled only when its
call counter crosses a configurable threshold (default: 500 calls). `@precompile` is the
explicit annotation that triggers eager compilation on the very first call.  
**Rationale:**  
- Lazy JIT avoids wasting compilation budget on cold or dead code paths, keeping startup
  latency low.  
- Tiered execution (interpret → JIT) is the proven model used by every production VM
  (Firefox SpiderMonkey, Java HotSpot, .NET Core RyuJIT). The interpreter is not a weakness;
  it is the cold-path tier that builds the frequency data needed to decide what to compile.
- `@precompile` restores eager compilation for the specific hot functions the programmer
  already knows about — without forcing the entire program to pay JIT compile time upfront.
- The threshold is tunable (`--jit-threshold N`) so it can be adjusted for short vs.
  long-running programs and benchmarked against interpreter-only and AOT baselines.

### 10. MIR as the Sole Interpretation Medium (Bytecode Deferred)

**Decision:** The interpreter works directly on MIR (SSA form CFG). A further lowering to
a compact linear bytecode is explicitly deferred to a future phase and only to be implemented
if profiling demonstrates a measurable bottleneck in the MIR interpreter itself.  
**Rationale:**  
- MIR is already flat, typed, and optimized by the pass manager. A bytecode tier would
  primarily remove phi-node resolution overhead and improve dispatch-loop cache locality —
  real but modest gains. Profiling MIR first is mandatory before paying the cost of an
  additional IR.
- The current performance bottleneck is `FidanValue` boxing and `Rc`/`Arc` reference counting,
  not interpreter dispatch speed. Bytecode does not address boxing overhead.
- Adding bytecode creates a third IR to maintain, breaks span/source-location mapping (needs
  a separate offset→span table), and duplicates the optimization story.
- If bytecode is ever added, it becomes **Tier 0.5** between MIR and JIT — MIR is still the
  canonical form for all three backends (bytecode, Cranelift JIT, LLVM AOT). The MIR
  interpreter (`MirMachine`) would then be retired in favour of the bytecode interpreter.
- **The criteria for scheduling bytecode:** profiling after Phase 9 shows that MIR dispatch
  (not value boxing, not I/O) is >20% of runtime on a representative workload.

---

## 20. Implementation Phases (Milestones)

### Phase 0 – Skeleton (1–2 weeks)
**Goal:** Cargo workspace compiles. Each crate exists. Integration test harness exists.

- [ ] Set up Cargo workspace with all 14 crates (initially empty)
- [ ] `fidan-source`: `SourceFile`, `Span`, `SourceMap`, `SymbolInterner`
- [ ] Integration test: load `test/examples/test.fdn` and print its contents
- [ ] CI setup (GitHub Actions: `cargo test`, `cargo clippy`, `cargo fmt --check`)

### Phase 1 – Lexer (1–2 weeks)
**Goal:** Tokenize `test/examples/test.fdn` correctly.

- [ ] Implement all `TokenKind` variants
- [ ] Synonym normalization table (`phf` map)
- [ ] `#` and `#/ ... /#` (nested) comment handling
- [ ] `CommentStore` for formatter round-trip
- [ ] Span tracking
- [ ] Symbol interning integration
- [ ] Unit tests: every token type, all synonyms, nested comments, string with interpolation

### Phase 2 – AST + Parser (2–3 weeks)
**Goal:** Parse `test/examples/test.fdn` to AST and pretty-print it back.

- [ ] All AST node types with arena allocation
- [ ] Recursive descent parser: items, statements
- [ ] Pratt expression parser with full precedence table
- [ ] Ternary and null-coalesce parsing
- [ ] Named argument call parsing
- [ ] Extension action declaration parsing
- [ ] `parallel action` modifier parsing
- [ ] `concurrent { task ... }` block parsing
- [ ] `parallel { task ... }` block parsing
- [ ] `parallel for item in collection { ... }` parsing
- [ ] `for item in collection { ... }` (sequential) parsing
- [ ] `spawn expr` and `await expr` parsing
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
- [ ] `parallel action` type registration (`T` vs `Pending oftype T` depending on call form)
- [ ] `parallel_check.rs`: capture set + mutation set intersection analysis (data race detection)
- [ ] `Shared oftype T` type: recognized as safe in parallel contexts
- [ ] `Pending oftype T` type: inferred from `spawn expr`; `await pending` unboxes to `T`
- [ ] W3xx warnings: unawaited `Pending oftype T` dropped without `.wait()` or `await`

### Phase 3.5 – Syntax Completion (before any HIR/MIR work) ⚠️ MANDATORY
**Goal:** Every surface-language construct is defined, parsed, and type-checked *before*
HIR lowering begins. Adding syntax after HIR exists means patching the lowering retroactively.

**Constructs that MUST be decided and implemented here:**

- [ ] **Dict literal syntax** — `{k: v}` is ambiguous with blocks; a deliberate syntax
      must be chosen (e.g. `dict { k: v }`, `#{ k: v }`, `{ k => v }`, etc.) and implemented
      end-to-end (lexer → parser → AST → type checker).
- [ ] **`match` / pattern statement** — decide the keyword alias (e.g. `match`, `check`,
      `when`), decide the arm syntax, implement fully. The `Stmt::When` AST node and
      `When` token already exist; only the parser dispatch is missing.
- [ ] **Constructor syntax** — `new TypeName(args)` vs `initialize` convention vs another
      form. Must be decided, implementing `new` keyword fully in parser + typeck.
- [ ] **`export use` re-exports** — `export use std.io.print` makes an imported name
      part of the current module's public API. Implement in the module-resolution pass
      (before HIR); no backend changes required.
- [ ] **`->` as `oftype` alias** — `Arrow` is already a separate token. In the parser,
      every position that consumes `Oftype` must also accept `Arrow`. This covers
      variable declarations (`var x -> integer`), action params (`required n -> integer`),
      nested composite types (`list -> list -> integer`), and return annotations.
      Do NOT emit `Oftype` from the lexer for `->` — keep `Arrow` distinct so it can
      later serve as a closure/lambda arrow (`(x) -> x + 1`) without ambiguity.
- [ ] **Any other v1 surface syntax** agreed upon before this phase closes.

> **Rule:** No PR that touches `fidan-hir`, `fidan-mir`, or any codegen crate is merged
> until this phase is marked complete in PROGRESS.md.

### Phase 4 – Diagnostics (1–2 weeks)
**Goal:** Error messages that make users say "wow".

- [ ] Full `Diagnostic` / `Label` / `Suggestion` types
- [ ] `ariadne` rendering integration
- [ ] `FixEngine` with rules for all E1xx, E2xx, E3xx codes
- [ ] Edit-distance suggestions for undefined names
- [ ] Test every error code produces a beautiful message

### Phase 5 – HIR + MIR + Interpreter + `concurrent` (3–4 weeks)
**Goal:** `fidan run test/examples/test.fdn` works end-to-end. `concurrent` blocks work cooperatively.

- [ ] HIR types and AST→HIR lowering
- [ ] MIR types (BasicBlock, Phi, SSA locals)
- [ ] HIR→MIR lowering with Braun SSA construction
- [ ] Exception handling lowering (landing pads)
- [ ] `concurrent` block lowering → `SpawnConcurrent` + `JoinAll` MIR instructions
- [ ] MIR text dump (`--emit mir`)
- [ ] MIR interpreter with call stack, frame locals, and owned-value drop tracking
- [ ] `fidan-runtime`: `FidanValue`, `FidanObject`, `FidanClass`; owned values via `OwnedRef<T>` (`Rc<RefCell<T>>` interpreter-internally, single-threaded only)
- [ ] Green-thread scheduler (`corosensei`) for `concurrent` tasks
- [ ] Builtin functions: `print`, `input`, `len`, `toString`, etc.
- [ ] Run `test/examples/test.fdn` fully and verify output

### Phase 5.5 – `parallel` Execution + Rayon (2–3 weeks)
**Goal:** `parallel` blocks, `parallel action`, `parallel for`, `Shared oftype T`, `spawn`/`await` work.

- [ ] Rayon thread pool integration in `fidan-runtime`
- [ ] Type checker enforces thread-crossing rule: owned values may only **move** into a `parallel` block (transferred, not shared); values to be shared across tasks MUST be declared `Shared oftype T` at the call site — no implicit promotion to `Arc`
- [ ] `SpawnParallel` + `JoinAll` MIR instructions → Rayon `join` / `spawn`
- [ ] `ParallelIter` MIR instruction → Rayon `par_iter`
- [ ] `SpawnExpr` + `AwaitPending` → `Pending oftype T` type backed by `JoinHandle<FidanValue>` (Rust)
- [ ] `Shared oftype T` runtime type (backed by `Arc<Mutex<FidanValue>>` in Rust)
- [ ] `std.parallel` module: `parallelMap`, `parallelFor`, `Shared`, `Pending`
- [ ] Parallel capture safety (`parallel_check.rs`) producing E4xx errors
- [ ] Benchmark: `parallel for` vs sequential `for` on a compute-heavy workload

### Phase 6 – Optimization Passes (1 week)
**Goal:** MIR is faster after passes.

- [ ] `ConstantFolding`, `DeadCodeElimination`, `CopyPropagation`, `UnreachablePruning`
- [ ] Benchmark: run `scripts/performance_bm.sh` equivalent in Fidan, measure improvement

### Phase 7 – Standard Library Core (2–3 weeks)
**Goal:** `std.io`, `std.string`, `std.math`, `std.collections`, `std.test` implemented.

- [ ] Module import system (`use std.io`)
- [ ] All listed stdlib modules (Rust implementation, Fidan-callable via FFI)
- [ ] `fidan test` command works, runs test blocks

### Phase 8 – Cranelift AOT (correctness baseline) (2–3 weeks)
**Goal:** `fidan build test.fdn -o test` produces a correct working binary using Cranelift.
This is a **transitional phase** — it validates the full AOT pipeline (compilation, linking,
stack root tracking, unwind) before LLVM is introduced. Cranelift AOT is NOT the final release backend.

- [ ] Cranelift `ObjectModule` setup
- [ ] MIR → Cranelift IR translation for all instruction types
- [ ] Runtime library (`fidan-runtime`) compiled as static library
- [ ] System linker invocation (`cc` on Unix, `link.exe`/`lld` on Windows)
- [ ] Stack root tracking in compiled code (for AOT exception unwind maps and sanitizers — NOT for a tracing collector; Fidan has no tracing collector)
- [ ] DWARF unwind info for exception handling (Linux/macOS), SEH (Windows)
- [ ] **Test: compiled binary output must exactly match interpreter output** (this is the
  correctness contract that the LLVM backend must also satisfy in Phase 11)

### Phase 9 – Cranelift JIT / `@precompile` (2 weeks)
**Goal:** `@precompile` decorator accelerates functions in interpreter mode. This is
Cranelift's **permanent, final role** in the architecture — it will never be replaced here.

**JIT strategy (decided and recorded — see Key Technical Decision #9):**
- JIT is **lazy by default**: a per-function call counter in `MirMachine` triggers Cranelift
  compilation when a function is called `JIT_THRESHOLD` times (default 500).
- `@precompile` is the **eager escape hatch**: it pre-sets the counter to `JIT_THRESHOLD`,
  so the function is compiled on its very first call.
- `--jit-threshold N` flag makes the threshold tunable for benchmarking.
- Compiled native code replaces the `MirMachine` dispatch entry for that `FunctionId`;
  subsequent calls bypass the interpreter entirely.

- [ ] Cranelift `JITModule` setup
- [ ] JIT compilation on first call to `@precompile` function (eager path)
- [ ] Per-function call counter in `MirMachine`; compile at threshold (lazy path)
- [ ] ABI trampoline between interpreter stack and native calling convention
- [ ] `--jit-threshold N` CLI flag in `fidan-driver`
- [ ] Benchmark: `@precompile` annotated tight loop vs. without vs. full AOT

### Phase 10 – CLI Polish & LSP (2–3 weeks)
**Goal:** Usable development experience.

- [ ] All `fidan` subcommands working
- [ ] REPL migrated to MIR pipeline (incremental MIR append model; retire AST walker)
- [ ] REPL with history and multi-line input
- [ ] LSP server: diagnostics, hover, go-to-def, completion
- [ ] VS Code extension skeleton (JSON grammar + LSP client)
- [ ] Formatter (`fidan fmt`)

### Phase 11 – LLVM AOT Backend + Performance (4–6 weeks)
**Goal:** `fidan build --release` produces C++-competitive native binaries.
This phase replaces Cranelift as the AOT backend with LLVM and adds all performance features.

- [ ] Add `fidan-codegen-llvm` crate with `inkwell` dependency
- [ ] MIR → LLVM IR translation for all instruction types
- [ ] LLVM optimization pass pipeline (`-O2` then `-O3`)
- [ ] Auto-vectorization enabled (`-loop-vectorize`, `-slp-vectorize`)
- [ ] LTO support (`fidan build --release --lto`)
- [ ] Monomorphization collector in `fidan-typeck`: track all concrete generic instantiations
- [ ] Specialized LLVM function emission per concrete instantiation
- [ ] Escape analysis MIR pass: stack-allocate non-escaping objects
- [ ] `fidan build --release` links against LLVM AOT object; `fidan run` still uses Cranelift JIT
- [ ] Benchmark suite: compare Cranelift AOT vs LLVM AOT vs equivalent C++ on compute benchmarks
- [ ] PGO instrumentation mode: `fidan build --instrument` → `fidan build --use-profile`
- [ ] All correctness tests from Phase 8 pass with the LLVM backend

---

## 21. Pitfalls & Pre-planned Mitigations

| Pitfall | Mitigation |
|---|---|
| **Arena lifetime hell in Rust** | Use index-based references (`ExprId(u32)`) instead of raw `&'ast` references if lifetime inference becomes too complex. The arena is still used for storage; lookups go through an index. |
| **`is not` expression parsing** | Token-pair normalization in the parser (documented above). Test exhaustively: `a is not b`, `a is not nothing`, `not a is b`. |
| **Default param evaluation (Python trap)** | Default values are stored as `Expr` in the AST and re-evaluated at each call site during interpretation. Never evaluated once at definition time. |
| **Recursive object references (ownership cycles)** | Use `WeakShared oftype T` for back-references in object graphs that contain `Shared` values. Compiler emits a warning when a statically-detectable ownership cycle is found in `Shared` types. Owned (non-Shared) values structurally cannot form cycles because single ownership forms a DAG by definition. |
| **JIT ABI mismatch between interpreter values and compiled code** | Define a clear `FidanABI` spec. All JIT functions receive and return tagged `FidanValue` structs. Trampolines handle boxing/unboxing. Tests verify ABI correctness for every type. |
| **`this` in free-function call of extension actions** | Clearly specified: `this === person` (the extension parameter) in free-function context. Implemented as a single consistent rule in MIR lowering. |
| **Exception unwind crossing compiled frames** | In AOT mode, use Dwarf unwinding (like Rust's panics). In interpreter mode, use an explicit unwind loop. In mixed mode (interpreter calling compiled), the ABI trampoline must also be a landing pad candidate. This is complex; handle it in Phase 9, not Phase 5. |
| **`parallel` + `OwnedRef`: `Rc` is not `Send`** | `OwnedRef<T>` (interpreter-internal `Rc<RefCell<T>>`) is single-threaded by design and stays that way — it is NEVER upgraded to `Arc`. The type checker prevents owned values from crossing thread boundaries: passing an owned value into a `parallel` block is a **move** (compiler enforces one owner, the task — `Rc` is valid because only one thread touches it). Shared mutation across threads requires `Shared oftype T` which is always `Arc<Mutex<T>>`. There is no whole-runtime `Rc`→`Arc` upgrade. |
| **Rayon threadpool panic propagation** | If a parallel task panics, Rayon propagates the panic to the joining thread. MIR lowering ensures that `JoinAll` maps to Rayon's `join` result check. A panic in a parallel task is caught at the `JoinAll` boundary and re-raised as a Fidan `throw` in the calling scope. |
| **`parallel for` with mutable accumulator (classic race)** | `parallel_check.rs` catches this at compile time (E401). The idiomatic fix (`parallelMap`, `parallelFilter`) is always suggested. Document this prominently in the language guide. |
| **`spawn` without `await` (dropped `Pending oftype T`)** | `Pending oftype T` is marked `#[must_use]` in Rust. Dropping without `await` or `.wait()` produces a W301 warning. The runtime does NOT silently discard the result; it joins the thread on drop (blocking), with a warning. |
| **`Shared oftype T` deadlock** | `Shared oftype T` uses a non-recursive `Mutex` (Rust). Calling `.withLock()` inside another `.withLock()` on the same value from the same thread is a runtime panic with a clear message: "deadlock: attempted re-entrant lock on Shared". Detected via `try_lock` + thread ID check. |
| **`concurrent` tasks and owned values** | All `concurrent` tasks run cooperatively on a single OS thread. Owned values can be freely passed between coroutines because there is no true parallelism — only one coroutine runs at a time. No `Arc`, no mutex needed for `concurrent`-only code. `parallel` is the only form that requires `Shared oftype T` for shared state. |
| **`parallel` task capturing a mutable variable from enclosing scope** | Caught at compile-time by `parallel_check.rs` (E401). Immutable captures are passed by clone. `Shared oftype T` is the only pathway for shared mutation. |
| **String interpolation with complex nested expressions** | Recursively call the full expression parser. Limit nesting depth (`MAX_INTERP_DEPTH = 16`) to prevent pathological cases. Report a clean error if exceeded. |
| **`dynamic` type in AOT mode** | All `dynamic`-typed values are lowered to a 2-word tagged union in memory. Dispatch is handled by a runtime helper. This works but is slower; warn users that `dynamic` opts out of AOT type optimizations. |
| **Bootstrapping: stdlib written in Fidan calling Fidan** | Keep the stdlib in Rust until the compiler is self-hosting. Define a clear FFI surface (`@extern(rust)` decorator) that Fidan code can call into Rust. Bootstrap incrementally. |
| **Symbol `set` ambiguity** | `set` is always `Assign` from the lexer. `var x set 10` parses as `VarDecl { init: Assign(10) }`. A future collection type named `Set` uses `Set` (capitalized) as a type name, never as a keyword. Lowercase `set` is permanently reserved as `Assign`. |

---

*This document is the ground truth for the Fidan implementation. It should be updated as
decisions change. All architectural changes should be reflected here before code is written.*

---

## 22. Differentiating Features Roadmap

> These features are not scheduled for immediate implementation. They are recorded here as
> first-class architectural commitments — each one is designed to exploit Fidan's existing
> foundations (deterministic execution, structured diagnostics, controlled memory model, span
> tracking) in ways that no mainstream language can easily replicate.

---

### 22.1 – Time-Travel Debugging (`--trace-time`)

**Elevator pitch:** Step *backwards* through any execution, inspect every variable at every
past moment — built into the runtime, not a plugin.

**Why Fidan can do this when others cannot:**

| Language | Status |
|---|---|
| Python | Not possible (GIL, mutable everything) |
| C++ | Essentially impossible (raw pointers, UB) |
| Rust | Extremely hard (borrow checker fights you) |
| Dart | Limited snapshots only |
| **Fidan** | **Deterministic execution + controlled memory model + structured diagnostics** |

**What it records:**
- State diff per step: only changed variables (memory-efficient delta log)
- Call stack snapshots at each frame push/pop
- Works in both interpreter mode and precompiled (`@precompile`) mode

**CLI surface:**
```
fidan run app.fdn --trace-time
# produces: app.fdn.trace (binary diff log)

fidan replay app.fdn.trace
# interactive: step forward/backward, inspect state
```

**Implementation notes (for when this is scheduled):**
- New crate: `fidan-timetrace` — `TraceRecorder` writes diff log; `TracePlayer` reads it
- `fidan-interp` gets an optional `RecordHook` injected at frame push, variable write
- `@precompile` JIT functions emit trace callbacks via Cranelift `call_indirect` to a C ABI hook
- Trace format: length-prefixed binary (variable ID, old value, new value, step counter)
- Target: MVP records variables + call stack; advanced version records heap object mutations

---

### 22.2 – Built-in Code Explanation (`fidan explain --line N`)

**Elevator pitch:** Ask Fidan what any line does — fully offline, fully deterministic, zero AI.

```
fidan explain app.fdn --line 42
```

**Output per line:**
```
line 42: total = total + i

  what it does    assigns the sum of `total` and `i` to `total`
  values flowing  total: integer (currently 0), i: integer (loop variable)
  depends on      `total` (line 38), `i` (for-loop induction variable, line 39)
  mutates         `total`
  could go wrong  integer overflow if total + i exceeds integer range
```

**Why no mainstream language has this:**
- Requires span tracking from source to runtime value (Fidan has this)
- Requires def-use chains (available once MIR/SSA is in place)
- Requires type inference results to be queryable per-span (typeck already stores this)

**Implementation notes:**
- New sub-command in `fidan-cli`: `run_explain_line`
- Uses MIR def-use chains (`fidan-mir`) + typeck inference results
- Phase: schedulable after Phase 5 (MIR) + Phase 3 (typeck)
- The existing `fidan explain <CODE>` (diagnostic code explanations) is a separate feature;
  this is *source-line* explanation, a different code path

---

### 22.3 – Deterministic Replayable Bugs (`--replay`)

**Elevator pitch:** Every runtime error carries a replay ID. Run the exact same failure again,
anytime, on any machine.

```
runtime error[R2001]: division by zero
  → app.fdn:14:17

replay id: 7f3a-19b2

reproduce with:
  fidan run app.fdn --replay 7f3a-19b2
```

**Why this is possible:**
- Fidan's execution is deterministic by default (no hidden entropy, no OS scheduling in the
  interpreter path)
- A replay ID encodes the PRNG seed + any external inputs that were captured at panic time
- Re-running with `--replay` re-injects the same inputs and seeds → identical execution

**What gets captured:**
- PRNG seed (if language-level random is used)
- Captured stdin / file reads at panic time (stored in replay bundle)
- Clock values (frozen to capture time)

**Implementation notes:**
- New type in `fidan-runtime`: `ReplayBundle { id: ReplayId, seed: u64, inputs: Vec<CapturedInput> }`
- Panic handler in `fidan-interp` serialises bundle to `~/.fidan/replays/<id>.bundle`
- `--replay <id>` deserialises bundle and injects a `ReplayDriver` that overrides all I/O
- Replay IDs are 8-hex-char (4 bytes) for readability; collisions handled by appending a counter

---

### 22.4 – Language-Level Profiling (`fidan profile`)

**Elevator pitch:** Zero annotations, zero flags, zero tooling setup. Just run `fidan profile`.

```
fidan profile app.fdn
```

**Output:**
```
profile: app.fdn  (1 234 ms total)

  hot paths
    action compute_score  called 10 000×  avg 0.12 ms  total 1 200 ms  91.3%
    action parse_token    called 84 000×  avg 0.001 ms total   84 ms   6.4%

  allocation points
    line 88  list literal   10 000 allocs  avg 24 B
    line 109 string interp   84 000 allocs  avg 12 B

  suggestion
    action compute_score is >80% of runtime
    consider annotating with @precompile
```

**Why Fidan already has everything needed:**
- Frame tracking → call counts are free (already implemented for stack traces)
- Span tracking → pinpoint allocations to source line
- MIR instruction count → cost model for `@precompile` suggestions

**Implementation notes:**
- New `fidan-profiler` crate (or module inside `fidan-interp`)
- `ProfileSink` trait: `on_call(action, span)`, `on_alloc(span, bytes)`, `on_return(action, duration)`
- Injected into `fidan-interp` via the same `RecordHook` mechanism as time-travel debugging
- `fidan profile` = `fidan run` with `ProfileSink` enabled + report rendered at exit
- Output format: human-readable terminal table (default) + `--profile-out app.fdn.prof` (JSON)

---

### 22.5 – Compile-Time "Why Is This Slow?" (`W5xxx` hints)

**Elevator pitch:** Not just *what* is slow — *why*, with a concrete fix suggestion. Emitted by
the compiler, not a profiler.

**Example diagnostic output:**
```
warning[W5001]: loop body cannot be precompiled
  → app.fdn:34:5
   |
34 |     for item in data {
   |     ^^^
   |
   = reason: loop variable `item` has type `flexible` (dynamic dispatch required)
   = reason: closure on line 37 captures mutable `total` (prevents hoisting)

  suggestion: annotate enclosing action with @precompile
  suggestion: replace `flexible` with a concrete type (e.g. `integer`) if inputs allow
```

**New diagnostic codes:**

| Code | Meaning |
|---|---|
| `W5001` | Loop not precompilable — dynamic type in induction variable |
| `W5002` | Loop not precompilable — closure captures mutable outer variable |
| `W5003` | Action called in hot path but lacks `@precompile` |
| `W5004` | `@precompile` has no effect in AOT build mode (supersedes proposed W3001) |

**Implementation notes:**
- New pass in `fidan-passes`: `precompile_analysis.rs`
- Runs after constant-folding; inspects MIR for dynamic-dispatch instructions in loop bodies
- Emits `W5xxx` diagnostics via the existing `fidan-diagnostics` machinery
- Phase: schedulable after Phase 9 (Cranelift JIT) when `@precompile` semantics are stable

---

### 22.6 – Zero-Config Sandboxing (`--sandbox`)

**Elevator pitch:** Safe-by-default script execution. No setup, no seccomp boilerplate, no OS
expertise required.

```
fidan run script.fdn --sandbox
```

**Default sandbox policy:**

| Resource | Default | Override flag |
|---|---|---|
| File system | denied | `--allow-read=./data` |
| Network | denied | `--allow-net=api.example.com` |
| Environment variables | denied | `--allow-env` |
| Subprocess spawn | denied | `--allow-spawn` |
| CPU / wall time | 30 s | `--time-limit=N` |
| Memory | 256 MB | `--mem-limit=N` |

**Why this is ergonomically ahead of alternatives:**
- Python: `subprocess`, `os`, `socket` all open by default; sandboxing requires seccomp + effort
- C++: no concept of sandboxing
- Rust: safe memory but unrestricted I/O
- Deno does this for JS — Fidan would be the **first systems-adjacent language** with it built in

**Implementation notes:**
- New crate: `fidan-sandbox`
- All I/O in `fidan-stdlib` (file, net, env, spawn) routes through a `SandboxPolicy` trait
- `fidan-driver` constructs the policy from CLI flags and passes it into the session
- OS enforcement: `seccomp-bpf` on Linux, Job Objects on Windows (defence-in-depth over
  stdlib-only interception)
- Policy violations produce `E6001: operation not permitted in sandbox` with the resource named

---

### 22.7 – Strict / "No Foot-Guns" Mode (`--strict`)

**Elevator pitch:** A production-grade lint tier that promotes every dangerous pattern from
warning to hard error.

```
fidan build app.fdn --strict
```

**What `--strict` escalates from warning to error:**

| Check | Normal code | `--strict` |
|---|---|---|
| Unused variables | `W1001` | **error** |
| Implicit `nothing` flows into typed variable | `W2001` | **error** |
| Unchecked error from action that can throw | `W2003` | **error** |
| `dynamic` / `flexible` type in hot path | `W5001` | **error** |
| Action with no return type annotation | `W2004` | **error** |
| `@precompile` in AOT build (no-op) | `W5004` | **error** |

**Implementation notes:**
- New `--strict` flag in `fidan-driver/src/options.rs`: `strict_mode: bool`
- `fidan-typeck` checks `session.options.strict_mode`; if true, promotes listed W codes to E
- `fidan-diagnostics` utility: `Diagnostic::escalate()` upgrades severity in-place
- Phase: schedulable at any point after Phase 3 (typeck); no MIR dependency
- Composable with `--sandbox`: `fidan build app.fdn --strict --sandbox` is valid

---

---

### 22.8 – Hot Reloading (`--reload`)

**Elevator pitch:** Save a source file; your running program re-executes instantly — no
restart, no manual refresh. Works for *any* imported file in the dependency graph, not just
the entry point.

```
fidan run app.fdn --reload
```

**What it does:**
- Starts a file-system watcher (via the `notify` crate) on the entry-point file and
  every file that was transitively `use`-imported.
- On any change event (write, rename, or create in the watched set), the watcher signals
  the driver, which re-runs the full pipeline from lexing through execution.
- The previous run is cleanly terminated before the new one starts.
- A compact diff of what changed (`+N lines`, `−M lines`, or `module X reloaded`) is
  printed to stderr before re-execution.

**Why Fidan can do this cleanly:**
- The entire pipeline is stateless and re-entrant — `SourceMap`, `TypeChecker`, `MirProgram`
  are all newly constructed per run. There is no mutable global state to corrupt.
- Because the MIR interpreter owns all runtime state in one `MirMachine` struct, re-running
  from scratch on change is a clean and correct model without snapshot-and-patch complexity.

**Implementation notes:**
- New `--reload` flag on `fidan run` in `fidan-driver/src/options.rs`.
- `fidan-driver` grows a `watch_and_rerun(opts)` function that wraps `run_pipeline(opts)` in
  a `notify` watcher loop.
- The watcher set is populated after the first parse pass by walking `Item::Use` imports and
  resolving them to `PathBuf`s via the source map.
- Requires the module import system (Phase 7); before Phase 7, only the single entry-point
  file is watched.
- On Windows: uses `ReadDirectoryChangesW` via `notify`. On Linux: `inotify`. On macOS: FSEvents.
- `Ctrl+C` exits the reload loop cleanly.

**Future enhancement (incremental reload):**  
Once the MIR pipeline is incremental (salsa-style demand-driven recompilation), only the
changed function bodies need to be re-lowered and re-optimised, not the whole program.
The MIR for unchanged functions is reused from the previous run. This is a stretch goal;
clean full-restart is the v1 semantics.

**Dependency:** Phase 7 for multi-file watching; Phase 5 MIR interpreter for execution.

---

### Feature → Phase Dependency Map

| Feature | Earliest schedulable after | Estimated effort |
|---|---|---|
| 22.7 Strict mode | Phase 3 (typeck) | 1–2 days |
| 22.8 Hot reloading (single file) | Phase 5 (MIR interpreter) | 2–3 days |
| 22.5 Compile-time slow hints | Phase 9 (Cranelift JIT) | 1 week |
| 22.4 Language profiling | Phase 5 (MIR interpreter) | 1–2 weeks |
| 22.3 Replayable bugs | Phase 5 (MIR interpreter) | 1–2 weeks |
| 22.8 Hot reloading (multi-file) | Phase 7 (import system) | + 1–2 days on top of single-file |
| 22.6 Sandboxing | Phase 7 (stdlib) | 2–3 weeks |
| 22.2 Explain line | Phase 5 (MIR + typeck) | 2–3 weeks |
| 22.1 Time-travel debug | Phase 9 (JIT tracing hooks) | 3–5 weeks |
