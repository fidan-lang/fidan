use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn debug_target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target"))
        .join("debug")
}

fn fixture_dylib_path() -> PathBuf {
    let debug_dir = debug_target_dir();
    if cfg!(windows) {
        debug_dir.join("fidan_extern_fixture.dll")
    } else if cfg!(target_os = "macos") {
        debug_dir.join("libfidan_extern_fixture.dylib")
    } else {
        debug_dir.join("libfidan_extern_fixture.so")
    }
}

fn cargo_exe() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn build_fixture() -> PathBuf {
    let workspace = workspace_root();
    let output = Command::new(cargo_exe())
        .args(["build", "-p", "fidan-extern-fixture"])
        .current_dir(&workspace)
        .output()
        .expect("failed to build extern fixture");
    assert!(
        output.status.success(),
        "building fidan-extern-fixture failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let dylib = fixture_dylib_path();
    assert!(
        dylib.is_file(),
        "missing fixture dylib at `{}`",
        dylib.display()
    );
    dylib
}

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn with_runtime_env(command: &mut Command, runtime_dir: &Path) {
    #[cfg(windows)]
    {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![runtime_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        command.env("PATH", std::env::join_paths(paths).expect("join PATH"));
    }
    #[cfg(target_os = "macos")]
    {
        let current = std::env::var_os("DYLD_LIBRARY_PATH").unwrap_or_default();
        let mut paths = vec![runtime_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        command.env(
            "DYLD_LIBRARY_PATH",
            std::env::join_paths(paths).expect("join DYLD_LIBRARY_PATH"),
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let current = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
        let mut paths = vec![runtime_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        command.env(
            "LD_LIBRARY_PATH",
            std::env::join_paths(paths).expect("join LD_LIBRARY_PATH"),
        );
    }
}

#[test]
fn cli_run_boxed_extern_dylib_ok() {
    let dylib = build_fixture();
    let runtime_dir = dylib.parent().expect("fixture runtime dir").to_path_buf();
    let src = temp_dir("fidan_cli_extern").join("extern_cli_smoke.fdn");
    let dylib_path = dylib.display().to_string().replace('\\', "/");
    let program = format!(
        r#"@extern("{dylib_path}", symbol = "fidan_fixture_native_add")
action fixtureNativeAdd with (a oftype integer, b oftype integer) returns integer

@unsafe
@extern("{dylib_path}", symbol = "fidan_fixture_add_boxed", abi = "fidan")
action fixtureBoxedAdd with (a oftype integer, b oftype integer) returns integer

@unsafe
@extern("{dylib_path}", symbol = "fidan_fixture_echo_boxed", abi = "fidan")
action fixtureBoxedEcho with (text oftype string) returns string

assert_eq(fixtureNativeAdd(20, 22), 42)
assert_eq(fixtureBoxedAdd(10, 32), 42)
assert_eq(fixtureBoxedEcho("hello"), "hello")
"#
    );
    fs::write(&src, program).expect("write CLI extern smoke source");

    let mut command = Command::new(env!("CARGO_BIN_EXE_fidan"));
    command.arg("run").arg(&src);
    command.current_dir(workspace_root());
    with_runtime_env(&mut command, &runtime_dir);
    let output = command.output().expect("run fidan CLI");

    fs::remove_file(&src).ok();
    fs::remove_dir_all(src.parent().expect("temp dir")).ok();

    assert!(
        output.status.success(),
        "CLI boxed extern smoke failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
