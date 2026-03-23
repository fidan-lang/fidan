mod api;
mod archive;
mod config;

use crate::prompts::prompt_yes_no;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::dal::{
    DalCliMeta, DalLock, DalLockedPackage, DalManifest, cli_binary_stem, global_bin_dir,
    global_dal_dir, global_lock_path, global_package_store, global_roots_path, local_bin_dir,
    local_package_store, lock_path, module_dir_name, package_install_dir, parse_dependency_req,
    project_root_or_fallback, prune_lock_to_dependencies, read_dependency_roots_if_exists,
    read_lock_from_path, read_lock_if_exists, read_manifest, read_manifest_if_exists,
    validate_package_name, write_dependency_roots, write_lock, write_lock_to_path, write_manifest,
};
use fidan_driver::install::ensure_persistent_path_entry;
use fidan_driver::{
    CompileOptions, ExecutionMode, Session, compile, compile_file_to_mir, resolve_fidan_home,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use self::api::DalClient;
use self::archive::{build_package_archive, install_downloaded_package};

#[derive(Subcommand)]
pub(crate) enum DalCommand {
    /// Store a Dal API token in the OS keychain for CLI use
    Login {
        /// Dal API token from the dashboard
        #[arg(long)]
        token: Option<String>,
        /// Override the Dal registry base URL
        #[arg(long)]
        registry: Option<String>,
    },
    /// Remove the stored Dal API token from the OS keychain
    Logout,
    /// Show the currently authenticated Dal account
    Whoami {
        /// Override the Dal registry base URL
        #[arg(long)]
        registry: Option<String>,
    },
    /// Search Dal packages
    Search {
        query: String,
        #[arg(long, default_value = "1")]
        page: u32,
        #[arg(long, default_value = "10")]
        per_page: u32,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Show package metadata and versions
    Info {
        package: String,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Download and vendor a package into an importable local module layout
    Add {
        package: String,
        /// SemVer requirement or exact version
        #[arg(long)]
        version: Option<String>,
        /// Parent directory to install into (default: current directory)
        #[arg(long)]
        into: Option<PathBuf>,
        /// Install into the global FIDAN_HOME package store instead of the project-local .dal store
        #[arg(long, conflicts_with = "into")]
        global: bool,
        /// Overwrite an existing installed package directory
        #[arg(long)]
        force: bool,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Remove an installed package from the managed local/global DAL stores
    Remove {
        package: String,
        /// Parent directory used with `dal add --into`
        #[arg(long)]
        into: Option<PathBuf>,
        /// Remove from the global FIDAN_HOME package store
        #[arg(long, conflicts_with = "into")]
        global: bool,
        /// Skip the interactive confirmation prompt
        #[arg(long)]
        confirm: bool,
    },
    /// Build a canonical Dal .tar.gz archive locally without publishing
    Package {
        /// Package project directory containing dal.toml
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output archive path (default: {name}-{version}.tar.gz in current dir)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Build and publish the current package to Dal
    Publish {
        /// Package project directory containing dal.toml
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Yank a published package version
    Yank {
        package: String,
        version: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        registry: Option<String>,
    },
    /// Unyank a published package version
    Unyank {
        package: String,
        version: String,
        #[arg(long)]
        registry: Option<String>,
    },
}

pub(crate) fn run(command: DalCommand) -> Result<()> {
    match command {
        DalCommand::Login { token, registry } => run_login(token, registry),
        DalCommand::Logout => run_logout(),
        DalCommand::Whoami { registry } => run_whoami(registry),
        DalCommand::Search {
            query,
            page,
            per_page,
            registry,
        } => run_search(&query, page, per_page, registry),
        DalCommand::Info { package, registry } => run_info(&package, registry),
        DalCommand::Add {
            package,
            version,
            into,
            global,
            force,
            registry,
        } => run_add(&package, version.as_deref(), into, global, force, registry),
        DalCommand::Remove {
            package,
            into,
            global,
            confirm,
        } => run_remove(&package, into, global, confirm),
        DalCommand::Package { path, output } => run_package(&path, output),
        DalCommand::Publish { path, registry } => run_publish(&path, registry),
        DalCommand::Yank {
            package,
            version,
            reason,
            registry,
        } => run_yank(&package, &version, reason.as_deref(), registry),
        DalCommand::Unyank {
            package,
            version,
            registry,
        } => run_unyank(&package, &version, registry),
    }
}

fn run_login(token: Option<String>, registry: Option<String>) -> Result<()> {
    let registry = config::resolve_registry(registry.as_deref())?;
    let token = match token {
        Some(token) => token,
        None => prompt_token()?,
    };

    let client = DalClient::new(registry.clone(), Some(token.clone()))?;
    let user = client.whoami().context(
        "Dal token verification failed — make sure you pasted a valid API token from the dashboard",
    )?;

    config::store_token(&token)?;
    config::verify_stored_token(&token)?;

    println!(
        "Logged in to {} as {}{}",
        registry,
        user.username,
        user.display_name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default()
    );

    Ok(())
}

fn run_logout() -> Result<()> {
    config::clear_token()?;
    println!("Dal API token removed from the OS keychain.");
    Ok(())
}

fn run_whoami(registry: Option<String>) -> Result<()> {
    let client = authenticated_client(registry)?;
    let user = client.whoami()?;

    let mut rows = vec![("username", user.username), ("email", user.email)];
    if let Some(display_name) = user.display_name {
        rows.push(("display name", display_name));
    }

    let label_width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let lines = rows
        .into_iter()
        .map(|(label, value)| format!("{label:<width$}  {value}", width = label_width))
        .collect::<Vec<_>>();

    render_message_to_stderr(Severity::Note, "dal", &lines.join("\n"));
    Ok(())
}
fn run_search(query: &str, page: u32, per_page: u32, registry: Option<String>) -> Result<()> {
    let registry = config::resolve_registry(registry.as_deref())?;
    let client = DalClient::new(registry, None)?;
    let results = client.search(query, page, per_page)?;

    if results.items.is_empty() {
        println!("No packages found for `{query}`.");
        return Ok(());
    }

    println!(
        "Page {}/{} — {} result(s)",
        results.page, results.pages, results.total
    );
    for item in results.items {
        println!(
            "- {}{}",
            item.name,
            item.latest_version
                .as_deref()
                .map(|version| format!(" @ {version}"))
                .unwrap_or_default()
        );
        if let Some(description) = item.description {
            println!("  {}", description.trim());
        }
        println!("  downloads: {}", item.downloads);
    }

    Ok(())
}

fn run_info(package: &str, registry: Option<String>) -> Result<()> {
    let registry = config::resolve_registry(registry.as_deref())?;
    let client = DalClient::new(registry, None)?;
    let info = client.package_info(package)?;
    let versions = client.versions(package)?;

    println!("name: {}", info.name);
    if let Some(description) = info.description {
        println!("description: {}", description);
    }
    if let Some(license) = info.license {
        println!("license: {}", license);
    }
    if let Some(repository) = info.repository {
        println!("repository: {}", repository);
    }
    if let Some(homepage) = info.homepage {
        println!("homepage: {}", homepage);
    }
    println!("downloads: {}", info.downloads);
    println!();
    println!("versions:");
    for version in versions {
        println!(
            "- {}{}{}",
            version.version,
            if version.yanked { " [yanked]" } else { "" },
            version
                .yank_reason
                .as_deref()
                .map(|reason| format!(" — {}", reason))
                .unwrap_or_default()
        );
    }

    Ok(())
}

fn run_add(
    package: &str,
    version_req: Option<&str>,
    into: Option<PathBuf>,
    global: bool,
    force: bool,
    registry: Option<String>,
) -> Result<()> {
    let registry = config::resolve_registry(registry.as_deref())?;
    let client = DalClient::new(registry, None)?;
    if global && into.is_some() {
        bail!("`--global` cannot be used together with `--into`");
    }

    let cwd = std::env::current_dir().context("cannot determine cwd")?;
    let project_root = project_root_or_fallback(&cwd);
    let global_home = if global {
        Some(resolve_fidan_home()?)
    } else {
        None
    };
    let existing_manifest = if into.is_none() && !global {
        read_manifest_if_exists(&project_root)?
    } else {
        None
    };
    let existing_global_roots = if into.is_none() && global {
        read_dependency_roots_if_exists(&global_roots_path(
            global_home.as_ref().expect("global home is set"),
        ))?
    } else {
        None
    };

    let mut requested_requirements = BTreeMap::new();
    if let Some(manifest) = &existing_manifest {
        for (name, req) in &manifest.dependencies {
            requested_requirements.insert(name.clone(), req.clone());
        }
    }
    if let Some(roots) = &existing_global_roots {
        for (name, req) in &roots.dependencies {
            requested_requirements.insert(name.clone(), req.clone());
        }
    }

    let top_requirement = version_req
        .map(ToOwned::to_owned)
        .or_else(|| requested_requirements.get(package).cloned())
        .unwrap_or_else(|| "*".to_string());
    requested_requirements.insert(package.to_string(), top_requirement);

    let resolver = PackageResolver::new(&client);
    let resolved = resolver.resolve_all(package, &requested_requirements)?;

    let install_root = if let Some(into_dir) = &into {
        into_dir.clone()
    } else if global {
        global_package_store(global_home.as_ref().expect("global home is set"))
    } else {
        local_package_store(&project_root)
    };
    fs::create_dir_all(&install_root)
        .with_context(|| format!("cannot create {}", install_root.display()))?;

    for package_state in resolved.packages.values() {
        let archive = client.download_archive(&package_state.name, &package_state.version)?;
        install_downloaded_package(
            &archive,
            &package_state.name,
            &package_state.version,
            &install_root,
            force,
        )?;
    }

    let manifest_requirement = version_req
        .map(ToOwned::to_owned)
        .or_else(|| {
            existing_manifest
                .as_ref()
                .and_then(|manifest| manifest.dependencies.get(package).cloned())
        })
        .unwrap_or_else(|| format!("={}", resolved.top_version));

    if into.is_none() && !global {
        let mut manifest = existing_manifest.unwrap_or(DalManifest {
            package: fidan_driver::dal::DalPackageMeta {
                name: project_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("local-project")
                    .to_string(),
                version: "0.1.0".to_string(),
                readme: Some("README.md".to_string()),
            },
            dependencies: BTreeMap::new(),
            cli: None,
        });
        manifest
            .dependencies
            .insert(package.to_string(), manifest_requirement);
        write_manifest(&project_root, &manifest)?;
        write_lock(&project_root, &resolved.to_lock())?;
    } else if into.is_none() && global {
        let mut roots = existing_global_roots.unwrap_or_default();
        roots
            .dependencies
            .insert(package.to_string(), manifest_requirement);
        write_dependency_roots(
            &global_roots_path(global_home.as_ref().expect("global home is set")),
            &roots,
        )?;
        write_lock_to_path(
            &global_lock_path(global_home.as_ref().expect("global home is set")),
            &resolved.to_lock(),
        )?;
    }

    let requested_package = resolved
        .packages
        .get(package)
        .with_context(|| format!("resolved package graph did not contain `{package}`"))?;
    let installed_to = package_install_dir(&install_root, package, &requested_package.version);
    let import_name = module_dir_name(package);

    let mut extra_note = None;
    if into.is_none()
        && let Some(cli_meta) =
            read_manifest_if_exists(&installed_to)?.and_then(|manifest| manifest.cli)
    {
        let binary_root = if global {
            global_bin_dir(global_home.as_ref().expect("global home is set"))
        } else {
            local_bin_dir(&project_root)
        };
        let binary_path = build_package_cli(&installed_to, &cli_meta, &binary_root)?;
        if global {
            ensure_persistent_path_entry(&binary_root).with_context(|| {
                format!(
                    "failed to add `{}` to the persistent PATH",
                    binary_root.display()
                )
            })?;
        }
        extra_note = Some(format!("Built CLI binary at {}", binary_path.display()));
    }

    println!(
        "Installed {}@{} into {}",
        package,
        requested_package.version,
        installed_to.display()
    );
    println!("Import it in Fidan with: use {import_name}");
    if let Some(note) = extra_note {
        println!("{note}");
    }

    Ok(())
}

fn run_remove(package: &str, into: Option<PathBuf>, global: bool, confirm: bool) -> Result<()> {
    validate_package_name(package)?;
    if global && into.is_some() {
        bail!("`--global` cannot be used together with `--into`");
    }

    let scope = if let Some(into_dir) = &into {
        format!("custom install root `{}`", into_dir.display())
    } else if global {
        "global DAL store".to_string()
    } else {
        "local project DAL store".to_string()
    };

    if !confirm && !prompt_yes_no(&format!("remove `{package}` from the {scope}?"), false)? {
        println!("Cancelled.");
        return Ok(());
    }

    if let Some(into_dir) = into {
        let target_dir = into_dir.join(module_dir_name(package));
        if !target_dir.exists() {
            bail!(
                "package `{package}` is not installed under `{}`",
                into_dir.display()
            );
        }
        fs::remove_dir_all(&target_dir)
            .with_context(|| format!("cannot remove {}", target_dir.display()))?;
        println!("Removed {} from {}", package, target_dir.display());
        return Ok(());
    }

    if global {
        let home = resolve_fidan_home()?;
        remove_global_package(package, &home)
    } else {
        let cwd = std::env::current_dir().context("cannot determine cwd")?;
        let project_root = project_root_or_fallback(&cwd);
        remove_local_package(package, &project_root)
    }
}

#[derive(Debug, Clone)]
struct RemovedInstalledPackage {
    install_dir: PathBuf,
}

fn remove_local_package(package: &str, project_root: &Path) -> Result<()> {
    let mut manifest = read_manifest(project_root)
        .with_context(|| format!("cannot remove `{package}` without a local dal.toml"))?;
    let Some(_) = manifest.dependencies.remove(package) else {
        bail!("package `{package}` is not a direct dependency in this project");
    };

    let existing_lock = read_lock_if_exists(project_root)?.unwrap_or_default();
    let next_lock = prune_lock_to_dependencies(&existing_lock, &manifest.dependencies)?;
    let install_root = local_package_store(project_root);
    let removed_packages = removed_installed_packages(&existing_lock, &next_lock, &install_root);
    let removed_cli = remove_package_cli_binary(
        package,
        &existing_lock,
        &install_root,
        &local_bin_dir(project_root),
    )?;

    write_manifest(project_root, &manifest)?;
    if next_lock.packages.is_empty() {
        remove_file_if_exists(&lock_path(project_root))?;
    } else {
        write_lock(project_root, &next_lock)?;
    }

    remove_installed_packages(&removed_packages)?;
    cleanup_empty_local_layout(project_root)?;

    println!(
        "Removed direct dependency `{package}` from {}",
        project_root.display()
    );
    if !removed_packages.is_empty() {
        println!("Pruned {} installed package(s).", removed_packages.len());
    }
    if removed_cli {
        println!("Removed local CLI binary for `{package}`.");
    }
    Ok(())
}

fn remove_global_package(package: &str, home: &Path) -> Result<()> {
    let roots_path = global_roots_path(home);
    let lock_path = global_lock_path(home);
    let install_root = global_package_store(home);
    let bin_root = global_bin_dir(home);

    let mut roots = read_dependency_roots_if_exists(&roots_path)?.unwrap_or_default();
    let existing_lock = read_lock_from_path(&lock_path).unwrap_or_default();

    let removed_direct_root = roots.dependencies.remove(package).is_some();
    if !removed_direct_root && !install_root.join(module_dir_name(package)).exists() {
        bail!("package `{package}` is not installed globally");
    }

    let next_lock = if removed_direct_root {
        prune_lock_to_dependencies(&existing_lock, &roots.dependencies)?
    } else {
        existing_lock.clone()
    };
    let removed_packages = if removed_direct_root {
        removed_installed_packages(&existing_lock, &next_lock, &install_root)
    } else {
        vec![RemovedInstalledPackage {
            install_dir: install_root.join(module_dir_name(package)),
        }]
    };
    let removed_cli = remove_package_cli_binary(package, &existing_lock, &install_root, &bin_root)?;

    if removed_direct_root {
        if roots.dependencies.is_empty() {
            remove_file_if_exists(&roots_path)?;
            remove_file_if_exists(&lock_path)?;
        } else {
            write_dependency_roots(&roots_path, &roots)?;
            write_lock_to_path(&global_lock_path(home), &next_lock)?;
        }
    }

    remove_installed_packages(&removed_packages)?;
    cleanup_empty_dir_if_possible(&install_root)?;
    cleanup_empty_global_bin(home, &bin_root)?;

    println!(
        "Removed global package `{package}` from {}",
        install_root.display()
    );
    if !removed_packages.is_empty() {
        println!("Pruned {} installed package(s).", removed_packages.len());
    }
    if removed_cli {
        println!("Removed global CLI binary for `{package}`.");
    }
    Ok(())
}

fn removed_installed_packages(
    old_lock: &DalLock,
    new_lock: &DalLock,
    store_root: &Path,
) -> Vec<RemovedInstalledPackage> {
    let retained = new_lock
        .packages
        .iter()
        .map(|pkg| pkg.name.clone())
        .collect::<BTreeSet<_>>();

    old_lock
        .packages
        .iter()
        .filter(|pkg| !retained.contains(&pkg.name))
        .map(|pkg| RemovedInstalledPackage {
            install_dir: package_install_dir(store_root, &pkg.name, &pkg.version),
        })
        .collect()
}

fn remove_installed_packages(packages: &[RemovedInstalledPackage]) -> Result<()> {
    for package in packages {
        if package.install_dir.exists() {
            fs::remove_dir_all(&package.install_dir)
                .with_context(|| format!("cannot remove {}", package.install_dir.display()))?;
            if let Some(parent) = package.install_dir.parent() {
                cleanup_empty_dir_if_possible(parent)?;
            }
        }
    }
    Ok(())
}

fn remove_package_cli_binary(
    package: &str,
    lock: &DalLock,
    store_root: &Path,
    bin_root: &Path,
) -> Result<bool> {
    let Some(locked) = lock.packages.iter().find(|entry| entry.name == package) else {
        return Ok(false);
    };
    let package_root = package_install_dir(store_root, &locked.name, &locked.version);
    let Some(manifest) = read_manifest_if_exists(&package_root)? else {
        return Ok(false);
    };
    let Some(cli) = manifest.cli else {
        return Ok(false);
    };

    let mut removed = false;
    for candidate in cli_binary_candidates(bin_root, &cli)? {
        if candidate.exists() {
            fs::remove_file(&candidate)
                .with_context(|| format!("cannot remove {}", candidate.display()))?;
            removed = true;
        }
    }

    if removed {
        cleanup_empty_dir_if_possible(bin_root)?;
    }
    Ok(removed)
}

fn cli_binary_candidates(bin_root: &Path, cli: &DalCliMeta) -> Result<Vec<PathBuf>> {
    let stem = cli_binary_stem(cli)?;
    let mut candidates = vec![bin_root.join(&stem)];
    #[cfg(target_os = "windows")]
    {
        candidates.push(bin_root.join(format!("{stem}.exe")));
    }
    Ok(candidates)
}

fn cleanup_empty_local_layout(project_root: &Path) -> Result<()> {
    let dal_dir = fidan_driver::dal::local_dal_dir(project_root);
    cleanup_empty_dir_if_possible(&local_package_store(project_root))?;
    cleanup_empty_dir_if_possible(&local_bin_dir(project_root))?;
    cleanup_empty_dir_if_possible(&dal_dir)
}

fn cleanup_empty_global_bin(home: &Path, bin_root: &Path) -> Result<()> {
    if !bin_root.is_dir() {
        remove_global_bin_path_if_needed(bin_root)?;
        return Ok(());
    }

    let has_entries = fs::read_dir(bin_root)
        .with_context(|| format!("cannot read {}", bin_root.display()))?
        .next()
        .transpose()?
        .is_some();
    if !has_entries {
        remove_global_bin_path_if_needed(bin_root)?;
        cleanup_empty_dir_if_possible(bin_root)?;
        cleanup_empty_dir_if_possible(&global_dal_dir(home))?;
    }
    Ok(())
}

#[cfg(not(test))]
fn remove_global_bin_path_if_needed(bin_root: &Path) -> Result<()> {
    fidan_driver::install::remove_persistent_path_entry(bin_root)?;
    Ok(())
}

#[cfg(test)]
fn remove_global_bin_path_if_needed(_bin_root: &Path) -> Result<()> {
    Ok(())
}

fn cleanup_empty_dir_if_possible(path: &Path) -> Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    let is_empty = fs::read_dir(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .next()
        .transpose()?
        .is_none();
    if is_empty {
        fs::remove_dir(path).with_context(|| format!("cannot remove {}", path.display()))?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
    Ok(true)
}

fn run_package(path: &std::path::Path, output: Option<PathBuf>) -> Result<()> {
    let built = build_package_archive(path)?;
    let out_path = match output {
        Some(path) => path,
        None => std::env::current_dir()
            .context("cannot determine cwd")?
            .join(&built.archive_name),
    };

    fs::write(&out_path, &built.archive_bytes)
        .with_context(|| format!("cannot write {}", out_path.display()))?;

    println!(
        "Packaged {}@{} -> {}",
        built.manifest.package.name,
        built.manifest.package.version,
        out_path.display()
    );

    Ok(())
}

#[derive(Debug, Clone)]
struct SparseDependency {
    name: String,
    req: String,
}

#[derive(Debug, Clone)]
struct ResolvedPackageState {
    name: String,
    version: String,
    dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct ResolvedGraph {
    top_version: String,
    packages: BTreeMap<String, ResolvedPackageState>,
}

impl ResolvedGraph {
    fn to_lock(&self) -> DalLock {
        DalLock {
            packages: self
                .packages
                .values()
                .map(|pkg| DalLockedPackage {
                    name: pkg.name.clone(),
                    module: module_dir_name(&pkg.name),
                    version: pkg.version.clone(),
                    dependencies: pkg.dependencies.clone(),
                })
                .collect(),
            ..Default::default()
        }
    }
}

struct PackageResolver<'a> {
    client: &'a DalClient,
    constraints: BTreeMap<String, Vec<semver::VersionReq>>,
    packages: BTreeMap<String, ResolvedPackageState>,
    stack: Vec<String>,
}

impl<'a> PackageResolver<'a> {
    fn new(client: &'a DalClient) -> Self {
        Self {
            client,
            constraints: BTreeMap::new(),
            packages: BTreeMap::new(),
            stack: vec![],
        }
    }

    fn resolve_all(
        &self,
        top_package: &str,
        root_requirements: &BTreeMap<String, String>,
    ) -> Result<ResolvedGraph> {
        let mut resolver = PackageResolver::new(self.client);
        for (package, req) in root_requirements {
            resolver.resolve_package(package, req)?;
        }

        let top_version = resolver
            .packages
            .get(top_package)
            .map(|pkg| pkg.version.clone())
            .unwrap_or_default();

        Ok(ResolvedGraph {
            top_version,
            packages: resolver.packages,
        })
    }

    fn resolve_package(&mut self, package: &str, req: &str) -> Result<()> {
        validate_package_name(package)?;
        let requirement = parse_dependency_req(req)?;
        self.constraints
            .entry(package.to_string())
            .or_default()
            .push(requirement);
        self.resolve_locked(package)
    }

    fn resolve_locked(&mut self, package: &str) -> Result<()> {
        if let Some(pos) = self.stack.iter().position(|entry| entry == package) {
            let mut cycle = self.stack[pos..].to_vec();
            cycle.push(package.to_string());
            bail!("package dependency cycle detected: {}", cycle.join(" -> "));
        }

        let constraints = self.constraints.get(package).cloned().unwrap_or_default();
        let index = self.client.index(package)?;
        if index.is_empty() {
            bail!("package `{package}` has no published versions");
        }
        let chosen = select_version_with_constraints(&index, &constraints)?;
        let deps = parse_sparse_dependencies(&chosen.deps)?;
        let dependency_versions = deps
            .iter()
            .map(|dep| (dep.name.clone(), dep.req.clone()))
            .collect::<BTreeMap<_, _>>();

        let needs_update = self
            .packages
            .get(package)
            .map(|pkg| pkg.version != chosen.vers || pkg.dependencies != dependency_versions)
            .unwrap_or(true);
        if !needs_update {
            return Ok(());
        }

        self.stack.push(package.to_string());
        self.packages.insert(
            package.to_string(),
            ResolvedPackageState {
                name: package.to_string(),
                version: chosen.vers.clone(),
                dependencies: dependency_versions,
            },
        );

        for dep in deps {
            self.resolve_package(&dep.name, &dep.req)?;
        }
        self.stack.pop();
        Ok(())
    }
}

fn select_version_with_constraints<'a>(
    entries: &'a [api::IndexEntry],
    constraints: &[semver::VersionReq],
) -> Result<&'a api::IndexEntry> {
    let mut candidates = entries
        .iter()
        .filter(|entry| !entry.yanked)
        .filter_map(|entry| {
            semver::Version::parse(&entry.vers)
                .ok()
                .map(|version| (entry, version))
        })
        .filter(|(_, version)| constraints.iter().all(|req| req.matches(version)))
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| right.1.cmp(&left.1));
    candidates
        .into_iter()
        .map(|(entry, _)| entry)
        .next()
        .ok_or_else(|| anyhow::anyhow!("no non-yanked version satisfies all requested constraints"))
}

fn parse_sparse_dependencies(values: &[Value]) -> Result<Vec<SparseDependency>> {
    let mut dependencies = Vec::with_capacity(values.len());
    for value in values {
        match value {
            Value::String(name) => dependencies.push(SparseDependency {
                name: name.clone(),
                req: "*".to_string(),
            }),
            Value::Object(map) => {
                let name = map
                    .get("package")
                    .or_else(|| map.get("name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("dependency entry is missing `package`/`name`")
                    })?;
                let req = map
                    .get("req")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        map.get("version")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .unwrap_or_else(|| "*".to_string());
                dependencies.push(SparseDependency {
                    name: name.to_string(),
                    req,
                });
            }
            _ => bail!("unsupported dependency entry in sparse index"),
        }
    }
    Ok(dependencies)
}

fn build_package_cli(package_root: &Path, cli: &DalCliMeta, bin_root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(bin_root)
        .with_context(|| format!("cannot create {}", bin_root.display()))?;
    let entry_path = package_root.join(fidan_driver::dal::remap_source_relative_path(Path::new(
        &cli.entry,
    )));
    if !entry_path.is_file() {
        bail!(
            "installed package CLI entry `{}` was not found at `{}`",
            cli.entry,
            entry_path.display()
        );
    }
    let output = bin_root.join(cli_binary_stem(cli)?);
    let frontend = compile_file_to_mir(&entry_path)?;
    let opts = CompileOptions {
        input: entry_path.clone(),
        output: Some(output.clone()),
        mode: ExecutionMode::Build,
        ..Default::default()
    };
    compile(&Session::new(), frontend.mir, frontend.interner, &opts)?;
    Ok(output)
}

fn run_publish(path: &std::path::Path, registry: Option<String>) -> Result<()> {
    let built = build_package_archive(path)?;
    let client = authenticated_client(registry)?;
    let response = client.publish(
        &built.manifest.package.name,
        &built.archive_name,
        built.archive_bytes,
    )?;

    println!(
        "{} {}@{}",
        response.message, response.package, response.version
    );

    Ok(())
}

fn run_yank(
    package: &str,
    version: &str,
    reason: Option<&str>,
    registry: Option<String>,
) -> Result<()> {
    let client = authenticated_client(registry)?;
    let response = client.yank(package, version, reason)?;
    println!("{}", response.message);
    Ok(())
}

fn run_unyank(package: &str, version: &str, registry: Option<String>) -> Result<()> {
    let client = authenticated_client(registry)?;
    let response = client.unyank(package, version)?;
    println!("{}", response.message);
    Ok(())
}

fn authenticated_client(registry: Option<String>) -> Result<DalClient> {
    let registry = config::resolve_registry(registry.as_deref())?;
    let token = config::resolve_token(None)?;
    DalClient::new(registry, token)
}

fn prompt_token() -> Result<String> {
    let token = read_masked_token("Paste your Dal API token: ")?;
    if token.is_empty() {
        bail!("Dal API token must not be empty");
    }
    Ok(token)
}

fn read_masked_token(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush().ok();

    enable_raw_mode().context("failed to enable terminal raw mode for secure token input")?;
    let _raw_mode = RawModeGuard;

    let mut token = String::new();
    loop {
        match read().context("failed to read API token from stdin")? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') | KeyCode::Char('C') => {
                            println!();
                            bail!("Dal login cancelled");
                        }
                        KeyCode::Char('w')
                        | KeyCode::Char('W')
                        | KeyCode::Char('\u{17}')
                        | KeyCode::Backspace => {
                            erase_masked_chars(delete_last_word(&mut token));
                        }
                        KeyCode::Char(ch) if ch.is_control() => {}
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Enter => {
                        println!();
                        return Ok(token.trim().to_string());
                    }
                    KeyCode::Backspace => {
                        if token.pop().is_some() {
                            erase_masked_chars(1);
                        }
                    }
                    KeyCode::Char('\u{17}') => {
                        erase_masked_chars(delete_last_word(&mut token));
                    }
                    KeyCode::Char(ch) if !ch.is_control() => {
                        token.push(ch);
                        print!("*");
                        io::stdout().flush().ok();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn delete_last_word(token: &mut String) -> usize {
    let trailing_ws = token
        .chars()
        .rev()
        .take_while(|ch| ch.is_whitespace())
        .count();
    for _ in 0..trailing_ws {
        token.pop();
    }

    let word_len = token
        .chars()
        .rev()
        .take_while(|ch| !ch.is_whitespace())
        .count();
    for _ in 0..word_len {
        token.pop();
    }

    trailing_ws + word_len
}

fn erase_masked_chars(count: usize) {
    for _ in 0..count {
        print!("\u{8} \u{8}");
    }
    if count > 0 {
        io::stdout().flush().ok();
    }
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_driver::dal::{DalDependencyRoots, DalPackageMeta};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn remove_local_package_updates_manifest_lock_and_cli() -> Result<()> {
        let project_root = make_temp_dir("fidan_dal_remove_local");
        let mut manifest = DalManifest {
            package: DalPackageMeta {
                name: "consumer".to_string(),
                version: "0.1.0".to_string(),
                readme: Some("README.md".to_string()),
            },
            dependencies: BTreeMap::from([("tool-package".to_string(), "=1.0.0".to_string())]),
            cli: None,
        };
        write_manifest(&project_root, &manifest)?;
        write_lock(
            &project_root,
            &DalLock {
                schema_version: 1,
                packages: vec![
                    DalLockedPackage {
                        name: "tool-package".to_string(),
                        module: "tool_package".to_string(),
                        version: "1.0.0".to_string(),
                        dependencies: BTreeMap::from([(
                            "leaf-package".to_string(),
                            "=1.0.0".to_string(),
                        )]),
                    },
                    DalLockedPackage {
                        name: "leaf-package".to_string(),
                        module: "leaf_package".to_string(),
                        version: "1.0.0".to_string(),
                        dependencies: BTreeMap::new(),
                    },
                ],
            },
        )?;
        write_installed_package(
            &package_install_dir(&local_package_store(&project_root), "tool-package", "1.0.0"),
            true,
        )?;
        write_installed_package(
            &package_install_dir(&local_package_store(&project_root), "leaf-package", "1.0.0"),
            false,
        )?;
        let binary = cli_binary_path(&local_bin_dir(&project_root), "tool-package");
        fs::create_dir_all(binary.parent().expect("bin parent"))?;
        fs::write(&binary, b"stub")?;

        remove_local_package("tool-package", &project_root)?;

        manifest = read_manifest(&project_root)?;
        assert!(!manifest.dependencies.contains_key("tool-package"));
        assert!(!lock_path(&project_root).exists());
        assert!(!binary.exists());
        assert!(!local_package_store(&project_root).exists());
        Ok(())
    }

    #[test]
    fn remove_global_package_updates_global_metadata_and_cli() -> Result<()> {
        let home = make_temp_dir("fidan_dal_remove_global");
        write_dependency_roots(
            &global_roots_path(&home),
            &DalDependencyRoots {
                dependencies: BTreeMap::from([("tool-package".to_string(), "=1.0.0".to_string())]),
            },
        )?;
        write_lock_to_path(
            &global_lock_path(&home),
            &DalLock {
                schema_version: 1,
                packages: vec![
                    DalLockedPackage {
                        name: "tool-package".to_string(),
                        module: "tool_package".to_string(),
                        version: "1.0.0".to_string(),
                        dependencies: BTreeMap::from([(
                            "leaf-package".to_string(),
                            "=1.0.0".to_string(),
                        )]),
                    },
                    DalLockedPackage {
                        name: "leaf-package".to_string(),
                        module: "leaf_package".to_string(),
                        version: "1.0.0".to_string(),
                        dependencies: BTreeMap::new(),
                    },
                ],
            },
        )?;
        write_installed_package(
            &package_install_dir(&global_package_store(&home), "tool-package", "1.0.0"),
            true,
        )?;
        write_installed_package(
            &package_install_dir(&global_package_store(&home), "leaf-package", "1.0.0"),
            false,
        )?;
        let binary = cli_binary_path(&global_bin_dir(&home), "tool-package");
        fs::create_dir_all(binary.parent().expect("bin parent"))?;
        fs::write(&binary, b"stub")?;

        remove_global_package("tool-package", &home)?;

        assert!(!global_roots_path(&home).exists());
        assert!(!global_lock_path(&home).exists());
        assert!(!binary.exists());
        assert!(!global_package_store(&home).exists());
        Ok(())
    }

    fn write_installed_package(root: &Path, with_cli: bool) -> Result<()> {
        fs::create_dir_all(root)?;
        let package_name = root
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("pkg")
            .replace('_', "-");
        let manifest = DalManifest {
            package: DalPackageMeta {
                name: package_name.clone(),
                version: "1.0.0".to_string(),
                readme: Some("README.md".to_string()),
            },
            dependencies: BTreeMap::new(),
            cli: with_cli.then(|| DalCliMeta {
                entry: "src/main.fdn".to_string(),
                name: Some(package_name),
            }),
        };
        write_manifest(root, &manifest)?;
        Ok(())
    }

    fn cli_binary_path(bin_root: &Path, stem: &str) -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            bin_root.join(format!("{stem}.exe"))
        }

        #[cfg(not(target_os = "windows"))]
        {
            bin_root.join(stem)
        }
    }

    fn make_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
        fs::create_dir_all(&dir).expect("failed to create temp test dir");
        dir
    }
}
