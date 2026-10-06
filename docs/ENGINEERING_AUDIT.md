# Engineering audit: final 1.0.15 release preparation

This report describes the resulting code and validation. It is not an authority for intended language semantics. Contracts follow established architecture, pre-audit intended APIs, type-system consistency and explicit owner decisions.

## Architecture and identity

Fidan is an AI-native general-purpose programming language and compiler toolchain. AI tools can use structured diagnostics, inferred types, symbols, reads/writes, call graphs, type maps and static statement traces alongside source text through first-party workflows and MCP. Static traces are analysis/value hints, not observed execution. Provider workflows require configuration; deterministic compiler analysis does not require a model.

The Rust pipeline is lexer/parser -> arena AST -> type analysis -> typed HIR -> SSA/CFG MIR -> optimization -> MIR interpreter/selective Cranelift JIT or Cranelift/optional LLVM AOT. Runtime values, boxed native operators and standard-library semantics are shared where practical. The driver/CLI, formatter, LSP, DAL/package/toolchain management and C/Rust embedding complete the tooling. JIT fallback is per function where lowering is unsupported or scalar/boxed ABI conversion cannot be proved safe.

## Final contracts and fixes

### Arithmetic and constants

Integer addition, subtraction, multiplication, negation and nonnegative powers are checked i64 operations. Overflow, including MIN / -1 and MIN % -1, reports R2003. Division/modulo by zero reports R2001. These inputs do not trap the Rust compiler or native host.

The owner explicitly selected value-dependent negative integer powers: base zero reports R2001; bases 1/-1 retain exact integers; other integer bases produce floating reciprocals. Nonnegative powers remain checked integers. Compile-time facts prove precise result categories where possible; otherwise integer/integer power has Dynamic MIR representation. Interpreter fast paths, boxed runtime, constant evaluator and folding share power semantics. Scalar lowering requires a proven integer result.

Constant integer ordering remains exact across the full i64 domain. Overflowing constants remain unfurled runtime expressions rather than panicking or folding invalid values. Dead-code elimination preserves discarded failing arithmetic/slices. Conservative strength-reduction proofs preserve runtime values, categories and errors.

### Shared and receiver contracts

Shared.update is the originally intended atomic transformation. One exclusive guard spans read, existing callable dispatch, validation and replacement, returning the new value. Failure leaves the slot unchanged. get/set are separately synchronized; get followed by set is not a compound atomic operation.

Statically known callbacks must accept one argument compatible with T and return a value assignable to T. Additional optional/default parameters may be omitted. Known wrong arities/types are rejected. Named actions, inline actions, captured closures and stdlib callables reuse existing metadata/dispatch. Flexible Fidan action callbacks receive runtime arity/parameter checking; typed Shared calls validate their results before replacement, even when discarded.

Dynamic AOT trampolines check argument count, apply defaults/optional omission, reject nothing for certain parameters and validate declared types. Interpreter uses the same contracts. Runtime descriptors reuse existing MIR serialization; public schemas/protocols are unchanged. Receiver metadata generically substitutes T/K/V for List, Dict, Shared and other parameterized families. Mutable callable alias proofs are invalidated on assignment/shadowing.

Shared guards track held identities per thread. Recursive same-Shared access through aliases/weak upgrades reports R0001 instead of self-deadlocking. Cross-Shared lock cycles and waiting for tasks requiring the held lock remain programmer hazards; atomicity is not weakened.

### Boxed operators, indexing and strings

String + value uses display concatenation. Unsupported arithmetic/comparison/unary operands report R0001 instead of nothing. Logical and/or/not require booleans. Native truthiness helpers do not redefine these operators. Native enum type/variant cross-comparisons and class-type identity now preserve interpreter equality behavior.

Interpreter and native bracket get/set share one runtime implementation. Negative indices count from the end; real bounds failures report R2002. Wrong target/index types and unhashable dict keys report R0001. Missing dict keys return nothing. Invalid list assignment cannot silently no-op; valid nothing elements remain valid accesses.

Colon [start:stop:step] is canonical for exclusive slices and legacy .. / step aliases. Inclusive ... retains compatibility spelling when colon conversion would change dynamic/negative endpoint behavior. Standalone ranges remain unchanged. Bounds clamp; omitted/nothing bounds follow step direction; invalid/zero steps fail safely. Strings use Unicode scalars, not UTF-8 bytes or graphemes.

Receiver substring/substr/slice clamp bounds to character positions and return empty for reversed bounds. charAt returns empty for negative/out-of-range positions. indexOf/lastIndexOf report scalar offsets. Receiver contracts intentionally differ from bracket negative indexing/stepping.

### Equality and ownership

std.test float tolerance applies recursively through lists, tuples and dict values; dictionary key identity remains exact. General structural value equality remains exact for floats; List.contains/find share that runtime implementation through nested values and enums. A missing List.find result is -1 on both paths. Dictionaries accept structural collection keys; a list key is not a type error.

Boxed ABI parameters are borrowed; returns are owned. Both AOT backends clone borrowed non-scalar identity returns before argument cleanup, including boxed unary-plus results. Fixtures cover direct/dynamic strings, lists, tuples, dicts, functions and captured closures. Callback argument/default boxes are released on success/error. Fresh list/dict construction boxes, explicitly cloned direct-call arguments and fresh fallback operator inputs/results are released locally in both backends. Remaining concrete allocation limits are recorded below; no broad ownership/object rewrite is included.

## Earlier fixes retained and targeted audit-diff review

The file_exists symptom originated in native boolean not using bitwise complement: true remained truthy after negation. Both AOT backends now implement logical negation. Original file-manager save/reload, relative/absolute/Windows paths and object mutation regressions retain their assertions. Runtime file_exists uses try_exists so missing paths return false while other inspection errors propagate. Native callback method lookup releases its field borrow before invocation.

Confirmed audit regressions reviewed against main: wrapping arithmetic replaced R2003; separate-counter examples avoided the incomplete Shared.update API; AI-native identity was weakened; Roadmap was deleted; range-only formatter canonicalization was asserted without an owner decision; negative powers were restricted by an agent-selected rule; release selection was attributed to an earlier user decision. Final code/copy corrects each. Other planned features remain future work, not bugs to implement now.

## Dependencies and explicit release decisions

The current owner request explicitly confirms Fidan 1.0.15 and LLVM helper/toolchain 1.0.6, using upstream LLVM 21.1.8. AI helper remains 1.0.4. This is the final owner decision, not a claim of earlier selection. No tags, releases or merge are performed.

Six Cranelift requirements remain 0.136, locked at 0.136.2. llvm-sys remains optional version 211, locked at 211.1.0; Inkwell remains 0.10/LLVM 21.1. Protocols remain AI analysis 1, AI helper 2, LLVM backend 5. Internal dependency edges reuse runtime power logic and existing serde_json for MIR descriptors; no external versions change in this pass.

Earlier compatible dependency updates and unused declaration removal remain:

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


## Documentation and tooling

AI-native positioning remains prominent and compiler-grounded. World-first, C-performance, Rust-safety and universal parity claims remain removed; bytecode/VM tags remain absent. README restores Implemented / Partial / Planned Roadmap, distinguishing cooperative async, real parallel tasks, scoped LSP/native support and planned GPU/network/process APIs. Implemented comprehensions/actions no longer carry FUTURE comments; genuinely planned shorthand remains marked future.

CI retains locked workspace builds/tests/doctests, strict Clippy and formatting. LLVM packaging uses the repository script with feature tests/lints and explicit Windows CRT selection. Example sweeps distinguish intended diagnostics from failures.

## Final validation

On Windows x86_64 with Rust 1.99, locked workspace build/tests passed: 899 tests across 54 suites including doctests, no failures or ignored tests. Formatting and strict all-target Clippy passed. The rebuilt LLVM 1.0.6 helper passed 11 feature-enabled backend tests and release Clippy through scripts/package-toolchain.ps1. Both example sweeps passed 42/42 with no skips; LLVM used full LTO. All 72 driver concurrency/backend tests passed with that helper installed. The preceding callable-arity fix added seven metadata/frontend/interpreter/backend test functions. Nine additional functions cover this final operator/builtin follow-up. The pre-fix release CLI fails the same arity fixture; the rebuilt CLI passes it.

Shared.update stress runs cover 1,000 two-task trials and 25,600 parallel-loop updates per AOT backend, plus interpreter threshold-zero/one runs and the runtime thread stress test. The original LOCAL/test_file_manager.fdn class source, with separate save/reload assertions, passed interpreter, Cranelift and LLVM in isolated data directories. One concurrent sweep attempt collided on shared executable paths; serial sweeps passed. A Windows boolean-test cleanup failure was resolved by separating executable artifacts from checked data sandboxes and using distinct backend filenames, retaining all behavior/cleanup assertions. A new presumed-invalid list dictionary key test was corrected after confirming existing structural hashing supports it.

| Changed contract | Frontend / IR evidence | Execution evidence |
| --- | --- | --- |
| Receiver T/K/V, Shared callback signature | Static rejection/acceptance tests; inferred action metadata | Typed result/argument validation in optimized fixture |
| Powers, constants, arithmetic errors | Precise/Dynamic type tests; checked constant evaluation/folding; failing arithmetic retained by DCE | Optimized interpreter, selective JIT/fallback, both AOT fixtures |
| Dynamic operators and indexing | Existing flexible lowering; shared runtime operators/index helpers | Shared release fixture, arithmetic/string/slice fixtures, both AOT sweeps |
| Equality and owned returns | Runtime unit tests; real direct calls protected from inlining | Nested equality and repeated owned-return fixture, both AOT backends |
| Slice canonical formatting | Parser/formatter round-trip tests; ranges/inclusive aliases preserved | Formatter-produced fixture runs on interpreter and both AOT backends; AOT error tests |
| Shared atomicity | Existing parallel/E0401 and metadata tests retained | Repeated shared-value task/parallel-for stress on interpreter and both AOT backends |
| Stdlib callable arity | Single pure config source; every alias/public signature checked; static wrong-call rejection retained | Optimized erased-callable/Shared fixture on interpreter, JIT interaction and both AOT backends |

These tests establish the covered cases, not universal backend parity. Logs and local toolchain artifacts are under ignored target/final-contract-*, target/final-stdlib-* and target/operator-*.

## Standard-library callable arity

The final callback-arity blocker is resolved through the owner-approved low-level metadata placement. `fidan-config::stdlib` holds the eleven existing member tables, names/aliases, raw signatures/docs and pure parameter-shape lookup. The tables are unchanged, with no copied table left in `fidan-stdlib`. That crate re-exports `StdlibMemberInfo`, `member_info` and `module_members`; compiler type inference, signature rendering and runtime implementation/dispatch remain in their existing crates.

Required positional bounds, optional parameters and unbounded variadic tails derive from those descriptors. The existing optional pattern default for `time.format` is preserved. Top-level callable builtins reuse their existing config signatures; the compiler no longer has a separate builtin arity parser. Alias lookup resolves the same canonical member descriptor.

Interpreter stdlib entry and native inline stdlib dispatch call the same runtime validator before implementations can ignore surplus or synthesize missing arguments. Wrong arity reports catchable R0001 with the same message. Native `fdn_call_dynamic` and Shared callbacks reuse this dispatcher. Shared guards remain held across callback invocation; errors leave the slot unchanged. Statically known calls retain frontend diagnostics; erased action/callable/fn/flexible values use runtime validation without a richer function type system.

Cargo confirms the graph is acyclic: config has no dependencies; runtime depends on config and never on stdlib; stdlib retains its existing config/runtime dependencies. No manifest, lockfile, version, protocol or extension change is required. Public metadata/rendering and all aliases are regression-tested; the optimized callable fixture covers zero/one/multiple required arguments, optional/default/variadic calls, aliases, erased callables and Shared failure preservation on interpreter/JIT and both AOT backends.

## Final operator and core-builtin parity follow-up

Frontend E0203 now rejects known non-integer bitwise/range operands, non-Boolean logical operands, non-numeric unary negation, and unsupported ordering pairs. Dynamic/Unknown operands defer validation; equality/inequality and unary-plus identity retain their existing contracts. Ordering accepts numeric pairs (including mixed Integer/Float) and String/String.

All integer shift counts use `rhs & 63`, including negative counts. Cranelift scalar shifts explicitly mask before `ishl`/`sshr`; LLVM uses the masked boxed helpers. Interpreter and constant folding already masked counts. Native dynamic bitwise failures now use the existing R0001 exception slot rather than silent Nothing.

Both AOT backends use a strict boxed range constructor when bounds are dynamic; permissive scalar ABI unboxers remain unchanged. Core integer/float/len contracts are shared in runtime helpers: integer accepts Integer/Float/Boolean/parseable String, float accepts Float/Integer/parseable String, and len accepts String/List/Dict/HashSet/Tuple/Range. Invalid calls report R0001. First-class interpreter builtin invocation now preserves runtime diagnostic codes rather than converting them to panic signals.

Native range errors no longer exit the process. Core len range-length failures retain interpreter R0001; receiver range-method failures retain interpreter R2002. Existing may-throw classification and generated pending-exception checks propagate scalar placeholders before subsequent statements execute. Explicit panic/assert behavior is outside this change.

Nine new test functions cover frontend rejection, optimized interpreter/JIT interaction, both AOT backends, helper exception channels, constant shift masking and callable-alias editor metadata/hover. The shared adversarial fixture includes indirect dynamic operators, all requested shift counts, direct/erased conversions, unsupported lengths and caught full-i64-range failures. Six additional uncaught-error programs per AOT backend verify unsuccessful exit and R0001 output. Callable type aliases action/callable/fn now have central editor metadata without duplicate builtin-function entries. A Windows optimizer-test executable cleanup failure was corrected using the existing separation of ignored binary artifacts from checked data sandboxes; semantic and cleanup assertions remain intact.

## Local release readiness

No required correctness item identified in the previous final-polish report remains unfinished. The branch is locally release-ready for Fidan 1.0.15 with LLVM helper 1.0.6, pending independent review and remote Windows/Linux/macOS CI. No merge, tag or publication is performed. Planned features and the concrete limitations below are not being presented as implemented or silently treated as fixed.

## Intentional remaining limits

- Some temporary scalar/constant/default direct-call argument boxes and interpolation/tuple/capture allocation paths still lack matching cleanup. Broader argument cleanup was rejected by automatic approval review because ownership proof was insufficient. The narrowly proven cleanup does not claim general leak freedom.
- Selective JIT uses interpreter fallback; threshold-one testing is not a claim that every operation is native.
- Native objects use Dict-backed fields/methods, without complete nominal class metadata or universal interpreter/AOT object introspection parity.
- Runtime contracts validate primitives/callables and collection contents; erased nested Shared/Pending and native nominal-object metadata are not a dependent runtime type system.
- Cooperative spawn/await is distinct from parallel OS-thread execution. Cross-Shared callback lock cycles can still deadlock.
- Static call graphs/traces contain unknowns and bounded analysis, not measured execution.
- Windows validation does not replace Linux/macOS CI or external extension validation.
- @gpu, std.net, std.process and a broad Phase 11.2 redesign remain planned.
