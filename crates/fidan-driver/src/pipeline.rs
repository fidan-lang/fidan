use crate::options::ExecutionMode;
use crate::{CompileOptions, Session};
use anyhow::{Context, Result};
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_lexer::SymbolInterner;
use fidan_mir::MirProgram;
#[cfg(windows)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Compile a fully-lowered `MirProgram` to a native binary.
///
/// The caller (fidan-cli) is responsible for running the full frontend pipeline
/// (lex → parse → typecheck → HIR → MIR) and passing the result here.
pub fn compile(
    _session: &Session,
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    opts: &CompileOptions,
) -> Result<()> {
    let output = {
        let raw = opts.output.clone().unwrap_or_else(|| {
            opts.input
                .with_extension(if cfg!(windows) { "exe" } else { "" })
        });
        #[cfg(windows)]
        let raw = if raw
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
        {
            raw
        } else {
            raw.with_extension("exe")
        };
        raw
    };

    match opts.mode {
        ExecutionMode::Build => match crate::install::resolve_effective_backend(opts.backend)? {
            crate::install::EffectiveBackend::Cranelift => {
                let progress =
                    crate::progress::ProgressReporter::spinner("build", "compiling with Cranelift");
                let result = compile_aot_cranelift(&program, interner.clone(), opts, output);
                progress.finish_and_clear();
                let out = result?;
                stage_windows_runtime_dependencies(&program, opts, &out)?;
                render_message_to_stderr(
                    Severity::Note,
                    "cranelift",
                    &format!("compiled to `{}`", out.display()),
                );
                Ok(())
            }
            crate::install::EffectiveBackend::Llvm(toolchain) => {
                let progress = crate::progress::ProgressReporter::spinner(
                    "build",
                    format!(
                        "compiling with LLVM toolchain {}",
                        toolchain.metadata.toolchain_version
                    ),
                );
                let symbols = interner
                    .snapshot()
                    .into_iter()
                    .map(|symbol| symbol.as_ref().to_owned())
                    .collect();
                let out = crate::llvm_helper::invoke_llvm_helper(
                    &toolchain, &program, symbols, opts, output,
                );
                progress.finish_and_clear();
                let out = out?;
                stage_windows_runtime_dependencies(&program, opts, &out)?;
                render_message_to_stderr(
                    Severity::Note,
                    "llvm",
                    &format!(
                        "compiled to `{}` via toolchain `{}`",
                        out.display(),
                        toolchain.metadata.toolchain_version
                    ),
                );
                Ok(())
            }
        },
        _ => unreachable!("compile() called with non-Build execution mode"),
    }
}

/// AOT via Cranelift — pure Rust, zero system dependencies.
fn compile_aot_cranelift(
    program: &MirProgram,
    interner: Arc<SymbolInterner>,
    opts: &CompileOptions,
    output: std::path::PathBuf,
) -> Result<PathBuf> {
    use fidan_codegen_cranelift::{CraneliftAotCompiler, CraneliftAotOptions};
    use fidan_codegen_cranelift::{CraneliftLtoMode, CraneliftOptLevel, CraneliftStripMode};

    let cl_opt = match opts.opt_level {
        crate::options::OptLevel::O0 => CraneliftOptLevel::None,
        crate::options::OptLevel::O1 | crate::options::OptLevel::O2 => CraneliftOptLevel::Speed,
        crate::options::OptLevel::O3
        | crate::options::OptLevel::Os
        | crate::options::OptLevel::Oz => CraneliftOptLevel::SpeedAndSize,
    };

    let aot_opts = CraneliftAotOptions {
        output: output.clone(),
        opt_level: cl_opt,
        lto: match opts.lto {
            crate::options::LtoMode::Off => CraneliftLtoMode::Off,
            crate::options::LtoMode::Full => CraneliftLtoMode::Full,
        },
        strip: match opts.strip {
            crate::options::StripMode::Off => CraneliftStripMode::Off,
            crate::options::StripMode::Symbols => CraneliftStripMode::Symbols,
            crate::options::StripMode::All => CraneliftStripMode::All,
        },
        emit_obj: opts.emit.contains(&crate::options::EmitKind::Obj),
        extra_lib_dirs: opts.extra_lib_dirs.clone(),
        link_dynamic: opts.link_dynamic,
    };

    let out = CraneliftAotCompiler::compile(program, interner, &aot_opts)?;
    Ok(out)
}

fn stage_windows_runtime_dependencies(
    program: &MirProgram,
    opts: &CompileOptions,
    output: &Path,
) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (program, opts, output);
        Ok(())
    }

    #[cfg(windows)]
    {
        let output_dir = output
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let runtime_deps = collect_windows_runtime_dependencies(program, opts);
        for source in runtime_deps {
            stage_runtime_artifact(&source, output_dir)?;
        }
        if opts.link_dynamic {
            let runtime_dir = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .context("failed to resolve the running Fidan installation directory")?;
            let runtime_dll = runtime_dir.join("fidan_runtime.dll");
            if runtime_dll.is_file() {
                stage_runtime_artifact(&runtime_dll, output_dir)?;
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn collect_windows_runtime_dependencies(
    program: &MirProgram,
    opts: &CompileOptions,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut deps = Vec::new();
    for function in &program.functions {
        let Some(extern_decl) = &function.extern_decl else {
            continue;
        };
        let raw = extern_decl.lib.trim();
        if raw.is_empty() || raw == "self" {
            continue;
        }
        let is_dll = Path::new(raw)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"));
        if !is_dll {
            continue;
        }
        if let Some(path) = resolve_windows_runtime_dependency(raw, opts)
            && seen.insert(path.clone())
        {
            deps.push(path);
        }
    }
    deps
}

#[cfg(windows)]
fn resolve_windows_runtime_dependency(raw: &str, opts: &CompileOptions) -> Option<PathBuf> {
    let path = Path::new(raw);
    let mut candidates = Vec::new();
    if path.is_absolute() {
        candidates.push(path.to_path_buf());
    } else {
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join(path));
        }
        if let Some(parent) = opts.input.parent() {
            candidates.push(parent.join(path));
        }
        if let Some(file_name) = path.file_name() {
            for dir in &opts.extra_lib_dirs {
                candidates.push(dir.join(file_name));
            }
        }
    }

    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn stage_runtime_artifact(source: &Path, output_dir: &Path) -> Result<()> {
    let file_name = source
        .file_name()
        .context("runtime artifact path does not have a file name")?;
    let destination = output_dir.join(file_name);
    if source == destination {
        return Ok(());
    }
    std::fs::copy(source, &destination).with_context(|| {
        format!(
            "failed to copy runtime dependency `{}` to `{}`",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::{resolve_windows_runtime_dependency, stage_runtime_artifact};
    use crate::{Backend, CompileOptions, ExecutionMode};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn windows_runtime_dependency_prefers_current_dir() {
        let sandbox = temp_dir("fidan_runtime_dep");
        let dll_dir = sandbox.join("libs");
        std::fs::create_dir_all(&dll_dir).expect("create dll dir");
        let source = sandbox.join("smoke.fdn");
        let dll = dll_dir.join("ffi_demo.dll");
        std::fs::write(&source, "action main {}").expect("write source");
        std::fs::write(&dll, []).expect("write dll");

        let old_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&sandbox).expect("set cwd");

        let opts = CompileOptions {
            input: source,
            output: Some(sandbox.join("out.exe")),
            mode: ExecutionMode::Build,
            backend: Backend::Cranelift,
            ..Default::default()
        };
        let resolved = resolve_windows_runtime_dependency("./libs/ffi_demo.dll", &opts);

        std::env::set_current_dir(old_cwd).expect("restore cwd");
        std::fs::remove_dir_all(&sandbox).ok();

        assert_eq!(resolved.as_deref(), Some(dll.as_path()));
    }

    #[test]
    fn stage_runtime_artifact_copies_dll_next_to_binary() {
        let sandbox = temp_dir("fidan_runtime_stage");
        let source_dir = sandbox.join("src");
        let out_dir = sandbox.join("out");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let dll = source_dir.join("fixture.dll");
        std::fs::write(&dll, b"dll-bytes").expect("write source dll");

        stage_runtime_artifact(&dll, &out_dir).expect("stage runtime artifact");

        let copied = out_dir.join("fixture.dll");
        assert_eq!(
            std::fs::read(&copied).expect("read copied dll"),
            b"dll-bytes"
        );

        std::fs::remove_dir_all(&sandbox).ok();
    }
}
