# Changelog

All notable changes to the Fidan programming language are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Fidan uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for
compiler releases.

---

## [Unreleased]

---

## [1.0.7] — 2026-04-08

### Added
- New `std.json` module with parsing, validation, file I/O, compact
  serialization, and pretty-print helpers, backed by runtime, stdlib metadata,
  type-checker inference, interpreter coverage, and editor hover/completion
  support.
- Native tuple runtime packing and tuple-index access are now covered by
  dedicated regressions across the runtime, Cranelift JIT, interpreter JIT,
  Cranelift AOT, and LLVM AOT paths.
- Parser, lexer, formatter, and LSP regression coverage now includes nested
  string literals and member/index expressions inside interpolation fragments,
  plus grouped stdlib-import hover resolution.

### Changed
- Tuple-valued results are now used consistently for helpers such as
  `std.collections.zip`, `std.collections.enumerate`, `std.collections.partition`,
  `std.async.waitAny`, and `std.async.timeout`, aligning runtime behavior with
  type metadata, display formatting, and backend lowering.
- Stdlib return-type metadata is now significantly more precise and shared more
  broadly across the type checker and editor tooling, improving inferred tuple,
  pending, collection, JSON, regex, IO, and numeric helper result types.
- Unannotated action return inference now merges all reachable return paths,
  preserving precise result types where possible and avoiding premature
  degradation to `dynamic`.

### Fixed
- Cranelift JIT now supports tuple-valued ABI crossings natively instead of
  silently relying on fallback execution for non-primitive tuple signatures.
- Indexed assignment diagnostics now reject immutable targets such as tuples and
  strings with explicit errors instead of permitting invalid assignment shapes.
- LSP identifier/member indexing inside interpolation fragments now points to
  the real inner expression spans, improving hover, navigation, and method-error
  locations for interpolated code.
- Loop binding scope analysis and grouped stdlib import handling now remain
  consistent between type checking, symbol lookup, completion, and hover.

---

## [1.0.6] — 2026-04-07

### Added
- Multi-line normal and raw string literals are now documented and covered by
  end-to-end regressions across the interpreter, Cranelift JIT, Cranelift AOT,
  and LLVM AOT, including a shared runnable smoke fixture.
- New diagnostic `E0308` now reports attempts to call non-callable values such
  as literals, `nothing`, and built-in return values with a dedicated error code
  and explanation.
- Formatter regression coverage now includes configurable line-wrapping for
  long expressions, collections, and multiline string literals based on
  `max_line_len`.
- LSP analysis now records richer typed member-access and import-resolution
  metadata, enabling better hover and navigation across aliased, exported,
  grouped, direct, and wildcard imports.

### Changed
- `fidan fmt` now makes line-wrapping decisions using the configured
  `max_line_len`, including preserving large multiline strings when collapsing
  them would create unreadable escaped one-line output.
- Receiver method metadata is now centralized and shared across the type
  checker, runtime, and stdlib, keeping string/list/dict/`Shared`/`WeakShared`
  method behavior and diagnostics aligned.
- Import analysis and document refresh in the LSP now distinguish namespace,
  direct, and wildcard imports for both file imports and user-module imports.
- Type checking and editor analysis now model enum variants, constructors, and
  receiver members more consistently, improving downstream hover, completion,
  and go-to-definition behavior.

### Fixed
- String literals now normalize source line endings inside multiline string
  bodies so values are stable across LF and CRLF checkouts on all supported
  execution backends.
- Calling non-callable values now emits proper type-checker diagnostics instead
  of falling through to confusing call-site behavior.
- Invalid receiver method calls on built-in and shared container types now
  report clearer, more specific errors.
- Go-to-definition, hover, semantic token classification, and import-aware LSP
  lookups now resolve aliased/exported imports, wildcard imports, and imported
  enum/class symbols more reliably.
- Constructor calls, parent-constructor calls, and enum unit-variant access are
  now preserved correctly through type checking and editor navigation paths.

---

## [1.0.5] — 2026-04-06

### Added
- `%=` compound assignment is now supported across parsing, formatting, and
  execution paths, with regression coverage for interpreter and end-to-end
  behavior.
- LSP symbol indexing now tracks lexical scopes for locals, nested actions,
  loops, methods, and tests, enabling smarter in-scope completion and lookup.
- Enum and enum-variant symbols are now indexed consistently for completion,
  document symbols, hover, and semantic token classification.
- Toolchain packaging now records the AI-analysis backend protocol version so
  packaged helper distributions can expose protocol compatibility.

### Changed
- VS Code / LSP completion now prefers currently visible local symbols over
  globals and resolves chained receivers such as nested object-field member
  access more accurately, including manual `Ctrl+Space` completion after `.`.
- Semantic token classification now uses analyzed symbol information for enum
  types, enum members, namespace imports, and import aliases instead of relying
  only on lexical heuristics.

### Fixed
- Unknown object field types now emit proper diagnostics during type checking
  instead of being silently accepted during object declaration analysis.
- Enum members are now colored consistently both inside enum declarations and
  at qualified usage sites such as `Direction.Up`.
- Local-scope completions no longer leak branch-only symbols across sibling
  blocks, and newly declared locals/actions are suggested reliably.
- Action and constructor parameter validation coverage was tightened so invalid
  argument and `certain`/`nothing` cases are exercised by regressions.

---

## [1.0.4] — 2026-04-05

### Added
- Nested block-scoped action declarations now work across parsing,
  type-checking, lowering, the interpreter, Cranelift, and LLVM paths.
- Decorators on nested local actions are now supported consistently.
- Regression coverage for AI-assisted fixes now includes structural scope
  repairs such as moving a nested helper or action into the correct scope.

### Changed
- `fidan fix --ai` now re-checks candidate AI edits against compiler state
  before accepting them and can keep valid hunks from mixed-quality model
  output while rejecting the rest.
- Diagnostics-mode AI fix prompting now allows minimal structural supporting
  edits outside the exact diagnostic line when required to fix the root cause.

### Fixed
- AI fix payload validation now rejects empty fix responses when compiler
  errors still remain.
- `fidan fix --ai` no longer accepts syntax-breaking hunks that worsen parser
  or type-checker state.
- AI helper integration tests on Linux no longer hang by probing desktop
  keychain or secret-service providers during local-provider test runs.

---

## [1.0.3] — 2026-04-04

### Added
- `fidan exec ai configure` subcommand for interactive AI-analysis provider
  configuration; auto-creates `ai-analysis.toml` on first use.
- API key is now optional when targeting local LLM providers (e.g. Ollama).
- `fidan exec ai doctor` now validates the active provider before running
  diagnostics.

### Fixed
- Config auto-creation written to the correct Fidan home path in all cases.

---

## [1.0.2] — 2026-04-03

### Added
- `fidan exec ai` subcommand family: `analyze`, `explain`, `doctor` — in-editor
  AI-powered diagnostic explanations backed by the ai-analysis helper toolchain.
- `fidan exec ai` routes to an installed `ai-analysis` toolchain over a stable
  binary protocol; toolchain version is resolved from `fidan.home/toolchains/`.

### Changed
- `fidan toolchain available` now prints the helper protocol version alongside
  the tool version for each published toolchain package.

---

## [1.0.1] — 2026-04-03

### Added
- Rich structured diagnostics output: all errors and warnings render with
  source spans, caret underlining, and prose context.
- `fidan explain <code>` — prose explanation for any diagnostic error code.
- Hot-reload (`--reload`) and replay-based crash reproduction (`--replay`).
- `@precompile` decorator to eagerly warm Cranelift JIT for hot action paths.
- `Shared oftype T` type for safe cross-thread shared state (`Arc<Mutex<T>>`).
- `fidan repl` — MIR-based interactive REPL.

### Fixed
- Unawaited `spawn` handles now emit warning W1004 instead of silently
  dropping the join handle.
- Type-checker correctly rejects re-assignment to `certain` (non-optional)
  parameters.

---

## [1.0.0] — 2026-04-02

### Added
- **Lexer, parser, and AST** — full Fidan grammar; synonym token support
  (`equals`, `set`, `oftype`, `certain`, `optional`, `nothing`, …).
- **Two-pass type checker** — null safety, data-race detection, scope
  resolution, type inference.
- **HIR → MIR lowering** and MIR optimization pass pipeline.
- **MIR tree-walking interpreter** (`fidan run`).
- **Cranelift JIT** — auto hot-path promotion with interpreter fallback for
  unsupported MIR nodes.
- **Cranelift AOT** (`fidan build`) — produces native binaries on all three
  host platforms.
- **LLVM AOT backend** (`fidan build --release`) — full O0–Os/Oz optimization
  levels, full LTO, and symbol stripping via `--opt`, `--lto`, `--strip` flags;
  `--release` shorthand enables O3 + full LTO + strip-all.
- **Standard library**: `std.io`, `std.math`, `std.string`, `std.collections`,
  `std.test`, `std.parallel`, `std.time`.
- **`fidan test`** — discovers and runs inline `test { "name" => { … } }` blocks.
- **`fidan fmt`** — source file formatter.
- **`fidan check`** — type-check only (no codegen).
- **Full LSP server** — diagnostics, hover, completion, signature help,
  go-to-definition, renaming, inlay hints.
- **VS Code extension** — syntax highlighting, semantic tokens, full LSP
  integration.
- **Package manager (DAL)** — `fidan dal add/remove/search/publish/yank/…`;
  versioned local installs, `dal.toml` manifests, `dal.lock` lockfiles.
- **`libfidan`** — embeddable C/Rust API for hosting the Fidan runtime.
- **`fidan self`** — self-managed install, update, and uninstall.
- **`fidan toolchain`** — optional toolchain package management (LLVM,
  ai-analysis).
- **Real OS thread parallelism** — `parallel { … }`, `spawn`/`await`,
  `Shared oftype T`.
- **C FFI (`extern`)** — call into native C/C++ libraries from Fidan.
- **Enum types**, slices, decorator system, `check`/`case` pattern matching,
  `loop from … to`, `for … in`, `while`, `concurrent { … }`, `parallel { … }`.

[Unreleased]: https://github.com/fidan-lang/fidan/compare/v1.0.7...HEAD
[1.0.7]: https://github.com/fidan-lang/fidan/compare/v1.0.6...v1.0.7
[1.0.6]: https://github.com/fidan-lang/fidan/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/fidan-lang/fidan/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/fidan-lang/fidan/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/fidan-lang/fidan/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/fidan-lang/fidan/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/fidan-lang/fidan/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/fidan-lang/fidan/releases/tag/v1.0.0
