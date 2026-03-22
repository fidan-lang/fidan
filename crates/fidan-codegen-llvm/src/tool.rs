use crate::model::{CompileRequest, LtoMode, ToolchainLayout};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn link_codegen_input(
    layout: &ToolchainLayout,
    request: &CompileRequest,
    input_path: &Path,
    object_output_path: &Path,
) -> Result<()> {
    if cfg!(target_os = "windows") {
        link_windows(layout, request, input_path, object_output_path)
    } else {
        link_unix(layout, request, input_path)
    }
}

fn link_windows(
    layout: &ToolchainLayout,
    request: &CompileRequest,
    input_path: &Path,
    _object_output_path: &Path,
) -> Result<()> {
    let linker = layout.linker_path();
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
        cmd.arg("fidan_runtime.dll.lib");
    } else {
        cmd.arg(
            find_static_runtime_lib(&request.runtime_dir)
                .context("cannot find fidan_runtime.lib — install/rebuild Fidan first")?,
        );
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
    Ok(())
}

fn link_unix(layout: &ToolchainLayout, request: &CompileRequest, input_path: &Path) -> Result<()> {
    let clang = layout.clang_driver_path();
    let mut cmd = Command::new(&clang);
    cmd.arg("-o").arg(&request.output).arg(input_path);
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
    cmd.args(["-lpthread", "-ldl", "-lm"]);
    #[cfg(target_os = "macos")]
    cmd.args(["-framework", "Security", "-framework", "CoreFoundation"]);
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
    Ok(())
}

fn configure_unix_link_environment(command: &mut Command, layout: &ToolchainLayout) {
    if !layout.lib_dir.is_dir() {
        return;
    }

    #[cfg(target_os = "linux")]
    prepend_env_path(command, "LD_LIBRARY_PATH", &layout.lib_dir);
    #[cfg(target_os = "macos")]
    {
        prepend_env_path(command, "DYLD_LIBRARY_PATH", &layout.lib_dir);
        prepend_env_path(command, "DYLD_FALLBACK_LIBRARY_PATH", &layout.lib_dir);
    }
}

fn prepend_env_path(command: &mut Command, key: &str, value: &Path) {
    let existing = std::env::var_os(key).unwrap_or_default();
    let mut paths = vec![value.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env(key, joined);
    }
}

fn find_static_runtime_lib(dir: &Path) -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &["fidan_runtime.lib", "libfidan_runtime.lib"]
    } else {
        &["libfidan_runtime.a"]
    };
    let mut matches = Vec::new();

    for &name in candidates {
        let path = dir.join(name);
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.is_file() {
                matches.push((metadata.modified().ok(), path));
            }
        }
    }

    let deps = dir.join("deps");
    for &name in candidates {
        let path = deps.join(name);
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.is_file() {
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
