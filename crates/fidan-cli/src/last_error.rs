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

    let path = cache_path();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }

    let payload = format!("{code}\n{message}");
    let _ = std::fs::write(path, payload);
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
    use super::{cache_path, is_diagnostic_code, record};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fidan-last-error-{label}-{}-{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn diagnostic_code_shape_is_checked() {
        assert!(is_diagnostic_code("E0101"));
        assert!(is_diagnostic_code("W1005"));
        assert!(is_diagnostic_code("R0001"));
        assert!(!is_diagnostic_code("cli"));
        assert!(!is_diagnostic_code("E101"));
        assert!(!is_diagnostic_code("E0101X"));
    }

    #[test]
    fn record_creates_parent_dirs_for_override_path() {
        let _guard = env_lock().lock().expect("env lock");
        let sandbox = temp_dir("override");
        let override_path = sandbox.join("nested").join("last-error.txt");
        let previous = std::env::var_os("FIDAN_LAST_ERROR_PATH");
        // SAFETY: tests serialize access to process env via `env_lock`.
        unsafe { std::env::set_var("FIDAN_LAST_ERROR_PATH", &override_path) };

        record("E0101", "override path works");

        assert_eq!(
            fs::read_to_string(&override_path).expect("read override last-error file"),
            "E0101\noverride path works"
        );
        assert_eq!(cache_path(), override_path);

        if let Some(previous) = previous {
            // SAFETY: tests serialize access to process env via `env_lock`.
            unsafe { std::env::set_var("FIDAN_LAST_ERROR_PATH", previous) };
        } else {
            // SAFETY: tests serialize access to process env via `env_lock`.
            unsafe { std::env::remove_var("FIDAN_LAST_ERROR_PATH") };
        }
        let _ = fs::remove_dir_all(&sandbox);
    }
}
