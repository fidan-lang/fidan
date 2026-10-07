# Contributing to Fidan

Thank you for your interest in contributing to **Fidan**. Contributions help improve the language, tooling, and ecosystem. We welcome improvements such as bug fixes, performance optimizations, documentation updates, tooling, editor support, and ecosystem integrations.

Fidan is an AI-native general-purpose programming language and compiler toolchain. Its design connects human-readable code and native backends with AI tools that use structured compiler information. The [AI-native tooling documentation](README.md#ai-native-tooling) describes the implemented analysis interfaces, first-party workflows, and their limitations. Keep public descriptions grounded in those capabilities and distinguish design goals from verified results.

Please read this document before submitting a contribution.

---

## Development Setup

### 1. Clone the repository

```bash
git clone https://github.com/fidan-lang/fidan.git
cd fidan
```

### 2. Build the workspace

```bash
cargo build --workspace --locked
```

### 3. Run tests

```bash
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Before submitting a pull request, make sure the project builds successfully and all relevant tests pass.

On Windows, `test\scripts\test.bat` also runs every `.fdn` file under `test/` and its `test {}` blocks. It supplies the replay fixture and treats the trace demo's deliberate failure as expected. `test\scripts\test-runner-coverage.ps1` verifies that a failing test block makes this runner fail; run it separately from example sweeps because it temporarily adds a failing fixture.

Run `test/scripts/test_examples_aot.ps1` (PowerShell) or `test/scripts/test_examples_aot.sh` (Bash) with `--backend cranelift` / `--backend llvm` (PowerShell: `-Backend`). LLVM tests execute the installed helper, so compiler contributors must rebuild/install the matching helper after backend changes; rebuilding the workspace alone does not replace it. Check `fidan toolchain list`, and use the runner's `-FidanHome` / `--fidan-home` option for an isolated toolchain installation. Golden-file checks live in `test/scripts/test_aot.bat` and `test/scripts/test_aot.sh`.

The benchmark helpers use their full default workloads. Unix `performance_bm.sh` additionally requires Valgrind and KCachegrind for Callgrind profiling; it reports missing tools rather than pretending to profile successfully.

Use Rust 1.96 or newer and a host C/C++ toolchain. The current lockfile was tested with Rust 1.99. Linux builds also need `pkg-config` and `libdbus-1-dev` (Debian/Ubuntu names). Default workspace builds do not require LLVM; optional backend setup is documented in the [README](README.md#build-from-source). All six Cranelift crates are constrained to the compatible `0.136` series, currently locked at `0.136.2`. Commit the lockfile with dependency updates and repeat workspace/backend validation before changing the supported series.

---

## Release infrastructure

Windows installer packaging must leave the compiled `target/release/fidan.exe` unchanged. The Inno bootstrap downloads Fidan rather than embedding the compiler; its signing and LZMA2 compression are independent of the distribution binary. Packaging checks the compiler's SHA256 before and after building the installer. Run `./test/scripts/test-release-windows.ps1` on Windows to verify binary/archive preservation, signing cleanup, published-release staging and WinGet bootstrap failures.

WinGet preparation uses Microsoft's `Microsoft.WinGet.Client` module and `Repair-WinGetPackageManager -AllUsers -Latest -Force`, followed by `winget --info`. Preparation, validation or submission failures fail the WinGet job explicitly; an already published Fidan release is left intact.

To retry WinGet for an existing release, run **Submit WinGet release** (`.github/workflows/submit-winget.yaml`) from `main` using `workflow_dispatch`, with version `1.0.15` or another published stable version. It downloads the existing GitHub bootstrap installer, verifies its release digest when available and uses the original Windows VC++ minimum from the published distribution manifest. It then runs the existing `winget validate` and `wingetcreate submit` path. It does not compile, retag, upload or replace release assets. The `releases` environment/repository must provide `WINGET_GITHUB_TOKEN`; that secret is exposed only to the submission step.

For local validation without submitting, run on Windows from the repository root:

```powershell
./scripts/package-release-windows.ps1 -Mode stage-winget-release -Version 1.0.15 -OutputRoot target/winget-retry
./scripts/package-release-windows.ps1 -Mode prepare-winget -Version 1.0.15 -OutputRoot target/winget-retry -WingetManifestRoot target/winget-retry/winget/manifests
```

---

## Project Structure

The repository is organized as a Cargo workspace.

```text
crates/
    fidan-lexer / fidan-parser / fidan-ast / fidan-typeck
    fidan-hir / fidan-mir / fidan-passes
    fidan-interp / fidan-codegen-cranelift / fidan-codegen-llvm
    fidan-runtime / fidan-stdlib
    fidan-driver / fidan-cli / fidan-lsp / fidan-fmt

test/
    ...
```

Core language components live inside `crates/`.  
Tests and examples live inside `test/`.

See the [workspace architecture](README.md#architecture) for all 25 crates, including embedding and optional analysis/toolchain helpers. Crate-local tests also live under `crates/*/tests`.

As the project evolves, additional crates and tooling may be added. Please try to keep contributions aligned with the existing project structure.

---

## Contribution Guidelines

Please follow these guidelines when contributing:

- Keep pull requests **focused and minimal**
- Include **tests whenever possible**
- Write **clear and descriptive commit messages**
- Follow the existing code style and architecture
- Avoid unrelated refactoring in the same pull request
- Keep changes easy to review

Large architectural changes, language design changes, or major runtime/compiler changes should be discussed in an **issue before implementation**.

If you are unsure whether something fits the project direction, open an issue first before investing large amounts of time.

---

## Branching Rules

Do **not commit directly to `main`**.

All contributions must be made from a separate branch.

Example workflows:

```bash
git checkout -b feature/my-improvement
```

```bash
git checkout -b fix/parser-bug
```

Use descriptive branch names that reflect the purpose of the change.

Recommended prefixes:

- `feature/`
- `fix/`
- `docs/`
- `perf/`
- `refactor/`

---

## Code Style

Fidan follows standard Rust conventions.

Before submitting a pull request, run:

```bash
cargo fmt
cargo clippy
```

Code should compile cleanly and avoid unnecessary warnings whenever possible.

Please try to match the style and structure already used in the surrounding code. Consistency is more important than personal style preferences.

---

## Pull Request Process

1. Fork the repository
2. Create a new branch for your change
3. Implement your changes
4. Run formatting and tests
5. Open a pull request

Pull requests should include:

- A clear description of the change
- Motivation for the change
- Any relevant issue references
- Tests if applicable
- Notes about limitations or unfinished parts if relevant

All pull requests must pass CI before they can be merged.

Pull requests that mix multiple unrelated changes may be asked to be split into smaller PRs.

---

## Commit Messages

Please write commit messages that clearly explain the purpose of the change.

Good examples:

- `parser: fix precedence handling for null-coalescing operator`
- `runtime: reduce allocation overhead in the runtime`
- `docs: add syntax examples for extension actions`

Avoid vague commit messages like:

- `fix stuff`
- `update`
- `changes`

---

## Tests

If your contribution changes behavior, please add or update tests whenever practical.

Relevant test categories may include:

- lexer tests
- parser tests
- semantic analysis tests
- runtime/interpreter tests
- language server/editor tests

Bug fixes should ideally include a regression test so the issue does not return later.

---

## Documentation

If you introduce new syntax, change behavior, or modify developer workflows, please update the relevant documentation as part of the same pull request when possible.

This may include:

- `README`
- language documentation
- architecture notes
- editor/tooling documentation

---

## Contributor License Agreement (CLA)

Before a pull request can be merged, contributors must sign the **Fidan Contributor License Agreement (CLA)**.

The CLA ensures that contributions can legally be included in the Fidan project and distributed under the project’s license.

Details will be provided during the pull request process.

---

## Reporting Bugs

If you encounter a bug, please open a GitHub issue and include:

- Fidan version
- Operating system
- Minimal reproduction code
- Expected behavior
- Actual behavior
- Any relevant logs or diagnostics

Clear reproduction steps make issues much easier to investigate.

---

## Feature Requests

Feature proposals should include:

- Motivation for the feature
- A short design overview
- Potential impact on the language or tooling
- Examples of intended usage
- Possible tradeoffs if relevant

Major language features should always be discussed before implementation.

---

## Security

If you discover a security issue, please avoid posting exploit details publicly before the issue can be assessed.

If a dedicated security policy exists later, follow that process. Until then, report security-sensitive issues responsibly.

See [SECURITY.md](SECURITY.md) for more details.

---

## Code of Conduct

Please be respectful, constructive, and professional when interacting with other contributors.

Fidan aims to maintain a welcoming and high-quality development environment.

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for more details.

---

## Final Note

By contributing to Fidan, you help improve the language and its ecosystem for everyone. Thank you for your contribution.
