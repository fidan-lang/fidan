// fidan-interp/tests/integration.rs
//
// End-to-end integration tests: source string → full pipeline → result.
//
// Pipeline:
//   Source  →  Lexer  →  Parser  →  TypeChecker  →  HIR  →  MIR  →  Passes  →  Interpreter
//
// These tests guard against regressions across the entire front-to-back path
// and also verify the static analysis passes (E0401, W1004) through real MIR.

use std::ffi::c_void;
use std::sync::Arc;

use fidan_interp::{FidanValue, RunError, register_self_symbol, run_mir};
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::SourceMap;

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_native_add(a: i64, b: i64) -> i64 {
    a + b
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_sum3(a: i64, b: i64, c: i64) -> i64 {
    a + b + c
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_mix4(a: i64, b: f64, c: i8, d: usize) -> i64 {
    let bool_part = if c == 0 { 0 } else { 100 };
    a + (b as i64) + bool_part + d as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_float_scale(x: f64, scale: f64) -> f64 {
    x * scale
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_negate_bool(v: i8) -> i8 {
    if v == 0 { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_make_handle() -> usize {
    41
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_inc_handle(h: usize) -> usize {
    h + 1
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_read_handle(h: usize) -> i64 {
    h as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_test_free_handle(_: usize) {}

#[unsafe(no_mangle)]
/// # Safety
///
/// `args_ptr` must point to `args_cnt` valid `*mut FidanValue` entries, and
/// each pointed-to value must remain valid for the duration of the call.
pub unsafe extern "C" fn fidan_test_add_boxed(
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = unsafe { std::slice::from_raw_parts(args_ptr, args_cnt as usize) };
    let a = match unsafe { &*args[0] } {
        FidanValue::Integer(n) => *n,
        _ => 0,
    };
    let b = match unsafe { &*args[1] } {
        FidanValue::Integer(n) => *n,
        _ => 0,
    };
    Box::into_raw(Box::new(FidanValue::Integer(a + b)))
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `args_ptr` must point to `args_cnt` valid `*mut FidanValue` entries, and
/// each pointed-to value must remain valid for the duration of the call.
pub unsafe extern "C" fn fidan_test_echo_boxed(
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = unsafe { std::slice::from_raw_parts(args_ptr, args_cnt as usize) };
    let first = unsafe { &*args[0] }.clone();
    Box::into_raw(Box::new(first))
}

fn register_extern_test_symbols() {
    register_self_symbol(
        "fidan_test_native_add",
        fidan_test_native_add as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_sum3",
        fidan_test_sum3 as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_mix4",
        fidan_test_mix4 as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_float_scale",
        fidan_test_float_scale as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_negate_bool",
        fidan_test_negate_bool as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_make_handle",
        fidan_test_make_handle as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_inc_handle",
        fidan_test_inc_handle as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_read_handle",
        fidan_test_read_handle as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_free_handle",
        fidan_test_free_handle as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_add_boxed",
        fidan_test_add_boxed as *const () as *mut c_void,
    );
    register_self_symbol(
        "fidan_test_echo_boxed",
        fidan_test_echo_boxed as *const () as *mut c_void,
    );
}

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

#[test]
fn extern_native_integer_call_ok() {
    register_extern_test_symbols();
    let (mut mir, interner) = build_mir(
        r#"@extern("self", symbol = "fidan_test_native_add")
        action nativeAdd with (a oftype integer, b oftype integer) returns integer

        assert_eq(nativeAdd(20, 22), 42)"#,
    );
    fidan_passes::run_all(&mut mir);
    let native_add = interner.intern("nativeAdd");
    let extern_fn = mir
        .functions
        .iter()
        .find(|f| f.name == native_add)
        .expect("missing nativeAdd function in MIR");
    assert!(
        extern_fn.extern_decl.is_some(),
        "nativeAdd lost extern metadata in MIR"
    );
    assert!(
        matches!(extern_fn.return_ty, fidan_mir::MirTy::Integer),
        "nativeAdd return type lowered incorrectly: {:?}",
        extern_fn.return_ty
    );
    let builtin_call_found = mir.functions.iter().any(|func| {
        func.blocks.iter().any(|block| {
            block.instructions.iter().any(|instr| {
                matches!(instr,
                    fidan_mir::Instr::Call {
                        callee: fidan_mir::Callee::Builtin(sym),
                        ..
                    } if *sym == native_add
                ) || matches!(instr,
                    fidan_mir::Instr::Assign {
                        rhs: fidan_mir::Rvalue::Call {
                            callee: fidan_mir::Callee::Builtin(sym),
                            ..
                        },
                        ..
                    } if *sym == native_add
                )
            })
        })
    });
    let dynamic_call_found = mir.functions.iter().any(|func| {
        func.blocks.iter().any(|block| {
            block.instructions.iter().any(|instr| {
                matches!(
                    instr,
                    fidan_mir::Instr::Call {
                        callee: fidan_mir::Callee::Dynamic(_),
                        ..
                    }
                ) || matches!(
                    instr,
                    fidan_mir::Instr::Assign {
                        rhs: fidan_mir::Rvalue::Call {
                            callee: fidan_mir::Callee::Dynamic(_),
                            ..
                        },
                        ..
                    }
                )
            })
        })
    });
    assert!(
        !(builtin_call_found || dynamic_call_found),
        "call lowering debug: builtin={builtin_call_found} dynamic={dynamic_call_found}"
    );
    let result = run_src(
        r#"@extern("self", symbol = "fidan_test_native_add")
        action nativeAdd with (a oftype integer, b oftype integer) returns integer

        assert_eq(nativeAdd(20, 22), 42)"#,
    );
    if let Err(err) = result {
        panic!("{}: {}", err.code, err.message);
    }
}

#[test]
fn extern_native_handle_lifecycle_ok() {
    register_extern_test_symbols();
    let result = run_src(
        r#"@extern("self", symbol = "fidan_test_make_handle")
        action makeHandle returns handle

        @extern("self", symbol = "fidan_test_inc_handle")
        action incHandle with (h oftype handle) returns handle

        @extern("self", symbol = "fidan_test_read_handle")
        action readHandle with (h oftype handle) returns integer

        @extern("self", symbol = "fidan_test_free_handle")
        action freeHandle with (h oftype handle)

        var h = makeHandle()
        h = incHandle(h)
        assert_eq(readHandle(h), 42)
        freeHandle(h)"#,
    );
    if let Err(err) = result {
        panic!("{}: {}", err.code, err.message);
    }
}

#[test]
fn extern_fidan_abi_boxed_call_ok() {
    register_extern_test_symbols();
    let result = run_src(
        r#"@unsafe
        @extern("self", symbol = "fidan_test_add_boxed", abi = "fidan")
        action boxedAdd with (a oftype integer, b oftype integer) returns integer

        @unsafe
        @extern("self", symbol = "fidan_test_echo_boxed", abi = "fidan")
        action boxedEcho with (text oftype string) returns string

        assert_eq(boxedAdd(10, 32), 42)
        assert_eq(boxedEcho("hello"), "hello")"#,
    );
    if let Err(err) = result {
        panic!("{}: {}", err.code, err.message);
    }
}

#[test]
fn extern_mixed_native_signatures_do_not_corrupt_param_types() {
    register_extern_test_symbols();
    let result = run_src(
        r#"@extern("self", symbol = "fidan_test_native_add")
        action nativeAdd with (a oftype integer, b oftype integer) returns integer

        @extern("self", symbol = "fidan_test_sum3")
        action sum3 with (
            a oftype integer,
            b oftype integer,
            c oftype integer
        ) returns integer

        @extern("self", symbol = "fidan_test_mix4")
        action mix4 with (
            a oftype integer,
            b oftype float,
            c oftype boolean,
            d oftype handle
        ) returns integer

        @extern("self", symbol = "fidan_test_float_scale")
        action floatScale with (value oftype float, factor oftype float) returns float

        @extern("self", symbol = "fidan_test_negate_bool")
        action negateBool with (value oftype boolean) returns boolean

        assert_eq(nativeAdd(20, 22), 42)
        assert_eq(sum3(10, 20, 12), 42)
        assert_eq(mix4(7, 8.0, true, 9), 124)
        assert_eq(floatScale(2.5, 4.0), 10.0)
        assert_eq(negateBool(true), false)"#,
    );
    if let Err(err) = result {
        panic!("{}: {}", err.code, err.message);
    }
}

#[test]
fn extern_native_more_than_four_params_is_type_error() {
    let interner = make_interner();
    let source_map = Arc::new(SourceMap::new());
    let file = source_map.add_file(
        "<test>",
        r#"@extern("self")
        action tooWide with (
            a oftype integer,
            b oftype integer,
            c oftype integer,
            d oftype integer,
            e oftype integer
        ) returns integer"#,
    );
    let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
    assert!(
        parse_diags
            .iter()
            .all(|diag| diag.severity != fidan_diagnostics::Severity::Error),
        "unexpected parse errors: {parse_diags:?}"
    );
    let typed = fidan_typeck::typecheck_full(&module, interner);
    assert!(
        typed
            .diagnostics
            .iter()
            .any(|diag| diag.message.contains("supports at most 4 parameters")),
        "expected native extern arity error, got {:?}",
        typed
            .diagnostics
            .iter()
            .map(|diag| diag.message.clone())
            .collect::<Vec<_>>()
    );
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

#[test]
fn integer_invalid_string_returns_runtime_error() {
    let err = run_src(r#"var n = integer("cls")"#)
        .expect_err("expected invalid integer conversion to fail");
    assert_eq!(err.code.0, "R0001");
    assert!(
        err.message.contains("cannot convert")
            && err.message.contains("\"cls\"")
            && err.message.contains("integer"),
        "unexpected conversion error message: {}",
        err.message
    );
}

#[test]
fn float_invalid_string_returns_runtime_error() {
    let err =
        run_src(r#"var n = float("wat")"#).expect_err("expected invalid float conversion to fail");
    assert_eq!(err.code.0, "R0001");
    assert!(
        err.message.contains("cannot convert")
            && err.message.contains("\"wat\"")
            && err.message.contains("float"),
        "unexpected conversion error message: {}",
        err.message
    );
}

#[test]
fn integer_invalid_type_returns_runtime_error() {
    let err = run_src("var n = integer([1, 2, 3])")
        .expect_err("expected invalid integer conversion from list to fail");
    assert_eq!(err.code.0, "R0001");
    assert!(
        err.message.contains("list") && err.message.contains("integer"),
        "unexpected conversion error message: {}",
        err.message
    );
}

#[test]
fn len_invalid_type_returns_runtime_error() {
    let err =
        run_src("var n = len(42)").expect_err("expected len on integer to produce runtime error");
    assert_eq!(err.code.0, "R0001");
    assert!(
        err.message.contains("len()") && err.message.contains("integer"),
        "unexpected len error message: {}",
        err.message
    );
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
