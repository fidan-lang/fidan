use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

pub const DEFAULT_DISTRIBUTION_MANIFEST: &str = "https://releases.fidan.dev/manifest.json";
const MANIFEST_ENV: &str = "FIDAN_DIST_MANIFEST";

#[derive(Debug, Clone, Deserialize)]
pub struct DistributionManifest {
    pub schema_version: u32,
    #[serde(default, alias = "fidan")]
    pub fidan_versions: Vec<FidanRelease>,
    #[serde(default)]
    pub toolchains: Vec<ToolchainRelease>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FidanRelease {
    pub version: String,
    pub host_triple: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub binary_relpath: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolchainRelease {
    pub kind: String,
    pub toolchain_version: String,
    pub tool_version: String,
    pub host_triple: String,
    pub url: String,
    pub sha256: String,
    pub helper_relpath: String,
    #[serde(default)]
    pub exec_commands: Vec<fidan_driver::ToolchainExecCommand>,
    pub supported_fidan_versions: String,
    pub backend_protocol_version: u32,
}

pub fn resolve_manifest_url(explicit: Option<&str>) -> String {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var(MANIFEST_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_DISTRIBUTION_MANIFEST.to_string())
}

pub fn fetch_manifest(explicit: Option<&str>) -> Result<DistributionManifest> {
    let url = resolve_manifest_url(explicit);
    let body = fetch_text(&url)?;
    let manifest: DistributionManifest = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse distribution manifest `{url}`"))?;
    if manifest.schema_version == 0 {
        bail!("distribution manifest `{url}` has invalid schema_version 0");
    }
    Ok(manifest)
}

pub fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = resolve_local_file_url(url)? {
        return fs::read(&path).with_context(|| {
            format!("failed to read local file URL `{url}` ({})", path.display())
        });
    }

    let response = reqwest::blocking::get(url)
        .with_context(|| format!("failed to fetch `{url}`"))?
        .error_for_status()
        .with_context(|| format!("download failed for `{url}`"))?;
    read_response_bytes(response, url, "download")
}

pub fn fetch_text(url: &str) -> Result<String> {
    if let Some(path) = resolve_local_file_url(url)? {
        return fs::read_to_string(&path).with_context(|| {
            format!("failed to read local file URL `{url}` ({})", path.display())
        });
    }

    let response = reqwest::blocking::get(url)
        .with_context(|| format!("failed to fetch `{url}`"))?
        .error_for_status()
        .with_context(|| format!("download failed for `{url}`"))?;
    let bytes = read_response_bytes(response, url, "fetch")?;
    String::from_utf8(bytes).with_context(|| format!("failed to decode text from `{url}`"))
}

fn read_response_bytes(
    mut response: reqwest::blocking::Response,
    url: &str,
    prefix: &str,
) -> Result<Vec<u8>> {
    let label = download_label(url);
    let total = response.content_length();
    let progress = fidan_driver::progress::ProgressReporter::bytes(
        prefix,
        format!("retrieving {label}"),
        total,
    );
    let mut out = Vec::with_capacity(total.unwrap_or(0).min(8 * 1024 * 1024) as usize);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut buf)
            .with_context(|| format!("failed to read download body from `{url}`"))?;
        if read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..read]);
        progress.inc(read as u64);
    }
    progress.finish_and_clear();
    Ok(out)
}

pub fn fetch_cached_bytes(url: &str, cache_path: &Path, expected_sha256: &str) -> Result<Vec<u8>> {
    if let Ok(bytes) = fs::read(cache_path)
        && verify_sha256(&bytes, expected_sha256).is_ok()
    {
        return Ok(bytes);
    }

    let bytes = fetch_bytes(url)?;
    verify_sha256(&bytes, expected_sha256)?;
    write_bytes(cache_path, &bytes)?;
    Ok(bytes)
}

fn download_label(url: &str) -> String {
    url.rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| url.to_string())
}

fn resolve_local_file_url(url: &str) -> Result<Option<PathBuf>> {
    let Some(rest) = url.strip_prefix("file://") else {
        return Ok(None);
    };

    let path_part = if looks_like_windows_drive_path(rest) || rest.starts_with('/') {
        rest.to_string()
    } else if let Some(stripped) = rest.strip_prefix("localhost/") {
        format!("/{stripped}")
    } else {
        bail!("unsupported file URL `{url}`");
    };

    let decoded = urlencoding::decode(&path_part)
        .with_context(|| format!("failed to decode local file URL `{url}`"))?;

    #[cfg(windows)]
    {
        let mut normalized = decoded.replace('/', "\\");
        if normalized.starts_with('\\') && looks_like_windows_drive_path(&normalized[1..]) {
            normalized.remove(0);
        }
        Ok(Some(PathBuf::from(normalized)))
    }

    #[cfg(not(windows))]
    {
        Ok(Some(PathBuf::from(decoded.as_ref())))
    }
}

fn looks_like_windows_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = sha256_hex_bytes(hasher.finalize().as_slice());
    if actual != expected.trim().to_lowercase() {
        bail!("SHA-256 mismatch: expected {}, got {}", expected, actual);
    }
    Ok(())
}

fn sha256_hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn extract_tar_gz(bytes: &[u8], destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create `{}`", destination.display()))?;
    let gz = GzDecoder::new(bytes);
    let mut archive = Archive::new(gz);
    archive
        .unpack(destination)
        .with_context(|| format!("failed to unpack archive into `{}`", destination.display()))
}

pub fn materialize_release_root(
    staging_dir: &Path,
    expected_relpath: &Path,
    final_dir: &Path,
) -> Result<()> {
    let candidate = if staging_dir.join(expected_relpath).exists() {
        staging_dir.to_path_buf()
    } else {
        let mut entries = fs::read_dir(staging_dir)
            .with_context(|| format!("failed to inspect `{}`", staging_dir.display()))?
            .filter_map(|entry| entry.ok())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        if entries.len() != 1 || !entries[0].file_type()?.is_dir() {
            bail!(
                "downloaded archive does not contain `{}` at the root or inside a single top-level directory",
                expected_relpath.display()
            );
        }
        let nested = entries.remove(0).path();
        if !nested.join(expected_relpath).exists() {
            bail!(
                "downloaded archive does not contain the expected file `{}`",
                expected_relpath.display()
            );
        }
        nested
    };

    if final_dir.exists() {
        bail!("destination `{}` already exists", final_dir.display());
    }
    fs::rename(&candidate, final_dir).with_context(|| {
        format!(
            "failed to move extracted release from `{}` to `{}`",
            candidate.display(),
            final_dir.display()
        )
    })?;

    if staging_dir.exists() {
        let _ = fs::remove_dir_all(staging_dir);
    }
    Ok(())
}

pub fn binary_relpath() -> &'static str {
    if cfg!(windows) { "fidan.exe" } else { "fidan" }
}

pub fn select_fidan_release<'a>(
    manifest: &'a DistributionManifest,
    version: Option<&str>,
    host_triple: &str,
) -> Result<&'a FidanRelease> {
    let mut candidates = manifest
        .fidan_versions
        .iter()
        .filter(|release| release.host_triple == host_triple)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!("no Fidan releases are available for host `{host_triple}` in the manifest");
    }

    candidates.sort_by(|left, right| {
        match (
            Version::parse(&left.version),
            Version::parse(&right.version),
        ) {
            (Ok(left), Ok(right)) => right.cmp(&left),
            _ => right.version.cmp(&left.version),
        }
    });

    if let Some(version) = version
        && version != "latest"
    {
        return candidates
            .into_iter()
            .find(|release| release.version == version)
            .with_context(|| {
                format!("Fidan version `{version}` is not available for `{host_triple}`")
            });
    }

    Ok(candidates[0])
}

pub fn select_toolchain_release<'a>(
    manifest: &'a DistributionManifest,
    kind: &str,
    version: Option<&str>,
    host_triple: &str,
) -> Result<&'a ToolchainRelease> {
    let current_fidan_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("failed to parse current Fidan version for toolchain selection")?;
    let mut candidates = manifest
        .toolchains
        .iter()
        .filter(|release| release.kind == kind && release.host_triple == host_triple)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!("no `{kind}` toolchain packages are available for host `{host_triple}`");
    }

    candidates.sort_by(|left, right| {
        match (
            Version::parse(&left.toolchain_version),
            Version::parse(&right.toolchain_version),
        ) {
            (Ok(left), Ok(right)) => right.cmp(&left),
            _ => right.toolchain_version.cmp(&left.toolchain_version),
        }
    });

    if let Some(version) = version
        && version != "latest"
    {
        let release = candidates
            .into_iter()
            .find(|release| release.toolchain_version == version)
            .with_context(|| {
                format!(
                    "toolchain `{kind}` version `{version}` is not available for `{host_triple}`"
                )
            })?;
        let requirement = VersionReq::parse(&release.supported_fidan_versions).with_context(|| {
            format!(
                "toolchain `{kind}` version `{version}` has invalid supported_fidan_versions `{}`",
                release.supported_fidan_versions
            )
        })?;
        if !requirement.matches(&current_fidan_version) {
            bail!(
                "toolchain `{kind}` version `{version}` is not compatible with Fidan {} (requires `{}`)",
                current_fidan_version,
                release.supported_fidan_versions
            );
        }
        return Ok(release);
    }

    candidates
        .into_iter()
        .find(|release| {
            VersionReq::parse(&release.supported_fidan_versions)
                .map(|req| req.matches(&current_fidan_version))
                .unwrap_or(false)
        })
        .with_context(|| {
            format!(
                "no compatible `{kind}` toolchain packages are available for host `{host_triple}` and Fidan {}",
                current_fidan_version
            )
        })
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("failed to write `{}`", path.display()))
}

pub fn stage_dir(base: &Path, prefix: &str) -> PathBuf {
    base.join(format!(
        "{}.tmp-{}-{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fidan-cli-distribution-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        sha256_hex_bytes(hasher.finalize().as_slice())
    }

    #[test]
    fn fetch_cached_bytes_reuses_matching_cache() {
        let dir = test_temp_dir("reuse");
        let source = dir.join("source.tar.gz");
        let cache = dir.join("cache.tar.gz");
        let expected = b"cached-archive";
        fs::write(&source, expected).expect("failed to write source archive");
        let url = format!("file://{}", source.display());
        let sha = sha256_hex(expected);

        let first = fetch_cached_bytes(&url, &cache, &sha).expect("first fetch should succeed");
        assert_eq!(first, expected);
        fs::remove_file(&source).expect("failed to remove source archive");

        let second = fetch_cached_bytes(&url, &cache, &sha).expect("cached fetch should succeed");
        assert_eq!(second, expected);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_cached_bytes_refreshes_stale_cache() {
        let dir = test_temp_dir("refresh");
        let source = dir.join("source.tar.gz");
        let cache = dir.join("cache.tar.gz");
        let expected = b"fresh-archive";
        fs::write(&source, expected).expect("failed to write source archive");
        fs::write(&cache, b"stale-archive").expect("failed to write stale cache");
        let url = format!("file://{}", source.display());
        let sha = sha256_hex(expected);

        let bytes = fetch_cached_bytes(&url, &cache, &sha).expect("fetch should refresh cache");
        assert_eq!(bytes, expected);
        assert_eq!(fs::read(&cache).expect("failed to read cache"), expected);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_cached_bytes_does_not_reuse_stale_cache_without_source() {
        let dir = test_temp_dir("reject-stale");
        let source = dir.join("missing.tar.gz");
        let cache = dir.join("cache.tar.gz");
        fs::write(&cache, b"stale-archive").expect("failed to write stale cache");
        let url = format!("file://{}", source.display());
        let sha = sha256_hex(b"expected-archive");

        let err = fetch_cached_bytes(&url, &cache, &sha)
            .expect_err("stale cache should not be reused when the source is unavailable");
        let err_text = err.to_string();
        assert!(
            err_text.contains("failed to read"),
            "expected source read failure after stale-cache rejection, got {err_text}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn test_manifest_with_toolchains(toolchains: Vec<ToolchainRelease>) -> DistributionManifest {
        DistributionManifest {
            schema_version: 1,
            fidan_versions: vec![],
            toolchains,
        }
    }

    fn test_toolchain_release(
        toolchain_version: &str,
        supported_fidan_versions: &str,
    ) -> ToolchainRelease {
        ToolchainRelease {
            kind: "ai-analysis".to_string(),
            toolchain_version: toolchain_version.to_string(),
            tool_version: "1.0.0".to_string(),
            host_triple: "x86_64-pc-windows-msvc".to_string(),
            url: format!("https://example.invalid/{toolchain_version}.tar.gz"),
            sha256: "abc123".to_string(),
            helper_relpath: "fidan-ai-analysis-helper.exe".to_string(),
            exec_commands: vec![],
            supported_fidan_versions: supported_fidan_versions.to_string(),
            backend_protocol_version: 1,
        }
    }

    #[test]
    fn select_toolchain_release_prefers_latest_compatible_version() {
        let manifest = test_manifest_with_toolchains(vec![
            test_toolchain_release("9.9.9", "^999.0.0"),
            test_toolchain_release("1.2.0", "^1.0.0"),
            test_toolchain_release("1.1.0", "^1.0.0"),
        ]);

        let selected =
            select_toolchain_release(&manifest, "ai-analysis", None, "x86_64-pc-windows-msvc")
                .expect("latest compatible toolchain should be selected");

        assert_eq!(selected.toolchain_version, "1.2.0");
    }

    #[test]
    fn select_toolchain_release_rejects_explicit_incompatible_version() {
        let manifest =
            test_manifest_with_toolchains(vec![test_toolchain_release("9.9.9", "^999.0.0")]);

        let err = select_toolchain_release(
            &manifest,
            "ai-analysis",
            Some("9.9.9"),
            "x86_64-pc-windows-msvc",
        )
        .expect_err("explicit incompatible version should be rejected");

        assert!(err.to_string().contains("is not compatible with Fidan"));
    }

    #[test]
    fn select_toolchain_release_errors_when_only_incompatible_versions_exist() {
        let manifest = test_manifest_with_toolchains(vec![
            test_toolchain_release("9.9.9", "^999.0.0"),
            test_toolchain_release("8.0.0", "^998.0.0"),
        ]);

        let err =
            select_toolchain_release(&manifest, "ai-analysis", None, "x86_64-pc-windows-msvc")
                .expect_err("latest should fail when no compatible toolchain exists");

        assert!(
            err.to_string()
                .contains("no compatible `ai-analysis` toolchain packages")
        );
    }

    #[cfg(windows)]
    #[test]
    fn fetch_text_accepts_standard_windows_file_urls() {
        let dir = test_temp_dir("std-file-url-text");
        let source = dir.join("manifest.json");
        fs::write(&source, "{\"schema_version\":1}").expect("failed to write manifest");
        let url = format!(
            "file:///{}",
            source.display().to_string().replace('\\', "/")
        );

        let text = fetch_text(&url).expect("standard Windows file URL should be readable");

        assert_eq!(text, "{\"schema_version\":1}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn fetch_cached_bytes_accepts_standard_windows_file_urls() {
        let dir = test_temp_dir("std-file-url-bytes");
        let source = dir.join("archive.tar.gz");
        let cache = dir.join("cache.tar.gz");
        let expected = b"archive-bytes";
        fs::write(&source, expected).expect("failed to write archive");
        let url = format!(
            "file:///{}",
            source.display().to_string().replace('\\', "/")
        );
        let sha = sha256_hex(expected);

        let bytes = fetch_cached_bytes(&url, &cache, &sha)
            .expect("standard Windows file URL should be readable");

        assert_eq!(bytes, expected);
        assert_eq!(fs::read(&cache).expect("failed to read cache"), expected);
        let _ = fs::remove_dir_all(&dir);
    }
}
