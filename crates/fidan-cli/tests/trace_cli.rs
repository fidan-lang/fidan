use fidan_driver::{
    AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisCommand,
    AiAnalysisRequest, AiAnalysisResponse, AiFixMode, ToolchainExecCommand, ToolchainMetadata,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::Builder;

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

fn make_temp_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("fidan_{name}_{}_{}", std::process::id(), nonce));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn host_triple() -> String {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            "x86_64-pc-windows-msvc".to_string()
        } else {
            "aarch64-pc-windows-msvc".to_string()
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "x86_64") {
            "x86_64-apple-darwin".to_string()
        } else {
            "aarch64-apple-darwin".to_string()
        }
    } else if cfg!(target_arch = "x86_64") {
        "x86_64-unknown-linux-gnu".to_string()
    } else {
        "aarch64-unknown-linux-gnu".to_string()
    }
}

fn make_fake_ai_helper(root: &Path, response_json: &str, capture_path: &Path) -> PathBuf {
    if cfg!(windows) {
        let helper = root.join("fidan-ai-analysis-helper.cmd");
        let response_path = root.join("response.json");
        std::fs::write(&response_path, response_json).expect("write helper response");
        let escaped_capture = capture_path.display().to_string().replace('\'', "''");
        let escaped_response = response_path.display().to_string().replace('\'', "''");
        let script = format!(
            "@echo off\r\npowershell -NoProfile -Command \"$inputJson = [Console]::In.ReadToEnd(); Set-Content -LiteralPath '{escaped_capture}' -Value $inputJson -NoNewline; [Console]::Out.Write((Get-Content -LiteralPath '{escaped_response}' -Raw))\"\r\n"
        );
        std::fs::write(&helper, script).expect("write helper cmd");
        helper
    } else {
        let helper = root.join("fidan-ai-analysis-helper.sh");
        let script = format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s' '{}'\n",
            capture_path.display(),
            response_json.replace('\'', "'\"'\"'")
        );
        std::fs::write(&helper, script).expect("write helper sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&helper)
                .expect("helper metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&helper, perms).expect("set helper perms");
        }
        helper
    }
}

fn install_fake_ai_toolchain(home: &Path, helper_path: &Path) {
    install_fake_toolchain(
        home,
        "ai-analysis",
        "1.0.2-test",
        AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        helper_path,
        vec![ToolchainExecCommand {
            namespace: "ai".to_string(),
            description: Some("AI analysis helper commands".to_string()),
        }],
    );
}

fn install_fake_toolchain(
    home: &Path,
    kind: &str,
    version: &str,
    backend_protocol_version: u32,
    helper_path: &Path,
    exec_commands: Vec<ToolchainExecCommand>,
) {
    let toolchain_dir = home
        .join("toolchains")
        .join(kind)
        .join(host_triple())
        .join(version);
    std::fs::create_dir_all(&toolchain_dir).expect("create toolchain dir");
    let helper_name = helper_path.file_name().expect("helper filename");
    std::fs::copy(helper_path, toolchain_dir.join(helper_name)).expect("copy helper");
    let metadata = ToolchainMetadata {
        schema_version: 1,
        kind: kind.to_string(),
        toolchain_version: version.to_string(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        host_triple: host_triple(),
        supported_fidan_versions: format!("^{}", env!("CARGO_PKG_VERSION")),
        backend_protocol_version,
        helper_relpath: helper_name.to_string_lossy().to_string(),
        exec_commands,
        archive_sha256: None,
    };
    std::fs::write(
        toolchain_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
    )
    .expect("write metadata");
}

struct InstallableAiHelper {
    helper_path: PathBuf,
}

fn make_installable_ai_helper(root: &Path, response_json: &str) -> InstallableAiHelper {
    if cfg!(windows) {
        let helper_path = root.join("fidan-ai-analysis-helper.cmd");
        let response_path = root.join("response.json");
        std::fs::write(&response_path, response_json).expect("write helper response");

        let script = "@echo off\r\nset \"FAKE_HELPER_ARGS=%*\"\r\nif /I \"%~1\"==\"analyze\" (\r\n  powershell -NoProfile -Command \"$inputJson = [Console]::In.ReadToEnd(); Set-Content -LiteralPath '%~dp0analyze_capture.json' -Value $inputJson -NoNewline; [Console]::Out.Write((Get-Content -LiteralPath '%~dp0response.json' -Raw))\"\r\n  exit /b %ERRORLEVEL%\r\n)\r\nif /I \"%~1\"==\"exec\" if /I \"%~2\"==\"ai\" if /I \"%~3\"==\"setup\" (\r\n  powershell -NoProfile -Command \"$line = [Console]::In.ReadLine(); if ([string]::IsNullOrWhiteSpace($line)) { exit 23 }; Set-Content -LiteralPath '%~dp0setup_args.txt' -Value $env:FAKE_HELPER_ARGS -NoNewline; Set-Content -LiteralPath '%~dp0setup_input.txt' -Value $line -NoNewline\"\r\n  exit /b %ERRORLEVEL%\r\n)\r\necho unexpected args 1>&2\r\nexit /b 99\r\n".to_string();
        std::fs::write(&helper_path, script).expect("write installable helper cmd");

        InstallableAiHelper { helper_path }
    } else {
        let helper_path = root.join("fidan-ai-analysis-helper.sh");
        let response_path = root.join("response.json");
        std::fs::write(&response_path, response_json).expect("write helper response");
        let script = "#!/bin/sh
set -eu
script_dir=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)
if [ \"${1-}\" = \"analyze\" ]; then
  cat > \"$script_dir/analyze_capture.json\"
  cat \"$script_dir/response.json\"
  exit 0
fi
if [ \"${1-}\" = \"exec\" ] && [ \"${2-}\" = \"ai\" ] && [ \"${3-}\" = \"setup\" ]; then
  IFS= read -r line || exit 23
  [ -n \"$line\" ] || exit 23
  printf '%s' \"$*\" > \"$script_dir/setup_args.txt\"
  printf '%s' \"$line\" > \"$script_dir/setup_input.txt\"
  exit 0
fi
echo unexpected args >&2
exit 99
";
        std::fs::write(&helper_path, script).expect("write installable helper sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&helper_path)
                .expect("helper metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&helper_path, perms).expect("set helper perms");
        }

        InstallableAiHelper { helper_path }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn create_toolchain_archive(source_dir: &Path, archive_path: &Path) -> String {
    let tar_gz = File::create(archive_path).expect("create toolchain archive");
    let encoder = GzEncoder::new(tar_gz, Compression::default());
    let mut builder = Builder::new(encoder);
    builder
        .append_dir_all("package", source_dir)
        .expect("append helper files to archive");
    let encoder = builder.into_inner().expect("finish tar builder");
    encoder.finish().expect("finish gz encoder");

    let archive_bytes = std::fs::read(archive_path).expect("read toolchain archive");
    sha256_hex(&archive_bytes)
}

fn write_local_toolchain_manifest(
    manifest_path: &Path,
    archive_path: &Path,
    sha256: &str,
    helper_relpath: &str,
) {
    let manifest = serde_json::json!({
        "schema_version": 1,
        "fidan_versions": [],
        "toolchains": [{
            "kind": "ai-analysis",
            "toolchain_version": "1.0.4-test",
            "tool_version": env!("CARGO_PKG_VERSION"),
            "host_triple": host_triple(),
            "url": format!("file://{}", archive_path.display()),
            "sha256": sha256,
            "helper_relpath": helper_relpath,
            "exec_commands": [{
                "namespace": "ai",
                "description": "AI analysis helper commands"
            }],
            "supported_fidan_versions": format!("^{}", env!("CARGO_PKG_VERSION")),
            "backend_protocol_version": AI_ANALYSIS_HELPER_PROTOCOL_VERSION
        }]
    });
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize local manifest"),
    )
    .expect("write local manifest");
}

fn make_fake_exec_helper(root: &Path, stdout_text: &str, args_capture_path: &Path) -> PathBuf {
    if cfg!(windows) {
        let helper = root.join("fidan-exec-helper.cmd");
        let stdout_path = root.join("stdout.txt");
        std::fs::write(&stdout_path, stdout_text).expect("write helper stdout");
        let escaped_args = args_capture_path.display().to_string().replace('\'', "''");
        let escaped_stdout = stdout_path.display().to_string().replace('\'', "''");
        let script = format!(
            "@echo off\r\nset \"FAKE_EXEC_ARGS=%*\"\r\npowershell -NoProfile -Command \"Set-Content -LiteralPath '{escaped_args}' -Value $env:FAKE_EXEC_ARGS -NoNewline; [Console]::Out.Write((Get-Content -LiteralPath '{escaped_stdout}' -Raw))\"\r\n"
        );
        std::fs::write(&helper, script).expect("write exec helper cmd");
        helper
    } else {
        let helper = root.join("fidan-exec-helper.sh");
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > '{}'\nprintf '%s' '{}'\n",
            args_capture_path.display(),
            stdout_text.replace('\'', "'\"'\"'")
        );
        std::fs::write(&helper, script).expect("write exec helper sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&helper)
                .expect("helper metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&helper, perms).expect("set helper perms");
        }
        helper
    }
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
fn full_trace_prefers_failing_inner_call_location() {
    let file = make_temp_program(
        "trace_inner_callsite",
        r#"object StorageManager {
  var tasks oftype hashset oftype string = hashset()

  action loadData returns boolean {
        var dyn oftype dynamic = this.tasks
        return dyn.noSuchMethod()
  }
}

action main {
  var sm = StorageManager()
  sm.loadData()
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
        .expect("run fidan inner-call trace demo");
    std::fs::remove_file(&file).ok();

    assert!(!output.status.success(), "trace demo should fail");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let file_text = file.display().to_string();
    assert!(
        stderr.contains("loadData(this = <StorageManager>)"),
        "full trace should render the failing inner frame:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("{file_text}:6:")),
        "full trace should point the innermost frame at the failing call inside loadData:\n{stderr}"
    );
    assert!(
        stderr.contains("main()"),
        "full trace should still preserve caller frames:\n{stderr}"
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
        .arg("--in-place")
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
        .arg("--in-place")
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
fn fix_removes_unused_aliased_import_without_leaving_alias_text() {
    let file = make_temp_program(
        "fix_unused_alias_import",
        r#"use std.math as math

action main {
    print("ok")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on aliased unused import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert!(
        !patched.contains("use std.math as math"),
        "expected aliased unused import to be removed:\n{patched}"
    );
    assert!(
        !patched.lines().any(|line| line.trim() == "math"),
        "expected no dangling alias token after removing the import:\n{patched}"
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
        .arg("--in-place")
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
        .arg("--in-place")
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
fn format_refuses_to_rewrite_parse_invalid_source() {
    let original = r#"use std.json

action main {
  var nested_data = json.parse("{"name": "Alice", "age": 30}")
}
"#;
    let file = make_temp_program("fmt_rejects_invalid_interp", original);

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("format")
        .arg("--in-place")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan format on malformed source");

    let patched = std::fs::read_to_string(&file).expect("read malformed source after format");
    std::fs::remove_file(&file).ok();

    assert!(
        !output.status.success(),
        "expected fidan format to reject malformed input:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        patched, original,
        "formatter must not rewrite malformed input"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected trailing tokens in interpolation expression")
            || stderr.contains("refusing to format"),
        "expected syntax-aware format rejection, got:\n{stderr}"
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
        .arg("--in-place")
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
fn fix_removes_cross_style_duplicate_import_member() {
    let file = make_temp_program(
        "fix_cross_style_duplicate_import",
        r#"use std.io.print
use std.io.{print, readFile}

action main {
    print(readFile("demo.txt"))
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on cross-style duplicate import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    assert!(
        patched.contains("use std.io.print"),
        "expected the direct import to remain:\n{patched}"
    );
    assert!(
        patched.contains("use std.io.{readFile}"),
        "expected only the grouped duplicate member to be removed:\n{patched}"
    );
    assert!(
        !patched.contains("use std.io.{print, readFile}"),
        "expected grouped duplicate member to be rewritten away:\n{patched}"
    );
}

#[test]
fn fix_prefers_export_import_over_plain_duplicate() {
    let file = make_temp_program(
        "fix_export_duplicate_import",
        r#"use std.io.print
export use std.io.print

action main {
    print("ok")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix on export-priority duplicate import demo");

    assert!(
        output.status.success(),
        "expected fidan fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let patched = std::fs::read_to_string(&file).expect("read patched file");
    std::fs::remove_file(&file).ok();

    let import_lines: Vec<&str> = patched
        .lines()
        .filter(|line| line.starts_with("use ") || line.starts_with("export use "))
        .collect();
    assert_eq!(
        import_lines,
        vec!["export use std.io.print"],
        "expected the exported import to win over the plain duplicate:\n{patched}"
    );
    assert!(
        !patched.lines().any(|line| line.trim() == "export"),
        "expected no dangling `export` token after fixing duplicates:\n{patched}"
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

#[test]
fn internal_ai_analysis_explain_context_returns_grounded_data() {
    let file = make_temp_program(
        "ai_analysis_context",
        r#"use std.io

action main {
    print("hello")
}

main()
"#,
    );
    let request = AiAnalysisRequest {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        command: AiAnalysisCommand::ExplainContext {
            file: file.clone(),
            line_start: Some(1),
            line_end: Some(6),
        },
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("__ai-analysis")
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fidan __ai-analysis");
    child
        .stdin
        .as_mut()
        .expect("analysis stdin")
        .write_all(&serde_json::to_vec(&request).expect("serialize request"))
        .expect("write analysis request");
    let output = child.wait_with_output().expect("wait for analysis output");

    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected internal ai-analysis request to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let response: AiAnalysisResponse =
        serde_json::from_slice(&output.stdout).expect("parse analysis response");
    assert!(response.success, "expected success response: {response:?}");
    let context = match response.result.expect("analysis result") {
        fidan_driver::AiAnalysisResult::ExplainContext(context) => context,
        other => panic!("unexpected result kind: {other:?}"),
    };
    assert!(context.selected_source.contains("print(\"hello\")"));
    assert!(
        context.dependencies.iter().any(|dep| dep.path == "std.io"),
        "expected std.io dependency in context: {:?}",
        context.dependencies
    );
    assert!(
        context
            .deterministic_lines
            .iter()
            .any(|line| line.what_it_does.contains("calls `print`")),
        "expected print call in deterministic context: {:?}",
        context.deterministic_lines
    );
    assert!(
        context.module_outline.iter().any(|item| item.name == "main"
            && item
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("calls `print`"))),
        "expected outline to include action body summary: {:?}",
        context.module_outline
    );
}

#[test]
fn explain_ai_uses_installed_toolchain_and_renders_structured_output() {
    let home = make_temp_dir("ai_toolchain_home");
    let helper_src = make_temp_dir("ai_toolchain_helper");
    let capture_path = helper_src.join("capture.json");
    let helper_response = serde_json::json!({
        "protocol_version": AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        "success": true,
        "result": {
            "kind": "explain",
            "summary": "Simple summary",
            "input_output_behavior": "Reads input and prints output.",
            "dependencies": "Uses std.io.",
            "possible_edge_cases": "No edge cases.",
            "why_pattern_is_used": "The action keeps the entry point explicit.",
            "related_symbols": "main",
            "underlying_behaviour": "Runs top-level code and calls main().",
            "provider": "test-provider",
            "model": "test-model"
        },
        "error": null
    });
    let helper_path = make_fake_ai_helper(&helper_src, &helper_response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program(
        "explain_ai",
        r#"action main {
    print("hello")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain --ai");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected explain --ai to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("summary"));
    assert!(stdout.contains("input/output behavior"));
    assert!(stdout.contains("underlying behaviour"));
    assert!(stdout.contains("Simple summary"));
}

#[test]
fn explain_ai_passes_optional_prompt_to_helper() {
    let home = make_temp_dir("ai_toolchain_prompt_home");
    let helper_src = make_temp_dir("ai_toolchain_prompt_helper");
    let capture_path = helper_src.join("capture.json");
    let helper_response = serde_json::json!({
        "protocol_version": AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        "success": true,
        "result": {
            "kind": "explain",
            "summary": "Prompted summary",
            "input_output_behavior": "IO behavior.",
            "dependencies": "Dependencies.",
            "possible_edge_cases": "Edge cases.",
            "why_pattern_is_used": "Pattern reason.",
            "related_symbols": "main",
            "underlying_behaviour": "Behaviour.",
            "provider": "test-provider",
            "model": "test-model"
        },
        "error": null
    });
    let helper_path = make_fake_ai_helper(&helper_src, &helper_response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program(
        "explain_ai_prompt",
        r#"action main {
    print("hello")
}

main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg(&file)
        .arg("--ai")
        .arg("Explain this regarding control flow and user-visible effects.")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain --ai with prompt");

    assert!(
        output.status.success(),
        "expected explain --ai prompt to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = std::fs::read_to_string(&capture_path).expect("read helper capture");
    let request: fidan_driver::AiAnalysisHelperRequest =
        serde_json::from_str(&captured).expect("parse helper request");
    let prompt = match request.command {
        fidan_driver::AiAnalysisHelperCommand::Explain { prompt, .. } => prompt,
        fidan_driver::AiAnalysisHelperCommand::Fix { .. } => panic!("unexpected Fix command"),
    };
    assert_eq!(
        prompt.as_deref(),
        Some("Explain this regarding control flow and user-visible effects.")
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();
}

#[test]
fn explain_ai_path_range_alias_is_forwarded_to_helper() {
    let home = make_temp_dir("ai_toolchain_alias_home");
    let helper_src = make_temp_dir("ai_toolchain_alias_helper");
    let capture_path = helper_src.join("capture.json");
    let helper_response = serde_json::json!({
        "protocol_version": AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        "success": true,
        "result": {
            "kind": "explain",
            "summary": "Alias summary",
            "input_output_behavior": "IO behavior.",
            "dependencies": "Dependencies.",
            "possible_edge_cases": "Edge cases.",
            "why_pattern_is_used": "Pattern reason.",
            "related_symbols": "main",
            "underlying_behaviour": "Behaviour.",
            "provider": "test-provider",
            "model": "test-model"
        },
        "error": null
    });
    let helper_path = make_fake_ai_helper(&helper_src, &helper_response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program(
        "explain_ai_alias",
        r#"action main {
    print("hello")
    print("again")
}

main()
"#,
    );
    let target = format!("{}:2-3", file.display());

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg(&target)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan explain path:range --ai");

    assert!(
        output.status.success(),
        "expected explain path:range --ai to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = std::fs::read_to_string(&capture_path).expect("read helper capture");
    let request: fidan_driver::AiAnalysisHelperRequest =
        serde_json::from_str(&captured).expect("parse helper request");
    let (line_start, line_end) = match request.command {
        fidan_driver::AiAnalysisHelperCommand::Explain {
            line_start,
            line_end,
            ..
        } => (line_start, line_end),
        fidan_driver::AiAnalysisHelperCommand::Fix { .. } => panic!("unexpected Fix command"),
    };
    assert_eq!(line_start, Some(2));
    assert_eq!(line_end, Some(3));

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();
}

#[test]
fn explain_ai_installs_toolchain_and_runs_setup_on_first_use() {
    let home = make_temp_dir("ai_install_home");
    let dist_dir = make_temp_dir("ai_install_dist");
    let helper_src = make_temp_dir("ai_install_helper");

    let helper_response = serde_json::json!({
        "protocol_version": AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        "success": true,
        "result": {
            "kind": "explain",
            "summary": "Installed toolchain explain summary",
            "input_output_behavior": "Prints output.",
            "dependencies": "Uses std.io.",
            "possible_edge_cases": "None.",
            "why_pattern_is_used": "Entry point stays explicit.",
            "related_symbols": "main",
            "underlying_behaviour": "Calls print.",
            "provider": "test-provider",
            "model": "test-model"
        },
        "error": null
    });
    let helper = make_installable_ai_helper(&helper_src, &helper_response.to_string());
    let archive_path = dist_dir.join("ai-analysis-toolchain.tar.gz");
    let sha256 = create_toolchain_archive(&helper_src, &archive_path);
    let manifest_path = dist_dir.join("manifest.json");
    let helper_relpath = helper
        .helper_path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("helper filename");
    write_local_toolchain_manifest(&manifest_path, &archive_path, &sha256, helper_relpath);

    let file = make_temp_program(
        "explain_ai_install",
        r#"action main {
    print("hello")
}

main()
"#,
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("explain")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .env(
            "FIDAN_DIST_MANIFEST",
            format!("file://{}", manifest_path.display()),
        )
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn explain --ai with install prompt");

    let mut stdin = child.stdin.take().expect("child stdin");
    let input_thread = std::thread::spawn(move || {
        stdin.write_all(b"y\n").expect("write install confirmation");
        std::thread::sleep(Duration::from_millis(150));
        stdin
            .write_all(b"local-setup-confirmed\n")
            .expect("write setup input");
    });

    let output = child.wait_with_output().expect("wait for explain --ai");
    input_thread.join().expect("join staged stdin writer");

    let installed_helper = home
        .join("toolchains")
        .join("ai-analysis")
        .join(host_triple())
        .join("1.0.4-test")
        .join(helper_relpath);

    let setup_args = std::fs::read_to_string(
        installed_helper
            .parent()
            .expect("installed helper dir")
            .join("setup_args.txt"),
    )
    .expect("read setup args capture");
    let setup_input = std::fs::read_to_string(
        installed_helper
            .parent()
            .expect("installed helper dir")
            .join("setup_input.txt"),
    )
    .expect("read setup input capture");
    let analyze_capture = std::fs::read_to_string(
        installed_helper
            .parent()
            .expect("installed helper dir")
            .join("analyze_capture.json"),
    )
    .expect("read analyze capture");

    let helper_installed = installed_helper.is_file();

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dist_dir).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected explain --ai to install the toolchain, run setup, and succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        helper_installed,
        "expected toolchain helper to be installed"
    );
    assert_eq!(setup_args.trim(), "exec ai setup");
    assert_eq!(setup_input.trim(), "local-setup-confirmed");
    assert!(
        analyze_capture.contains("\"kind\":\"explain\""),
        "expected analyze request to reach the installed helper:\n{analyze_capture}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Installed toolchain explain summary"),
        "expected explain output to use the installed helper response:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn exec_lists_registered_namespaces() {
    let home = make_temp_dir("exec_list_home");
    let helper_src = make_temp_dir("exec_list_helper");
    let args_capture = helper_src.join("args.txt");
    let helper_path = make_fake_exec_helper(&helper_src, "doctor ok", &args_capture);
    install_fake_ai_toolchain(&home, &helper_path);

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("exec")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan exec");

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected `fidan exec` to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ai"));
    assert!(stdout.contains("AI analysis helper commands"));
}

#[test]
fn exec_delegates_registered_namespace_to_helper() {
    let home = make_temp_dir("exec_delegate_home");
    let helper_src = make_temp_dir("exec_delegate_helper");
    let args_capture = helper_src.join("args.txt");
    let helper_path = make_fake_exec_helper(&helper_src, "doctor ok", &args_capture);
    install_fake_ai_toolchain(&home, &helper_path);

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .args(["exec", "ai", "doctor"])
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan exec ai doctor");

    let captured_args = std::fs::read_to_string(&args_capture).expect("read exec args capture");

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected `fidan exec ai doctor` to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(captured_args.trim(), "exec ai doctor");
    assert!(String::from_utf8_lossy(&output.stdout).contains("doctor ok"));
}

#[test]
fn exec_rejects_unknown_namespace() {
    let home = make_temp_dir("exec_unknown_home");
    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .args(["exec", "missing"])
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan exec missing");

    std::fs::remove_dir_all(&home).ok();

    assert!(
        !output.status.success(),
        "expected unknown exec namespace to fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("no external exec namespaces are registered")
            || String::from_utf8_lossy(&output.stderr).contains("is not registered"),
        "expected missing namespace error:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn exec_unknown_namespace_lists_available_namespaces() {
    let home = make_temp_dir("exec_unknown_known_home");
    let helper_src = make_temp_dir("exec_unknown_known_helper");
    let args_capture = helper_src.join("args.txt");
    let helper_path = make_fake_exec_helper(&helper_src, "doctor ok", &args_capture);
    install_fake_ai_toolchain(&home, &helper_path);

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .args(["exec", "missing"])
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan exec missing with registered namespaces");

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        !output.status.success(),
        "expected unknown exec namespace to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("available namespaces: ai"));
}

#[test]
fn exec_rejects_conflicting_namespaces_from_different_toolchains() {
    let home = make_temp_dir("exec_conflict_home");
    let helper_src = make_temp_dir("exec_conflict_helper");
    let args_capture = helper_src.join("args.txt");
    let helper_path = make_fake_exec_helper(&helper_src, "doctor ok", &args_capture);
    install_fake_toolchain(
        &home,
        "ai-analysis",
        "1.0.2-test",
        AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        &helper_path,
        vec![ToolchainExecCommand {
            namespace: "ai".to_string(),
            description: Some("AI analysis helper commands".to_string()),
        }],
    );
    install_fake_toolchain(
        &home,
        "other-toolchain",
        "1.0.2-test",
        1,
        &helper_path,
        vec![ToolchainExecCommand {
            namespace: "ai".to_string(),
            description: Some("Conflicting namespace".to_string()),
        }],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .args(["exec", "ai"])
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run conflicting exec namespace");

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        !output.status.success(),
        "expected conflicting namespace to fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exported by both toolchains"),
        "expected namespace conflict error:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── fidan fix --ai integration tests ─────────────────────────────────────────

/// A Fidan program whose only diagnostic is E0101 for `completely_unknown_xyz`,
/// which has no High-confidence auto-fix (no typo match in scope).
/// This guarantees `remaining_diags` is non-empty and the AI helper is called.
fn make_fix_ai_source() -> &'static str {
    r#"action main {
    print(completely_unknown_xyz)
}
main()
"#
}

fn make_fix_ai_nested_scope_source() -> &'static str {
    r#"var decorator_hits = 0

action decorate with (target oftype dynamic, label oftype string) {
    decorator_hits = decorator_hits + 1
    assert_eq(type(target), "action")
    assert_eq(label, "local")
}

action main {
    @decorate("local")
    @precompile
    action square with (certain value oftype integer) returns integer {
        return value * value
    }

    assert_eq(decorator_hits, 1)
    assert_eq(square(7), 49)
    print("49")
}

main()
square(8)
"#
}

fn make_fix_ai_helper_response(hunks: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        "success": true,
        "result": {
            "kind": "fix",
            "summary": "AI applied fix.",
            "hunks": hunks,
            "model": "test-model",
            "provider": "test-provider"
        },
        "error": null
    })
}

#[test]
fn fix_ai_diff_shows_ai_hunk_without_writing_file() {
    let home = make_temp_dir("fix_ai_diff_home");
    let helper_src = make_temp_dir("fix_ai_diff_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 2,
            "line_end": 2,
            "old_text": "    print(completely_unknown_xyz)",
            "new_text": "    print(\"hello\")",
            "reason": "E0101: completely_unknown_xyz is undefined"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program("fix_ai_diff", make_fix_ai_source());
    let original_contents = std::fs::read_to_string(&file).expect("read original file");

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --ai");

    let file_after = std::fs::read_to_string(&file).expect("read file after fix");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected fidan fix --ai to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Without --in-place the file must be unchanged.
    assert_eq!(
        file_after, original_contents,
        "fidan fix --ai (no --in-place) must not write the file"
    );

    // Diff should appear on stdout: old line removed, new line added.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("completely_unknown_xyz"),
        "diff should show the old line:\n{stdout}"
    );
    assert!(
        stdout.contains("print(\"hello\")"),
        "diff should show the replacement line:\n{stdout}"
    );
}

#[test]
fn fix_ai_in_place_applies_hunk_to_file() {
    let home = make_temp_dir("fix_ai_inplace_home");
    let helper_src = make_temp_dir("fix_ai_inplace_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 2,
            "line_end": 2,
            "old_text": "    print(completely_unknown_xyz)",
            "new_text": "    print(\"hello\")",
            "reason": "E0101: completely_unknown_xyz is undefined"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program("fix_ai_inplace", make_fix_ai_source());

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --in-place --ai");

    let patched = std::fs::read_to_string(&file).expect("read patched file");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected fidan fix --in-place --ai to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        patched.contains("print(\"hello\")"),
        "expected AI hunk to be applied to file:\n{patched}"
    );
    assert!(
        !patched.contains("completely_unknown_xyz"),
        "expected original undefined symbol to be replaced:\n{patched}"
    );
}

#[test]
fn fix_ai_in_place_rejects_syntax_breaking_hunks_and_keeps_valid_ones() {
    let home = make_temp_dir("fix_ai_reject_bad_hunk_home");
    let helper_src = make_temp_dir("fix_ai_reject_bad_hunk_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 2,
            "line_end": 2,
            "old_text": "    print(completely_unknown_xyz)",
            "new_text": "    print(\"hello\")",
            "reason": "E0101: completely_unknown_xyz is undefined"
        },
        {
            "line_start": 1,
            "line_end": 1,
            "old_text": "action main {",
            "new_text": "action main",
            "reason": "Bad model edit that would break braces"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program("fix_ai_reject_bad_hunk", make_fix_ai_source());

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --in-place --ai with mixed hunks");

    let patched = std::fs::read_to_string(&file).expect("read patched file");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected mixed AI hunks to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        patched.contains("print(\"hello\")"),
        "expected the valid AI hunk to be applied:\n{patched}"
    );
    assert!(
        patched.contains("action main {"),
        "expected the syntax-breaking hunk to be rejected:\n{patched}"
    );
    assert!(
        !patched.contains("completely_unknown_xyz"),
        "expected the undefined symbol to be resolved:\n{patched}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rejected or skipped 1 AI hunk"),
        "expected rejection note for the bad AI hunk:\n{stderr}"
    );
}

#[test]
fn fix_ai_accepts_minimal_scope_fix_that_moves_nested_action_outside_main() {
    let home = make_temp_dir("fix_ai_scope_move_home");
    let helper_src = make_temp_dir("fix_ai_scope_move_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 9,
            "line_end": 14,
            "old_text": "action main {\n    @decorate(\"local\")\n    @precompile\n    action square with (certain value oftype integer) returns integer {\n        return value * value\n    }",
            "new_text": "@decorate(\"local\")\n@precompile\naction square with (certain value oftype integer) returns integer {\n    return value * value\n}\n\naction main {",
            "reason": "E0101: move `square` to top-level scope so both `main` and the trailing call can resolve it"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program("fix_ai_scope_move", make_fix_ai_nested_scope_source());

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .arg("--ai")
        .arg("move the square action definition outside of main")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --ai for nested scope move");

    let patched = std::fs::read_to_string(&file).expect("read patched scope-fix file");
    let check_output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("check")
        .arg(&file)
        .current_dir(workspace_root())
        .output()
        .expect("check patched scope-fix file");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected scope move AI fix to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        check_output.status.success(),
        "expected moved scope fix to pass compiler check:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check_output.stdout),
        String::from_utf8_lossy(&check_output.stderr)
    );
    assert!(
        patched.contains("@decorate(\"local\")\n@precompile\naction square with (certain value oftype integer) returns integer {"),
        "expected square to be moved to top-level scope:\n{patched}"
    );
    assert!(
        patched.contains("main()\nsquare(8)"),
        "expected trailing top-level square call to remain valid:\n{patched}"
    );
}

#[test]
fn fix_ai_steering_prompt_forwarded_to_helper() {
    let home = make_temp_dir("fix_ai_prompt_home");
    let helper_src = make_temp_dir("fix_ai_prompt_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program("fix_ai_prompt", make_fix_ai_source());

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .arg("--ai")
        .arg("use type-safe alternatives")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --ai with steering prompt");

    std::fs::remove_file(&file).ok();

    assert!(
        output.status.success(),
        "expected fidan fix --ai with prompt to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = std::fs::read_to_string(&capture_path).expect("read helper capture");
    let request: fidan_driver::AiAnalysisHelperRequest =
        serde_json::from_str(&captured).expect("parse helper request");

    let prompt = match request.command {
        fidan_driver::AiAnalysisHelperCommand::Fix { prompt, .. } => prompt,
        fidan_driver::AiAnalysisHelperCommand::Explain { .. } => {
            panic!("unexpected Explain command")
        }
    };
    assert_eq!(
        prompt.as_deref(),
        Some("use type-safe alternatives"),
        "steering prompt should be forwarded to the Fix helper request"
    );

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();
}

#[test]
fn fix_ai_skips_helper_when_no_remaining_diagnostics() {
    let home = make_temp_dir("fix_ai_clean_home");
    let helper_src = make_temp_dir("fix_ai_clean_helper");
    let capture_path = helper_src.join("capture.json");

    // This response would succeed if the helper were called, but it shouldn't be.
    let response = make_fix_ai_helper_response(serde_json::json!([]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    // A clean program — no errors, no warnings, no fixable diagnostics.
    let file = make_temp_program(
        "fix_ai_clean",
        r#"action main {
    print("hello")
}
main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --ai on clean file");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected fidan fix --ai on clean file to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The helper should never have been invoked — capture file must not exist.
    assert!(
        !capture_path.exists(),
        "AI helper should not be called when there are no remaining diagnostics"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no fixes needed"),
        "expected 'no fixes needed' message for clean file:\n{stderr}"
    );
}

#[test]
fn fix_improve_calls_helper_on_clean_file() {
    let home = make_temp_dir("fix_improve_clean_home");
    let helper_src = make_temp_dir("fix_improve_clean_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 2,
            "line_end": 2,
            "old_text": "    print(\"hello\")",
            "new_text": "    print(\"hello, world\")",
            "reason": "Clarify the sample output"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program(
        "fix_improve_clean",
        r#"action main {
    print("hello")
}
main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .arg("--improve")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --improve on clean file");

    let patched = std::fs::read_to_string(&file).expect("read improved file");
    let captured = std::fs::read_to_string(&capture_path).expect("read helper capture");
    let request: fidan_driver::AiAnalysisHelperRequest =
        serde_json::from_str(&captured).expect("parse helper request");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected fidan fix --improve on clean file to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        patched.contains("print(\"hello, world\")"),
        "expected improve mode to apply the helper hunk:\n{patched}"
    );

    match request.command {
        fidan_driver::AiAnalysisHelperCommand::Fix {
            diagnostics,
            explain_context,
            mode,
            prompt,
            ..
        } => {
            assert!(
                diagnostics.is_empty(),
                "clean file should send no diagnostics"
            );
            assert_eq!(mode, AiFixMode::Improve);
            assert!(
                prompt.is_none(),
                "plain --improve should not inject extra prompt text"
            );
            let explain_context = explain_context.expect(
                "improve mode should bundle compiler-backed explain context for the current source",
            );
            assert_eq!(explain_context.file, file);
            assert!(
                !explain_context.module_outline.is_empty(),
                "expected module outline in explain context"
            );
            assert!(
                !explain_context.call_graph.is_empty(),
                "expected call graph in explain context"
            );
            assert!(
                explain_context.runtime_trace.is_some(),
                "expected runtime trace in explain context"
            );
        }
        fidan_driver::AiAnalysisHelperCommand::Explain { .. } => {
            panic!("unexpected Explain command")
        }
    }
}

#[test]
fn fix_refactor_alias_calls_helper_on_clean_file() {
    let home = make_temp_dir("fix_refactor_clean_home");
    let helper_src = make_temp_dir("fix_refactor_clean_helper");
    let capture_path = helper_src.join("capture.json");

    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 2,
            "line_end": 2,
            "old_text": "    print(\"hello\")",
            "new_text": "    print(len(\"hello\"))",
            "reason": "Demonstrate a small refactor"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program(
        "fix_refactor_clean",
        r#"action main {
    print("hello")
}
main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .arg("--refactor")
        .arg("prefer using built-ins")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --refactor on clean file");

    let patched = std::fs::read_to_string(&file).expect("read refactored file");
    let captured = std::fs::read_to_string(&capture_path).expect("read helper capture");
    let request: fidan_driver::AiAnalysisHelperRequest =
        serde_json::from_str(&captured).expect("parse helper request");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "expected fidan fix --refactor on clean file to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        patched.contains("print(len(\"hello\"))"),
        "expected refactor alias to apply the helper hunk:\n{patched}"
    );

    match request.command {
        fidan_driver::AiAnalysisHelperCommand::Fix {
            diagnostics,
            mode,
            prompt,
            ..
        } => {
            assert!(
                diagnostics.is_empty(),
                "clean file should send no diagnostics"
            );
            assert_eq!(mode, AiFixMode::Improve);
            assert_eq!(prompt.as_deref(), Some("prefer using built-ins"));
        }
        fidan_driver::AiAnalysisHelperCommand::Explain { .. } => {
            panic!("unexpected Explain command")
        }
    }
}

#[test]
fn fix_rejects_improve_and_refactor_together() {
    let file = make_temp_program(
        "fix_improve_refactor_conflict",
        r#"action main {
    print("hello")
}
main()
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg(&file)
        .arg("--improve")
        .arg("--refactor")
        .current_dir(workspace_root())
        .output()
        .expect("run conflicting improve/refactor flags");

    std::fs::remove_file(&file).ok();

    assert!(
        !output.status.success(),
        "expected conflicting flags to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflicts with"),
        "expected clap conflict error, got:\n{stderr}"
    );
}

#[test]
fn fix_ai_hunk_mismatch_is_warned_and_hunk_skipped() {
    let home = make_temp_dir("fix_ai_mismatch_home");
    let helper_src = make_temp_dir("fix_ai_mismatch_helper");
    let capture_path = helper_src.join("capture.json");

    // The hunk has old_text that does NOT match the source.
    let response = make_fix_ai_helper_response(serde_json::json!([
        {
            "line_start": 2,
            "line_end": 2,
            "old_text": "    this text does not exist in the source file",
            "new_text": "    print(\"should not appear\")",
            "reason": "test: intentional mismatch"
        }
    ]));
    let helper_path = make_fake_ai_helper(&helper_src, &response.to_string(), &capture_path);
    install_fake_ai_toolchain(&home, &helper_path);

    let file = make_temp_program("fix_ai_mismatch", make_fix_ai_source());
    let original_contents = std::fs::read_to_string(&file).expect("read original file");

    let output = Command::new(env!("CARGO_BIN_EXE_fidan"))
        .arg("fix")
        .arg("--in-place")
        .arg(&file)
        .arg("--ai")
        .env("FIDAN_HOME", &home)
        .current_dir(workspace_root())
        .output()
        .expect("run fidan fix --in-place --ai with mismatched hunk");

    let file_after = std::fs::read_to_string(&file).expect("read file after fix attempt");

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&helper_src).ok();

    assert!(
        output.status.success(),
        "a hunk mismatch should not cause a non-zero exit:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The file must remain unchanged since the only hunk mismatched.
    assert_eq!(
        file_after, original_contents,
        "file should be unchanged when AI hunk does not match"
    );

    // A warning should have been emitted about the mismatch.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`old_text` did not match source") || stderr.contains("skipped"),
        "expected mismatch warning in stderr:\n{stderr}"
    );
}
