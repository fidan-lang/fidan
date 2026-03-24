use crate::distribution::{
    extract_tar_gz, fetch_cached_bytes, fetch_manifest, materialize_release_root,
    select_toolchain_release, stage_dir, write_bytes,
};
use crate::prompts::prompt_yes_no;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::{ToolchainMetadata, progress::ProgressReporter, resolve_fidan_home};
use std::fs;
use std::path::PathBuf;

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
        println!(
            "- {} {} (tool {}, helper protocol {})",
            metadata.kind,
            metadata.toolchain_version,
            metadata.tool_version,
            metadata.backend_protocol_version
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
