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
| `certain` / `optional` parameter modifiers | ✅ | |
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
| Round-trip test (parse → print → parse → compare) | ✅ | `fidan-fmt` round-trip test: `format_source()` → reparse → zero errors + same item count + idempotent |
| Parser unit tests | ✅ | 41/41 passing in `fidan-parser/src/lib.rs` — var/action/object/control-flow/concurrency/slicing/comprehension/test-block syntax + error recovery |

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
| `certain` / `optional` parameter checking | ✅ | Certain params checked at call sites |
| Null safety flow analysis (warnings) | ✅ | W2006 pass in `fidan-passes/src/null_safety.rs`; E0205 compile-time error in typeck (`check.rs`) when non-`certain` param / uninitialised var used as arithmetic/bitwise/range operand, `for` iterable, or index object; string concat (`+` with `String`) exempt — coerces to string at runtime |
| Decorator validation (`@precompile`, etc.) | ✅ | `check_decorators()` validates builtins (`precompile`, `deprecated`) and user action names (custom decorators §22.11); W2004 for anything else |
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
| Bit ops in tree-walk interpreter (`eval_binary`) | ✅ | `&` `\|` `^` `<<` `>>` on `Integer`; shift masked to `& 63` |
| Bit ops in MIR interpreter (`eval_binary`) | ✅ | Same; both paths verified with binary literals (`0b…`) |
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
| `FidanValue` enum | ✅ | Integer, Float, Boolean, Nothing, String, List, Dict, Object, Shared, Function, `EnumVariant { tag }`, `ClassType(Arc<str>)` (class-as-value), `StdlibFn(module, name)` |
| `OwnedRef<T>` (`Rc<RefCell<T>>`, interpreter-internal) | ✅ | `derive(Debug, Clone)`, COW helpers |
| `SharedRef<T>` (`Arc<Mutex<T>>`, for `Shared oftype T`) | ✅ | `derive(Debug, Clone)` |
| `FidanObject`, `FidanClass` | ✅ | Field lookup, inheritance chain via `parent: Option<Arc<FidanClass>>` |
| `FidanList` (COW) | ✅ | `Arc<Vec<T>>` + `Arc::make_mut` on mutation; `set_at()` added Phase 5 |
| `FidanDict` (COW) | ✅ | `Arc<HashMap<K,V>>` + `Arc::make_mut` on mutation; `iter()` added Phase 5 |
| `FidanString` (COW) | ✅ | `Arc<str>`, `append()` produces new Arc |
| Drop / owned-value lifetime tracking | ✅ | RAII via `Instr::Drop` → `exec_drop_dispatch()`; `FidanClass::has_drop_action` cached at class-table build; fires user-defined `drop` action when `Rc::strong_count == 2` (sole owner); deterministic, no GC |

### `fidan-interp`
| Item | Status | Notes |
|---|---|---|
| MIR walker / eval loop (`mir_interp.rs`) | ✅ | Full SSA-form MIR interpreter: `MirMachine` runs `call_function` / `run_function`; φ-node resolution; all `Rvalue` variants; try/catch landing pads; method dispatch (`Callee::Method`/`Callee::Fn`/`Callee::Builtin`); object construction (`Rvalue::Construct`) + class table from `MirObjectInfo` |
| Call stack + `CallFrame` | ✅ | `env.rs` frame stack; `frame.rs` Signal enum |
| Built-in functions (`print`, `input`, `len`, etc.) | ✅ | `builtins.rs` — true language builtins only: `print`, `eprint`, `input`, `type`, `len`, type coercions (`string`/`integer`/`float`/`boolean`), math free-functions (`abs`, `sqrt`, `floor`, `ceil`, `round`, `max`, `min`). Bootstrap receiver methods live in `bootstrap/` (split by type: `string_methods.rs`, `list_methods.rs`, `dict_methods.rs`, `numeric_methods.rs`) — placeholder until Phase 7 stdlib. |
| `List.forEach` / `List.firstWhere` callback methods | ✅ | `dispatch_list_callbacks()` private method on `MirMachine`; `for_each`/`first_where` snake_case aliases; must live in the interpreter layer — callbacks require `call_function()` access |
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
| Thread-crossing type rule enforcement | ✅ | `parallel_check.rs` data-race analysis — compile-time E4xx — `E0401` for shared writes in parallel tasks |
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

## Hot Reloading (`--reload`) — Feature 22.8 ✅

| Item | Status | Notes |
|---|---|---|
| `--reload` flag on `fidan run` | ✅ | `CompileOptions::reload: bool`; wired in `fidan-cli` |
| `notify` crate file-system watcher integration | ✅ | `recommended_watcher` via `notify` workspace dep; cross-platform (inotify / FSEvents / ReadDirectoryChangesW) |
| Single-file watch + sibling `.fdn` files | ✅ | `run_with_reload()` watches the entry-point directory (`NonRecursive`); reacts to any `.fdn` write/create/remove event |
| Re-run on change, diff printed to stderr | ✅ | Prints `[↻ reload] <filename> changed — re-running` before each re-run |
| Debounce (100 ms) | ✅ | Drains queued events; ignores events within 100 ms of the last one |
| Ctrl+C exits cleanly | ✅ | Channel close propagates from OS signal handler |
| Multi-file watch (transitive `use` imports) | ⬜ | Currently watches the whole directory; per-import tracking deferred to Phase 7+ |
| Incremental MIR reuse on reload | ⬜ | Requires salsa-style demand-driven recompilation — stretch goal |

---

## Explain Line (`fidan explain-line`) — Feature 22.2 ✅

Static analysis report for one or more source lines — fully offline, zero AI.
Pipeline: lex → parse → `typecheck_full()` → AST walk → render.

| Item | Status | Notes |
|---|---|---|
| `ExplainLine` subcommand in CLI | ✅ | `fidan explain-line <file> --line N [--end-line M]` |
| AST/item walker | ✅ | Walks `module.items` (ActionDecl, ExtensionAction, Stmt, ExprStmt, VarDecl); recurses into nested stmts (if/for/while/attempt/check/concurrent) |
| Span → line-number mapping | ✅ | `offset_line(src, byte_offset)` counts newlines; `span_overlaps()` checks overlap with target range |
| `what it does` field | ✅ | Plain-English description per `Stmt` variant and per `Expr` type |
| `type` field | ✅ | Drawn from `TypedModule.expr_types: FxHashMap<ExprId, FidanType>` |
| `reads` field | ✅ | Recursive `collect_reads()` over all `Expr::Ident` nodes reachable from the statement |
| `writes` field | ✅ | `collect_writes()` — VarDecl name, Assign target, For/ParallelFor binding, Destructure bindings |
| `could go wrong` field | ✅ | `binary_risks()` — Div/Rem → division by zero; Add/Sub/Mul/Pow → overflow; Index → out of bounds |
| Colour output in TTY | ✅ | ANSI colour codes suppressed when `NO_COLOR` is set or stdout is not a terminal |
| `depends on` field | ⬜ | Requires full def-use chains (SSA or use-def map) — deferred |

---

## Replayable Bugs (`--replay`) — Feature 22.3 ✅

Captures `input()` calls during a failing run and lets you reproduce the exact
stdin sequence with `fidan run <file> --replay <id>`.

| Item | Status | Notes |
|---|---|---|
| `stdin_capture` in `MirMachine` | ✅ | `pub stdin_capture: Vec<String>` — every line read from real stdin appended in call order |
| `replay_inputs` / `replay_pos` in `MirMachine` | ✅ | Pre-loaded list; `"input"` arm returns `replay_inputs[replay_pos++]` instead of blocking on stdin |
| `"input"` intercept in `dispatch_call` | ✅ | Inserted before `_ => {}` in `Callee::Builtin` match; replay takes priority; falls back to real stdin + capture |
| `set_replay_inputs()` / `get_stdin_capture()` | ✅ | Public setters / getters on `MirMachine` |
| `run_mir_with_replay()` public function | ✅ | Returns `(Result<(), RunError>, Vec<String>)`; replaces `run_mir_with_jit` in the interpret pipeline |
| `replay_inputs` in `CompileOptions` | ✅ | `Vec<String>`; default empty (= normal run) |
| `--replay <id|path>` on `fidan run` | ✅ | Accepts 8-hex ID or explicit bundle path; loaded by `load_replay_bundle()` before `run_pipeline` |
| Replay bundle save on error | ✅ | After `RunError`: if `stdin_capture` non-empty → `save_replay_bundle()` → prints `fidan run <file> --replay <id>` hint |
| Bundle format | ✅ | `~/.fidan/replays/<id>.bundle`; plain text; header `fidan-replay-v1`; one captured line per line |
| Replay ID | ✅ | 8 lowercase hex chars from `DefaultHasher(source_path + unix_timestamp_secs)` |
| Thread isolation | ✅ | `clone_for_thread()` inherits `replay_inputs` but starts a fresh `stdin_capture` and resets `replay_pos` |
| Replay in `--reload` mode | ⬜ | Hot-reload always uses a fresh `CompileOptions`; replay bundle not currently re-fed on each run |

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
| W5xxx slow-hints pass (`precompile_hints.rs`) | ✅ | `fidan-passes/src/precompile_hints.rs` — back-edge loop detection; W5001 (dynamic type in hot loop); W5003 (action called in hot loop without `@precompile`); W5002/W5004 stubs in codes + explanations; wired into CLI `emit_mir_safety_diags()`; W5001 escalated in `--strict` mode; 4/4 tests pass |
| Custom decorators §22.11 | ✅ | `DecoratorArg` / `CustomDecorator` types in `fidan-hir`; `extract_custom_decorators()` in HIR lowering; `MirFunction::custom_decorators: Vec<(FunctionId, Vec<MirLit>)>`; MIR post-pass resolves Symbol→FunctionId via `fn_map`; interpreter startup dispatch fires decorators before `FunctionId(0)` with `[fn_name, ...extra_args]` |

---

## Phase 7 – Standard Library Core

| Item | Status | Notes |
|---|---|---|
| Module import system (`use std.io`) | ✅ | HIR `HirUseDecl` → MIR `MirUseDecl` / `MirLit::Namespace` → `FidanValue::Namespace`; typeck registers namespace imports as `FidanType::Dynamic` |
| User file imports (Python-style relative) | ✅ | `find_relative(base_dir, segments)` — resolves `use mymod` → `{dir}/mymod.fdn` or `{dir}/mymod/init.fdn`; `use mymod.utils` → `{dir}/mymod/utils.fdn` or `{dir}/mymod/utils/init.fdn`. No magic folder name required. Explicit path strings (`use "./other.fdn"`) still accepted. Transitive imports + cycle detection via `HashSet<PathBuf>`. `pre_register_hir_into_tc` prevents false "undefined" errors. **Known limit:** `use std.io` creates `io` as an SSA local in the init function (not a MIR global), so `io.print(…)` is inaccessible from named action bodies — use the global `print()` builtin instead. |
| `export use` re-export | ✅ | `export use std.math` in `fileB.fdn` makes the `math` namespace accessible in any file that does `use fileB`, without that file needing its own `use std.math`. `re_export: bool` threaded through `Item::Use` → `HirUseDecl` → `MirUseDecl`; `TypeChecker::pre_register_namespace()` exposes only `re_export=true` entries to the importing file; `merge_module` keeps all use_decls for runtime correctness. |
| `std.io` | ✅ | `fidan-stdlib/src/io.rs` — print, println, eprint, readLine, readFile, writeFile, appendFile, deleteFile, fileExists, listDir, getEnv, setEnv, args, cwd, etc. |
| `std.string` | ✅ | `fidan-stdlib/src/string.rs` — toUpper/`to_upper`, toLower/`to_lower`, trim, split, join, replace, startsWith, endsWith, contains, len, repeat, pad, etc. All methods available in both camelCase and snake_case. |
| `std.math` | ✅ | `fidan-stdlib/src/math.rs` — sqrt, abs, floor, ceil, round, pow, log, log2, log10, sin/cos/tan, min, max, clamp, PI, E, etc. |
| `std.collections` | ✅ | `fidan-stdlib/src/collections.rs` — Queue, Stack, Set, OrderedDict; enqueue/dequeue, push/pop, setAdd/setRemove/setContains, stackPeek/`stack_peek`, etc. |
| `std.test` | ✅ | `fidan-stdlib/src/test_runner.rs` — assertEqual, assertNotEqual, assertTrue, assertFalse, assertSome, assertNone, assertContains, assertGt/Lt/Ge/Le, fail; returns `__test_fail__:` sentinel on failure |
| `std.parallel` | ✅ | `fidan-stdlib/src/parallel.rs` — parallelMap, parallelFilter, parallelForEach, parallelReduce; `NeedsCallbackDispatch` protocol wired in `MirMachine::exec_parallel_op` (serial in MIR phase; true parallelism in Phase 8/9) |
| `fidan test` command | ✅ | `ExecutionMode::Test` in `fidan-cli`; runs full pipeline, reports pass/fail with coloured output and exit code |

---

## Phase 8 – Cranelift JIT / `@precompile`

> **Note:** Cranelift is used exclusively for JIT compilation in Fidan.
> Static AOT compilation (object files, system linker) is handled by LLVM in Phase 11.

| Item | Status | Notes |
|---|---|---|
| Decorator pipeline (parser → HIR → MIR) | ✅ | `parse_decorators()` propagates `Vec<Decorator>` through `parse_action_decl`; forwarded via HIR `precompile: bool` to `MirFunction::precompile` |
| Type-check decorator validation (W2004) | ✅ | `check_decorators()` in `fidan-typeck`; W2004 "unknown decorator" for unrecognised names; accepts `@precompile`, `@deprecated`, and any user-defined action in scope (§22.11 custom decorators) |
| Cranelift workspace deps | ✅ | `cranelift-jit`, `cranelift-codegen` (x86 feature), `cranelift-frontend`, `cranelift-native`, `cranelift-module` at 0.115 |
| `JitCompiler` struct + `JITModule` setup | ✅ | `fidan-codegen-cranelift/src/jit.rs`; native ISA via `cranelift_native::builder()` |
| `JitCompiler::compile_function` | ✅ | Full MIR → Cranelift IR lowering: SSA Variables, block params for phi nodes, I64 ABI boundary, entry-block param binding |
| MIR instruction set in JIT | ✅ | `Assign`, `LoadGlobal`, `Call(Method/stdlib)`, `Drop`, `Nop`; terminators: `Return`, `Goto`, `Branch` (brif), `Unreachable` |
| Binary / unary operator emission | ✅ | `emit_binop`: iadd/isub/imul/sdiv/srem (int); fadd/fsub/fmul/fdiv (float); icmp/fcmp (comparisons); band/bor (boolean). `emit_unop`: fneg/ineg, icmp-zero-check |
| stdlib `math.*` dispatch in JIT | ✅ | `emit_stdlib_method_call`: `math.sqrt/abs/floor/ceil/trunc` → native Cranelift intrinsics |
| ABI I64 trampoline (`call_jit_fn`) | ✅ | Packs `FidanValue` args to `i64`; `dispatch_native` unsafe transmute for 0-8 args; result unpacked |
| Eligibility check | ✅ | Only Integer/Float/Boolean params + return eligible; non-primitive types fall back to interpreter |
| Per-function call counters in `MirMachine` | ✅ | `Arc<Vec<AtomicU32>>` shared across threads; `@precompile` pre-warmed to `threshold − 1` |
| Hot-path auto-compilation (counter ≥ threshold → Cranelift) | ✅ | Threshold reached → `JitCompiler::compile_function` → stored in `Arc<RwLock<Vec<Option<JitFnEntry>>>>` |
| `--jit-threshold N` CLI flag | ✅ | `CompileOptions::jit_threshold` (default 500); `--jit-threshold 0` disables JIT; `run_mir_with_jit` public API |
| Precompiled frame debug map | ⬜ | Stack traces show `[precompiled]` for JIT-compiled frames |
| Benchmark JIT vs. interpreter | ⬜ | Numeric-heavy loops expected 3–10× speedup |

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
| REPL with history + multi-line | ✅ | `count_brace_delta()` string-literal-and-comment-aware; `open_braces` tracks depth; `...` continuation prompt; `:cancel` aborts block at any depth; Ctrl+C also cancels |
| `:help` REPL command | ✅ | Prints all available REPL commands; banner directs users to `:help` on startup |
| stdin support (`fidan run -`) | ✅ | Reads from stdin when file is `-` |
| `fidan check` | ✅ | Parse + typecheck only; exits non-zero on any error; `--max-errors N` accepted |
| `fidan fix` | ✅ | Collects `Confidence::High` `SourceEdit` suggestions; applies to file or `--dry-run` prints old/new lines |
| `fidan explain <code>` | ✅ | Prints code, title, category from `codes.rs` registry |
| LSP server | ✅ | **All implemented:** hover (type+decl detail), go-to-definition, completion (context-aware dot-trigger via full cross-module member chain + `module.` alias prefix completion; named-arg `paramName = ` suggestions inside calls; snippet `insertText`), semantic tokens, formatting, cross-module diagnostics, import chain analysis, `textDocument/signatureHelp` (works for both standalone actions and method calls including cross-module inherited methods), `textDocument/references`, `textDocument/rename`, `textDocument/codeAction` (fix-it patches from `Diagnostic::data` JSON), `textDocument/documentSymbol` (outline with nested hierarchy), inlay hints (untyped var type annotations, cross-module return type patched by `refresh()`), folding ranges (braces, block/line comments) |
| VS Code extension | ✅ | **All implemented:** TextMate grammar (`fidan.tmLanguage.json`), `language-configuration.json`, LSP client with format-on-save, `fidan.restartServer` + `fidan.showOutput` + `fidan.runFile` commands, status-bar server indicator (spinning/check/error icons), snippets file (`fidan.code-snippets.json` — 19 snippets), completion `insertText` with snippet syntax (done on LSP side), constructor calls `Dog()` now highlighted (grammar `function-call` regex changed from `[a-z_]` to `[A-Za-z_]` prefix). `Debuggers` category listed but no debug adapter (Phase 11+). |
| `fidan format` formatter | ✅ | `fidan-fmt` crate: `format_source()`, `check_formatted()`, `FormatOptions`; `fidan format <file> [--in-place] [--check]` CLI; 18/18 formatter unit tests (incl. round-trip); idempotent on all constructs; `panic(expr)` bug fixed |

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

## Future Language Features

| Feature | Status | Notes |
|---|---|---|
| Slicing syntax (`list[1..3]`, `str[0..5]`, `list[::2]`) | ✅ | `Expr::Slice` → `HirExprKind::Slice` → `Rvalue::Slice`; negative indices, inclusive/exclusive end, step support; works on lists and strings. |
| List comprehension (`[x * 2 for x in items if x > 0]`) | ✅ | Parser production + HIR desugar to `for` loop that appends to a fresh list local; `if` guard as conditional append; nested comprehensions supported. |
| Dict comprehension (`{k: v for k, v in pairs}`) | ✅ | Same desugaring as list comprehension; emits `Dict` insert calls; destructuring iteration over list-of-tuples or dict entries. |
| `test{}` blocks | ✅ | `parse_test_decl()` → `HirTestDecl` → `MirProgram::test_functions`; `fidan test` command runs them; per-function pass/fail with coloured output. |
| Enums / ADTs (§22.13) | ✅ | Simple enums implemented: `enum Direction { North, South, East, West }`. `Direction.North` field access returns `FidanValue::EnumVariant { tag }`. `check` pattern matching and `==`/`!=` comparison work. Phase 2 payload (e.g. `Result.Ok(value)`) deferred. |
| Regex (§22.14) | ✅ | `std.regex` stdlib module: `isMatch`, `find`/`find_first`, `findAll`, `capture`, `captureAll`/`exec_all`, `replace`/`replaceFirst`/`replace_first`, `replaceAll`, `split`, `isValid`. Backed by the `regex` crate (NFA/DFA engine). All methods available in camelCase and snake_case. Regex literal syntax and Cranelift DFA emit deferred. |
| Class-as-value (`ClassType`) | ✅ | `var b = Animal` → `FidanValue::ClassType("Animal")`; displays as `<class:Animal>`; `type(b)` → `"class-type"`; `==`/`!=` comparison supported; no new syntax — leverages existing identifier resolution; `MirLit::ClassType` added; MIR lowerer emits `ClassType` literals in init-fn |
| LSP inlay hint: `FidanType::ClassType` | ✅ | `var b = TRex` (bare class used as value) showed `b -> TRex` inlay hint (colliding with instance hint); fixed by adding `FidanType::ClassType(Symbol)` variant to `fidan-typeck/src/types.rs` with `display_name()` → `"class<TRex>"`; 2 registration sites in `check.rs` (`pre_register_object` + `register_item/ObjectDecl`) now emit `ClassType`; `fidan_ty_to_mir` maps `ClassType(_) => MirTy::Dynamic`; LSP `symbols.rs` fallback chain auto-resolves — no LSP changes needed |
| Lambda expressions (`action with (params) { body }`) | ✅ | Full pipeline: `Expr::Lambda` in AST (`expr.rs`, `print.rs`); formatter arm (`fidan-fmt/src/emit_expr.rs`); parser `TokenKind::Action` arm in `pratt.rs` parses `action [with (params)] [returns T] { body }`; typeck arm in `check.rs` calls `check_action_body`, returns `FidanType::Function`; `HirExprKind::Lambda { params, body }` in `hir.rs`; HIR lowering arm resolves param types; MIR lowering synthesises a `MirFunction` via `PendingParallelFor` deferred mechanism (`binding = None`), returns `Operand::Const(MirLit::FunctionRef(id))`; `hir_walk_expr` stub is empty (Phase 1: no outer-scope capture); `FnCtx::lambda_sym` interned as `"__lambda__"` in `lower_program`; integration tests: `lambda_no_param_ok`, `lambda_with_param_foreach_ok`, `lambda_first_where_ok`; `test/examples/lambda_demo.fdn` end-to-end |
| Lambda capture (Phase 2) | ⬜ | `hir_walk_expr` Lambda arm intentionally empty; outer-scope variable capture deferred |

---

## Test Coverage

| Suite | Status | Notes |
|---|---|---|
| Lexer unit tests | ✅ | 12/12 passing in `fidan-lexer/src/lexer.rs` |
| Parser unit tests | ✅ | 41/41 passing in `fidan-parser/src/lib.rs` — var/action/object/control-flow/concurrency/slicing/comprehension/test-block syntax + error recovery |
| Diagnostic code registry tests | ✅ | 9/9 passing in `fidan-diagnostics/src/codes.rs` — compile-time `diag_code!` const items, `lookup()`, `explain()`, code/title/category invariants |
| Interpreter integration | ✅ | 25/25 passing in `fidan-interp/tests/integration.rs` — empty/arithmetic/action/if/while/attempt/recursive programs + R0001 panic + R9001 parallel fail + 3 lambda tests (`lambda_no_param_ok`, `lambda_with_param_foreach_ok`, `lambda_first_where_ok`) |
| Static-analysis passes (E0401, W1004, W2006, W5001, W5003) | ✅ | 5/5 E0401/W1004/W2006 tests + 4/4 W5001/W5003 precompile-hints tests |
| Typeck unit tests | ✅ | 12/12 passing in `fidan-typeck/src/lib.rs` — var inference, action return types, object field/method resolution, certain/optional params, decorator validation, null-safety escalation |
| HIR unit tests | ✅ | 9/9 passing in `fidan-hir/src/lib.rs` (action lowering, object fields/methods, test{} blocks, for-loops, multi-action modules) |
| MIR unit tests | ✅ | 7/7 passing in `fidan-mir/src/lib.rs` (global registration, function lowering, test_functions vec, object construction, multi-action programs) |
| AOT integration (`test.fdn` binary) | ⬜ | |
| Parallel benchmark suite | ✅ | `test/examples/parallel_benchmark.fdn` + `scripts/performance_bm.bat` |

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
| 2026-03-06 | Inlay hints showed `-> dynamic` for cross-module method return types | `refresh()` now patches `inlay_hint_sites` alongside `symbol_table` for cross-module call-result variables |
| 2026-03-06 | Goto-def failed for named args whose method is inherited from an imported type | New `NamedArgLookup::CrossModule` path in `find_named_arg_param` + `Phase1::CrossDocNamedArg` routes through `resolve_member_cross_doc` |
| 2026-03-06 | Dot-completion (`rex.`) missed inherited methods from cross-module parent types | `DocumentStore::collect_type_members` walks full cross-module chain; completion handler uses it instead of local table |
| 2026-03-06 | `module.` dot-completion showed nothing (module alias not in symbol table) | Dot-completion now detects import-alias receivers and calls `DocumentStore::get_doc_top_level` to show all top-level exports |
| 2026-03-06 | Signature help showed nothing for method calls (`rex.roar(`) | `signature_help` refactored to two-phase: local receiver-qualified lookup + `resolve_member_cross_doc` fallback |
| 2026-03-06 | No `paramName = ` suggestions inside function calls | Completion handler now scans backward for enclosing call, resolves callee (local + cross-doc), and surfaces `param_names` as `CompletionItemKind::KEYWORD` items sorted to top |
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
| (current) | `var greet = action {...}; greet()` raised `R0001 unknown builtin 'greet'` | Both call-dispatch paths in `fidan-mir/src/lower.rs` were missing a `global_map` lookup between `env.get(name)` and `Callee::Builtin` — fixed in `lower_expr` (~line 785) and `lower_stmt` (~line 1403): inserted `global_map.get(name) → LoadGlobal + Callee::Dynamic` branch in both paths |

---

*Last updated: 2026-03-05 (current session) — **E0205 null-safety typeck + string-concat coercion.** (1) `E0205 "nullable value used in non-nullable context"` added to `codes.rs` and `explanations.rs`. (2) `possibly_nothing_ident` + `require_non_nullable` helpers added to `check.rs`; E0205 emitted for non-`certain`/uninitialised vars used as arithmetic, bitwise, range operands (`Expr::Binary`), unary +/- (`Expr::Unary`), `for`/`parallel for` iterables, and index objects (`Expr::Index`). (3) `Expr::Binary` infers types first — `is_string_concat` flag suppresses E0205 when `op == BinOp::Add` and either side is `FidanType::String`. (4) Runtime: `eval_binary` in `mir_interp.rs` gains `(Add, String(a), v)` and `(Add, v, String(b))` arms using `fidan_runtime::display` for coercion — `"Hello " + nothing` → `"Hello nothing"`. Build clean.*

*Last updated: 2026-03-07 — Phase 8 Cranelift JIT + Decorator system complete. (1) **Decorator pipeline**: `parse_decorators()` in parser; `Vec<Decorator>` threaded through `parse_action_decl(is_parallel, decorators)`; HIR gains `precompile: bool` on `HirFunction`; MIR gains `precompile: bool` on `MirFunction`. (2) **W2004 "unknown decorator"**: `check_decorators()` validates against `const KNOWN = ["precompile"]`; emits W2004 for any unrecognised decorator name. (3) **Cranelift deps**: cranelift-jit/codegen(x86)/frontend/native/module at 0.115 added to workspace; fidan-codegen-cranelift wired with fidan-ast, fidan-mir, fidan-runtime, fidan-lexer. (4) **JIT compiler** (`fidan-codegen-cranelift/src/jit.rs`): `JitCompiler::compile_function` lowers eligible `MirFunction`s (Integer/Float/Boolean params+return only) to native code via `cranelift_native` ISA detection; SSA Variables + explicit block params for phi nodes; I64 ABI boundary (bitcast at entry/exit for float/bool); `emit_binop/unop/stdlib_method_call` handle arithmetic, comparisons, and `math.*` intrinsics; `call_jit_fn` trampoline packs/unpacks `FidanValue` ↔ `i64`. (5) **JIT counter infra** in `MirMachine`: `Arc<Vec<AtomicU32>>` call counters + `Arc<RwLock<Vec<Option<JitFnEntry>>>>` jit_fns, both shared across parallel threads; `@precompile` pre-warmed to `threshold − 1`; `call_function` increments counter and attempts `JitCompiler::compile_function` at threshold; fast-path read-lock dispatch for compiled functions. (6) **CLI wiring**: `CompileOptions::jit_threshold: u32` (default 500); `fidan run --jit-threshold N`; `run_mir_with_jit(program, interner, source_map, threshold)` public API; `run_mir` delegates with threshold=500. Clean build, all 65 tests pass, all example programs execute correctly.*
*Last updated: (current session) — **W5xxx slow-hints pass + Custom decorators §22.11 + round-trip test + bug fixes.** (1) **Round-trip test**: `fidan-fmt` test 18: reads `test/examples/test.fdn`, formats, reparses, checks zero errors + same item count + idempotent. (2) **`panic(expr)` formatter bug**: `emit_stmt.rs` emitted `panic expr` (without parens) — fixed to `panic(expr)`. (3) **W5xxx slow-hints pass**: new `fidan-passes/src/precompile_hints.rs`; back-edge detection for loop blocks; W5001 (dynamic-type call result used in hot loop) and W5003 (direct call to non-`@precompile` fn in loop) emitted; W5002/W5004 registered in `codes.rs`/`explanations.rs`; wired into `emit_mir_safety_diags()`; W5001 escalated in `--strict` mode; 4/4 tests pass. (4) **Custom decorators §22.11**: `DecoratorArg`/`CustomDecorator` types added to `fidan-hir`; `extract_custom_decorators()` + `BUILTIN_DECORATORS` constant in HIR lowering; `HirFunction::custom_decorators` field; `MirFunction::custom_decorators: Vec<(FunctionId, Vec<MirLit>)>`; MIR post-pass resolves symbols→FunctionIds via `fn_map`; interpreter `run()` fires all decorator calls before entry point using `[fn_name, ...extra_args]` ABI; `check_decorators()` in typeck updated to accept user action names (no W2004 for valid actions in scope). (5) **Lexer test fix**: `&src` → `&*src` in `fidan-lexer` integration test (type coercion). Build clean, all tests pass.*

*Last updated: 2026-03-08 — Four tasks completed. (1) **ARCHITECTURE.md §22.11**: Added "User-Defined (Custom) Decorators" roadmap section and Feature→Phase table row (Phase 5 MIR interpreter, 1–2 weeks). (2) **REPL → MIR migration** (`fidan-interp/src/mir_interp.rs`, `fidan-cli/src/main.rs`): deleted AST interpreter (`interp.rs`, `env.rs`, `frame.rs`); added `MirReplState { accumulated_source, init_bb0_cursor, globals_snapshot }` and `run_mir_repl_line()`; `run_repl()` fully rewritten with full-source recompile per line, `init_bb0_cursor`-based delta execution for globals, `globals_snapshot` pre-fill across lines, and multiline input (brace counting + `...` continuation prompt). (3) **Null safety pass W2006** (`fidan-passes/src/null_safety.rs`): flow-insensitive SSA pass builds `definitely_nothing: HashSet<LocalId>` from `Rvalue::Literal(Nothing)` + SSA copies; flags arithmetic on `nothing`, method calls on `nothing` receiver, field/index access on `nothing`, and calls to `certain=true` params with `nothing` arg; wired into `emit_mir_safety_diags()` and REPL eval loop. (4) **`--strict` mode** (`fidan-driver/src/options.rs`, `fidan-cli/src/main.rs`): `CompileOptions::strict_mode: bool`; `--strict` flag on `fidan run` and `fidan check`; `is_strict_escalated(code)` returns `true` for W1001–W1003, W2004–W2005, W2006; typeck diagnostic loop escalates qualifying warnings to hard errors in strict mode; `emit_mir_safety_diags` accepts `strict_mode` and escalates W2006 accordingly; REPL always uses `strict_mode=false`. 66 tests pass, 0 failures.*
*Last updated: (current) — **Slicing** fully implemented (Option D: `[n..]`, `[..n]`, `[..]`, `[n..m]`, `[n...m]` inclusive, `[n..m step k]`, negative indices, works on lists and strings). Added `Expr::Slice` to AST (`fidan-ast/src/expr.rs`), `HirExprKind::Slice` to HIR (`fidan-hir/src/hir.rs`), `Rvalue::Slice` to MIR (`fidan-mir/src/mir.rs`); threaded through all 10 pipeline stages: AST printer, HIR lowerer, MIR lowerer (`hir_walk_expr` + `lower_expr`), MIR display, type-checker (list→same type, string→string, else Dynamic), interpreter `eval_slice()` (negative-index normalization, step support, inclusive/exclusive end, list+string), dead-code pass, copy-propagation pass, inlining pass. Parser: `in_slice_start: bool` field suppresses `DotDot`/`DotDotDot` in `infix_bp` during start-expression parsing; `sym_step` contextual keyword; complete `LBracket` suffix rewrite handles open-start, closed-start, plain index, and all step/inclusive variants. Build clean, 66 tests pass, smoke test validates `nums[2..5]→[3,4,5]`, `nums[..3]→[1,2,3]`, `nums[7..]→[8,9,10]`, `nums[..]`×10, `nums[0..10 step 2]→[1,3,5,7,9]`, `nums[-3..]→[8,9,10]`, `nums[1...4]→[2,3,4,5]`, `"hello"[1..4]→"ell"`, `"hello"[..3]→"hel"` all correct.*
*Last updated: (current session) — Quality + completeness pass. 113 tests passing, 0 failing (was 85; +9 HIR, +7 MIR, +12 typeck already counted). (1) **PROGRESS.md corrected**: parser tests updated to 41/41 (was 22); typeck tests added as 12/12; slicing, list/dict comprehensions, and `test{}` blocks marked ✅ in Future Language Features table. (2) **REPL multiline** (`fidan-cli/src/main.rs`): replaced naive char-by-char brace count with string-literal-and-comment-aware `count_brace_delta()`; added `:cancel` command available even inside a multiline block; colon-command guard checks `:cancel` before the `open_braces == 0` gate. (3) **HIR unit tests** (`fidan-hir/Cargo.toml` + `fidan-hir/src/lib.rs`): 9/9 tests — action lowering, parallel flag, object fields/methods (`var` keyword), `test{}` block collection, `for` loop in init, multi-action modules. (4) **MIR unit tests** (`fidan-mir/Cargo.toml` + `fidan-mir/src/lib.rs`): 7/7 tests — empty-program init fn, action→MIR function, test_functions vec (single + multiple), global registration, object with 2 fields, two-action program. (5) **Drop/RAII dispatch** (`fidan-interp/src/mir_interp.rs`, `fidan-runtime/src/object.rs`): `FidanClass::has_drop_action` cached at class-table build time; `MirMachine::drop_sym` interned once; `Instr::Drop` handler calls `exec_drop_dispatch()` which checks `Rc::strong_count == 2` (sole-owner threshold: +1 `val` binding + +1 frame slot) and invokes the user-defined `drop` action — deterministic RAII, no GC.*
*Last updated: 2026-03-01 — Phase 6 optimization passes complete (constant folding, dead code elimination, copy propagation, unreachable pruning — all 4 in `fidan-passes`); MIR interpreter complete (`fidan-interp/src/mir_interp.rs`); `fidan run` now executes the full MIR pipeline (HIR → MIR → optimization passes → MIR interpreter); `test/examples/test.fdn` produces all 7 expected output lines correctly. Key MIR lowering fixes: per-class `method_ids` map prevents function-ID collision for same-named methods across classes; `this` parameter added as implicit param 0 for all object methods; `HirExprKind::This`/`Parent` lower to the `this` local register; `parent.method(args)` lowers to a direct `Callee::Fn(parent_fn_id)` call (not virtual dispatch); parameter stubs (`_N = nothing`) removed from function bodies — the frame pre-initialises all locals + call ABI fills params before bb0 runs. Previous: Phase 5 HIR/MIR complete.*
*Last updated: 2026-03-05 — Code quality + `StdlibFn` session. (1) `FidanValue::StdlibFn(module, name)`: specific-name stdlib imports (`use std.io.{readFile}`) now produce first-class callable values — `print(readFile)` → `<action:io.readFile>`, callable as `var f = readFile; f("path")`; MIR pre-pass ④ registers each specific name as a `MirGlobal`; init fn emits `MirLit::StdlibFn`; `Callee::Dynamic` handles `StdlibFn` via `dispatch_stdlib_call`. (2) Display DRY fix: `pub fn display()` added to `fidan-runtime/src/value.rs` as the single source of truth; `builtins::display` now delegates; `io.rs` and `test_runner.rs` import it as `format_val` — zero code duplication. (3) O(n) method dispatch fix: `MirMachine` gains `stdlib_modules: HashSet<Arc<str>>` built at startup; `dispatch_method`'s `is_stdlib` check is now O(1). All 12 tests pass. Previous: 2026-03-05 — Three import/display fixes (E0106, user module imports, `[value]` display bug).*
*Last updated: 2026-03-07 — ClassType + list callback methods + synonym audit. (1) **`FidanValue::ClassType(Arc<str>)`**: `var b = Animal` (bare class name used as value) → `ClassType` sentinel; displayed as `<class:Animal>`; `type(b)` → `"class-type"`; `==`/`!=` comparison supported; `MirLit::ClassType` added to MIR; lowerer registers each object as a `MirGlobal` and emits `ClassType` literals in the init-fn. (2) **List callback methods**: `list.forEach(fn)` and `list.firstWhere(predicate)` implemented with `for_each`/`first_where` snake_case aliases; extracted to `dispatch_list_callbacks()` private method on `MirMachine` — must stay in interpreter layer since callbacks require `call_function()` access. (3) **Synonym audit**: `to_upper`/`to_lower` added to `std.string`; `find_first`/`exec_all`/`replace_first` added to `std.regex`; `stack_peek` added to `std.collections`. All tests pass (0 failures).*
*Last updated: 2026-03-06 — Test hygiene complete. `RunError.code` upgraded from `&'static str` to `DiagCode` (compile-time validation via `diag_code!()` macro at all 9 construction sites in `interp.rs` + `mir_interp.rs`). 65 tests now passing across 4 crates: 12 lexer, 22 parser, 9 diagnostic-code registry, 22 integration (interpreter + static analysis). Also fixed real W1004 false positive: `unawaited_pending.rs` copy-chain now follows `StoreGlobal`/`LoadGlobal` pairs, fixing false alarms for module-level `var h = spawn ...; ... await h` patterns. E0401 parallel data-race detection (`parallel_check.rs`), W1004 unawaited-Pending warning (`unawaited_pending.rs`), R9001 parallel task failure display (`FidanPending::try_join` + `MirSignal::ParallelFail` + `RunError.code` field), and parallel benchmark (`test/examples/parallel_benchmark.fdn`, `scripts/performance_bm.bat`) are all done. Phase 5.5 parallel execution complete. `parallel for` dispatches to Rayon `par_iter` (real multi-core); `parallel { task{} }` and `concurrent { task{} }` each lift task bodies to synthetic `MirFunction`s via the `PendingParallelFor` deferred-body mechanism and spawn real OS threads via `std::thread::scope` + `JoinAll`; `spawn expr` / `await handle` backed by `std::thread::spawn` returning `FidanValue::Pending(JoinHandle)` — `AwaitPending` calls `.join()`; new `SpawnDynamic` MIR instruction handles `spawn obj.method(args)` (dynamic method dispatch in spawned thread) and `spawn fnVar(args)` (dynamic fn-value call in spawned thread); `Shared oftype T` backed by `Arc<Mutex<FidanValue>>` with `.get()` / `.update()` / `.withLock()` interpreter builtins; `SpawnDynamic` propagated to both optimization passes (`dead_code.rs`, `copy_propagation.rs`) and `mir_interp.rs`; new test file `test/examples/spawn_method_test.fdn` validates method-spawn (results 307, 342) and concurrent-block real parallelism; all existing test files pass without regressions. Remaining Phase 5.5 work: `parallel_check.rs` compile-time E4xx data-race detection, W3xx unawaited-Pending warning, parallel task failure display, benchmark.*

*Last updated: (current session) — **Code refactoring + performance analysis.** (1) **`fidan-cli/src/main.rs` split** (3781 L → 426 L): extracted 6 submodules — `explain.rs` (`run_explain`/`run_explain_line`), `fix.rs` (`run_fix`), `imports.rs` (`collect_file_import_paths`, `pre_register_hir_into_tc`, `filter_hir_module`, `STDLIB_MODULES`, `find_relative`), `pipeline.rs` (`run_with_reload`, `run_new`, `run_fmt`, `render_trace_to_stderr`, `emit_mir_safety_diags`, `run_pipeline`), `replay.rs` (`save_replay_bundle`, `load_replay_bundle`), `repl.rs` (`run_repl`, `ReplHelper`, `count_brace_delta`); dead code removed; 0 warnings. (2) **`fidan-interp/src/mir_interp.rs` split** (2843 L → 2183 L + 687 L): extracted `mir_interp/api.rs` containing `MirReplState` struct/impls, all REPL-helper `impl MirMachine` methods, and 6 free public entry-point functions (`run_mir`, `run_mir_repl_line`, `run_mir_with_jit`, `run_mir_with_profile`, `run_mir_with_replay`, `run_tests`); uses Rust 2024 module layout (`mir_interp.rs` + `mir_interp/api.rs`); child module accesses parent-private fields with zero visibility changes; 0 warnings. (3) **Performance analysis**: release build benchmarks — sequential loop N=800M: **937ms**; parallel (4 tasks): **224ms** (**4.18× speedup** ≈ theoretical 4× on 4-core); spawn/await (4 tasks): **225ms** (4.16×); profiler hot paths show `greet`/`to_string`/`describe` dominate user code; `fidan test` time: **20ms** end-to-end. All 25/25 integration tests passing.*

*Last updated: (current session) — **LSP inlay hint fix + Lambda expressions + global-var lambda call bugfix.** (1) **`FidanType::ClassType(Symbol)`**: `var b = TRex` (bare class name) showed `b -> TRex` inlay hint, colliding with instance hint — fixed by adding `ClassType(Symbol)` variant to `FidanType` in `fidan-typeck/src/types.rs`; `display_name()` produces `"class<TRex>"`; `pre_register_object` and `register_item/ObjectDecl` in `check.rs` now emit `ClassType` instead of `Object` for class-name type bindings; `fidan_ty_to_mir` maps `ClassType(_) => MirTy::Dynamic`; no LSP changes needed — existing fallback chain auto-resolves. (2) **Lambda expressions**: full pipeline for `action with (params) { body }`: `Expr::Lambda` added to AST (`fidan-ast/src/expr.rs`, `print.rs`); formatter arm in `fidan-fmt/src/emit_expr.rs`; parser `TokenKind::Action` arm in `fidan-parser/src/pratt.rs` parses optional `with (params)` and `returns T`; typeck arm in `fidan-typeck/src/check.rs` calls `check_action_body`, returns `FidanType::Function`; `HirExprKind::Lambda { params, body }` in `fidan-hir/src/hir.rs`; HIR lowering arm in `fidan-hir/src/lower.rs`; MIR lowering in `fidan-mir/src/lower.rs` synthesises a `MirFunction` via existing `PendingParallelFor` deferred-body mechanism (`binding = None`) — lambda params become `env_params`, result is `Operand::Const(MirLit::FunctionRef(id))`; `hir_walk_expr` Lambda arm is intentionally empty (Phase 1: no outer-scope capture); `FnCtx::lambda_sym` debug field interned as `"__lambda__"` in `lower_program`; 3 new integration tests added (`lambda_no_param_ok`, `lambda_with_param_foreach_ok`, `lambda_first_where_ok`) — total now 25/25; `test/examples/lambda_demo.fdn` validates no-param, typed-param, forEach, firstWhere, and pass-to-action scenarios. (3) **Global-var lambda call bugfix**: `var greet = action {...}; greet()` raised `R0001 unknown builtin 'greet'` — root cause: both call-dispatch paths in `fidan-mir/src/lower.rs` (`lower_expr` at ~line 785 and `lower_stmt` at ~line 1403) had `env.get(name) -> Callee::Builtin` with no intermediate `global_map` check; fixed both paths by inserting `global_map.get(name) -> LoadGlobal + Callee::Dynamic` between those two branches. All 25 integration tests pass, `lambda_demo.fdn` works end-to-end.*
