use anyhow::{Context, Result, bail};
use fidan_driver::dal::{
    DalManifest, package_install_dir, read_manifest, remap_source_relative_path,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
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
}

pub fn build_package_archive(project_dir: &Path) -> Result<BuiltPackage> {
    let project_dir = project_dir
        .canonicalize()
        .with_context(|| format!("cannot access {}", project_dir.display()))?;
    let manifest = read_manifest(&project_dir)?;
    let ignore = load_dalignore(&project_dir)?;
    validate_package_dir(&project_dir, &manifest, ignore.as_ref())?;

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

    Ok(BuiltPackage {
        manifest,
        archive_name,
        archive_bytes,
    })
}

pub fn install_downloaded_package(
    archive_bytes: &[u8],
    package: &str,
    version: &str,
    into_dir: &Path,
    force: bool,
) -> Result<PathBuf> {
    let target_dir = package_install_dir(into_dir, package, version);
    if target_dir.exists() {
        if !force {
            return Ok(target_dir);
        }
        fs::remove_dir_all(&target_dir)
            .with_context(|| format!("cannot remove {}", target_dir.display()))?;
    }
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("cannot create {}", target_dir.display()))?;

    let root_name = format!("{package}-{version}");
    let decoder = flate2::read::GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = Archive::new(decoder);
    let mut saw_init = false;

    for entry in archive.entries().context("cannot read archive entries")? {
        let mut entry = entry.context("invalid archive entry")?;
        let path = entry
            .path()
            .context("cannot read archive path")?
            .to_path_buf();
        let mut components = path.components();
        let root = components.next();
        let Some(Component::Normal(root_component)) = root else {
            bail!("archive entry has invalid root path");
        };
        if root_component.to_string_lossy() != root_name {
            bail!("archive root directory does not match expected package root");
        }

        let remainder: PathBuf = components.collect();
        if remainder.as_os_str().is_empty() {
            continue;
        }

        if !is_safe_relative_path(&remainder) {
            bail!("archive contains unsafe path `{}`", remainder.display());
        }

        let out_path = remap_install_path(&target_dir, &remainder);
        match entry.header().entry_type() {
            EntryType::Directory => {
                fs::create_dir_all(&out_path)
                    .with_context(|| format!("cannot create {}", out_path.display()))?;
            }
            EntryType::Regular => {
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("cannot create {}", parent.display()))?;
                }
                entry
                    .unpack(&out_path)
                    .with_context(|| format!("cannot write {}", out_path.display()))?;
                if out_path == target_dir.join("init.fdn") {
                    saw_init = true;
                }
            }
            _ => bail!("archive contains unsupported entry type"),
        }
    }

    if !saw_init {
        bail!("downloaded package did not contain src/init.fdn");
    }

    Ok(target_dir)
}

pub fn read_manifest_from_archive(archive_bytes: &[u8]) -> Result<DalManifest> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = Archive::new(decoder);
    for entry in archive.entries().context("cannot read archive entries")? {
        let mut entry = entry.context("invalid archive entry")?;
        let path = entry
            .path()
            .context("cannot read archive path")?
            .to_path_buf();
        let file_name = path.file_name().and_then(|name| name.to_str());
        if file_name != Some("dal.toml") {
            continue;
        }
        let mut manifest_bytes = Vec::new();
        use std::io::Read;
        entry
            .read_to_end(&mut manifest_bytes)
            .context("cannot read `dal.toml` from archive")?;
        let manifest: DalManifest =
            toml::from_slice(&manifest_bytes).context("invalid `dal.toml` inside archive")?;
        fidan_driver::dal::validate_manifest(&manifest)?;
        return Ok(manifest);
    }
    bail!("downloaded archive did not contain `dal.toml`");
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
        if !allowed.contains(&file_name) && !is_license_file(&file_name) {
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
        if is_license_file(&file_name) && !is_ignored_path(ignore, project_dir, &entry.path()) {
            validate_file(&entry.path(), project_dir)?;
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

    for entry in fs::read_dir(project_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if is_ignored_path(ignore, project_dir, &path) {
            continue;
        }
        if name == "dal.toml" {
            continue;
        }
        if ALLOWED_TOP_FILES.contains(&name.as_str()) || is_license_file(&name) {
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
    for entry in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
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
            builder.append_dir(&archive_path, &path)?;
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
    builder
        .append_path_with_name(source, archive_path)
        .with_context(|| format!("cannot archive {}", source.display()))?;
    Ok(())
}

fn validate_tree(path: &Path, project_dir: &Path, ignore: Option<&DalIgnore>) -> Result<()> {
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
            validate_tree(&entry.path(), project_dir, ignore)?;
        } else if meta.is_file() {
            validate_file(&entry.path(), project_dir)?;
        }
    }
    let relative = path.strip_prefix(project_dir).unwrap_or(path);
    if !is_safe_relative_path(relative) {
        bail!("package contains unsafe path `{}`", relative.display());
    }
    Ok(())
}

fn validate_file(path: &Path, project_dir: &Path) -> Result<()> {
    let relative = path.strip_prefix(project_dir).unwrap_or(path);
    if !is_safe_relative_path(relative) {
        bail!("package contains unsafe path `{}`", relative.display());
    }
    Ok(())
}

fn is_license_file(name: &str) -> bool {
    name == "LICENSE" || name.starts_with("LICENSE.")
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

fn load_dalignore(project_dir: &Path) -> Result<Option<DalIgnore>> {
    let ignore_path = project_dir.join(".dalignore");
    if !ignore_path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&ignore_path)
        .with_context(|| format!("cannot read {}", ignore_path.display()))?;
    let mut builder = GlobSetBuilder::new();
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
    Ok(Some(DalIgnore { set }))
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
    !rel.is_empty() && ignore.set.is_match(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_driver::dal::module_dir_name;
    use std::time::{SystemTime, UNIX_EPOCH};

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
