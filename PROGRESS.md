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
| `parallel_check.rs` data race detection (E4xx) | ✅ | Phase 5.5 — `fidan-passes/src/parallel_check.rs`; E0401 shared-write in parallel tasks; wired into CLI pipeline |
| `Shared oftype T` recognised as thread-safe | ✅ | `FidanType::Shared` variant recognised in `resolve_type_expr` |
| `Pending oftype T` from `spawn expr` | ✅ | `FidanType::Pending` inferred from `Expr::Spawn` |
| W3xx: unawaited `Pending` dropped | ✅ | `fidan-passes/src/unawaited_pending.rs`; W1004 emitted when Pending var is never awaited |
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
| `parallel_check.rs` E4xx errors | ✅ | `fidan-passes/src/parallel_check.rs`; E0401 shared-write detection; wired into CLI pipeline |
| W3xx: unawaited `Pending` dropped | ✅ | `fidan-passes/src/unawaited_pending.rs`; W1004 emitted for never-awaited `Pending` vars |
| Parallel task failure display | ✅ | `error[R9001]`; `FidanPending::try_join()`; `MirSignal::ParallelFail`; `JoinAll` collects all task failures; `RunError.code` field routes to correct diagnostic |
| Parallel benchmark | ✅ | `test/examples/parallel_benchmark.fdn` — sequential vs `parallel { task }` vs `spawn/await`; `scripts/performance_bm.bat` for Windows |

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
| `ConstantFolding` | ✅ | Folds integer/float/boolean/string binary and unary constant expressions; emitted as `Rvalue::Literal`. Also performs **strength reduction**: 14 algebraic identities (`x+0→x`, `x*1→x`, `x*0→0`, `x**0→1`, `x**1→x`, `x&&true→x`, `x\|\|true→true`, `+x→x`, etc.) — all in `try_reduce()` in `constant_folding.rs` |
| `Inlining` | ✅ | `fidan-passes/src/inlining.rs` — `is_inlinable`: 1 basic block, ≤15 instructions, `Return` terminator, no recursion/spawn; `do_inline`: deep-copies callee body with remapped locals + adjusted `LocalId` offsets; call sites processed in descending index order to keep indices stable |
| `DeadCodeElimination` | ✅ | Removes unused locals (written but never read after analysis over all BBs) |
| `CopyPropagation` | ✅ | Replaces `_a = _b; … use _a` with direct use of `_b` across a function |
| `UnreachablePruning` | ✅ | Removes basic blocks with `Terminator::Unreachable` and dead successors |
| `run_all()` pass manager | ✅ | `fidan-passes/src/lib.rs` — 6-pass pipeline: ConstantFolding → Inlining → ConstantFolding (2nd) → CopyPropagation → DeadCodeElimination → UnreachablePruning |
| CLI wiring | ✅ | `fidan run` pipeline: HIR → MIR → `run_all(&mut mir)` → `run_mir(mir)` |
| Benchmark before/after | ⬜ | |

---

## Phase 7 – Standard Library Core

| Item | Status | Notes |
|---|---|---|
| Module import system (`use std.io`) | ✅ | HIR `HirUseDecl` → MIR `MirUseDecl` / `MirLit::Namespace` → `FidanValue::Namespace`; typeck registers namespace imports as `FidanType::Dynamic` |
| User file imports (Python-style relative) | ✅ | `find_relative(base_dir, segments)` — resolves `use mymod` → `{dir}/mymod.fdn` or `{dir}/mymod/init.fdn`; `use mymod.utils` → `{dir}/mymod/utils.fdn` or `{dir}/mymod/utils/init.fdn`. No magic folder name required. Explicit path strings (`use "./other.fdn"`) still accepted. Transitive imports + cycle detection via `HashSet<PathBuf>`. `pre_register_hir_into_tc` prevents false "undefined" errors. **Known limit:** `use std.io` creates `io` as an SSA local in the init function (not a MIR global), so `io.print(…)` is inaccessible from named action bodies — use the global `print()` builtin instead. |
| `export use` re-export | ✅ | `export use std.math` in `fileB.fdn` makes the `math` namespace accessible in any file that does `use fileB`, without that file needing its own `use std.math`. `re_export: bool` threaded through `Item::Use` → `HirUseDecl` → `MirUseDecl`; `TypeChecker::pre_register_namespace()` exposes only `re_export=true` entries to the importing file; `merge_module` keeps all use_decls for runtime correctness. |
| `std.io` | ✅ | `fidan-stdlib/src/io.rs` — print, println, eprint, readLine, readFile, writeFile, appendFile, deleteFile, fileExists, listDir, getEnv, setEnv, args, cwd, etc. |
| `std.string` | ✅ | `fidan-stdlib/src/string.rs` — toUpper, toLower, trim, split, join, replace, startsWith, endsWith, contains, len, repeat, pad, etc. |
| `std.math` | ✅ | `fidan-stdlib/src/math.rs` — sqrt, abs, floor, ceil, round, pow, log, log2, log10, sin/cos/tan, min, max, clamp, PI, E, etc. |
| `std.collections` | ✅ | `fidan-stdlib/src/collections.rs` — Queue, Stack, Set, OrderedDict; enqueue/dequeue, push/pop, setAdd/setRemove/setContains, etc. |
| `std.test` | ✅ | `fidan-stdlib/src/test_runner.rs` — assertEqual, assertNotEqual, assertTrue, assertFalse, assertSome, assertNone, assertContains, assertGt/Lt/Ge/Le, fail; returns `__test_fail__:` sentinel on failure |
| `std.parallel` | ✅ | `fidan-stdlib/src/parallel.rs` — parallelMap, parallelFilter, parallelForEach, parallelReduce; `NeedsCallbackDispatch` protocol wired in `MirMachine::exec_parallel_op` (serial in MIR phase; true parallelism in Phase 8/9) |
| `fidan test` command | ✅ | `ExecutionMode::Test` in `fidan-cli`; runs full pipeline, reports pass/fail with coloured output and exit code |

---

## Phase 8 – Cranelift JIT / `@precompile`

> **Note:** Cranelift is used exclusively for JIT compilation in Fidan.
> Static AOT compilation (object files, system linker) is handled by LLVM in Phase 11.

| Item | Status | Notes |
|---|---|---|
| MIR → Cranelift IR (all instructions) | ⬜ | Shared foundation for JIT emission |
| Cranelift `JITModule` setup | ⬜ | |
| JIT compilation on first `@precompile` call (eager path) | ⬜ | Counter pre-set to threshold; compiles on first call |
| Per-function call counter in `MirMachine` (lazy path) | ⬜ | `u32` counter per `FunctionId`; compile at `JIT_THRESHOLD` (default 500) |
| ABI trampoline (interpreter ↔ native calling convention) | ⬜ | |
| `--jit-threshold N` CLI flag | ⬜ | Tunable for benchmarking |
| Hot-path auto-compilation (counter ≥ threshold → Cranelift) | ⬜ | |
| Dispatch table replacement (replace `MirMachine` entry with native ptr) | ⬜ | |
| Precompiled frame debug map (MIR → source span) | ⬜ | Preserves source spans for `@precompile` frames in stack traces; shown as `[precompiled]` |
| Benchmark JIT vs. interpreter | ⬜ | |

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

> **Note:** All static AOT compilation lives here (LLVM `ObjectModule`, system linker, `.a` file).
> Cranelift handles JIT only (Phase 8).

| Item | Status | Notes |
|---|---|---|
| `fidan-codegen-llvm` crate (`inkwell`) | ⬜ | |
| MIR → LLVM IR (all instructions) | ⬜ | |
| `fidan-runtime` as static `.a` | ⬜ | Linked into every AOT binary |
| System linker invocation | ⬜ | `cc` / `lld` depending on platform |
| Stack root tracking (unwind maps) | ⬜ | |
| DWARF / SEH unwind info | ⬜ | |
| Binary output matches interpreter output | ⬜ | Golden-file correctness suite |
| LLVM `-O2` / `-O3` pass pipeline | ⬜ | |
| Auto-vectorisation | ⬜ | |
| LTO | ⬜ | |
| Monomorphisation collector | ⬜ | |
| Specialised function emission | ⬜ | |
| Escape analysis MIR pass | ⬜ | |
| PGO instrumentation mode | ⬜ | |
| All Phase 8 JIT correctness tests pass under AOT | ⬜ | |
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
| 2026-03-04 | Module-level `const var`/`var` not accessible inside function bodies | Added MIR-level globals infrastructure: `GlobalId`, `MirGlobal`, `Instr::LoadGlobal`, `Instr::StoreGlobal`; pre-pass scans `init_stmts` for top-level `VarDecl`s; init fn writes each global via `StoreGlobal` without adding to SSA env (so reads always use `LoadGlobal` for freshness); named functions read globals via `LoadGlobal`; unshadowed-global writes emit `StoreGlobal`; `MirMachine` gains `Arc<Mutex<Vec<FidanValue>>>` globals table shared across parallel threads |
| 2026-03-05 | `use std.io` / `use std.math` namespace aliases only accessible from init scope | Pre-pass ④ in `lower.rs` now also registers each `use std.X` alias as a `MirGlobal`; init fn emits `StoreGlobal` instead of `define_var`; all named action bodies read via `LoadGlobal` — `io.print()` and `math.sqrt()` now work everywhere |
| 2026-03-05 | Concurrent/parallel task synthetic functions named `"new"` (E0401 showed `task 'new'`) | `ConcurrentBlock` lowering changed from `self.new_sym` to `task.name.unwrap_or(self.new_sym)` — task labels now shown correctly in diagnostics |
| 2026-03-05 | `use nonexistent` silently succeeded with no error | Added E0106 "module not found" to `codes.rs` + `explanations.rs`; `collect_file_import_paths()` now returns `(VecDeque, Vec<(String, Span)>)` — unresolved imports carry spans; both call sites in `fidan-cli/src/main.rs` emit span-annotated `error[E0106]` |
| 2026-03-05 | `use test2` (user module) → E0101 undefined name on `test2.fn()` | Four-layer fix: typechecker registers first path segment as `Dynamic` binding; HIR lowerer emits `HirUseDecl` for non-stdlib paths; MIR pre-pass ④ registers user namespace as `MirGlobal`; init fn emits `Namespace("test2")` + `StoreGlobal`; interpreter `dispatch_method` routes user-module calls through `user_fn_map` (built at startup from non-init, non-method functions) |
| 2026-03-05 | `print(io)` / `print(math)` printed `[value]` instead of `<module:io>` / `<module:math>` | `format_val()` in `fidan-stdlib/src/io.rs` and `fidan-stdlib/src/test_runner.rs` was missing arms for `Namespace`, `Function`, `Pending`, `Tuple`, `Shared` — all fell through to `_ => "[value]"`. Added full match arms consistent with `builtins::display()`: `Namespace(m) => "<module:{m}>"`, `Function(id) => "<action#{id}>"`, recursive `List`/`Dict`/`Tuple`/`Shared` display, `Pending => "<pending>"` |
| 2026-03-05 | `print(readFile)` → `nothing` — specific-name stdlib imports had no first-class value | Added `FidanValue::StdlibFn(Arc<str>, Arc<str>)` + `MirLit::StdlibFn{module,name}`. MIR pre-pass ④ now registers each specific-name import as a `MirGlobal`; init fn emits `StdlibFn` literal + `StoreGlobal`. `Callee::Dynamic` handles `StdlibFn` via `dispatch_stdlib_call`. `print(readFile)` → `<action:io.readFile>`, callable via `var f = readFile; f("path")` |
| 2026-03-05 | `format_val` triplicated across `io.rs`, `test_runner.rs`, `builtins.rs` | Added `pub fn display()` to `fidan-runtime/src/value.rs` — single canonical source of truth. `fidan-runtime/src/lib.rs` re-exports it. `builtins::display` delegates to `fidan_runtime::display`. `io.rs` and `test_runner.rs` use `use fidan_runtime::display as format_val` — no call-site changes needed |
| 2026-03-05 | `dispatch_method` scanned `program.use_decls` on every `Namespace` method call (O(n)) | Added `stdlib_modules: HashSet<Arc<str>>` to `MirMachine`, built at startup from all module names and aliases. `is_stdlib` check is now O(1) |

---

*Last updated: 2026-03-01 — Phase 6 optimization passes complete (constant folding, dead code elimination, copy propagation, unreachable pruning — all 4 in `fidan-passes`); MIR interpreter complete (`fidan-interp/src/mir_interp.rs`); `fidan run` now executes the full MIR pipeline (HIR → MIR → optimization passes → MIR interpreter); `test/examples/test.fdn` produces all 7 expected output lines correctly. Key MIR lowering fixes: per-class `method_ids` map prevents function-ID collision for same-named methods across classes; `this` parameter added as implicit param 0 for all object methods; `HirExprKind::This`/`Parent` lower to the `this` local register; `parent.method(args)` lowers to a direct `Callee::Fn(parent_fn_id)` call (not virtual dispatch); parameter stubs (`_N = nothing`) removed from function bodies — the frame pre-initialises all locals + call ABI fills params before bb0 runs. Previous: Phase 5 HIR/MIR complete.*
*Last updated: 2026-03-05 — Code quality + `StdlibFn` session. (1) `FidanValue::StdlibFn(module, name)`: specific-name stdlib imports (`use std.io.{readFile}`) now produce first-class callable values — `print(readFile)` → `<action:io.readFile>`, callable as `var f = readFile; f("path")`; MIR pre-pass ④ registers each specific name as a `MirGlobal`; init fn emits `MirLit::StdlibFn`; `Callee::Dynamic` handles `StdlibFn` via `dispatch_stdlib_call`. (2) Display DRY fix: `pub fn display()` added to `fidan-runtime/src/value.rs` as the single source of truth; `builtins::display` now delegates; `io.rs` and `test_runner.rs` import it as `format_val` — zero code duplication. (3) O(n) method dispatch fix: `MirMachine` gains `stdlib_modules: HashSet<Arc<str>>` built at startup; `dispatch_method`'s `is_stdlib` check is now O(1). All 12 tests pass. Previous: 2026-03-05 — Three import/display fixes (E0106, user module imports, `[value]` display bug).*
*Last updated: 2026-03-06 — Phase 5.5 fully complete. E0401 parallel data-race detection (`parallel_check.rs`), W1004 unawaited-Pending warning (`unawaited_pending.rs`), R9001 parallel task failure display (`FidanPending::try_join` + `MirSignal::ParallelFail` + `RunError.code` field), and parallel benchmark (`test/examples/parallel_benchmark.fdn`, `scripts/performance_bm.bat`) are all done. Phase 5.5 parallel execution complete. `parallel for` dispatches to Rayon `par_iter` (real multi-core); `parallel { task{} }` and `concurrent { task{} }` each lift task bodies to synthetic `MirFunction`s via the `PendingParallelFor` deferred-body mechanism and spawn real OS threads via `std::thread::scope` + `JoinAll`; `spawn expr` / `await handle` backed by `std::thread::spawn` returning `FidanValue::Pending(JoinHandle)` — `AwaitPending` calls `.join()`; new `SpawnDynamic` MIR instruction handles `spawn obj.method(args)` (dynamic method dispatch in spawned thread) and `spawn fnVar(args)` (dynamic fn-value call in spawned thread); `Shared oftype T` backed by `Arc<Mutex<FidanValue>>` with `.get()` / `.update()` / `.withLock()` interpreter builtins; `SpawnDynamic` propagated to both optimization passes (`dead_code.rs`, `copy_propagation.rs`) and `mir_interp.rs`; new test file `test/examples/spawn_method_test.fdn` validates method-spawn (results 307, 342) and concurrent-block real parallelism; all existing test files pass without regressions. Remaining Phase 5.5 work: `parallel_check.rs` compile-time E4xx data-race detection, W3xx unawaited-Pending warning, parallel task failure display, benchmark.*