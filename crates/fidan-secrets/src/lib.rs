use anyhow::{Context, Result, bail};
use keyring::Entry;

#[derive(Debug, Clone, Copy)]
pub struct SecretSpec<'a> {
    pub service: &'a str,
    pub account: &'a str,
    pub env_var: Option<&'a str>,
    pub display_name: &'a str,
}

pub fn resolve_secret(spec: &SecretSpec<'_>, explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(value) = normalize_secret_value_option(explicit) {
        return Ok(Some(value));
    }

    if let Some(env_name) = spec.env_var
        && let Ok(value) = std::env::var(env_name)
        && let Some(value) = normalize_secret_value(&value)
    {
        return Ok(Some(value));
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

fn is_no_entry_error(err: &keyring::Error) -> bool {
    matches!(err, keyring::Error::NoEntry)
}

fn normalize_secret_value_option(value: Option<&str>) -> Option<String> {
    value.and_then(normalize_secret_value)
}

fn normalize_secret_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_secret_value;

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
}
