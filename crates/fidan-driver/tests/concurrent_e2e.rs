use fidan_driver::install::{installed_llvm_toolchains, resolve_fidan_home};
use fidan_driver::{
    Backend, CompileOptions, EmitKind, ExecutionMode, FrontendOutput, LtoMode, OptLevel, Session,
    StripMode, compile, compile_file_to_mir,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn concurrent_source() -> &'static str {
    r#"var counter = 0

action bump_many with (times oftype integer) {
    var i = 0
    while i < times {
        counter = counter + 1
        i = i + 1
    }
}

action main {
    concurrent {
        task A { bump_many(100000) }
        task B { bump_many(100000) }
    }
    assert_eq(counter, 200000)
    print(counter)
}

main()
"#
}

fn spawn_source() -> &'static str {
    r#"var counter = Shared(0)

action bump returns integer {
    counter.set(counter.get() + 1)
    return counter.get()
}

action main {
    var pendingStatic = spawn bump()
    assert_eq(counter.get(), 0)
    assert_eq(await pendingStatic, 1)
    assert_eq(counter.get(), 1)

    var work = action with () returns integer {
        counter.set(counter.get() + 1)
        return counter.get()
    }
    var pendingDynamic = spawn work()
    assert_eq(counter.get(), 1)
    assert_eq(await pendingDynamic, 2)
    assert_eq(counter.get(), 2)
    print(counter.get())
}

main()
"#
}

fn scheduler_source() -> &'static str {
    r#"var trace = Shared(0)

action inner returns integer {
    trace.set(trace.get() * 10 + 3)
    return trace.get()
}

action main {
    concurrent {
        task A {
            trace.set(trace.get() * 10 + 1)
            var pending = spawn inner()
            assert_eq(await pending, 123)
            trace.set(trace.get() * 10 + 4)
        }
        task B {
            trace.set(trace.get() * 10 + 2)
        }
    }
    assert_eq(trace.get(), 1234)
    print(trace.get())
}

main()
"#
}

fn weak_shared_source() -> &'static str {
    r#"action main {
    var shared = Shared(12)
    var weak = WeakShared(shared)
    assert_eq(type(weak), "WeakShared")
    assert_eq(weak.isAlive(), true)

    var revived = weak.upgrade()
    assert_eq(type(revived), "Shared")
    assert_eq(revived.get(), 12)

    revived = nothing
    shared = nothing
    assert_eq(weak.isAlive(), false)
    assert_eq(weak.upgrade(), nothing)
    print("weak-ok")
}

main()
"#
}

fn object_method_source() -> &'static str {
    r#"use std.math.{sqrt}

object Point {
    var x oftype float = 0.0
    var y oftype float = 0.0

    new with (certain x oftype float, certain y oftype float) {
        this.x = x
        this.y = y
    }

    action distance_to with (certain other oftype Point) returns float {
        var dx = this.x - other.x
        var dy = this.y - other.y
        return sqrt(dx * dx + dy * dy)
    }
}

action main {
    var p1 = Point(0.0, 0.0)
    var p2 = Point(3.0, 4.0)
    assert_eq(p1.distance_to(p2), 5.0)
    print(p1.distance_to(p2))
}

main()
"#
}

fn stdlib_function_value_source() -> &'static str {
    r#"use std.string as strings

action apply_to_string with (value, fn) returns string {
    return fn(value)
}

action apply_substr with (value, start, finish, fn) returns string {
    return fn(value, start, finish)
}

action main {
    var to_text = string
    assert_eq(apply_to_string(42, to_text), "42")

    var substr_fn = strings.substr
    assert_eq(apply_substr("abcdef", 2, 5, substr_fn), "cde")
    print("ok")
}

main()
"#
}

fn builtin_assert_source() -> &'static str {
    r#"assert_eq(20 + 22, 42)
assert_ne(20 + 21, 42)
print("ok")
"#
}

fn parallel_reduce_source() -> &'static str {
    r#"use std.parallel.{parallelReduce}

action reduce_sum with (certain acc oftype integer, certain item oftype integer) returns integer {
    return acc + item
}

action main {
    assert_eq(parallelReduce([1, 2, 3, 4], 0, reduce_sum), 10)
    print("10")
}

main()
"#
}

fn parallel_reduce_top_level_source() -> &'static str {
    r#"use std.parallel.{parallelReduce}

action reduce_sum with (certain acc oftype integer, certain item oftype integer) returns integer {
    return acc + item
}

var reduced_parallel = parallelReduce([1, 2, 3, 4], 0, reduce_sum)
assert_eq(reduced_parallel, 10)
print("10")
"#
}

fn scalar_conversion_source() -> &'static str {
    r#"assert(boolean(1) == true)
assert(string(99) == "99")
assert(integer("42") == 42)
assert(float("3.5") == 3.5)
print("ok")
"#
}

fn time_format_source() -> &'static str {
    r#"use std.time

var fixed = 1700000000000
assert_eq(time.date(fixed), "2023-11-14")
assert_eq(time.time(fixed), "22:13:20")
assert_eq(time.datetime(fixed), "2023-11-14 22:13:20")
assert_eq(len(time.time(fixed)), 8)
assert_eq(len(time.datetime(fixed)), 19)
print("ok")
"#
}

fn default_args_source() -> &'static str {
    r#"action approx_equal with (
    certain a oftype float,
    certain b oftype float,
    optional rel_tol oftype float = 0.0000001,
    optional abs_tol oftype float = 0.0001
) returns boolean {
    var diff = a - b
    if diff < 0.0 {
        diff = 0.0 - diff
    }
    return diff <= abs_tol or diff <= rel_tol
}

assert(approx_equal(4.0, 4.0))
assert_eq(approx_equal(4.0, 4.2), false)
print("ok")
"#
}

fn percent_assign_source() -> &'static str {
    r#"action fold_mod with (certain value oftype integer, certain divisor oftype integer) returns integer {
    var local = value
    local %= divisor
    return local
}

action main {
    var total = 20
    total %= 6
    assert_eq(total, 2)

    assert_eq(fold_mod(20, 7), 6)
    assert_eq(fold_mod(15, 4), 3)
    assert_eq(fold_mod(9, 5), 4)
    print("ok")
}

main()
"#
}

fn collections_helpers_source() -> &'static str {
    r#"use std.collections

var enumerated = collections.enumerate(["a", "b", "c"])
assert_eq(enumerated[0][0], 0)
assert_eq(enumerated[0][1], "a")
assert_eq(enumerated[2][0], 2)

var zipped = collections.zip([1, 2], ["a", "b"])
assert_eq("{zipped[0]}", "(1, a)")
assert_eq(zipped[1][1], "b")

var chunked = collections.chunk([1, 2, 3, 4, 5], 2)
assert_eq(len(chunked), 3)
assert_eq(chunked[1][0], 3)
assert_eq(chunked[2][0], 5)

var windows = collections.window([1, 2, 3, 4], 3)
assert_eq(len(windows), 2)
assert_eq(windows[1][2], 4)

var partitioned = collections.partition([0, 1, nothing, "ok", false])
assert_eq("{partitioned}", "([1, ok], [0, nothing, false])")
assert_eq(len(partitioned[0]), 2)
assert_eq(len(partitioned[1]), 3)

var grouped = collections.groupBy(["red", "blue", "red"])
assert_eq(len(grouped["red"]), 2)
assert_eq(len(grouped["blue"]), 1)
print("ok")
"#
}

fn hashset_source() -> &'static str {
    r#"use std.collections

action main {
    var numbers oftype hashset oftype integer set hashset([1, 2, 2, 3])
    assert_eq(type(numbers), "hashset")
    assert_eq(numbers.len(), 3)
    assert_eq(numbers.contains(2), true)

    numbers.insert(5)
    numbers.remove(1)
    assert_eq(numbers.contains(1), false)

    var overlap set hashset([3, 4])
    var merged set numbers.union(overlap)
    var shared set numbers.intersect(overlap)
    var only_numbers set numbers.diff(overlap)

    assert_eq(merged.contains(4), true)
    assert_eq(shared.contains(3), true)
    assert_eq(only_numbers.contains(5), true)
    assert_eq(len(merged.toList()), 4)

    var legacy set hashset(["x", "x", "y"])
    assert_eq(type(legacy), "hashset")
    assert_eq(collections.setContains(legacy, "x"), true)
    assert_eq(collections.setLen(legacy), 2)
    assert_eq(len(collections.setToList(legacy)), 2)

    var tuple_values oftype hashset oftype (integer, string) set hashset([(1, "a"), (1, "a"), (2, "b")])
    assert_eq(tuple_values.len(), 2)
    assert_eq(tuple_values.contains((2, "b")), true)

    var scores oftype dict oftype (string, integer) set {"ada": 42}
    assert_eq(scores.get("ada"), 42)
    assert_eq(scores["ada"], 42)
    scores.set("grace", 99)
    assert_eq(scores.containsKey("grace"), true)
    assert_eq(scores["grace"], 99)
    assert_eq(len(scores.keys()), 2)
    assert_eq(len(scores.values()), 2)
    assert_eq(len(scores.entries()), 2)
    scores.remove("grace")
    assert_eq(scores.containsKey("grace"), false)

    var truthy_scores oftype dict oftype (boolean, integer) set {true: 7, false: 3}
    assert_eq(truthy_scores.get(true), 7)
    truthy_scores[false] = 11
    assert_eq(truthy_scores[false], 11)

    var tuple_scores oftype dict oftype ((string, integer), integer) set {("ada", 1): 42}
    assert_eq(tuple_scores.get(("ada", 1)), 42)
    tuple_scores[("grace", 2)] = 77
    assert_eq(tuple_scores[("grace", 2)], 77)
    print("ok")

}

main()
"#
}

fn invalid_hashset_constructor_source() -> &'static str {
    r#"action main {
    var source oftype dynamic = 42
    var bad = hashset(source)
    print(bad)
}

main()
"#
}

fn json_roundtrip_source(dict_path: &str, set_path: &str) -> String {
    format!(
        r#"use std.json

action main {{
    var tuple_scores = {{(1, true): "ok", (2, false): "nope"}}
    assert_eq(json.dump(tuple_scores, "{dict_path}"), true)
    var loaded_scores = json.load("{dict_path}")
    assert_eq(loaded_scores[(1, true)], "ok")
    assert_eq(loaded_scores[(2, false)], "nope")

    var tuple_values = hashset([(1, "a"), (1, "a"), (2, "b")])
    assert_eq(json.dump(tuple_values, "{set_path}"), true)
    var loaded_values = json.load("{set_path}")
    assert_eq(type(loaded_values), "hashset")
    assert_eq(loaded_values.len(), 2)
    assert_eq(loaded_values.contains((2, "b")), true)
    print("ok")
}}

main()
"#
    )
}

fn tuple_literal_source() -> &'static str {
    r#"var pair = (42, "ok")
assert_eq(type(pair), "tuple")
assert_eq(pair[0], 42)
assert_eq(pair[1], "ok")
print("ok")
"#
}

fn iterable_loop_source() -> &'static str {
    r#"action main {
    var tasks oftype hashset oftype integer set hashset([1, 2, 2, 3])
    var total = 0
    for item in tasks {
        total = total + item
    }
    assert_eq(total, 6)

    var tuple_values = (4, 5, 6)
    var tuple_total = 0
    for value in tuple_values {
        tuple_total = tuple_total + value
    }
    assert_eq(tuple_total, 15)

    var ran_hashset = Shared(false)
    parallel for item in tasks {
        assert_eq(tasks.contains(item), true)
        ran_hashset.set(true)
    }
    assert_eq(ran_hashset.get(), true)

    var ran_tuple = Shared(false)
    parallel for value in tuple_values {
        assert(value >= 4)
        ran_tuple.set(true)
    }
    assert_eq(ran_tuple.get(), true)
    print("ok")
}

main()
"#
}

fn async_std_source() -> &'static str {
    r#"use std.async

action compute returns integer {
    return 41
}

var pending = spawn compute()
var gathered = async.gather([async.ready(1), pending, async.ready(3)])
var results = await gathered
assert_eq(results[0], 1)
assert_eq(results[1], 41)
assert_eq(results[2], 3)

var raced = await async.waitAny([async.sleep(25), async.ready(99)])
assert_eq("{raced}", "(1, 99)")
assert_eq(raced[0], 1)
assert_eq(raced[1], 99)

var timeoutFast = await async.timeout(async.ready(7), 10)
assert_eq("{timeoutFast}", "(true, 7)")
assert_eq(timeoutFast[0], true)
assert_eq(timeoutFast[1], 7)

var timeoutSlow = await async.timeout(async.sleep(20), 1)
assert_eq(timeoutSlow[0], false)
assert_eq(timeoutSlow[1], nothing)

var sleeper = async.sleep(1)
assert_eq(await sleeper, nothing)
print("ok")
"#
}

fn io_runtime_error_catch_source(path: &str) -> String {
    format!(
        r#"use std.io

action main {{
    attempt {{
        var value = io.readFile("{path}")
        print("unexpected: {{value}}")
    }} catch err {{
        assert_eq(type(err), "string")
        assert_eq(err.contains("R3001"), true)
        assert_eq(err.contains("failed to open file"), true)
        print("io-caught")
    }}
}}

main()
"#
    )
}

fn json_runtime_error_unhandled_source(path: &str) -> String {
    format!(
        r#"use std.json

action main {{
    var value = json.load("{path}")
    print("unexpected: {{value}}")
}}

main()
"#
    )
}

fn raw_string_source() -> &'static str {
    r#"assert_eq(r"literal \n {value}", "literal \\n \{value\}")
print("ok")
"#
}

fn multiline_string_source() -> &'static str {
    include_str!("../../../test/examples/multiline_strings.fdn")
}

fn repeated_assert_source() -> &'static str {
    r#"use std.io
use std.time

var root = io.join("LOCAL", "repeat-aot-" + string(time.now()))
var file = io.join(root, "x.txt")
var ok1 = io.makeDir(root)
var ok2 = io.writeFile(file, "a")
var ok3 = io.appendFile(file, "b")

assert(ok1)
assert(ok2)
assert(ok3)
assert_eq(io.readFile(file), "ab")
print("ok")
"#
}

fn branching_input_loop_source() -> &'static str {
    r#"action main {
    while true {
        const var command = input("cmd: ")
        if command == "add" {
            const var description = input("desc: ")
            print(description)
        } otherwise when command == "list" {
            print("list")
        } otherwise when command == "remove" {
            const var description = input("remove: ")
            print(description)
        } otherwise when command == "exit" {
            print("bye")
            break
        } else {
            print("bad")
        }
    }
}

main()
"#
}

fn top_level_scalar_globals_source() -> &'static str {
    r#"var a = 120000000
var b = 60000000
var c = 80000000
var d = 4

assert_eq(a, 120000000)
assert_eq(b, 60000000)
assert_eq(c, 80000000)
assert_eq(d, 4)
print("ok")
"#
}

#[derive(Debug, Clone)]
struct AotTestSettings {
    emit_obj: bool,
    lto: LtoMode,
    strip: StripMode,
    target_cpu: Option<String>,
}

impl Default for AotTestSettings {
    fn default() -> Self {
        Self {
            emit_obj: false,
            lto: LtoMode::Off,
            strip: StripMode::Off,
            target_cpu: None,
        }
    }
}

fn compile_program(source: &str, backend: Backend, output_path: &Path) {
    compile_program_with_settings(source, backend, output_path, &AotTestSettings::default());
}

fn compile_program_with_settings(
    source: &str,
    backend: Backend,
    output_path: &Path,
    settings: &AotTestSettings,
) {
    let src_path = output_path.with_extension("fdn");
    fs::write(&src_path, source).expect("write concurrent smoke source");
    let FrontendOutput { interner, mir, .. } =
        compile_file_to_mir(&src_path).expect("compile source to MIR");
    let opts = CompileOptions {
        input: src_path,
        output: Some(output_path.to_path_buf()),
        mode: ExecutionMode::Build,
        emit: if settings.emit_obj {
            vec![EmitKind::Obj]
        } else {
            vec![]
        },
        trace: fidan_driver::TraceMode::None,
        max_errors: None,
        jit_threshold: 0,
        strict_mode: false,
        replay_inputs: vec![],
        program_args: vec![],
        suppress: vec![],
        sandbox: None,
        opt_level: OptLevel::O2,
        extra_lib_dirs: vec![],
        link_dynamic: false,
        lto: settings.lto,
        strip: settings.strip,
        backend,
        target_cpu: settings.target_cpu.clone(),
    };
    compile(&Session::new(), mir, interner, &opts).expect("compile concurrent smoke program");
}

fn sidecar_object_path(bin: &Path) -> PathBuf {
    bin.with_extension(if cfg!(windows) { "obj" } else { "o" })
}

fn expect_compile_program_error(
    source: &str,
    backend: Backend,
    output_path: &Path,
    settings: &AotTestSettings,
) -> String {
    let src_path = output_path.with_extension("fdn");
    fs::write(&src_path, source).expect("write concurrent smoke source");
    let FrontendOutput { interner, mir, .. } =
        compile_file_to_mir(&src_path).expect("compile source to MIR");
    let opts = CompileOptions {
        input: src_path,
        output: Some(output_path.to_path_buf()),
        mode: ExecutionMode::Build,
        emit: if settings.emit_obj {
            vec![EmitKind::Obj]
        } else {
            vec![]
        },
        trace: fidan_driver::TraceMode::None,
        max_errors: None,
        jit_threshold: 0,
        strict_mode: false,
        replay_inputs: vec![],
        program_args: vec![],
        suppress: vec![],
        sandbox: None,
        opt_level: OptLevel::O2,
        extra_lib_dirs: vec![],
        link_dynamic: false,
        lto: settings.lto,
        strip: settings.strip,
        backend,
        target_cpu: settings.target_cpu.clone(),
    };
    compile(&Session::new(), mir, interner, &opts)
        .expect_err("expected compile to fail")
        .to_string()
}

fn run_compiled_binary(bin: &Path, expected_stdout_fragment: &str) {
    let output = Command::new(bin)
        .output()
        .expect("run compiled concurrent smoke binary");
    assert!(
        output.status.success(),
        "compiled concurrent smoke binary failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(expected_stdout_fragment),
        "expected compiled program output to contain {expected_stdout_fragment:?}, got:\n{}",
        stdout,
    );
}

fn run_compiled_binary_clean(bin: &Path, expected_stdout_fragment: &str) {
    let output = Command::new(bin)
        .output()
        .expect("run compiled concurrent smoke binary");
    assert!(
        output.status.success(),
        "compiled concurrent smoke binary failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(expected_stdout_fragment),
        "expected compiled program output to contain {expected_stdout_fragment:?}, got:\n{}",
        stdout,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.trim().is_empty(),
        "expected compiled program stderr to stay empty, got:\n{}",
        stderr,
    );
}

fn run_compiled_binary_with_input_clean(
    bin: &Path,
    input: &str,
    expected_stdout_fragments: &[&str],
) {
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn compiled concurrent smoke binary with input");
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .expect("write stdin for compiled concurrent smoke binary");
    }
    let output = child
        .wait_with_output()
        .expect("wait for compiled concurrent smoke binary with input");
    assert!(
        output.status.success(),
        "compiled concurrent smoke binary failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for fragment in expected_stdout_fragments {
        assert!(
            stdout.contains(fragment),
            "expected compiled program output to contain {fragment:?}, got:\n{}",
            stdout,
        );
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.trim().is_empty(),
        "expected compiled program stderr to stay empty, got:\n{}",
        stderr,
    );
}

fn run_compiled_binary_expect_failure(bin: &Path, expected_stderr_fragment: &str) {
    let output = Command::new(bin)
        .output()
        .expect("run compiled concurrent smoke binary expecting failure");
    assert!(
        !output.status.success(),
        "expected compiled concurrent smoke binary to fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected_stderr_fragment),
        "expected compiled failure stderr to contain {expected_stderr_fragment:?}, got:\n{}",
        stderr,
    );
}

fn run_compiled_binary_clean_n_times(bin: &Path, expected_stdout_fragment: &str, runs: usize) {
    for attempt in 0..runs {
        let output = Command::new(bin)
            .output()
            .expect("run compiled concurrent smoke binary");
        assert!(
            output.status.success(),
            "compiled concurrent smoke binary failed on run {}:\nstdout:\n{}\nstderr:\n{}",
            attempt + 1,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(expected_stdout_fragment),
            "expected compiled program output to contain {expected_stdout_fragment:?} on run {}, got:\n{}",
            attempt + 1,
            stdout,
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.trim().is_empty(),
            "expected compiled program stderr to stay empty on run {}, got:\n{}",
            attempt + 1,
            stderr,
        );
    }
}

fn llvm_available() -> bool {
    resolve_fidan_home()
        .ok()
        .and_then(|home| installed_llvm_toolchains(&home).ok())
        .is_some_and(|toolchains| !toolchains.is_empty())
}

#[test]
fn concurrent_cranelift_aot_same_thread_ok() {
    let sandbox = temp_dir("fidan_concurrent_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("concurrent_smoke.exe")
    } else {
        sandbox.join("concurrent_smoke")
    };
    compile_program(concurrent_source(), Backend::Cranelift, &output);
    run_compiled_binary(&output, "200000");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn concurrent_llvm_aot_same_thread_ok() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM concurrent AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_concurrent_llvm");
    let output = if cfg!(windows) {
        sandbox.join("concurrent_smoke.exe")
    } else {
        sandbox.join("concurrent_smoke")
    };
    compile_program(concurrent_source(), Backend::Llvm, &output);
    run_compiled_binary(&output, "200000");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn spawn_cranelift_aot_defers_until_await() {
    let sandbox = temp_dir("fidan_spawn_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("spawn_smoke.exe")
    } else {
        sandbox.join("spawn_smoke")
    };
    compile_program(spawn_source(), Backend::Cranelift, &output);
    run_compiled_binary(&output, "2");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn spawn_llvm_aot_defers_until_await() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM spawn AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_spawn_llvm");
    let output = if cfg!(windows) {
        sandbox.join("spawn_smoke.exe")
    } else {
        sandbox.join("spawn_smoke")
    };
    compile_program(spawn_source(), Backend::Llvm, &output);
    run_compiled_binary(&output, "2");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn weak_shared_cranelift_aot_round_trip_ok() {
    let sandbox = temp_dir("fidan_weak_shared_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("weak_shared_smoke.exe")
    } else {
        sandbox.join("weak_shared_smoke")
    };
    compile_program(weak_shared_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "weak-ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn weak_shared_llvm_aot_round_trip_ok() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM weak-shared smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_weak_shared_llvm");
    let output = if cfg!(windows) {
        sandbox.join("weak_shared_smoke.exe")
    } else {
        sandbox.join("weak_shared_smoke")
    };
    compile_program(weak_shared_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "weak-ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn concurrent_cranelift_aot_scheduler_yields_across_spawn_await() {
    let sandbox = temp_dir("fidan_scheduler_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("scheduler_smoke.exe")
    } else {
        sandbox.join("scheduler_smoke")
    };
    compile_program(scheduler_source(), Backend::Cranelift, &output);
    run_compiled_binary(&output, "1234");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn concurrent_llvm_aot_scheduler_yields_across_spawn_await() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM scheduler AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_scheduler_llvm");
    let output = if cfg!(windows) {
        sandbox.join("scheduler_smoke.exe")
    } else {
        sandbox.join("scheduler_smoke")
    };
    compile_program(scheduler_source(), Backend::Llvm, &output);
    run_compiled_binary(&output, "1234");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn object_method_cranelift_aot_preserves_object_args() {
    let sandbox = temp_dir("fidan_object_method_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("object_method_smoke.exe")
    } else {
        sandbox.join("object_method_smoke")
    };
    compile_program(object_method_source(), Backend::Cranelift, &output);
    run_compiled_binary(&output, "5");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn object_method_llvm_aot_preserves_object_args() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM object-method AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_object_method_llvm");
    let output = if cfg!(windows) {
        sandbox.join("object_method_smoke.exe")
    } else {
        sandbox.join("object_method_smoke")
    };
    compile_program(object_method_source(), Backend::Llvm, &output);
    run_compiled_binary(&output, "5");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn stdlib_function_values_cranelift_aot_are_callable() {
    let sandbox = temp_dir("fidan_stdlib_fn_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("stdlib_fn_smoke.exe")
    } else {
        sandbox.join("stdlib_fn_smoke")
    };
    compile_program(stdlib_function_value_source(), Backend::Cranelift, &output);
    run_compiled_binary(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn stdlib_function_values_llvm_aot_are_callable() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM stdlib function-value AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_stdlib_fn_llvm");
    let output = if cfg!(windows) {
        sandbox.join("stdlib_fn_smoke.exe")
    } else {
        sandbox.join("stdlib_fn_smoke")
    };
    compile_program(stdlib_function_value_source(), Backend::Llvm, &output);
    run_compiled_binary(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn builtin_asserts_cranelift_aot_do_not_fall_through_to_builtin_stderr() {
    let sandbox = temp_dir("fidan_builtin_asserts_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("builtin_asserts_smoke.exe")
    } else {
        sandbox.join("builtin_asserts_smoke")
    };
    compile_program(builtin_assert_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn builtin_asserts_llvm_aot_do_not_crash_or_fall_through() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM builtin assert AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_builtin_asserts_llvm");
    let output = if cfg!(windows) {
        sandbox.join("builtin_asserts_smoke.exe")
    } else {
        sandbox.join("builtin_asserts_smoke")
    };
    compile_program(builtin_assert_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn parallel_reduce_cranelift_aot_uses_initial_then_callback_order() {
    let sandbox = temp_dir("fidan_parallel_reduce_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("parallel_reduce_smoke.exe")
    } else {
        sandbox.join("parallel_reduce_smoke")
    };
    compile_program(parallel_reduce_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "10");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn parallel_reduce_top_level_cranelift_aot_uses_callback_as_third_arg() {
    let sandbox = temp_dir("fidan_parallel_reduce_top_level_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("parallel_reduce_top_level_smoke.exe")
    } else {
        sandbox.join("parallel_reduce_top_level_smoke")
    };
    compile_program(
        parallel_reduce_top_level_source(),
        Backend::Cranelift,
        &output,
    );
    run_compiled_binary_clean(&output, "10");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn scalar_conversions_cranelift_aot_round_trip_cleanly() {
    let sandbox = temp_dir("fidan_scalar_conversions_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("scalar_conversions_smoke.exe")
    } else {
        sandbox.join("scalar_conversions_smoke")
    };
    compile_program(scalar_conversion_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn percent_compound_assign_cranelift_aot_round_trip_cleanly() {
    let sandbox = temp_dir("fidan_percent_assign_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("percent_assign_smoke.exe")
    } else {
        sandbox.join("percent_assign_smoke")
    };
    compile_program(percent_assign_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn time_formatting_cranelift_aot_matches_stdlib_contract() {
    let sandbox = temp_dir("fidan_time_formatting_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("time_formatting_smoke.exe")
    } else {
        sandbox.join("time_formatting_smoke")
    };
    compile_program(time_format_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn default_args_llvm_aot_fill_missing_parameters() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM default-arg AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_default_args_llvm");
    let output = if cfg!(windows) {
        sandbox.join("default_args_smoke.exe")
    } else {
        sandbox.join("default_args_smoke")
    };
    compile_program(default_args_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn percent_compound_assign_llvm_aot_round_trip_cleanly() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM percent-assign AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_percent_assign_llvm");
    let output = if cfg!(windows) {
        sandbox.join("percent_assign_smoke.exe")
    } else {
        sandbox.join("percent_assign_smoke")
    };
    compile_program(percent_assign_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn repeated_cranelift_aot_runs_keep_dynamic_asserts_stable() {
    let sandbox = temp_dir("fidan_repeated_asserts_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("repeated_asserts_smoke.exe")
    } else {
        sandbox.join("repeated_asserts_smoke")
    };
    compile_program(repeated_assert_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean_n_times(&output, "ok", 12);
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn branching_input_loop_cranelift_aot_survives_invalid_then_exit() {
    let sandbox = temp_dir("fidan_branch_input_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("branch_input.exe")
    } else {
        sandbox.join("branch_input")
    };
    compile_program(branching_input_loop_source(), Backend::Cranelift, &output);
    run_compiled_binary_with_input_clean(&output, "bad\nexit\n", &["bad", "bye"]);
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn branching_input_loop_llvm_aot_survives_invalid_then_exit() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM branch-input AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_branch_input_llvm");
    let output = if cfg!(windows) {
        sandbox.join("branch_input.exe")
    } else {
        sandbox.join("branch_input")
    };
    compile_program(branching_input_loop_source(), Backend::Llvm, &output);
    run_compiled_binary_with_input_clean(&output, "bad\nexit\n", &["bad", "bye"]);
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn collections_helpers_cranelift_aot_match_interpreter_contract() {
    let sandbox = temp_dir("fidan_collections_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("collections_smoke.exe")
    } else {
        sandbox.join("collections_smoke")
    };
    compile_program(collections_helpers_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn collections_helpers_llvm_aot_match_interpreter_contract() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM collections AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_collections_llvm");
    let output = if cfg!(windows) {
        sandbox.join("collections_smoke.exe")
    } else {
        sandbox.join("collections_smoke")
    };
    compile_program(collections_helpers_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn hashset_cranelift_aot_matches_interpreter_contract() {
    let sandbox = temp_dir("fidan_hashset_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("hashset_smoke.exe")
    } else {
        sandbox.join("hashset_smoke")
    };
    compile_program(hashset_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn hashset_llvm_aot_matches_interpreter_contract() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM hashset AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_hashset_llvm");
    let output = if cfg!(windows) {
        sandbox.join("hashset_smoke.exe")
    } else {
        sandbox.join("hashset_smoke")
    };
    compile_program(hashset_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn iterable_for_cranelift_aot_supports_hashsets_and_tuples() {
    let sandbox = temp_dir("fidan_iterable_loop_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("iterable_loop.exe")
    } else {
        sandbox.join("iterable_loop")
    };
    compile_program(iterable_loop_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn iterable_for_llvm_aot_supports_hashsets_and_tuples() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM iterable-loop AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_iterable_loop_llvm");
    let output = if cfg!(windows) {
        sandbox.join("iterable_loop.exe")
    } else {
        sandbox.join("iterable_loop")
    };
    compile_program(iterable_loop_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn hashset_constructor_cranelift_aot_reports_runtime_error_for_invalid_dynamic_input() {
    let sandbox = temp_dir("fidan_hashset_invalid_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("hashset_invalid.exe")
    } else {
        sandbox.join("hashset_invalid")
    };
    compile_program(
        invalid_hashset_constructor_source(),
        Backend::Cranelift,
        &output,
    );
    run_compiled_binary_expect_failure(
        &output,
        "hashset(items) expects a list or hashset, got integer",
    );
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn hashset_constructor_llvm_aot_reports_runtime_error_for_invalid_dynamic_input() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM invalid hashset constructor smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_hashset_invalid_llvm");
    let output = if cfg!(windows) {
        sandbox.join("hashset_invalid.exe")
    } else {
        sandbox.join("hashset_invalid")
    };
    compile_program(invalid_hashset_constructor_source(), Backend::Llvm, &output);
    run_compiled_binary_expect_failure(
        &output,
        "hashset(items) expects a list or hashset, got integer",
    );
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn json_roundtrip_cranelift_aot_preserves_typed_dict_keys_and_hashsets() {
    let sandbox = temp_dir("fidan_json_roundtrip_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("json_roundtrip.exe")
    } else {
        sandbox.join("json_roundtrip")
    };
    let dict_path = sandbox.join("typed_dict.json");
    let set_path = sandbox.join("hashset.json");
    let source = json_roundtrip_source(
        &dict_path.display().to_string().replace('\\', "/"),
        &set_path.display().to_string().replace('\\', "/"),
    );
    compile_program(&source, Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn json_roundtrip_llvm_aot_preserves_typed_dict_keys_and_hashsets() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM json roundtrip smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_json_roundtrip_llvm");
    let output = if cfg!(windows) {
        sandbox.join("json_roundtrip.exe")
    } else {
        sandbox.join("json_roundtrip")
    };
    let dict_path = sandbox.join("typed_dict.json");
    let set_path = sandbox.join("hashset.json");
    let source = json_roundtrip_source(
        &dict_path.display().to_string().replace('\\', "/"),
        &set_path.display().to_string().replace('\\', "/"),
    );
    compile_program(&source, Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn io_runtime_error_cranelift_aot_is_catchable() {
    let sandbox = temp_dir("fidan_io_runtime_error_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("io_runtime_error_smoke.exe")
    } else {
        sandbox.join("io_runtime_error_smoke")
    };
    let missing = sandbox.join("missing.txt");
    let source = io_runtime_error_catch_source(&missing.display().to_string().replace('\\', "/"));
    compile_program(&source, Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "io-caught");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn io_runtime_error_llvm_aot_is_catchable() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM IO runtime error smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_io_runtime_error_llvm");
    let output = if cfg!(windows) {
        sandbox.join("io_runtime_error_smoke.exe")
    } else {
        sandbox.join("io_runtime_error_smoke")
    };
    let missing = sandbox.join("missing.txt");
    let source = io_runtime_error_catch_source(&missing.display().to_string().replace('\\', "/"));
    compile_program(&source, Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "io-caught");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn json_runtime_error_cranelift_aot_reports_unhandled_failure() {
    let sandbox = temp_dir("fidan_json_runtime_error_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("json_runtime_error_smoke.exe")
    } else {
        sandbox.join("json_runtime_error_smoke")
    };
    let missing = sandbox.join("missing.json");
    let source =
        json_runtime_error_unhandled_source(&missing.display().to_string().replace('\\', "/"));
    compile_program(&source, Backend::Cranelift, &output);
    run_compiled_binary_expect_failure(&output, "R3001");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn json_runtime_error_llvm_aot_reports_unhandled_failure() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM JSON runtime error smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_json_runtime_error_llvm");
    let output = if cfg!(windows) {
        sandbox.join("json_runtime_error_smoke.exe")
    } else {
        sandbox.join("json_runtime_error_smoke")
    };
    let missing = sandbox.join("missing.json");
    let source =
        json_runtime_error_unhandled_source(&missing.display().to_string().replace('\\', "/"));
    compile_program(&source, Backend::Llvm, &output);
    run_compiled_binary_expect_failure(&output, "R3001");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn tuple_literals_cranelift_aot_preserve_tuple_runtime_contract() {
    let sandbox = temp_dir("fidan_tuple_literal_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("tuple_literal_smoke.exe")
    } else {
        sandbox.join("tuple_literal_smoke")
    };
    compile_program(tuple_literal_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn tuple_literals_llvm_aot_preserve_tuple_runtime_contract() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM tuple-literal AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_tuple_literal_llvm");
    let output = if cfg!(windows) {
        sandbox.join("tuple_literal_smoke.exe")
    } else {
        sandbox.join("tuple_literal_smoke")
    };
    compile_program(tuple_literal_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn async_std_cranelift_aot_supports_pending_combinators() {
    let sandbox = temp_dir("fidan_async_std_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("async_std_smoke.exe")
    } else {
        sandbox.join("async_std_smoke")
    };
    compile_program(async_std_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn async_std_llvm_aot_supports_pending_combinators() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM async std AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_async_std_llvm");
    let output = if cfg!(windows) {
        sandbox.join("async_std_smoke.exe")
    } else {
        sandbox.join("async_std_smoke")
    };
    compile_program(async_std_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn raw_string_cranelift_aot_stays_literal() {
    let sandbox = temp_dir("fidan_raw_string_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("raw_string_smoke.exe")
    } else {
        sandbox.join("raw_string_smoke")
    };
    compile_program(raw_string_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn raw_string_llvm_aot_stays_literal() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM raw-string AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_raw_string_llvm");
    let output = if cfg!(windows) {
        sandbox.join("raw_string_smoke.exe")
    } else {
        sandbox.join("raw_string_smoke")
    };
    compile_program(raw_string_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn multiline_strings_cranelift_aot_round_trip_cleanly() {
    let sandbox = temp_dir("fidan_multiline_strings_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("multiline_strings_smoke.exe")
    } else {
        sandbox.join("multiline_strings_smoke")
    };
    compile_program(multiline_string_source(), Backend::Cranelift, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn multiline_strings_llvm_aot_round_trip_cleanly() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM multiline-string AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_multiline_strings_llvm");
    let output = if cfg!(windows) {
        sandbox.join("multiline_strings_smoke.exe")
    } else {
        sandbox.join("multiline_strings_smoke")
    };
    compile_program(multiline_string_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn top_level_scalar_globals_cranelift_aot_round_trip_cleanly() {
    let sandbox = temp_dir("fidan_top_level_scalar_globals_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("top_level_scalar_globals.exe")
    } else {
        sandbox.join("top_level_scalar_globals")
    };
    compile_program(
        top_level_scalar_globals_source(),
        Backend::Cranelift,
        &output,
    );
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn top_level_scalar_globals_llvm_aot_round_trip_cleanly() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM top-level scalar-global AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_top_level_scalar_globals_llvm");
    let output = if cfg!(windows) {
        sandbox.join("top_level_scalar_globals.exe")
    } else {
        sandbox.join("top_level_scalar_globals")
    };
    compile_program(top_level_scalar_globals_source(), Backend::Llvm, &output);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn cranelift_aot_accepts_target_cpu_native() {
    let sandbox = temp_dir("fidan_target_cpu_native_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_native.exe")
    } else {
        sandbox.join("target_cpu_native")
    };
    let settings = AotTestSettings {
        target_cpu: Some("native".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn cranelift_aot_accepts_target_cpu_generic() {
    let sandbox = temp_dir("fidan_target_cpu_generic_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_generic.exe")
    } else {
        sandbox.join("target_cpu_generic")
    };
    let settings = AotTestSettings {
        target_cpu: Some("generic".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[cfg(target_arch = "x86_64")]
#[test]
fn cranelift_aot_accepts_custom_target_cpu_preset() {
    let sandbox = temp_dir("fidan_target_cpu_custom_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_custom.exe")
    } else {
        sandbox.join("target_cpu_custom")
    };
    let settings = AotTestSettings {
        target_cpu: Some("haswell".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[cfg(target_arch = "x86_64")]
#[test]
fn cranelift_aot_accepts_custom_target_cpu_feature_aliases() {
    let sandbox = temp_dir("fidan_target_cpu_custom_features_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_custom_features.exe")
    } else {
        sandbox.join("target_cpu_custom_features")
    };
    let settings = AotTestSettings {
        target_cpu: Some("generic,+sse3,+ssse3,+sse4.1,+popcnt".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn cranelift_aot_rejects_unknown_target_cpu_feature() {
    let sandbox = temp_dir("fidan_target_cpu_bad_feature_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_bad_feature.exe")
    } else {
        sandbox.join("target_cpu_bad_feature")
    };
    let settings = AotTestSettings {
        target_cpu: Some("generic,+totally_fake_feature".to_string()),
        ..Default::default()
    };
    let error = expect_compile_program_error(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    assert!(
        error.contains("target CPU feature `totally_fake_feature`"),
        "unexpected error: {error}"
    );
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn llvm_aot_accepts_target_cpu_generic() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM generic target-cpu smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_target_cpu_generic_llvm");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_generic.exe")
    } else {
        sandbox.join("target_cpu_generic")
    };
    let settings = AotTestSettings {
        target_cpu: Some("generic".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(builtin_assert_source(), Backend::Llvm, &output, &settings);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn llvm_aot_accepts_target_cpu_native() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM native target-cpu smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_target_cpu_native_llvm");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_native.exe")
    } else {
        sandbox.join("target_cpu_native")
    };
    let settings = AotTestSettings {
        target_cpu: Some("native".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(builtin_assert_source(), Backend::Llvm, &output, &settings);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[cfg(target_arch = "x86_64")]
#[test]
fn llvm_aot_accepts_custom_target_cpu_spec() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM custom target-cpu smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_target_cpu_custom_llvm");
    let output = if cfg!(windows) {
        sandbox.join("target_cpu_custom.exe")
    } else {
        sandbox.join("target_cpu_custom")
    };
    let settings = AotTestSettings {
        target_cpu: Some("x86-64,+sse2".to_string()),
        ..Default::default()
    };
    compile_program_with_settings(builtin_assert_source(), Backend::Llvm, &output, &settings);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn cranelift_aot_emit_obj_keeps_object_sidecar() {
    let sandbox = temp_dir("fidan_emit_obj_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("emit_obj.exe")
    } else {
        sandbox.join("emit_obj")
    };
    let settings = AotTestSettings {
        emit_obj: true,
        ..Default::default()
    };
    compile_program_with_settings(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    run_compiled_binary_clean(&output, "ok");
    assert!(
        sidecar_object_path(&output).is_file(),
        "expected object sidecar at `{}`",
        sidecar_object_path(&output).display()
    );
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn llvm_aot_emit_obj_keeps_object_sidecar() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM emit-obj smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_emit_obj_llvm");
    let output = if cfg!(windows) {
        sandbox.join("emit_obj.exe")
    } else {
        sandbox.join("emit_obj")
    };
    let settings = AotTestSettings {
        emit_obj: true,
        ..Default::default()
    };
    compile_program_with_settings(builtin_assert_source(), Backend::Llvm, &output, &settings);
    run_compiled_binary_clean(&output, "ok");
    assert!(
        sidecar_object_path(&output).is_file(),
        "expected object sidecar at `{}`",
        sidecar_object_path(&output).display()
    );
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn cranelift_aot_lto_full_smoke() {
    let sandbox = temp_dir("fidan_lto_full_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("lto_full.exe")
    } else {
        sandbox.join("lto_full")
    };
    let settings = AotTestSettings {
        lto: LtoMode::Full,
        ..Default::default()
    };
    compile_program_with_settings(
        builtin_assert_source(),
        Backend::Cranelift,
        &output,
        &settings,
    );
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn llvm_aot_lto_full_smoke() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM full-LTO smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let sandbox = temp_dir("fidan_lto_full_llvm");
    let output = if cfg!(windows) {
        sandbox.join("lto_full.exe")
    } else {
        sandbox.join("lto_full")
    };
    let settings = AotTestSettings {
        lto: LtoMode::Full,
        ..Default::default()
    };
    compile_program_with_settings(builtin_assert_source(), Backend::Llvm, &output, &settings);
    run_compiled_binary_clean(&output, "ok");
    fs::remove_dir_all(&sandbox).ok();
}
