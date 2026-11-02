# Operators and evaluation order

The changes below target the next release; the published Fidan 1.1.0 parser
uses the older bitwise precedence. See the migration notes before upgrading.

## Operand contracts

`xor` is Boolean exclusive-or: `false xor false` and `true xor true` are false;
`false xor true` and `true xor false` are true. Both operands are evaluated
exactly once, left to right. There is no short-circuiting. An exception raised
while evaluating the left expression stops evaluation normally. If both
expressions return values, both are evaluated before Boolean type validation.

`^` is integer bitwise XOR: `0b1101 ^ 0b1011` is `6`, `-1 ^ 3` is `-4`.
It does not accept Boolean values. `xor` does not accept integers, floats,
strings, collections or `nothing`. No implicit truthiness/conversion is used.
Known invalid operands receive E0203; dynamic/flexible operands are checked
at runtime and invalid values raise catchable R0001 through the normal exception
channel. Both operands undergo normal static checking in every case.

`and`/`&&` and `or`/`||` retain strict Boolean short-circuit evaluation.
`&` and `|` remain eager integer bitwise operators. Integer shift counts use
`rhs & 63`, including negative counts.

## Precedence and associativity

Highest precedence first. Parser and formatter consume the same binding-power
contract in `fidan-ast`; aliases inherit their canonical operator's rules.

| Tier | Operators | Association |
| --- | --- | --- |
| Postfix | calls, `.`, `[]` | left |
| Prefix | `not`, unary `+`/`-`, `spawn`, `await` | nested prefix |
| Power | `**` (`pow`) | left, preserving existing behavior |
| Multiplicative | `*`, `/`, `%` (`mod`) | left |
| Additive | `+`, `-` | left |
| Shifts | `<<`, `>>` | left |
| Bitwise AND | `&` | left |
| Bitwise XOR | `^` | left |
| Bitwise OR | `|` | left |
| Comparisons | `==`, `!=`, `<`, `<=`, `>`, `>=`, `is`, `is not` | left |
| Logical AND | `and`, `&&` | left, short-circuit |
| Logical XOR | `xor` | left, eager |
| Logical OR | `or`, `||` | left, short-circuit |
| Range | `..`, `...` | left |
| Null coalescing | `??` | left, lazy right side |
| Conditional | `value if condition else fallback` | explicit nesting recommended |

Prefix `not` continues to bind more tightly than comparisons: write
`not (a < b)` to negate a comparison. Power keeps its existing left association:
`2 ** 3 ** 2` is `64`; `2 ** (3 ** 2)` is `512`.

```fidan
assert_eq(4 ^ 2 & 3, 6)           # 4 ^ (2 & 3)
assert_eq((4 ^ 2) & 3, 2)
assert_eq(8 >> 1 + 1, 2)          # 8 >> (1 + 1)
assert_eq(1 | 2 == 3, true)       # (1 | 2) == 3
assert_eq(false xor true and false, false)
assert_eq(true or true xor true, true)
assert_eq(not true xor true, true)
```

`xor` is contextual, not reserved. It remains valid as a variable, action,
parameter, import or property name. In infix position it denotes the operator:
`var xor = true; print(xor xor false)`. A new line beginning with `xor` starts
a statement; it is not implicitly joined to the previous statement. This keeps
existing `xor(value)` calls after declarations valid. Keep a binary XOR on one
expression line (the formatter handles wrapping whole expressions/call arguments).

## Backend and optimizer contract

MIR evaluates both operands first, strictly checks Boolean values using the
existing checked Boolean operations, then compares the inverted Boolean values
for inequality. This common lowering is used by the REPL, interpreter, selective
Cranelift JIT, Cranelift AOT and LLVM AOT. It emits no new runtime symbol or MIR
wire opcode. LLVM protocol 8 and helper binary 1.1.0 are wire-compatible, as
verified by native conformance tests. The published toolchain metadata currently
permits compiler `^1.1.0`; a future 2.0 release must explicitly prepare compatible
toolchain metadata rather than bypass that version check.
Constant expressions use the same Boolean truth table. Optimizers must preserve
operand effects, exceptions and dynamic checks even if the result is discarded.

## Migration from 1.1.0

The old parser grouped `^` with power, `&` and shifts with multiplication, and
`|` with addition. Its formatter already assumed separate bitwise tiers and
could remove parentheses that mattered to the parser. Unparenthesized mixed
expressions can therefore change meaning after this correction. For example,
`4 ^ 2 & 3` previously meant `(4 ^ 2) & 3` (`2`); it now means `4 ^ (2 & 3)` (`6`).

Parenthesize any expression where the old grouping was intentional **before**
upgrading. Run the new formatter with the new compiler, rather than formatting
with one version and executing with the other. Explicit parentheses are retained
whenever required to preserve the parsed expression tree. Existing source scans
found no checked-in Fidan program asserting the old mixed-bitwise grouping;
regression expectations were added rather than silently rewritten.

Because valid existing programs can produce different values, the recommended
next release is **2.0.0** under the project's SemVer policy. This change does not
publish a version or modify the current source/package version.
