use anyhow::{Context, Result, bail};
use fidan_secrets::{SecretSpec, clear_secret, resolve_secret, store_secret};
use serde::Deserialize;
use std::path::PathBuf;

const CONFIG_ENV: &str = "FIDAN_AI_ANALYSIS_CONFIG";
const KEYCHAIN_SERVICE: &str = "fidan";
const DEFAULT_KEYCHAIN_ACCOUNT: &str = "ai_analysis_api_key";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub keyring_account: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub replace_system_prompt: bool,
}

fn default_timeout_secs() -> u64 {
    60
}

pub fn resolve_config_path() -> Result<PathBuf> {
    Ok(
        if let Ok(path) = std::env::var(CONFIG_ENV)
            && !path.trim().is_empty()
        {
            PathBuf::from(path)
        } else {
            fidan_driver::resolve_fidan_home()?.join("ai-analysis.toml")
        },
    )
}

pub fn load() -> Result<Config> {
    let path = resolve_config_path()?;
    load_from_path(&path)
}

pub fn load_if_present() -> Result<Option<Config>> {
    let path = resolve_config_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    load_from_path(&path).map(Some)
}

pub fn load_from_path(path: &PathBuf) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("failed to parse `{}`", path.display()))?;
    if config.schema_version != 1 {
        bail!(
            "unsupported ai-analysis config schema_version {} (expected 1)",
            config.schema_version
        );
    }
    if config.provider.trim().is_empty() || config.model.trim().is_empty() {
        bail!("`provider` and `model` must be configured in ai-analysis.toml");
    }
    Ok(config)
}

pub fn resolve_api_key(config: &Config) -> Result<Option<String>> {
    resolve_api_key_with(
        config.api_key_env.as_deref(),
        config.keyring_account.as_deref(),
    )
}

pub fn resolve_api_key_with(
    api_key_env: Option<&str>,
    keyring_account: Option<&str>,
) -> Result<Option<String>> {
    resolve_secret(&secret_spec(api_key_env, keyring_account), None)
}

pub fn store_api_key(api_key: &str, keyring_account: Option<&str>) -> Result<()> {
    store_secret(&secret_spec(None, keyring_account), api_key)
}

pub fn clear_api_key(keyring_account: Option<&str>) -> Result<()> {
    clear_secret(&secret_spec(None, keyring_account))
}

pub fn resolved_keyring_account(config: &Config) -> &str {
    config
        .keyring_account
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_KEYCHAIN_ACCOUNT)
}

pub fn default_keyring_account() -> &'static str {
    DEFAULT_KEYCHAIN_ACCOUNT
}

fn secret_spec<'a>(env_var: Option<&'a str>, keyring_account: Option<&'a str>) -> SecretSpec<'a> {
    let account = keyring_account
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_KEYCHAIN_ACCOUNT);
    SecretSpec {
        service: KEYCHAIN_SERVICE,
        account,
        env_var,
        display_name: "ai-analysis API key",
    }
}
