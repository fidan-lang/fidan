use anyhow::{Context, Result, bail};
use semver::VersionReq;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const DAL_LOCK_SCHEMA_VERSION: u32 = 1;

fn default_dependency_version() -> String {
    "*".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DalManifest {
    pub package: DalPackageMeta,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DalDependencySpec>,
    #[serde(rename = "optional-dependencies", default)]
    pub optional_dependencies: BTreeMap<String, DalDependencySpec>,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub cli: Option<DalCliMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DalDependencySpec {
    Simple(String),
    Detailed(DalDependencyDetail),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DalDependencyDetail {
    #[serde(default = "default_dependency_version")]
    pub version: String,
    #[serde(default)]
    pub features: Vec<String>,
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
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DalDependencyRoots {
    #[serde(default)]
    pub dependencies: BTreeMap<String, DalDependencySpec>,
}

impl Default for DalLock {
    fn default() -> Self {
        Self {
            schema_version: DAL_LOCK_SCHEMA_VERSION,
            packages: vec![],
        }
    }
}

impl DalDependencySpec {
    pub fn simple(version: impl Into<String>) -> Self {
        Self::Simple(version.into())
    }

    pub fn detailed(version: impl Into<String>, features: Vec<String>) -> Self {
        let mut detail = DalDependencyDetail {
            version: version.into(),
            features,
        };
        detail.normalize();
        if detail.features.is_empty() {
            Self::Simple(detail.version)
        } else {
            Self::Detailed(detail)
        }
    }

    pub fn version_req(&self) -> &str {
        match self {
            DalDependencySpec::Simple(version) => version.as_str(),
            DalDependencySpec::Detailed(detail) => detail.version.as_str(),
        }
    }

    pub fn features(&self) -> &[String] {
        match self {
            DalDependencySpec::Simple(_) => &[],
            DalDependencySpec::Detailed(detail) => &detail.features,
        }
    }

    pub fn normalized(&self) -> Self {
        match self {
            DalDependencySpec::Simple(version) => {
                DalDependencySpec::Simple(version.trim().to_string())
            }
            DalDependencySpec::Detailed(detail) => {
                let mut detail = detail.clone();
                detail.normalize();
                if detail.features.is_empty() {
                    DalDependencySpec::Simple(detail.version)
                } else {
                    DalDependencySpec::Detailed(detail)
                }
            }
        }
    }
}

impl DalDependencyDetail {
    fn normalize(&mut self) {
        self.version = self.version.trim().to_string();
        let mut seen = BTreeSet::new();
        self.features.retain(|feature| seen.insert(feature.clone()));
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
        validate_dependency_spec(req)
            .with_context(|| format!("invalid dependency requirement for `{name}`"))?;
    }
    for (name, req) in &manifest.optional_dependencies {
        validate_package_name(name)?;
        validate_dependency_spec(req)
            .with_context(|| format!("invalid optional dependency requirement for `{name}`"))?;
    }
    for (feature, members) in &manifest.features {
        validate_feature_name(feature)
            .with_context(|| format!("invalid feature name `{feature}`"))?;
        for member in members {
            validate_feature_member(manifest, member).with_context(|| {
                format!("invalid feature member `{member}` in feature `{feature}`")
            })?;
        }
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

fn validate_dependency_spec(spec: &DalDependencySpec) -> Result<()> {
    parse_dependency_req(spec.version_req())
        .with_context(|| format!("invalid version requirement `{}`", spec.version_req()))?;
    for feature in spec.features() {
        validate_feature_name(feature)
            .with_context(|| format!("invalid dependency feature `{feature}`"))?;
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

pub fn validate_feature_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("invalid feature name `{name}`");
    }
    if name.starts_with('-') || name.starts_with('_') || name.ends_with('-') || name.ends_with('_')
    {
        bail!("invalid feature name `{name}`");
    }
    for ch in name.chars() {
        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_') {
            bail!("invalid feature name `{name}`");
        }
    }
    Ok(())
}

fn validate_feature_member(manifest: &DalManifest, member: &str) -> Result<()> {
    if let Some(dep_name) = member.strip_prefix("dep:") {
        validate_package_name(dep_name)?;
        if !manifest.optional_dependencies.contains_key(dep_name) {
            bail!("unknown optional dependency `{dep_name}`");
        }
        return Ok(());
    }

    validate_feature_name(member)?;
    if !manifest.features.contains_key(member) {
        bail!("unknown feature `{member}`");
    }
    Ok(())
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
    let text = toml::to_string_pretty(manifest).context("failed to encode `dal.toml`")?;
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
    let text = toml::to_string_pretty(&normalized).context("failed to encode `dal.lock`")?;
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
            "unsupported `dal.lock` schema version `{}` (expected {})",
            lock.schema_version,
            DAL_LOCK_SCHEMA_VERSION
        );
    }

    for package in &mut lock.packages {
        validate_package_name(&package.name)?;
        semver::Version::parse(&package.version)
            .with_context(|| format!("invalid locked version `{}`", package.version))?;
        package.module = module_dir_name(&package.name);
        let mut seen = BTreeSet::new();
        package
            .features
            .retain(|feature| seen.insert(feature.clone()));
        for feature in &package.features {
            validate_feature_name(feature)
                .with_context(|| format!("invalid locked feature `{feature}`"))?;
        }
    }

    lock.packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    lock.packages
        .dedup_by(|left, right| left.name == right.name);
    Ok(())
}

pub fn prune_lock_to_dependencies(
    lock: &DalLock,
    roots: &BTreeMap<String, DalDependencySpec>,
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

pub fn resolve_manifest_dependencies(
    manifest: &DalManifest,
    enabled_features: &[String],
) -> Result<BTreeMap<String, DalDependencySpec>> {
    let mut resolved = manifest
        .dependencies
        .iter()
        .map(|(name, spec)| (name.clone(), spec.normalized()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = Vec::new();
    let mut enabled = BTreeSet::new();

    for feature in enabled_features {
        enable_manifest_feature(
            manifest,
            feature,
            &mut enabled,
            &mut visiting,
            &mut resolved,
        )?;
    }

    Ok(resolved)
}

fn enable_manifest_feature(
    manifest: &DalManifest,
    feature: &str,
    enabled: &mut BTreeSet<String>,
    visiting: &mut Vec<String>,
    resolved: &mut BTreeMap<String, DalDependencySpec>,
) -> Result<()> {
    validate_feature_name(feature)?;
    if enabled.contains(feature) {
        return Ok(());
    }
    if let Some(pos) = visiting.iter().position(|entry| entry == feature) {
        let mut cycle = visiting[pos..].to_vec();
        cycle.push(feature.to_string());
        bail!("package feature cycle detected: {}", cycle.join(" -> "));
    }
    let members = manifest
        .features
        .get(feature)
        .ok_or_else(|| anyhow::anyhow!("package does not define feature `{feature}`"))?
        .clone();
    visiting.push(feature.to_string());
    for member in members {
        if let Some(dep_name) = member.strip_prefix("dep:") {
            let spec = manifest
                .optional_dependencies
                .get(dep_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "feature `{feature}` references unknown optional dependency `{dep_name}`"
                    )
                })?
                .normalized();
            resolved.insert(dep_name.to_string(), spec);
        } else {
            enable_manifest_feature(manifest, &member, enabled, visiting, resolved)?;
        }
    }
    visiting.pop();
    enabled.insert(feature.to_string());
    Ok(())
}

fn validate_dependency_roots(roots: &DalDependencyRoots) -> Result<()> {
    for (name, req) in &roots.dependencies {
        validate_package_name(name)?;
        validate_dependency_spec(req)
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
        .ok_or_else(|| anyhow::anyhow!("package `{package}` was missing from `dal.lock`"))?;
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
                    features: vec![],
                },
                DalLockedPackage {
                    name: "my-package".to_string(),
                    module: "stale".to_string(),
                    version: "1.2.3".to_string(),
                    dependencies: BTreeMap::new(),
                    features: vec![],
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
            dependencies: BTreeMap::from([(
                "other-package".to_string(),
                DalDependencySpec::simple("^2.1"),
            )]),
            optional_dependencies: BTreeMap::new(),
            features: BTreeMap::new(),
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
                    features: vec![],
                },
                DalLockedPackage {
                    name: "leaf".to_string(),
                    module: "leaf".to_string(),
                    version: "1.0.0".to_string(),
                    dependencies: BTreeMap::new(),
                    features: vec![],
                },
                DalLockedPackage {
                    name: "unused".to_string(),
                    module: "unused".to_string(),
                    version: "9.9.9".to_string(),
                    dependencies: BTreeMap::new(),
                    features: vec![],
                },
            ],
        };

        let pruned = prune_lock_to_dependencies(
            &lock,
            &BTreeMap::from([("tool".to_string(), DalDependencySpec::simple("=1.0.0"))]),
        )?;

        assert_eq!(pruned.packages.len(), 2);
        assert!(pruned.packages.iter().any(|pkg| pkg.name == "tool"));
        assert!(pruned.packages.iter().any(|pkg| pkg.name == "leaf"));
        assert!(!pruned.packages.iter().any(|pkg| pkg.name == "unused"));
        Ok(())
    }

    #[test]
    fn resolve_manifest_dependencies_enables_optional_deps_via_features() -> Result<()> {
        let manifest = DalManifest {
            package: DalPackageMeta {
                name: "torch".to_string(),
                version: "1.0.0".to_string(),
                readme: None,
            },
            dependencies: BTreeMap::from([("core".to_string(), DalDependencySpec::simple("^1"))]),
            optional_dependencies: BTreeMap::from([(
                "python-runtime".to_string(),
                DalDependencySpec::detailed("^3", vec!["c-api".to_string()]),
            )]),
            features: BTreeMap::from([(
                "pybindings".to_string(),
                vec!["dep:python-runtime".to_string()],
            )]),
            cli: None,
        };

        let resolved = resolve_manifest_dependencies(&manifest, &["pybindings".to_string()])?;
        assert!(resolved.contains_key("core"));
        let python = resolved
            .get("python-runtime")
            .expect("feature should activate optional dependency");
        assert_eq!(python.version_req(), "^3");
        assert_eq!(python.features(), ["c-api"]);
        Ok(())
    }
}
