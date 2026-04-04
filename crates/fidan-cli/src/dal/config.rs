use anyhow::Result;
use fidan_secrets::{SecretSpec, clear_secret, resolve_secret, store_secret, verify_stored_secret};
use std::env;

pub const DEFAULT_REGISTRY: &str = "https://api.dal.fidan.dev";
const REGISTRY_ENV: &str = "FIDAN_DAL_REGISTRY";
const TOKEN_ENV: &str = "FIDAN_DAL_API_TOKEN";
const KEYCHAIN_SERVICE: &str = "fidan";
const KEYCHAIN_ACCOUNT: &str = "dal_api_token";

pub fn clear_token() -> Result<()> {
    clear_secret(&token_spec())
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
    resolve_secret(&token_spec(), explicit)
}

pub fn store_token(token: &str) -> Result<()> {
    store_secret(&token_spec(), token)
}

pub fn verify_stored_token(token: &str) -> Result<()> {
    verify_stored_secret(&token_spec(), token)
}

fn normalize_registry(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn token_spec() -> SecretSpec<'static> {
    SecretSpec {
        service: KEYCHAIN_SERVICE,
        account: KEYCHAIN_ACCOUNT,
        env_var: Some(TOKEN_ENV),
        display_name: "Dal API token",
    }
}
