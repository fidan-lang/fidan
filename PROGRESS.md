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
| Parallel MIR instructions | ✅ | `SpawnConcurrent`, `SpawnParallel`, `JoinAll`, `SpawnExpr`, `ParallelIter`, `SpawnDynamic` — all fully wired |
| HIR → MIR lowering (Braun SSA) | ✅ | `lower_program()`: scope-based renaming, φ-nodes at if/else joins, for/while loop headers back-patched; all stmt/expr variants covered |
| Exception landing pads | ✅ | `lower_attempt()` — try/catch/otherwise/finally basic-block structure |
| `concurrent` / `parallel` → `SpawnConcurrent`/`SpawnParallel` + `JoinAll` | ✅ | Each task body lifted to a synthetic `MirFunction`; real OS threads via `std::thread::scope` (Phase 5.5) |
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
| MIR walker / eval loop (`mir_interp.rs`) | ✅ | Full SSA-form MIR interpreter: `MirMachine` runs `call_function` / `run_function`; φ-node resolution; all `Rvalue` variants; try/catch landing pads; method dispatch (`Callee::Method`/`Callee::Fn`/`Callee::Builtin`); object construction (`Rvalue::Construct`) + class table from `MirObjectInfo` |
| Call stack + `CallFrame` | ✅ | `env.rs` frame stack; `frame.rs` Signal enum |
| Built-in functions (`print`, `input`, `len`, etc.) | ✅ | `builtins.rs` — true language builtins only: `print`, `eprint`, `input`, `type`, `len`, type coercions (`string`/`integer`/`float`/`boolean`), math free-functions (`abs`, `sqrt`, `floor`, `ceil`, `round`, `max`, `min`). Bootstrap receiver methods live in `bootstrap/` (split by type: `string_methods.rs`, `list_methods.rs`, `dict_methods.rs`, `numeric_methods.rs`) — placeholder until Phase 7 stdlib. |
| AST-walking interpreter (Phase 5 bootstrap) | ✅ | `interp.rs` — full eval_expr / exec_stmt; `fidan run test.fdn` works end-to-end |
| Object construction + `initialize` dispatch | ✅ | `construct_object` + inherited fields via `make_fidan_class` |
| Extension actions as methods + free functions | ✅ | `ext_actions: HashMap<class, HashMap<name, FuncDef>>` |
| `parent.method()` dispatch | ✅ | Dispatches to parent class in class hierarchy |
| String interpolation | ✅ | `InterpPart::Literal` + `InterpPart::Expr` evaluated inline |
| `attempt / catch / otherwise / finally` | ✅ | `finally` always runs even on re-panic |
| `check` statement + `check` expression | ✅ | Wildcard `_` + value matching |
| Binary / unary operators (all variants) | ✅ | Arithmetic, bitwise, comparison, logical (short-circuit) |
| `for` / `while` / `parallel for` loops | ✅ | Break / Continue signals propagate correctly |
| `concurrent { task{} }` / `parallel { task{} }` (real threads) | ✅ | Task bodies lifted to synthetic `MirFunction`s; `std::thread::scope` + `JoinAll` — real OS parallelism |
| `spawn` / `await` (real async) | ✅ | `SpawnExpr` → `std::thread::spawn` returning `JoinHandle`; `AwaitPending` joins the handle; `SpawnDynamic` handles `spawn obj.method(args)` and `spawn fnVar(args)` |
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
| OS thread integration in `fidan-runtime` + Rayon | ✅ | `std::thread::scope` for `parallel`/`concurrent` tasks; Rayon `par_iter` for `ParallelIter` |
| Thread-crossing type rule enforcement | ⬜ | `parallel_check.rs` data-race analysis — compile-time E4xx — not yet built |
| `SpawnParallel` + `JoinAll` → real OS threads | ✅ | `std::thread::scope` with scoped handles collected into `JoinAll` |
| `ParallelIter` → `par_iter` | ✅ | Rayon `par_iter()` + `for_each` in `mir_interp.rs`; captures passed as `ParallelCapture` vec |
| `SpawnExpr` + `AwaitPending` | ✅ | `spawn expr` → `std::thread::spawn` → `FidanValue::Pending(JoinHandle)`; `await` calls `.join()` |
| `Shared oftype T` runtime type | ✅ | `Arc<Mutex<FidanValue>>` in `fidan-runtime`; `.get()` / `.update()` / `.withLock()` interpreter builtins |
| `SpawnDynamic` — spawn on method/dynamic calls | ✅ | New MIR instruction; `spawn obj.method(args)` → `dispatch_method` in thread; `spawn fnVar(args)` → dynamic `call_function` in thread |
| `std.parallel` module | ✅ | Phase 7 — `fidan-stdlib/src/parallel.rs`; `NeedsCallbackDispatch` protocol; serial in MIR interp, true parallel in Phase 8/9 |
| `parallel_check.rs` E4xx errors | ⬜ | Compile-time data-race detection; needs dedicated MIR analysis pass |
| W3xx: unawaited `Pending` dropped | ⬜ | Runtime warning when `Pending oftype T` is dropped without `await` |
| Parallel task failure display | ⬜ | `runtime error[R9001]: N tasks failed in 'parallel' block` + per-task failure list |
| Parallel benchmark | ⬜ | `scripts/performance_bm.sh` equivalent — `parallel for` vs sequential on compute-heavy workload |

---

## Hot Reloading (`--reload`) — Future Feature 22.8

| Item | Status | Notes |
|---|---|---|
| `--reload` flag on `fidan run` | ⬜ | `fidan-driver/src/options.rs` |
| `notify` crate file-system watcher integration | ⬜ | Cross-platform: `inotify`/FSEvents/ReadDirectoryChangesW |
| Single-file watch (entry point only) | ⬜ | Schedulable now (Phase 5 complete) |
| Re-run on change, diff printed to stderr | ⬜ | |
| Multi-file watch (transitive `use` imports) | ⬜ | Requires Phase 7 (import system) |
| Incremental MIR reuse on reload | ⬜ | Requires salsa-style demand-driven recompilation — stretch goal |

---



| Item | Status | Notes |
|---|---|---|
| `ConstantFolding` | ✅ | Folds integer/float/boolean/string binary and unary constant expressions; emitted as `Rvalue::Literal` |
| `DeadCodeElimination` | ✅ | Removes unused locals (written but never read after analysis over all BBs) |
| `CopyPropagation` | ✅ | Replaces `_a = _b; … use _a` with direct use of `_b` across a function |
| `UnreachablePruning` | ✅ | Removes basic blocks with `Terminator::Unreachable` and dead successors |
| `run_all()` pass manager | ✅ | `fidan-passes/src/lib.rs` — runs all 4 passes in order on a `MirProgram` |
| CLI wiring | ✅ | `fidan run` pipeline: HIR → MIR → `run_all(&mut mir)` → `run_mir(mir)` |
| Benchmark before/after | ⬜ | |

---

## Phase 7 – Standard Library Core

| Item | Status | Notes |
|---|---|---|
| Module import system (`use std.io`) | ✅ | HIR `HirUseDecl` → MIR `MirUseDecl` / `MirLit::Namespace` → `FidanValue::Namespace`; typeck registers namespace imports as `FidanType::Dynamic` |
| `std.io` | ✅ | `fidan-stdlib/src/io.rs` — print, println, eprint, readLine, readFile, writeFile, appendFile, deleteFile, fileExists, listDir, getEnv, setEnv, args, cwd, etc. |
| `std.string` | ✅ | `fidan-stdlib/src/string.rs` — toUpper, toLower, trim, split, join, replace, startsWith, endsWith, contains, len, repeat, pad, etc. |
| `std.math` | ✅ | `fidan-stdlib/src/math.rs` — sqrt, abs, floor, ceil, round, pow, log, log2, log10, sin/cos/tan, min, max, clamp, PI, E, etc. |
| `std.collections` | ✅ | `fidan-stdlib/src/collections.rs` — Queue, Stack, Set, OrderedDict; enqueue/dequeue, push/pop, setAdd/setRemove/setContains, etc. |
| `std.test` | ✅ | `fidan-stdlib/src/test_runner.rs` — assertEqual, assertNotEqual, assertTrue, assertFalse, assertSome, assertNone, assertContains, assertGt/Lt/Ge/Le, fail; returns `__test_fail__:` sentinel on failure |
| `std.parallel` | ✅ | `fidan-stdlib/src/parallel.rs` — parallelMap, parallelFilter, parallelForEach, parallelReduce; `NeedsCallbackDispatch` protocol wired in `MirMachine::exec_parallel_op` (serial in MIR phase; true parallelism in Phase 8/9) |
| `fidan test` command | ✅ | `ExecutionMode::Test` in `fidan-cli`; runs full pipeline, reports pass/fail with coloured output and exit code |

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
| JIT compilation on first `@precompile` call (eager path) | ⬜ | Counter pre-set to threshold; compiles on first call |
| Per-function call counter in `MirMachine` (lazy path) | ⬜ | `u32` counter per `FunctionId`; compile at `JIT_THRESHOLD` (default 500) |
| ABI trampoline (interpreter ↔ native calling convention) | ⬜ | |
| `--jit-threshold N` CLI flag | ⬜ | Tunable for benchmarking |
| Hot-path auto-compilation (counter ≥ threshold → Cranelift) | ⬜ | |
| Dispatch table replacement (replace `MirMachine` entry with native ptr) | ⬜ | |
| Precompiled frame debug map (MIR → source span) | ⬜ | Preserves source spans for `@precompile` frames in stack traces; shown as `[precompiled]` |
| Benchmark JIT vs. interpreter vs. AOT | ⬜ | |

> **Decided:** Lazy JIT with user-directed eager escape hatch (Key Technical Decision #9,
> recorded in ARCHITECTURE.md). `@precompile` = eager; call-counter threshold = lazy.
> `--jit-threshold N` tunes the threshold.

---

## Deferred: Bytecode Interpretation Tier

| Item | Status | Notes |
|---|---|---|
| Compact linear bytecode IR below MIR | ⬜ | Explicitly deferred; see Key Technical Decision #10 |
| Bytecode interpreter (`BytecodeMachine`) | ⬜ | Would replace `MirMachine` if scheduled |
| MIR → bytecode lowering pass | ⬜ | Would include phi-node elimination and operand flattening |
| Offset → source span mapping table | ⬜ | Needed to preserve stack-trace location info after lowering |

> **Decided:** Do NOT implement until profiling after Phase 9 shows MIR dispatch (not value
> boxing, not I/O) is >20% of runtime on a representative workload. The current bottleneck
> is `FidanValue` boxing and Rc/Arc refcounting, not interpreter dispatch speed.
> Bytecode would add a third IR to maintain without addressing the actual bottleneck.

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

## Bug Fix Log

| Date | Bug | Fix |
|---|---|---|
| 2026-03-03 | `:last --full` REPL command showed "(no errors recorded)" for lex/parse/typecheck errors | `error_history.push()` added after every `render_to_stderr` call in REPL eval loop (`fidan-cli/src/main.rs`) |
| 2026-03-03 | `var x oftype strig` silently accepted (unknown type resolved to `FidanType::Object(sym)`) | Added E0105 "undefined type name" with `suggest_name`; `registering` flag prevents double-fire; `map` alias added for `dict` |
| 2026-03-03 | E0105 had no `fidan explain E0105` entry | Added full prose + erroneous/corrected examples to `explanations.rs` |
| 2026-03-03 | False E0202 "return type mismatch" on unannotated actions | `check_action_body`: use `None` (unconstrained) when `return_ty` is absent instead of defaulting to `Nothing` |
| 2026-03-03 | `new` constructor body never executed at runtime | `sym_initialize` was interned as `"initialize"` but parser stores constructor name as `"new"` — fixed to intern `"new"` in both `Interpreter::new()` and `new_repl_state()` |
| 2026-03-03 | `return someValue` inside `new` constructor silently accepted | `check_action_body` now receives implicit `Nothing` return type for `new`-named actions inside object scope — `return value` now gives E0202 |

---

*Last updated: 2026-03-01 — Phase 6 optimization passes complete (constant folding, dead code elimination, copy propagation, unreachable pruning — all 4 in `fidan-passes`); MIR interpreter complete (`fidan-interp/src/mir_interp.rs`); `fidan run` now executes the full MIR pipeline (HIR → MIR → optimization passes → MIR interpreter); `test/examples/test.fdn` produces all 7 expected output lines correctly. Key MIR lowering fixes: per-class `method_ids` map prevents function-ID collision for same-named methods across classes; `this` parameter added as implicit param 0 for all object methods; `HirExprKind::This`/`Parent` lower to the `this` local register; `parent.method(args)` lowers to a direct `Callee::Fn(parent_fn_id)` call (not virtual dispatch); parameter stubs (`_N = nothing`) removed from function bodies — the frame pre-initialises all locals + call ABI fills params before bb0 runs. Previous: Phase 5 HIR/MIR complete.*
*Last updated: 2026-03-02 — All 21 sections of `test/examples/comprehensive.fdn` now pass correctly. Key fixes this session: (1) `continue` in for-loops caused phi-node `nothing` values — fixed by introducing a dedicated `step_bb` for index increment and tracking all continue-site env snapshots to build proper phi nodes in `step_bb`; (2) typed `catch err -> string` clauses were all lowered as `FidanType::Dynamic` — fixed by reading `CatchClause.ty` in HIR lower via `resolve_type_expr_simple`; (3) parent class fields not accessible in child objects — fixed by including inherited parent fields when building `FidanClass::fields` in `build_class_table`; (4) `lower_attempt` (try/catch/finally) did not create phi nodes for variables modified in catch/otherwise branches — fixed by tracking `(end_bb, env_snapshot)` for each normal-exit path and building phi nodes at `finally_bb`. All existing test files (`test/*.fdn`) continue to pass without regressions.*
*Last updated: 2026-03-01 — Phase 5.5 parallel execution complete. `parallel for` dispatches to Rayon `par_iter` (real multi-core); `parallel { task{} }` and `concurrent { task{} }` each lift task bodies to synthetic `MirFunction`s via the `PendingParallelFor` deferred-body mechanism and spawn real OS threads via `std::thread::scope` + `JoinAll`; `spawn expr` / `await handle` backed by `std::thread::spawn` returning `FidanValue::Pending(JoinHandle)` — `AwaitPending` calls `.join()`; new `SpawnDynamic` MIR instruction handles `spawn obj.method(args)` (dynamic method dispatch in spawned thread) and `spawn fnVar(args)` (dynamic fn-value call in spawned thread); `Shared oftype T` backed by `Arc<Mutex<FidanValue>>` with `.get()` / `.update()` / `.withLock()` interpreter builtins; `SpawnDynamic` propagated to both optimization passes (`dead_code.rs`, `copy_propagation.rs`) and `mir_interp.rs`; new test file `test/examples/spawn_method_test.fdn` validates method-spawn (results 307, 342) and concurrent-block real parallelism; all existing test files pass without regressions. Remaining Phase 5.5 work: `parallel_check.rs` compile-time E4xx data-race detection, W3xx unawaited-Pending warning, parallel task failure display, benchmark.*