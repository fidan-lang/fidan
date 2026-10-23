//! Deterministic bounded backtracking over immutable, checksum-verified candidates.
use super::{
    api::{DalClient, IndexEntry},
    archive::read_manifest_from_archive,
};
use anyhow::{Context, Result, bail};
use fidan_driver::dal::{
    DalDependencySpec, DalLock, DalLockedPackage, DalManifest, compiler_compatible,
    ensure_compiler_compatible, module_dir_name, parse_dependency_req, prune_lock_to_dependencies,
    resolve_manifest_dependencies, validate_package_name,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const MAX_DECISIONS: usize = 10_000;

const MAX_CACHED_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone)]
struct Candidate {
    entry: IndexEntry,
    manifest: DalManifest,
    archive: Arc<Vec<u8>>,
}
#[derive(Clone)]
pub(super) struct ResolvedPackageState {
    pub name: String,
    pub version: String,
    dependencies: BTreeMap<String, DalDependencySpec>,
    features: Vec<String>,
    checksum: String,
    fidan: Option<String>,
}
pub(super) struct ResolvedGraph {
    pub top_version: String,
    pub packages: BTreeMap<String, ResolvedPackageState>,
    pub archives: BTreeMap<String, Arc<Vec<u8>>>,
}
impl ResolvedGraph {
    pub fn to_lock(&self) -> DalLock {
        DalLock {
            packages: self
                .packages
                .values()
                .map(|pkg| DalLockedPackage {
                    name: pkg.name.clone(),
                    module: module_dir_name(&pkg.name),
                    version: pkg.version.clone(),
                    checksum: Some(pkg.checksum.clone()),
                    fidan: pkg.fidan.clone(),
                    features: pkg.features.clone(),
                    dependencies: pkg
                        .dependencies
                        .keys()
                        .map(|name| (name.clone(), format!("={}", self.packages[name].version)))
                        .collect(),
                })
                .collect(),
            ..Default::default()
        }
    }
}

pub(super) struct PackageResolver<'a> {
    client: &'a DalClient,
    lock: Option<&'a DalLock>,
    indexes: BTreeMap<String, Vec<IndexEntry>>,
    candidates: BTreeMap<(String, String), Candidate>,
    decisions: usize,
    cached_bytes: usize,
    conflict: String,
}
impl<'a> PackageResolver<'a> {
    pub fn new(client: &'a DalClient, lock: Option<&'a DalLock>) -> Self {
        Self {
            client,
            lock,
            indexes: BTreeMap::new(),
            candidates: BTreeMap::new(),
            decisions: 0,
            cached_bytes: 0,
            conflict: String::new(),
        }
    }
    pub fn resolve_all(
        mut self,
        top: &str,
        roots: &BTreeMap<String, DalDependencySpec>,
    ) -> Result<ResolvedGraph> {
        fidan_diagnostics::tracing::info!(
            roots = roots.len(),
            locked = self.lock.is_some(),
            "resolving package graph"
        );
        let mut graph = self
            .search(roots, BTreeMap::new())?
            .with_context(|| format!("no compatible package graph: {}", self.conflict))?;
        graph.top_version = graph
            .packages
            .get(top)
            .context("requested package missing from graph")?
            .version
            .clone();
        fidan_diagnostics::tracing::info!(
            packages = graph.packages.len(),
            "package graph resolved"
        );
        Ok(graph)
    }
    fn search(
        &mut self,
        roots: &BTreeMap<String, DalDependencySpec>,
        selected: BTreeMap<String, Candidate>,
    ) -> Result<Option<ResolvedGraph>> {
        self.decisions += 1;
        fidan_diagnostics::tracing::trace!(
            decision = self.decisions,
            selected = selected.len(),
            "resolver branch"
        );
        if self.decisions > MAX_DECISIONS || selected.len() > fidan_driver::dal::MAX_LOCKED_PACKAGES
        {
            bail!("package resolution complexity limit exceeded");
        }
        // Recompute requirements from roots on every branch. Constraints from rejected
        // versions must never survive backtracking. Features monotonically unify here.
        let mut requirements: BTreeMap<String, Vec<DalDependencySpec>> = roots
            .iter()
            .map(|(k, v)| (k.clone(), vec![v.clone()]))
            .collect();
        let mut expanded: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut packages = BTreeMap::new();
        loop {
            let mut changed = false;
            for (name, specs) in requirements.clone() {
                validate_package_name(&name)?;
                if requirements.len() > fidan_driver::dal::MAX_LOCKED_PACKAGES {
                    bail!("package graph exceeds package limit");
                }
                let Some(candidate) = selected.get(&name) else {
                    continue;
                };
                let version = semver::Version::parse(&candidate.entry.vers)?;
                if specs
                    .iter()
                    .map(|s| parse_dependency_req(s.version_req()))
                    .collect::<Result<Vec<_>>>()?
                    .iter()
                    .any(|r| !r.matches(&version))
                {
                    self.conflict = format!("conflicting requirements for `{name}`");
                    return Ok(None);
                }
                let features = specs
                    .iter()
                    .flat_map(|s| s.features().iter().cloned())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                if expanded.get(&name) == Some(&features) {
                    continue;
                }
                let dependencies =
                    match resolve_manifest_dependencies(&candidate.manifest, &features) {
                        Ok(deps) => deps,
                        Err(error) => {
                            self.conflict = format!("{name}: {error}");
                            return Ok(None);
                        }
                    };
                for (dep, spec) in &dependencies {
                    let entries = requirements.entry(dep.clone()).or_default();
                    if !entries.contains(spec) {
                        entries.push(spec.clone());
                    }
                }
                packages.insert(
                    name.clone(),
                    ResolvedPackageState {
                        name: name.clone(),
                        version: candidate.entry.vers.clone(),
                        dependencies,
                        features: features.clone(),
                        checksum: candidate.entry.cksum.clone(),
                        fidan: candidate.manifest.package.fidan.clone(),
                    },
                );
                expanded.insert(name, features);
                changed = true;
            }
            if !changed {
                break;
            }
        }
        let pending = requirements
            .keys()
            .find(|name| !selected.contains_key(*name))
            .cloned();
        let Some(name) = pending else {
            let graph = ResolvedGraph {
                top_version: String::new(),
                archives: packages
                    .keys()
                    .map(|name| (name.clone(), selected[name].archive.clone()))
                    .collect(),
                packages,
            };
            if let Err(error) = prune_lock_to_dependencies(&graph.to_lock(), roots) {
                self.conflict = error.to_string();
                return Ok(None);
            }
            return Ok(Some(graph));
        };
        fidan_diagnostics::tracing::debug!(package = %name, index_cached = self.indexes.contains_key(&name), "resolving dependency");
        if !self.indexes.contains_key(&name) {
            self.indexes.insert(name.clone(), self.client.index(&name)?);
        }
        let locked = self
            .lock
            .and_then(|l| l.packages.iter().find(|p| p.name == name));
        let reqs = requirements[&name]
            .iter()
            .map(|s| parse_dependency_req(s.version_req()))
            .collect::<Result<Vec<_>>>()?;
        let mut entries = Vec::new();
        for entry in &self.indexes[&name] {
            let is_locked = locked.is_some_and(|p| {
                p.version == entry.vers
                    && p.checksum
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(&entry.cksum))
            });
            if locked.is_some_and(|p| {
                p.version == entry.vers
                    && p.checksum
                        .as_deref()
                        .is_some_and(|c| !c.eq_ignore_ascii_case(&entry.cksum))
            }) {
                bail!(
                    "registry checksum changed for locked `{name}@{}`",
                    entry.vers
                );
            }
            if (!entry.yanked || is_locked)
                && compiler_compatible(entry.fidan.as_deref(), env!("CARGO_PKG_VERSION"))?
            {
                let version = semver::Version::parse(&entry.vers)?;
                if reqs.iter().all(|r| r.matches(&version)) {
                    entries.push((entry.clone(), version));
                }
            }
        }
        entries.sort_by(|a, b| {
            let a_locked = locked.is_some_and(|p| p.version == a.0.vers);
            let b_locked = locked.is_some_and(|p| p.version == b.0.vers);
            b_locked.cmp(&a_locked).then_with(|| b.1.cmp(&a.1))
        });
        self.conflict = format!(
            "no allowed version of `{name}` satisfies its requirements (yanked versions require an existing checksum lock)"
        );
        for (entry, _) in entries {
            fidan_diagnostics::tracing::trace!(package = %name, version = %entry.vers, cached = self.candidates.contains_key(&(name.clone(), entry.vers.clone())), "considering package candidate");
            let key = (name.clone(), entry.vers.clone());
            if !self.candidates.contains_key(&key) {
                let bytes = self
                    .client
                    .download_archive(&name, &entry.vers, &entry.cksum)?;
                self.cached_bytes += bytes.len();
                if self.cached_bytes > MAX_CACHED_BYTES {
                    bail!("package resolution archive cache limit exceeded");
                }
                let manifest = read_manifest_from_archive(&bytes)?;
                if manifest.package.name != name || manifest.package.version != entry.vers {
                    bail!(
                        "downloaded archive manifest does not match `{name}@{}`",
                        entry.vers
                    );
                }
                ensure_compiler_compatible(&manifest)?;
                self.candidates.insert(
                    key.clone(),
                    Candidate {
                        entry,
                        manifest,
                        archive: Arc::new(bytes),
                    },
                );
            }
            let mut branch = selected.clone();
            branch.insert(name.clone(), self.candidates[&key].clone());
            if let Some(solution) = self.search(roots, branch)? {
                return Ok(Some(solution));
            }
        }
        Ok(None)
    }
}
