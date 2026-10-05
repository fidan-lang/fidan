# Engineering audit — 2026-10-05

This pass reviewed the existing toolchain, repaired exposed behavior, refreshed compatible dependencies, and corrected presentation claims. It does not certify every language feature across every backend.

## Architecture and scope

The 25-crate workspace separates source mapping, lexing, arena AST parsing, diagnostics, type checking, typed HIR, SSA/CFG MIR, optimization passes, runtime values, and execution. The MIR interpreter selectively invokes Cranelift JIT where lowering is supported; native executables use Cranelift AOT or an optional external LLVM helper. Boxed native operations and standard-library semantics live in `fidan-runtime`; `fidan-stdlib` supplies compiler metadata and wrappers.

The driver and CLI coordinate imports, DAL/package operations, diagnostics, toolchain installation, REPL, profiling, and test execution. The formatter and LSP share the compiler frontend. The AI helper contains provider integration and compiler-grounded analysis/MCP commands. `libfidan` and `fidan-embed` expose C and Rust embedding; `fidan-secrets` handles credential storage. Native interop fixtures, crate tests, examples, replay fixtures, benchmark runners, CI, release/installation scripts, and platform branches were included in the review.

Source flows through lexer/parser → AST → semantic/type analysis → HIR → MIR → passes → interpreter/JIT or AOT. Unsupported native lowering remains an explicit compilation error; selective JIT fallback is per function.

## Findings and resolutions

### Slicing

Colon slicing was absent from the parser despite runtime support for range-style slices. Existing interpreted and native implementations duplicated semantics, mishandled negative inclusive endpoints, risked integer overflow, and differed on invalid bounds.

- `[start:stop:step]` now lowers to the existing slice AST/MIR, with an exclusive stop. Indexing and the existing `..`, `...`, and `step` syntax remain supported.
- Omitted or `nothing` components use direction-aware defaults. The default step is one. Negative indices count from the end; negative steps traverse backward. Bounds clamp to the sequence. An explicit reverse stop of `-1` differs from an omitted reverse stop.
- Strings operate on Unicode scalar values, consistently with string indexing and language-level length. These are not grapheme clusters.
- One runtime implementation serves the interpreter and native ABI for strings, lists, and lazy ranges. Wide intermediate arithmetic handles extreme i64 steps and full-domain ranges.
- Static non-integer components produce type diagnostics. Dynamic invalid components, zero steps, and invalid targets produce runtime errors.
- The formatter emits its existing canonical range syntax; formatting colon syntax preserves behavior.

`test/examples/slice_regression.fdn` is the shared semantic fixture. It covers omissions, defaults, stepping, reversal, negative/clamped/empty bounds, Unicode, nested indexing, inclusive range syntax, list/range slices, and extreme integer cases. Interpreter tests use JIT thresholds zero and one; threshold one exercises selective JIT/fallback, not a claim that every operation is JIT compiled. Native tests cover valid behavior and invalid components on both AOT backends when LLVM is installed.

### file_exists and the LOCAL file manager

Direct `file_exists` calls correctly returned true after saving, so the initial interpreter/path checks did not reproduce an always-false IO function. Stronger release validation of the original `LOCAL/test_file_manager.fdn` declarations reproduced the reported symptom on Cranelift AOT: a new object's `loadData()` took the missing-file branch despite an existing file. Both AOT backends reproduced the underlying logical-negation defect independently.

**Root cause:** native `not` used bytewise complement on an i8 boolean. `true` (1) became 254, which remained truthy when branched on or boxed. Thus `if not io.file_exists(...)` incorrectly entered the missing-file branch even when the function returned true. Cranelift now compares the operand to zero; LLVM compares to zero and extends the i1 result to the existing i8 boolean ABI. Interpreter and JIT already implemented logical negation correctly. No protocol or ABI shape changed. A shared truth-table/branch fixture and the file-manager save/reload test cover this regression.

Once the correct reload branch ran, it exposed another native bug: method dispatch retained an immutable dictionary-field borrow through the user callback, so `this.tasks = json.load(...)` panicked on a conflicting mutable borrow. Method lookup now clones the function and releases the borrow before invocation. The regression verifies reload, membership, subsequent task addition, and repeated object-field mutation; valid assertions remain intact.

Relative paths resolve against the process working directory, not the source file directory. The regression deliberately separates these directories and checks relative and absolute paths, Windows backslash paths, aliases, missing files, object constants, negated existence checks, and JSON persistence/reload through the interpreter, Cranelift AOT, and LLVM AOT when available. Path normalization, argument conversion, and object constants were not responsible for the reproduced failure.

A separate IO issue was found: `Path::exists` hides filesystem inspection errors. `file_exists` now uses `try_exists`: missing paths return false, existing paths return true, and inspection failures propagate IO diagnostics, including permission errors. An invalid-path regression verifies errors are not converted to false. Existing directory behavior is preserved.

### Other correctness and tooling fixes

- Native string indexing now returns Unicode characters. Out-of-range native sequence indexing reports R2002; overly negative list indices no longer clamp to the first element. Invalid list assignment reports an error instead of changing the first element or silently doing nothing. Valid `nothing` list elements remain distinguishable from failed lookup.
- Language-level string length consistently counts Unicode scalars. The embedding-facing Rust `FidanString::len` retains its byte-length semantics; `char_len` supplies the language behavior.
- Native range `contains` no longer returns a placeholder `nothing`. Range methods share interpreter/runtime semantics, inclusive materialization handles i64::MAX, and unrepresentable range lengths report errors.
- Standard-library assertions use the existing structural comparator for collections rather than always treating them as unequal.
- LSP diagnostics, semantic tokens, document edits, and incoming ranges use UTF-16 columns; ranges splitting a surrogate pair are rejected.
- LLVM target-CPU prefix inspection no longer slices a UTF-8 string at an invalid byte boundary.
- Inkwell 0.10 reports exact memory-buffer length. Removed the old trailing-zero stripping workaround, which truncated binary bitcode and broke full LTO; added a bitcode serialization/parse regression.
- Nested native-fixture Cargo builds now use an isolated target directory. Previously they could overwrite the workspace runtime rlib with a narrower feature set and break subsequent doctests with E0463. Relative CARGO_TARGET_DIR values also resolve consistently against the workspace root rather than each test crate directory.
- The syntax reference had a lost-update race: two parallel tasks incremented one `Shared` counter with separate get/set calls. It now aggregates independent task results after joining, and a regression runs the reference 20 times per native backend. README's unsupported `Shared.update` example was replaced with runnable get/set usage and explicit atomicity limits.
- The loose `crash.fdn` reproduced a debug interpreter panic on integer subtraction overflow. The initial audit incorrectly adopted wrapping behavior from native code. External review identified the older R2003 contract; the follow-up restores checked arithmetic and catchable Fidan diagnostics. The archived workload must now report R2003 rather than a wrapped checksum. `test/examples/integer_overflow_regression.fdn` tests the documented boundary and error semantics.
- Rust 1.99 exposed two additional LSP Clippy findings, resolved with normal Option propagation and direct closure passing. A Windows file-manager test cleanup hit an executable sharing violation; binaries now live under ignored target artifacts while temporary source/data cleanup stays checked.
- Original root scratch files are preserved under ignored `LOCAL/scratch/release-1.0.15`. `compare.py` and `compare.cpp` use floating point and exclude the final iteration, unlike the integer Fidan workload. C++ also multiplies signed 32-bit integers before casting, causing overflow undefined behavior. They are not parity or benchmark oracles.

## Dependencies

The Cargo graph was reviewed and compatible stable updates applied, followed by compilation and regression testing. The initial Rust 1.95 audit used Cranelift 0.135.5. After the user upgraded to Rust 1.99 and broadened Cranelift requirements to `0`, the lockfile advanced to **0.136.2**. The reviewer follow-up narrows all six Cranelift workspace requirements to `0.136`, preserving locked **0.136.2** and preventing unrelated future 0.x API/MSRV changes. Cargo.lock needs no dependency-version changes for this constraint correction. Inkwell moved from 0.9 to 0.10 while retaining LLVM 21.1.

`llvm-sys = { version = "211", optional = true }` is unchanged; the user's subsequent Cargo update selected **211.1.0**, within the required 211 series. Six unused workspace declarations were removed: `phf`, `phf_codegen`, `typed-arena`, `ariadne`, `indexmap`, and `smol_str`. These were not active crate dependencies. `cargo machete --with-metadata` reports no unused crate dependencies. A fresh `cargo update --dry-run --verbose` found zero permitted updates. `generic-array` stays at 0.14.7 because Linux keyring dependencies include `crypto-common` 0.1.7 with an exact `=0.14.7` requirement. Newer LLVM bindings and unnecessary major migrations were deliberately excluded. Transitive duplicates imposed by upstream crates remain where necessary.

The table below records direct dependencies, including unchanged entries. Multiple versions include transitive instances of the same package.

| Dependency | Previous lock | Current lock |
|---|---|---|
| anyhow | 1.0.102 | 1.0.104 |
| apple-native-keyring-store | 1.0.0 | 1.0.2 |
| clap | 4.6.1 | 4.6.7 |
| cranelift-codegen | 0.132.2 | 0.136.2 |
| cranelift-frontend | 0.132.2 | 0.136.2 |
| cranelift-jit | 0.132.2 | 0.136.2 |
| cranelift-module | 0.132.2 | 0.136.2 |
| cranelift-native | 0.132.2 | 0.136.2 |
| cranelift-object | 0.132.2 | 0.136.2 |
| crossterm | 0.29.0 | 0.29.0 |
| dashmap | 5.5.3, 6.2.1 | 5.5.3, 6.2.1 |
| dbus-secret-service-keyring-store | 1.0.0 | 1.0.1 |
| flate2 | 1.1.9 | 1.1.10 |
| globset | 0.4.18 | 0.4.20 |
| indicatif | 0.18.4 | 0.18.6 |
| inkwell | 0.9.0 | 0.10.0 |
| itoa | 1.0.18 | 1.0.18 |
| keyring-core | 1.0.0 | 1.0.0 |
| libffi | 5.1.0 | 5.2.0 |
| llvm-sys | 211.0.1 | 211.1.0 |
| mimalloc | 0.1.52 | 0.1.52 |
| notify | 8.2.0 | 8.2.0 |
| parking_lot | 0.12.5 | 0.12.5 |
| regex | 1.12.4 | 1.13.1 |
| reqwest | 0.13.4 | 0.13.5 |
| rustc-hash | 2.1.2 | 2.1.3 |
| rustyline | 18.0.0 | 18.0.1 |
| semver | 1.0.28 | 1.0.28 |
| serde | 1.0.228 | 1.0.229 |
| serde_json | 1.0.150 | 1.0.151 |
| sha2 | 0.10.9, 0.11.0 | 0.10.9, 0.11.0 |
| strsim | 0.11.1 | 0.11.1 |
| tar | 0.4.46 | 0.4.46 |
| target-lexicon | 0.13.5 | 0.13.5 |
| thiserror | 2.0.18 | 2.0.21 |
| tokio | 1.52.3 | 1.53.2 |
| toml | 1.1.2+spec-1.1.0 | 1.1.6+spec-1.1.0 |
| tower-lsp | 0.20.0 | 0.20.0 |
| unicode-width | 0.2.2 | 0.2.2 |
| urlencoding | 2.1.3 | 2.1.3 |
| windows-native-keyring-store | 1.1.0 | 1.1.0 |

## Documentation and CI

README now describes the real pipeline, backend selection, optional LLVM requirements, current project status, slicing semantics, and IO working-directory behavior. Removed unsupported completeness, universal parity, and comparative performance claims, a reference to an ignored LOCAL demo, and an unaudited extension feature inventory. Editor integration is explicitly maintained in its separate repository. Benchmark timings remain workload/host-specific, without invented performance claims.

CI builds and tests the locked workspace, includes doctests, checks formatting, and runs Clippy with warnings denied. Optional LLVM validation now watches manifest/lock changes and the shared slicing/integer/boolean fixtures. LLVM packaging runs feature-gated backend unit tests and strict Clippy before pruning development libraries; its CI installs Clippy explicitly. Toolchain release workflows enable packaged-artifact validation before upload; their input descriptions now match helper-specific version defaults. AI packaging now reads the independent helper manifest for its tool-version default instead of mislabeling the binary with the compiler version. The workflow's actual resolver was executed locally with both an empty version and an explicit override. LLVM-only lint findings were resolved without suppressions. Both example runners require the expected diagnostic for the intentionally failing trace example; the Unix runner previously treated it as an unexpected failure. The contributor guide now names actual crates and workspace verification commands. Ignored type-checker and diagnostic documentation placeholders became runnable doctests; the blocking LSP startup example is compile-checked with `no_run`. No tests or valid assertions were removed or weakened.

Rust 1.99 reports linker messages as compiler warnings. Native LLVM helper packaging exposed MSVC LNK4098: the official LLVM archive requests LIBCMT while Rust, the LLVM C wrapper, libffi, and configured `x64-windows-static-md` libxml2 request MSVCRT. Packaging explicitly selects the DLL CRT using `/NODEFAULTLIB:libcmt` in scoped/restored RUSTFLAGS, following [Microsoft's CRT selection guidance](https://learn.microsoft.com/en-us/cpp/error-messages/tool-errors/linker-tools-warning-lnk4098?view=msvc-170). This changes library selection rather than disabling the warning.

## Verification

The table below records the initial Rust 1.95 validation. The release follow-up
after the user's compiler/dependency update is recorded separately below.

Baseline on Windows x86_64 with Rust/Cargo 1.95.0: formatting, workspace unit/integration tests, and Clippy passed. Final stable-source verification results are recorded below.

The LLVM 21.1.8 helper was built using `scripts/package-toolchain.ps1`, the configured official Windows archive and SHA-256, `llvm-toolchain-21`, `LLVM_SYS_211_PREFIX`, and vcpkg `x64-windows-static-md` libraries. The packaged helper was installed under an isolated `target/audit-fidan-home` for backend tests, avoiding reliance on a stale installed helper.

Verified on Windows x86_64, Rust/Cargo 1.95.0:

| Command or scenario | Result |
|---|---|
| `cargo fmt --all` followed by `cargo fmt --all --check` | Pass |
| `cargo build --workspace --locked` | Pass |
| `cargo build --workspace --release --locked` | Pass |
| `cargo test --workspace --locked`, with isolated rebuilt LLVM toolchain | 849 passed, 0 failed, 0 ignored; includes doctests and both native backends |
| `cargo check --workspace --all-targets --locked` | Pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass |
| `cargo check -p fidan-codegen-llvm --tests --locked --features llvm-toolchain-21,llvm-sys/no-llvm-linking` | Pass |
| `cargo clippy -p fidan-codegen-llvm --all-targets --locked --features llvm-toolchain-21,llvm-sys/no-llvm-linking -- -D warnings` | Pass |
| `scripts/package-toolchain.ps1` with LLVM 21.1.8, upstream SHA-256, and configured vcpkg libraries | Helper release build, 11 LLVM-enabled unit tests, strict LLVM-enabled Clippy, and packaging pass |
| `test/scripts/test_examples_aot.ps1 -Backend cranelift` | 34 cases pass, zero skips |
| `test/scripts/test_examples_aot.ps1 -Backend llvm -Lto full -FidanHome target/audit-fidan-home` | 34 cases pass, zero skips |
| Shared slicing fixture via release CLI, JIT thresholds 0 and 1, and release CLI builds with Cranelift / LLVM full LTO | All pass |
| Invalid slicing, invalid indexing assignment, and overflowing range length | Regression tests pass in interpreter and both AOT backends |
| File-manager path/persistence regression | Interpreter, Cranelift AOT, and LLVM AOT pass |
| Original LOCAL file-manager declarations plus save/reload/add calls in an isolated cwd | Pass; original LOCAL source unchanged |
| Syntax reference repeated native execution | 20 runs per backend pass; interpreter also passes |
| Native fixture tests with relative `CARGO_TARGET_DIR`, launched from each crate directory | CLI and driver cases pass |
| README introductory, slicing, and shared-state snippets; CLI help/version and documented MIR command | Pass |
| `cargo machete --with-metadata` | No unused crate dependencies |
| PowerShell script parsing and `bash -n test/scripts/test_examples_aot.sh` | Pass; Unix runner execution not tested locally |
| README local file links and `git diff --check` | No missing local file links or whitespace errors |

Logs are retained under ignored `target/audit-*.log`, with the original LOCAL probe under `target/local-file-manager-run`. Example sweeps include the deliberately failing trace demonstration and require its expected diagnostic. No benchmark timings are presented as performance comparisons.

### Rust 1.99 release follow-up (before external review)

After the user's compiler and Cargo update, workspace check/build initially
passed. Baseline failures were two new LSP Clippy findings, a Windows temporary
executable cleanup sharing violation, and the real debug integer-overflow crash.
The stronger LOCAL save/reload check then reproduced native boolean negation
and object-method borrowing bugs. These were fixed and validated before the
release candidate was prepared.

Verified on Windows x86_64 with Rust/Cargo **1.99.0**, Cranelift **0.136.2**,
Inkwell **0.10.0**, `llvm-sys` **211.1.0**, and a freshly packaged LLVM helper
**1.0.6** / LLVM **21.1.8**, installed under `target/final-release-home`:

| Command or scenario | Result |
|---|---|
| `cargo fmt --all` and `cargo fmt --all --check` | Pass, all workspace Rust sources |
| `cargo test --workspace --locked` | **854 passed**, zero failed/ignored, including doctests and native regressions |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass |
| `cargo clippy --workspace --all-targets --all-features --locked --features llvm-sys/no-llvm-linking -- -D warnings` | Pass; optional LLVM code type-checked, native linking separately verified below |
| `cargo check --workspace --all-targets --locked` | Pass |
| `cargo build --workspace --locked` and `cargo build --workspace --release --locked` | Pass |
| Required `scripts/package-toolchain.ps1` LLVM 21.1.8 configuration | Release helper build, **11 LLVM-enabled unit tests**, strict native-feature Clippy, packaging; **no compiler/linker warnings** after CRT selection |
| `test/scripts/test_examples_aot.ps1 -Backend cranelift -Release` | **36 pass**, zero skips |
| `test/scripts/test_examples_aot.ps1 -Backend llvm -Lto full -Release -FidanHome target/final-release-home` | **36 pass**, zero skips |
| Release CLI slicing/integer fixtures, JIT thresholds 0 and 1 | Pass |
| Original archived `crash.fdn` | Initial audit passed a wrapped checksum; superseded by the R2003 correction below |
| Original LOCAL file-manager declarations plus missing/save/reload/membership/add assertions in separate fresh working directories | Interpreter, Cranelift AOT, and LLVM AOT pass; LOCAL source unchanged |
| Boolean truth table, branching, double negation, and repeated object-field mutation | Interpreter, selective JIT, both AOT backends pass |
| `cargo update --dry-run --verbose` | Zero permitted updates; upstream exact crypto pin and LLVM series constraint retained |
| `cargo machete --with-metadata` | No unused crate dependencies |
| GitHub Actions YAML, PowerShell package/example scripts, Unix runner syntax | Parse checks pass; AI helper-version resolver default/override both pass |

Follow-up logs are under ignored `target/recheck-*.log`; the original LOCAL
release probe path is recorded in `target/recheck-local-probe-path.txt`.
Scratch originals remain under ignored `LOCAL/scratch/release-1.0.15`.

## External review follow-up

The diagnostic explanation and history predate the audit and describe MAX + 1
as R2003. The previous wrapping regression was therefore a language-semantics
mistake, not evidence of intended behavior.

- Shared checked i64 arithmetic covers addition, subtraction, multiplication,
  unary negation, integer power, and MIN / -1 or MIN % -1. R2003 is catchable;
  integer, float, and mixed zero divisors raise R2001. Normal integer division
  still truncates toward zero. The full i64 exponent is processed without u32
  truncation; 0 ** 0 remains 1.
- Negative integer powers had contradictory implementations and no established
  regression/documentation supporting fractional integer results. The type
  checker explicitly gives integer ** integer an integer result. Exact reciprocal
  powers of bases 1 and -1 are preserved, including i64::MIN exponents. Other
  negative integer powers raise R2003 with guidance to use a float operand;
  2.0 ** -2 and math.pow(2, -2) are 0.25. This is a documented domain resolution,
  not a claim of prior parity for fractional integer results.
- Native arithmetic helpers store diagnostics in the existing exception slot;
  JIT and AOT callers check it before continuing. MIR call analysis propagates
  fallibility through direct calls. JIT callbacks preserve interpreter errors
  instead of replacing them with nothing. Constant folding leaves invalid
  arithmetic for runtime evaluation instead of folding wraparound or infinity.
- Indirect flexible-value calls prevent inlining from bypassing boxed native
  arithmetic helpers in the regression fixture. They also exposed a JIT
  pointer/scalar mismatch: scalar intrinsics and unary/binary lowering now
  verify logical MIR operand types, falling back to the interpreter for boxed
  operations. A dedicated eligibility regression protects this boundary.
- Final review also found unchecked integer absolute value. `math.abs(MIN)` and
  `MIN.abs()` now use the same checked arithmetic and report R2003, including
  flexible standard-library calls and the selective-JIT/AOT math intrinsics.
  Existing public std.math dispatch signatures remain available; error-aware
  dispatch propagates the typed diagnostic to Fidan callers.
- Repeating the full suite exposed the syntax reference's existing lost-update
  race in HEAD: separate shared counter get/set calls produced 1 instead of 2.
  Each parallel task now writes its own Shared result; the parent combines them
  after joining. The existing 20-run-per-backend assertions remain unchanged.
- Receiver substring/substr/slice use the existing std.string half-open scalar
  range semantics: bounds clamp to [0,len], omitted end defaults to len, reversed
  bounds return empty. charAt returns a string, empty for negative/out-of-range
  positions. Receiver searches return scalar indices or -1. Interpreter and
  native receivers share std.string dispatch. Bracket slicing retains its
  separate negative-index/step semantics.
- The LLVM setup command uses toolchain add llvm. WinGet text and tags now state
  implemented capabilities without speed, safety, bytecode-VM, or novelty claims.
  CONTRIBUTING, executable syntax-reference headers, and receiver module comments
  no longer describe obsolete phase placeholders. The empty typechecker
  parallel_check module is removed; the actual MIR parallel race checker remains.
- Cranelift constraints are 0.136, locked at 0.136.2; llvm-sys remains exactly the
  required manifest series 211, locked at 211.1.0. Release versions and serialized
  protocols are unchanged. A fresh LLVM bundle is built locally at version 1.0.6.

Final follow-up validation on Windows with Rust/Cargo 1.99.0:

| Command or scenario | Result |
|---|---|
| `cargo fmt --all --check` | Pass |
| `cargo build --workspace --locked` and CLI-only release build | Pass |
| `cargo test --workspace --locked` with fresh LLVM helper | **862 passed**, zero failed/ignored; includes doctests |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass |
| Strict all-target/all-feature Clippy with `llvm-sys/no-llvm-linking` | Pass; native LLVM linking separately verified by packaging |
| Required LLVM packaging script at existing version 1.0.6 | **11 LLVM-enabled tests** pass; native-feature strict Clippy and release helper build pass |
| Checked integer native JIT unit regression | Eleven error cases explicitly assert native compilation and diagnostic propagation, including math.abs |
| Flexible arithmetic, constant-folding preservation, and MIR call fallibility | Targeted regressions pass |
| Slicing, integer errors, and string receiver fixtures | Release interpreter thresholds 0/1 and both AOT backends pass |
| Cranelift release example sweep | **37 passed**, zero skipped |
| LLVM full-LTO release example sweep | **37 passed**, zero skipped |
| Original LOCAL file-manager declarations plus save/reload/add assertions | Interpreter and both AOT backends pass; original source hash unchanged |
| Archived original crash workload | Reports Fidan R2003 instead of a Rust panic or wrapped checksum |

Logs are retained under ignored `target/followup-*.log`. The example-sweep build
commands also validate the CLI-only release configuration. Existing W5003
precompile advisory hints can appear for the integer fixture's catch CFG; these
are separate from the clean Rust/Clippy checks and were not suppressed.

## Release preparation

The user selected these release-candidate versions; the follow-up does not bump
them. No release has been tagged or published by this audit.

| Component | Prepared version | Protocol |
|---|---|---|
| Fidan workspace / CLI | 1.0.15 | AI analysis 1 |
| LLVM helper and toolchain bundle | 1.0.6 (upstream LLVM stays 21.1.8) | LLVM backend 5 |
| AI helper | 1.0.4, unchanged | AI helper 2 |

The LLVM helper must be republished because code generation and its LLVM binding
dependency changed. The AI helper source did not change; it obtains compiler
facts from the installed Fidan CLI. Its existing compatible 1.x version range
covers Fidan 1.0.15. No serialized request, response, or MIR schema changed,
so the protocol constants remain unchanged. The changelog describes the final
patch, and Cargo regenerated the lockfile for the independent package versions.

The audit and follow-up are prepared on polish/repo-audit. Require the
Windows/Linux/macOS CI and toolchain validation workflows to pass before merging;
then publish LLVM toolchain 1.0.6 before publishing
Fidan 1.0.15 through the existing release workflows. An AI helper release is not
required for these changes. Local validation is distinct from remote CI.

## Remaining limits

- Linux/macOS builds are covered by CI configuration but were not executed on this Windows host; this pass does not claim results from remote CI.
- Optional LLVM tests skip when a compatible toolchain is unavailable. In this audit LLVM tests were run with the rebuilt helper.
- Live AI providers, production registry operations, OS keychain interactions on other platforms, and every possible external native library were not exhaustively exercised. Their existing offline/unit/protocol tests are distinct from live-service validation.
- Backend coverage is supported by the specific regression and existing integration tests, not universal semantic parity. Resource exhaustion from extremely large materializations remains possible, as with other collection allocations.
- Arithmetic parity is verified for the boundary/error cases in the follow-up fixture, including dynamic values and full-width exponents. These tests do not establish universal backend parity or cover resource exhaustion.
- The original file-manager symptom was reproduced and traced to native boolean negation, independently of cwd behavior and hidden IO errors. Other backend parity remains limited to tested scenarios.
- LLVM's existing Windows host-CPU string disposal workaround remains: two small buffers survive until the short-lived helper exits. It was not replaced speculatively without reproducing the upstream/platform failure.

## Main changed areas

`fidan-parser/src/pratt.rs`, `fidan-typeck/src/check.rs`, new runtime `slice.rs` and `range.rs`, runtime `ffi.rs`/`stdlib/io.rs`, interpreter arithmetic/dispatch, LSP position conversion, Cranelift API migration and logical negation, LLVM bitcode/CPU/boolean handling, and their regression suites. Runtime ownership comments now describe actual boxed AOT ownership. Workspace/helper manifests and lock, native fixture test setup, CI/toolchain packaging and CRT selection, README/changelog, this report, and the local ignored AUDIT_PLAN record complete the reviewable changes.
