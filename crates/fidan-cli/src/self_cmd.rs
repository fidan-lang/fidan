use crate::distribution::{
    binary_relpath, extract_tar_gz, fetch_bytes, fetch_manifest, materialize_release_root,
    read_all, select_fidan_release, stage_dir, verify_sha256, write_bytes,
};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::install::{
    load_or_repair_metadata, register_install, remove_bootstrap_path_entries,
    remove_install_record, resolve_current_binary, scan_installed_versions, set_active_version,
};
#[cfg(target_os = "windows")]
use fidan_driver::install::{
    persist_active_version, read_current_version_from_pointer, resolve_powershell_exe,
    schedule_current_pointer_update,
};
use fidan_driver::{resolve_fidan_home, resolve_install_root};
use std::fs;
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::{os::windows::process::CommandExt, process::Stdio};

#[derive(Subcommand)]
pub(crate) enum SelfCommand {
    /// List installed Fidan versions
    List,
    /// Show the active Fidan version and install paths
    Current,
    /// Install a Fidan version from the distribution manifest (default: `latest`)
    Install { version: Option<String> },
    /// Switch the active Fidan version (default: `latest`)
    Use { version: Option<String> },
    /// Remove an installed Fidan version (default: `latest`)
    Remove { version: Option<String> },
}

pub(crate) fn run(command: SelfCommand) -> Result<()> {
    match command {
        SelfCommand::List => run_list(),
        SelfCommand::Current => run_current(),
        SelfCommand::Install { version } => {
            let version = version.unwrap_or_else(|| "latest".to_string());
            run_install(&version)
        }
        SelfCommand::Use { version } => {
            let version = version.unwrap_or_else(|| "latest".to_string());
            run_use(&version)
        }
        SelfCommand::Remove { version } => {
            let version = version.unwrap_or_else(|| "latest".to_string());
            run_remove(&version)
        }
    }
}

fn run_list() -> Result<()> {
    let root = resolve_install_root()?;
    let installed = scan_installed_versions(&root)?;
    if installed.is_empty() {
        render_message_to_stderr(
            Severity::Note,
            "self",
            "no Fidan versions are installed yet",
        );
        return Ok(());
    }
    let (active, _) = load_or_repair_metadata(&root)?;
    for version in installed {
        let marker = if version == active.active_version {
            "*"
        } else {
            " "
        };
        println!("{marker} {version}");
    }
    Ok(())
}

fn run_current() -> Result<()> {
    let root = resolve_install_root()?;
    let home = resolve_fidan_home()?;
    let (active, _) = load_or_repair_metadata(&root)?;
    let current_binary = resolve_current_binary(&root)?;
    let lines = [
        format!("version       {}", active.active_version),
        format!("install root  {}", root.display()),
        format!("current bin   {}", current_binary.display()),
        format!("fidan home    {}", home.display()),
    ];
    render_message_to_stderr(Severity::Note, "self", &lines.join("\n"));
    Ok(())
}

fn run_install(version: &str) -> Result<()> {
    let root = resolve_install_root()?;
    let home = resolve_fidan_home()?;
    let manifest = fetch_manifest(None)?;
    let host = fidan_driver::install::host_triple();
    let release = select_fidan_release(&manifest, Some(version), &host)?;

    let cache_path = home
        .join("cache")
        .join("downloads")
        .join(format!("fidan-{}-{}.tar.gz", release.version, host));
    let bytes = fetch_bytes(&release.url)?;
    verify_sha256(&bytes, &release.sha256)?;
    write_bytes(&cache_path, &bytes)?;
    let archive = read_all(&cache_path)?;

    let versions_dir = fidan_driver::install::versions_dir(&root);
    fs::create_dir_all(&versions_dir)
        .with_context(|| format!("failed to create `{}`", versions_dir.display()))?;
    let final_dir = versions_dir.join(&release.version);
    if final_dir.exists() {
        bail!("Fidan version `{}` is already installed", release.version);
    }

    let staging = stage_dir(&versions_dir, &format!("fidan-{}", release.version));
    extract_tar_gz(&archive, &staging)?;
    let expected = PathBuf::from(
        release
            .binary_relpath
            .as_deref()
            .unwrap_or(binary_relpath()),
    );
    materialize_release_root(&staging, &expected, &final_dir)?;

    let first_install = register_install(&root, &release.version)?;
    let message = if first_install {
        format!(
            "installed Fidan {} and made it active — PATH should point to `{}`",
            release.version,
            fidan_driver::install::current_dir(&root).display()
        )
    } else {
        format!(
            "installed Fidan {} — run `fidan self use {}` to activate it",
            release.version, release.version
        )
    };
    render_message_to_stderr(Severity::Note, "self", &message);
    Ok(())
}

fn run_use(version: &str) -> Result<()> {
    let root = resolve_install_root()?;
    let version = resolve_version_selector(&root, version)?;

    #[cfg(target_os = "windows")]
    {
        if read_current_version_from_pointer(&root)?.as_deref() != Some(version.as_str()) {
            persist_active_version(&root, &version)?;
            schedule_current_pointer_update(&root, &version)?;
            render_message_to_stderr(
                Severity::Note,
                "self",
                &format!(
                    "scheduled active Fidan version switch to `{version}` — open a new shell after this command exits"
                ),
            );
            return Ok(());
        }
    }

    set_active_version(&root, &version)?;
    render_message_to_stderr(
        Severity::Note,
        "self",
        &format!("active Fidan version is now `{version}`"),
    );
    Ok(())
}

fn run_remove(version: &str) -> Result<()> {
    let root = resolve_install_root()?;
    let home = resolve_fidan_home()?;
    let version = resolve_version_selector(&root, version)?;
    let installed = scan_installed_versions(&root)?;
    let (active, _) = load_or_repair_metadata(&root)?;
    let is_active = active.active_version == version;
    if is_active && installed.len() > 1 {
        bail!(
            "cannot remove the active Fidan version `{version}` while other versions are installed — switch first with `fidan self use <other-version>`"
        );
    }

    if is_active && installed.len() == 1 {
        let purge_home = prompt_yes_no("also purge FIDAN_HOME shared data?", false)?;
        if let Err(error) = remove_bootstrap_path_entries(&root) {
            render_message_to_stderr(
                Severity::Warning,
                "self",
                &format!("failed to remove the Fidan PATH entry automatically\n  cause: {error}"),
            );
        }
        schedule_last_uninstall_cleanup(
            &root,
            if purge_home {
                Some(home.as_path())
            } else {
                None
            },
        )?;
        render_message_to_stderr(
            Severity::Note,
            "self",
            "scheduled cleanup for the last installed Fidan version — the install root will be removed after this process exits",
        );
        return Ok(());
    }

    fs::remove_dir_all(fidan_driver::install::versions_dir(&root).join(&version))
        .with_context(|| format!("failed to remove installed version directory for `{version}`"))?;
    let _ = remove_install_record(&root, &version)?;
    render_message_to_stderr(
        Severity::Note,
        "self",
        &format!("removed Fidan version `{version}`"),
    );
    Ok(())
}

fn resolve_version_selector(root: &std::path::Path, version: &str) -> Result<String> {
    let installed = scan_installed_versions(root)?;
    if installed.is_empty() {
        bail!("no Fidan versions are installed yet");
    }

    if version == "latest" {
        return installed
            .into_iter()
            .next()
            .context("no Fidan versions are installed yet");
    }

    if installed.iter().any(|entry| entry == version) {
        Ok(version.to_string())
    } else {
        bail!("Fidan version `{version}` is not installed");
    }
}

fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    use std::io::{self, Write};

    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    print!("{prompt} {suffix} ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to read response")?;
    let trimmed = line.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

fn schedule_last_uninstall_cleanup(
    root: &std::path::Path,
    purge_home: Option<&std::path::Path>,
) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut script = format!(
            "$ErrorActionPreference = 'SilentlyContinue'; \
             Start-Sleep -Milliseconds 900; \
             if (Test-Path -LiteralPath {root}) {{ Remove-Item -LiteralPath {root} -Force -Recurse; }}",
            root = powershell_literal(root)
        );
        if let Some(home) = purge_home {
            script.push_str(&format!(
                "; if (Test-Path -LiteralPath {home}) {{ Remove-Item -LiteralPath {home} -Force -Recurse; }}",
                home = powershell_literal(home)
            ));
        }
        spawn_hidden_powershell(&script).context("failed to schedule Windows cleanup process")?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let script = if let Some(home) = purge_home {
            format!("sleep 1; rm -rf '{}' '{}';", root.display(), home.display())
        } else {
            format!("sleep 1; rm -rf '{}';", root.display())
        };
        std::process::Command::new("sh")
            .args(["-c", &script])
            .spawn()
            .context("failed to schedule POSIX cleanup process")?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn powershell_literal(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

#[cfg(target_os = "windows")]
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
