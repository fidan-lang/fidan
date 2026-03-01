# Fidan – Implementation Progress

> This file tracks what has been built, what is in progress, and what remains.
> It is the live diff between ARCHITECTURE.md (the plan) and reality.
> **Do not edit ARCHITECTURE.md to reflect progress — edit this file.**

---

## Legend

- ✅ Done and tested
- 🔨 In progress
- ⬜ Not started
- 🚫 Blocked (reason noted)

---

## Workspace

| Item | Status | Notes |
|---|---|---|
| `Cargo.toml` workspace root | ✅ | 17 crates, shared deps, release profile |
| All crate stubs created | ✅ | All 17 crates have `Cargo.toml` + stub `lib.rs`/`main.rs` |
| `cargo build` clean on all crates | ✅ | 1 inconsequential dead_code warning in `fidan-diagnostics` |

---

## Phase 1 – Workspace + Lexer

> Goal: `fidan-lexer` tokenises `test/examples/test.fdn` correctly. All tokens round-trip.

### `fidan-source`
| Item | Status | Notes |
|---|---|---|
| `FileId`, `SourceFile`, `SourceMap` | ✅ | Thread-safe RwLock-backed SourceMap |
| `Span` (byte-offset range + FileId) | ✅ | Half-open `[start, end)` byte range |
| `Location` (line + col, computed on demand) | ✅ | Lazily computed from `line_starts` |
| `SourceFile::line_col(offset)` | ✅ | Binary search over `line_starts` |

### `fidan-lexer`
| Item | Status | Notes |
|---|---|---|
| `TokenKind` enum (all variants) | ✅ | All literals, keywords, operators, delimiters |
| `Token { kind, span }` | ✅ | |
| `Lexer` struct + `next_token()` | ✅ | |
| Whitespace / newline handling | ✅ | `skip_non_newline_whitespace` correctly preserves `\n` |
| `Newline` token emission (Go-style rules) | ✅ | `terminates_statement()` drives insertion |
| Integer literals | ✅ | Underscore separators stripped |
| Float literals | ✅ | |
| String literals (raw + interpolated markers) | ✅ | Escape sequences supported |
| Boolean literals (`true`/`false`) | ✅ | |
| `nothing` literal | ✅ | |
| Single-line comments (`#`) | ✅ | |
| Nested multi-line comments (`#/ ... /#`) | ✅ | Arbitrary nesting depth |
| All operators and delimiters | ✅ | |
| `SynonymMap` (phf perfect hash) | ✅ | `synonyms.rs` |
| Keyword → canonical `TokenKind` mapping | ✅ | |
| `;` / `sep` → `Semicolon` | ✅ | |
| `&&` / `||` punct-level lexing | ✅ | Phase 3.5 — handled in `lex_punct` before synonyms |
| Hex (`0x…`) and binary (`0b…`) number literals | ✅ | Phase 3.5 |
| `SymbolInterner` (DashMap, Symbol = u32) | ✅ | Thread-safe, lock-free fast path |
| Identifier interning | ✅ | |
| Error recovery (Unknown token) | ✅ | `TokenKind::Unknown(char)` |
| Lexer test: tokenise `test/examples/test.fdn` | ⬜ | Phase 2 |
| Lexer test: round-trip all token types | ✅ | 12 unit tests, all passing |
| `--emit tokens` output in CLI | ✅ | `fidan run --emit tokens file.fdn` works |

---

## Phase 2 – Parser

> Goal: Parse `test/examples/test.fdn` to a full AST. Pretty-print round-trips.

### `fidan-ast`
| Item | Status | Notes |
|---|---|---|
| Arena allocator (`typed_arena`) | ✅ | Vec-backed pools: ExprId/StmtId/ItemId |
| `ExprId`, `StmtId`, `ItemId` index types | ✅ | |
| All expression AST nodes | ✅ | Full `Expr` enum + `Ternary`, `List`, `Dict`, `Error` variants (Phase 2); `Expr::Check` (Phase 3.5) |
| All statement AST nodes | ✅ | Full `Stmt` enum + `Panic`, `Error` (Phase 2); `Stmt::Check` / `CheckArm` (Phase 3.5) |
| All item AST nodes (`object`, `action`, etc.) | ✅ | `Item` enum + `VarDecl`, `ExprStmt` (Phase 2); `Item::Use` gains `re_export: bool` (Phase 3.5) |
| `Module` root node | ✅ | |
| AST visitor trait | ✅ | Default no-op `AstVisitor` |
| `Expr::span()` helper | ✅ | Returns span for any expression variant |
| `TypeExpr::span()` / `span_end()` helpers | ✅ | |
| AST pretty-printer | ✅ | Phase 5 — `print.rs` full tree-walk printer; `--emit ast` shows all nodes |

### `fidan-parser`
| Item | Status | Notes |
|---|---|---|
| Recursive-descent top-level parser | ✅ | `parse_module()` + `parse_top_level()` |
| `object` declaration parsing | ✅ | With `extends`, fields, nested methods |
| `action` declaration parsing | ✅ | With `with (params)`, `returns type`, body |
| Extension action (`action X extends Y`) | ✅ | |
| `parallel action` modifier | ✅ | |
| `var` / `set` statement parsing | ✅ | With optional `oftype` and default value |
| `if` / `otherwise when` / `otherwise` / `else` parsing | ✅ | Full else-if chain |
| `for item in collection` parsing | ✅ | |
| `while` parsing | ✅ | |
| `attempt / catch / otherwise / finally` parsing | ✅ | |
| `return` / `break` / `continue` parsing | ✅ | |
| `panic(expr)` statement | ✅ | |
| Assignment statement (`lhs = rhs` / `lhs set rhs`) | ✅ | |
| Expression statement | ✅ | |
| Pratt expression parser (full precedence table) | ✅ | All 10 precedence levels |
| `??` (null-coalesce) operator | ✅ | |
| Ternary `value if condition else fallback` | ✅ | |
| Implicit-subject ternary `x if is not nothing else y` | ✅ | Fidan-specific shorthand |
| `is not` → `NotEq` normalization | ✅ | Two-token lookahead in Pratt loop |
| Named argument call parsing | ✅ | `name set value` and `name = value` |
| `required` / `optional` parameter modifiers | ✅ | |
| Mixed grouped/ungrouped param lists | ✅ | `(p1) also p2` style |
| `default` / `=` param defaults | ✅ | Contextual keyword |
| `with` param keyword | ✅ | Contextual keyword (interned at parse-init) |
| `returns` return-type keyword | ✅ | Contextual keyword |
| `else` keyword in ternary | ✅ | Contextual keyword |
| String interpolation AST node | ✅ | `{expr}` split into `InterpPart` fragments |
| List literal `[...]` | ✅ | |
| `spawn` / `await` expressions | ✅ | |
| `concurrent { task ... }` / `parallel { task ... }` blocks | ✅ | |
| `parallel for` parsing | ✅ | |
| `decorator` (`@name`) parsing | ✅ | |
| `use` import parsing | ✅ | |
| Grouped import `use std.io.{print, foo}` | ✅ | Phase 3.5 — emits one `Item::Use` per name |
| `export use` re-export syntax | ✅ | Phase 3.5 — `Item::Use { re_export: true }` |
| `->` as type-annotation introducer | ✅ | Phase 3.5 — `eat_type_ann()` accepts `Oftype` or `Arrow` |
| `Pending` type in `parse_type_expr` | ✅ | Phase 3.5 |
| Tuple/pair type `(K, V)` in `parse_type_expr` | ✅ | Phase 3.5 — used by `dict oftype (string, integer)` |
| `..` range operator in Pratt table | ✅ | Phase 3.5 — `DotDot` infix BP; `...` inclusive range normalised to `DotDot` |
| `**` power operator + `BinOp::Pow` | ✅ | Phase 3.5 — `StarStar` infix BP; `Caret` → `BinOp::BitXor` |
| Unary `+` (no-op) | ✅ | Phase 3.5 |
| `Shared` / `Pending` as call-target expressions | ✅ | Phase 3.5 — keywords treated as `Expr::Ident` in `parse_primary` |
| Dict literal `{ k: v, … }` | ✅ | Phase 3.5 — `LBrace` in expression context |
| `check` statement | ✅ | Phase 3.5 — `parse_check_stmt()`; arms with `pattern => { body }` or `pattern => expr` |
| `check` expression (inline) | ✅ | Phase 3.5 — `Expr::Check` in `parse_primary` |
| `new` constructor block in object body | ✅ | Phase 3.5 — `new with (params) { body }` inside `object` |
| `catch err -> Type` arrow annotation | ✅ | Phase 3.5 — catch clause uses `eat_type_ann()` |
| Decorator–declaration newline fix | ✅ | Phase 3.5 — `skip_terminators()` after `parse_decorators()` |
| `else if` chain (both `else if` and `otherwise when`) | ✅ | Phase 3.5 — `Otherwise + If` handled identically to `Otherwise + When` |
| Error recovery (synchronisation set) | ✅ | `synchronize()` in recovery.rs |
| `Expr::Error` / `Stmt::Error` placeholder nodes | ✅ | |
| Parse errors rendered via ariadne | ✅ | `render_to_stderr` called in CLI |
| Parse `test/examples/test.fdn` without errors | ✅ | 6 items, 94 exprs, 25 stmts — zero diagnostics |
| Parse `test/syntax.fdn` without parser errors | ✅ | Phase 3.5+ — 132 items, 672 exprs, 150 stmts; zero `[P000]` and zero `[E1xx]`/`[E2xx]` diagnostics |
| `--emit ast` node-count summary | ✅ | |
| Round-trip test (parse → print → parse → compare) | ⬜ | Needs pretty-printer (Phase 2 stretch) |
| Parser unit tests | ⬜ | Phase 2 stretch |

---

## Phase 3 – Semantic Analysis

> Goal: Typecheck `test.fdn`; report all type errors on a buggy version.

### `fidan-typeck`
| Item | Status | Notes |
|---|---|---|
| Symbol table with scope stack | ✅ | `SymbolTable` + `Scope` stack in `scope.rs` |
| `object` registration + field/method resolution | ✅ | Two-pass: register then check; field + inheritance lookup |
| Inheritance chain (`extends`) resolution | ✅ | `resolve_field` + `method_return` walk parent chain |
| `var` type inference | ✅ | Inferred from init expr; `oftype` annotation respected |
| Expression type inference | ✅ | `infer_expr` covers all `Expr` variants |
| Type checking (assignments, returns, args) | ✅ | E201 assignment mismatch, E202 return mismatch |
| `this` and `parent` binding | ✅ | Injected into object + extension-action scopes |
| Extension action dual-registration | ✅ | Free function + method on target object |
| Named / positional argument checking | ✅ | E301 missing required param (constructor calls) |
| `required` / `optional` parameter checking | ✅ | Required params checked at call sites |
| Null safety flow analysis (warnings) | ⬜ | Deferred — needs control-flow graph (Phase 5) |
| Decorator validation (`@precompile`, etc.) | ⬜ | Deferred to Phase 4 |
| `parallel action` → `Pending oftype T` inference | ✅ | `Spawn` expr infers `Pending<T>`; `Await` unwraps it |
| `parallel_check.rs` data race detection (E4xx) | ⬜ | Phase 5.5 — needs MIR |
| `Shared oftype T` recognised as thread-safe | ✅ | `FidanType::Shared` variant recognised in `resolve_type_expr` |
| `Pending oftype T` from `spawn expr` | ✅ | `FidanType::Pending` inferred from `Expr::Spawn` |
| W3xx: unawaited `Pending` dropped | ⬜ | Phase 5.5 |
| `test.fdn` typechecks with zero diagnostics | ✅ | `fidan run test.fdn` → zero type errors |

---

## Phase 3.5 – Parser Completeness Pass

> Goal: `cargo run -- run test/syntax.fdn --emit ast` produces **zero parse errors** (`[P000]`).  
> All Fidan surface syntax covered; unimplemented constructs documented with `# [FUTURE]` markers.

| Item | Status | Notes |
|---|---|---|
| `&&` / `||` punct-level lexing | ✅ | Handled in `lex_punct`; synonyms.rs cleaned |
| `0x…` / `0b…` number literal prefixes | ✅ | Hex and binary integer literals |
| `&` bitwise AND lexing (`Ampersand` token) | ✅ | Was `Unknown('&')` |
| `\|` bitwise OR lexing (`Pipe` token) | ✅ | Was `Unknown('\|')` |
| `<<` shift-left (`LtLt` token) | ✅ | New two-char token |
| `>>` shift-right (`GtGt` token) | ✅ | New two-char token |
| Grouped import `use mod.{a, b, c}` | ✅ | Expands to one `Item::Use` per name |
| `export use` re-export | ✅ | `Item::Use { re_export: true }` |
| `->` type-annotation introducer | ✅ | `eat_type_ann()` accepts `Oftype` \| `Arrow` everywhere |
| `Pending` named type | ✅ | Parsed in `parse_type_expr` |
| Tuple/pair type `(K, V)` | ✅ | Dict key-value type syntax |
| `..` / `...` range operators | ✅ | `DotDot` added to Pratt infix table; `...` normalised to `DotDot` |
| `**` (`StarStar`) → `BinOp::Pow` | ✅ | Precedence 13 right-assoc |
| `^` (`Caret`) → `BinOp::BitXor` | ✅ | Was incorrectly mapped to `Pow` |
| `&` (`Ampersand`) → `BinOp::BitAnd` | ✅ | Phase 3.5+ |
| `\|` (`Pipe`) → `BinOp::BitOr` | ✅ | Phase 3.5+ |
| `<<` (`LtLt`) → `BinOp::Shl` | ✅ | Phase 3.5+ |
| `>>` (`GtGt`) → `BinOp::Shr` | ✅ | Phase 3.5+ |
| `BinOp::BitXor` in typeck | ✅ | Numeric result same as `Sub`/`Mul`/etc. |
| `BinOp::BitAnd` / `BitOr` / `Shl` / `Shr` in typeck | ✅ | Same numeric arm |
| `List<T>` / `Dict<K,V>` / `Shared<T>` / `Pending<T>` covariant widening | ✅ | `is_assignable_from` recurses into parameterized types |
| `_` wildcard in check-arm patterns | ✅ | `infer_expr` returns `Dynamic` for `_` instead of E101 |
| Keywords as field / method names after `.` | ✅ | `expect_field_name()` accepts any keyword token; `TokenKind::as_keyword_str()` |
| Unary `+` | ✅ | Parses operand, no semantic effect |
| `Shared(…)` / `Pending(…)` call expressions | ✅ | Keywords treated as `Expr::Ident` in `parse_primary` |
| Dict literal `{ k: v, … }` | ✅ | `LBrace` in expression context always a dict |
| `check` statement + `Stmt::Check` / `CheckArm` | ✅ | Full pattern-matching statement |
| `check` expression + `Expr::Check` | ✅ | Inline check used as a value |
| `Expr::Check` in typeck `infer_expr` | ✅ | Walks scrutinee + arms |
| `new` constructor block in `object` body | ✅ | `new with (…) { … }` sugar |
| `catch err -> Type` arrow annotation | ✅ | `eat_type_ann()` in `parse_attempt_stmt` |
| `else if` after `otherwise` block | ✅ | `Otherwise + If` → else-if branch |
| Decorator newline tolerance | ✅ | `skip_terminators()` after `parse_decorators()` |
| Error cascade suppression (`recovering` flag) | ✅ | One `[P000]` per desync; `synchronize()` resets the flag |
| `ariadne` ASCII fallback for non-TTY output | ✅ | `CharSet::Ascii` when stderr is a pipe |
| Windows UTF-8 console setup | ✅ | `SetConsoleOutputCP(65001)` in CLI `main()` |
| ASCII charset fallback for non-TTY | ✅ | Phase 3.5 — `CharSet::Ascii` when stderr is a pipe; prevents garbled box chars |
| `FixEngine` with E1xx, E2xx, E3xx rules | ✅ | `suggest_name` via Jaro-Winkler ≥ 0.75; Phase 4 |
| Edit-distance suggestions for undefined names | ✅ | `strsim` Jaro-Winkler; E101 emits `Note: did you mean '…'?` |
| All error codes produce rendered output | ✅ | ariadne spans + notes + help lines |
| `Suggestion` type with `SourceEdit` + `Confidence` | ✅ | `suggestion.rs` — `High/Medium/Low`, `SourceEdit`, `Suggestion::hint/fix` |
| `Diagnostic` notes + suggestions fields | ✅ | `with_note()`, `with_suggestion()`, `add_note()`, `add_suggestion()` |
| `cause_chain: Vec<Diagnostic>` rendering | ✅ | Causality chain rendered as indented sub-blocks via ariadne |
| Custom Fidan diagnostic visual identity | ✅ | `error[E0xxx]:` headers; spanless `✖/⚠/◆` badges; distinct from Rust format |
| Error code registry (`codes.rs`) | ✅ | `E0xxx`/`W1xxx`/`W2xxx`/`R2xxx`/`R3xxx` codes with category + title; `lookup()` / `title()` API |
| Context window in diagnostics | ✅ | 1 line before + error line + 1 line after; gutter with aligned line numbers |
| Inline underline label (`^^^^^^^ unknown name`) | ✅ | `Label::primary(span, message)` shown after carets on primary span |
| Fix-it patch block | ✅ | `Suggestion::fix(msg, span, replacement)` renders patched line + `++++` in green |
| Backtick identifiers in messages | ✅ | All error messages use backtick-quoted names (e.g. `\`greting\``) |
| `cause_chain: Vec<Diagnostic>` rendering | ✅ | Causes labelled `caused by (1/2):` and rendered indented one level deeper |
| Panic is not a separate code prefix | ✅ | `panic(expr)` is a runtime error like any other — rendered as `error[R####]`, no `P####` prefix |

---

## Phase 5 – HIR + MIR + Interpreter + `concurrent`

> Goal: `fidan run test/examples/test.fdn` works end-to-end.

### `fidan-hir`
| Item | Status | Notes |
|---|---|---|
| HIR types | ✅ | Complete type system: `HirModule`, `HirObject`, `HirFunction`, `HirStmt`, `HirExpr` + `HirExprKind` with all variants; every expr carries `FidanType` |
| AST → HIR lowering | ✅ | Full `lower_module()` — all statements/expressions; ternary desugared to `IfExpr`; param types resolved from `TypedModule` |

### `fidan-mir`
| Item | Status | Notes |
|---|---|---|
| `BasicBlock`, `Phi`, SSA locals | ✅ | `BlockId`, `LocalId`, `FunctionId`; `PhiNode`; full `MirFunction`/`MirProgram` |
| All `MirInstruction` variants | ✅ | `Assign`, `Call`, `SetField`, `GetField`, `GetIndex`, `SetIndex`, `Drop`, `AwaitPending`, `Nop` + placeholders for concurrency |
| Parallel MIR instructions | ✅ | `SpawnConcurrent`, `SpawnParallel`, `JoinAll`, `SpawnExpr`, `ParallelIter` (lowered but sequential in Phase 5) |
| HIR → MIR lowering (Braun SSA) | ✅ | `lower_program()`: scope-based renaming, φ-nodes at if/else joins, for/while loop headers back-patched; all stmt/expr variants covered |
| Exception landing pads | ✅ | `lower_attempt()` — try/catch/otherwise/finally basic-block structure |
| `concurrent` → `SpawnConcurrent` + `JoinAll` | ✅ | `ConcurrentBlock` lowered sequentially (Phase 5.5 will add real scheduling) |
| MIR text dump (`--emit mir`) | ✅ | `display::print_program` — function headers, BB labels, φ-nodes, instructions, terminators |

### `fidan-runtime`
| Item | Status | Notes |
|---|---|---|
| `FidanValue` enum | ✅ | Integer, Float, Boolean, Nothing, String, List, Dict, Object, Shared, Function |
| `OwnedRef<T>` (`Rc<RefCell<T>>`, interpreter-internal) | ✅ | `derive(Debug, Clone)`, COW helpers |
| `SharedRef<T>` (`Arc<Mutex<T>>`, for `Shared oftype T`) | ✅ | `derive(Debug, Clone)` |
| `FidanObject`, `FidanClass` | ✅ | Field lookup, inheritance chain via `parent: Option<Arc<FidanClass>>` |
| `FidanList` (COW) | ✅ | `Arc<Vec<T>>` + `Arc::make_mut` on mutation; `set_at()` added Phase 5 |
| `FidanDict` (COW) | ✅ | `Arc<HashMap<K,V>>` + `Arc::make_mut` on mutation; `iter()` added Phase 5 |
| `FidanString` (COW) | ✅ | `Arc<str>`, `append()` produces new Arc |
| Drop / owned-value lifetime tracking | ⬜ | Phase 5 — interpreter needed |

### `fidan-interp`
| Item | Status | Notes |
|---|---|---|
| MIR walker / eval loop | ⬜ | Phase 6 — when HIR/MIR lowering is done |
| Call stack + `CallFrame` | ✅ | `env.rs` frame stack; `frame.rs` Signal enum |
| Built-in functions (`print`, `input`, `len`, etc.) | ✅ | `builtins.rs` — print, input, len, string, integer, float, boolean, range, append |
| AST-walking interpreter (Phase 5 bootstrap) | ✅ | `interp.rs` — full eval_expr / exec_stmt; `fidan run test.fdn` works end-to-end |
| Object construction + `initialize` dispatch | ✅ | `construct_object` + inherited fields via `make_fidan_class` |
| Extension actions as methods + free functions | ✅ | `ext_actions: HashMap<class, HashMap<name, FuncDef>>` |
| `parent.method()` dispatch | ✅ | Dispatches to parent class in class hierarchy |
| String interpolation | ✅ | `InterpPart::Literal` + `InterpPart::Expr` evaluated inline |
| `attempt / catch / otherwise / finally` | ✅ | `finally` always runs even on re-panic |
| `check` statement + `check` expression | ✅ | Wildcard `_` + value matching |
| Binary / unary operators (all variants) | ✅ | Arithmetic, bitwise, comparison, logical (short-circuit) |
| `for` / `while` / `parallel for` loops | ✅ | Break / Continue signals propagate correctly |
| Concurrent block (sequential fallback) | ✅ | Phase 5.5 will add real green threads |
| `spawn` / `await` (sequential fallback) | ✅ | Evaluated synchronously; async in Phase 5.5 |
| Green-thread scheduler (`corosensei`) | ⬜ | |
| Exception unwind loop | ✅ | Signal::Panic caught by attempt/catch |
| `test/examples/test.fdn` runs, output verified | ✅ | 7 lines of output, exactly correct |
| Stack traces in diagnostics (runtime error + call stack) | ✅ | `Signal::Panic` carries `trace: Vec<String>`; `Env::stack_trace()` captures frame names at panic site |
| Evidence / context blocks in diagnostics | ⬜ | |
| `--trace short` / `--trace full` / `--trace compact` | ✅ | `TraceMode` in `CompileOptions`; `--trace` flag on `fidan run`; innermost-first with `#N  name` or compact `a -> b -> c` |
| `:type expr` in REPL | ✅ | Parses snippet, runs `infer_snippet_type`, prints `: TypeName` |
| `:last --full` in REPL | ✅ | Error history buffer in REPL loop; `:last` shows most recent, `:last --full` shows all |
| Error history buffer in REPL | ✅ | `error_history: Vec<String>` accumulates runtime error messages per session |

---

## Phase 5.5 – `parallel` Execution + Rayon

| Item | Status | Notes |
|---|---|---|
| Rayon thread pool in `fidan-runtime` | ⬜ | |
| Thread-crossing type rule enforcement | ⬜ | |
| `SpawnParallel` + `JoinAll` → Rayon | ⬜ | |
| `ParallelIter` → `par_iter` | ⬜ | |
| `SpawnExpr` + `AwaitPending` | ⬜ | |
| `Shared oftype T` runtime type | ⬜ | |
| `std.parallel` module | ⬜ | |
| `parallel_check.rs` E4xx errors | ⬜ | Phase 5.5 — needs MIR |
| Parallel task failure display | ⬜ | `runtime error[R9001]: N tasks failed in 'parallel' block` + per-task failure list |
| Parallel benchmark | ⬜ | |

---

## Phase 6 – Optimisation Passes

| Item | Status | Notes |
|---|---|---|
| `ConstantFolding` | ⬜ | |
| `DeadCodeElimination` | ⬜ | |
| `CopyPropagation` | ⬜ | |
| `UnreachablePruning` | ⬜ | |
| Benchmark before/after | ⬜ | |

---

## Phase 7 – Standard Library Core

| Item | Status | Notes |
|---|---|---|
| Module import system (`use std.io`) | ⬜ | |
| `std.io` | ⬜ | |
| `std.string` | ⬜ | |
| `std.math` | ⬜ | |
| `std.collections` | ⬜ | |
| `std.test` | ⬜ | |
| `fidan test` command | ⬜ | |

---

## Phase 8 – Cranelift AOT (correctness baseline)

| Item | Status | Notes |
|---|---|---|
| Cranelift `ObjectModule` setup | ⬜ | |
| MIR → Cranelift IR (all instructions) | ⬜ | |
| `fidan-runtime` as static `.a` | ⬜ | |
| System linker invocation | ⬜ | |
| Stack root tracking (unwind maps) | ⬜ | |
| DWARF / SEH unwind info | ⬜ | |
| Binary output matches interpreter output | ⬜ | |

---

## Phase 9 – Cranelift JIT / `@precompile`

| Item | Status | Notes |
|---|---|---|
| Cranelift `JITModule` setup | ⬜ | |
| JIT compilation on first `@precompile` call | ⬜ | |
| ABI trampoline | ⬜ | |
| Hot-path auto-detection (call counter) | ⬜ | |
| Benchmark JIT vs. interpreter | ⬜ | |
| Precompiled frame debug map (MIR → source span) | ⬜ | Preserves source spans for `@precompile` frames in stack traces; shown as `[precompiled]` |

---

## Phase 10 – CLI Polish & LSP

| Item | Status | Notes |
|---|---|---|
| All `fidan` subcommands | ✅ | `run`, `build`, `check`, `fix`, `explain`, `test`, `lsp` wired; backends are stubs where needed |
| `--emit tokens` | ✅ | Drives lexer, prints full token stream |
| `--emit ast` | ✅ | Phase 2 — node-count summary; phase 3.5 — works cleanly on `syntax.fdn` |
| `--emit hir/mir` | ✅ | `fidan run file.fdn --emit hir` and `--emit mir` both work |
| REPL with history + multi-line | 🔨 | Colon commands done; full eval working; multi-line block input still needs Phase 5 |
| stdin support (`fidan run -`) | ✅ | Reads from stdin when file is `-` |
| `fidan check` | ✅ | Parse + typecheck only; exits non-zero on any error; `--max-errors N` accepted |
| `fidan fix` | ✅ | Collects `Confidence::High` `SourceEdit` suggestions; applies to file or `--dry-run` prints old/new lines |
| `fidan explain <code>` | ✅ | Prints code, title, category from `codes.rs` registry |
| LSP server | ⬜ | |
| VS Code extension skeleton | ⬜ | |
| `fidan fmt` formatter | ⬜ | |

---

## Phase 11 – LLVM AOT + Performance

| Item | Status | Notes |
|---|---|---|
| `fidan-codegen-llvm` crate (`inkwell`) | ⬜ | |
| MIR → LLVM IR (all instructions) | ⬜ | |
| LLVM `-O2` / `-O3` pass pipeline | ⬜ | |
| Auto-vectorisation | ⬜ | |
| LTO | ⬜ | |
| Monomorphisation collector | ⬜ | |
| Specialised function emission | ⬜ | |
| Escape analysis MIR pass | ⬜ | |
| PGO instrumentation mode | ⬜ | |
| All Phase 8 correctness tests pass | ⬜ | |
| C++ benchmark comparison | ⬜ | |

---

## Test Coverage

| Suite | Status | Notes |
|---|---|---|
| Lexer unit tests | ✅ | 12/12 passing in `fidan-lexer` |
| Parser unit tests | ⬜ | |
| Typeck unit tests | ⬜ | |
| Interpreter integration (`test.fdn`) | ⬜ | |
| AOT integration (`test.fdn` binary) | ⬜ | |
| Error code rendering tests | ⬜ | |
| Parallel benchmark suite | ⬜ | |

---

## Known Issues / Blockers

_None._

---

*Last updated: 2026-02-28 — Phase 5 HIR/MIR complete: full `HirModule` type system; AST→HIR lowering (`lower_module`); `TypedModule` from typeck with per-expression type map; full MIR type system (SSA `BasicBlock`/`PhiNode`/`MirFunction`/`MirProgram`); HIR→MIR Braun-style SSA lowering (`lower_program`) with φ-nodes at if/else joins and back-patched loop headers; `display::print_program` for MIR text dump; `--emit hir` and `--emit mir` both wired into CLI and verified on `test/examples/test.fdn`. Previous: Phase 5 leftovers + Phase 10 CLI gaps: runtime call-stack capture; `--trace` flag; REPL commands; `fidan check/fix/explain`.*
