use anyhow::{Context, Result, bail};
use reqwest::Method;
use reqwest::blocking::{Client, multipart};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::fmt::Display;
use std::io::Read;
use std::time::Duration;

pub const MAX_ARCHIVE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DalClient {
    base: String,
    token: Option<String>,
    http: Client,
    authenticated_http: Client,
}

impl DalClient {
    pub fn new(base: String, token: Option<String>) -> Result<Self> {
        if let Some(token) = &token {
            fidan_diagnostics::logging::register_secret(token);
        }
        let base = super::config::canonical_registry(&base)?;
        fidan_diagnostics::tracing::debug!(registry = %base, authenticated = token.is_some(), "registry client initialized");
        let url = reqwest::Url::parse(&base)?;
        let http = Client::builder()
            .https_only(url.scheme() == "https")
            .user_agent(concat!("fidan/", env!("CARGO_PKG_VERSION"), " dal-cli"))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build Dal HTTP client")?;

        // Credentials must not follow even same-origin redirects outside the registry prefix.
        // Public downloads retain redirect support for presigned object-store URLs.
        let authenticated_http = Client::builder()
            .https_only(url.scheme() == "https")
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("fidan/", env!("CARGO_PKG_VERSION"), " dal-cli"))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build authenticated Dal HTTP client")?;
        Ok(Self {
            base,
            token,
            http,
            authenticated_http,
        })
    }

    pub fn whoami(&self) -> Result<UserInfo> {
        self.auth_json(Method::GET, "/auth/me", None::<&Value>)
    }

    pub fn search(&self, query: &str, page: u32, per_page: u32) -> Result<Page<PackageSummary>> {
        let url = format!(
            "{}/search?q={}&page={page}&per_page={per_page}",
            self.base,
            urlencoding::encode(query)
        );
        self.json_url(Method::GET, &url, None::<&Value>)
    }

    pub fn package_info(&self, name: &str) -> Result<PackageInfo> {
        self.json(Method::GET, &format!("/packages/{name}"), None::<&Value>)
    }

    pub fn versions(&self, name: &str) -> Result<Vec<PackageVersion>> {
        self.json(
            Method::GET,
            &format!("/packages/{name}/versions"),
            None::<&Value>,
        )
    }

    pub fn index(&self, name: &str) -> Result<Vec<IndexEntry>> {
        fidan_diagnostics::tracing::debug!(package = name, "fetching sparse index");
        let url = format!("{}/index/{name}", self.base);
        let response = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("failed to fetch sparse index for `{name}`"))?;
        // A missing dependency is a resolver conflict; other failures stay fatal.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        let response = ensure_success_bytes(response, "fetch sparse index")?;
        const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;
        let mut bytes = Vec::new();
        response
            .take(MAX_INDEX_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("failed to read sparse index body")?;
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            bail!("sparse index exceeds 8 MiB limit");
        }
        let body = String::from_utf8(bytes).context("sparse index is not UTF-8")?;
        let mut versions = std::collections::BTreeSet::new();
        let mut entries = Vec::new();
        for (line_no, line) in body.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: IndexEntry = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid NDJSON line {} in sparse index", line_no + 1))?;
            if entry.name != name {
                bail!("sparse index package identity does not match `{name}`");
            }
            validate_checksum(&entry.cksum)?;
            fidan_driver::dal::compiler_compatible(
                entry.fidan.as_deref(),
                env!("CARGO_PKG_VERSION"),
            )?;
            semver::Version::parse(&entry.vers).context("invalid version in sparse index")?;
            if !versions.insert(entry.vers.clone()) || versions.len() > 10_000 {
                bail!("sparse index contains duplicate versions or exceeds the version limit");
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    pub fn download_archive(&self, name: &str, version: &str, checksum: &str) -> Result<Vec<u8>> {
        fidan_diagnostics::tracing::info!(package = name, version, "downloading package archive");
        validate_checksum(checksum)?;
        let url = format!("{}/packages/{name}/versions/{version}/download", self.base);
        let response = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("failed to download `{name}@{version}`"))?;
        let response = ensure_success_bytes(response, "download package archive")?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_ARCHIVE_BYTES)
        {
            bail!("downloaded archive exceeds the 50 MiB limit");
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_ARCHIVE_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("failed to read downloaded archive")?;
        verify_archive_checksum(&bytes, checksum)?;
        fidan_diagnostics::tracing::debug!(bytes = bytes.len(), "package checksum verified");
        Ok(bytes)
    }

    pub fn publish(
        &self,
        package: &str,
        archive_name: &str,
        archive_bytes: Vec<u8>,
    ) -> Result<PublishResponse> {
        let token = self.require_token()?;
        let url = format!("{}/packages/{package}/publish", self.base);
        let form = multipart::Form::new().part(
            "archive",
            multipart::Part::bytes(archive_bytes)
                .file_name(archive_name.to_string())
                .mime_str("application/gzip")?,
        );

        let response = self
            .authenticated_http
            .post(&url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .with_context(|| format!("failed to publish `{package}`"))?;
        let response = ensure_success_bytes(response, "publish package")?;
        read_json(response)
    }

    pub fn yank(&self, package: &str, version: &str, reason: Option<&str>) -> Result<MessageOnly> {
        self.auth_json(
            Method::PUT,
            &format!("/packages/{package}/versions/{version}/yank"),
            Some(&json!({ "reason": reason })),
        )
    }

    pub fn unyank(&self, package: &str, version: &str) -> Result<MessageOnly> {
        self.auth_json(
            Method::PUT,
            &format!("/packages/{package}/versions/{version}/unyank"),
            Some(&json!({})),
        )
    }

    fn auth_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        let token = self.require_token()?;
        let url = format!("{}{}", self.base, path);
        let mut request = self
            .authenticated_http
            .request(method, &url)
            .bearer_auth(token);
        if let Some(json_body) = body {
            request = request.json(json_body);
        }
        let response = request
            .send()
            .with_context(|| format!("request failed: {url}"))?;
        ensure_success(response, path)
    }

    fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        self.json_url(method, &url, body)
    }

    fn json_url<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        fidan_diagnostics::tracing::trace!(method = %method, "registry request started");
        let mut request = self.http.request(method, url);
        if let Some(json_body) = body {
            request = request.json(json_body);
        }
        let response = request
            .send()
            .with_context(|| format!("request failed: {url}"))?;
        ensure_success(response, url)
    }

    fn require_token(&self) -> Result<&str> {
        self.token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("not logged in — run `fidan dal login` first"))
    }
}

fn ensure_success<T: DeserializeOwned>(
    response: reqwest::blocking::Response,
    action: impl Display,
) -> Result<T> {
    let response = ensure_success_bytes(response, action)?;
    read_json(response)
}

fn read_json<T: DeserializeOwned>(response: reqwest::blocking::Response) -> Result<T> {
    const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    response
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read registry JSON")?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        bail!("registry JSON exceeds the 8 MiB limit");
    }
    serde_json::from_slice(&bytes).context("invalid registry JSON response")
}

fn ensure_success_bytes(
    response: reqwest::blocking::Response,
    action: impl Display,
) -> Result<reqwest::blocking::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    fidan_diagnostics::tracing::debug!(status = status.as_u16(), "registry request rejected");
    let mut body = String::new();
    response
        .take(16 * 1024)
        .read_to_string(&mut body)
        .context("failed to read registry error response")?;
    // Terminal control characters from an untrusted registry must not execute.
    body.retain(|character| !character.is_control() || character == '\n' || character == '\t');
    bail!("{action} failed: HTTP {status} - {body}");
}

#[derive(Debug, Deserialize)]
pub struct UserInfo {
    pub username: String,
    pub email: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PackageSummary {
    pub name: String,
    pub description: Option<String>,
    pub latest_version: Option<String>,
    pub downloads: i64,
}

#[derive(Debug, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub downloads: i64,
}

#[derive(Debug, Deserialize)]
pub struct PackageVersion {
    pub version: String,
    pub yanked: bool,
    pub yank_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page: u32,
    pub total: i64,
    pub pages: u32,
}

#[derive(Debug, Deserialize)]
pub struct PublishResponse {
    pub message: String,
    pub package: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct MessageOnly {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    pub cksum: String,
    #[serde(default)]
    pub fidan: Option<String>,
    pub vers: String,
    pub yanked: bool,
}

fn validate_checksum(checksum: &str) -> Result<()> {
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("registry archive checksum must be exactly 64 hexadecimal SHA-256 characters");
    }
    Ok(())
}

pub fn verify_archive_checksum(bytes: &[u8], checksum: &str) -> Result<()> {
    validate_checksum(checksum)?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        bail!("downloaded archive exceeds the 50 MiB limit");
    }
    if crate::distribution::verify_sha256(bytes, checksum).is_err() {
        bail!("downloaded archive SHA-256 checksum does not match the registry index");
    }
    Ok(())
}

#[cfg(test)]
mod integrity_tests {
    use super::*;

    #[test]
    fn archive_checksum_is_strict_and_detects_tampering() {
        let bytes = b"abc";
        let checksum =
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string();
        verify_archive_checksum(bytes, &checksum).unwrap();
        verify_archive_checksum(bytes, &checksum.to_uppercase()).unwrap();
        assert!(verify_archive_checksum(b"modified artifact", &checksum).is_err());
        for invalid in [
            "".to_string(),
            "0".repeat(63),
            "g".repeat(64),
            "0".repeat(65),
        ] {
            assert!(verify_archive_checksum(bytes, &invalid).is_err());
        }
    }

    #[test]
    fn index_requires_identity_and_checksum() {
        assert!(serde_json::from_str::<IndexEntry>(r#"{"vers":"1.0.0","yanked":false}"#).is_err());
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    #[test]
    fn registry_urls_cannot_send_credentials_over_remote_plaintext_http() {
        for url in [
            "http://example.test",
            "https://user:password@example.test",
            "file:///tmp/index",
            "https://example.test?key=value",
        ] {
            assert!(DalClient::new(url.into(), None).is_err());
        }
        assert!(DalClient::new("https://dal.example.test".into(), None).is_ok());
        assert!(DalClient::new("http://127.0.0.1:8080".into(), None).is_ok());
    }
}
