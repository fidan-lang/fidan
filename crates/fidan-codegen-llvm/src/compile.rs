use crate::context::BackendContext;
use crate::model::CompileRequest;
use crate::validate::{validate_backend_payload, validate_toolchain_layout};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub fn compile_request(helper_path: &Path, request: &CompileRequest) -> Result<PathBuf> {
    trace("compile_request:start");
    #[cfg(feature = "llvm-toolchain-21")]
    let layout = validate_toolchain_layout(helper_path)?;
    #[cfg(not(feature = "llvm-toolchain-21"))]
    let _layout = validate_toolchain_layout(helper_path)?;
    trace("compile_request:validated_toolchain");
    validate_backend_payload(&request.payload)?;
    trace("compile_request:validated_payload");
    #[cfg(feature = "llvm-toolchain-21")]
    let backend = BackendContext::new(&request.payload);
    #[cfg(not(feature = "llvm-toolchain-21"))]
    let _backend = BackendContext::new(&request.payload);
    trace("compile_request:backend_context_ready");

    if !request.input.is_file() {
        bail!("input source `{}` does not exist", request.input.display());
    }
    if !request.runtime_dir.is_dir() {
        bail!(
            "Fidan runtime directory `{}` does not exist",
            request.runtime_dir.display()
        );
    }

    if let Some(parent) = request.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    for lib_dir in &request.extra_lib_dirs {
        if !lib_dir.is_dir() {
            bail!(
                "extra library directory `{}` does not exist",
                lib_dir.display()
            );
        }
    }

    #[cfg(feature = "llvm-toolchain-21")]
    let init_symbol = backend
        .init_function()
        .map(|function| backend.mangled_function_name(function))
        .transpose()?;
    #[cfg(feature = "llvm-toolchain-21")]
    let main_symbol = backend
        .main_function()
        .map(|function| backend.mangled_function_name(function))
        .transpose()?;

    #[cfg(feature = "llvm-toolchain-21")]
    {
        trace("compile_request:inkwell_backend");
        crate::inkwell_backend::compile_and_link_module(&layout, &backend, request)
            .with_context(|| {
                format!(
                    "failed to compile LLVM module (toolchain={}, llvm={}, init={}, main={}, opt={}, lto={}, strip={}, emit_obj={}, link_dynamic={})",
                    layout.metadata.toolchain_version,
                    layout.metadata.tool_version,
                    init_symbol.as_deref().unwrap_or("none"),
                    main_symbol.as_deref().unwrap_or("none"),
                    request.opt_level_name(),
                    request.lto_name(),
                    request.strip_name(),
                    request.emit_obj,
                    request.link_dynamic
                )
            })
    }

    #[cfg(not(feature = "llvm-toolchain-21"))]
    {
        bail!(
            "this build of fidan-llvm-helper was compiled without the required LLVM backend feature `llvm-toolchain-21`"
        );
    }
}

pub(crate) fn trace(message: &str) {
    let enabled = std::env::var("FIDAN_LLVM_TRACE")
        .ok()
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes")
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }

    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "[fidan-llvm] {message}");
    let _ = std::io::stderr().flush();
}

#[cfg(feature = "llvm-toolchain-21")]
pub(crate) fn dump_ir(module_ir: &str) {
    if !env_flag_enabled("FIDAN_LLVM_DUMP_IR") {
        return;
    }

    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "[fidan-llvm-ir] ----- begin -----");
    let _ = writeln!(std::io::stderr(), "{module_ir}");
    let _ = writeln!(std::io::stderr(), "[fidan-llvm-ir] -----  end  -----");
    let _ = std::io::stderr().flush();
}

#[cfg(feature = "llvm-toolchain-21")]
pub(crate) fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes")
        })
        .unwrap_or(false)
}
