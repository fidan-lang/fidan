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
| `stop` / `separate` → `Semicolon` | ✅ | |
| `SymbolInterner` (DashMap, Symbol = u32) | ✅ | Thread-safe, lock-free fast path |
| Identifier interning | ✅ | |
| Error recovery (Unknown token) | ✅ | `TokenKind::Unknown(char)` |
| Lexer test: tokenise `test/examples/test.fdn` | ⬜ | Phase 2 |
| Lexer test: round-trip all token types | ✅ | 10 unit tests, all passing |
| `--emit tokens` output in CLI | ⬜ | Phase 1 extension |

---

## Phase 2 – Parser

> Goal: Parse `test/examples/test.fdn` to a full AST. Pretty-print round-trips.

### `fidan-ast`
| Item | Status | Notes |
|---|---|---|
| Arena allocator (`typed_arena`) | ✅ | Vec-backed pools: ExprId/StmtId/ItemId |
| `ExprId`, `StmtId`, `ItemId` index types | ✅ | |
| All expression AST nodes | ✅ | Full `Expr` enum in `expr.rs` |
| All statement AST nodes | ✅ | Full `Stmt` enum + `TypeExpr` in `stmt.rs` |
| All item AST nodes (`object`, `action`, etc.) | ✅ | `Item` enum in `item.rs` |
| `Module` root node | ✅ | |
| AST visitor trait | ✅ | Default no-op `AstVisitor` |
| AST pretty-printer | ⬜ | Phase 2 |

### `fidan-parser`
| Item | Status | Notes |
|---|---|---|
| Recursive-descent top-level parser | ⬜ | |
| `object` declaration parsing | ⬜ | |
| `action` declaration parsing | ⬜ | |
| `var` / `set` statement parsing | ⬜ | |
| `if` / `otherwise when` / `otherwise` parsing | ⬜ | |
| `when` (match/switch) parsing | ⬜ | |
| `for item in collection` parsing | ⬜ | |
| `attempt / catch / otherwise / finally` parsing | ⬜ | |
| `return` / `break` / `continue` parsing | ⬜ | |
| `print` and builtin call parsing | ⬜ | |
| Pratt expression parser (full precedence table) | ⬜ | |
| `??` (null-coalesce) operator | ⬜ | |
| Named argument call parsing | ⬜ | |
| Extension action declaration | ⬜ | |
| `parallel action` modifier | ⬜ | |
| `concurrent { task ... }` block | ⬜ | |
| `parallel { task ... }` block | ⬜ | |
| `parallel for` parsing | ⬜ | |
| `spawn` / `await` expressions | ⬜ | |
| String interpolation AST node | ⬜ | |
| `is not` token-pair → `NotEq` normalisation | ⬜ | |
| Error recovery (synchronisation set) | ⬜ | |
| Parse `test/examples/test.fdn` without errors | ⬜ | |
| Round-trip test (parse → print → parse → compare) | ⬜ | |

---

## Phase 3 – Semantic Analysis

> Goal: Typecheck `test.fdn`; report all type errors on a buggy version.

### `fidan-typeck`
| Item | Status | Notes |
|---|---|---|
| Symbol table with scope stack | ⬜ | |
| `object` registration + field/method resolution | ⬜ | |
| Inheritance chain (`extends`) resolution | ⬜ | |
| `var` type inference | ⬜ | |
| Expression type inference | ⬜ | |
| Type checking (assignments, returns, args) | ⬜ | |
| `this` and `parent` binding | ⬜ | |
| Extension action dual-registration | ⬜ | |
| Named / positional argument checking | ⬜ | |
| `required` / `optional` parameter checking | ⬜ | |
| Null safety flow analysis (warnings) | ⬜ | |
| Decorator validation (`@precompile`, etc.) | ⬜ | |
| `parallel action` → `Pending oftype T` inference | ⬜ | |
| `parallel_check.rs` data race detection (E4xx) | ⬜ | |
| `Shared oftype T` recognised as thread-safe | ⬜ | |
| `Pending oftype T` from `spawn expr` | ⬜ | |
| W3xx: unawaited `Pending` dropped | ⬜ | |

---

## Phase 4 – Diagnostics

> Goal: Error messages that make users say "wow".

### `fidan-diagnostics`
| Item | Status | Notes |
|---|---|---|
| `Diagnostic` / `Label` / `Suggestion` types | ⬜ | |
| `ariadne` rendering integration | ⬜ | |
| `FixEngine` with E1xx, E2xx, E3xx rules | ⬜ | |
| Edit-distance suggestions for undefined names | ⬜ | |
| All error codes produce rendered output | ⬜ | |

---

## Phase 5 – HIR + MIR + Interpreter + `concurrent`

> Goal: `fidan run test/examples/test.fdn` works end-to-end.

### `fidan-hir`
| Item | Status | Notes |
|---|---|---|
| HIR types | ⬜ | |
| AST → HIR lowering | ⬜ | |

### `fidan-mir`
| Item | Status | Notes |
|---|---|---|
| `BasicBlock`, `Phi`, SSA locals | ⬜ | |
| All `MirInstruction` variants | ⬜ | |
| Parallel MIR instructions | ⬜ | |
| HIR → MIR lowering (Braun SSA) | ⬜ | |
| Exception landing pads | ⬜ | |
| `concurrent` → `SpawnConcurrent` + `JoinAll` | ⬜ | |
| MIR text dump (`--emit mir`) | ⬜ | |

### `fidan-runtime`
| Item | Status | Notes |
|---|---|---|
| `FidanValue` enum | ⬜ | |
| `OwnedRef<T>` (`Rc<RefCell<T>>`, interpreter-internal) | ⬜ | |
| `SharedRef<T>` (`Arc<Mutex<T>>`, for `Shared oftype T`) | ⬜ | |
| `FidanObject`, `FidanClass` | ⬜ | |
| `FidanList` (COW) | ⬜ | |
| `FidanDict` (COW) | ⬜ | |
| `FidanString` (COW) | ⬜ | |
| Drop / owned-value lifetime tracking | ⬜ | |

### `fidan-interp`
| Item | Status | Notes |
|---|---|---|
| MIR walker / eval loop | ⬜ | |
| Call stack + `CallFrame` | ⬜ | |
| Built-in functions (`print`, `input`, `len`, etc.) | ⬜ | |
| Green-thread scheduler (`corosensei`) | ⬜ | |
| Exception unwind loop | ⬜ | |
| `test/examples/test.fdn` runs, output verified | ⬜ | |

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
| `parallel_check.rs` E4xx errors | ⬜ | |
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

---

## Phase 10 – CLI Polish & LSP

| Item | Status | Notes |
|---|---|---|
| All `fidan` subcommands | ⬜ | |
| REPL with history + multi-line | ⬜ | |
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
| Lexer unit tests | ⬜ | |
| Parser unit tests | ⬜ | |
| Typeck unit tests | ⬜ | |
| Interpreter integration (`test.fdn`) | ⬜ | |
| AOT integration (`test.fdn` binary) | ⬜ | |
| Error code rendering tests | ⬜ | |
| Parallel benchmark suite | ⬜ | |

---

## Known Issues / Blockers

_None yet — implementation not started._

---

*Last updated: 2026-02-28 — Phase 1 starting.*
