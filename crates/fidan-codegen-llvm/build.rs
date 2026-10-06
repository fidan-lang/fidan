use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var_os("CARGO_FEATURE_LLVM_TOOLCHAIN_23").is_none()
    {
        return;
    }

    // llvm-sys's Windows static linking mixes upstream rpmalloc with Rust's CRT.
    // Its no-llvm-linking feature still builds the target-initialization wrappers;
    // use the official C API import library and keep LLVM's allocator in its DLL.
    let prefix = PathBuf::from(
        env::var_os("LLVM_SYS_231_PREFIX")
            .expect("Windows LLVM backend requires LLVM_SYS_231_PREFIX"),
    );
    let library = prefix.join("lib/LLVM-C.lib");
    assert!(
        library.is_file(),
        "missing LLVM C API import library: {}",
        library.display()
    );
    println!("cargo:rerun-if-changed={}", library.display());
    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-lib=dylib=LLVM-C");
}
