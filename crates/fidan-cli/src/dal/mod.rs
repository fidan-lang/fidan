mod api;
mod archive;
mod config;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use self::api::DalClient;
pub(crate) use self::archive::validate_package_name;
use self::archive::{
    build_package_archive, install_downloaded_package, module_dir_name, select_version,
};

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
        /// Overwrite an existing installed package directory
        #[arg(long)]
        force: bool,
        #[arg(long)]
        registry: Option<String>,
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
            force,
            registry,
        } => run_add(&package, version.as_deref(), into, force, registry),
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

    println!("username: {}", user.username);
    println!("email: {}", user.email);
    if let Some(display_name) = user.display_name {
        println!("display_name: {display_name}");
    }
    println!("email_verified: {}", user.email_verified);
    println!("is_admin: {}", user.is_admin);

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
    force: bool,
    registry: Option<String>,
) -> Result<()> {
    let registry = config::resolve_registry(registry.as_deref())?;
    let client = DalClient::new(registry, None)?;
    let index = client.index(package)?;
    if index.is_empty() {
        bail!("package `{package}` has no published versions");
    }
    let chosen = select_version(&index, version_req)?;
    let archive = client.download_archive(package, &chosen.vers)?;
    let install_parent = into.unwrap_or(std::env::current_dir().context("cannot determine cwd")?);
    let installed_to =
        install_downloaded_package(&archive, package, &chosen.vers, &install_parent, force)?;
    let import_name = module_dir_name(package);

    println!(
        "Installed {}@{} into {}",
        package,
        chosen.vers,
        installed_to.display()
    );
    println!("Import it in Fidan with: use {import_name}");

    if !chosen.deps.is_empty() {
        println!("Note: this package declares dependencies in the sparse index.");
        println!("Transitive dependency installation is not automatic yet.");
    }

    Ok(())
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
    print!("Paste your Dal API token: ");
    io::stdout().flush().ok();

    let mut token = String::new();
    io::stdin()
        .read_line(&mut token)
        .context("failed to read API token from stdin")?;

    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("Dal API token must not be empty");
    }
    Ok(token)
}
