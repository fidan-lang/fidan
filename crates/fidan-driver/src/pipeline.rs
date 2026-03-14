use crate::options::ExecutionMode;
use crate::{CompileOptions, Session};
use anyhow::Result;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_lexer::SymbolInterner;
use fidan_mir::MirProgram;
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
    match opts.mode {
        ExecutionMode::Build => match opts.backend {
            crate::options::Backend::Cranelift => compile_aot_cranelift(program, interner, opts),
            crate::options::Backend::Llvm => {
                anyhow::bail!(
                    "LLVM backend is not yet implemented. Use --backend cranelift (default)."
                )
            }
        },
        // Interpret / check / test modes are dispatched in the CLI before reaching here.
        _ => unreachable!("compile() called with non-Build execution mode"),
    }
}

/// AOT via Cranelift — pure Rust, zero system dependencies.
fn compile_aot_cranelift(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    opts: &CompileOptions,
) -> Result<()> {
    use fidan_codegen_cranelift::CraneliftOptLevel;
    use fidan_codegen_cranelift::{CraneliftAotCompiler, CraneliftAotOptions};

    let output = {
        let raw = opts.output.clone().unwrap_or_else(|| {
            opts.input
                .with_extension(if cfg!(windows) { "exe" } else { "" })
        });
        // On Windows, ensure the output path always has the .exe extension so
        // the resulting binary is directly executable without renaming.
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

    let cl_opt = match opts.opt_level {
        crate::options::OptLevel::O0 => CraneliftOptLevel::None,
        crate::options::OptLevel::O1 | crate::options::OptLevel::O2 => CraneliftOptLevel::Speed,
        // O3 (release mode), Os, Oz — use Cranelift's most aggressive preset.
        crate::options::OptLevel::O3
        | crate::options::OptLevel::Os
        | crate::options::OptLevel::Oz => CraneliftOptLevel::SpeedAndSize,
    };

    let aot_opts = CraneliftAotOptions {
        output: output.clone(),
        opt_level: cl_opt,
        emit_obj: opts.emit.contains(&crate::options::EmitKind::Obj),
        extra_lib_dirs: opts.extra_lib_dirs.clone(),
        link_dynamic: opts.link_dynamic,
    };

    let out = CraneliftAotCompiler::compile(&program, interner, &aot_opts)?;
    render_message_to_stderr(
        Severity::Note,
        "cranelift",
        &format!("compiled to `{}`", out.display()),
    );
    Ok(())
}
