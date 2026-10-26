# Global CLI diagnostics

This feature belongs to the upcoming compiler candidate. The previously published
Fidan 1.0.16 binary does not acquire new flags from documentation updates.

| Option | Operational diagnostics |
| --- | --- |
| none | Off; existing output, errors, warnings and progress remain active |
| `-v`, `--verbose` | INFO: operation lifecycle, compilation/execution/backend selection, package and artifact operations |
| `-vv` | DEBUG: frontend counts, phase boundaries, optimizer/build policy, cache/checksum and toolchain decisions |
| `-vvv` | TRACE: nested phase timings, import processing, resolver branches, LLVM transitions and LSP document changes |
| `-V`, `--version` | Version only; independent of verbosity |

Repeat `-v` or `--verbose` to increase the level. Counts add across nested command
levels. More than three CLI occurrences is a usage error (exit 2). Normal compiler
errors, warnings, essential progress and exit codes do not depend on these levels.
The existing `--trace` option controls Fidan runtime stack reports, not these logs.

## Placement and program arguments

```sh
fidan -vv run main.fdn -- --verbose -v -V
fidan run -vv main.fdn -- --verbose
fidan -v build main.fdn --backend cranelift --opt O3
fidan -vv build main.fdn --backend llvm --lto full
fidan dal -v add example-package
fidan toolchain list -vv
fidan -vv check .
fidan -vvv repl
```

`run` and `exec` reserve their trailing arguments for the user program/toolchain
command. Put Fidan verbosity **before the source file or exec namespace**. After
that positional, `-v`, `--verbose` and `-V` belong to the program, even without
`--`. Existing run options such as `--jit-threshold` remain supported after the
file until the program argument tail starts. Use `--` for unequivocal forwarding:

```sh
fidan -vv run main.fdn -- --verbose > result.txt 2> compiler.log
```

Program stdout remains byte-for-byte program output. Formatter output, explicit
IR output, profile JSON files, helper JSON and LSP JSON-RPC retain their existing
channels. Operational diagnostics use stderr only, with no ANSI sequences, even
in a terminal. Existing terminal-aware language-diagnostic rendering is separate.

## Environment and filtering

Previously the compiler did not initialize a RUST_LOG subscriber. The candidate
supports `RUST_LOG=debug`, or first-party target directives such as
`RUST_LOG=fidan::pipeline=debug,fidan_driver=trace`. Explicit CLI verbosity wins
over RUST_LOG entirely, including an invalid RUST_LOG value. Without flags, an
invalid filter is a startup error; it is never silently accepted. Accepted levels
are `off,error,warn,info,debug,trace`. Targets must be `fidan`, `fidan::*`, or
`fidan_*`. Field/regex filters and third-party targets are deliberately unsupported;
`-vvv` never enables HTTP, TLS, Tokio or Cranelift dependency trace floods.

The LLVM helper receives the resolved filter through a private child-process
environment entry, not a wire-protocol change. Its diagnostics stay on stderr;
its stdout remains the compile-response JSON. Existing explicit LLVM developer
IR-dump switches are separate and are never enabled by verbosity.

## LSP and embedding

`fidan -vvv lsp --stdio` keeps stdout exclusively for JSON-RPC. In the official
VS Code extension, set `fidan.server.extraArgs` to `["-vvv"]` and inspect the
language-server output channel. This requires the new compiler; normal editing
does not require verbose logging or AI configuration. `fidan.trace.server`
continues to control client-side protocol tracing independently.

Libraries emit structured tracing events but install no subscriber. The shared
`fidan_diagnostics::logging::subscriber(configuration, writer)` builds an optional
redacting subscriber with a caller-provided sink; embedded hosts may instead use
their own tracing subscriber/scoped dispatcher. The CLI and helper entry points
own installation and report an already-installed subscriber as a startup error.

## Privacy and diagnostic bounds

Operational events omit source text, runtime values, argument lists, request or
response bodies, HTTP headers, cookies and environment contents. Credential
acquisition registers explicit/keychain credentials for redaction. CLI startup
also registers values of credential-related environment variables. The final
formatter redacts registered values, credential-labelled fields and URL paths,
userinfo, queries and fragments. It escapes control characters and omits oversized
records rather than emitting a potentially truncated credential. Individual records
are limited to 8 KiB before redaction; no instruction-by-instruction VM logging is
added. TRACE volume still scales with modules, resolver branches and compilations.

Operational errors from HTTP/native tools pass through the same redaction boundary.
Existing source diagnostics, explicit IR output and application-defined runtime
messages keep their language contracts; they are not new tracing records.
User-program output is intentionally not redacted. Inspect logs before sharing:
local paths, package identities and compilation structure can still be private.
An embedding application's own subscriber is responsible for its output policy.
