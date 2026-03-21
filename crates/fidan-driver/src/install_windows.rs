use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{os::windows::process::CommandExt, process::Stdio};

pub fn remove_user_path_entries(current: &Path) -> Result<bool> {
    let current_text = current.to_string_lossy().to_string();
    let normalized_current = normalize_windows_path_entry(&current_text);
    let stored_path = current_user_path_value()?.unwrap_or_default();
    let mut changed = false;
    let filtered = stored_path
        .split(';')
        .filter_map(|entry| {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                return None;
            }
            if normalize_windows_path_entry(trimmed) == normalized_current {
                changed = true;
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join(";");

    if changed {
        persist_user_path_value(&filtered)?;
        // SAFETY: this short-lived CLI updates its own environment after persisting the
        // user PATH so child processes launched afterward observe the same value.
        unsafe {
            std::env::set_var("Path", &filtered);
            std::env::set_var("PATH", &filtered);
        }
    }

    Ok(changed)
}

pub fn schedule_directory_pointer_update(current: &Path, target: &Path) -> Result<()> {
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         Start-Sleep -Milliseconds 900; \
         if (Test-Path -LiteralPath {current}) {{ Remove-Item -LiteralPath {current} -Force -Recurse; }}; \
         New-Item -ItemType Junction -Path {current} -Target {target} | Out-Null",
        current = powershell_literal(current),
        target = powershell_literal(target)
    );

    spawn_hidden_powershell(&script)
        .context("failed to schedule Windows current-version switch")?;
    Ok(())
}

pub fn schedule_cleanup(paths: &[&Path]) -> Result<()> {
    let joined = paths
        .iter()
        .map(|path| powershell_literal(path))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'; \
         Start-Sleep -Milliseconds 900; \
         foreach ($path in @({joined})) {{ \
             if (Test-Path -LiteralPath $path) {{ \
                 Remove-Item -LiteralPath $path -Force -Recurse; \
             }} \
         }}",
    );
    spawn_hidden_powershell(&script).context("failed to schedule Windows cleanup process")?;
    Ok(())
}

pub fn resolve_powershell_exe() -> OsString {
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let explicit = PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if explicit.is_file() {
            return explicit.into_os_string();
        }
    }

    OsString::from("powershell.exe")
}

fn normalize_windows_path_entry(text: &str) -> String {
    text.trim()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

fn powershell_literal(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn powershell_string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn spawn_hidden_powershell(script: &str) -> Result<std::process::Child> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new(resolve_powershell_exe())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("failed to spawn hidden Windows PowerShell helper")
}

fn current_user_path_value() -> Result<Option<String>> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = std::process::Command::new(resolve_powershell_exe())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path', 'User')",
        ])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("failed to query Windows user PATH")?;
    if !output.status.success() {
        bail!("failed to query Windows user PATH");
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text))
    }
}

fn persist_user_path_value(value: &str) -> Result<()> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let script = format!(
        "[Environment]::SetEnvironmentVariable('Path', {}, 'User')",
        powershell_string_literal(value)
    );
    let status = std::process::Command::new(resolve_powershell_exe())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .context("failed to persist Windows user PATH")?;
    if !status.success() {
        bail!("failed to persist Windows user PATH");
    }
    Ok(())
}
