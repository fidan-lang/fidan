# Changelog

All notable changes to the Fidan programming language are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Fidan uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for
compiler releases.

---

## [Unreleased]

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

[Unreleased]: https://github.com/fidan-lang/fidan/compare/v1.0.3...HEAD
[1.0.3]: https://github.com/fidan-lang/fidan/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/fidan-lang/fidan/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/fidan-lang/fidan/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/fidan-lang/fidan/releases/tag/v1.0.0
