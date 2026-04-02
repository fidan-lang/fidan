use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("repl")
        .args(["--max-errors-per-input", "1"])
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fidan repl");

    {
        let stdin = child.stdin.as_mut().expect("repl stdin");
        stdin
            .write_all(
                br#"parallel {
    task io.print("native work")
    task io.print("real threads")
}
:quit
"#,
            )
            .expect("write repl input");
    }

    let output = child.wait_with_output().expect("wait for repl output");
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
