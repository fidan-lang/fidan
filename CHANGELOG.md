# Changelog

All notable changes to the Fidan programming language are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Fidan uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for
compiler releases.

---

## [Unreleased]

---

## [1.0.13] — 2026-04-21

### Added
- Windows bootstrap/install flows now enforce the Microsoft Visual C++
  Redistributable prerequisite, including version-aware detection in the
  bootstrap script, generated release metadata for the required runtime
  version, and matching Winget dependency metadata for Windows distribution.
- Windows bootstrap coverage now includes a dedicated smoke test under
  `test/scripts/test-bootstrap-windows.ps1`, and the CI workflow runs that
  smoke path on Windows.
- The Windows Inno bootstrap installer now ships expanded language coverage and
  updated localized strings for bootstrap options and error messaging.
- Shared terminal capability helpers were added for CLI-facing tools so color
  output can be gated consistently across TTY/non-TTY environments.

### Changed
- Bootstrap and Windows packaging scripts now use more robust host/platform
  detection, improving consistency across `pwsh`, Windows PowerShell 5.1, and
  release-packaging contexts.
- Windows release packaging now emits VC++ runtime metadata into distribution
  fragments and generated Winget installer metadata, keeping bootstrap,
  packaged release manifests, and Winget dependency data aligned.
- Installer and README wording around bootstrap defaults, installer download
  flows, and Windows install options was polished for clearer first-install and
  release-channel guidance.
- Terminal-facing CLI output was refactored to use shared color/prompt helpers
  instead of ad hoc ANSI sequences in multiple command paths.

### Fixed
- Windows PowerShell 5.1 bootstrap execution now resolves the correct Windows
  host triple and install root more reliably instead of falling back to
  non-Windows platform assumptions.
- First-install Windows bootstrap now installs or upgrades the required VC++
  runtime before staging `fidan`, reducing missing-`VCRUNTIME140.dll` failures
  on clean machines.
- CLI prompts and status output such as self/toolchain/DAL confirmations,
  reload/test status lines, fix diffs, REPL prompt handling, profiler output,
  and AI-helper prompts no longer emit raw ANSI escape sequences when color is
  disabled or the output stream is not a terminal.
- POSIX bootstrap failure output now suppresses ANSI coloring on `NO_COLOR` or
  `TERM=dumb` environments instead of printing literal escape sequences.
- Bootstrap progress messaging around Windows downloads/runtime setup now
  reports steps in a clearer order and surfaces more diagnostic context during
  prerequisite installation.

### Tests
- Added plain-output regression coverage for CLI prompt rendering and profiler
  rendering without color.
- Added Windows bootstrap smoke coverage for local-manifest archive installs and
  VC++ prerequisite handling.

---

## [1.0.12] — 2026-04-19

### Added
- Windows release distribution now includes a signed bootstrap installer and
  Winget publication workflow support, including installer asset upload in
  GitHub Releases and CI-driven Winget submission wiring.
- New packaging modes for Winget handling:
  - `prepare-winget` to rewrite/validate manifests without publishing.
  - `submit-winget` to perform full validation and submission.
- Windows signing flow now supports secret-backed certificate handling for CI:
  base64 PFX decoding, temporary signing material management, and direct
  `ISCC /SCertForge=...` sign-command injection while preserving the Inno
  `CertForge` SignTool alias.
- Internal CLI `__ai-analysis` invocation handling now includes explicit
  internal-command behavior and messaging to prevent accidental manual use.

### Changed
- Release packaging internals were split so cross-platform artifact packaging
  remains in `scripts/package-release.ps1` while Windows installer and Winget
  responsibilities live in `scripts/package-release-windows.ps1`.
- Windows installer build gating now uses clearer environment-truthiness checks
  for CI/local release script behavior.
- Dependency updates across the workspace include `inkwell`/
  `inkwell_internals`, `aws-lc-rs`/`aws-lc-sys`, `rand`, `tokio`,
  `webpki-root-certs`, `clap`, and `regalloc2`.

### Fixed
- Winget installer URL generation now uses the correct release-download format.
- CI workflow packaging step now includes required build environment plumbing
  for signing paths.
- LSP wildcard import go-to-definition test stability was improved by avoiding
  a re-entrant store-lock pattern in test lookup flow.
- Internal AI-analysis command handling was tightened so hidden/internal command
  paths are less likely to leak into normal CLI usage patterns.

### Notes
- This is a minor infrastructure-focused release to validate Winget and Windows
  distribution behavior.
- No breaking language changes in Fidan itself are introduced in this release.

---

## [1.0.10] — 2026-04-15

### Added
- Capacity-aware list and dictionary construction paths, including runtime
  `with_capacity` support and matching stdlib/bootstrap wiring to reduce
  reallocation pressure in collection-heavy programs.
- Targeted LSP regressions for user-module import navigation and module-doc
  hover behavior.
- `itoa` in runtime formatting paths to speed up integer-to-string conversion
  during interpolation and display operations.

### Changed
- MIR interpreter internals now handle scalar operands more efficiently and
  recycle frames more aggressively to reduce execution overhead.
- String interpolation and display plumbing across runtime/FFI/backends now
  pre-sizes and reuses formatting buffers more effectively.
- Dependency set refreshed (including async/runtime dependency updates), and
  release documentation in `README.md` expanded for clearer language and
  AI-tooling guidance.

### Fixed
- User-module import go-to-definition now resolves module tokens consistently
  across grouped imports, direct module imports, and string/file-path imports,
  while keeping `std.*` module imports non-file-navigable.
- Background-loaded module docs in LSP hover now refresh from disk so module
  doc-comment additions/updates/removals are reflected immediately.

### Tests
- Added coverage for go-to-definition on grouped/direct/string user-module
  imports plus stdlib import exclusion.
- Added hover regressions verifying grouped user-module doc-comment refresh
  behavior after file updates.

---

## [1.0.9] — 2026-04-10

### Added
- Owned operand handling across the evaluator, improving correctness of value ownership and usage in expressions and input loops.
- Support for constant object fields via `const var` syntax, including parser support and enforcement in type checking.
- Extended standard library capabilities for file and JSON handling:
  - File operations now include structured error handling for read, write, append, delete, and directory management.
  - JSON parsing and file loading support soft error handling, returning `nothing` when enabled.
  - Dispatch functions now return `Result<FidanValue, StdlibRuntimeError>` for more explicit error propagation.
- Improved diagnostics and language tooling:
  - `HashSet` support in receiver chain diagnostics and completions.
  - Enhanced hover rendering with richer type name markdown.
  - Improved method arity validation for built-in types.
- Expanded test coverage for object field semantics, standard library argument validation, and diagnostics.

### Changed
- Parser and type checker now distinguish between mutable and constant object fields, preventing reassignment and enforcing stricter type guarantees.
- Standard library parameter validation now handles optional and variadic arguments more consistently across functions.
- Completion and diagnostic pipelines now include `HashSet` members and methods, improving editor feedback and correctness.
- Internal refactoring of diagnostic and hover-related utilities to improve maintainability and clarity.

### Fixed
- Cross-module hover resolution in VSCode now correctly handles nested member access.
- `HashSet` iteration behavior corrected to ensure consistent runtime semantics.

### Tests
- Added tests for constant object field declarations and invalid assignment scenarios.
- Added tests for standard library argument type validation across multiple literal and parameter configurations.
- Added tests for `HashSet` diagnostics, completions, and method handling.
- Added tests for file and JSON handling, including error propagation and soft-failure modes.

---

## [1.0.8] — 2026-04-10

### Added
- New `hashset oftype T` support now spans the language surface, runtime,
  interpreter, stdlib metadata, JSON/container handling, and both Cranelift and
  LLVM code generation paths, with expanded integration and concurrency
  regressions.
- Leading `#>` documentation extraction and richer hover coverage now surface
  docs for user-module imports, stdlib members, and related editor entry points.
- Parser, CLI trace, and release-smoke coverage now exercise richer string
  interpolation handling and error reporting, including nested expressions in
  interpolated fragments.

### Changed
- Type checking, MIR lowering, runtime metadata, and LSP analysis now share
  more precise object/container/stdlib call information, improving hover,
  signature help, completion, imported-constructor handling, and member/arity
  validation instead of degrading to `dynamic`.
- Import and namespace handling in the LSP now distinguish consecutive module
  imports, direct stdlib bindings, grouped imports, aliases, and user-module
  namespaces more consistently, keeping semantic tokens and navigation aligned.
- CLI/distribution install candidate resolution, local test directory naming,
  packaging helpers, and AI/helper provider plumbing were tightened for more
  predictable toolchain and distribution workflows.

### Fixed
- LLVM backend borrow-checker issues in `inkwell_backend.rs` no longer block the
  LLVM build path.
- Module hover/signature rendering and semantic token classification now stay
  consistent for stdlib and imported-module usage sites such as `io.join(...)`
  and `math.abs(...)`.
- Runtime and stdlib edge cases around dict/hashset/string/time/io helpers,
  external value handling, and release-smoke filesystem flows now have aligned
  behavior and regression coverage.

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

[Unreleased]: https://github.com/fidan-lang/fidan/compare/v1.0.13...HEAD
[1.0.13]: https://github.com/fidan-lang/fidan/compare/v1.0.12...v1.0.13
[1.0.12]: https://github.com/fidan-lang/fidan/compare/v1.0.10...v1.0.12
[1.0.10]: https://github.com/fidan-lang/fidan/compare/v1.0.9...v1.0.10
[1.0.9]: https://github.com/fidan-lang/fidan/compare/v1.0.8...v1.0.9
[1.0.8]: https://github.com/fidan-lang/fidan/compare/v1.0.7...v1.0.8
[1.0.7]: https://github.com/fidan-lang/fidan/compare/v1.0.6...v1.0.7
[1.0.6]: https://github.com/fidan-lang/fidan/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/fidan-lang/fidan/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/fidan-lang/fidan/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/fidan-lang/fidan/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/fidan-lang/fidan/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/fidan-lang/fidan/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/fidan-lang/fidan/releases/tag/v1.0.0
