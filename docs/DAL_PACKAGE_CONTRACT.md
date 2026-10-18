# Dal package contract — local relaunch candidate

This describes source on the relaunch audit branch, not capabilities retroactively
added to published Fidan 1.0.16 binaries. Release versions remain unchanged locally.

```toml
[package]
name = "example"
version = "0.1.0"
fidan = ">=1.0.16, <2.0.0"
```

`package.fidan` is an optional SemVer compiler requirement, independent of dependency
version requirements. Missing means legacy/unconstrained. Invalid constraints fail
validation. Candidates incompatible with the running compiler are skipped; locked,
cached and installed package manifests are checked again. SemVer prerelease matching
uses the same semver crate contract as ordinary requirements: prereleases require an
explicit compatible prerelease comparator. Build metadata does not change precedence.
The example range is syntax documentation, not a certified minimum for `args`.

The client validates sparse-index identity, version, compiler requirement and a strict
64-hex SHA-256, then verifies bounded downloaded bytes **before extraction**. Archives
allow only portable regular files/directories in the canonical package root. Limits:
50 MiB compressed, 256 MiB expanded, expansion <=100:1, 10,000 entries, depth <=64,
manifest/lock <=1 MiB. Index and JSON responses are bounded to 8 MiB; error text to
16 KiB. HTTPS is required except literal HTTP loopback development URLs.

Resolution is deterministic bounded backtracking (128 packages, 10,000 decisions,
256 MiB candidate-byte cache), intersects transitive requirements and unifies enabled
features. Exhausting a budget reports an error, not a proof of unsatisfiability.
Unsupported git/rev sources fail explicitly. `optional=true` dependencies and
`[optional-dependencies]` activate through existing feature `dep:name` references;
feature cycles fail and deep chains do not recurse on the Rust stack.

Schema-1 locks preserve optional checksum/compiler fields for legacy reading. New
resolved locks record these fields. New resolutions exclude yanked versions. Existing
exact checksum-bearing locks may retain a yanked version only with the identical index
checksum. Immutable downloads remain accessible; yank never deletes artifacts.
Publisher-embedded locks do not override the consumer's selected transitive graph.

Installations validate in a private staging directory and rename on success. Existing
cache contents are checked against verified archive bytes; force replacement retains a
backup until success. Per-root file locks serialize concurrent CLI metadata changes.
Manifest and lock writes are individually atomic; if the second write fails, the first
is restored. These two files are not a crash-atomic database transaction: after power
loss between replacements, inspect any retained backup and rerun add against the
verified graph. Do not delete backups before recovery. Same-user hostile filesystem
races require operating-system isolation, beyond these path/symlink checks.

For process arguments, both forms are supported and regression-tested:

```sh
fidan run main.fdn -- --name Dal
fidan run main.fdn --name Dal
```

Run compiler options **before** the source path, e.g. `fidan run --sandbox main.fdn`.
Options after the source are program arguments. Environment token overrides allow
isolated automation without writing OS keychain state. Never put real tokens in logs.

## Necessary compiler integration fixes and release order

Real package execution exposed file-scoped action/global/re-export collisions and
nullable/Dynamic native representation defects. The fixes retain source-language
contracts. Boxed if/while conditions execute natively in the JIT using the same
fdn_truthy helper as AOT. Functions using other unsupported instructions still use
the established selective-JIT fallback; no coverage percentage is claimed.
LLVM helper protocol is now **6**, because
old protocol-5 helpers cannot safely consume the new module/export representation.
The existing exact compatibility gate rejects them rather than silently miscompiling.
AI helper protocol is unchanged. LLVM 23.1.2/llvm-sys 231 and Cranelift 0.136 are unchanged.

A future owner-approved compiler patch and matching LLVM helper patch must be released
before publishing `args`; choose their versions and tighten its compiler minimum first.
Do not replace immutable Fidan 1.0.16 or helper 1.0.6 artifacts. Private audit helper
`1.0.6-audit.3` is local-only evidence, not a published release.

Cross-repository manifest/lock fixtures live under `test/fixtures/dal/`; actual CLI
loopback regressions are in `crates/fidan-cli/tests/dal_registry.rs`. Full local
API/auth/storage/worker/browser lifecycle is maintained in the sibling Dal repository.
