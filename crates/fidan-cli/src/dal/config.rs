use anyhow::{Context, Result, bail};
use fidan_secrets::{
    SecretSpec, clear_secret, load_secret, resolve_secret, store_secret, verify_stored_secret,
};
use sha2::{Digest, Sha256};
use std::env;

pub const DEFAULT_REGISTRY: &str = "https://api.dal.fidan.dev";
const REGISTRY_ENV: &str = "FIDAN_DAL_REGISTRY";
const TOKEN_ENV: &str = "FIDAN_DAL_API_TOKEN";
const KEYCHAIN_SERVICE: &str = "fidan";
const LEGACY_ACCOUNT: &str = "dal_api_token";

pub fn resolve_registry(explicit: Option<&str>) -> Result<String> {
    if let Some(value) = explicit {
        return canonical_registry(value);
    }
    match env::var(REGISTRY_ENV) {
        Ok(value) if !value.trim().is_empty() => canonical_registry(&value),
        Err(env::VarError::NotUnicode(_)) => bail!("FIDAN_DAL_REGISTRY is not valid Unicode"),
        _ => Ok(DEFAULT_REGISTRY.to_owned()),
    }
}

pub fn canonical_registry(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("invalid Dal registry URL")?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if url.host_str().is_none()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "Dal registry requires HTTPS (HTTP only on literal loopback), without credentials, query or fragment"
        );
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

// The full canonical URL includes scheme, effective port and meaningful path prefix.
fn account(registry: &str) -> Result<String> {
    let identity = canonical_registry(registry)?;
    let digest = Sha256::digest(identity.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{LEGACY_ACCOUNT}_{hex}"))
}

fn token_spec(account: &str) -> SecretSpec<'_> {
    SecretSpec {
        service: KEYCHAIN_SERVICE,
        account,
        env_var: None,
        display_name: "Dal API token",
    }
}

pub fn resolve_token(registry: &str, explicit: Option<&str>) -> Result<Option<String>> {
    let registry = canonical_registry(registry)?;
    let account = account(&registry)?;
    let spec = token_spec(&account);
    // A present-but-empty env override deliberately disables saved credentials.
    if explicit.is_some_and(|value| !value.trim().is_empty()) || env::var_os(TOKEN_ENV).is_some() {
        return resolve_secret(
            &SecretSpec {
                env_var: Some(TOKEN_ENV),
                ..spec
            },
            explicit,
        );
    }
    if let Some(token) = load_secret(&spec)? {
        return Ok(Some(token));
    }
    // Never try the unscoped legacy credential for an alternate registry.
    if registry == DEFAULT_REGISTRY
        && let Some(token) = load_secret(&token_spec(LEGACY_ACCOUNT))?
    {
        store_token(&registry, &token)?;
        return Ok(Some(token));
    }
    Ok(None)
}

pub fn store_token(registry: &str, token: &str) -> Result<()> {
    let registry = canonical_registry(registry)?;
    let account = account(&registry)?;
    let spec = token_spec(&account);
    store_secret(&spec, token)?;
    verify_stored_secret(&spec, token)?;
    // Retain the legacy entry until the scoped replacement is verified.
    if registry == DEFAULT_REGISTRY {
        clear_secret(&token_spec(LEGACY_ACCOUNT))?;
    }
    Ok(())
}

pub fn clear_token(registry: &str) -> Result<()> {
    let registry = canonical_registry(registry)?;
    let account = account(&registry)?;
    clear_secret(&token_spec(&account))?;
    // Otherwise an old login could reappear on the next lookup after logout.
    if registry == DEFAULT_REGISTRY {
        clear_secret(&token_spec(LEGACY_ACCOUNT))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_identity_has_no_origin_or_prefix_collisions() {
        assert_eq!(
            account(DEFAULT_REGISTRY).unwrap(),
            account("HTTPS://API.DAL.FIDAN.DEV:443///").unwrap()
        );
        assert_eq!(
            account("https://bücher.example").unwrap(),
            account("https://xn--bcher-kva.example/").unwrap()
        );
        for other in [
            "https://api.dal.fidan.dev.evil.test",
            "https://api.dal.fidan.dev:444",
            "https://api.dal.fidan.dev/prefix",
            "https://api.dal.fidan.dev/Prefix",
        ] {
            assert_ne!(account(DEFAULT_REGISTRY).unwrap(), account(other).unwrap());
        }
        assert_ne!(
            account("https://example.test/prefix").unwrap(),
            account("https://example.test/Prefix").unwrap()
        );
        assert_ne!(
            account("http://localhost").unwrap(),
            account("https://localhost").unwrap()
        );
    }

    #[test]
    fn legacy_migration_and_lookup_failure_are_isolated() {
        const CASE: &str = "FIDAN_TEST_LEGACY_CREDENTIAL";
        if env::var_os(CASE).is_none() {
            let status = std::process::Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "dal::config::tests::legacy_migration_and_lookup_failure_are_isolated",
                ])
                .env(CASE, "1")
                .env_remove(TOKEN_ENV)
                .env_remove(REGISTRY_ENV)
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        let legacy = token_spec(LEGACY_ACCOUNT);
        store_secret(&legacy, "fixture_legacy").unwrap();
        for registry in [
            "https://other.example",
            "https://api.dal.fidan.dev:444",
            "https://api.dal.fidan.dev/prefix",
        ] {
            assert!(resolve_token(registry, None).unwrap().is_none());
            assert!(load_secret(&legacy).unwrap().is_some());
        }
        let scoped_account = account(DEFAULT_REGISTRY).unwrap();
        let entry = keyring_core::Entry::new(KEYCHAIN_SERVICE, &scoped_account).unwrap();
        let mock = entry
            .as_any()
            .downcast_ref::<keyring_core::mock::Cred>()
            .unwrap();
        mock.set_error(keyring_core::Error::Invalid(
            "fixture lookup".into(),
            "denied".into(),
        ));
        assert!(resolve_token(DEFAULT_REGISTRY, None).is_err());
        assert!(
            load_secret(&legacy).unwrap().is_some(),
            "lookup failure must not fall back or migrate"
        );
        assert_eq!(
            resolve_token(DEFAULT_REGISTRY, None).unwrap().as_deref(),
            Some("fixture_legacy")
        );
        assert!(load_secret(&legacy).unwrap().is_none());
        assert_eq!(
            resolve_token(DEFAULT_REGISTRY, None).unwrap().as_deref(),
            Some("fixture_legacy")
        );
        store_secret(&legacy, "fixture_stale").unwrap();
        clear_token(DEFAULT_REGISTRY).unwrap();
        assert!(resolve_token(DEFAULT_REGISTRY, None).unwrap().is_none());
    }
}
