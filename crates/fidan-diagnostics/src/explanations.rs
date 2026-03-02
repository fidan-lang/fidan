//! Long-form explanations for Fidan diagnostic codes.
//!
//! Each entry provides:
//!   - A prose explanation of what caused the error.
//!   - A minimal erroneous code example.
//!   - A corrected version (where applicable).
//!
//! Retrieved via [`explain`].  Returns `None` for unknown codes so callers
//! can fall back to the short title from [`crate::codes::lookup`].

use crate::DiagCode;

/// Return the long-form explanation text for a diagnostic code, or `None`
/// if no extended text is registered.
///
/// Accepts a [`DiagCode`] value, which is guaranteed to be a registered code.
pub fn explain(code: DiagCode) -> Option<&'static str> {
    match code.0 {
        // ── Syntax / parse ────────────────────────────────────────────────────
        "E0000" => Some(
            r#"The parser encountered a token it did not expect at this position.
This can happen when a keyword, operator, or delimiter is used in the
wrong context, or when valid punctuation is accidentally omitted.

Erroneous example:

    action greet {
        var message =          # missing value after `=`
        print(message)
    }

Fix: supply the missing value or correct the surrounding syntax:

    action greet {
        var message = "hello"
        print(message)
    }
"#,
        ),

        "E0001" => Some(
            r#"A string literal was opened with `"` but never closed before the end
of the line or file.

Erroneous example:

    var greeting = "Hello, world!

Fix: close the string with a matching `"`:

    var greeting = "Hello, world!"
"#,
        ),

        // ── Name resolution ───────────────────────────────────────────────────
        "E0100" => Some(
            r#"An `object` declaration uses `extends` to inherit from another object,
but the named parent does not exist in the current scope.

Erroneous example:

    object Dog extends Animal {     # error: `Animal` is not defined
        var name oftype string
    }

Fix: define the parent object before (or alongside) the child, or check
for a typo in the name:

    object Animal {
        var name oftype string
    }

    object Dog extends Animal {
        var breed oftype string
    }
"#,
        ),

        "E0101" => Some(
            r#"An identifier was used but has not been declared anywhere in the
current scope or any enclosing scope.

Erroneous example:

    print(greating)     # error: `greating` is undefined

Fix: correct the spelling or declare the variable first:

    var greeting = "Hello"
    print(greeting)

Note: Fidan performs a similarity search and will suggest names that are
close to the misspelled identifier.
"#,
        ),

        "E0102" => Some(
            r#"A variable name was declared with `var` but the same name was already
declared in the current scope.  Re-declaring a variable with `var` is
not allowed in scripts; use assignment (`=` or `set`) to update an
existing variable.

Erroneous example:

    var score = 10
    var score = 20      # error: `score` already declared

Fix: assign to the existing variable instead of re-declaring it:

    var score = 10
    score = 20          # OK: assignment

Note: In the interactive REPL, re-declaration with `var` is allowed so
that you can redefine variables between experiments.
"#,
        ),

        "E0103" => Some(
            r#"An assignment was made to a variable declared with `const var`.  Constants
cannot be reassigned after their initial declaration.

Erroneous example:

    const var MAX set 100
    MAX set 200         # error: cannot assign to constant `MAX`

Fix: either change the declaration to `var` if the value needs to change:

    var MAX set 100
    MAX set 200         # OK

or remove the reassignment.
"#,
        ),

        "E0104" => Some(
            r#"A `const var` declaration has no initializer.  A constant whose value
is `nothing` and can never be changed is always useless.

Erroneous example:

    const var MAX           # error: constant must have an initializer

Fix: provide an initial value:

    const var MAX set 100   # OK
"#,
        ),

        "E0105" => Some(
            r#"A type annotation refers to a name that is not a built-in type and has
not been declared as an `object` in the current scope.  This usually
means a typo in the type name.

Built-in types you can use in annotations:

    integer   float   boolean   string   nothing   dynamic
    list      dict    map       shared   pending

Erroneous example:

    var age oftype integre = 25        # error: `integre` is not a known type
    var flag oftype Booleon = true     # error: `Booleon` is not a known type

Fix: correct the spelling:

    var age oftype integer = 25        # OK
    var flag oftype boolean = true     # OK

For container types, the `oftype` keyword introduces the element type:

    var nums oftype list oftype integer    # list of integers
    var data oftype map oftype string      # map with string values

Note: Fidan performs a similarity search and will suggest the closest
matching type name when a typo is detected.
"#,
        ),

        // ── Type system ───────────────────────────────────────────────────────
        "E0201" => Some(
            r#"The type of the value being assigned or used as an initialiser does not
match the declared or inferred type of the variable.

Erroneous example:

    var age oftype integer = "twenty"   # error: string cannot be assigned to integer

Fix: use a value of the correct type, or remove the type annotation and
let the compiler infer it:

    var age oftype integer = 20         # OK
    var age = "twenty"                  # also OK — inferred as string
"#,
        ),

        "E0202" => Some(
            r#"An action declared to return a specific type produced a value of a
different type on at least one code path.

Erroneous example:

    action double with (required n -> integer) returns integer {
        return "twice"      # error: returns string, expected integer
    }

Fix: return a value of the declared return type:

    action double with (required n -> integer) returns integer {
        return n * 2
    }
"#,
        ),

        "E0203" => Some(
            r#"The `+`, `-`, `*`, `/`, or another binary operator was applied to a
combination of types that does not support that operation.

Erroneous example:

    var flag = true
    print(flag + 1)     # error: operator `+` cannot be applied to `boolean` and `integer`

Fix: make sure both operands have compatible types:

    var count = 5
    print(count + 1)    # OK: integer + integer

    # Or convert explicitly:
    var flag = true
    print((flag if flag else 0) + 1)
"#,
        ),

        // ── Argument / call ───────────────────────────────────────────────────
        "E0301" => Some(
            r#"A `required` parameter was not supplied at the call site.  Every
parameter marked `required` must be passed either positionally or by
name.

Erroneous example:

    action greet with (required name -> string) {
        print("Hello, {name}!")
    }

    greet()     # error: missing required argument `name`

Fix: pass the argument:

    greet("Alice")          # positional
    greet(name = "Alice")   # named
"#,
        ),

        // ── Concurrency / safety ──────────────────────────────────────────────
        "E0401" => Some(
            r#"A module-level variable is written by one parallel task and read or 
written by another task in the same `parallel` or `concurrent` block.
Because both tasks run on separate OS threads, these accesses are not
synchronised and constitute a data race.

Fidan detects this at compile time by tracking `StoreGlobal` and
`LoadGlobal` instructions across the task functions that are joined by
the same `JoinAll`.

Erroneous example:

    var counter = 0

    parallel {
        task A { counter = counter + 1 }   # error E0401: writes `counter`
        task B { counter = counter + 1 }   # error E0401: also writes `counter`
    }

Note: In `parallel for` bodies, each iteration runs concurrently.  A write
to a captured outer variable inside the body means every iteration races
to update the same slot.

Fix: wrap the shared variable in `Shared oftype T`:

    var counter = Shared(0)

    parallel {
        task A { counter.update(x => x + 1) }
        task B { counter.update(x => x + 1) }
    }
    print(counter.get())   # 2
"#,
        ),

        "E0402" => Some(
            r#"A `Pending oftype T` value produced by `spawn` was dropped without
being `await`-ed.  The spawned task may still be running when the
enclosing scope exits, which leads to undefined behaviour.

Erroneous example:

    action work { ... }

    spawn work()    # error: result discarded without await

Fix: either await the value or store it and await later:

    var task = spawn work()
    var result = await task
"#,
        ),

        // ── Warnings: lifecycle ───────────────────────────────────────────────
        "W1001" => Some(
            r#"A variable was declared with `var` but was given no initial value and
no type annotation.  Its value will be `nothing` until assigned, which
may cause surprising behaviour.

Example:

    var name            # warning: declared without a value

Consider providing an initial value or a type annotation:

    var name = ""
    var name oftype string          # explicit type; initial value is `nothing`
"#,
        ),

        "W1002" => Some(
            r#"A variable was declared and assigned but its value was never read.
This often indicates dead code, a misspelling in a later reference, or
an unnecessary computation.

Example:

    action compute {
        var temp = expensive_calc()     # warning: `temp` never used
        return 42
    }

Fix: either use the variable or remove the declaration.
"#,
        ),

        "W1003" => Some(
            r#"An action parameter was declared but never referenced inside the
action body.  This may indicate a forgotten use or a parameter that
is no longer needed.

Example:

    action greet with (required name -> string, optional title -> string) {
        print("Hello!")     # warning: `name` and `title` never used
    }

Fix: either use the parameter or remove it from the signature.
"#,
        ),

        "W1004" => Some(
            r#"A `spawn expr` expression produces a `Pending oftype T` handle but the
handle is never passed to `await`.  The spawned thread continues to run
but its return value is silently discarded when the handle goes out of scope.

This is almost always a bug: either you intended to await the result
or the spawn is unnecessary.

Erroneous example:

    spawn heavy_work()   # warning: Pending never awaited

Fix — option A: await the result if you need it:

    var result = await spawn heavy_work()
    print(result)

Fix — option B: store the handle and await it later:

    var task = spawn heavy_work()
    # … do other things …
    var result = await task

Fix — option C: if the side-effect is intentional and the return value
truly does not matter, assign to `_` to silence the warning:

    var _ = spawn fire_and_forget()
"#,
        ),

        // ── Warnings: style ───────────────────────────────────────────────────
        "W2001" => Some(
            r#"The file passed to `fidan run` or `fidan build` does not end in `.fdn`.
Fidan source files should use the `.fdn` extension so that editors,
build tools, and syntax highlighters recognise them correctly.

This warning does not prevent compilation or execution; it is purely
informational.
"#,
        ),

        "W2002" => Some(
            r#"A literal value (string, integer, float, boolean) appears as a
standalone statement but is not assigned to a variable or passed to a
function.  The value is computed and immediately discarded.

Erroneous example:

    42              # warning: has no effect
    "hello"         # warning: has no effect

Fix: assign the value or pass it somewhere useful:

    var answer = 42
    print("hello")
"#,
        ),

        "W2003" => Some(
            r#"An action (function) name appears as a standalone expression without
being called.  The reference resolves to the action itself but is
immediately discarded — no code inside the action runs.

Erroneous example:

    print           # warning: bare reference — print never runs
    greet           # warning: ditto

Fix: call the action with parentheses (and any required arguments):

    print("hello")
    greet(name)
"#,
        ),

        // ── Runtime ───────────────────────────────────────────────────────────
        "R0001" => Some(
            r#"An unhandled runtime error propagated to the top level.  This
typically means a `panic(...)` statement was executed and no enclosing
`attempt / catch` block caught the error.

Use `attempt / catch` to handle errors gracefully:

    attempt {
        risky_operation()
    } catch err {
        print("caught: {err}")
    }

Run with `--trace full` to see the full call stack at the point of the
panic:

    fidan run myfile.fdn --trace full
"#,
        ),

        "R1001" => Some(
            r#"The interpreter exceeded the maximum call-stack depth.  This almost
always means an action is calling itself (directly or indirectly)
without a base case that terminates the recursion.

Erroneous example:

    action forever {
        forever()       # runtime error: stack overflow
    }

Fix: add a termination condition:

    action countdown with (required n -> integer) {
        if n <= 0 { return }
        countdown(n - 1)
    }
"#,
        ),

        "R1002" => Some(
            r#"A `panic(...)` expression was executed in user code and the panic value
propagated to the top level without being caught.

Example:

    action divide with (required a -> integer, required b -> integer) returns integer {
        if b == 0 {
            panic("cannot divide by zero")
        }
        return a / b
    }

Wrap the call in `attempt / catch` to handle it:

    attempt {
        var result = divide(10, 0)
    } catch err {
        print("error: {err}")
    }
"#,
        ),

        "R2001" => Some(
            r#"Integer or float division (or the `%` remainder operator) was
attempted with a divisor of zero.

Erroneous example:

    var a = 10
    var b = 0
    print(a / b)    # runtime error: division by zero

Fix: guard against zero before dividing:

    if b != 0 {
        print(a / b)
    } otherwise {
        print("undefined")
    }
"#,
        ),

        "R2002" => Some(
            r#"A list or string was indexed with a position that is outside its
valid range (`0` to `length - 1`).

Erroneous example:

    var items = [10, 20, 30]
    print(items[5])     # runtime error: index 5 out of bounds for length 3

Fix: check the index before accessing:

    if idx < len(items) {
        print(items[idx])
    }
"#,
        ),

        "R2003" => Some(
            r#"An arithmetic operation produced a result that cannot be represented
in the target integer type (64-bit signed integer).  Fidan's integer
arithmetic wraps on overflow in debug mode and raises this error when
wrapping is detected.

Example:

    var big = 9223372036854775807   # i64::MAX
    print(big + 1)                  # runtime error: arithmetic overflow

Fix: use float arithmetic for very large numbers, or guard with a
range check before the operation.
"#,
        ),

        "R3001" => Some(
            r#"The standard library attempted to open a file but the operating system
returned an error.  The file may not exist, or the path may be wrong.

Example:

    use std.io
    var contents = std.io.read_file("missing.txt")   # runtime error: failed to open

Fix: check that the file exists before reading, or use `attempt / catch`:

    attempt {
        var contents = std.io.read_file("data.txt")
    } catch err {
        print("could not open file: {err}")
    }
"#,
        ),

        "R3002" => Some(
            r#"A file was opened successfully but an error occurred while reading its
contents.  This can happen if the file is deleted between the open and
read operations, or if a permission changes mid-read.

Use `attempt / catch` to handle the error gracefully.
"#,
        ),

        "R3003" => Some(
            r#"An attempt to write to a file failed.  Common causes include a full
disk, a read-only file system, or a file that was opened read-only.

Use `attempt / catch` to handle the error gracefully.
"#,
        ),

        "R3004" => Some(
            r#"The operating system denied access to a file or directory.  The
current user does not have the required read or write permissions.

Fix: check the file's permissions, or run the program with the
appropriate privileges.
"#,
        ),

        _ => None,
    }
}
