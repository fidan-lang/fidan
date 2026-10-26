//! `fidan-sandbox` — Zero-config sandboxing policy for Fidan programs.
//!
//! When `--sandbox` is passed to `fidan run`, a [`SandboxPolicy`] is
//! constructed from the allow-flags and stored on the interpreter.  Every
//! stdlib call that touches a guarded resource is checked against the policy
//! before execution.
//!
//! # Guarded resources
//!
//! | Resource group | `io` functions covered |
//! |---|---|
//! | **file** | `readFile`, `writeFile`, `appendFile`, `deleteFile`, `fileExists`, `isFile`, `isDir`, `makeDir`, `listDir`, `copyFile`, `renameFile` |
//! | **env**  | `getEnv`, `setEnv` |
//!
//! `spawn` (OS subprocess) is not yet implemented in stdlib; the group is
//! reserved for when it is added.
//!
//! # Error codes
//!
//! Policy violations surface as `RunError`s with the following codes:
//! `R4001` (file-system read denied), `R4002` (file-system write denied),
//! `R4003` (environment access denied).
//!
//! ```text
//! error[R4001]: sandbox: file-system read denied (readFile "secret.txt")
//! ```

/// Which file-system paths the sandbox allows.
#[derive(Debug, Clone)]
pub enum FileAccess {
    /// All file operations denied.
    Denied,
    /// All file operations allowed (e.g. `--allow-read=*`).
    AllowAll,
    /// Only paths that start with one of these prefixes are allowed.
    AllowPrefixes(Vec<String>),
}

impl FileAccess {
    /// Returns `true` when access to `path` is permitted.
    pub fn permits(&self, path: &str) -> bool {
        match self {
            FileAccess::Denied => false,
            FileAccess::AllowAll => true,
            FileAccess::AllowPrefixes(prefixes) => {
                let canonical = normalise_path(path);
                !canonical.is_empty()
                    && prefixes.iter().any(|prefix| {
                        !prefix.is_empty()
                            && std::path::Path::new(&canonical)
                                .starts_with(std::path::Path::new(prefix))
                    })
            }
        }
    }
}

/// Normalise a path string for prefix-matching (forward slashes, no trailing slash).
// Canonicalize existing ancestors so a sibling prefix, .. or existing symlink
// cannot escape a grant. Missing normal suffixes allow creating new files.
// This is an interpreter policy, not OS isolation against hostile filesystem races.
fn normalise_path(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut path = std::path::PathBuf::from(value);
    if !path.is_absolute() {
        let Ok(cwd) = std::env::current_dir() else {
            return String::new();
        };
        path = cwd.join(path);
    }
    let mut suffix = Vec::new();
    loop {
        match path.canonicalize() {
            Ok(mut parent) => {
                for component in suffix.iter().rev() {
                    parent.push(component);
                }
                return parent.to_string_lossy().into_owned();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return String::new();
                }
                let Some(component) = path.file_name().map(|c| c.to_os_string()) else {
                    return String::new();
                };
                suffix.push(component);
                if !path.pop() {
                    return String::new();
                }
            }
            Err(_) => return String::new(),
        }
    }
}

/// The sandbox policy constructed from CLI flags.
///
/// All fields default to *denied* — construct with [`SandboxPolicy::default`]
/// and call the builder methods to loosen specific permissions.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// File read access (`readFile`, `fileExists`, `isFile`, `isDir`, `listDir`).
    pub allow_read: FileAccess,
    /// File write access (`writeFile`, `appendFile`, `deleteFile`, `makeDir`, `copyFile`, `renameFile`).
    pub allow_write: FileAccess,
    /// Network access — reserved for a future `std.net` module.
    pub allow_net: bool,
    /// Environment variable read/write (`getEnv`, `setEnv`, `args`).
    pub allow_env: bool,
    /// OS subprocess spawn — reserved for a future `std.process` module.
    pub allow_spawn: bool,
    /// Wall-time limit in seconds (0 = no limit).
    pub time_limit_secs: u64,
    /// Resident memory limit in MB (0 = no limit).
    pub mem_limit_mb: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            allow_read: FileAccess::Denied,
            allow_write: FileAccess::Denied,
            allow_net: false,
            allow_env: false,
            allow_spawn: false,
            time_limit_secs: 30,
            mem_limit_mb: 256,
        }
    }
}

impl SandboxPolicy {
    /// Permit reading from all paths.
    pub fn with_allow_read_all(mut self) -> Self {
        self.allow_read = FileAccess::AllowAll;
        self
    }

    /// Permit reading from a specific path prefix (can be called multiple times).
    pub fn with_allow_read_prefix(mut self, prefix: impl Into<String>) -> Self {
        let normalized = normalise_path(&prefix.into());
        match &mut self.allow_read {
            FileAccess::AllowPrefixes(v) => v.push(normalized),
            other => *other = FileAccess::AllowPrefixes(vec![normalized]),
        }
        self
    }

    /// Permit writing to all paths.
    pub fn with_allow_write_all(mut self) -> Self {
        self.allow_write = FileAccess::AllowAll;
        self
    }

    /// Permit writing to a specific path prefix.
    pub fn with_allow_write_prefix(mut self, prefix: impl Into<String>) -> Self {
        let normalized = normalise_path(&prefix.into());
        match &mut self.allow_write {
            FileAccess::AllowPrefixes(v) => v.push(normalized),
            other => *other = FileAccess::AllowPrefixes(vec![normalized]),
        }
        self
    }

    /// Allow environment variable access.
    pub fn with_allow_env(mut self) -> Self {
        self.allow_env = true;
        self
    }

    /// Allow network access.
    pub fn with_allow_net(mut self) -> Self {
        self.allow_net = true;
        self
    }

    /// Allow subprocess spawn.
    pub fn with_allow_spawn(mut self) -> Self {
        self.allow_spawn = true;
        self
    }

    /// Override the default 30-second wall-time limit (0 = no limit).
    pub fn with_time_limit(mut self, secs: u64) -> Self {
        self.time_limit_secs = secs;
        self
    }

    /// Override the default 256 MB memory limit (0 = no limit).
    pub fn with_mem_limit(mut self, mb: u64) -> Self {
        self.mem_limit_mb = mb;
        self
    }

    // ── Policy checks ─────────────────────────────────────────────────────────

    /// Check whether a stdlib `io` call is permitted under this policy.
    ///
    /// Returns `Ok(())` if allowed, or `Err(SandboxViolation)` if denied.
    /// The caller is responsible for mapping `SandboxViolation` to the
    /// appropriate diagnostic code and `RunError`.
    pub fn check_io_call(&self, fn_name: &str, args: &[&str]) -> Result<(), SandboxViolation> {
        let first_arg = args.first().copied().unwrap_or("");
        let second_arg = args.get(1).copied().unwrap_or("");
        match fn_name {
            // ── File reads ────────────────────────────────────────────────────
            "readFile" | "read_file" | "fileExists" | "file_exists" | "exists" | "isFile"
            | "is_file" | "isDir" | "is_dir" | "listDir" | "list_dir" | "readDir" | "read_dir"
            | "absolutePath" | "absolute_path" => {
                if self.allow_read.permits(first_arg) {
                    Ok(())
                } else {
                    Err(SandboxViolation::ReadDenied {
                        fn_name: fn_name.to_string(),
                        path: first_arg.to_string(),
                    })
                }
            }
            "copyFile" | "copy_file" => {
                if !self.allow_read.permits(first_arg) {
                    return Err(SandboxViolation::ReadDenied {
                        fn_name: fn_name.to_string(),
                        path: first_arg.to_string(),
                    });
                }
                if !self.allow_write.permits(second_arg) {
                    return Err(SandboxViolation::WriteDenied {
                        fn_name: fn_name.to_string(),
                        path: second_arg.to_string(),
                    });
                }
                Ok(())
            }
            "renameFile" | "rename_file" | "moveFile" | "move_file" => {
                if !self.allow_write.permits(first_arg) {
                    return Err(SandboxViolation::WriteDenied {
                        fn_name: fn_name.to_string(),
                        path: first_arg.to_string(),
                    });
                }
                if !self.allow_write.permits(second_arg) {
                    return Err(SandboxViolation::WriteDenied {
                        fn_name: fn_name.to_string(),
                        path: second_arg.to_string(),
                    });
                }
                Ok(())
            }
            // ── File writes ───────────────────────────────────────────────────
            "writeFile" | "write_file" | "appendFile" | "append_file" | "deleteFile"
            | "delete_file" | "makeDir" | "make_dir" | "mkdir" => {
                if self.allow_write.permits(first_arg) {
                    Ok(())
                } else {
                    Err(SandboxViolation::WriteDenied {
                        fn_name: fn_name.to_string(),
                        path: first_arg.to_string(),
                    })
                }
            }
            // ── Environment ───────────────────────────────────────────────────
            "getEnv" | "get_env" | "env" | "setEnv" | "set_env" | "args" | "argv" | "cwd"
            | "currentDir" | "current_dir" => {
                if self.allow_env {
                    Ok(())
                } else {
                    Err(SandboxViolation::EnvDenied {
                        fn_name: fn_name.to_string(),
                    })
                }
            }
            // Everything else (print, readLine, flush, path utils) is always allowed.
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SandboxPolicy, SandboxViolation};

    #[test]
    fn path_grants_are_component_bounded_and_resolve_existing_ancestors() {
        let root = std::env::temp_dir().join(format!(
            "fidan-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("allowed")).unwrap();
        std::fs::create_dir_all(root.join("allowed-sibling")).unwrap();
        std::fs::write(root.join("outside"), "private fixture").unwrap();
        let policy =
            SandboxPolicy::default().with_allow_read_prefix(root.join("allowed").to_string_lossy());
        assert!(
            policy
                .allow_read
                .permits(&root.join("allowed/new.txt").to_string_lossy())
        );
        assert!(
            !policy
                .allow_read
                .permits(&root.join("allowed-sibling/file").to_string_lossy())
        );
        assert!(
            !policy
                .allow_read
                .permits(&root.join("allowed/../outside").to_string_lossy())
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("outside"), root.join("allowed/link")).unwrap();
            assert!(
                !policy
                    .allow_read
                    .permits(&root.join("allowed/link").to_string_lossy())
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_file_requires_readable_source_and_writable_destination() {
        let policy = SandboxPolicy::default()
            .with_allow_read_prefix("/allowed/source")
            .with_allow_write_prefix("/allowed/dest");

        assert!(
            policy
                .check_io_call(
                    "copyFile",
                    &["/allowed/source/input.txt", "/allowed/dest/output.txt"],
                )
                .is_ok()
        );

        assert!(matches!(
            policy.check_io_call(
                "copyFile",
                &["/blocked/input.txt", "/allowed/dest/output.txt"],
            ),
            Err(SandboxViolation::ReadDenied { path, .. }) if path == "/blocked/input.txt"
        ));

        assert!(matches!(
            policy.check_io_call(
                "copyFile",
                &["/allowed/source/input.txt", "/blocked/output.txt"],
            ),
            Err(SandboxViolation::WriteDenied { path, .. }) if path == "/blocked/output.txt"
        ));
    }

    #[test]
    fn rename_file_requires_writable_source_and_destination() {
        let policy = SandboxPolicy::default().with_allow_write_prefix("/allowed");

        assert!(
            policy
                .check_io_call("renameFile", &["/allowed/from.txt", "/allowed/to.txt"])
                .is_ok()
        );

        assert!(matches!(
            policy.check_io_call("renameFile", &["/blocked/from.txt", "/allowed/to.txt"]),
            Err(SandboxViolation::WriteDenied { path, .. }) if path == "/blocked/from.txt"
        ));

        assert!(matches!(
            policy.check_io_call("renameFile", &["/allowed/from.txt", "/blocked/to.txt"]),
            Err(SandboxViolation::WriteDenied { path, .. }) if path == "/blocked/to.txt"
        ));
    }
}

/// A sandbox policy violation, produced by [`SandboxPolicy::check_io_call`].
///
/// Each variant corresponds to one diagnostic code:
/// - [`SandboxViolation::ReadDenied`]  → `R4001`
/// - [`SandboxViolation::WriteDenied`] → `R4002`
/// - [`SandboxViolation::EnvDenied`]   → `R4003`
///
/// The caller (`fidan-interp`) is responsible for converting this into a
/// `RunError` with the correct `DiagCode`.
#[derive(Debug, Clone)]
pub enum SandboxViolation {
    /// A file-system read operation was denied.
    ReadDenied { fn_name: String, path: String },
    /// A file-system write operation was denied.
    WriteDenied { fn_name: String, path: String },
    /// An environment variable access was denied.
    EnvDenied { fn_name: String },
}

impl SandboxViolation {
    /// Human-readable message (without the diagnostic code prefix).
    pub fn message(&self) -> String {
        match self {
            SandboxViolation::ReadDenied { fn_name, path } => {
                format!("sandbox: file-system read denied ({fn_name} {path:?})")
            }
            SandboxViolation::WriteDenied { fn_name, path } => {
                format!("sandbox: file-system write denied ({fn_name} {path:?})")
            }
            SandboxViolation::EnvDenied { fn_name } => {
                format!("sandbox: environment access denied ({fn_name})")
            }
        }
    }
}
