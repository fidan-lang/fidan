use anyhow::{Context, Result, bail};
use fidan_driver::dal::{
    DalManifest, package_install_dir, read_manifest, remap_source_relative_path,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};
use tar::{Archive, Builder, EntryType};

const ALLOWED_TOP_FILES: &[&str] = &[
    "dal.toml",
    "dal.lock",
    ".dalignore",
    "README",
    "README.md",
    "README.txt",
    "CHANGELOG.md",
];

const ALLOWED_TOP_DIRS: &[&str] = &["src", "examples", "tests", "docs", "assets"];

#[derive(Debug, Clone)]
pub struct BuiltPackage {
    pub manifest: DalManifest,
    pub archive_name: String,
    pub archive_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct DalIgnore {
    set: GlobSet,
    include: Option<GlobSet>,
}

pub fn build_package_archive(project_dir: &Path) -> Result<BuiltPackage> {
    let project_dir = project_dir
        .canonicalize()
        .with_context(|| format!("cannot access {}", project_dir.display()))?;
    let manifest = read_manifest(&project_dir)?;
    fidan_diagnostics::tracing::info!(package = %manifest.package.name, version = %manifest.package.version, "building package archive");
    let ignore = load_dalignore(&project_dir, &manifest)?;
    validate_package_dir(&project_dir, &manifest, ignore.as_ref())?;
    validate_pack_budget(&project_dir, &project_dir, ignore.as_ref(), &mut (0, 0))?;

    let root_name = package_root_name(&manifest);
    let archive_name = format!("{root_name}.tar.gz");

    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = Builder::new(encoder);

    append_allowed_entries(&mut builder, &project_dir, &root_name, ignore.as_ref())?;

    let encoder = builder
        .into_inner()
        .context("failed to finalize tar archive")?;
    let archive_bytes = encoder
        .finish()
        .context("failed to finalize gzip archive")?;

    visit_archive(&archive_bytes, |_, _, _| Ok(()))?;
    fidan_diagnostics::tracing::debug!(bytes = archive_bytes.len(), "package archive validated");
    Ok(BuiltPackage {
        manifest,
        archive_name,
        archive_bytes,
    })
}

const MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;

// All archive consumers share validation before accepting manifest data or files.
fn visit_archive(
    bytes: &[u8],
    mut visit: impl FnMut(&str, EntryType, &mut dyn Read) -> Result<()>,
) -> Result<String> {
    if bytes.len() as u64 > super::api::MAX_ARCHIVE_BYTES {
        bail!("archive exceeds the compressed size limit");
    }
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder.take(MAX_UNCOMPRESSED_BYTES + 1));
    let mut root = None;
    let mut seen = HashSet::new();
    let mut destinations = HashMap::new();
    let mut destination_parents = HashMap::new();
    let mut spellings = HashMap::new();
    let mut total = 0_u64;
    let mut has_manifest = false;
    let mut has_init = false;
    for (count, entry) in archive
        .entries()
        .context("cannot read archive entries")?
        .enumerate()
    {
        if count >= MAX_ARCHIVE_ENTRIES {
            bail!("archive contains too many entries");
        }
        let mut entry = entry.context("invalid archive entry")?;
        let kind = entry.header().entry_type();
        if kind != EntryType::Regular && kind != EntryType::Directory {
            bail!("archive contains unsupported entry type");
        }
        let path_bytes = entry.path_bytes();
        let path = std::str::from_utf8(&path_bytes)
            .context("archive path is not valid UTF-8")?
            .trim_end_matches('/')
            .to_string();
        if path.len() > 4096 || path.contains(['\\', ':']) {
            bail!("archive contains an unsafe portable path");
        }
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() > 64 {
            bail!("archive nesting exceeds the portable depth limit");
        }
        let mut prefix = String::new();
        for part in &parts {
            prefix.push_str(part);
            prefix.push('/');
            if let Some(previous) = spellings.insert(prefix.to_ascii_lowercase(), prefix.clone())
                && previous != prefix
            {
                bail!("archive contains case-aliased paths");
            }
        }
        if parts.iter().any(|part| {
            part.is_empty()
                || matches!(*part, "." | "..")
                || part.ends_with(['.', ' '])
                || part
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
        }) {
            bail!("archive contains an unsafe path");
        }
        for part in &parts {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
            {
                bail!("archive contains a Windows device path");
            }
        }
        if root.as_deref().is_some_and(|expected| expected != parts[0]) {
            bail!("archive contains multiple package roots");
        }
        root.get_or_insert_with(|| parts[0].to_string());
        if !seen.insert(path.to_ascii_lowercase()) {
            bail!("archive contains duplicate paths");
        }
        let size = entry.size();
        total = total.checked_add(size).context("archive size overflow")?;
        if total > MAX_UNCOMPRESSED_BYTES || total > (bytes.len() as u64).saturating_mul(100) {
            bail!("archive exceeds expansion limits");
        }
        if parts.len() == 1 {
            if kind != EntryType::Directory {
                bail!("archive root must be a directory");
            }
            continue;
        }
        let relative = parts[1..].join("/");
        if !ALLOWED_TOP_DIRS.contains(&parts[1])
            && !(parts.len() == 2
                && (ALLOWED_TOP_FILES.contains(&parts[1]) || is_legal_file(parts[1])))
        {
            bail!("archive contains unsupported top-level entry");
        }
        if relative == "src" && kind == EntryType::Directory {
            continue;
        }
        let destination = remap_source_relative_path(Path::new(&relative));
        // src/ is flattened by the existing install ABI. Reject aliases before writing.
        let destination_name = destination.to_string_lossy().into_owned();
        let segments: Vec<_> = destination_name.split('/').collect();
        for depth in 1..segments.len() {
            let prefix = segments[..depth].join("/");
            if let Some((_, previous_kind)) = destinations.get(&prefix.to_ascii_lowercase())
                && *previous_kind != EntryType::Directory
            {
                bail!("archive file used as parent directory");
            }
            if let Some(previous) =
                destination_parents.insert(prefix.to_ascii_lowercase(), prefix.clone())
                && previous != prefix
            {
                bail!("archive contains remapped case-aliased parent directories");
            }
        }
        if kind == EntryType::Regular
            && destination_parents.contains_key(&destination_name.to_ascii_lowercase())
        {
            bail!("archive file collides with an implicit directory");
        }
        if let Some((previous_name, previous_kind)) = destinations.insert(
            destination_name.to_ascii_lowercase(),
            (destination_name.clone(), kind),
        ) && (previous_name != destination_name
            || previous_kind != EntryType::Directory
            || kind != EntryType::Directory)
        {
            bail!("archive entries collide after source-path remapping");
        }
        if relative == "dal.toml" && kind == EntryType::Regular {
            if size > MAX_MANIFEST_BYTES {
                bail!("archive manifest exceeds size limit");
            }
            has_manifest = true;
        }
        has_init |= relative == "src/init.fdn" && kind == EntryType::Regular;
        visit(&relative, kind, &mut entry)?;
    }
    // TAR iteration can stop before gzip's trailer; drain bounded input to check CRC.
    let mut reader = archive.into_inner();
    io::copy(&mut reader, &mut io::sink()).context("invalid gzip stream")?;
    if reader.limit() == 0 {
        bail!("archive exceeds uncompressed size limit");
    }
    if !has_manifest || !has_init {
        bail!("archive must contain dal.toml and src/init.fdn");
    }
    root.context("archive is empty")
}

struct StagedDirectory(PathBuf);

impl StagedDirectory {
    fn new(parent: &Path) -> Result<Self> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = parent.join(format!(".dal-stage-{}-{nonce}", std::process::id()));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder
            .create(&path)
            .context("cannot create private install staging directory")?;
        Ok(Self(path))
    }
}

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("warning: cannot clean Dal staging directory: {error}");
        }
    }
}

fn reject_symlink_ancestors(path: &Path) -> Result<()> {
    let absolute = std::path::absolute(path)?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component);
        if !matches!(component, Component::Normal(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("installation path contains a symbolic link")
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn identical_tree(left: &Path, right: &Path) -> Result<bool> {
    let left_meta = fs::symlink_metadata(left)?;
    let right_meta = fs::symlink_metadata(right)?;
    if left_meta.file_type().is_symlink() || right_meta.file_type().is_symlink() {
        return Ok(false);
    }
    if left_meta.is_file() && right_meta.is_file() {
        if left_meta.len() != right_meta.len() {
            return Ok(false);
        }
        let limit = left_meta
            .len()
            .checked_add(1)
            .context("cached file length overflow")?;
        let mut expected = Vec::new();
        let mut cached = Vec::new();
        fs::File::open(left)?
            .take(limit)
            .read_to_end(&mut expected)?;
        fs::File::open(right)?
            .take(limit)
            .read_to_end(&mut cached)?;
        return Ok(expected == cached && expected.len() as u64 == left_meta.len());
    }
    if !left_meta.is_dir() || !right_meta.is_dir() {
        return Ok(false);
    }
    let names = |dir: &Path| -> Result<std::collections::BTreeSet<_>> {
        fs::read_dir(dir)?
            .take(MAX_ARCHIVE_ENTRIES + 1)
            .map(|entry| Ok(entry?.file_name()))
            .collect()
    };
    let entries = names(left)?;
    if entries != names(right)? {
        return Ok(false);
    }
    for name in entries {
        if !identical_tree(&left.join(&name), &right.join(&name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn install_downloaded_package(
    archive_bytes: &[u8],
    package: &str,
    version: &str,
    into_dir: &Path,
    force: bool,
) -> Result<PathBuf> {
    fidan_driver::dal::validate_package_name(package)?;
    semver::Version::parse(version).context("invalid package version")?;
    let target = package_install_dir(into_dir, package, version);
    reject_symlink_ancestors(&target)?;
    let parent = target
        .parent()
        .context("missing package parent directory")?;
    fs::create_dir_all(parent)?;
    fidan_diagnostics::tracing::info!(package, version, "installing verified package");
    let staging = StagedDirectory::new(parent)?;
    let root = visit_archive(archive_bytes, |relative, kind, reader| {
        let output = remap_install_path(&staging.0, Path::new(relative));
        if kind == EntryType::Directory {
            fs::create_dir_all(&output)?;
        } else {
            fs::create_dir_all(output.parent().context("missing archive entry parent")?)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)?;
            io::copy(reader, &mut file)?;
        }
        Ok(())
    })?;
    let manifest = read_manifest(&staging.0)?;
    fidan_driver::dal::ensure_compiler_compatible(&manifest)?;
    if root != format!("{package}-{version}")
        || manifest.package.name != package
        || manifest.package.version != version
    {
        bail!("archive identity does not match requested package/version");
    }
    if target.exists() && !force {
        if !identical_tree(&staging.0, &target)? {
            bail!("existing package differs from verified archive; use --force to replace it");
        }
        return Ok(target);
    }
    if target.exists() {
        let backup = StagedDirectory::new(parent)?;
        let previous = backup.0.join("previous");
        fs::rename(&target, &previous)?;
        if let Err(error) = fs::rename(&staging.0, &target) {
            if let Err(restore_error) = fs::rename(&previous, &target) {
                let saved = previous.display().to_string();
                // Preserve the original installation if rollback itself fails.
                std::mem::forget(backup);
                bail!(
                    "install failed ({error}); rollback failed ({restore_error}); original preserved at {saved}"
                );
            }
            return Err(error.into());
        }
    } else {
        fs::rename(&staging.0, &target)?;
    }
    fidan_diagnostics::tracing::debug!(path = %target.display(), "package installation committed");
    Ok(target)
}

pub fn read_manifest_from_archive(archive_bytes: &[u8]) -> Result<DalManifest> {
    let mut manifest = None;
    let root = visit_archive(archive_bytes, |relative, kind, reader| {
        if relative == "dal.toml" && kind == EntryType::Regular {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes)?;
            let parsed: DalManifest =
                toml::from_slice(&bytes).context("invalid dal.toml inside archive")?;
            fidan_driver::dal::validate_manifest(&parsed)?;
            manifest = Some(parsed);
        }
        Ok(())
    })?;
    let manifest = manifest.context("missing archive manifest")?;
    if root != package_root_name(&manifest) {
        bail!("archive root does not match manifest identity");
    }
    Ok(manifest)
}

fn validate_package_dir(
    project_dir: &Path,
    manifest: &DalManifest,
    ignore: Option<&DalIgnore>,
) -> Result<()> {
    let mut allowed = HashSet::new();
    for name in ALLOWED_TOP_FILES {
        allowed.insert((*name).to_string());
    }
    for name in ALLOWED_TOP_DIRS {
        allowed.insert((*name).to_string());
    }

    let readme = manifest.package.readme.as_deref().unwrap_or("README.md");
    let readme_path = project_dir.join(readme);
    if manifest.package.readme.is_some() && !readme_path.is_file() {
        bail!("declared readme `{readme}` not found");
    }

    let init_path = project_dir.join("src").join("init.fdn");
    if !init_path.is_file() {
        bail!("package must contain src/init.fdn");
    }
    if is_ignored_path(ignore, project_dir, &init_path) {
        bail!("`.dalignore` must not exclude required path `src/init.fdn`");
    }

    if (!manifest.dependencies.is_empty() || !manifest.optional_dependencies.is_empty())
        && !project_dir.join("dal.lock").is_file()
    {
        bail!("packages with dependencies must include a `dal.lock` file");
    }
    if project_dir.join("dal.lock").is_file()
        && is_ignored_path(ignore, project_dir, &project_dir.join("dal.lock"))
    {
        bail!("`.dalignore` must not exclude required path `dal.lock`");
    }
    if manifest.package.readme.is_some() && is_ignored_path(ignore, project_dir, &readme_path) {
        bail!("`.dalignore` must not exclude the declared readme `{readme}`");
    }

    if let Some(cli) = &manifest.cli {
        let entry_path = project_dir.join(&cli.entry);
        if !entry_path.is_file() {
            bail!("declared CLI entry `{}` not found", cli.entry);
        }
        if is_ignored_path(ignore, project_dir, &entry_path) {
            bail!(
                "`.dalignore` must not exclude the declared CLI entry `{}`",
                cli.entry
            );
        }
    }

    for entry in fs::read_dir(project_dir)
        .with_context(|| format!("cannot read {}", project_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if is_ignored_path(ignore, project_dir, &path) {
            continue;
        }
        if !allowed.contains(&file_name) && !is_legal_file(&file_name) {
            // A package include list selects archive files, not every development
            // directory in the source checkout. Still reject selected forbidden files.
            if ignore.is_some_and(|rules| rules.include.is_some())
                && path.is_dir()
                && !fs::symlink_metadata(&path)?.file_type().is_symlink()
                && !validate_tree(&path, project_dir, ignore)?
            {
                continue;
            }
            bail!("top-level entry `{file_name}` is not allowed in a Dal package archive");
        }

        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if meta.file_type().is_symlink() {
            bail!("symlinks are not allowed in Dal packages");
        }
    }

    for dir_name in ALLOWED_TOP_DIRS {
        let dir = project_dir.join(dir_name);
        if dir.exists() && !is_ignored_path(ignore, project_dir, &dir) {
            validate_tree(&dir, project_dir, ignore)?;
        }
    }

    for file_name in ALLOWED_TOP_FILES {
        let file = project_dir.join(file_name);
        if file.exists()
            && *file_name != ".dalignore"
            && !is_ignored_path(ignore, project_dir, &file)
        {
            validate_file(&file, project_dir)?;
        }
    }

    if readme_path.exists() && !is_ignored_path(ignore, project_dir, &readme_path) {
        validate_file(&readme_path, project_dir)?;
    }

    for entry in fs::read_dir(project_dir)
        .with_context(|| format!("cannot read {}", project_dir.display()))?
    {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        if is_legal_file(&file_name) && !is_ignored_path(ignore, project_dir, &entry.path()) {
            validate_file(&entry.path(), project_dir)?;
        }
    }

    Ok(())
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)?
        .take(MAX_ARCHIVE_ENTRIES + 1)
        .collect::<io::Result<Vec<_>>>()?;
    if entries.len() > MAX_ARCHIVE_ENTRIES {
        bail!("package contains too many entries");
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}
fn validate_pack_budget(
    path: &Path,
    root: &Path,
    ignore: Option<&DalIgnore>,
    budget: &mut (usize, u64),
) -> Result<()> {
    if path.strip_prefix(root)?.components().count() > 64 {
        bail!("package nesting exceeds limit");
    }
    for entry in sorted_entries(path)? {
        if is_ignored_path(ignore, root, &entry.path()) {
            continue;
        }
        let metadata = entry.path().symlink_metadata()?;
        if metadata.file_type().is_symlink() {
            bail!("symlinks are not allowed in Dal packages");
        }
        budget.0 += 1;
        budget.1 = budget
            .1
            .checked_add(metadata.len())
            .context("package size overflow")?;
        if budget.0 > MAX_ARCHIVE_ENTRIES || budget.1 > MAX_UNCOMPRESSED_BYTES {
            bail!("package exceeds archive limits");
        }
        if metadata.is_dir() {
            validate_pack_budget(&entry.path(), root, ignore, budget)?;
        }
    }
    Ok(())
}

fn append_allowed_entries(
    builder: &mut Builder<GzEncoder<Vec<u8>>>,
    project_dir: &Path,
    root_name: &str,
    ignore: Option<&DalIgnore>,
) -> Result<()> {
    append_file(
        builder,
        &project_dir.join("dal.toml"),
        Path::new(root_name).join("dal.toml"),
    )?;

    for entry in sorted_entries(project_dir)? {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if is_ignored_path(ignore, project_dir, &path) {
            continue;
        }
        if name == "dal.toml" {
            continue;
        }
        if ALLOWED_TOP_FILES.contains(&name.as_str()) || is_legal_file(&name) {
            if name != ".dalignore" && path.is_file() {
                append_file(builder, &path, Path::new(root_name).join(&name))?;
            }
            continue;
        }
        if ALLOWED_TOP_DIRS.contains(&name.as_str()) && path.is_dir() {
            append_tree(
                builder,
                &path,
                &path,
                Path::new(root_name),
                project_dir,
                ignore,
            )?;
        }
    }

    Ok(())
}

fn append_tree(
    builder: &mut Builder<GzEncoder<Vec<u8>>>,
    dir: &Path,
    anchor: &Path,
    root: &Path,
    project_dir: &Path,
    ignore: Option<&DalIgnore>,
) -> Result<()> {
    for entry in sorted_entries(dir)? {
        let path = entry.path();
        if is_ignored_path(ignore, project_dir, &path) {
            continue;
        }
        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if meta.file_type().is_symlink() {
            bail!("symlinks are not allowed in Dal packages");
        }
        let rel = path
            .strip_prefix(anchor.parent().unwrap_or(anchor))
            .unwrap_or(&path);
        let archive_path = root.join(rel);
        if meta.is_dir() {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(&mut header, &archive_path, io::empty())?;
            append_tree(builder, &path, anchor, root, project_dir, ignore)?;
        } else if meta.is_file() {
            append_file(builder, &path, archive_path)?;
        }
    }
    Ok(())
}

fn append_file(
    builder: &mut Builder<GzEncoder<Vec<u8>>>,
    source: &Path,
    archive_path: PathBuf,
) -> Result<()> {
    let meta = fs::symlink_metadata(source)
        .with_context(|| format!("cannot read {}", source.display()))?;
    if meta.file_type().is_symlink() {
        bail!("symlinks are not allowed in Dal packages");
    }
    if !meta.is_file() {
        bail!("package entries must be regular files");
    }
    if meta.len() > MAX_UNCOMPRESSED_BYTES {
        bail!("package file exceeds archive limit");
    }
    let mut header = tar::Header::new_gnu();
    header.set_size(meta.len());
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            archive_path,
            fs::File::open(source)?.take(meta.len()),
        )
        .with_context(|| format!("cannot archive {}", source.display()))?;
    Ok(())
}

fn validate_tree(path: &Path, project_dir: &Path, ignore: Option<&DalIgnore>) -> Result<bool> {
    if path.strip_prefix(project_dir)?.components().count() > 64 {
        bail!("package nesting exceeds limit");
    }
    let mut selected = false;
    for entry in fs::read_dir(path).with_context(|| format!("cannot read {}", path.display()))? {
        let entry = entry?;
        if is_ignored_path(ignore, project_dir, &entry.path()) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())
            .with_context(|| format!("cannot read {}", entry.path().display()))?;
        if meta.file_type().is_symlink() {
            bail!("symlinks are not allowed in Dal packages");
        }
        if meta.is_dir() {
            selected |= validate_tree(&entry.path(), project_dir, ignore)?;
        } else if meta.is_file() {
            validate_file(&entry.path(), project_dir)?;
            selected = true;
        }
    }
    let relative = path.strip_prefix(project_dir).unwrap_or(path);
    if !is_safe_relative_path(relative) {
        bail!("package contains unsafe path `{}`", relative.display());
    }
    Ok(selected)
}

fn validate_file(path: &Path, project_dir: &Path) -> Result<()> {
    let relative = path.strip_prefix(project_dir).unwrap_or(path);
    if !is_safe_relative_path(relative) {
        bail!("package contains unsafe path `{}`", relative.display());
    }
    Ok(())
}

fn is_legal_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "license"
        || lower.starts_with("license.")
        || lower == "notice"
        || lower.starts_with("notice.")
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path.components().all(|component| match component {
            Component::Normal(_) => true,
            Component::CurDir => true,
            Component::ParentDir => false,
            Component::RootDir | Component::Prefix(_) => false,
        })
}

fn remap_install_path(target_dir: &Path, remainder: &Path) -> PathBuf {
    target_dir.join(remap_source_relative_path(remainder))
}

fn package_root_name(manifest: &DalManifest) -> String {
    format!("{}-{}", manifest.package.name, manifest.package.version)
}

fn load_dalignore(project_dir: &Path, manifest: &DalManifest) -> Result<Option<DalIgnore>> {
    let ignore_path = project_dir.join(".dalignore");
    let raw = if ignore_path.is_file() {
        fs::read_to_string(&ignore_path)
            .with_context(|| format!("cannot read {}", ignore_path.display()))?
    } else {
        String::new()
    };
    let mut builder = GlobSetBuilder::new();
    for pattern in [
        ".git",
        ".git/**",
        "target",
        "target/**",
        "node_modules",
        "node_modules/**",
        ".env",
        ".env.*",
        "**/.env",
        "**/.env.*",
    ] {
        builder.add(Glob::new(pattern)?);
    }
    let mut include_builder = GlobSetBuilder::new();
    let mut has_include = false;
    for key in ["include", "exclude"] {
        if let Some(value) = manifest.package.additional_metadata.get(key) {
            for pattern in value
                .as_array()
                .context("package include/exclude must be a string array")?
            {
                let pattern = pattern
                    .as_str()
                    .context("package include/exclude must contain strings")?;
                for normalized in normalize_ignore_patterns(pattern) {
                    if key == "include" {
                        has_include = true;
                        include_builder.add(Glob::new(&normalized)?);
                    } else {
                        builder.add(Glob::new(&normalized)?);
                    }
                }
            }
        }
    }
    for (idx, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for normalized in normalize_ignore_patterns(line) {
            let glob = Glob::new(&normalized).with_context(|| {
                format!(
                    "invalid .dalignore pattern on line {}: `{}`",
                    idx + 1,
                    raw_line
                )
            })?;
            builder.add(glob);
        }
    }
    let set = builder
        .build()
        .context("cannot compile .dalignore patterns")?;
    Ok(Some(DalIgnore {
        set,
        include: if has_include {
            Some(include_builder.build()?)
        } else {
            None
        },
    }))
}

fn normalize_ignore_patterns(pattern: &str) -> Vec<String> {
    let mut normalized = pattern.replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    if normalized.ends_with('/') {
        let bare = normalized.trim_end_matches('/').to_string();
        return vec![bare.clone(), format!("{bare}/**")];
    }
    vec![normalized]
}

fn is_ignored_path(ignore: Option<&DalIgnore>, project_dir: &Path, path: &Path) -> bool {
    let Some(ignore) = ignore else {
        return false;
    };
    let Ok(relative) = path.strip_prefix(project_dir) else {
        return false;
    };
    let rel = relative.to_string_lossy().replace('\\', "/");
    if rel.is_empty() {
        return false;
    }
    if ignore.set.is_match(&rel) {
        return true;
    }
    // Traverse directories even with include filters; always ship manifest/lock.
    if path.is_dir() || matches!(rel.as_str(), "dal.toml" | "dal.lock") {
        return false;
    }
    ignore
        .include
        .as_ref()
        .is_some_and(|set| !set.is_match(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_driver::dal::module_dir_name;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn include_selection_ignores_development_directories_but_rejects_forbidden_selected_entries()
    -> Result<()> {
        let root = make_temp_dir("fidan_unselected_directory");
        fs::create_dir_all(root.join("src"))?;
        fs::create_dir_all(root.join("scripts"))?;
        fs::write(
            root.join("src/init.fdn"),
            "action version returns integer { return 1 }\n",
        )?;
        fs::write(root.join("scripts/check.mjs"), "console.log('local only');")?;
        let manifest = "[package]\nname='selection'\nversion='1.0.0'\ninclude=['src/**']\n";
        fs::write(root.join("dal.toml"), manifest)?;
        let built = build_package_archive(&root)?;
        visit_archive(&built.archive_bytes, |path, _, _| {
            assert!(!path.contains("scripts"));
            Ok(())
        })?;
        fs::write(
            root.join("dal.toml"),
            manifest.replace("['src/**']", "['src/**', 'scripts/**']"),
        )?;
        assert!(
            build_package_archive(&root)
                .unwrap_err()
                .to_string()
                .contains("top-level entry")
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn packaging_is_reproducible_and_honors_selection_without_shipping_private_files() -> Result<()>
    {
        let root = make_temp_dir("fidan_selection");
        fs::create_dir_all(root.join("src"))?;
        fs::create_dir_all(root.join(".git"))?;
        fs::write(root.join("src/init.fdn"), "action hello {}\n")?;
        fs::write(root.join("src/private.fdn"), "private")?;
        fs::write(root.join("src/.env"), "fake-only")?;
        fs::write(root.join(".env"), "fake-only")?;
        fs::write(root.join(".git/config"), "local metadata")?;
        fs::write(root.join("README.md"), "selection")?;
        fs::write(
            root.join("dal.toml"),
            "[package]\nname=\"selection\"\nversion=\"0.1.0\"\ninclude=[\"src/**\",\"README.md\"]\nexclude=[\"src/private.fdn\"]\n",
        )?;
        let first = build_package_archive(&root)?;
        let second = build_package_archive(&root)?;
        assert_eq!(first.archive_bytes, second.archive_bytes);
        let mut files = Vec::new();
        visit_archive(&first.archive_bytes, |name, kind, _| {
            if kind.is_file() {
                files.push(name.to_string());
            }
            Ok(())
        })?;
        files.sort();
        assert_eq!(files, vec!["README.md", "dal.toml", "src/init.fdn"]);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn package_build_and_install_preserve_legal_attribution() -> Result<()> {
        for name in [
            "NOTICE",
            "NOTICE.txt",
            "NOTICE.md",
            "notice",
            "notice.txt",
            "license.txt",
        ] {
            let root = make_temp_dir("fidan_legal_archive");
            let project = root.join("project");
            fs::create_dir_all(project.join("src"))?;
            fs::write(
                project.join("src/init.fdn"),
                "action version returns integer { return 1 }\n",
            )?;
            fs::write(
                project.join("dal.toml"),
                format!(
                    "[package]\nname='legal-archive'\nversion='0.1.0'\ninclude=['src/**','{name}']\n"
                ),
            )?;
            fs::write(project.join(name), "Original legal attribution\n")?;
            let built = build_package_archive(&project)?;
            read_manifest_from_archive(&built.archive_bytes)?;
            let installed = install_downloaded_package(
                &built.archive_bytes,
                "legal-archive",
                "0.1.0",
                &root.join("store"),
                false,
            )?;
            assert_eq!(
                fs::read(installed.join(name))?,
                b"Original legal attribution\n"
            );
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn module_dir_name_normalizes_for_imports() {
        assert_eq!(module_dir_name("my-package"), "my_package");
        assert_eq!(module_dir_name("pkg123"), "pkg123");
        assert_eq!(module_dir_name("1pkg"), "_1pkg");
    }

    #[test]
    fn package_build_and_install_preserve_canonical_layout() -> Result<()> {
        let sandbox = make_temp_dir("fidan_dal_archive_test");
        let project_dir = sandbox.join("project");
        let install_dir = sandbox.join("installed");

        fs::create_dir_all(project_dir.join("src"))?;
        fs::write(
            project_dir.join("dal.toml"),
            r#"[package]
name = "my-package"
version = "0.1.0"
readme = "README.md"
"#,
        )?;
        fs::write(project_dir.join("README.md"), "# My Package\n")?;
        fs::write(project_dir.join("src").join("init.fdn"), "action main {}\n")?;

        let built = build_package_archive(&project_dir)?;
        assert_eq!(built.archive_name, "my-package-0.1.0.tar.gz");

        let installed_to = install_downloaded_package(
            &built.archive_bytes,
            "my-package",
            "0.1.0",
            &install_dir,
            false,
        )?;

        assert_eq!(installed_to, install_dir.join("my_package").join("0.1.0"));
        assert!(installed_to.join("init.fdn").is_file());
        assert!(installed_to.join("README.md").is_file());

        fs::remove_dir_all(&sandbox).ok();
        Ok(())
    }

    #[test]
    fn package_with_dependencies_requires_lockfile() {
        let sandbox = make_temp_dir("fidan_dal_archive_lock_test");
        let project_dir = sandbox.join("project");

        fs::create_dir_all(project_dir.join("src")).expect("create src");
        fs::write(
            project_dir.join("dal.toml"),
            r#"[package]
name = "my-package"
version = "0.1.0"
readme = "README.md"

[dependencies]
other-package = "^1.2"
"#,
        )
        .expect("write dal.toml");
        fs::write(project_dir.join("README.md"), "# My Package\n").expect("write readme");
        fs::write(project_dir.join("src").join("init.fdn"), "action main {}\n")
            .expect("write init");

        let error =
            build_package_archive(&project_dir).expect_err("missing `dal.lock` should fail");
        assert!(
            error
                .to_string()
                .contains("packages with dependencies must include a `dal.lock` file")
        );

        fs::remove_dir_all(&sandbox).ok();
    }

    #[test]
    fn package_with_optional_dependencies_requires_lockfile() {
        let sandbox = make_temp_dir("fidan_dal_archive_optional_lock_test");
        let project_dir = sandbox.join("project");

        fs::create_dir_all(project_dir.join("src")).expect("create src");
        fs::write(
            project_dir.join("dal.toml"),
            r#"[package]
name = "my-package"
version = "0.1.0"
readme = "README.md"

[optional-dependencies]
python-runtime = "^3"

[features]
pybindings = ["dep:python-runtime"]
"#,
        )
        .expect("write dal.toml");
        fs::write(project_dir.join("README.md"), "# My Package\n").expect("write readme");
        fs::write(project_dir.join("src").join("init.fdn"), "action main {}\n")
            .expect("write init");

        let error =
            build_package_archive(&project_dir).expect_err("missing `dal.lock` should fail");
        assert!(
            error
                .to_string()
                .contains("packages with dependencies must include a `dal.lock` file")
        );

        fs::remove_dir_all(&sandbox).ok();
    }

    #[test]
    fn package_build_respects_dalignore() -> Result<()> {
        let sandbox = make_temp_dir("fidan_dal_archive_ignore_test");
        let project_dir = sandbox.join("project");

        fs::create_dir_all(project_dir.join("src"))?;
        fs::create_dir_all(project_dir.join("tests"))?;
        fs::create_dir_all(project_dir.join("assets").join("tmp"))?;
        fs::write(
            project_dir.join("dal.toml"),
            r#"[package]
name = "my-package"
version = "0.1.0"
"#,
        )?;
        fs::write(project_dir.join("src").join("init.fdn"), "action main {}\n")?;
        fs::write(project_dir.join("tests").join("keep.txt"), "keep\n")?;
        fs::write(project_dir.join("tests").join("skip.snap"), "skip\n")?;
        fs::write(
            project_dir.join("assets").join("tmp").join("scratch.bin"),
            "tmp\n",
        )?;
        fs::write(
            project_dir.join(".dalignore"),
            "tests/**/*.snap\nassets/tmp/\n",
        )?;

        let built = build_package_archive(&project_dir)?;
        let decoder = flate2::read::GzDecoder::new(Cursor::new(built.archive_bytes));
        let mut archive = Archive::new(decoder);
        let mut paths = Vec::new();
        for entry in archive.entries()? {
            let entry = entry?;
            paths.push(entry.path()?.to_string_lossy().to_string());
        }

        assert!(paths.iter().any(|path| path.ends_with("tests/keep.txt")));
        assert!(!paths.iter().any(|path| path.ends_with("tests/skip.snap")));
        assert!(!paths.iter().any(|path| path.contains("assets/tmp")));

        fs::remove_dir_all(&sandbox).ok();
        Ok(())
    }

    #[test]
    fn dalignore_cannot_exclude_required_init() {
        let sandbox = make_temp_dir("fidan_dal_archive_ignore_required_test");
        let project_dir = sandbox.join("project");

        fs::create_dir_all(project_dir.join("src")).expect("create src");
        fs::write(
            project_dir.join("dal.toml"),
            r#"[package]
name = "my-package"
version = "0.1.0"
"#,
        )
        .expect("write manifest");
        fs::write(project_dir.join("src").join("init.fdn"), "action main {}\n")
            .expect("write init");
        fs::write(project_dir.join(".dalignore"), "src/init.fdn\n").expect("write .dalignore");

        let error = build_package_archive(&project_dir).expect_err("ignoring init.fdn should fail");
        assert!(
            error
                .to_string()
                .contains("must not exclude required path `src/init.fdn`")
        );

        fs::remove_dir_all(&sandbox).ok();
    }

    const TEST_MANIFEST: &[u8] = b"[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    fn crafted_archive(extra: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        for (path, kind, contents) in [
            ("demo-0.1.0/dal.toml", EntryType::Regular, TEST_MANIFEST),
            (
                "demo-0.1.0/src/init.fdn",
                EntryType::Regular,
                b"original".as_slice(),
            ),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(0o644);
            header.set_size(contents.len() as u64);
            // Raw names deliberately bypass Builder's own path sanitization.
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn archive_consumers_reject_unsafe_and_remapped_duplicate_entries() {
        for path in [
            "demo-0.1.0/../escape",
            "demo-0.1.0/src/../escape",
            "demo-0.1.0/src\\..\\escape",
            "demo-0.1.0/src/C:/escape",
            "demo-0.1.0/src/NUL.txt",
            "demo-0.1.0/src/name.",
            "demo-0.1.0/src/file:stream",
            "demo-0.1.0/src/init.fdn",
            "demo-0.1.0/src/dal.toml",
            "/absolute",
            "another-0.1.0/src/other.fdn",
        ] {
            let bytes = crafted_archive(&[(path, EntryType::Regular, b"attack")]);
            assert!(
                read_manifest_from_archive(&bytes).is_err(),
                "accepted {path}"
            );
        }
        for kind in [
            EntryType::Symlink,
            EntryType::Link,
            EntryType::Fifo,
            EntryType::Char,
        ] {
            let bytes = crafted_archive(&[("demo-0.1.0/src/link", kind, b"")]);
            assert!(
                read_manifest_from_archive(&bytes).is_err(),
                "accepted {kind:?}"
            );
        }
        let case_aliases = crafted_archive(&[
            ("demo-0.1.0/src/Util/a.fdn", EntryType::Regular, b"a"),
            ("demo-0.1.0/src/util/b.fdn", EntryType::Regular, b"b"),
        ]);
        assert!(read_manifest_from_archive(&case_aliases).is_err());
    }

    #[test]
    fn failed_forced_install_preserves_existing_package_and_cleans_staging() -> Result<()> {
        let sandbox = make_temp_dir("dal_atomic_install");
        let bytes = crafted_archive(&[]);
        let target = install_downloaded_package(&bytes, "demo", "0.1.0", &sandbox, false)?;
        let bad = crafted_archive(&[("demo-0.1.0/src/late", EntryType::Symlink, b"")]);
        assert!(install_downloaded_package(&bad, "demo", "0.1.0", &sandbox, true).is_err());
        assert_eq!(fs::read(target.join("init.fdn"))?, b"original");
        assert_eq!(fs::read_dir(target.parent().unwrap())?.count(), 1);
        fs::write(target.join("init.fdn"), b"modified cache")?;
        assert!(install_downloaded_package(&bytes, "demo", "0.1.0", &sandbox, false).is_err());
        install_downloaded_package(&bytes, "demo", "0.1.0", &sandbox, true)?;
        assert_eq!(fs::read(target.join("init.fdn"))?, b"original");
        assert_eq!(fs::read_dir(target.parent().unwrap())?.count(), 1);
        fs::remove_dir_all(sandbox)?;
        Ok(())
    }

    #[test]
    fn archive_validation_checks_crc_expansion_and_package_identity() {
        let mut bytes = crafted_archive(&[]);
        let crc = bytes.len() - 8;
        bytes[crc] ^= 1;
        assert!(read_manifest_from_archive(&bytes).is_err());
        let bomb = vec![0_u8; 1024 * 1024];
        assert!(
            read_manifest_from_archive(&crafted_archive(&[(
                "demo-0.1.0/assets/bomb",
                EntryType::Regular,
                &bomb
            )]))
            .is_err()
        );
        let sandbox = make_temp_dir("dal_identity_install");
        assert!(
            install_downloaded_package(&crafted_archive(&[]), "other", "0.1.0", &sandbox, false)
                .is_err()
        );
        assert!(!package_install_dir(&sandbox, "other", "0.1.0").exists());
        assert!(
            install_downloaded_package(
                &crafted_archive(&[]),
                "../escape",
                "0.1.0",
                &sandbox,
                false
            )
            .is_err()
        );
        fs::remove_dir_all(sandbox).unwrap();
    }

    fn make_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
        fs::create_dir_all(&dir).expect("failed to create temp test dir");
        #[cfg(unix)]
        let dir = std::fs::canonicalize(dir).expect("canonical temporary fixture");
        dir
    }
}
