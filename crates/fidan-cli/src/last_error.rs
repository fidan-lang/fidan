use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastErrorRecord {
    pub code: String,
    pub message: String,
}

fn is_diagnostic_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 5
        && matches!(bytes[0], b'E' | b'W' | b'R')
        && bytes[1..].iter().all(|b| b.is_ascii_digit())
}

fn cache_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("Fidan");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(cache_home).join("fidan");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".cache").join("fidan");
        }
    }

    std::env::temp_dir().join("fidan")
}

fn cache_path() -> PathBuf {
    if let Some(override_path) = std::env::var_os("FIDAN_LAST_ERROR_PATH") {
        return PathBuf::from(override_path);
    }
    cache_dir().join("last-error.txt")
}

pub(crate) fn record(code: impl std::fmt::Display, message: &str) {
    let code = code.to_string();
    if !is_diagnostic_code(&code) {
        return;
    }

    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }

    let payload = format!("{code}\n{message}");
    let _ = std::fs::write(cache_path(), payload);
}

pub(crate) fn load() -> Result<LastErrorRecord> {
    let path = cache_path();
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no recorded Fidan error found at {}", path.display()))?;
    let mut lines = raw.lines();
    let code = lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("last-error cache is missing the diagnostic code")?
        .to_string();
    let message = lines.collect::<Vec<_>>().join("\n");
    if !is_diagnostic_code(&code) {
        anyhow::bail!("last recorded entry `{code}` is not a diagnostic code");
    }
    Ok(LastErrorRecord { code, message })
}

#[cfg(test)]
mod tests {
    use super::is_diagnostic_code;

    #[test]
    fn diagnostic_code_shape_is_checked() {
        assert!(is_diagnostic_code("E0101"));
        assert!(is_diagnostic_code("W1005"));
        assert!(is_diagnostic_code("R0001"));
        assert!(!is_diagnostic_code("cli"));
        assert!(!is_diagnostic_code("E101"));
        assert!(!is_diagnostic_code("E0101X"));
    }
}
