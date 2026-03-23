use crate::Backend;
use crate::llvm_helper::LLVM_BACKEND_PROTOCOL_VERSION;
use anyhow::{Context, Result, bail};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
#[path = "install_windows.rs"]
mod windows_support;

const INSTALL_SCHEMA_VERSION: u32 = 1;
const TOOLCHAIN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveVersionMetadata {
    pub schema_version: u32,
    pub active_version: String,
    pub updated_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallEntry {
    pub version: String,
    pub installed_at_secs: u64,
    #[serde(default)]
    pub archive_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallsMetadata {
    pub schema_version: u32,
    pub installs: Vec<InstallEntry>,
    pub updated_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainMetadata {
    pub schema_version: u32,
    pub kind: String,
    pub toolchain_version: String,
    pub tool_version: String,
    pub host_triple: String,
    pub supported_fidan_versions: String,
    pub backend_protocol_version: u32,
    pub helper_relpath: String,
    #[serde(default)]
    pub archive_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedToolchain {
    pub root: PathBuf,
    pub metadata: ToolchainMetadata,
    pub helper_path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum EffectiveBackend {
    Cranelift,
    Llvm(Box<ResolvedToolchain>),
}

pub fn resolve_install_root() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        local_data_dir().map(|dir| dir.join("Programs").join("Fidan"))
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|dir| dir.join("Applications").join("Fidan"))
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        data_home_dir().map(|dir| dir.join("fidan").join("installs"))
    }
}

pub fn resolve_fidan_home() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("FIDAN_HOME")
        && !value.trim().is_empty()
    {
        return Ok(PathBuf::from(value));
    }

    #[cfg(target_os = "windows")]
    {
        local_data_dir().map(|dir| dir.join("Fidan"))
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|dir| {
            dir.join("Library")
                .join("Application Support")
                .join("Fidan")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        data_home_dir().map(|dir| dir.join("fidan"))
    }
}

pub fn versions_dir(root: &Path) -> PathBuf {
    root.join("versions")
}

pub fn metadata_dir(root: &Path) -> PathBuf {
    root.join("metadata")
}

pub fn current_dir(root: &Path) -> PathBuf {
    root.join("current")
}

pub fn active_version_path(root: &Path) -> PathBuf {
    metadata_dir(root).join("active-version.json")
}

pub fn installs_path(root: &Path) -> PathBuf {
    metadata_dir(root).join("installs.json")
}

pub fn current_binary_for_version(root: &Path, version: &str) -> PathBuf {
    let exe = if cfg!(windows) { "fidan.exe" } else { "fidan" };
    versions_dir(root).join(version).join(exe)
}

pub fn remove_bootstrap_path_entries(root: &Path) -> Result<bool> {
    remove_persistent_path_entry(&current_dir(root))
}

pub fn ensure_persistent_path_entry(path: &Path) -> Result<bool> {
    #[cfg(target_os = "windows")]
    {
        windows_support::ensure_user_path_entry(path)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let path_line = bootstrap_path_line(path);
        let profiles = candidate_bootstrap_profiles()?;
        let already_present = profiles.iter().any(|profile| {
            profile.exists()
                && fs::read_to_string(profile)
                    .map(|text| {
                        split_lines_preserve_trailing_newline(&text)
                            .into_iter()
                            .any(|line| line.trim_end_matches('\r') == path_line)
                    })
                    .unwrap_or(false)
        });

        if already_present {
            return Ok(false);
        }

        let target_profile = primary_bootstrap_profile()?;
        let mut contents = if target_profile.exists() {
            fs::read_to_string(&target_profile)
                .with_context(|| format!("failed to read `{}`", target_profile.display()))?
        } else {
            String::new()
        };

        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(&path_line);
        contents.push('\n');

        if let Some(parent) = target_profile.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
        fs::write(&target_profile, contents)
            .with_context(|| format!("failed to update `{}`", target_profile.display()))?;
        Ok(true)
    }
}

pub fn remove_persistent_path_entry(path: &Path) -> Result<bool> {
    #[cfg(target_os = "windows")]
    {
        windows_support::remove_user_path_entry(path)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let path_line = bootstrap_path_line(path);
        let mut changed = false;

        for profile in candidate_bootstrap_profiles()? {
            if !profile.exists() {
                continue;
            }
            let original = fs::read_to_string(&profile)
                .with_context(|| format!("failed to read `{}`", profile.display()))?;
            let filtered_lines = split_lines_preserve_trailing_newline(&original)
                .into_iter()
                .filter(|line| line.trim_end_matches('\r') != path_line)
                .collect::<Vec<_>>();
            let rewritten = filtered_lines.join("\n");
            if rewritten != original {
                fs::write(&profile, rewritten)
                    .with_context(|| format!("failed to update `{}`", profile.display()))?;
                changed = true;
            }
        }

        Ok(changed)
    }
}

pub fn ensure_install_layout(root: &Path) -> Result<()> {
    fs::create_dir_all(versions_dir(root))
        .with_context(|| format!("failed to create `{}`", versions_dir(root).display()))?;
    fs::create_dir_all(metadata_dir(root))
        .with_context(|| format!("failed to create `{}`", metadata_dir(root).display()))?;
    Ok(())
}

pub fn scan_installed_versions(root: &Path) -> Result<Vec<String>> {
    let dir = versions_dir(root);
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut versions = vec![];
    for entry in
        fs::read_dir(&dir).with_context(|| format!("failed to read `{}`", dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let version = entry.file_name().to_string_lossy().to_string();
        if current_binary_for_version(root, &version).is_file() {
            versions.push(version);
        }
    }
    versions.sort_by(|left, right| version_sort_desc(left, right));
    Ok(versions)
}

pub fn load_or_repair_metadata(root: &Path) -> Result<(ActiveVersionMetadata, InstallsMetadata)> {
    ensure_install_layout(root)?;
    let scanned = scan_installed_versions(root)?;
    if scanned.is_empty() {
        bail!(
            "no installed Fidan versions found in `{}`",
            versions_dir(root).display()
        );
    }

    let active = read_json::<ActiveVersionMetadata>(&active_version_path(root)).ok();
    let installs = read_json::<InstallsMetadata>(&installs_path(root)).ok();

    let installs = installs
        .filter(|meta| meta.schema_version == INSTALL_SCHEMA_VERSION)
        .map(normalize_installs)
        .unwrap_or_else(|| InstallsMetadata {
            schema_version: INSTALL_SCHEMA_VERSION,
            installs: scanned
                .iter()
                .map(|version| InstallEntry {
                    version: version.clone(),
                    installed_at_secs: now_secs(),
                    archive_sha256: None,
                })
                .collect(),
            updated_at_secs: now_secs(),
        });

    let current_from_pointer = read_current_version_from_pointer(root)?;
    let active_version = if let Some(active) = active
        && active.schema_version == INSTALL_SCHEMA_VERSION
        && scanned.contains(&active.active_version)
    {
        active.active_version
    } else if let Some(pointer_version) = current_from_pointer
        && scanned.contains(&pointer_version)
    {
        pointer_version
    } else if scanned.len() == 1 {
        scanned[0].clone()
    } else {
        bail!(
            "install metadata is missing or inconsistent in `{}` — multiple versions are installed, so Fidan cannot infer the active one safely",
            root.display()
        );
    };

    let active = ActiveVersionMetadata {
        schema_version: INSTALL_SCHEMA_VERSION,
        active_version: active_version.clone(),
        updated_at_secs: now_secs(),
    };

    write_json_atomic(&active_version_path(root), &active)?;
    write_json_atomic(&installs_path(root), &installs)?;
    ensure_current_points_to(root, &active_version)?;
    Ok((active, installs))
}

pub fn register_install(root: &Path, version: &str, archive_sha256: Option<&str>) -> Result<bool> {
    ensure_install_layout(root)?;
    let scanned = scan_installed_versions(root)?;
    let first_install = scanned.len() <= 1;

    let mut installs =
        read_json::<InstallsMetadata>(&installs_path(root)).unwrap_or(InstallsMetadata {
            schema_version: INSTALL_SCHEMA_VERSION,
            installs: vec![],
            updated_at_secs: now_secs(),
        });
    installs.schema_version = INSTALL_SCHEMA_VERSION;
    if let Some(entry) = installs
        .installs
        .iter_mut()
        .find(|entry| entry.version == version)
    {
        entry.archive_sha256 = archive_sha256.map(ToOwned::to_owned);
    } else {
        installs.installs.push(InstallEntry {
            version: version.to_string(),
            installed_at_secs: now_secs(),
            archive_sha256: archive_sha256.map(ToOwned::to_owned),
        });
    }
    installs = normalize_installs(installs);
    write_json_atomic(&installs_path(root), &installs)?;

    if first_install {
        let active = ActiveVersionMetadata {
            schema_version: INSTALL_SCHEMA_VERSION,
            active_version: version.to_string(),
            updated_at_secs: now_secs(),
        };
        write_json_atomic(&active_version_path(root), &active)?;
        ensure_current_points_to(root, version)?;
    }

    Ok(first_install)
}

pub fn remove_install_record(root: &Path, version: &str) -> Result<Vec<String>> {
    let mut installs =
        read_json::<InstallsMetadata>(&installs_path(root)).unwrap_or(InstallsMetadata {
            schema_version: INSTALL_SCHEMA_VERSION,
            installs: vec![],
            updated_at_secs: now_secs(),
        });
    installs.installs.retain(|entry| entry.version != version);
    installs.updated_at_secs = now_secs();
    installs = normalize_installs(installs);
    if installs.installs.is_empty() {
        if installs_path(root).exists() {
            fs::remove_file(installs_path(root))
                .with_context(|| "failed to remove installs metadata")?;
        }
        if active_version_path(root).exists() {
            fs::remove_file(active_version_path(root))
                .with_context(|| "failed to remove active-version metadata")?;
        }
        return Ok(vec![]);
    }
    write_json_atomic(&installs_path(root), &installs)?;
    Ok(installs
        .installs
        .into_iter()
        .map(|entry| entry.version)
        .collect())
}

pub fn set_active_version(root: &Path, version: &str) -> Result<()> {
    ensure_installed_version(root, version)?;
    persist_active_version(root, version)?;
    ensure_current_points_to(root, version)
}

pub fn persist_active_version(root: &Path, version: &str) -> Result<()> {
    ensure_installed_version(root, version)?;
    let active = ActiveVersionMetadata {
        schema_version: INSTALL_SCHEMA_VERSION,
        active_version: version.to_string(),
        updated_at_secs: now_secs(),
    };
    write_json_atomic(&active_version_path(root), &active)
}

pub fn resolve_current_binary(root: &Path) -> Result<PathBuf> {
    let (active, _) = load_or_repair_metadata(root)?;
    let binary = current_binary_for_version(root, &active.active_version);
    if !binary.is_file() {
        bail!(
            "active Fidan binary is missing at `{}` — reinstall or run `fidan self install {}`",
            binary.display(),
            active.active_version
        );
    }
    Ok(binary)
}

pub fn read_current_version_from_pointer(root: &Path) -> Result<Option<String>> {
    let current = current_dir(root);
    if !current.exists() {
        return Ok(None);
    }
    let target = fs::canonicalize(&current)
        .with_context(|| format!("failed to resolve `{}`", current.display()))?;
    Ok(target
        .file_name()
        .map(|name| name.to_string_lossy().to_string()))
}

pub fn ensure_current_points_to(root: &Path, version: &str) -> Result<()> {
    let target = versions_dir(root).join(version);
    if !target.is_dir() {
        bail!(
            "cannot activate Fidan version `{version}` because `{}` does not exist",
            target.display()
        );
    }

    let current = current_dir(root);
    if read_current_version_from_pointer(root)?.as_deref() == Some(version) {
        return Ok(());
    }

    if current.exists() {
        remove_existing_path(&current)?;
    }

    create_directory_pointer(&target, &current)
}

#[cfg(target_os = "windows")]
pub fn schedule_current_pointer_update(root: &Path, version: &str) -> Result<()> {
    let target = versions_dir(root).join(version);
    if !target.is_dir() {
        bail!(
            "cannot activate Fidan version `{version}` because `{}` does not exist",
            target.display()
        );
    }

    let current = current_dir(root);
    windows_support::schedule_directory_pointer_update(&current, &target)
        .context("failed to schedule Windows current-version switch")
}

#[cfg(target_os = "windows")]
pub fn schedule_active_version_refresh(
    root: &Path,
    version: &str,
    replacement: &Path,
) -> Result<()> {
    let target = versions_dir(root).join(version);
    let current = current_dir(root);
    windows_support::schedule_directory_replace_and_pointer_update(&current, &target, replacement)
        .context("failed to schedule Windows active-version refresh")
}

pub fn schedule_last_uninstall_cleanup(root: &Path, purge_home: Option<&Path>) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut paths = vec![root];
        if let Some(home) = purge_home {
            paths.push(home);
        }
        windows_support::schedule_cleanup(&paths)
            .context("failed to schedule Windows cleanup process")
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

pub fn installed_toolchains(home: &Path, kind: Option<&str>) -> Result<Vec<ResolvedToolchain>> {
    let host = host_triple();
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("failed to parse current Fidan version for toolchain compatibility")?;
    let mut toolchains = vec![];

    let toolchains_root = home.join("toolchains");
    if !toolchains_root.exists() {
        return Ok(vec![]);
    }

    for kind_entry in fs::read_dir(&toolchains_root)
        .with_context(|| format!("failed to read `{}`", toolchains_root.display()))?
    {
        let kind_entry = kind_entry?;
        if !kind_entry.file_type()?.is_dir() {
            continue;
        }
        let kind_name = kind_entry.file_name().to_string_lossy().to_string();
        if let Some(expected) = kind
            && expected != kind_name
        {
            continue;
        }

        let host_dir = kind_entry.path().join(&host);
        if !host_dir.exists() {
            continue;
        }

        for entry in fs::read_dir(&host_dir)
            .with_context(|| format!("failed to read `{}`", host_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let root = entry.path();
            let metadata_path = root.join("metadata.json");
            let metadata = match read_json::<ToolchainMetadata>(&metadata_path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.schema_version != TOOLCHAIN_SCHEMA_VERSION
                || metadata.kind != kind_name
                || metadata.host_triple != host
            {
                continue;
            }
            let req = match VersionReq::parse(&metadata.supported_fidan_versions) {
                Ok(req) => req,
                Err(_) => continue,
            };
            if !req.matches(&current_version) {
                continue;
            }
            let helper_path = root.join(&metadata.helper_relpath);
            toolchains.push(ResolvedToolchain {
                root,
                metadata,
                helper_path,
            });
        }
    }

    toolchains.sort_by(|a, b| {
        version_sort_desc(&a.metadata.toolchain_version, &b.metadata.toolchain_version)
    });
    Ok(toolchains)
}

pub fn installed_llvm_toolchains(home: &Path) -> Result<Vec<ResolvedToolchain>> {
    Ok(installed_toolchains(home, Some("llvm"))?
        .into_iter()
        .filter(|toolchain| {
            toolchain.metadata.backend_protocol_version == LLVM_BACKEND_PROTOCOL_VERSION
        })
        .collect())
}

pub fn resolve_effective_backend(requested: Backend) -> Result<EffectiveBackend> {
    match requested {
        Backend::Cranelift => Ok(EffectiveBackend::Cranelift),
        Backend::Llvm => installed_llvm_toolchains(&resolve_fidan_home()?)?
            .into_iter()
            .next()
            .map(Box::new)
            .map(EffectiveBackend::Llvm)
            .ok_or_else(|| anyhow::anyhow!(
                "LLVM backend is not installed for this Fidan version — run `fidan toolchain add llvm` or use `--backend cranelift`"
            )),
        Backend::Auto => Ok(installed_llvm_toolchains(&resolve_fidan_home()?)?
            .into_iter()
            .next()
            .map(Box::new)
            .map(EffectiveBackend::Llvm)
            .unwrap_or(EffectiveBackend::Cranelift)),
    }
}

pub fn host_triple() -> String {
    let os = if cfg!(target_os = "windows") {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-gnu"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        std::env::consts::ARCH
    };
    format!("{arch}-{os}")
}

fn normalize_installs(mut installs: InstallsMetadata) -> InstallsMetadata {
    let mut deduped = BTreeSet::new();
    installs
        .installs
        .retain(|entry| deduped.insert(entry.version.clone()));
    installs
        .installs
        .sort_by(|a, b| version_sort_desc(&a.version, &b.version));
    installs.updated_at_secs = now_secs();
    installs
}

fn ensure_installed_version(root: &Path, version: &str) -> Result<()> {
    let installed = scan_installed_versions(root)?;
    if installed
        .iter()
        .any(|installed_version| installed_version == version)
    {
        Ok(())
    } else {
        bail!("Fidan version `{version}` is not installed");
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .context("metadata path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create `{}`", parent.display()))?;
    let temp_path = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_secs()));
    let bytes = serde_json::to_vec_pretty(value).context("failed to serialize metadata")?;
    fs::write(&temp_path, bytes)
        .with_context(|| format!("failed to write `{}`", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to replace `{}`", path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| format!("failed to finalize `{}`", path.display()))
}

#[cfg(not(target_os = "windows"))]
fn candidate_bootstrap_profiles() -> Result<Vec<PathBuf>> {
    let home = home_dir()?;
    Ok(vec![
        home.join(".profile"),
        home.join(".zprofile"),
        home.join(".bash_profile"),
    ])
}

#[cfg(not(target_os = "windows"))]
fn primary_bootstrap_profile() -> Result<PathBuf> {
    Ok(home_dir()?.join(".profile"))
}

#[cfg(not(target_os = "windows"))]
fn bootstrap_path_line(path: &Path) -> String {
    format!("export PATH=\"{}:$PATH\"", path.to_string_lossy())
}

#[cfg(not(target_os = "windows"))]
fn split_lines_preserve_trailing_newline(text: &str) -> Vec<String> {
    let mut lines = text
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.push(String::new());
    }
    lines
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse `{}`", path.display()))
}

fn version_sort_desc(left: &str, right: &str) -> std::cmp::Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => right.cmp(&left),
        _ => right.cmp(left),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn local_data_dir() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .context("LOCALAPPDATA is not set")
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn data_home_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg));
    }
    home_dir().map(|home| home.join(".local").join("share"))
}

#[cfg(any(not(target_os = "windows"), target_os = "macos"))]
fn home_dir() -> Result<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .context("home directory environment variable is not set")
}

fn remove_existing_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path).with_context(|| format!("failed to remove `{}`", path.display()))
    } else {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove `{}`", path.display()))
    }
}

#[cfg(target_os = "windows")]
fn create_directory_pointer(target: &Path, link: &Path) -> Result<()> {
    let output = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .with_context(|| "failed to create Windows directory junction")?;
    if !output.status.success() {
        bail!(
            "failed to create junction `{}` -> `{}`: {}",
            link.display(),
            target.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn create_directory_pointer(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).with_context(|| {
        format!(
            "failed to create symbolic link `{}` -> `{}`",
            link.display(),
            target.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fidan-driver-install-test-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn metadata_repairs_single_install() {
        let root = sandbox();
        let version_dir = versions_dir(&root).join("1.2.3");
        fs::create_dir_all(&version_dir).unwrap();
        fs::write(
            version_dir.join(if cfg!(windows) { "fidan.exe" } else { "fidan" }),
            b"",
        )
        .unwrap();

        let (active, installs) = load_or_repair_metadata(&root).unwrap();
        assert_eq!(active.active_version, "1.2.3");
        assert_eq!(installs.installs.len(), 1);
    }

    #[test]
    fn auto_backend_falls_back_to_cranelift_without_toolchain() {
        let root = sandbox();
        let previous = std::env::var_os("FIDAN_HOME");
        unsafe { std::env::set_var("FIDAN_HOME", &root) };
        let backend = resolve_effective_backend(Backend::Auto).unwrap();
        if let Some(previous) = previous {
            unsafe { std::env::set_var("FIDAN_HOME", previous) };
        } else {
            unsafe { std::env::remove_var("FIDAN_HOME") };
        }
        assert!(matches!(backend, EffectiveBackend::Cranelift));
    }
}
