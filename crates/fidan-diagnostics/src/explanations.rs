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

        "E0106" => Some(
            r#"A `use` statement refers to a module that cannot be found on the file system.
Fidan resolves user modules relative to the importing file, mirroring Python's
package layout:

    use mymod           →  {dir}/mymod.fdn  or  {dir}/mymod/init.fdn
    use mymod.utils     →  {dir}/mymod/utils.fdn  or  {dir}/mymod/utils/init.fdn

Stdlib modules (e.g. `use std.io`) are handled separately and do not require
an on-disk file.

Erroneous example:

    use helpers         # error: no `helpers.fdn` or `helpers/init.fdn` found

Fix options:
  1. Create the missing file at the expected path.
  2. Correct the module name if it is a typo.
  3. Use a path string for explicit locations: `use "./helpers.fdn"`
"#,
        ),

        "E0107" => Some(
            r#"An `extends` clause names the same object that is being declared.
An object cannot be its own parent — this would create an unresolvable
inheritance cycle.

Erroneous example:

    object Dinosaur extends Dinosaur   # error: self-extension

Fix: extend a different object, or remove the `extends` clause entirely.
"#,
        ),

        "E0109" => Some(
            r#"A top-level name (action, object, or enum) is used more than once in
the same module scope.  This can happen in two distinct ways:

── Case 1: duplicate declaration ────────────────────────────────────────
Two top-level declarations share the same name.  Fidan does not support
overloading — each name must be unique within a module.

    action greet with (name oftype string) { ... }
    action greet with (name oftype string) { ... }   # error: `greet` already defined

    object Point { x oftype float, y oftype float }
    object Point { x oftype integer }                # error: `Point` already defined

Fix: rename one of the declarations, or remove the duplicate.

── Case 2: import conflicts with a local declaration ────────────────────
An import (`use`) binds a name that is also declared as a top-level
action, object, or enum in the same file.  Because both would resolve to
the same symbol, the compiler cannot determine which one a call site
should reach.

    use somelib.{greet}

    action greet with (...) { ... }   # error: conflicts with the import above

This also triggers when the import comes after the declaration:

    action greet with (...) { ... }

    use somelib.{greet}               # error: conflicts with the declaration above

Fix: give the import an alias so the two names no longer clash:

    use somelib.{greet as lib_greet}

    action greet with (...) { ... }   # OK — `greet` is the local one, `lib_greet` is the import
"#,
        ),

        "E0108" => Some(
            r#"A grouped or specific-name stdlib import refers to a name that the
module does not export.  Fidan validates these names at compile time
using each module's known export list.

This diagnostic only fires for specific-name imports (three or more path
segments):  `use std.io.{name}` or `use std.io.name`.  Plain namespace
imports (`use std.io`) are always allowed.

Erroneous example:

    use std.io.{dir_namewwwuowuwoi}   # error: `dir_namewwwuowuwoi` is not exported by `std.io`
    use std.math.{squirt}             # error: `squirt` is not exported by `std.math`

Fix: use a name that the module actually exports.  Hover over the module
path in your editor to see valid exports, or consult the standard library
documentation:

    use std.io.{read_file}    # OK
    use std.math.{sqrt}       # OK

Note: `fidan explain E0108` lists valid exports for each stdlib module
if you need a quick reference.
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

    action double with (certain n -> integer) returns integer {
        return "twice"      # error: returns string, expected integer
    }

Fix: return a value of the declared return type:

    action double with (certain n -> integer) returns integer {
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
        "E0204" => Some(
            r#"A field or method was accessed on an object that does not declare it
and it is not inherited from any locally-known parent class.

Erroneous example:

    object Cat {
        var name oftype string
        new with (name oftype string) { this.name = name }
    }

    const var c = Cat("Whiskers")
    print(c.wouf)   # error: Cat has no field or method `wouf`

Fix: check the spelling or add the missing declaration to the object.
"#,
        ),

        "E0205" => Some(
            r#"A parameter or variable that may hold `nothing` at runtime was used in
a context that requires a concrete value (such as a range bound, an arithmetic
operand, a for-loop iterable, or an index target).

This happens when a parameter is declared without the `certain` keyword, which
means callers may pass `nothing` as the argument.  If `nothing` is passed and
the code reaches the problematic expression, a runtime error (R0001) will occur.

Erroneous example:

    action roar with (times oftype integer) returns string {  # no `certain`!
        parallel for x in 1 .. times {   # error: `times` may be `nothing`
            print("ROAR!")
        }
        return "done"
    }

Fix (option 1 — add `certain` to guarantee the caller cannot pass `nothing`):

    action roar with (certain times oftype integer) returns string {
        parallel for x in 1 .. times {   # OK
            print("ROAR!")
        }
        return "done"
    }

Fix (option 2 — keep optional but guard at the call site with `??`):

    action roar with (times oftype integer) returns string {
        var safe_times = times ?? 0
        parallel for x in 1 .. safe_times {
            print("ROAR!")
        }
        return "done"
    }
"#,
        ),

        "E0301" => Some(
            r#"A required parameter was not supplied at the call site. Every
parameter not marked with `optional` must be passed either positionally or by
name.

Erroneous example:

    action greet with (certain name -> string) { # `certain` means `name` can not be `nothing`
        print("Hello, {name}!")
    }

    greet()     # error: missing required argument `name`

Fix: pass the argument:

    greet("Alice")          # positional
    greet(name = "Alice")   # named
"#,
        ),

        "E0302" => Some(
            r#"A method or action was called with an argument of the wrong type.

Erroneous example:

    action add with (certain a oftype integer, certain b oftype integer) returns integer {
        return a + b
    }

    add("one", "two")   # error: expected `integer`, found `string`

Fix: pass an argument of the correct type:

    add(1, 2)           # OK
"#,
        ),

        "E0305" => Some(
            r#"More arguments were passed to an action than it declares parameters.

Erroneous example:

    action greet with (certain name oftype string) {
        print("Hello, " + name)
    }

    greet("Alice", 2, 3)   # error E0305: expected 1 argument, got 3

Fix: remove the extra arguments:

    greet("Alice")          # OK

If the action should accept additional data, add the corresponding parameters:

    action greet with (certain name oftype string, certain times oftype integer) {
        for i in 1..times { print("Hello, " + name) }
    }

    greet("Alice", 3)       # OK
"#,
        ),

        // ── Object context ──────────────────────────────────────────────────
        "E0306" => Some(
            r#"`this` can only be used inside an `object` body, one of its methods,
or an `action` that extends an object (`action name extends ObjectName ...`).
Using `this` anywhere else — in a free action, at module level, or inside
a test block — is invalid because there is no object instance in scope.

Erroneous example:

    action greet() {
        print(this.name)   # error E0306: no object in scope
    }

Fix: move the logic into an object method:

    object Greeter {
        var name oftype string

        action greet() {
            print(this.name)   # OK — inside an object method
        }
    }
"#,
        ),

        "E0307" => Some(
            r#"`parent` can only be used inside an object that extends another object,
or inside an `action` that extends a child object.
`parent` refers to the parent object's fields and methods; it is meaningless
if the current object has no declared parent.

Erroneous example:

    object Animal {
        action speak() {
            print(parent.sound)   # error E0307: Animal has no parent
        }
    }

Fix: either remove the `parent` reference or add an `extends` clause:

    object LivingThing {
        var sound = "..."
    }

    object Animal extends LivingThing {
        action speak() {
            print(parent.sound)   # OK
        }
    }
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

    action greet with (certain name -> string, optional title -> string) {
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

        "W1005" => Some(
            r#"A `use` statement imports a module or symbol that is never referenced
anywhere in the file body.  The import has no effect and can be removed
to reduce noise and improve build times.

Erroneous example:

    use std.io          # note W1005: `io` imported but never used
    use std.math        # note W1005: `math` imported but never used

    action main {
        print("hello")
    }

Fix: remove the unused imports:

    action main {
        print("hello")
    }

or use them:

    use std.io

    action main {
        var line = io.readLine()
        print("you typed: " + line)
    }

Note: `fidan fix` can remove all unused imports automatically:

    fidan fix myfile.fdn
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

        "W2004" => Some(
            r#"A decorator name was applied to an action but is not recognised by
the Fidan compiler.  Recognised decorators are:

    @precompile   — eagerly JIT-compile the action before the first call
    @deprecated   — mark the action as deprecated; callers receive W2005
    @extern       — declare a foreign function imported from a native library
    @unsafe       — acknowledge an intentionally unsafe extern boundary
    @gpu          — (reserved, not yet implemented)

The unrecognised decorator is silently ignored at runtime, but it is
likely a typo or refers to a decorator that has not yet been implemented.

Erroneous example:

    @optimize    # warning: unknown decorator
    action heavy_compute { ... }

Fix: either correct the spelling or remove the unknown decorator:

    @precompile
    action heavy_compute { ... }
"#,
        ),

        "W2005" => Some(
            r#"An action that has been marked `@deprecated` was called.  Deprecated
actions are scheduled for removal in a future version of the codebase
and callers should be migrated to a replacement.

Erroneous example:

    @deprecated
    action old_api() { ... }

    old_api()   # warning: `old_api` is marked @deprecated

Fix: replace the call with the recommended replacement, then remove the
`@deprecated` decorator from the action once all callers are migrated.
"#,
        ),

        "W2006" => Some(
            r#"A local variable that is statically known to hold `nothing` was used
in a context that requires a real value — arithmetic, a method call,
field or index access, or a function parameter marked `certain`.

Because `nothing` is not a number, object, or container, these
operations will raise a runtime panic when the program is executed.

Erroneous example:

    var count           # never assigned — always `nothing`
    print(count + 1)    # warning W2006: `count` is nothing; `+` will panic

Another common pattern:

    action find with (certain haystack -> list) returns dynamic {
        # ... search that may return nothing ...
    }

    var result = find(items)
    print(result.name)      # warning W2006: `result` may be nothing; `.name` will panic

Fix — option A: provide an initial value:

    var count = 0
    print(count + 1)    # OK

Fix — option B: guard with a nil-check before use:

    var result = find(items)
    if result is not nothing {
        print(result.name)
    }

Fix — option C: use the null-coalescing operator `??`:

    print((result ?? default_item).name)

Note: this pass is flow-insensitive — it tracks `nothing` assignments
through SSA copies but does not reason about branch conditions.  Some
warnings may be false positives if the code is guarded by a condition
that the pass cannot see.  Use `--strict` to promote these warnings to
hard errors in safety-critical code.
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

    action countdown with (certain n -> integer) {
        if n <= 0 { return }
        countdown(n - 1)
    }
"#,
        ),

        "R1002" => Some(
            r#"A `panic(...)` expression was executed in user code and the panic value
propagated to the top level without being caught.

Example:

    action divide with (certain a -> integer, certain b -> integer) returns integer {
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

        // ── Runtime: sandbox / security ──────────────────────────────────────
        "R4001" => Some(
            r#"A file-system **read** operation was blocked by the active sandbox policy.

When a program is run with `fidan run --sandbox`, all file-system, environment,
and network access is denied by default.  Use the allow-flags to grant specific
permissions:

    fidan run --sandbox --allow-read=./data  myprogram.fdn

Erroneous example (run with `--sandbox`):

    use std.io
    var content = io.readFile("config.json")   # error R4001: read denied

Fix — option A: grant read access to the required path:

    fidan run --sandbox --allow-read=.  myprogram.fdn   # allow reads under .

Fix — option B: grant unrestricted read access:

    fidan run --sandbox --allow-read=*  myprogram.fdn

Fix — option C: remove `--sandbox` if sandboxing is not required.
"#,
        ),

        "R4002" => Some(
            r#"A file-system **write** operation was blocked by the active sandbox policy.

When a program is run with `fidan run --sandbox`, all file-system writes are
denied by default.  Use `--allow-write` to grant write access to specific paths:

    fidan run --sandbox --allow-write=./out  myprogram.fdn

Erroneous example (run with `--sandbox`):

    use std.io
    io.writeFile("output.txt", "hello")    # error R4002: write denied

Fix — option A: grant write access to the output directory:

    fidan run --sandbox --allow-write=./out  myprogram.fdn

Fix — option B: grant unrestricted write access:

    fidan run --sandbox --allow-write=*  myprogram.fdn
"#,
        ),

        "R4003" => Some(
            r#"An **environment** access (`getEnv`, `setEnv`, `args`, `cwd`) was blocked
by the active sandbox policy.

When a program is run with `fidan run --sandbox`, environment-variable access is
denied by default.  Pass `--allow-env` to enable it:

    fidan run --sandbox --allow-env  myprogram.fdn

Erroneous example (run with `--sandbox`):

    use std.io
    var home = io.getEnv("HOME")   # error R4003: environment access denied

Fix: add `--allow-env` to the run command, or remove `--sandbox` if sandboxing
is not required.
"#,
        ),

        // ── Runtime: parallel / concurrency ──────────────────────────────────
        "R9001" => Some(
            r#"One or more tasks inside a `parallel` block panicked or threw an
uncaught error before the block could complete.  The block waits for
ALL tasks to finish before reporting the combined failure.

Example:

    parallel {
        task compute { panic("something went wrong") }
        task store   { var x = 42 }
    }
    # runtime error[R9001]: 1 task failed in `parallel` block
    #   task `compute`: runtime panic: something went wrong

The block itself propagates the first failure as a panic.  Use
`attempt / catch` around the whole `parallel` block to handle it:

    attempt {
        parallel {
            task compute { panic("something went wrong") }
            task store   { var x = 42 }
        }
    } catch err {
        print("parallel block failed: {err}")
    }

If multiple tasks fail, all failures are listed in the error message
so you can diagnose them together.
"#,
        ),

        // ── Performance hints ─────────────────────────────────────────────────
        "W5001" => Some(
            r#"A loop body contains a local variable with type `flexible` (dynamic)
that is produced by a function call and then used as an argument to
another call, field access, or index access.

Because the type is only known at runtime, the JIT compiler cannot
specialize the loop body — every iteration incurs dynamic dispatch
overhead.

Erroneous example:

    action process_items with (certain items -> list) {
        for item in items {
            var result = transform(item)   # result: flexible
            print(result.name)             # W5001: dynamic field access in loop
        }
    }

Fix — option A: annotate the enclosing action with `@precompile`:

    @precompile
    action process_items with (certain items -> list) {
        for item in items {
            var result = transform(item)
            print(result.name)
        }
    }

Fix — option B: replace `flexible` with a concrete type (e.g. `string`):

    action transform with (certain x -> integer) returns string { ... }

    action process_items with (certain items -> list oftype integer) {
        for item in items {
            var result oftype string = transform(item)  # concrete type
            print(result)
        }
    }

Note: this hint is a best-effort static analysis.  Not every dynamic
variable in a loop causes a measurable slowdown — profile first.
Use `--strict` to treat performance hints as hard errors.
"#,
        ),

        "W5002" => Some(
            r#"A closure defined inside a loop body captures a mutable variable
from the enclosing scope.  This prevents the JIT from hoisting the
closure's code out of the loop and may inhibit optimization.

This hint is reserved for a future pass that tracks closure upvalues.
"#,
        ),

        "W5003" => Some(
            r#"An action is called on every iteration of a loop, but it is not
annotated with `@precompile`.  Without `@precompile` the JIT will
compile the action lazily on first call; subsequent calls are fast,
but the first call incurs compilation latency inside the hot path.

Erroneous example:

    action helper with (certain x -> integer) returns integer {
        return x * 2
    }

    action main {
        for i in range(1000000) {
            var y = helper(i)   # W5003: `helper` not @precompile
        }
    }

Fix — annotate `helper` with `@precompile`:

    @precompile
    action helper with (certain x -> integer) returns integer {
        return x * 2
    }

`@precompile` tells the JIT to compile the action eagerly before the
program starts executing, so there is no per-call compilation overhead.
"#,
        ),

        "W5004" => Some(
            r#"`@precompile` is a hint to the Cranelift JIT to eagerly compile an
action before execution begins.  In AOT (ahead-of-time) build mode all
actions are already fully compiled before the program runs, so the
annotation has no additional effect and can be removed.

To silence this warning, remove `@precompile` from the action
declaration or switch to a JIT / interpreter build target.
"#,
        ),

        "E0303" => Some(
            r#"A decorator's first parameter must have type `action` (or `flexible` / `dynamic`).
The decorated action is passed implicitly as the first argument at every decorated
call site; if the first parameter is annotated with any concrete non-action type,
the implicit argument will always type-mismatch.

Erroneous example:

    action tag with (certain name -> string) {   # error: first param is `string`, not `action`
        print(name)
    }

    @tag                # error E0303
    action greet with (name oftype string) {
        print("Hello, " + name)
    }

Fix: change the first parameter's type annotation to `action`:

    action tag with (certain fn -> action) {
        print(fn.name)
    }

    @tag
    action greet with (name oftype string) {
        print("Hello, " + name)
    }
"#,
        ),

        "E0304" => Some(
            r#"The number of extra arguments supplied to a decorator does not match the
number of extra parameters the decorator action expects.  The first parameter
of a decorator receives the decorated action implicitly; all remaining
parameters must be provided explicitly inside the `@decorator(...)` call.

Erroneous example:

    action log with (certain fn -> action, certain prefix -> string) {
        print(prefix + fn.name)
    }

    @log                # error E0304: expected 1 extra arg, got 0
    action greet {  }

Fix: pass the required extra arguments:

    @log(">> ")
    action greet {  }

Alternatively, if no extra arguments are needed, remove the extra parameters
from the decorator's declaration:

    action log with (certain fn -> action) {
        print(fn.name)
    }

    @log
    action greet {  }
"#,
        ),

        _ => None,
    }
}
