<div align="center">

<p align="center">
  <img src="https://github.com/fidan-lang/fidan/blob/2c03644d2047e2bc1a42b301ff1df9c8e475a262/assets/icons/icon.png" width="40%" alt="Fidan Banner">
</p>

# Fidan

**A modern, expressive, human-readable programming language built for clarity, safety, and real-world performance.**

[![License](https://img.shields.io/badge/license-Apache%202.0%20%2B%20Fidan%20Terms-blue.svg)](LICENSE) &nbsp; ![Build](https://img.shields.io/badge/build-passing-brightgreen.svg) &nbsp; ![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg) &nbsp; [![VS Code Extension](https://img.shields.io/badge/VS%20Code-Extension%20Available-007ACC.svg)](editors/vscode)

[Getting Started](#-getting-started) • [Language Tour](#-language-tour) • [CLI Reference](#-cli-reference) • [Standard Library](#-standard-library) • [VS Code Extension](#-vs-code-extension) • [Contributing](#-contributing)

</div>

---

## What is Fidan?

Fidan is a general-purpose programming language that prioritizes **human readability without sacrificing power**. It reads almost like English, yet compiles to native code and runs real threads. It is statically typed with full inference, null-safe by design, and ships a complete toolchain — formatter, linter, fixer, LSP server, REPL, and both JIT and AOT compilers — out of the box.

```fidan
object Person extends Creature {
    var name oftype string
    var age  oftype integer

    new with (certain name oftype string, optional age oftype integer = 18) {
        this.name = name
        this.age  = age
        parent(species set "Human")
    }

    action introduce returns nothing {
        print("My name is {this.name} and I am {this.age} years old!")
    }
}

action main {
    var john = Person("John" also 20)
    john.introduce()
    print("Hello, {john.name}!")
}

main()
```

---

## Why Fidan?

Most languages make a trade-off: either **readable** (Python) or **fast** (C++/Rust) or **safe** (Rust) — but rarely all three at once without significant ceremony. Fidan's goal is to hit all three without requiring you to understand lifetimes, borrow checkers, or cryptic syntax.

| Goal | How Fidan achieves it |
|---|---|
| **Readable code** | English-like syntax (`and`, `or`, `not`, `is`, `certain`, `otherwise when`…) |
| **Safety without ceremony** | Null-safety analysis, `certain` non-null guarantees, data-race detection at compile time |
| **Performance** | MIR-level optimization passes + Cranelift JIT (`@precompile`, auto hot-path) + AOT, LLVM on the roadmap |
| **Real concurrency** | `parallel`, `concurrent`, `spawn`/`await` — backed by real OS threads, not green threads |
| **Great tooling** | Formatter, linter, fixer, REPL, LSP, VS Code extension — all built-in, not plugins |
| **Reproducible debugging** | `--replay` captures stdin and replays crashes exactly |
| **Readable errors** | Ariadne-rendered diagnostics with source context, inline carets, fix-it patches |

---

## Feature Comparison

| Feature | Fidan | Python | TypeScript | Go | Rust |
|---|:---:|:---:|:---:|:---:|:---:|
| English-like readable syntax | ✅ | ⚠️ partial | ❌ | ❌ | ❌ |
| Static typing + full inference | ✅ | ❌ | ✅ | ✅ | ✅ |
| Null safety (compile-time) | ✅ | ❌ | ⚠️ opt-in | ⚠️ partial | ✅ |
| `certain` non-null parameter contract | ✅ | ❌ | ❌ | ❌ | ❌ |
| Data-race detection (compile-time) | ✅ | N/A | N/A | ❌ | ✅ |
| Real OS thread parallelism | ✅ | ❌ (GIL) | ❌ | ✅ | ✅ |
| Built-in `spawn`/`await` model | ✅ | ⚠️ asyncio | ✅ | ✅ goroutines | ✅ |
| JIT compilation (`@precompile`) | ✅ | ❌ | ❌ | ❌ | ❌ |
| Built-in formatter | ✅ | ❌ (black) | ❌ (prettier) | ✅ | ✅ |
| Built-in linter + auto-fixer | ✅ | ❌ (ruff) | ❌ | ❌ | ⚠️ clippy |
| Built-in REPL | ✅ | ✅ | ❌ | ❌ | ❌ |
| Built-in LSP server | ✅ | ❌ (pylsp) | ✅ (tsserver) | ❌ | ❌ (rust-analyzer) |
| Replay-based crash reproduction | ✅ | ❌ | ❌ | ❌ | ❌ |
| `explain-line` static analysis | ✅ | ❌ | ❌ | ❌ | ❌ |
| First-class test blocks | ✅ | ❌ (unittest) | ❌ (jest) | ✅ | ✅ |
| Hot reload (`--reload`) | ✅ | ❌ | ❌ | ❌ | ❌ |
| String synonyms (`is`, `equals`, `and`, …) | ✅ | ❌ | ❌ | ❌ | ❌ |
| `check` pattern matching | ✅ | ❌ (match 3.10+) | ❌ | ❌ | ✅ |
| Multi-line comments (nested) | ✅ | ❌ | ❌ | ❌ | ❌ |

---

## Getting Started

### Prerequisites

- **Rust toolchain** (1.82+): [rustup.rs](https://rustup.rs)

### Build from source

```bash
git clone https://github.com/fidan-lang/fidan.git
cd Fidan
cargo build --release
```

The `fidan` binary will be at `target/release/fidan`. Add it to your `PATH`:

```bash
# Linux / macOS
export PATH="$PWD/target/release:$PATH"

# Windows (PowerShell)
$env:PATH = "$PWD\target\release;" + $env:PATH
```

### Verify installation

```bash
fidan --version
```

---

## Quick Start

Create a file `hello.fdn`:

```fidan
print("Hello, world!")
```

Run it:

```bash
fidan run hello.fdn
```

---

## Language Tour

### Variables and types

```fidan
var name    = "Alice"             # inferred: string
var age     = 30                  # inferred: integer
var score   = 9.5                 # inferred: float
var active  = true                # inferred: boolean
var nothing_here = nothing        # nothing (null)

var count oftype integer          # declared, not yet assigned — defaults to nothing
var pi oftype float = 3.14159     # explicit type + value
```

Type inference is full and bidirectional. Explicit `oftype` annotations are optional but always respected.

---

### Actions (functions)

```fidan
action greet with (certain name oftype string) returns string {
    return "Hello, {name}!"
}

print(greet("Fidan"))  # Hello, Fidan!
```

- `certain` means the parameter must be non-null at call time — enforced at compile time
- `optional` (default) parameters can be `nothing`
- Parameters can use `also` or `,` as separators

Named arguments work at every call site:

```fidan
action create_user with (certain name oftype string, optional age oftype integer = 18) {
    print("{name} is {age} years old")
}

create_user(name set "Alice", age = 25)   # both forms work
create_user("Bob")                         # positional, age defaults to 18
```

---

### Objects and inheritance

```fidan
object Animal {
    var sound oftype string

    new with (certain sound oftype string) {
        this.sound = sound
    }

    action speak returns string {
        return "I say: {this.sound}"
    }
}

object Dog extends Animal {
    var name oftype string

    new with (certain name oftype string) {
        this.name = name
        parent(sound set "Woof")
    }

    action fetch returns nothing {
        print("{this.name} fetches the ball!")
    }
}

var rex = Dog("Rex")
print(rex.speak())   # I say: Woof
rex.fetch()          # Rex fetches the ball!
```

Extension actions let you add methods to existing objects without modifying them:

```fidan
action bark extends Dog {
    print("{this.name} goes WOOF WOOF!")
}

rex.bark()
```

---

### Control flow

```fidan
# if / otherwise when / else
if age greaterthan 18 {
    print("Adult")
} otherwise when age equals 18 {
    print("Just turned 18!")
} else {
    print("Minor")
}

# Synonyms: use whichever reads most naturally
if score > 9 and score <= 10 {
    print("Excellent")
}
```

Ternary expressions:

```fidan
var label = "pass" if score >= 5 else "fail"

# Implicit-subject shorthand (Fidan-specific)
var display = person if is not nothing else defaultPerson
# Equivalent to:
var display = person if (person != nothing) else defaultPerson
# Or simply:
var display = person ?? defaultPerson    # null-coalescing operator
```

---

### Loops

```fidan
# for with range (exclusive)
for i in 1..10 {
    print(i)
}

# for with inclusive range
for i in 1...10 {
    print(i)
}

# for over a list
var fruits = ["apple", "banana", "cherry"]
for fruit in fruits {
    print(fruit)
}

# while
var n = 0
while n < 5 {
    n += 1
}
```

---

### Check (pattern matching)

```fidan
var code = 404

check code {
    200 => print("OK")
    404 => print("Not found")
    500 => print("Server error")
    _   => print("Unknown code: {code}")
}

# Inline check expression
var message = check code {
    200 => "OK"
    404 => "Not found"
    _   => "Other"
}
```

---

### String interpolation

```fidan
var name = "Fidan"
var version = 1

print("Welcome to {name} v{version}!")       # simple variable
print("2 + 2 = {2 + 2}")                     # expression
print("{name.upper()} is awesome!")           # method call
print("Pi is approximately {floor(3.14159)}") # function call
```

Nested multi-line comments are fully supported:

```fidan
#/
    This is a comment.
    #/ And this is nested. /#
    Still in the outer comment.
/#
```

---

### Error handling

```fidan
attempt {
    var data = readFile("config.json")
    print(data)
} catch error {
    print("Failed: {error}")
} otherwise {
    print("File read successfully.")    # runs only when no error
} finally {
    print("Cleanup always runs.")
}
```

You can annotate the error type:

```fidan
attempt {
    riskyOperation()
} catch error -> string {
    print("Got a string error: {error}")
}
```

---

### Concurrency and parallelism

Fidan has three concurrency models, backed by real OS threads:

#### `spawn` / `await` — explicit async

```fidan
action fetch_data with (certain url oftype string) returns string {
    # ... HTTP call ...
    return "data from {url}"
}

var handle1 = spawn fetch_data("https://api.example.com/users")
var handle2 = spawn fetch_data("https://api.example.com/posts")

var users = await handle1
var posts = await handle2
print("Got {users} and {posts}")
```

#### `parallel` block — run tasks simultaneously

```fidan
parallel {
    task { heavyComputation() }
    task { processBigFile("data.csv") }
    task { renderChart() }
}
# All three tasks ran in parallel — execution continues here when all finish
```

#### `parallel for` — parallel iteration

```fidan
parallel for item in largeDataset {
    processItem(item)   # each item processed on a separate thread
}
```

#### `concurrent` block — cooperative I/O-bound tasks

```fidan
concurrent {
    task { readFromDatabase() }
    task { callExternalAPI() }
}
```

#### `Shared` — thread-safe shared state

```fidan
var counter = Shared(0)

parallel for i in 1..100 {
    counter.update(action with (val) { return val + 1 })
}

print(counter.get())   # 99 (safe, no data races)
```

The compiler enforces this: writing to a non-`Shared` variable from a `parallel` block is a **compile-time error (E0401)**.

---

### Null safety

```fidan
action divide with (certain a oftype integer, certain b oftype integer) returns float {
    return a / b
}

divide(10, 0)    # runtime panic — but never a null pointer crash
divide(nothing, 5)  # compile-time error — `certain` blocks this
```

Variables default to `nothing`. The null-safety pass (`W2006`) warns whenever a possibly-null value is used in an unsafe context:

```fidan
var name oftype string   # nothing by default
print(name.upper())      # W2006: `name` may be nothing here
```

---

### List and dict comprehensions

```fidan
var numbers = [1, 2, 3, 4, 5]

var doubled = [x * 2 for x in numbers]             # [2, 4, 6, 8, 10]
var evens   = [x for x in numbers if x % 2 == 0]  # [2, 4]
var squares = [x * x for x in 1..6]               # [1, 4, 9, 16, 25]

# Dict comprehension
var sq_map = {x: x * x for x in numbers}          # {"1": 1, "2": 4, ...}
var filtered_map = {x: x * 2 for x in numbers if x > 2}
```

---

### Built-in test blocks

No test framework to install. Tests live in your source files:

```fidan
action add with (certain a oftype integer, certain b oftype integer) returns integer {
    return a + b
}

test "basic addition" {
    assert(add(2, 3) == 5)
    assert_eq(add(0, 0), 0)
    assert_ne(add(1, 1), 3)
}

test "negative numbers" {
    assert_eq(add(-5, 5), 0)
    assert(add(-10, -10) < 0)
}
```

Run them with `fidan test yourfile.fdn`. Each test reports pass/fail with coloured output.

---

### Decorators

```fidan
@precompile        # JIT-compile this action at startup (eager, not lazy)
action fibonacci with (certain n oftype integer) returns integer {
    if n <= 1 { return n }
    return fibonacci(n - 1) + fibonacci(n - 2)
}

@deprecated("Use fibonacci2 instead")
action fibonacci_old with (certain n oftype integer) returns integer {
    # ...
}
```

---

### Imports and modules

```fidan
# Standard library modules
use std.io
use std.math
use std.math.{sqrt, floor, ceil}
use std.collections

# User modules (file-relative)
use mymodule              # → mymodule.fdn or mymodule/init.fdn
use utils.helpers         # → utils/helpers.fdn

# Aliased import
use "./other.fdn" as other

# Re-export
export use std.math       # consumers of this module also get std.math
```

---

### JIT compilation

Hot functions are automatically JIT-compiled by Cranelift after a configurable number of calls (default: 500). You can force eager compilation:

```fidan
@precompile
action hot_inner_loop with (certain n oftype integer) returns integer {
    var sum = 0
    for i in 0..n { sum += i }
    return sum
}
```

Or tune from the CLI:

```bash
fidan run app.fdn --jit-threshold 100    # compile after 100 calls
fidan run app.fdn --jit-threshold 0     # disable JIT entirely
```

---

## CLI Reference

```
fidan <COMMAND> [OPTIONS] [FILE]
```

| Command | Description |
|---|---|
| `fidan run <file>` | Run a Fidan source file |
| `fidan build <file>` | Compile to a native binary |
| `fidan check <file>` | Type-check and lint without running |
| `fidan fix <file>` | Auto-apply high-confidence fixes |
| `fidan format <file>` | Format source code |
| `fidan test <file>` | Run inline `test {}` blocks |
| `fidan profile <file>` | Run with profiling output |
| `fidan repl` | Start the interactive REPL |
| `fidan lsp` | Start the Language Server (used by editors) |
| `fidan explain <code>` | Show detailed explanation of a diagnostic code |
| `fidan explain-line <file> --line N` | Explain what line N does (static analysis) |
| `fidan new <name>` | Scaffold a new Fidan project |

### `fidan run` flags

| Flag | Description |
|---|---|
| `--reload` | Watch source files and re-run on change |
| `--strict` | Treat select warnings as errors |
| `--trace short\|full\|compact` | Show call stack on panic |
| `--jit-threshold N` | JIT after N calls (0 = off) |
| `--sandbox` | Deny all I/O, env, net, spawn access by default |
| `--allow-read <paths>` | Whitelist read paths (sandbox mode) |
| `--allow-write <paths>` | Whitelist write paths (sandbox mode) |
| `--allow-env` | Allow environment variable access (sandbox) |
| `--allow-net` | Allow network access (sandbox) |
| `--allow-spawn` | Allow subprocess spawning (sandbox) |
| `--time-limit <secs>` | Hard wall-time limit |
| `--mem-limit <mb>` | Hard memory limit |
| `--max-errors N` | Stop after N errors |
| `--suppress W1005,W2006` | Suppress specific diagnostic codes |
| `--emit tokens\|ast\|hir\|mir` | Dump an intermediate representation |
| `--replay <id\|path>` | Replay a captured crash scenario |

### `fidan build` flags

| Flag | Description |
|---|---|
| `--output <path>` | Output binary path |
| `--release` | Enable full optimizations |
| `--emit tokens\|ast\|hir\|mir` | Dump intermediate representation |

### Hot reload

```bash
fidan run app.fdn --reload
# [↻ reload] app.fdn changed — re-running
```

Any `.fdn` file in the same directory triggers a re-run. Useful during development.

### Replay-based crash reproduction

When your program crashes and it read from stdin, Fidan saves the input sequence:

```
error[R0001]: division by zero
  → replay.fdn:7

  hint: fidan run replay.fdn --replay a3f82c91
```

Run that command to reproduce the exact crash, every time, without re-typing inputs.

### REPL

```bash
fidan repl
>>> var x = 10
>>> x * x
100
>>> :type x * x
: integer
>>> :last
R0001: division by zero at line 3
>>> :last --full
... full error history ...
```

The REPL supports multi-line input, continuation prompts (`...`), `:cancel` to abort a block, and `:type <expr>` to inspect inferred types.

### `explain-line` — static analysis on demand

```bash
fidan explain-line app.fdn --line 42
fidan explain-line app.fdn --line 10 --end-line 20
```

For each line in range, Fidan reports:
- **What it does** (plain-English description)
- **Inferred type** of the expression
- **Reads** (variables accessed)
- **Writes** (variables modified)
- **Could go wrong** (division by zero, out-of-bounds index, overflow…)

---

## Diagnostic System

Fidan's diagnostics are designed to be read, not feared.

<!-- PLACEHOLDER: screenshot of a Fidan diagnostic -->
![Diagnostic example](https://via.placeholder.com/800x300/1a1a2e/e0e0ff?text=Fidan+diagnostic+screenshot)

Every diagnostic includes:
- **Source context** — the offending line(s) with line numbers
- **Inline caret** — exactly which token is wrong and why
- **Fix-it patch** — a green `+` block showing the corrected version where applicable
- **Cause chain** — if error A caused error B, both are shown linked
- **`fidan fix`** — automatically applies all high-confidence patches

Diagnostic codes:

| Prefix | Category | Example |
|---|---|---|
| `E01xx` | Undefined names / scoping | `E0101` unknown name |
| `E02xx` | Type mismatches | `E0201` type mismatch on assignment |
| `E03xx` | Call errors | `E0301` missing required argument |
| `E04xx` | Parallelism / data race | `E0401` shared write in parallel block |
| `E02xx` | Null safety | `E0205` non-null operand used as null |
| `W10xx` | Code quality | `W1005` unused import |
| `W20xx` | Null safety warnings | `W2006` possibly-null dereference |
| `W50xx` | Performance hints | `W5001` dynamic type in hot loop |
| `R9xxx` | Runtime errors | `R9001` parallel task failed |

```bash
fidan explain E0401    # print a full page of documentation for this code
```

---

## Standard Library

| Module | What it provides |
|---|---|
| `std.io` | File I/O, environment, console, directory listing |
| `std.math` | `sqrt`, `abs`, `floor`, `ceil`, `round`, `pow`, `log`, trig, `PI`, `E`, `clamp` |
| `std.string` | `toUpper`, `toLower`, `trim`, `split`, `join`, `replace`, `startsWith`, `endsWith`, `pad`, `repeat` |
| `std.collections` | `Queue`, `Stack`, `Set`, `OrderedDict` with full method sets |
| `std.test` | `assertEqual`, `assertNotEqual`, `assertTrue`, `assertFalse`, `assertContains`, `fail`, and more |
| `std.parallel` | `parallelMap`, `parallelFilter`, `parallelForEach`, `parallelReduce` |
| `std.time` | `sleep`, timestamps, duration helpers |

Usage:

```fidan
use std.math.{sqrt, PI}
use std.collections

var stack = Stack()
stack.push(1)
stack.push(2)
print(stack.pop())   # 2

print(sqrt(144))     # 12.0
print(PI)            # 3.141592653589793
```

---

## VS Code Extension

<!-- PLACEHOLDER: screenshot of extension in action -->
![VS Code Extension](https://via.placeholder.com/800x400/1a1a2e/e0e0ff?text=VS+Code+Extension+screenshot)

Install from the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=fidan.fidan) *(coming soon)* or build locally:

```bash
git clone https://github.com/fidan-lang/fidan-editors.git
cd fidan-editors/vscode
npm install
npm run compile
# Press F5 to launch the Extension Development Host
```

**Features:**

| Feature | Status |
|---|---|
| Syntax highlighting (TextMate grammar) | ✅ |
| Semantic token highlighting | ✅ |
| Error and warning diagnostics (LSP) | ✅ |
| Hover documentation (type + declaration) | ✅ |
| Auto-completion (dot-trigger, named args, cross-module) | ✅ |
| Signature help | ✅ |
| Go to definition | ✅ |
| Find all references | ✅ |
| Rename symbol | ✅ |
| Format on save | ✅ |
| Inlay hints (inferred types) | ✅ |
| Code actions / fix-it patches | ✅ |
| Folding ranges | ✅ |
| Document outline | ✅ |
| 19 built-in code snippets | ✅ |
| Bracket / comment auto-close | ✅ |
| All CLI commands in Command Palette | ✅ |
| Debug adapter | 🔜 planned |

**Commands available in the Command Palette (`Ctrl+Shift+P`):**

| Command | Description |
|---|---|
| `Fidan: Run Current File` | Run the open file (with mode picker: once / reload) |
| `Fidan: Check File` | Type-check and lint (with strict mode option) |
| `Fidan: Fix File` | Apply fixes (apply or dry-run preview) |
| `Fidan: Format Current File` | Format via LSP |
| `Fidan: Build File` | Build binary (debug or release) |
| `Fidan: Run Tests in Current File` | Run `test {}` blocks |
| `Fidan: Profile Current File` | Profile execution |
| `Fidan: Explain Diagnostic Code` | Prompt for a code → `fidan explain` |
| `Fidan: Explain Current Line(s)` | Selection-aware `explain-line` |
| `Fidan: New Project` | Scaffold project with folder picker |
| `Fidan: Open REPL` | Open the interactive REPL |
| `Fidan: Restart Language Server` | Restart LSP |

---

## Architecture

Fidan is written entirely in Rust and organized as a Cargo workspace of 17 focused crates:

```
Source Text → Lexer → Parser → AST
                                 ↓
                    Type Checker + Symbol Resolution
                                 ↓
                         HIR  (typed, desugared)
                                 ↓
                    MIR  (SSA / control-flow graph)
                                 ↓
              Optimization Passes (constant folding, inlining,
                copy propagation, DCE, unreachable pruning)
                                 ↓
           ┌─────────────────────┼──────────────────────┐
     Interpreter            Cranelift JIT          LLVM AOT
   (always works)         (@precompile, hot        (fidan build
                           functions ≥ N calls)    --release)
```

The same MIR feeds all three backends — no behavioral divergence between run modes.

| Crate | Role |
|---|---|
| `fidan-source` | `SourceFile`, `Span`, `SourceMap` |
| `fidan-lexer` | Tokenizer, synonym normalization |
| `fidan-ast` | All AST node types, arena allocator |
| `fidan-parser` | Recursive-descent + Pratt expression parser |
| `fidan-typeck` | Symbol tables, type inference, null-safety, data-race detection |
| `fidan-hir` | Typed, desugared high-level IR |
| `fidan-mir` | SSA-form mid-level IR, CFG |
| `fidan-passes` | Optimization and analysis passes |
| `fidan-diagnostics` | Diagnostic types, ariadne rendering, fix engine |
| `fidan-runtime` | Value model, COW collections, object model, `Shared<T>` |
| `fidan-interp` | MIR tree-walking interpreter |
| `fidan-codegen-cranelift` | Cranelift JIT backend |
| `fidan-codegen-llvm` | LLVM AOT backend *(planned)* |
| `fidan-stdlib` | Rust-backed standard library |
| `fidan-fmt` | Canonical source formatter |
| `fidan-lsp` | Full LSP server |
| `fidan-cli` | `fidan` binary — all subcommands |

---

## Roadmap

| Milestone | Status |
|---|---|
| Lexer + Parser | ✅ Complete |
| Type checker + null safety + data-race detection | ✅ Complete |
| HIR + MIR lowering | ✅ Complete |
| MIR optimization passes | ✅ Complete |
| Interpreter (tree-walking + MIR) | ✅ Complete |
| Real OS thread parallelism (`parallel`, `spawn`/`await`) | ✅ Complete |
| Cranelift JIT (`@precompile`, auto hot-path) | ✅ Complete |
| Standard library (`std.io`, `std.math`, `std.string`, `std.collections`, `std.test`, `std.parallel`) | ✅ Complete |
| Full LSP server | ✅ Complete |
| VS Code extension | ✅ Complete |
| Hot reload (`--reload`) | ✅ Complete |
| Replay-based crash reproduction (`--replay`) | ✅ Complete |
| LLVM AOT backend (`fidan build --release`) | ⬜ Not started |
| Package manager | 🔜 Planned |
| Debug adapter (VS Code breakpoints) | 🔜 Planned |
| Playground (browser WASM) | 🔜 Planned |

---

## Contributing

Contributions are welcome! By submitting a pull request or patch you acknowledge that you have read [CONTRIBUTING.md](CONTRIBUTING.md) — your contribution becomes part of the Fidan project under its license terms.

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes and ensure `cargo test --workspace` passes
4. Open a pull request with a clear description

Please sign the CLA before your first PR is merged (the bot will prompt you automatically).

**Development setup:**

```bash
# Build all crates
cargo build

# Run all tests
cargo test --workspace

# Run a specific example
cargo run -- run test/examples/test.fdn

# Run with MIR dump
cargo run -- run test/examples/test.fdn --emit mir

# Build the VS Code extension
git clone https://github.com/fidan-lang/fidan-editors.git
cd fidan-editors/vscode && npm install && npm run compile
```

---

## License

The Fidan programming language — including its source code, compiler, interpreter, runtime, and official distributions — is licensed under the **Apache License 2.0 with Fidan Additional Terms**.

**Key points**:
- ✅ You can use Fidan to write and ship programs — those programs are entirely yours
- ✅ You can contribute to this repository
- ✅ You can study and read the source
- ❌ You cannot commercially redistribute or sell the Fidan compiler/runtime as a competing product
- ❌ You cannot use the Fidan™ name or logo for derivative languages without permission

See [LICENSE](LICENSE) for the full text.

**Fidan™ is a trademark of Kaan Gönüldinc (AppSolves).**

---

<div align="center">

<br>

Made with ❤️ by [Kaan Gönüldinc (AppSolves)](https://github.com/AppSolves).

[⭐ Star this repo](https://github.com/fidan-lang/fidan) if you find Fidan interesting!

</div>
