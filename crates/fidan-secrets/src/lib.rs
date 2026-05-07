use anyhow::{Context, Result, bail};
use keyring_core::{CredentialStore, Entry};
use std::sync::Arc;

pub struct SecretStoreGuard {
    previous_store: Option<Arc<CredentialStore>>,
}

impl SecretStoreGuard {
    fn install(store: Arc<CredentialStore>) -> Self {
        let previous_store = keyring_core::unset_default_store();
        keyring_core::set_default_store(store);
        Self { previous_store }
    }
}

impl Drop for SecretStoreGuard {
    fn drop(&mut self) {
        keyring_core::unset_default_store();
        if let Some(previous_store) = self.previous_store.take() {
            keyring_core::set_default_store(previous_store);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SecretSpec<'a> {
    pub service: &'a str,
    pub account: &'a str,
    pub env_var: Option<&'a str>,
    pub display_name: &'a str,
}

pub fn init_default_store() -> Result<SecretStoreGuard> {
    Ok(SecretStoreGuard::install(build_default_store()?))
}

pub fn resolve_secret(spec: &SecretSpec<'_>, explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(value) = normalize_secret_value_option(explicit) {
        return Ok(Some(value));
    }

    if let Some(env_name) = spec.env_var {
        let env_value = match std::env::var(env_name) {
            Ok(v) => normalize_secret_value(&v),
            Err(std::env::VarError::NotPresent) => None,
            Err(err) => return Err(err.into()),
        };
        return Ok(env_value);
    }

    load_secret(spec)
}

pub fn load_secret(spec: &SecretSpec<'_>) -> Result<Option<String>> {
    let entry = keychain_entry(spec)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(err) if is_no_entry_error(&err) => Ok(None),
        Err(err) => Err(err)
            .with_context(|| format!("failed to read {} from OS keychain", spec.display_name)),
    }
}

pub fn store_secret(spec: &SecretSpec<'_>, value: &str) -> Result<()> {
    let normalized = normalize_secret_value(value)
        .with_context(|| format!("{} must not be empty or whitespace-only", spec.display_name))?;
    keychain_entry(spec)?
        .set_password(&normalized)
        .with_context(|| format!("failed to store {} in OS keychain", spec.display_name))
}

pub fn verify_stored_secret(spec: &SecretSpec<'_>, expected: &str) -> Result<()> {
    let expected = normalize_secret_value(expected)
        .with_context(|| format!("{} must not be empty or whitespace-only", spec.display_name))?;
    let stored = load_secret(spec)?.with_context(|| {
        format!(
            "{} could not be read back from the OS keychain after storing it",
            spec.display_name
        )
    })?;

    if stored.trim() != expected {
        bail!(
            "{} round-trip verification failed after storing it in the OS keychain",
            spec.display_name
        )
    }

    Ok(())
}

pub fn clear_secret(spec: &SecretSpec<'_>) -> Result<()> {
    let entry = keychain_entry(spec)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(err) if is_no_entry_error(&err) => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("failed to remove {} from OS keychain", spec.display_name)),
    }
}

fn keychain_entry(spec: &SecretSpec<'_>) -> Result<Entry> {
    Entry::new(spec.service, spec.account).with_context(|| {
        format!(
            "failed to initialize OS keychain entry for {}",
            spec.display_name
        )
    })
}

fn is_no_entry_error(err: &keyring_core::Error) -> bool {
    matches!(err, keyring_core::Error::NoEntry)
}

fn normalize_secret_value_option(value: Option<&str>) -> Option<String> {
    value.and_then(normalize_secret_value)
}

fn normalize_secret_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(target_os = "windows")]
fn build_default_store() -> Result<Arc<CredentialStore>> {
    let store = windows_native_keyring_store::Store::new()
        .context("failed to initialize Windows Credential Manager store")?;
    Ok(store)
}

#[cfg(target_os = "macos")]
fn build_default_store() -> Result<Arc<CredentialStore>> {
    let store = apple_native_keyring_store::keychain::Store::new()
        .context("failed to initialize macOS Keychain store")?;
    Ok(store)
}

#[cfg(target_os = "ios")]
fn build_default_store() -> Result<Arc<CredentialStore>> {
    let store = apple_native_keyring_store::protected::Store::new()
        .context("failed to initialize iOS Protected Data store")?;
    Ok(store)
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
fn build_default_store() -> Result<Arc<CredentialStore>> {
    let store = dbus_secret_service_keyring_store::Store::new()
        .context("failed to initialize Secret Service keyring store")?;
    Ok(store)
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
)))]
fn build_default_store() -> Result<Arc<CredentialStore>> {
    bail!("no supported OS keyring backend is configured for this target")
}

#[cfg(test)]
mod tests {
    use super::{
        SecretSpec, SecretStoreGuard, clear_secret, load_secret, normalize_secret_value,
        store_secret,
    };
    use keyring_core::api::CredentialStoreApi;
    use keyring_core::{CredentialStore, Entry, Error, get_default_store, mock};
    use std::sync::{Arc, LazyLock, Mutex};

    static TEST_STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn install_test_store(store: Arc<CredentialStore>) -> SecretStoreGuard {
        SecretStoreGuard::install(store)
    }

    #[test]
    fn normalize_secret_value_trims_non_empty_values() {
        assert_eq!(
            normalize_secret_value("  token  ").as_deref(),
            Some("token")
        );
    }

    #[test]
    fn normalize_secret_value_rejects_empty_values() {
        assert!(normalize_secret_value("   ").is_none());
        assert!(normalize_secret_value("").is_none());
    }

    #[test]
    fn secret_store_guard_unsets_default_store_when_dropped() {
        let _lock = TEST_STORE_LOCK.lock().unwrap();
        let _ = keyring_core::unset_default_store();

        assert!(matches!(
            Entry::new("svc", "acct"),
            Err(Error::NoDefaultStore)
        ));

        let guard = install_test_store(mock::Store::new().unwrap());
        let entry = Entry::new("svc", "acct").expect("store guard should install a default store");
        entry
            .set_password("secret")
            .expect("mock store should allow storing a password");

        drop(guard);

        assert!(matches!(
            Entry::new("svc", "acct"),
            Err(Error::NoDefaultStore)
        ));
    }

    #[test]
    fn secret_store_guard_restores_previous_store_on_drop() {
        let _lock = TEST_STORE_LOCK.lock().unwrap();
        let _ = keyring_core::unset_default_store();

        let previous_store = mock::Store::new().unwrap();
        let previous_id = previous_store.id();
        keyring_core::set_default_store(previous_store);

        let replacement_store = mock::Store::new().unwrap();
        let replacement_id = replacement_store.id();
        let guard = install_test_store(replacement_store);

        assert_eq!(
            get_default_store()
                .expect("replacement store should be installed")
                .id(),
            replacement_id
        );

        drop(guard);

        assert_eq!(
            get_default_store()
                .expect("previous store should be restored")
                .id(),
            previous_id
        );
        let _ = keyring_core::unset_default_store();
    }

    #[test]
    fn secret_helpers_round_trip_with_installed_store() {
        let _lock = TEST_STORE_LOCK.lock().unwrap();
        let _ = keyring_core::unset_default_store();
        let _guard = install_test_store(mock::Store::new().unwrap());

        let spec = SecretSpec {
            service: "fidan-test-service",
            account: "fidan-test-account",
            env_var: None,
            display_name: "test token",
        };

        store_secret(&spec, "  token  ").expect("secret should store successfully");
        assert_eq!(
            load_secret(&spec)
                .expect("secret should load successfully")
                .as_deref(),
            Some("token")
        );
        clear_secret(&spec).expect("secret should clear successfully");
        assert_eq!(
            load_secret(&spec)
                .expect("cleared secret should read as missing")
                .as_deref(),
            None
        );
    }
}
