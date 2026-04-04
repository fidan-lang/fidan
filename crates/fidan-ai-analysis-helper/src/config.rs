use anyhow::{Context, Result, bail};
use fidan_secrets::{SecretSpec, clear_secret, resolve_secret, store_secret};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const CONFIG_ENV: &str = "FIDAN_AI_ANALYSIS_CONFIG";
const KEYCHAIN_SERVICE: &str = "fidan";
const DEFAULT_KEYCHAIN_ACCOUNT: &str = "ai_analysis_api_key";

/// Starter config written on first use.  Uses TOML comments so hand-editing is
/// self-explanatory.  `provider = ""` and `model = ""` will fail the validation
/// check, intentionally prompting the user to configure those required fields.
const DEFAULT_CONFIG_TEMPLATE: &str = r#"# Fidan ai-analysis configuration
# Generated automatically — edit this file or use:
#   fidan exec ai configure --set key=value

# Required: AI provider and model name
schema_version = 1
provider = ""
model = ""

# Optional: override the HTTP endpoint (leave unset to use the provider default)
# For local/self-hosted LLMs (Ollama, LM Studio, etc.):
#   base_url = "http://localhost:11434/v1/chat/completions"
# base_url = ""

# Optional: read the API key from an environment variable instead of the keychain
# api_key_env = "OPENAI_API_KEY"

# Optional: OS keychain account name for the API key (default: \"ai_analysis_api_key\")
# keyring_account = "ai_analysis_api_key"

# Optional: HTTP request timeout in seconds (default: 60)
# timeout_secs = 60

# Optional: extra instructions appended to the built-in system prompt
# system_prompt = ""

# Optional: set to true to replace the built-in system prompt entirely
# replace_system_prompt = false
"#;

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

pub fn ensure_default_config(path: &Path) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory `{}`", parent.display()))?;
    }
    std::fs::write(path, DEFAULT_CONFIG_TEMPLATE)
        .with_context(|| format!("failed to create starter config at `{}`", path.display()))
}

pub fn load() -> Result<Config> {
    let path = resolve_config_path()?;
    if !path.is_file() {
        ensure_default_config(&path)?;
        bail!(
            "AI analysis is not yet configured\n\n\
            A starter config was created at `{}`\n\n\
            Configure the required fields by running:\n\
            \x20 fidan exec ai configure --set provider=openai-compatible --set model=YOUR_MODEL\n\n\
            For local LLMs (Ollama, LM Studio, etc.) also set the endpoint:\n\
            \x20 fidan exec ai configure --set base_url=http://localhost:11434/v1/chat/completions\n\n\
            Then run `fidan exec ai doctor` to verify the configuration.",
            path.display()
        );
    }
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
        bail!(
            "`provider` and `model` must be set in `{}`\n\n\
            Run: fidan exec ai configure --set provider=openai-compatible --set model=YOUR_MODEL",
            path.display()
        );
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

#[cfg(test)]
mod tests {
    use super::{DEFAULT_CONFIG_TEMPLATE, ensure_default_config, load_from_path};
    use std::path::PathBuf;

    fn write_toml(path: &PathBuf, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fidan_cfg_test_{name}.toml"))
    }

    fn cleanup(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ensure_default_config_creates_file_when_missing() {
        let path = temp_path("creates_file");
        cleanup(&path);
        assert!(!path.exists());
        ensure_default_config(&path).unwrap();
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("schema_version = 1"));
        assert!(text.contains("provider = \"\""));
        cleanup(&path);
    }

    #[test]
    fn ensure_default_config_is_noop_when_file_exists() {
        let path = temp_path("noop_existing");
        write_toml(&path, "# custom content\n");
        ensure_default_config(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# custom content\n");
        cleanup(&path);
    }

    #[test]
    fn default_config_template_is_valid_toml() {
        // The template must parse without errors so the user can hand-edit it
        let result = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TEMPLATE);
        assert!(
            result.is_ok(),
            "DEFAULT_CONFIG_TEMPLATE is not valid TOML: {result:?}"
        );
    }

    #[test]
    fn load_from_path_succeeds_with_valid_config() {
        let path = temp_path("valid_config");
        write_toml(
            &path,
            r#"schema_version = 1
provider = "openai-compatible"
model = "gpt-4.1-mini"
"#,
        );
        let config = load_from_path(&path).unwrap();
        assert_eq!(config.provider, "openai-compatible");
        assert_eq!(config.model, "gpt-4.1-mini");
        assert_eq!(config.timeout_secs, 60); // default applied
        assert!(!config.replace_system_prompt); // default applied
        cleanup(&path);
    }

    #[test]
    fn load_from_path_populates_optional_fields() {
        let path = temp_path("optional_fields");
        write_toml(
            &path,
            r#"schema_version = 1
provider = "anthropic"
model = "claude-3-5-sonnet"
base_url = "http://localhost:8080"
api_key_env = "MY_KEY"
keyring_account = "my-account"
timeout_secs = 30
system_prompt = "Be concise."
replace_system_prompt = true
"#,
        );
        let config = load_from_path(&path).unwrap();
        assert_eq!(config.base_url.as_deref(), Some("http://localhost:8080"));
        assert_eq!(config.api_key_env.as_deref(), Some("MY_KEY"));
        assert_eq!(config.keyring_account.as_deref(), Some("my-account"));
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.system_prompt.as_deref(), Some("Be concise."));
        assert!(config.replace_system_prompt);
        cleanup(&path);
    }

    #[test]
    fn load_from_path_rejects_empty_provider() {
        let path = temp_path("empty_provider");
        write_toml(
            &path,
            r#"schema_version = 1
provider = ""
model = "gpt-4.1-mini"
"#,
        );
        let err = load_from_path(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("`provider` and `model` must be set")
        );
        cleanup(&path);
    }

    #[test]
    fn load_from_path_rejects_empty_model() {
        let path = temp_path("empty_model");
        write_toml(
            &path,
            r#"schema_version = 1
provider = "openai-compatible"
model = ""
"#,
        );
        let err = load_from_path(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("`provider` and `model` must be set")
        );
        cleanup(&path);
    }

    #[test]
    fn load_from_path_rejects_unsupported_schema_version() {
        let path = temp_path("bad_schema");
        write_toml(
            &path,
            r#"schema_version = 99
provider = "openai-compatible"
model = "gpt-4.1-mini"
"#,
        );
        let err = load_from_path(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported ai-analysis config schema_version 99")
        );
        cleanup(&path);
    }

    #[test]
    fn load_from_path_rejects_malformed_toml() {
        let path = temp_path("malformed");
        write_toml(&path, "this is [not valid\ntoml = \n");
        let err = load_from_path(&path).unwrap_err();
        assert!(err.to_string().contains("failed to parse"));
        cleanup(&path);
    }

    #[test]
    fn load_from_path_mentions_configure_command_in_empty_field_error() {
        let path = temp_path("suggest_configure");
        write_toml(
            &path,
            r#"schema_version = 1
provider = ""
model = ""
"#,
        );
        let err = load_from_path(&path).unwrap_err();
        assert!(err.to_string().contains("fidan exec ai configure"));
        cleanup(&path);
    }

    #[test]
    fn resolve_api_key_uses_env_var_override() {
        // Safe to call with a fake env var that doesn't exist — expects None back
        let result = super::resolve_api_key_with(Some("FIDAN_TEST_NONEXISTENT_KEY_XYZ"), None);
        assert!(result.is_ok());
        // env var not set → returns None (no error)
        assert!(result.unwrap().is_none());
    }
}
