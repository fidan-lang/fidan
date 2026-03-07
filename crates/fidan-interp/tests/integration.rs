// fidan-interp/tests/integration.rs
//
// End-to-end integration tests: source string → full pipeline → result.
//
// Pipeline:
//   Source  →  Lexer  →  Parser  →  TypeChecker  →  HIR  →  MIR  →  Passes  →  Interpreter
//
// These tests guard against regressions across the entire front-to-back path
// and also verify the static analysis passes (E0401, W1004) through real MIR.

use std::sync::Arc;

use fidan_interp::{RunError, run_mir};
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::SourceMap;

// ── Pipeline helper ───────────────────────────────────────────────────────────

fn make_interner() -> Arc<SymbolInterner> {
    Arc::new(SymbolInterner::new())
}

/// Lex → parse → typecheck → HIR → MIR lower → passes → `run_mir`.
///
/// Asserts there are no parse errors before running.
fn run_src(src: &str) -> Result<(), RunError> {
    let source_map = Arc::new(SourceMap::new());
    let interner = make_interner();
    let file = source_map.add_file("<test>", src);
    let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
    let parse_errors: Vec<_> = parse_diags
        .iter()
        .filter(|d| d.severity == fidan_diagnostics::Severity::Error)
        .collect();
    assert!(
        parse_errors.is_empty(),
        "unexpected parse errors in test source:\n{:?}",
        parse_errors
    );
    let tm = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    let hir = fidan_hir::lower_module(&module, &tm, &interner);
    let mut mir = fidan_mir::lower_program(&hir, &interner, &[]);
    fidan_passes::run_all(&mut mir);
    run_mir(mir, interner, source_map)
}

/// Build MIR without running it — used for static analysis assertions.
fn build_mir(src: &str) -> (fidan_mir::MirProgram, Arc<SymbolInterner>) {
    let source_map = Arc::new(SourceMap::new());
    let interner = make_interner();
    let file = source_map.add_file("<test>", src);
    let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, _) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
    let tm = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    let hir = fidan_hir::lower_module(&module, &tm, &interner);
    let mir = fidan_mir::lower_program(&hir, &interner, &[]);
    (mir, interner)
}

// ── Basic execution (Ok paths) ────────────────────────────────────────────────

#[test]
fn empty_program_ok() {
    assert!(run_src("").is_ok());
}

#[test]
fn var_integer_ok() {
    assert!(run_src("var x = 42").is_ok());
}

#[test]
fn var_arithmetic_ok() {
    assert!(run_src("var r = 1 + 2 * 3").is_ok());
}

#[test]
fn var_boolean_ok() {
    assert!(run_src("var flag = true").is_ok());
}

#[test]
fn var_string_ok() {
    assert!(run_src(r#"var s = "hello""#).is_ok());
}

#[test]
fn action_call_ok() {
    assert!(
        run_src(
            r#"action double with (n oftype integer) returns integer {
            return n * 2
        }
        var r = double(21)"#
        )
        .is_ok()
    );
}

#[test]
fn if_branch_ok() {
    assert!(
        run_src(
            r#"var x = 5
        var label = "unknown"
        if x > 0 {
            label = "positive"
        } otherwise {
            label = "nonpositive"
        }"#
        )
        .is_ok()
    );
}

#[test]
fn while_loop_ok() {
    assert!(
        run_src(
            r#"var i = 0
        var sum = 0
        while i < 5 {
            sum = sum + i
            i = i + 1
        }"#
        )
        .is_ok()
    );
}

#[test]
fn attempt_catch_ok() {
    assert!(
        run_src(
            r#"var result = 0
        attempt {
            panic("expected")
        } catch e {
            result = 1
        }"#
        )
        .is_ok()
    );
}

#[test]
fn recursive_action_ok() {
    assert!(
        run_src(
            r#"action fib with (n oftype integer) returns integer {
            if n <= 1 {
                return n
            }
            return fib(n - 1) + fib(n - 2)
        }
        var r = fib(10)"#
        )
        .is_ok()
    );
}

#[test]
fn null_coalesce_ok() {
    assert!(run_src("var x = nothing ?? 99").is_ok());
}

#[test]
fn list_literal_ok() {
    assert!(run_src("var xs = [1, 2, 3]").is_ok());
}

// ── Runtime error paths (Err with correct code) ───────────────────────────────

#[test]
fn panic_returns_r1002() {
    let err = run_src(r#"panic("deliberate")"#).expect_err("expected panic to produce RunError");
    assert_eq!(
        err.code.0, "R1002",
        "wrong code: expected R1002, got {}",
        err.code
    );
    assert!(
        err.message.contains("deliberate"),
        "error message should contain panic value: {}",
        err.message
    );
}

#[test]
fn uncaught_throw_returns_r1002() {
    // `panic` with a non-string value is R1002 (user-thrown panic)
    let err = run_src("panic(42)").expect_err("expected panic to produce RunError");
    assert_eq!(err.code.0, "R1002");
}

// ── Parallel execution ────────────────────────────────────────────────────────

#[test]
fn spawn_await_ok() {
    assert!(
        run_src(
            r#"action compute returns integer { return 42 }
        var h = spawn compute()
        var r = await h"#
        )
        .is_ok()
    );
}

#[test]
fn parallel_block_ok() {
    assert!(
        run_src(
            r#"var r1 = Shared(0)
        var r2 = Shared(0)
        parallel {
            task A { r1.set(1) }
            task B { r2.set(2) }
        }"#
        )
        .is_ok()
    );
}

#[test]
fn parallel_task_failure_returns_r9001() {
    let err = run_src(
        r#"parallel {
            task A { panic("task A failed") }
            task B { var x = 1 }
        }"#,
    )
    .expect_err("expected parallel task failure to produce RunError");
    assert_eq!(
        err.code.0, "R9001",
        "wrong code: expected R9001, got {}",
        err.code
    );
    assert!(
        err.message.contains("failed"),
        "error message should describe failure: {}",
        err.message
    );
}

// ── Static analysis passes ────────────────────────────────────────────────────

#[test]
fn e0401_parallel_data_race_detected() {
    // Two tasks both write a module-level global → should trigger E0401
    let (mir, interner) = build_mir(
        r#"var counter = 0
        parallel {
            task A { counter = counter + 1 }
            task B { counter = counter + 2 }
        }"#,
    );
    let races = fidan_passes::check_parallel_races(&mir, &interner);
    assert!(
        !races.is_empty(),
        "expected at least one E0401 data-race diagnostic, got none"
    );
    assert!(
        races[0].var_name.contains("counter"),
        "expected race on 'counter', got '{}'",
        races[0].var_name
    );
}

#[test]
fn e0401_no_race_with_shared() {
    // Shared<T> is always safe — writing via .set() should not trigger E0401
    let (mir, interner) = build_mir(
        r#"var counter = Shared(0)
        parallel {
            task A { counter.set(1) }
            task B { counter.set(2) }
        }"#,
    );
    let races = fidan_passes::check_parallel_races(&mir, &interner);
    assert!(
        races.is_empty(),
        "unexpected E0401 for Shared variable: {:?}",
        races.iter().map(|r| &r.var_name).collect::<Vec<_>>()
    );
}

#[test]
fn e0401_no_race_when_no_parallel() {
    // Sequential code should never trigger a data-race diagnostic
    let (mir, interner) = build_mir(
        r#"var x = 0
        x = x + 1
        x = x + 2"#,
    );
    let races = fidan_passes::check_parallel_races(&mir, &interner);
    assert!(races.is_empty(), "unexpected E0401 in sequential code");
}

#[test]
fn w1004_unawaited_spawn_detected() {
    // `spawn` expression whose result is never `await`ed → W1004
    let (mir, interner) = build_mir(
        r#"action work returns integer { return 1 }
        var h = spawn work()
        var x = 0"#, // h is never awaited
    );
    let warns = fidan_passes::check_unawaited_pending(&mir, &interner);
    assert!(
        !warns.is_empty(),
        "expected at least one W1004 unawaited-Pending diagnostic, got none"
    );
}

#[test]
fn w1004_no_warn_when_awaited() {
    // `spawn` followed by `await` should produce no W1004 warning
    let (mir, interner) = build_mir(
        r#"action work returns integer { return 1 }
        var h = spawn work()
        var r = await h"#,
    );
    let warns = fidan_passes::check_unawaited_pending(&mir, &interner);
    assert!(
        warns.is_empty(),
        "unexpected W1004 when spawn is correctly awaited"
    );
}

// ── Lambda (inline anonymous action) ─────────────────────────────────────────

#[test]
fn lambda_no_param_ok() {
    assert!(run_src(r#"var f = action { }"#).is_ok());
}

#[test]
fn lambda_with_param_foreach_ok() {
    assert!(
        run_src(
            r#"
var nums = [1, 2, 3]
nums.forEach(action with (x) { print(x) })
"#
        )
        .is_ok()
    );
}

#[test]
fn lambda_first_where_ok() {
    assert!(
        run_src(
            r#"
var nums = [1, 2, 3, 4]
var first_even = nums.firstWhere(action with (x) { return x % 2 == 0 })
"#
        )
        .is_ok()
    );
}

// ── Lambda capture (closure) ──────────────────────────────────────────────────

#[test]
fn lambda_captures_outer_var_ok() {
    assert!(
        run_src(
            r#"
var x = 10
var f = action with (n) { return n + x }
assert_eq(f(5), 15)
assert_eq(f(0), 10)
"#
        )
        .is_ok()
    );
}

// Module-level variables are captured by reference: mutations after the
// lambda is created are visible to the lambda.
#[test]
fn lambda_capture_sees_outer_mutation_ok() {
    assert!(
        run_src(
            r#"
var x = 10
var f = action with () { return x }
x = 99
assert_eq(f(), 99)
"#
        )
        .is_ok()
    );
}

#[test]
fn lambda_captures_multiple_vars_ok() {
    assert!(
        run_src(
            r#"
var a = 3
var b = 7
var add = action with () { return a + b }
assert_eq(add(), 10)
"#
        )
        .is_ok()
    );
}

// ── Enum payloads / ADTs ──────────────────────────────────────────────────────

#[test]
fn enum_unit_variants_ok() {
    assert!(
        run_src(
            r#"
enum Direction {
    North,
    South,
    East,
    West
}
var d = Direction.North
assert_eq(d, Direction.North)
assert_ne(d, Direction.South)
"#
        )
        .is_ok()
    );
}

#[test]
fn enum_payload_construct_ok() {
    assert!(
        run_src(
            r#"
enum Result {
    Ok(dynamic),
    Err(string)
}
var r = Result.Ok(42)
assert_eq(type(r), "enum")
"#
        )
        .is_ok()
    );
}

#[test]
fn enum_payload_check_ok() {
    assert!(
        run_src(
            r#"
enum Result {
    Ok(dynamic),
    Err(string)
}
var r = Result.Ok(99)
check r {
    Result.Ok(val) => assert_eq(val, 99)
    Result.Err(msg) => assert(false)
    _ => assert(false)
}
"#
        )
        .is_ok()
    );
}

#[test]
fn enum_payload_err_branch_ok() {
    assert!(
        run_src(
            r#"
enum Result {
    Ok(dynamic),
    Err(string)
}
var r = Result.Err("oops")
check r {
    Result.Ok(val) => assert(false)
    Result.Err(msg) => assert_eq(msg, "oops")
    _ => assert(false)
}
"#
        )
        .is_ok()
    );
}

#[test]
fn enum_payload_check_expr_ok() {
    assert!(
        run_src(
            r#"
enum Option {
    Some(dynamic),
    None
}
var o = Option.Some(7)
var v = check o {
    Option.Some(x) => x
    _ => 0
}
assert_eq(v, 7)
"#
        )
        .is_ok()
    );
}

#[test]
fn enum_mixed_unit_and_payload_ok() {
    assert!(
        run_src(
            r#"
enum Shape {
    Circle(float),
    Square(float),
    Point
}
var s = Shape.Circle(3.14)
check s {
    Shape.Circle(r) => assert(r > 3.0)
    Shape.Square(side) => assert(false)
    _ => assert(false)
}
var p = Shape.Point
check p {
    Shape.Point => assert(true)
    _ => assert(false)
}
"#
        )
        .is_ok()
    );
}

#[test]
fn enum_variant_payload_equality_ok() {
    // Regression guard: two variants with the same tag but *different* payloads
    // must NOT be considered equal (BUG 1 — payload was previously ignored).
    assert!(
        run_src(
            r#"
enum Result {
    Ok(dynamic),
    Err(string)
}
var a = Result.Ok(1)
var b = Result.Ok(2)
var c = Result.Ok(1)
assert_ne(a, b)
assert_eq(a, c)
var e1 = Result.Err("x")
var e2 = Result.Err("y")
assert_ne(e1, e2)
assert_ne(a, e1)
"#
        )
        .is_ok()
    );
}

#[test]
fn lambda_capture_in_foreach_ok() {
    assert!(
        run_src(
            r#"
var multiplier = 3
var nums = [1, 2, 4]
var results = []
nums.forEach(action with (x) { results.append(x * multiplier) })
assert_eq(results, [3, 6, 12])
"#
        )
        .is_ok()
    );
}

#[test]
fn lambda_capture_in_first_where_ok() {
    assert!(
        run_src(
            r#"
var threshold = 5
var nums = [1, 3, 7, 9]
var found = nums.firstWhere(action with (x) { return x > threshold })
assert_eq(found, 7)
"#
        )
        .is_ok()
    );
}

#[test]
fn lambda_no_capture_still_works_ok() {
    assert!(
        run_src(
            r#"
var double = action with (n) { return n * 2 }
assert_eq(double(6), 12)
"#
        )
        .is_ok()
    );
}

// ── Parameter semantics (certain / optional / default) ────────────────────────

/// Helper: returns typeck diagnostics for a source snippet.
fn typeck_diags(src: &str) -> Vec<fidan_diagnostics::Diagnostic> {
    let source_map = Arc::new(SourceMap::new());
    let interner = make_interner();
    let file = source_map.add_file("<test>", src);
    let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, _) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
    fidan_typeck::typecheck_full(&module, interner).diagnostics
}

#[test]
/// Case 1: plain param — must be passed, but `nothing` is allowed.
/// No E0205 should fire inside the body because the type is `dynamic`.
fn param_plain_nothing_allowed() {
    // `y` is plain (no certain/optional): must be passed, may be nothing.
    // Inside the body, using a dynamic param doesn't trigger E0205.
    assert!(
        run_src(
            r#"
action x with (y oftype dynamic) {
    print(y)
}
x(nothing)
"#
        )
        .is_ok()
    );
}

#[test]
/// Case 2: `certain` param — CertainCheck fires at runtime if `nothing` is passed.
fn param_certain_panics_on_nothing() {
    let result = run_src(
        r#"
action x with (certain y oftype dynamic) {
    print(y)
}
x(nothing)
"#,
    );
    assert!(
        result.is_err(),
        "expected runtime error when certain param receives nothing"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("y")
            || err.message.contains("certain")
            || err.message.contains("nothing"),
        "error message should mention the param: {}",
        err.message
    );
}

#[test]
/// Case 3: `optional` param with no default — omitting it leaves it as `nothing`.
/// E0205 should fire inside the body when the param is used as an arithmetic operand.
fn param_optional_no_default_e0205() {
    let diags = typeck_diags(
        r#"
action x with (optional y oftype integer) {
    var z = y + 1
}
"#,
    );
    let has_e0205 = diags.iter().any(|d| d.code.as_str() == "E0205");
    assert!(
        has_e0205,
        "expected E0205 for optional param without default used as arithmetic operand"
    );
}

#[test]
/// Case 4: `optional` param with a default — omitted OR passed as `nothing` both use the default.
fn param_optional_with_default_fills_in() {
    assert!(
        run_src(
            r#"
action greet with (optional name oftype string = "World") {
    return "Hello, " + name
}
# omitted → uses default
assert_eq(greet(), "Hello, World")
# explicit nothing → uses default
assert_eq(greet(nothing), "Hello, World")
# explicit value → uses that value
assert_eq(greet("Alice"), "Hello, Alice")
"#
        )
        .is_ok()
    );
}
