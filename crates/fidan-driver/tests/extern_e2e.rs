use fidan_driver::install::{installed_llvm_toolchains, resolve_fidan_home};
use fidan_driver::{
    Backend, CompileOptions, ExecutionMode, FrontendOutput, LtoMode, OptLevel, Session, StripMode,
    compile, compile_file_to_mir,
};
use fidan_interp::run_mir;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
struct FixtureArtifacts {
    dylib_path: PathBuf,
    link_input_path: PathBuf,
    runtime_dir: PathBuf,
}

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

fn fixture_name() -> &'static str {
    "fidan_extern_fixture"
}

fn cargo_exe() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn build_fixture_artifacts() -> &'static FixtureArtifacts {
    static ONCE: OnceLock<FixtureArtifacts> = OnceLock::new();
    ONCE.get_or_init(|| {
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

        let debug_dir = debug_target_dir();
        let dylib_path = if cfg!(windows) {
            debug_dir.join(format!("{}.dll", fixture_name()))
        } else if cfg!(target_os = "macos") {
            debug_dir.join(format!("lib{}.dylib", fixture_name()))
        } else {
            debug_dir.join(format!("lib{}.so", fixture_name()))
        };
        assert!(
            dylib_path.is_file(),
            "missing built extern fixture dylib at `{}`",
            dylib_path.display()
        );

        let link_input_path = if cfg!(windows) {
            let candidates = [
                debug_dir.join(format!("{}.dll.lib", fixture_name())),
                debug_dir.join(format!("{}.lib", fixture_name())),
            ];
            candidates
                .into_iter()
                .find(|path| path.is_file())
                .unwrap_or_else(|| {
                    panic!(
                        "missing Windows import library for extern fixture in `{}`",
                        debug_dir.display()
                    )
                })
        } else {
            dylib_path.clone()
        };

        FixtureArtifacts {
            runtime_dir: dylib_path
                .parent()
                .expect("fixture runtime dir")
                .to_path_buf(),
            dylib_path,
            link_input_path,
        }
    })
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

fn as_fidan_string(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn build_test_source(fixture: &FixtureArtifacts) -> String {
    let lib = as_fidan_string(&fixture.dylib_path);
    let link = as_fidan_string(&fixture.link_input_path);
    format!(
        r#"@extern("{lib}", link = "{link}")
action defaultExternAdd with (a oftype integer, b oftype integer) returns integer

@extern("{lib}", symbol = "fidan_fixture_native_add", link = "{link}")
action nativeAdd with (a oftype integer, b oftype integer) returns integer

@extern("{lib}", symbol = "fidan_fixture_make_handle", link = "{link}")
action makeHandle returns handle

@extern("{lib}", symbol = "fidan_fixture_inc_handle", link = "{link}")
action incHandle with (h oftype handle) returns handle

@extern("{lib}", symbol = "fidan_fixture_read_handle", link = "{link}")
action readHandle with (h oftype handle) returns integer

@extern("{lib}", symbol = "fidan_fixture_free_handle", link = "{link}")
action freeHandle with (h oftype handle)

@unsafe
@extern("{lib}", symbol = "fidan_fixture_add_boxed", abi = "fidan", link = "{link}")
action boxedAdd with (a oftype integer, b oftype integer) returns integer

@unsafe
@extern("{lib}", symbol = "fidan_fixture_echo_boxed", abi = "fidan", link = "{link}")
action boxedEcho with (text oftype string) returns string

action main {{
    assert_eq(defaultExternAdd(20, 22), 42)
    assert_eq(nativeAdd(20, 22), 42)

    var h = makeHandle()
    h = incHandle(h)
    assert_eq(readHandle(h), 42)
    freeHandle(h)

    assert_eq(boxedAdd(10, 32), 42)
    assert_eq(boxedEcho("hello"), "hello")
}}
"#
    )
}

fn compile_fixture_program(
    source: &str,
    backend: Backend,
    output_path: &Path,
    extra_lib_dirs: &[PathBuf],
) {
    let src_path = output_path.with_extension("fdn");
    fs::write(&src_path, source).expect("write extern smoke source");
    let FrontendOutput { interner, mir, .. } =
        compile_file_to_mir(&src_path).expect("compile source to MIR");
    let opts = CompileOptions {
        input: src_path,
        output: Some(output_path.to_path_buf()),
        mode: ExecutionMode::Build,
        emit: vec![],
        trace: fidan_driver::TraceMode::None,
        max_errors: None,
        jit_threshold: 0,
        strict_mode: false,
        replay_inputs: vec![],
        suppress: vec![],
        sandbox: None,
        opt_level: OptLevel::O2,
        extra_lib_dirs: extra_lib_dirs.to_vec(),
        link_dynamic: false,
        lto: LtoMode::Off,
        strip: StripMode::Off,
        backend,
    };
    compile(&Session::new(), mir, interner, &opts).expect("compile extern smoke program");
}

fn runtime_env_with_fixture_dir(dir: &Path) -> Vec<(String, OsString)> {
    let mut vars = Vec::new();
    if cfg!(windows) {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        vars.push((
            "PATH".to_string(),
            std::env::join_paths(paths).expect("join PATH"),
        ));
    } else if cfg!(target_os = "macos") {
        let current = std::env::var_os("DYLD_LIBRARY_PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        vars.push((
            "DYLD_LIBRARY_PATH".to_string(),
            std::env::join_paths(paths).expect("join DYLD_LIBRARY_PATH"),
        ));
    } else {
        let current = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        vars.push((
            "LD_LIBRARY_PATH".to_string(),
            std::env::join_paths(paths).expect("join LD_LIBRARY_PATH"),
        ));
    }
    vars
}

fn run_compiled_binary(bin: &Path, runtime_dir: &Path) {
    let mut command = Command::new(bin);
    for (key, value) in runtime_env_with_fixture_dir(runtime_dir) {
        command.env(key, value);
    }
    let output = command.output().expect("run compiled extern smoke binary");
    assert!(
        output.status.success(),
        "compiled extern smoke binary failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_interpreter_source(source: &str) {
    let sandbox = temp_dir("fidan_extern_interp");
    let src = sandbox.join("extern_smoke.fdn");
    fs::write(&src, source).expect("write interpreter source");
    let frontend = compile_file_to_mir(&src).expect("frontend for interpreter extern smoke");
    let result = run_mir(frontend.mir, frontend.interner, frontend.source_map);
    if let Err(err) = result {
        panic!("{}: {}", err.code, err.message);
    }
    fs::remove_dir_all(&sandbox).ok();
}

fn llvm_available() -> bool {
    resolve_fidan_home()
        .ok()
        .and_then(|home| installed_llvm_toolchains(&home).ok())
        .is_some_and(|toolchains| !toolchains.is_empty())
}

#[test]
fn extern_fixture_interpreter_dynamic_library_ok() {
    let fixture = build_fixture_artifacts().clone();
    let source = build_test_source(&fixture);
    run_interpreter_source(&source);
}

#[test]
fn extern_fixture_cranelift_aot_ok() {
    let fixture = build_fixture_artifacts().clone();
    let source = build_test_source(&fixture);
    let sandbox = temp_dir("fidan_extern_cranelift");
    let output = if cfg!(windows) {
        sandbox.join("extern_smoke.exe")
    } else {
        sandbox.join("extern_smoke")
    };
    compile_fixture_program(
        &source,
        Backend::Cranelift,
        &output,
        std::slice::from_ref(&fixture.runtime_dir),
    );
    run_compiled_binary(&output, &fixture.runtime_dir);
    fs::remove_dir_all(&sandbox).ok();
}

#[test]
fn extern_fixture_llvm_aot_ok() {
    if !llvm_available() {
        eprintln!(
            "skipping LLVM extern AOT smoke test because no compatible LLVM toolchain is installed"
        );
        return;
    }

    let fixture = build_fixture_artifacts().clone();
    let source = build_test_source(&fixture);
    let sandbox = temp_dir("fidan_extern_llvm");
    let output = if cfg!(windows) {
        sandbox.join("extern_smoke.exe")
    } else {
        sandbox.join("extern_smoke")
    };
    compile_fixture_program(
        &source,
        Backend::Llvm,
        &output,
        std::slice::from_ref(&fixture.runtime_dir),
    );
    run_compiled_binary(&output, &fixture.runtime_dir);
    fs::remove_dir_all(&sandbox).ok();
}
