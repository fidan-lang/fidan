use anyhow::{Context, Result, bail};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const DAL_LOCK_SCHEMA_VERSION: u32 = 1;
/// Shared resolver/lock budget bounds graph work and validation stack depth.
pub const MAX_LOCKED_PACKAGES: usize = 128;

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
    #[serde(rename = "dev-dependencies", default)]
    pub dev_dependencies: BTreeMap<String, DalDependencySpec>,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
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
    /// Optional compiler requirement; absent means no compiler-version restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidan: Option<String>,
    /// Retain registry metadata when add/remove rewrites a project manifest.
    #[serde(flatten)]
    pub additional_metadata: BTreeMap<String, toml::Value>,
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
    /// SHA-256 of the immutable registry archive. Missing only in legacy locks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidan: Option<String>,
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
            optional: false,
            git: None,
            rev: None,
            version: version.into(),
            features,
        };
        detail.normalize();
        if detail.features.is_empty()
            && !detail.optional
            && detail.git.is_none()
            && detail.rev.is_none()
        {
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
                if detail.features.is_empty()
                    && !detail.optional
                    && detail.git.is_none()
                    && detail.rev.is_none()
                {
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

/// Check a compiler constraint independently from the package's own SemVer.
pub fn compiler_compatible(requirement: Option<&str>, compiler_version: &str) -> Result<bool> {
    let Some(requirement) = requirement else {
        return Ok(true);
    };
    let requirement =
        VersionReq::parse(requirement).context("invalid package.fidan compiler requirement")?;
    let compiler = semver::Version::parse(compiler_version).context("invalid compiler version")?;
    Ok(requirement.matches(&compiler))
}

pub fn ensure_compiler_compatible(manifest: &DalManifest) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    if !compiler_compatible(manifest.package.fidan.as_deref(), current)? {
        bail!(
            "package `{}` requires Fidan `{}`, installed compiler is {current}",
            manifest.package.name,
            manifest.package.fidan.as_deref().unwrap_or_default()
        );
    }
    Ok(())
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
    if let Some(requirement) = &manifest.package.fidan {
        VersionReq::parse(requirement).context("invalid package.fidan compiler requirement")?;
    }
    if let Some(readme) = &manifest.package.readme
        && (!is_safe_relative_path(Path::new(readme)) || readme.contains(['\\', ':']))
    {
        bail!("package.readme must be a safe portable relative path");
    }
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
    for (name, spec) in &manifest.dev_dependencies {
        validate_package_name(name)?;
        validate_dependency_spec(spec)?;
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
        if !is_safe_relative_path(entry_path) || cli.entry.contains(['\\', ':']) {
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
    if let DalDependencySpec::Detailed(detail) = spec
        && (detail.git.is_some() || detail.rev.is_some())
    {
        bail!("git dependencies are not supported; use a registry version");
    }
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
        if !manifest.optional_dependencies.contains_key(dep_name)
            && !matches!(manifest.dependencies.get(dep_name), Some(DalDependencySpec::Detailed(detail)) if detail.optional)
        {
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
    crate::install::write_atomic(&path, format!("{text}\n").as_bytes())
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
    crate::install::write_atomic(path, format!("{text}\n").as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))
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
    crate::install::write_atomic(path, format!("{text}\n").as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))
}

pub fn normalize_lock(lock: &mut DalLock) -> Result<()> {
    if lock.packages.len() > MAX_LOCKED_PACKAGES {
        bail!("`dal.lock` exceeds the 128 package limit");
    }
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

    let mut names = BTreeSet::new();
    let mut modules = BTreeSet::new();
    for package in &mut lock.packages {
        if !names.insert(package.name.clone()) || !modules.insert(module_dir_name(&package.name)) {
            bail!("duplicate package or colliding import module in `dal.lock`");
        }
        if let Some(checksum) = &package.checksum
            && (checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            bail!("invalid locked checksum for `{}`", package.name);
        }
        if !compiler_compatible(package.fidan.as_deref(), env!("CARGO_PKG_VERSION"))? {
            bail!(
                "locked package `{}` requires Fidan {}",
                package.name,
                package.fidan.as_deref().unwrap_or_default()
            );
        }
        for (dependency, requirement) in &package.dependencies {
            validate_package_name(dependency)?;
            parse_dependency_req(requirement)?;
        }
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
    let by_name = lock
        .packages
        .iter()
        .map(|pkg| (pkg.name.clone(), pkg))
        .collect::<BTreeMap<_, _>>();
    let mut reachable = BTreeMap::new();
    let mut stack = Vec::new();
    for package in &lock.packages {
        for (dependency, requirement) in &package.dependencies {
            let resolved = by_name
                .get(dependency)
                .with_context(|| format!("package `{dependency}` was missing from `dal.lock`"))?;
            if !parse_dependency_req(requirement)?.matches(&Version::parse(&resolved.version)?) {
                bail!("locked dependency `{dependency}` does not satisfy `{requirement}`");
            }
        }
        visit_locked_package(&package.name, &by_name, &mut reachable, &mut stack)?;
    }
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
        .filter(|(_, spec)| !matches!(spec, DalDependencySpec::Detailed(detail) if detail.optional))
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
    // Untrusted feature chains must not recurse on the Rust call stack.
    let mut pending = vec![(feature.to_owned(), false)];
    let mut active = BTreeSet::new();
    while let Some((name, exiting)) = pending.pop() {
        if exiting {
            visiting.pop();
            active.remove(&name);
            enabled.insert(name);
            continue;
        }
        if let Some(dep_name) = name.strip_prefix("dep:") {
            let spec = manifest.optional_dependencies.get(dep_name)
                .or_else(|| manifest.dependencies.get(dep_name).filter(|spec| matches!(spec, DalDependencySpec::Detailed(detail) if detail.optional)))
                .ok_or_else(|| anyhow::anyhow!("feature references unknown optional dependency `{dep_name}`"))?
                .normalized();
            resolved.insert(dep_name.to_owned(), spec);
            continue;
        }
        validate_feature_name(&name)?;
        if enabled.contains(&name) {
            continue;
        }
        if !active.insert(name.clone()) {
            let pos = visiting
                .iter()
                .position(|value| value == &name)
                .unwrap_or(0);
            let mut cycle = visiting[pos..].to_vec();
            cycle.push(name);
            bail!("package feature cycle detected: {}", cycle.join(" -> "));
        }
        let members = manifest
            .features
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("package does not define feature `{name}`"))?;
        visiting.push(name.clone());
        pending.push((name, true));
        pending.extend(members.iter().rev().cloned().map(|member| (member, false)));
    }
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
    fn untrusted_lock_and_feature_depth_are_bounded_without_host_recursion() {
        let mut lock: DalLock =
            toml::from_str(include_str!("../../../test/fixtures/dal/lock-v1.toml")).unwrap();
        lock.packages = vec![lock.packages[0].clone(); MAX_LOCKED_PACKAGES + 1];
        assert!(
            normalize_lock(&mut lock)
                .unwrap_err()
                .to_string()
                .contains("128 package")
        );
        let mut manifest: DalManifest =
            toml::from_str("[package]\nname='deep'\nversion='1.0.0'\n").unwrap();
        for index in 0..10_000 {
            manifest.features.insert(
                format!("f{index}"),
                if index == 9_999 {
                    vec![]
                } else {
                    vec![format!("f{}", index + 1)]
                },
            );
        }
        resolve_manifest_dependencies(&manifest, &["f0".into()]).unwrap();
        manifest.features.insert("f9999".into(), vec!["f0".into()]);
        assert!(
            resolve_manifest_dependencies(&manifest, &["f0".into()])
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }

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
    fn normalize_lock_sets_modules_and_rejects_duplicate_package_names() -> Result<()> {
        let mut lock = DalLock {
            schema_version: 0,
            packages: vec![
                DalLockedPackage {
                    checksum: None,
                    fidan: None,
                    name: "my-package".to_string(),
                    module: String::new(),
                    version: "1.2.3".to_string(),
                    dependencies: BTreeMap::new(),
                    features: vec![],
                },
                DalLockedPackage {
                    checksum: None,
                    fidan: None,
                    name: "my-package".to_string(),
                    module: "stale".to_string(),
                    version: "1.2.3".to_string(),
                    dependencies: BTreeMap::new(),
                    features: vec![],
                },
            ],
        };

        assert!(normalize_lock(&mut lock).is_err());
        lock.packages.pop();
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
                fidan: None,
                additional_metadata: BTreeMap::new(),
            },
            dependencies: BTreeMap::from([(
                "other-package".to_string(),
                DalDependencySpec::simple("^2.1"),
            )]),
            optional_dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
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
                    checksum: None,
                    fidan: None,
                    name: "tool".to_string(),
                    module: "tool".to_string(),
                    version: "1.0.0".to_string(),
                    dependencies: BTreeMap::from([("leaf".to_string(), "=1.0.0".to_string())]),
                    features: vec![],
                },
                DalLockedPackage {
                    checksum: None,
                    fidan: None,
                    name: "leaf".to_string(),
                    module: "leaf".to_string(),
                    version: "1.0.0".to_string(),
                    dependencies: BTreeMap::new(),
                    features: vec![],
                },
                DalLockedPackage {
                    checksum: None,
                    fidan: None,
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
                fidan: None,
                additional_metadata: BTreeMap::new(),
            },
            dependencies: BTreeMap::from([("core".to_string(), DalDependencySpec::simple("^1"))]),
            optional_dependencies: BTreeMap::from([(
                "python-runtime".to_string(),
                DalDependencySpec::detailed("^3", vec!["c-api".to_string()]),
            )]),
            dev_dependencies: BTreeMap::new(),
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

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn compiler_requirements_are_independent_and_prereleases_are_explicit() {
        for (requirement, compiler, expected) in [
            (None, "1.0.16", true),
            (Some(">=1.0.16, <2.0.0"), "1.0.16", true),
            (Some(">=1.0.16, <2.0.0"), "1.0.15", false),
            (Some(">=1.0.16, <2.0.0"), "2.0.0", false),
            (Some("=1.0.16"), "1.0.16+local", true),
            (Some("^1.0"), "1.1.0-alpha.1", false),
            (Some(">=1.1.0-alpha.1, <2.0.0"), "1.1.0-alpha.2", true),
        ] {
            assert_eq!(
                compiler_compatible(requirement, compiler).unwrap(),
                expected
            );
        }
        assert!(compiler_compatible(Some("not-a-requirement"), "1.0.16").is_err());
    }

    #[test]
    fn registry_metadata_and_dev_dependencies_survive_manifest_rewrites() {
        let source = include_str!("../../../test/fixtures/dal/manifest-v1.toml");
        let original: toml::Value = toml::from_str(source).unwrap();
        let manifest: DalManifest = toml::from_str(source).unwrap();
        validate_manifest(&manifest).unwrap();
        assert_eq!(
            manifest.dev_dependencies["test-support"].version_req(),
            "^1"
        );
        let rewritten: toml::Value =
            toml::from_str(&toml::to_string_pretty(&manifest).unwrap()).unwrap();
        assert_eq!(original, rewritten);
    }
}

#[cfg(test)]
mod lock_contract_tests {
    use super::*;
    #[test]
    fn shared_lock_fixture_preserves_integrity_and_rejects_broken_graphs() {
        let mut lock: DalLock =
            toml::from_str(include_str!("../../../test/fixtures/dal/lock-v1.toml")).unwrap();
        normalize_lock(&mut lock).unwrap();
        assert_eq!(
            lock.packages[0].checksum.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(lock.packages[0].fidan.as_deref(), Some(">=1.0.16, <2.0.0"));
        lock.packages[0]
            .dependencies
            .insert("missing".into(), "^1".into());
        assert!(normalize_lock(&mut lock).is_err());
        lock.packages[0].dependencies.clear();
        lock.packages[0]
            .dependencies
            .insert("core".into(), "^2".into());
        assert!(normalize_lock(&mut lock).is_err());
        lock.packages[0]
            .dependencies
            .insert("core".into(), "^1".into());
        assert!(normalize_lock(&mut lock).is_err());
    }
}

/// Serialize cooperating CLI mutations before reading roots or lock metadata.
pub fn lock_dal_operation(root: &Path) -> Result<fs::File> {
    fs::create_dir_all(root)?;
    let path = root.join(".operation.lock");
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("refusing symlink operation lock");
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock().context("cannot acquire Dal operation lock")?;
    Ok(file)
}

/// Both files are individually atomic. Roll back the first on a checked second
/// failure; a process/power loss between commits still requires an add retry.
fn commit_metadata_pair(
    first: &Path,
    write_first: impl FnOnce() -> Result<()>,
    write_second: impl FnOnce() -> Result<()>,
) -> Result<()> {
    use std::io::Write;
    let backup = match fs::read(first) {
        Ok(bytes) => {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let path = first.with_extension(format!("dal-backup-{}-{nonce}", std::process::id()));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            let written = file.write_all(&bytes).and_then(|()| file.sync_all());
            drop(file);
            if let Err(error) = written {
                fs::remove_file(&path)?;
                return Err(error.into());
            }
            Some(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = write_first() {
        if let Some(backup) = backup {
            fs::remove_file(backup)?;
        }
        return Err(error);
    }
    if let Err(error) = write_second() {
        let restored: Result<()> = match &backup {
            Some(backup) => fs::rename(backup, first).map_err(Into::into),
            None => remove_metadata_if_present(first),
        };
        if let Err(rollback) = restored {
            bail!(
                "{error:#}; metadata rollback failed: {rollback}; original retained at {:?}",
                backup
            );
        }
        return Err(error);
    }
    if let Some(backup) = backup {
        fs::remove_file(&backup).with_context(|| {
            format!(
                "metadata committed; remove retained backup `{}`",
                backup.display()
            )
        })?;
    }
    Ok(())
}

fn remove_metadata_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Caller holds lock_dal_operation(project_root/.dal).
pub fn write_project_state(root: &Path, manifest: &DalManifest, lock: &DalLock) -> Result<()> {
    commit_metadata_pair(
        &manifest_path(root),
        || write_manifest(root, manifest),
        || {
            if lock.packages.is_empty() {
                remove_metadata_if_present(&lock_path(root))
            } else {
                write_lock(root, lock)
            }
        },
    )
}

/// Caller holds lock_dal_operation(global_dal_dir(home)).
pub fn write_global_state(home: &Path, roots: &DalDependencyRoots, lock: &DalLock) -> Result<()> {
    commit_metadata_pair(
        &global_roots_path(home),
        || {
            if roots.dependencies.is_empty() {
                remove_metadata_if_present(&global_roots_path(home))
            } else {
                write_dependency_roots(&global_roots_path(home), roots)
            }
        },
        || {
            if lock.packages.is_empty() {
                remove_metadata_if_present(&global_lock_path(home))
            } else {
                write_lock_to_path(&global_lock_path(home), lock)
            }
        },
    )
}

#[cfg(test)]
mod atomic_metadata_tests {
    use super::*;
    fn temporary_project() -> std::path::PathBuf {
        // Windows clock resolution can give concurrent tests the same timestamp.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fidan-metadata-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn parallel_metadata_tests_use_distinct_directories() {
        let threads: Vec<_> = (0..16)
            .map(|_| std::thread::spawn(temporary_project))
            .collect();
        let paths: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            paths.iter().collect::<std::collections::HashSet<_>>().len(),
            paths.len()
        );
        for path in paths {
            fs::remove_dir(path).unwrap();
        }
    }

    #[test]
    fn failed_lock_commit_restores_exact_original_manifest_and_cleans_backup() {
        let root = temporary_project();
        let original = "# user comments remain intact after a failed update\n[package]\nname=\"consumer\"\nversion=\"0.1.0\"\n";
        fs::write(manifest_path(&root), original).unwrap();
        let mut manifest = read_manifest(&root).unwrap();
        manifest.package.version = "0.2.0".into();
        fs::create_dir(lock_path(&root)).unwrap();
        let lock: DalLock =
            toml::from_str(include_str!("../../../test/fixtures/dal/lock-v1.toml")).unwrap();
        assert!(write_project_state(&root, &manifest, &lock).is_err());
        assert_eq!(fs::read_to_string(manifest_path(&root)).unwrap(), original);
        assert!(lock_path(&root).is_dir());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn windows_denied_atomic_replacement_preserves_previous_file() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = temporary_project();
        let path = root.join("metadata.json");
        fs::write(&path, "original").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(crate::install::write_atomic(&path, b"replacement").is_err());
        drop(held);
        assert_eq!(fs::read_to_string(path).unwrap(), "original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
