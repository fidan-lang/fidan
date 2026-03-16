use anyhow::{Context, Result};
use keyring::Entry;
use std::env;

pub const DEFAULT_REGISTRY: &str = "https://api.dal.fidan.dev";
const REGISTRY_ENV: &str = "FIDAN_DAL_REGISTRY";
const TOKEN_ENV: &str = "FIDAN_DAL_API_TOKEN";
const KEYCHAIN_SERVICE: &str = "fidan";
const KEYCHAIN_ACCOUNT: &str = "dal_api_token";

pub fn clear_token() -> Result<()> {
    let entry = keychain_entry()?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(err) if is_no_entry_error(&err) => Ok(()),
        Err(err) => Err(err).context("failed to remove Dal API token from OS keychain"),
    }
}

pub fn resolve_registry(explicit: Option<&str>) -> Result<String> {
    if let Some(value) = explicit {
        return Ok(normalize_registry(value));
    }
    if let Ok(value) = env::var(REGISTRY_ENV)
        && !value.trim().is_empty()
    {
        return Ok(normalize_registry(&value));
    }

    Ok(DEFAULT_REGISTRY.to_string())
}

pub fn resolve_token(explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(value) = explicit {
        return Ok(Some(value.trim().to_string()));
    }
    if let Ok(value) = env::var(TOKEN_ENV)
        && !value.trim().is_empty()
    {
        return Ok(Some(value.trim().to_string()));
    }

    load_token_from_keychain()
}

pub fn store_token(token: &str) -> Result<()> {
    keychain_entry()?
        .set_password(token.trim())
        .context("failed to store Dal API token in OS keychain")
}

fn load_token_from_keychain() -> Result<Option<String>> {
    let entry = keychain_entry()?;
    match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(err) if is_no_entry_error(&err) => Ok(None),
        Err(err) => Err(err).context("failed to read Dal API token from OS keychain"),
    }
}

fn keychain_entry() -> Result<Entry> {
    Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .context("failed to initialize OS keychain entry for Dal API token")
}

fn normalize_registry(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn is_no_entry_error(err: &keyring::Error) -> bool {
    matches!(
        err,
        keyring::Error::NoEntry | keyring::Error::NoStorageAccess(_)
    )
}
