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
                // Prefixes are pre-normalised at insertion — only normalise the
                // incoming path once per call rather than once per prefix.
                let canonical = normalise_path(path);
                prefixes.iter().any(|p| canonical.starts_with(p.as_str()))
            }
        }
    }
}

/// Normalise a path string for prefix-matching (forward slashes, no trailing slash).
fn normalise_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    s.trim_end_matches('/').to_string()
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
    pub fn check_io_call(&self, fn_name: &str, first_arg: &str) -> Result<(), SandboxViolation> {
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
            // ── File writes ───────────────────────────────────────────────────
            "writeFile" | "write_file" | "appendFile" | "append_file" | "deleteFile"
            | "delete_file" | "makeDir" | "make_dir" | "mkdir" | "renameFile" | "rename_file"
            | "copyFile" | "copy_file" => {
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
