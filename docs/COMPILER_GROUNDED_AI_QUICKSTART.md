# Compiler-grounded AI: a reproducible Fidan quickstart

Fidan exposes compiler-derived diagnostics, inferred types, source-level symbol relationships, and static trace information to its developer tools. You can try the deterministic analysis **without an AI provider or API key**. Model-assisted explain/fix/improve commands use the optional `ai-analysis` helper and a provider you configure.

This walkthrough uses a [real repository example](../test/examples/check_val.fdn) rather than a fabricated model transcript. It can be run using the published Fidan CLI from a checkout of this repository.

## 1. Install Fidan

Follow the [published installation instructions](../README.md#getting-started) for Windows, macOS, or Linux and verify that `fidan --version` works. Then:

```bash
git clone https://github.com/fidan-lang/fidan.git
cd fidan
fidan run test/examples/check_val.fdn
```

The example selects a weekday from a `check` expression and prints `Wednesday`.

## 2. Ask the compiler, without AI

```bash
fidan check test/examples/check_val.fdn
fidan explain test/examples/check_val.fdn --line 2
```

`check` performs deterministic validation; `explain` selects source-level context from the compiler's analysis. Exact details depend on the source construct and compiler version. This stage requires **no model, no login, and no AI credits**.

## 3. Optional: ask a model using compiler context

Install the separately versioned AI analysis toolchain and configure a provider. This does not change the compiler version:

```bash
fidan toolchain add ai-analysis --version 1.0.6
fidan exec ai setup
fidan exec ai doctor
fidan explain test/examples/check_val.fdn --line 2 --ai "Explain how this expression selects a branch"
```

You can configure an OpenAI-compatible endpoint, Anthropic, Ollama, or LM Studio. Local endpoints do not require a commercial API key, but a local model must be running. Remote providers may incur charges, and source/context may be sent to them. Read the configuration prompts before proceeding.

The model receives structured compiler context alongside source text. **A compiler-provided fact is not a proof that every model explanation is correct.** Verify its claims against the code and compiler output.

## 4. Optional: explore fixes without modifying files

Create a separate file named `broken.fdn` containing:

```fidan
var count = 41
print(completely_unknown_xyz)
```

Then run:

```bash
fidan check broken.fdn
fidan fix broken.fdn --ai
```

`check` should report an undefined symbol. With a configured model, `fix --ai` may propose changes, but **the model is not guaranteed to produce a useful fix**. Without `--in-place`, the command is for review. Avoid treating compiler validation of a suggested fix as a behavioral correctness proof.

## 5. MCP integration

```bash
fidan exec ai mcp
```

This is a **stdio MCP server** for a configured client, not an interactive terminal chat. It provides compiler-grounded tools for diagnostics, symbol information, types, call relationships, static traces, and fix previews. Configure your MCP client to spawn the command; do not expose the helper as an unauthenticated network service.

## What this does not claim

- **Not a live debugger:** `get_runtime_trace` reports a static, source-level trace, not sampled values, live stack frames, or changing scopes during execution.
- **Not automatic correctness:** the compiler can validate diagnostics/type state but does not prove model edits preserve behavior.
- **Not universal backend parity:** interpreter, selective JIT, Cranelift AOT, and optional LLVM AOT have different implementation coverage.
- **Not zero-cost AI:** deterministic tools are free of model calls, but configured external AI providers may charge.

For details, see [AI-native tooling](../README.md#ai-native-tooling) and the [CLI reference](../README.md#cli-reference).
