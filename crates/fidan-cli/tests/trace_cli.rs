use std::path::{Path, PathBuf};
use std::process::Command;

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
