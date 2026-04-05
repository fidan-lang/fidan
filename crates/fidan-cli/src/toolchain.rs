use crate::distribution::{
    extract_tar_gz, fetch_cached_bytes, fetch_manifest, materialize_release_root,
    select_toolchain_release, stage_dir, write_bytes,
};
use crate::prompts::prompt_yes_no;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::{
    ResolvedToolchain, ToolchainMetadata, progress::ProgressReporter, resolve_fidan_home,
};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Subcommand)]
pub(crate) enum ToolchainCommand {
    /// Show installable toolchains for the current host
    Available,
    /// Show installed toolchains
    List,
    /// Install an optional toolchain package
    Add {
        name: String,
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove an installed toolchain package
    Remove {
        name: String,
        #[arg(long)]
        version: Option<String>,
        /// Skip the interactive confirmation prompt
        #[arg(long)]
        confirm: bool,
    },
}

pub(crate) fn run(command: ToolchainCommand) -> Result<()> {
    match command {
        ToolchainCommand::Available => run_available(),
        ToolchainCommand::List => run_list(),
        ToolchainCommand::Add { name, version } => run_add(&name, version.as_deref()),
        ToolchainCommand::Remove {
            name,
            version,
            confirm,
        } => run_remove(&name, version.as_deref(), confirm),
    }
}

pub(crate) fn install_toolchain(name: &str, version: Option<&str>) -> Result<()> {
    run_add(name, version)
}

pub(crate) fn ensure_ai_toolchain_installed() -> Result<ResolvedToolchain> {
    let home = resolve_fidan_home()?;
    let installed = fidan_driver::install::installed_toolchains(&home, Some("ai-analysis"))?;
    if let Some(toolchain) = installed.iter().find(|toolchain| {
        toolchain.metadata.backend_protocol_version
            == fidan_driver::AI_ANALYSIS_HELPER_PROTOCOL_VERSION
    }) {
        return validate_toolchain("ai-analysis", toolchain.clone());
    }

    if !installed.is_empty() {
        let found_protocols = installed
            .iter()
            .map(|toolchain| toolchain.metadata.backend_protocol_version.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let expected_protocol = fidan_driver::AI_ANALYSIS_HELPER_PROTOCOL_VERSION;
        let prompt = format!(
            "Installed AI analysis toolchain is incompatible (helper protocol {found_protocols}; expected {expected_protocol}). Update `ai-analysis` now?"
        );
        if !prompt_yes_no(&prompt, true)? {
            bail!(
                "AI analysis toolchain is installed but incompatible (expected helper protocol {expected_protocol}, found {found_protocols}) — run `fidan toolchain add ai-analysis` to update"
            );
        }

        install_toolchain("ai-analysis", None)?;
        let refreshed = fidan_driver::install::installed_toolchains(&home, Some("ai-analysis"))?;
        let toolchain = refreshed
            .into_iter()
            .find(|toolchain| {
                toolchain.metadata.backend_protocol_version
                    == fidan_driver::AI_ANALYSIS_HELPER_PROTOCOL_VERSION
            })
            .with_context(|| {
                format!(
                    "ai-analysis install/update completed but no compatible toolchain was found afterward (expected helper protocol {expected_protocol})"
                )
            })?;
        let toolchain = validate_toolchain("ai-analysis", toolchain)?;
        run_installed_helper_command("ai-analysis", &toolchain, &["exec", "ai", "setup"])?;
        return Ok(toolchain);
    }

    ensure_toolchain_installed(
        "ai-analysis",
        "AI analysis",
        |home| {
            Ok(
                fidan_driver::install::installed_toolchains(home, Some("ai-analysis"))?
                    .into_iter()
                    .find(|toolchain| {
                        toolchain.metadata.backend_protocol_version
                            == fidan_driver::AI_ANALYSIS_HELPER_PROTOCOL_VERSION
                    }),
            )
        },
        Some(&["exec", "ai", "setup"]),
    )
}

pub(crate) fn ensure_llvm_toolchain_installed() -> Result<ResolvedToolchain> {
    ensure_toolchain_installed(
        "llvm",
        "LLVM backend",
        |home| {
            Ok(fidan_driver::install::installed_llvm_toolchains(home)?
                .into_iter()
                .next())
        },
        None,
    )
}

fn ensure_toolchain_installed<F>(
    kind: &str,
    display_name: &str,
    find_toolchain: F,
    post_install_args: Option<&[&str]>,
) -> Result<ResolvedToolchain>
where
    F: Fn(&std::path::Path) -> Result<Option<ResolvedToolchain>>,
{
    let home = resolve_fidan_home()?;
    if let Some(toolchain) = find_toolchain(&home)? {
        return validate_toolchain(kind, toolchain);
    }

    let prompt = format!(
        "{display_name} toolchain is not installed for this Fidan version. Install `{kind}` now?"
    );
    if !prompt_yes_no(&prompt, true)? {
        bail!(
            "{display_name} toolchain is not installed for this Fidan version — run `fidan toolchain add {kind}` first"
        );
    }

    install_toolchain(kind, None)?;
    let toolchain = find_toolchain(&home)?.with_context(|| {
        format!("{kind} install completed but no compatible toolchain was found afterward")
    })?;
    let toolchain = validate_toolchain(kind, toolchain)?;

    if let Some(args) = post_install_args {
        run_installed_helper_command(kind, &toolchain, args)?;
    }

    Ok(toolchain)
}

fn validate_toolchain(kind: &str, toolchain: ResolvedToolchain) -> Result<ResolvedToolchain> {
    if !toolchain.helper_path.is_file() {
        bail!(
            "installed {kind} helper is missing at `{}` — reinstall with `fidan toolchain add {kind} --version {}`",
            toolchain.helper_path.display(),
            toolchain.metadata.toolchain_version
        );
    }
    Ok(toolchain)
}

fn run_installed_helper_command(
    kind: &str,
    toolchain: &ResolvedToolchain,
    args: &[&str],
) -> Result<()> {
    let status = Command::new(&toolchain.helper_path)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to run `{}`", toolchain.helper_path.display()))?;
    if status.success() {
        return Ok(());
    }
    bail!("{kind} setup command failed with status {status}")
}

fn run_available() -> Result<()> {
    let manifest = fetch_manifest(None)?;
    let host = fidan_driver::install::host_triple();
    let mut any = false;
    for release in manifest
        .toolchains
        .iter()
        .filter(|release| release.host_triple == host)
    {
        any = true;
        println!(
            "- {} {} (tool {}, helper protocol {})",
            release.kind,
            release.toolchain_version,
            release.tool_version,
            release.backend_protocol_version
        );
    }
    if !any {
        render_message_to_stderr(
            Severity::Note,
            "toolchain",
            &format!("no toolchain packages are published for `{host}` yet"),
        );
    }
    Ok(())
}

fn run_list() -> Result<()> {
    let home = resolve_fidan_home()?;
    let toolchains = fidan_driver::install::installed_toolchains(&home, None)?;
    if toolchains.is_empty() {
        render_message_to_stderr(
            Severity::Note,
            "toolchain",
            "no toolchains are installed yet",
        );
        return Ok(());
    }

    for toolchain in toolchains {
        let metadata = toolchain.metadata;
        let compatibility_note = match metadata.kind.as_str() {
            "ai-analysis"
                if metadata.backend_protocol_version
                    != fidan_driver::AI_ANALYSIS_HELPER_PROTOCOL_VERSION =>
            {
                format!(
                    " [incompatible: cli expects helper protocol {}]",
                    fidan_driver::AI_ANALYSIS_HELPER_PROTOCOL_VERSION
                )
            }
            "llvm"
                if metadata.backend_protocol_version
                    != fidan_driver::LLVM_BACKEND_PROTOCOL_VERSION =>
            {
                format!(
                    " [incompatible: cli expects helper protocol {}]",
                    fidan_driver::LLVM_BACKEND_PROTOCOL_VERSION
                )
            }
            _ => String::new(),
        };
        println!(
            "- {} {} (tool {}, helper protocol {}){}",
            metadata.kind,
            metadata.toolchain_version,
            metadata.tool_version,
            metadata.backend_protocol_version,
            compatibility_note
        );
    }
    Ok(())
}

fn run_add(name: &str, version: Option<&str>) -> Result<()> {
    let manifest = fetch_manifest(None)?;
    let host = fidan_driver::install::host_triple();
    let release = select_toolchain_release(&manifest, name, version, &host)?;
    let home = resolve_fidan_home()?;
    let parent = home.join("toolchains").join(name).join(&host);
    fs::create_dir_all(&parent)
        .with_context(|| format!("failed to create `{}`", parent.display()))?;
    let final_dir = parent.join(&release.toolchain_version);
    let mut refreshed = false;
    if final_dir.exists() {
        let metadata_path = final_dir.join("metadata.json");
        let existing = fs::read(&metadata_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ToolchainMetadata>(&bytes).ok());
        if existing
            .as_ref()
            .and_then(|metadata| metadata.archive_sha256.as_deref())
            == Some(release.sha256.as_str())
        {
            bail!(
                "toolchain `{name}` version `{}` is already installed",
                release.toolchain_version
            );
        }

        fs::remove_dir_all(&final_dir).with_context(|| {
            format!(
                "failed to replace existing toolchain directory `{}`",
                final_dir.display()
            )
        })?;
        refreshed = true;
    }

    let cache_path = home.join("cache").join("downloads").join(format!(
        "toolchain-{}-{}-{}.tar.gz",
        name, release.toolchain_version, host
    ));
    let bytes = fetch_cached_bytes(&release.url, &cache_path, &release.sha256)?;

    let staging = stage_dir(&parent, &format!("{}-{}", name, release.toolchain_version));
    let progress = ProgressReporter::spinner(
        "extract",
        format!(
            "unpacking {} toolchain {}",
            release.kind, release.toolchain_version
        ),
    );
    let extract_result = extract_tar_gz(&bytes, &staging);
    progress.finish_and_clear();
    extract_result?;
    materialize_release_root(
        &staging,
        &PathBuf::from(&release.helper_relpath),
        &final_dir,
    )?;

    let metadata = ToolchainMetadata {
        schema_version: 1,
        kind: release.kind.clone(),
        toolchain_version: release.toolchain_version.clone(),
        tool_version: release.tool_version.clone(),
        host_triple: release.host_triple.clone(),
        supported_fidan_versions: release.supported_fidan_versions.clone(),
        backend_protocol_version: release.backend_protocol_version,
        helper_relpath: release.helper_relpath.clone(),
        exec_commands: release.exec_commands.clone(),
        archive_sha256: Some(release.sha256.clone()),
    };
    let metadata_bytes =
        serde_json::to_vec_pretty(&metadata).context("failed to serialize toolchain metadata")?;
    write_bytes(&final_dir.join("metadata.json"), &metadata_bytes)?;

    render_message_to_stderr(
        Severity::Note,
        "toolchain",
        &format!(
            "{} {} toolchain {} for {}",
            if refreshed { "refreshed" } else { "installed" },
            release.kind,
            release.toolchain_version,
            host
        ),
    );
    Ok(())
}

fn run_remove(name: &str, version: Option<&str>, confirm: bool) -> Result<()> {
    let home = resolve_fidan_home()?;
    let host = fidan_driver::install::host_triple();
    let toolchains_root = home.join("toolchains");
    let kind_root = toolchains_root.join(name);
    let parent = home.join("toolchains").join(name).join(&host);
    if !parent.exists() {
        bail!("no `{name}` toolchains are installed for `{host}`");
    }

    let target = if let Some(version) = version {
        parent.join(version)
    } else {
        let mut dirs = fs::read_dir(&parent)
            .with_context(|| format!("failed to read `{}`", parent.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .collect::<Vec<_>>();
        dirs.sort_by_key(|entry| entry.file_name());
        let Some(entry) = dirs.pop() else {
            bail!("no `{name}` toolchains are installed for `{host}`");
        };
        entry.path()
    };

    if !target.exists() {
        bail!(
            "toolchain `{name}` version `{}` is not installed",
            version.unwrap_or("<latest>")
        );
    }

    let target_label = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<unknown>");
    if !confirm
        && !prompt_yes_no(
            &format!("remove toolchain `{name}` version `{target_label}`?"),
            false,
        )?
    {
        render_message_to_stderr(Severity::Note, "toolchain", "cancelled");
        return Ok(());
    }
    fs::remove_dir_all(&target)
        .with_context(|| format!("failed to remove `{}`", target.display()))?;
    remove_dir_if_empty(&parent)?;
    remove_dir_if_empty(&kind_root)?;
    remove_dir_if_empty(&toolchains_root)?;
    render_message_to_stderr(
        Severity::Note,
        "toolchain",
        &format!("removed `{}`", target.display()),
    );
    Ok(())
}

fn remove_dir_if_empty(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let mut entries =
        fs::read_dir(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    if entries.next().is_none() {
        fs::remove_dir(path).with_context(|| format!("failed to remove `{}`", path.display()))?;
    }
    Ok(())
}
