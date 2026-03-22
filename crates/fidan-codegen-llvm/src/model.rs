use anyhow::{Result, bail};
use fidan_mir::MirProgram;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    Os,
    Oz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LtoMode {
    #[default]
    Off,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StripMode {
    #[default]
    Off,
    Symbols,
    All,
}

#[derive(Debug, Clone)]
pub struct CompileRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub runtime_dir: PathBuf,
    pub payload: BackendPayload,
    pub opt_level: OptLevel,
    pub lto: LtoMode,
    pub strip: StripMode,
    pub emit_obj: bool,
    pub extra_lib_dirs: Vec<PathBuf>,
    pub link_dynamic: bool,
}

#[derive(Debug, Clone)]
pub struct BackendPayload {
    pub program: MirProgram,
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ToolchainLayout {
    pub root: PathBuf,
    pub helper_path: PathBuf,
    pub metadata_path: PathBuf,
    pub metadata: ToolchainMetadata,
    pub llvm_root: PathBuf,
    pub bin_dir: PathBuf,
    pub lib_dir: PathBuf,
    pub include_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainMetadata {
    pub schema_version: u32,
    pub kind: String,
    pub toolchain_version: String,
    pub tool_version: String,
    pub host_triple: String,
    pub supported_fidan_versions: String,
    pub backend_protocol_version: u32,
    pub helper_relpath: String,
}

impl CompileRequest {
    pub fn opt_level_name(&self) -> &'static str {
        match self.opt_level {
            OptLevel::O0 => "O0",
            OptLevel::O1 => "O1",
            OptLevel::O2 => "O2",
            OptLevel::O3 => "O3",
            OptLevel::Os => "Os",
            OptLevel::Oz => "Oz",
        }
    }

    pub fn lto_name(&self) -> &'static str {
        match self.lto {
            LtoMode::Off => "off",
            LtoMode::Full => "full",
        }
    }

    pub fn strip_name(&self) -> &'static str {
        match self.strip {
            StripMode::Off => "off",
            StripMode::Symbols => "symbols",
            StripMode::All => "all",
        }
    }
}

impl ToolchainLayout {
    pub fn clang_driver_path(&self) -> PathBuf {
        self.bin_dir.join(if cfg!(target_os = "windows") {
            "clang.exe"
        } else {
            "clang"
        })
    }

    pub fn clang_cl_path(&self) -> PathBuf {
        self.bin_dir.join(if cfg!(target_os = "windows") {
            "clang-cl.exe"
        } else {
            "clang"
        })
    }

    pub fn linker_path(&self) -> PathBuf {
        self.bin_dir.join(if cfg!(target_os = "windows") {
            "lld-link.exe"
        } else {
            "lld"
        })
    }

    pub fn optimizer_path(&self) -> PathBuf {
        self.bin_dir.join(if cfg!(target_os = "windows") {
            "opt.exe"
        } else {
            "opt"
        })
    }

    pub fn codegen_path(&self) -> PathBuf {
        self.bin_dir.join(if cfg!(target_os = "windows") {
            "llc.exe"
        } else {
            "llc"
        })
    }

    pub fn strip_path(&self) -> Result<PathBuf> {
        let path = self.bin_dir.join(if cfg!(target_os = "windows") {
            "llvm-strip.exe"
        } else {
            "llvm-strip"
        });
        if path.is_file() {
            Ok(path)
        } else {
            bail!(
                "LLVM toolchain is missing required strip tool at `{}`",
                path.display()
            )
        }
    }

    pub fn libclang_path(&self) -> Result<PathBuf> {
        self.resolve_runtime_artifact(
            "libclang runtime library",
            if cfg!(target_os = "windows") {
                &["libclang.dll"]
            } else if cfg!(target_os = "macos") {
                &["libclang.dylib", "libclang-cpp.dylib"]
            } else {
                &[
                    "libclang.so",
                    "libclang.so.1",
                    "libclang-cpp.so",
                    "libclang-cpp.so.1",
                ]
            },
        )
    }

    pub fn lto_path(&self) -> Result<PathBuf> {
        self.resolve_runtime_artifact(
            "LTO runtime library",
            if cfg!(target_os = "windows") {
                &["LTO.dll"]
            } else if cfg!(target_os = "macos") {
                &["libLTO.dylib"]
            } else {
                &["libLTO.so", "libLTO.so.1"]
            },
        )
    }

    fn resolve_runtime_artifact(&self, label: &str, candidates: &[&str]) -> Result<PathBuf> {
        for directory in [&self.bin_dir, &self.lib_dir] {
            for candidate in candidates {
                let path = directory.join(candidate);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }

        bail!(
            "LLVM toolchain is missing required {label} in `{}` or `{}` (looked for: {})",
            self.bin_dir.display(),
            self.lib_dir.display(),
            candidates.join(", ")
        )
    }
}
