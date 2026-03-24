use fidan_driver::install::{installed_llvm_toolchains, resolve_fidan_home};
use fidan_driver::{
    Backend, CompileOptions, ExecutionMode, FrontendOutput, LtoMode, OptLevel, Session, StripMode,
    compile, compile_file_to_mir,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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

fn compile_program(source: &str, backend: Backend, output_path: &Path) {
    let src_path = output_path.with_extension("fdn");
    fs::write(&src_path, source).expect("write concurrent smoke source");
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
        extra_lib_dirs: vec![],
        link_dynamic: false,
        lto: LtoMode::Off,
        strip: StripMode::Off,
        backend,
    };
    compile(&Session::new(), mir, interner, &opts).expect("compile concurrent smoke program");
}

fn run_compiled_binary(bin: &Path) {
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
        stdout.contains("200000"),
        "expected concurrent smoke output to contain 200000, got:\n{}",
        stdout
    );
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
    run_compiled_binary(&output);
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
    run_compiled_binary(&output);
    fs::remove_dir_all(&sandbox).ok();
}
