use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn trace_demo_path() -> PathBuf {
    workspace_root()
        .join("test")
        .join("examples")
        .join("trace_demo.fdn")
}

fn run_trace(mode: &str) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("run")
        .arg(trace_demo_path())
        .args(["--trace", mode])
        .current_dir(workspace_root())
        .output()
        .expect("run fidan trace demo");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn make_temp_program(name: &str, source: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("fidan_{name}_{}_{}.fdn", std::process::id(), nonce));
    std::fs::write(&path, source).expect("write temp fidan program");
    path
}

fn run_repl_session(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("repl")
        .args(args)
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fidan repl");

    {
        let stdin = child.stdin.as_mut().expect("repl stdin");
        stdin.write_all(input).expect("write repl input");
    }

    child.wait_with_output().expect("wait for repl output")
}

fn last_error_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn cli_trace_modes_have_distinct_output_shapes() {
    let (ok_none, _stdout_none, stderr_none) = run_trace("none");
    assert!(!ok_none, "trace demo should fail");
    assert!(
        !stderr_none.contains("stack trace"),
        "none mode should suppress the stack trace:\n{stderr_none}"
    );

    let (ok_compact, _stdout_compact, stderr_compact) = run_trace("compact");
    assert!(!ok_compact, "trace demo should fail");
    assert!(
        stderr_compact.contains("stack: inner"),
        "compact mode should render a one-line stack:\n{stderr_compact}"
    );
    assert!(
        !stderr_compact.contains("stack trace (innermost first):"),
        "compact mode should not render the expanded trace header:\n{stderr_compact}"
    );

    let (ok_short, _stdout_short, stderr_short) = run_trace("short");
    assert!(!ok_short, "trace demo should fail");
    assert!(
        stderr_short.contains("stack trace (innermost first):"),
        "short mode should render the expanded trace header:\n{stderr_short}"
    );
    assert!(
        !stderr_short.contains(" at "),
        "short mode should omit source locations:\n{stderr_short}"
    );

    let (ok_full, _stdout_full, stderr_full) = run_trace("full");
    assert!(!ok_full, "trace demo should fail");
    assert!(
        stderr_full.contains("inner(msg = \"iteration 42\")"),
        "full mode should render the innermost frame with args:\n{stderr_full}"
    );
    assert!(
        stderr_full.contains("middle(count = 42)"),
        "full mode should preserve inlined caller frames:\n{stderr_full}"
    );
    assert!(
        stderr_full.contains("outer()"),
        "full mode should render the outer caller:\n{stderr_full}"
    );
    assert!(
        stderr_full.contains("test\\examples\\trace_demo.fdn")
            || stderr_full.contains("test/examples/trace_demo.fdn"),
        "full mode should include source locations:\n{stderr_full}"
    );
}

#[test]
fn interpreted_env_args_exclude_host_cli_args() {
    let file = make_temp_program(
        "env_args_empty",
        r#"use std.env

action main {
    var args = env.args()
    if len(args) == 0 {
        print("ARGS EMPTY")
    } else {
        print("ARGS {args[0]}")
    }
}

main()
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("run")
        .arg(&file)
        .args(["--trace", "full"])
        .current_dir(workspace_root())
        .output()
        .expect("run fidan env-args demo");
    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected env.args() demo to succeed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ARGS EMPTY"),
        "interpreted run should not leak host CLI args into env.args():\n{stdout}"
    );
}

#[test]
fn interpreted_env_args_forward_program_args_after_double_dash() {
    let file = make_temp_program(
        "env_args_forwarded",
        r#"use std.env

action main {
    var args = env.args()
    print("ARGS {args[0]} {args[1]}")
}

main()
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("run")
        .arg(&file)
        .args(["--trace", "full", "--", "alpha", "beta"])
        .current_dir(workspace_root())
        .output()
        .expect("run fidan forwarded-args demo");
    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected forwarded env.args() demo to succeed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ARGS alpha beta"),
        "interpreted run should forward args after `--` into env.args():\n{stdout}"
    );
}

#[test]
fn run_max_errors_one_stops_after_first_parse_error() {
    let file = make_temp_program(
        "max_errors_one",
        r#"use std.async as async
use std.io as io

action main {
    parallel {
        task io.print("native work")
        task io.print("real threads")
    }

    concurrent {
        task {
            await async.sleep(40)
            io.print("cooperative scheduling")
        }
    }

    io.print("Built for native speed.")
}
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("run")
        .arg(&file)
        .args(["--max-errors", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("run fidan max-errors demo");
    std::fs::remove_file(&file).ok();

    assert!(
        !output.status.success(),
        "expected malformed program to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("expected `LBrace`, found `Dot`"),
        "expected the first parse error to be shown:\n{stderr}"
    );
    assert!(
        !stderr.contains("expected `task` inside concurrent/parallel block"),
        "max-errors=1 should suppress follow-up parse errors:\n{stderr}"
    );
    assert!(
        stderr.contains("could not run") && stderr.contains("1 error"),
        "footer should report a single error:\n{stderr}"
    );
}

#[test]
fn repl_max_errors_per_input_one_stops_after_first_parse_error() {
    let output = run_repl_session(
        &["--max-errors-per-input", "1"],
        br#"parallel {
    task io.print("native work")
    task io.print("real threads")
}
:quit
"#,
    );
    assert!(
        output.status.success(),
        "repl should exit cleanly after :quit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("expected `LBrace`, found `Dot`"),
        "expected the first REPL parse error to be shown:\n{stderr}"
    );
    assert!(
        !stderr.contains("expected `task` inside concurrent/parallel block"),
        "max-errors-per-input=1 should suppress follow-up REPL parse errors:\n{stderr}"
    );
}

#[test]
fn repl_top_level_for_does_not_poison_subsequent_inputs() {
    let output = run_repl_session(
        &[],
        br#"for _ in 1..3 { print(_) }
print(99)
:quit
"#,
    );
    assert!(
        output.status.success(),
        "repl should exit cleanly after :quit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("1") && stdout.contains("2") && stdout.contains("99"),
        "expected both the loop output and the later print output:\n{stdout}"
    );
    assert!(
        !stderr.contains("Lt` on nothing and nothing"),
        "top-level for should not corrupt later REPL inputs:\n{stderr}"
    );
}

#[test]
fn repl_top_level_for_mutation_persists_and_follow_up_runs() {
    let output = run_repl_session(
        &[],
        br#"var total = 0
for _ in 1..4 { total = total + 1 }
print(total)
print(123)
:quit
"#,
    );

    assert!(
        output.status.success(),
        "repl should exit cleanly after :quit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("4") && stdout.contains("123"),
        "expected loop mutation to persist and later input to run:\n{stdout}"
    );
    assert!(
        !stderr.contains("Lt` on nothing and nothing"),
        "top-level for mutation should not poison later REPL inputs:\n{stderr}"
    );
}

#[test]
fn repl_top_level_parallel_for_range_does_not_poison_subsequent_inputs() {
    let output = run_repl_session(
        &[],
        br#"var seen = Shared(false)
parallel for n in 1..5 {
    if n == 4 {
        seen.set(true)
    }
}
print(seen.get())
print(77)
:quit
"#,
    );

    assert!(
        output.status.success(),
        "repl should exit cleanly after :quit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("true") && stdout.contains("77"),
        "expected range-based parallel for to run and later input to stay healthy:\n{stdout}"
    );
    assert!(
        !stderr.contains("Lt` on nothing and nothing"),
        "top-level parallel for should not poison later REPL inputs:\n{stderr}"
    );
}

#[test]
fn repl_runtime_error_does_not_break_later_inputs() {
    let output = run_repl_session(
        &[],
        br#"panic("boom")
print(42)
:quit
"#,
    );

    assert!(
        output.status.success(),
        "repl should exit cleanly after :quit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("boom"),
        "expected the original runtime error to be reported:\n{stderr}"
    );
    assert!(
        stdout.contains("42"),
        "expected later input to keep working after a runtime error:\n{stdout}"
    );
    assert!(
        !stderr.contains("Lt` on nothing and nothing"),
        "runtime errors should not poison later REPL inputs:\n{stderr}"
    );
}

#[test]
fn repl_reset_clears_prior_bindings() {
    let output = run_repl_session(
        &[],
        br#"var answer = 42
:reset
print(answer)
:quit
"#,
    );

    assert!(
        output.status.success(),
        "repl should exit cleanly after :quit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown variable `answer`")
            || stderr.contains("unknown name `answer`")
            || stderr.contains("answer"),
        "expected :reset to clear previously-defined bindings:\n{stderr}"
    );
}

#[test]
fn fix_removes_standalone_unused_imports() {
    let file = make_temp_program(
        "fix_unused_import",
        r#"use std.io

action main {
    print("ok")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on unused import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert!(
        !patched.contains("use std.io"),
        "expected unused import to be removed:\n{patched}"
    );
    assert!(
        patched.contains("print(\"ok\")"),
        "expected the rest of the program to remain intact:\n{patched}"
    );
}

#[test]
fn fix_removes_last_grouped_unused_import_by_deleting_statement() {
    let file = make_temp_program(
        "fix_grouped_unused_single",
        r#"use std.math.{floor}

action main {
    print("ok")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on grouped unused import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert!(
        !patched.contains("use std.math.{floor}"),
        "expected lone grouped import statement to be removed:\n{patched}"
    );
    assert!(
        patched.contains("print(\"ok\")"),
        "expected the rest of the program to remain intact:\n{patched}"
    );
}

#[test]
fn fix_removes_grouped_unused_import_member_without_dropping_braces() {
    let file = make_temp_program(
        "fix_grouped_unused_member",
        r#"use std.math.{sqrt, floor}

action main {
    print(sqrt(9.0))
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on grouped unused import member demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert!(
        patched.contains("use std.math.{sqrt}"),
        "expected grouped import braces to stay intact around the remaining import:\n{patched}"
    );
    assert!(
        !patched.contains("floor"),
        "expected only the unused grouped member to be removed:\n{patched}"
    );
}

#[test]
fn fix_removes_duplicate_imports() {
    let file = make_temp_program(
        "fix_duplicate_import",
        r#"use std.math
use std.math

action main {
    print(math.sqrt(9.0))
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on duplicate import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert_eq!(
        patched.matches("use std.math").count(),
        1,
        "expected the duplicate import to be removed:\n{patched}"
    );
}

#[test]
fn direct_stdlib_function_import_counts_as_used() {
    let file = make_temp_program(
        "direct_stdlib_import_used",
        r#"use std.io.readFile

action main {
    print(readFile("demo.txt"))
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("check")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan check on direct stdlib import demo");

    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected direct stdlib import example to check cleanly:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unused import `readFile`"),
        "direct imported stdlib function should count as used when called:\n{stderr}"
    );
}

#[test]
fn grouped_stdlib_function_imports_in_interpolation_count_as_used() {
    let file = make_temp_program(
        "grouped_stdlib_imports_used_in_interp",
        r#"use std.math.{abs, sqrt, floor, ceil, round, max, min}

action main {
    print("math {abs(-7)} {sqrt(16.0)} {floor(3.7)} {ceil(3.2)} {round(2.5)} {max(3, 7)} {min(3, 7)}")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("check")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan check on grouped stdlib imports inside interpolation");

    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected grouped stdlib import interpolation example to check cleanly:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    for name in ["abs", "sqrt", "floor", "ceil", "round", "max", "min"] {
        assert!(
            !stderr.contains(&format!("unused import `{name}`")),
            "grouped stdlib import `{name}` should count as used inside interpolation:\n{stderr}"
        );
    }
}

#[test]
fn fix_removes_grouped_duplicate_import_member_and_keeps_one_used_copy() {
    let file = make_temp_program(
        "fix_grouped_duplicate_import",
        r#"use std.math.{sqrt, sqrt, floor}

action main {
    print(sqrt(9.0))
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on grouped duplicate import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert!(
        patched.contains("use std.math.{sqrt}"),
        "expected exactly one used grouped import to remain:\n{patched}"
    );
    assert!(
        !patched.contains("floor"),
        "expected unused grouped imports to be removed:\n{patched}"
    );
    assert_eq!(
        patched.matches("sqrt").count(),
        2,
        "expected one import and one call-site `sqrt` to remain:\n{patched}"
    );
}

#[test]
fn explain_line_parent_constructor_is_described_humanly() {
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg("test/examples/release_mega_1_0.fdn")
        .arg("--line")
        .arg("200")
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain on parent constructor call");

    assert!(
        output.status.success(),
        "expected explain to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("calls the parent constructor"),
        "expected beginner-friendly parent constructor explanation:\n{stdout}"
    );
    assert!(
        stdout.contains("passing `name`"),
        "expected argument flow to be mentioned:\n{stdout}"
    );
}

#[test]
fn explain_accepts_diagnostic_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg("--diagnostic")
        .arg("E0101")
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain --diagnostic");

    assert!(
        output.status.success(),
        "expected explain --diagnostic to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("error[E0101]"),
        "expected diagnostic explain header:\n{stdout}"
    );
}

#[test]
fn explain_accepts_file_range_alias() {
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg("test/examples/release_mega_1_0.fdn:200-200")
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain with file:range alias");

    assert!(
        output.status.success(),
        "expected explain file:range alias to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("calls the parent constructor"),
        "expected file:range alias to reach the same line explanation:\n{stdout}"
    );
}

#[test]
fn explain_defaults_to_whole_file_when_no_line_flags_are_given() {
    let file = make_temp_program(
        "explain_whole_file",
        r#"use std.io

action main {
    print("hello")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain on whole file");

    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected explain without line flags to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("imports namespace `std.io`"),
        "expected whole-file explain to include top-level import lines:\n{stdout}"
    );
    assert!(
        stdout.contains("calls `print`, passing string literal `\"hello\"`")
            && stdout.contains("calls `main`"),
        "expected whole-file explain to include later statements too:\n{stdout}"
    );
}

#[test]
fn explain_with_only_end_line_starts_from_line_one() {
    let file = make_temp_program(
        "explain_end_line_only",
        r#"use std.io

action main {
    print("hello")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg(&file)
        .arg("--end-line")
        .arg("1")
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain with only --end-line");

    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected explain with only --end-line to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("imports namespace `std.io`"),
        "expected --end-line alone to start from line 1:\n{stdout}"
    );
    assert!(
        !stdout.contains("declares action `main`"),
        "expected --end-line 1 to stop before later lines:\n{stdout}"
    );
}

#[test]
fn explain_last_error_replays_last_recorded_diagnostic() {
    let _guard = last_error_lock().lock().expect("last-error lock");
    let cache_path = std::env::temp_dir().join(format!(
        "fidan_last_error_test_{}_{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    let file = make_temp_program(
        "explain_last_error",
        r#"action main {
    print(unknown_name)
}

main()
"#,
    );

    let check_output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("check")
        .arg(&file)
        .env("FIDAN_LAST_ERROR_PATH", &cache_path)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan check to seed last error");

    std::fs::remove_file(&file).ok();

    assert!(
        !check_output.status.success(),
        "expected malformed program to fail check:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check_output.stdout),
        String::from_utf8_lossy(&check_output.stderr)
    );

    let explain_output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg("--last-error")
        .env("FIDAN_LAST_ERROR_PATH", &cache_path)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain --last-error");

    assert!(
        explain_output.status.success(),
        "expected explain --last-error to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&explain_output.stdout),
        String::from_utf8_lossy(&explain_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&explain_output.stdout);
    assert!(
        stdout.contains("last recorded diagnostic: E0101"),
        "expected last-error header to mention the recorded code:\n{stdout}"
    );
    assert!(
        stdout.contains("error[E0101]"),
        "expected --last-error to explain the stored diagnostic:\n{stdout}"
    );

    std::fs::remove_file(&cache_path).ok();
}
