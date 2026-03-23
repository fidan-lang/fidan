use anyhow::{Context, Result, bail};
use semver::VersionReq;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const DAL_LOCK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DalManifest {
    pub package: DalPackageMeta,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub cli: Option<DalCliMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DalPackageMeta {
    pub name: String,
    pub version: String,
    pub readme: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DalCliMeta {
    pub entry: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DalLock {
    pub schema_version: u32,
    pub packages: Vec<DalLockedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DalLockedPackage {
    pub name: String,
    pub module: String,
    pub version: String,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DalDependencyRoots {
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

impl Default for DalLock {
    fn default() -> Self {
        Self {
            schema_version: DAL_LOCK_SCHEMA_VERSION,
            packages: vec![],
        }
    }
}

pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("invalid package name `{name}`");
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        bail!("invalid package name `{name}`");
    }
    for &byte in bytes {
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
            bail!("invalid package name `{name}`");
        }
    }
    if name.contains("--") {
        bail!("invalid package name `{name}`");
    }
    Ok(())
}

pub fn module_dir_name(package: &str) -> String {
    let mut normalized = package.replace('-', "_");
    if normalized
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
    {
        normalized.insert(0, '_');
    }
    normalized
}

pub fn validate_manifest(manifest: &DalManifest) -> Result<()> {
    validate_package_name(&manifest.package.name)?;
    semver::Version::parse(&manifest.package.version)
        .with_context(|| format!("invalid semver version `{}`", manifest.package.version))?;

    for (name, req) in &manifest.dependencies {
        validate_package_name(name)?;
        parse_dependency_req(req)
            .with_context(|| format!("invalid dependency requirement for `{name}`"))?;
    }

    if let Some(cli) = &manifest.cli {
        if cli.entry.trim().is_empty() {
            bail!("`[cli].entry` must not be empty");
        }
        let entry_path = Path::new(&cli.entry);
        if entry_path.is_absolute() {
            bail!("`[cli].entry` must be a relative path");
        }
        if !is_safe_relative_path(entry_path) {
            bail!("`[cli].entry` contains an unsafe path");
        }
        if entry_path.extension().and_then(|ext| ext.to_str()) != Some("fdn") {
            bail!("`[cli].entry` must point to a `.fdn` source file");
        }
        if let Some(name) = &cli.name {
            validate_cli_binary_name(name)?;
        }
    }

    Ok(())
}

pub fn validate_cli_binary_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("CLI binary name must not be empty");
    }
    if name.contains(['/', '\\']) {
        bail!("CLI binary name must not contain path separators");
    }
    if name == "." || name == ".." {
        bail!("CLI binary name must not be `.` or `..`");
    }
    Ok(())
}

pub fn parse_dependency_req(raw: &str) -> Result<VersionReq> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(VersionReq::STAR);
    }
    if let Ok(version) = semver::Version::parse(raw) {
        return VersionReq::parse(&format!("={version}"))
            .with_context(|| format!("invalid exact version requirement `{raw}`"));
    }
    VersionReq::parse(raw).with_context(|| format!("invalid version requirement `{raw}`"))
}

pub fn manifest_path(project_root: &Path) -> PathBuf {
    project_root.join("dal.toml")
}

pub fn lock_path(project_root: &Path) -> PathBuf {
    project_root.join("dal.lock")
}

pub fn global_roots_path(home: &Path) -> PathBuf {
    global_dal_dir(home).join("global.toml")
}

pub fn global_lock_path(home: &Path) -> PathBuf {
    global_dal_dir(home).join("global.lock")
}

pub fn read_manifest(project_root: &Path) -> Result<DalManifest> {
    let path = manifest_path(project_root);
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let manifest: DalManifest =
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn read_manifest_if_exists(project_root: &Path) -> Result<Option<DalManifest>> {
    let path = manifest_path(project_root);
    if !path.is_file() {
        return Ok(None);
    }
    read_manifest(project_root).map(Some)
}

pub fn write_manifest(project_root: &Path, manifest: &DalManifest) -> Result<()> {
    validate_manifest(manifest)?;
    let path = manifest_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(manifest).context("failed to encode dal.toml")?;
    fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("cannot write {}", path.display()))
}

pub fn read_lock(project_root: &Path) -> Result<DalLock> {
    let path = lock_path(project_root);
    read_lock_from_path(&path)
}

pub fn read_lock_from_path(path: &Path) -> Result<DalLock> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut lock: DalLock =
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    normalize_lock(&mut lock)?;
    Ok(lock)
}

pub fn read_lock_if_exists(project_root: &Path) -> Result<Option<DalLock>> {
    let path = lock_path(project_root);
    if !path.is_file() {
        return Ok(None);
    }
    read_lock(project_root).map(Some)
}

pub fn write_lock(project_root: &Path, lock: &DalLock) -> Result<()> {
    let path = lock_path(project_root);
    write_lock_to_path(&path, lock)
}

pub fn write_lock_to_path(path: &Path, lock: &DalLock) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut normalized = lock.clone();
    normalize_lock(&mut normalized)?;
    let text = toml::to_string_pretty(&normalized).context("failed to encode dal.lock")?;
    fs::write(path, format!("{text}\n")).with_context(|| format!("cannot write {}", path.display()))
}

pub fn read_dependency_roots(path: &Path) -> Result<DalDependencyRoots> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let roots: DalDependencyRoots =
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    validate_dependency_roots(&roots)?;
    Ok(roots)
}

pub fn read_dependency_roots_if_exists(path: &Path) -> Result<Option<DalDependencyRoots>> {
    if !path.is_file() {
        return Ok(None);
    }
    read_dependency_roots(path).map(Some)
}

pub fn write_dependency_roots(path: &Path, roots: &DalDependencyRoots) -> Result<()> {
    validate_dependency_roots(roots)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(roots).context("failed to encode dependency roots")?;
    fs::write(path, format!("{text}\n")).with_context(|| format!("cannot write {}", path.display()))
}

pub fn normalize_lock(lock: &mut DalLock) -> Result<()> {
    if lock.schema_version == 0 {
        lock.schema_version = DAL_LOCK_SCHEMA_VERSION;
    }
    if lock.schema_version != DAL_LOCK_SCHEMA_VERSION {
        bail!(
            "unsupported dal.lock schema version `{}` (expected {})",
            lock.schema_version,
            DAL_LOCK_SCHEMA_VERSION
        );
    }

    for package in &mut lock.packages {
        validate_package_name(&package.name)?;
        semver::Version::parse(&package.version)
            .with_context(|| format!("invalid locked version `{}`", package.version))?;
        package.module = module_dir_name(&package.name);
    }

    lock.packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    lock.packages
        .dedup_by(|left, right| left.name == right.name);
    Ok(())
}

pub fn prune_lock_to_dependencies(
    lock: &DalLock,
    roots: &BTreeMap<String, String>,
) -> Result<DalLock> {
    let mut normalized = lock.clone();
    normalize_lock(&mut normalized)?;

    if roots.is_empty() {
        return Ok(DalLock::default());
    }

    let by_name = normalized
        .packages
        .iter()
        .map(|pkg| (pkg.name.clone(), pkg))
        .collect::<BTreeMap<_, _>>();
    let mut reachable = BTreeMap::new();
    let mut stack = Vec::new();

    for root in roots.keys() {
        visit_locked_package(root, &by_name, &mut reachable, &mut stack)?;
    }

    Ok(DalLock {
        schema_version: DAL_LOCK_SCHEMA_VERSION,
        packages: reachable.into_values().collect(),
    })
}

fn validate_dependency_roots(roots: &DalDependencyRoots) -> Result<()> {
    for (name, req) in &roots.dependencies {
        validate_package_name(name)?;
        parse_dependency_req(req)
            .with_context(|| format!("invalid dependency requirement for `{name}`"))?;
    }
    Ok(())
}

fn visit_locked_package(
    package: &str,
    by_name: &BTreeMap<String, &DalLockedPackage>,
    reachable: &mut BTreeMap<String, DalLockedPackage>,
    stack: &mut Vec<String>,
) -> Result<()> {
    if let Some(pos) = stack.iter().position(|entry| entry == package) {
        let mut cycle = stack[pos..].to_vec();
        cycle.push(package.to_string());
        bail!("package dependency cycle detected: {}", cycle.join(" -> "));
    }
    if reachable.contains_key(package) {
        return Ok(());
    }

    let pkg = by_name
        .get(package)
        .ok_or_else(|| anyhow::anyhow!("package `{package}` was missing from dal.lock"))?;
    stack.push(package.to_string());
    for dep in pkg.dependencies.keys() {
        visit_locked_package(dep, by_name, reachable, stack)?;
    }
    stack.pop();
    reachable.insert(package.to_string(), (*pkg).clone());
    Ok(())
}

pub fn project_root_from(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent().map(Path::to_path_buf)
    } else {
        Some(start.to_path_buf())
    }?;

    loop {
        if manifest_path(&current).is_file() || lock_path(&current).is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

pub fn project_root_or_fallback(start: &Path) -> PathBuf {
    project_root_from(start).unwrap_or_else(|| {
        if start.is_file() {
            start
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        } else {
            start.to_path_buf()
        }
    })
}

pub fn local_dal_dir(project_root: &Path) -> PathBuf {
    project_root.join(".dal")
}

pub fn local_package_store(project_root: &Path) -> PathBuf {
    local_dal_dir(project_root).join("packages")
}

pub fn local_bin_dir(project_root: &Path) -> PathBuf {
    local_dal_dir(project_root).join("bin")
}

pub fn global_dal_dir(home: &Path) -> PathBuf {
    home.join("dal")
}

pub fn global_package_store(home: &Path) -> PathBuf {
    global_dal_dir(home).join("packages")
}

pub fn global_bin_dir(home: &Path) -> PathBuf {
    home.join("bin")
}

pub fn package_install_dir(store_root: &Path, package: &str, version: &str) -> PathBuf {
    store_root.join(module_dir_name(package)).join(version)
}

pub fn remap_source_relative_path(relative: &Path) -> PathBuf {
    let relative_string = relative.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = relative_string.strip_prefix("src/") {
        return PathBuf::from(stripped);
    }
    PathBuf::from(relative)
}

pub fn cli_binary_stem(cli: &DalCliMeta) -> Result<String> {
    if let Some(name) = &cli.name {
        validate_cli_binary_name(name)?;
        return Ok(name.clone());
    }

    let stem = Path::new(&cli.entry)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("`[cli].entry` must point to a named source file"))?;
    validate_cli_binary_name(stem)?;
    Ok(stem.to_string())
}

pub fn lock_entry_for_module<'a>(lock: &'a DalLock, module: &str) -> Option<&'a DalLockedPackage> {
    lock.packages.iter().find(|pkg| pkg.module == module)
}

pub fn lock_entry_for_package<'a>(
    lock: &'a DalLock,
    package: &str,
) -> Option<&'a DalLockedPackage> {
    lock.packages.iter().find(|pkg| pkg.name == package)
}

pub fn is_safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path.components().all(|component| match component {
            std::path::Component::Normal(_) => true,
            std::path::Component::CurDir => true,
            std::path::Component::ParentDir => false,
            std::path::Component::RootDir | std::path::Component::Prefix(_) => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_binary_stem_defaults_from_entry_filename() -> Result<()> {
        let cli = DalCliMeta {
            entry: "src/my-tool.fdn".to_string(),
            name: None,
        };
        assert_eq!(cli_binary_stem(&cli)?, "my-tool");
        Ok(())
    }

    #[test]
    fn normalize_lock_sets_modules_and_deduplicates_by_package_name() -> Result<()> {
        let mut lock = DalLock {
            schema_version: 0,
            packages: vec![
                DalLockedPackage {
                    name: "my-package".to_string(),
                    module: String::new(),
                    version: "1.2.3".to_string(),
                    dependencies: BTreeMap::new(),
                },
                DalLockedPackage {
                    name: "my-package".to_string(),
                    module: "stale".to_string(),
                    version: "1.2.3".to_string(),
                    dependencies: BTreeMap::new(),
                },
            ],
        };

        normalize_lock(&mut lock)?;

        assert_eq!(lock.schema_version, DAL_LOCK_SCHEMA_VERSION);
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].module, "my_package");
        Ok(())
    }

    #[test]
    fn validate_manifest_accepts_dependencies_and_cli() -> Result<()> {
        let manifest = DalManifest {
            package: DalPackageMeta {
                name: "demo-package".to_string(),
                version: "1.0.0".to_string(),
                readme: Some("README.md".to_string()),
            },
            dependencies: BTreeMap::from([("other-package".to_string(), "^2.1".to_string())]),
            cli: Some(DalCliMeta {
                entry: "src/main.fdn".to_string(),
                name: None,
            }),
        };

        validate_manifest(&manifest)
    }

    #[test]
    fn prune_lock_keeps_only_reachable_packages() -> Result<()> {
        let lock = DalLock {
            schema_version: 1,
            packages: vec![
                DalLockedPackage {
                    name: "tool".to_string(),
                    module: "tool".to_string(),
                    version: "1.0.0".to_string(),
                    dependencies: BTreeMap::from([("leaf".to_string(), "=1.0.0".to_string())]),
                },
                DalLockedPackage {
                    name: "leaf".to_string(),
                    module: "leaf".to_string(),
                    version: "1.0.0".to_string(),
                    dependencies: BTreeMap::new(),
                },
                DalLockedPackage {
                    name: "unused".to_string(),
                    module: "unused".to_string(),
                    version: "9.9.9".to_string(),
                    dependencies: BTreeMap::new(),
                },
            ],
        };

        let pruned = prune_lock_to_dependencies(
            &lock,
            &BTreeMap::from([("tool".to_string(), "=1.0.0".to_string())]),
        )?;

        assert_eq!(pruned.packages.len(), 2);
        assert!(pruned.packages.iter().any(|pkg| pkg.name == "tool"));
        assert!(pruned.packages.iter().any(|pkg| pkg.name == "leaf"));
        assert!(!pruned.packages.iter().any(|pkg| pkg.name == "unused"));
        Ok(())
    }
}
