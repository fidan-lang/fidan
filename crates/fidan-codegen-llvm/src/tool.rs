use crate::model::{CompileRequest, LtoMode, StripMode, ToolchainLayout};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn link_codegen_input(
    layout: &ToolchainLayout,
    request: &CompileRequest,
    input_path: &Path,
    object_output_path: &Path,
) -> Result<()> {
    let extern_link_inputs = collect_extern_link_inputs(request);
    if cfg!(target_os = "windows") {
        link_windows(
            layout,
            request,
            input_path,
            object_output_path,
            &extern_link_inputs,
        )
    } else {
        link_unix(layout, request, input_path, &extern_link_inputs)
    }
}

fn link_windows(
    layout: &ToolchainLayout,
    request: &CompileRequest,
    input_path: &Path,
    _object_output_path: &Path,
    extern_link_inputs: &[String],
) -> Result<()> {
    let linker = resolve_windows_linker(layout)?;
    let mut cmd = Command::new(&linker);
    cmd.arg(format!("/OUT:{}", request.output.display()));
    cmd.arg("/SUBSYSTEM:CONSOLE");
    cmd.arg(input_path);

    for dir in find_msvc_lib_paths() {
        cmd.arg(format!("/LIBPATH:{}", dir.display()));
    }
    for dir in &request.extra_lib_dirs {
        cmd.arg(format!("/LIBPATH:{}", dir.display()));
    }
    cmd.arg(format!("/LIBPATH:{}", request.runtime_dir.display()));
    if request.link_dynamic {
        cmd.arg(
            find_dynamic_runtime_import_lib(&request.runtime_dir)
                .context("cannot find `fidan_runtime.dll.lib` — install/rebuild Fidan first")?,
        );
    } else {
        cmd.arg(
            find_static_runtime_lib(&request.runtime_dir)
                .context("cannot find `fidan_runtime.lib` — install/rebuild Fidan first")?,
        );
    }
    for input in extern_link_inputs {
        append_windows_link_input(&mut cmd, input);
    }
    cmd.args([
        "kernel32.lib",
        "ucrt.lib",
        "msvcrt.lib",
        "vcruntime.lib",
        "ws2_32.lib",
        "userenv.lib",
        "ntdll.lib",
        "bcrypt.lib",
        "advapi32.lib",
    ]);

    let output = cmd
        .output()
        .with_context(|| format!("failed to launch linker `{}`", linker.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(": {stdout}"),
            (true, false) => format!(": {stderr}"),
            (false, false) => format!(":\n{stdout}\n{stderr}"),
        };
        bail!(
            "linker `{}` exited with code {:?}{}",
            linker.display(),
            output.status.code(),
            detail
        );
    }
    strip_binary(layout, request)?;
    Ok(())
}

fn link_unix(
    layout: &ToolchainLayout,
    request: &CompileRequest,
    input_path: &Path,
    extern_link_inputs: &[String],
) -> Result<()> {
    let clang = resolve_unix_linker_driver(layout)?;
    let mut cmd = Command::new(&clang);
    cmd.arg("-o").arg(&request.output).arg(input_path);
    #[cfg(target_os = "macos")]
    {
        let sdk_root = resolve_macos_sdk_root()?;
        cmd.arg("-isysroot").arg(&sdk_root);
        cmd.env("SDKROOT", sdk_root);
    }
    if request.lto == LtoMode::Full {
        cmd.arg("-flto=full");
        #[cfg(target_os = "linux")]
        cmd.arg("-fuse-ld=lld");
    }
    for dir in &request.extra_lib_dirs {
        cmd.arg(format!("-L{}", dir.display()));
    }
    cmd.arg(format!("-L{}", request.runtime_dir.display()));
    if request.link_dynamic {
        cmd.arg("-lfidan_runtime");
        cmd.arg(format!("-Wl,-rpath,{}", request.runtime_dir.display()));
        if !cfg!(target_os = "macos") {
            cmd.arg("-Wl,--enable-new-dtags");
        }
    } else {
        cmd.arg(
            find_static_runtime_lib(&request.runtime_dir)
                .context("cannot find the Fidan runtime library — install/rebuild Fidan first")?,
        );
    }
    for input in extern_link_inputs {
        append_unix_link_input(&mut cmd, input);
    }
    #[cfg(target_os = "linux")]
    cmd.args(["-lpthread", "-ldl", "-lm"]);
    configure_unix_link_environment(&mut cmd, layout);

    let output = cmd
        .output()
        .with_context(|| format!("failed to launch linker driver `{}`", clang.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(": {stdout}"),
            (true, false) => format!(": {stderr}"),
            (false, false) => format!(":\n{stdout}\n{stderr}"),
        };
        bail!(
            "linker driver `{}` exited with code {:?}{}",
            clang.display(),
            output.status.code(),
            detail
        );
    }
    strip_binary(layout, request)?;
    Ok(())
}

fn collect_extern_link_inputs(request: &CompileRequest) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut inputs = Vec::new();
    for function in &request.payload.program.functions {
        let Some(extern_decl) = &function.extern_decl else {
            continue;
        };
        let raw = extern_decl
            .link
            .as_deref()
            .unwrap_or(extern_decl.lib.as_str())
            .trim();
        if raw.is_empty() || raw == "self" {
            continue;
        }
        if seen.insert(raw.to_owned()) {
            inputs.push(raw.to_owned());
        }
    }
    inputs
}

fn append_windows_link_input(command: &mut Command, input: &str) {
    let resolved = resolve_windows_link_input(input);
    let path_like = input.contains(std::path::MAIN_SEPARATOR)
        || input.contains('/')
        || input.contains('\\')
        || input.contains(':');
    if path_like {
        command.arg(&resolved);
        return;
    }

    let has_lib_suffix = Path::new(&resolved)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("lib"))
        .unwrap_or(false);
    if has_lib_suffix {
        command.arg(&resolved);
    } else {
        command.arg(format!("{resolved}.lib"));
    }
}

fn resolve_windows_link_input(input: &str) -> String {
    let path = Path::new(input);
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    if ext.as_deref() != Some("dll") {
        return input.to_owned();
    }

    let dll_lib = PathBuf::from(format!("{input}.lib"));
    if dll_lib.is_file() {
        return dll_lib.to_string_lossy().into_owned();
    }

    let import_lib = path.with_extension("lib");
    if import_lib.is_file() {
        return import_lib.to_string_lossy().into_owned();
    }

    dll_lib.to_string_lossy().into_owned()
}

fn append_unix_link_input(command: &mut Command, input: &str) {
    let path_like =
        input.contains(std::path::MAIN_SEPARATOR) || input.contains('/') || input.contains('\\');
    let explicit_library = [".a", ".so", ".dylib", ".tbd"]
        .iter()
        .any(|suffix| input.ends_with(suffix));
    if path_like || explicit_library {
        command.arg(input);
    } else {
        command.arg(format!("-l{input}"));
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_windows_link_input;

    #[test]
    fn windows_link_input_prefers_dll_lib_sidecar() {
        let temp = std::env::temp_dir().join(format!(
            "fidan_llvm_link_input_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let dll = temp.join("ffi_demo.dll");
        let dll_lib = temp.join("ffi_demo.dll.lib");
        std::fs::write(&dll, []).expect("write dll placeholder");
        std::fs::write(&dll_lib, []).expect("write import lib placeholder");

        let resolved = resolve_windows_link_input(&dll.to_string_lossy());
        assert_eq!(resolved, dll_lib.to_string_lossy());

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn windows_link_input_falls_back_to_plain_lib_sidecar() {
        let temp = std::env::temp_dir().join(format!(
            "fidan_llvm_link_input_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let dll = temp.join("ffi_demo.dll");
        let lib = temp.join("ffi_demo.lib");
        std::fs::write(&dll, []).expect("write dll placeholder");
        std::fs::write(&lib, []).expect("write import lib placeholder");

        let resolved = resolve_windows_link_input(&dll.to_string_lossy());
        assert_eq!(resolved, lib.to_string_lossy());

        std::fs::remove_dir_all(&temp).ok();
    }
}

fn resolve_windows_linker(layout: &ToolchainLayout) -> Result<PathBuf> {
    let linker = std::env::var_os("FIDAN_LINKER")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| layout.linker_path());
    let stem = linker
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(stem.as_str(), "link" | "lld-link") {
        Ok(linker)
    } else {
        bail!(
            "LLVM backend on Windows requires a LINK-style linker (`link.exe` or `lld-link.exe`), got `{}`",
            linker.display()
        )
    }
}

fn resolve_unix_linker_driver(layout: &ToolchainLayout) -> Result<PathBuf> {
    let linker = std::env::var_os("FIDAN_LINKER")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| layout.clang_driver_path());
    let stem = linker
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(stem.as_str(), "cc" | "gcc" | "clang" | "clang++") {
        Ok(linker)
    } else {
        bail!(
            "LLVM backend on Unix requires a compiler-driver linker override (`cc`, `gcc`, `clang`, or `clang++`), got `{}`",
            linker.display()
        )
    }
}

fn strip_binary(layout: &ToolchainLayout, request: &CompileRequest) -> Result<()> {
    let mode = request.strip;
    if mode == StripMode::Off {
        return Ok(());
    }

    let strip = layout.strip_path()?;
    let mut cmd = Command::new(&strip);
    match mode {
        StripMode::Off => return Ok(()),
        StripMode::Symbols => {
            cmd.arg("--strip-unneeded");
        }
        StripMode::All => {
            cmd.arg("--strip-all");
        }
    }
    cmd.arg(&request.output);

    let output = cmd
        .output()
        .with_context(|| format!("failed to launch strip tool `{}`", strip.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(": {stdout}"),
            (true, false) => format!(": {stderr}"),
            (false, false) => format!(":\n{stdout}\n{stderr}"),
        };
        bail!(
            "strip tool `{}` exited with code {:?}{}",
            strip.display(),
            output.status.code(),
            detail
        );
    }
    Ok(())
}

fn configure_unix_link_environment(_command: &mut Command, layout: &ToolchainLayout) {
    if !layout.lib_dir.is_dir() {
        return;
    }

    #[cfg(target_os = "linux")]
    prepend_env_path(_command, "LD_LIBRARY_PATH", &layout.lib_dir);
    #[cfg(target_os = "macos")]
    {
        // Do not force LLVM's bundled libc++/libunwind onto the packaged clang
        // driver via DYLD_* on macOS. The official archive's driver expects the
        // host runtime layout, and overriding it breaks clang startup itself.
        _command.env_remove("DYLD_LIBRARY_PATH");
        _command.env_remove("DYLD_FALLBACK_LIBRARY_PATH");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prepend_env_path(command: &mut Command, key: &str, value: &Path) {
    let existing = std::env::var_os(key).unwrap_or_default();
    let mut paths = vec![value.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env(key, joined);
    }
}

#[cfg(target_os = "macos")]
fn resolve_macos_sdk_root() -> Result<PathBuf> {
    let output = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .context("failed to launch `xcrun --show-sdk-path` for macOS LLVM linking")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(": {stdout}"),
            (true, false) => format!(": {stderr}"),
            (false, false) => format!(":\n{stdout}\n{stderr}"),
        };
        bail!(
            "failed to resolve macOS SDK path with `xcrun --show-sdk-path`{}",
            detail
        );
    }

    let sdk = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if sdk.is_empty() {
        bail!("`xcrun --show-sdk-path` returned an empty macOS SDK path");
    }
    Ok(PathBuf::from(sdk))
}

fn find_static_runtime_lib(dir: &Path) -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &["fidan_runtime.lib", "libfidan_runtime.lib"]
    } else {
        &["libfidan_runtime.a"]
    };
    find_latest_runtime_artifact(dir, candidates)
}

fn find_dynamic_runtime_import_lib(dir: &Path) -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        find_latest_runtime_artifact(dir, &["fidan_runtime.dll.lib", "libfidan_runtime.dll.lib"])
    } else {
        None
    }
}

fn find_latest_runtime_artifact(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut matches = Vec::new();

    for candidate_dir in [Some(dir), Some(&dir.join("deps"))].into_iter().flatten() {
        for &name in names {
            let path = candidate_dir.join(name);
            if let Ok(metadata) = std::fs::metadata(&path)
                && metadata.is_file()
            {
                matches.push((metadata.modified().ok(), path));
            }
        }
    }

    matches.sort_by_key(|(modified, path)| (*modified, path.clone()));
    matches.pop().map(|(_, path)| path)
}

#[cfg(target_os = "windows")]
fn find_msvc_lib_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let vswhere_candidates = [
        r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe",
        r"C:\Program Files\Microsoft Visual Studio\Installer\vswhere.exe",
    ];
    for vswhere in &vswhere_candidates {
        if let Ok(out) = Command::new(vswhere)
            .args(["-latest", "-property", "installationPath"])
            .output()
            && out.status.success()
        {
            let vs_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !vs_path.is_empty() {
                let msvc_root = PathBuf::from(&vs_path).join(r"VC\Tools\MSVC");
                if let Ok(entries) = std::fs::read_dir(&msvc_root) {
                    let mut versions: Vec<PathBuf> = entries
                        .flatten()
                        .filter(|entry| entry.path().is_dir())
                        .map(|entry| entry.path())
                        .collect();
                    versions.sort();
                    if let Some(latest) = versions.last() {
                        let lib = latest.join(r"lib\x64");
                        if lib.exists() {
                            paths.push(lib);
                        }
                    }
                }
                break;
            }
        }
    }

    let sdk_root = query_registry_value(
        r"HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
        "KitsRoot10",
    )
    .or_else(|| {
        query_registry_value(
            r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots",
            "KitsRoot10",
        )
    });

    if let Some(root) = sdk_root {
        let lib_root = PathBuf::from(&root).join("Lib");
        if let Ok(entries) = std::fs::read_dir(&lib_root) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.path())
                .collect();
            versions.sort();
            if let Some(latest) = versions.last() {
                let um = latest.join(r"um\x64");
                let ucrt = latest.join(r"ucrt\x64");
                if um.exists() {
                    paths.push(um);
                }
                if ucrt.exists() {
                    paths.push(ucrt);
                }
            }
        }
    }

    paths
}

#[cfg(not(target_os = "windows"))]
fn find_msvc_lib_paths() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "windows")]
fn query_registry_value(key: &str, value_name: &str) -> Option<String> {
    let out = Command::new("reg")
        .args(["query", key, "/v", value_name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with(value_name) {
            continue;
        }
        let rest = line[value_name.len()..].trim();
        if let Some(pos) = rest.find("    ") {
            return Some(rest[pos..].trim().to_string());
        }
    }
    None
}
