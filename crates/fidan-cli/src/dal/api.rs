use anyhow::{Context, Result, bail};
use reqwest::Method;
use reqwest::blocking::{Client, multipart};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::fmt::Display;

#[derive(Debug, Clone)]
pub struct DalClient {
    base: String,
    token: Option<String>,
    http: Client,
}

impl DalClient {
    pub fn new(base: String, token: Option<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("fidan/", env!("CARGO_PKG_VERSION"), " dal-cli"))
            .build()
            .context("failed to build Dal HTTP client")?;

        Ok(Self { base, token, http })
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
        let url = format!("{}/index/{name}", self.base);
        let response = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("failed to fetch sparse index for `{name}`"))?;
        let response = ensure_success_bytes(response, "fetch sparse index")?;
        let body = response
            .text()
            .context("failed to read sparse index body")?;
        let mut entries = Vec::new();
        for (line_no, line) in body.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: IndexEntry = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid NDJSON line {} in sparse index", line_no + 1))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    pub fn download_archive(&self, name: &str, version: &str) -> Result<Vec<u8>> {
        let url = format!("{}/packages/{name}/versions/{version}/download", self.base);
        let response = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("failed to download `{name}@{version}`"))?;
        let response = ensure_success_bytes(response, "download package archive")?;
        let bytes = response
            .bytes()
            .context("failed to read downloaded archive")?;
        Ok(bytes.to_vec())
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
            .http
            .post(&url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .with_context(|| format!("failed to publish `{package}`"))?;
        let response = ensure_success_bytes(response, "publish package")?;
        response.json().context("invalid publish response")
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
        let mut request = self.http.request(method, &url).bearer_auth(token);
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
    response.json().context("invalid JSON response")
}

fn ensure_success_bytes(
    response: reqwest::blocking::Response,
    action: impl Display,
) -> Result<reqwest::blocking::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let body = response
        .text()
        .unwrap_or_else(|_| "<unreadable response body>".to_string());
    bail!("{action} failed: HTTP {status} — {body}");
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
    pub vers: String,
    pub deps: Vec<Value>,
    pub yanked: bool,
}
