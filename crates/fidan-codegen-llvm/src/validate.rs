use crate::context::BackendContext;
use crate::model::{BackendPayload, ToolchainLayout, ToolchainMetadata};
use anyhow::{Context, Result, bail};
use fidan_mir::{
    BasicBlock, Callee, FunctionId, GlobalId, Instr, LocalId, MirEnumInfo, MirFunction, MirGlobal,
    MirObjectInfo, MirStringPart, MirTy, Operand, PhiNode, Rvalue, Terminator,
};
use std::env;
use std::path::Path;

pub fn validate_toolchain_layout(helper_path: &Path) -> Result<ToolchainLayout> {
    let helper_path = helper_path
        .canonicalize()
        .with_context(|| format!("failed to resolve `{}`", helper_path.display()))?;
    if !helper_path.is_file() {
        bail!(
            "LLVM helper executable is missing at `{}`",
            helper_path.display()
        );
    }

    let helper_dir = helper_path
        .parent()
        .context("LLVM helper path is missing a parent directory")?;
    let root = helper_dir
        .parent()
        .context("LLVM helper path is missing the toolchain root directory")?
        .to_path_buf();
    let metadata_path = root.join("metadata.json");
    let metadata_bytes = std::fs::read(&metadata_path)
        .with_context(|| format!("failed to read `{}`", metadata_path.display()))?;
    let metadata: ToolchainMetadata = serde_json::from_slice(&metadata_bytes)
        .with_context(|| format!("failed to parse `{}`", metadata_path.display()))?;

    if metadata.kind != "llvm" {
        bail!(
            "toolchain at `{}` is `{}` instead of `llvm`",
            root.display(),
            metadata.kind
        );
    }

    let expected_helper_name = helper_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("LLVM helper executable name is not valid UTF-8")?;
    let expected_relpath = Path::new("helper").join(expected_helper_name);
    let normalized_helper_relpath = metadata.helper_relpath.replace('\\', "/");
    let normalized_expected_relpath = expected_relpath.to_string_lossy().replace('\\', "/");
    if normalized_helper_relpath != normalized_expected_relpath {
        bail!(
            "toolchain metadata expects helper `{}`, but the running helper is `{}`",
            metadata.helper_relpath,
            normalized_expected_relpath
        );
    }

    let expected_host = current_host_triple()?;
    if metadata.host_triple != expected_host {
        bail!(
            "toolchain host triple mismatch (toolchain={}, host={})",
            metadata.host_triple,
            expected_host
        );
    }

    let llvm_root = root.join("llvm");
    let bin_dir = llvm_root.join("bin");
    let lib_dir = llvm_root.join("lib");
    let include_dir = llvm_root.join("include");

    ensure_dir(&llvm_root, "LLVM toolchain root")?;
    ensure_dir(&bin_dir, "LLVM bin directory")?;
    ensure_dir(&lib_dir, "LLVM lib directory")?;

    for required_tool in required_tools() {
        let tool_path = bin_dir.join(required_tool);
        if !tool_path.is_file() {
            bail!(
                "LLVM toolchain is missing required tool `{}` at `{}`",
                required_tool,
                tool_path.display()
            );
        }
    }

    let layout = ToolchainLayout {
        root,
        helper_path,
        metadata_path,
        metadata,
        llvm_root,
        bin_dir,
        lib_dir,
        include_dir,
    };
    if !cfg!(target_os = "windows") {
        layout.libclang_path()?;
        layout.lto_path()?;
    }

    Ok(layout)
}

pub fn validate_backend_payload(payload: &BackendPayload) -> Result<()> {
    let backend = BackendContext::new(payload);

    for function in &payload.program.functions {
        validate_function(function, &backend)?;
    }
    for object in &payload.program.objects {
        validate_object(object, &backend)?;
    }
    for global in &payload.program.globals {
        validate_global(global, &backend)?;
    }
    for enum_info in &payload.program.enums {
        validate_enum(enum_info, &backend)?;
    }

    Ok(())
}

fn ensure_dir(path: &Path, label: &str) -> Result<()> {
    if path.is_dir() {
        Ok(())
    } else {
        bail!("{label} is missing at `{}`", path.display())
    }
}

fn current_host_triple() -> Result<String> {
    let os = match env::consts::OS {
        "windows" => "pc-windows-msvc",
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        other => bail!("unsupported operating system `{other}`"),
    };
    let arch = match env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("unsupported architecture `{other}`"),
    };
    Ok(format!("{arch}-{os}"))
}

fn required_tools() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["lld-link.exe"]
    } else {
        &["clang"]
    }
}

fn validate_function(function: &MirFunction, backend: &BackendContext<'_>) -> Result<()> {
    backend.symbol_name(function.name)?;
    backend.effective_return_ty(function);
    backend.build_local_type_map(function);

    for param in &function.params {
        backend.symbol_name(param.name)?;
        validate_type(&param.ty, backend)?;
    }
    for block in &function.blocks {
        validate_block(block, backend)?;
    }
    Ok(())
}

fn validate_block(block: &BasicBlock, backend: &BackendContext<'_>) -> Result<()> {
    for phi in &block.phis {
        validate_phi(phi, backend)?;
    }
    for instruction in &block.instructions {
        validate_instruction(instruction, backend)?;
    }
    validate_terminator(&block.terminator, backend)
}

fn validate_phi(phi: &PhiNode, backend: &BackendContext<'_>) -> Result<()> {
    validate_type(&phi.ty, backend)?;
    for (_, operand) in &phi.operands {
        validate_operand(operand, backend)?;
    }
    Ok(())
}

fn validate_instruction(instr: &Instr, backend: &BackendContext<'_>) -> Result<()> {
    match instr {
        Instr::Assign { ty, rhs, .. } => {
            validate_type(ty, backend)?;
            validate_rvalue(rhs, backend)
        }
        Instr::Call {
            result_ty,
            callee,
            args,
            ..
        } => {
            if let Some(result_ty) = result_ty {
                validate_type(result_ty, backend)?;
            }
            validate_callee(callee, backend)?;
            for arg in args {
                validate_operand(arg, backend)?;
            }
            Ok(())
        }
        Instr::SetField {
            object,
            field,
            value,
        } => {
            validate_operand(object, backend)?;
            backend.symbol_name(*field)?;
            validate_operand(value, backend)
        }
        Instr::GetField { object, field, .. } => {
            validate_operand(object, backend)?;
            backend.symbol_name(*field).map(|_| ())
        }
        Instr::GetIndex { object, index, .. } => {
            validate_operand(object, backend)?;
            validate_operand(index, backend)
        }
        Instr::SetIndex {
            object,
            index,
            value,
        } => {
            validate_operand(object, backend)?;
            validate_operand(index, backend)?;
            validate_operand(value, backend)
        }
        Instr::Drop { .. }
        | Instr::JoinAll { .. }
        | Instr::Nop
        | Instr::PushCatch(..)
        | Instr::PopCatch => Ok(()),
        Instr::SpawnConcurrent { args, .. }
        | Instr::SpawnParallel { args, .. }
        | Instr::SpawnExpr { args, .. } => validate_operands(args, backend),
        Instr::SpawnDynamic { method, args, .. } => {
            if let Some(method) = method {
                backend.symbol_name(*method)?;
            }
            validate_operands(args, backend)
        }
        Instr::AwaitPending { handle, .. } => validate_operand(handle, backend),
        Instr::ParallelIter {
            collection,
            closure_args,
            ..
        } => {
            validate_operand(collection, backend)?;
            validate_operands(closure_args, backend)
        }
        Instr::CertainCheck { operand, name } => {
            validate_operand(operand, backend)?;
            backend.symbol_name(*name).map(|_| ())
        }
        Instr::LoadGlobal { global, .. } => validate_global_ref(*global, backend),
        Instr::StoreGlobal { global, value } => {
            validate_global_ref(*global, backend)?;
            validate_operand(value, backend)
        }
    }
}

fn validate_terminator(term: &Terminator, backend: &BackendContext<'_>) -> Result<()> {
    match term {
        Terminator::Return(Some(operand)) => validate_operand(operand, backend),
        Terminator::Branch { cond, .. } | Terminator::Throw { value: cond } => {
            validate_operand(cond, backend)
        }
        Terminator::Return(None) | Terminator::Goto(..) | Terminator::Unreachable => Ok(()),
    }
}

fn validate_rvalue(rhs: &Rvalue, backend: &BackendContext<'_>) -> Result<()> {
    match rhs {
        Rvalue::Use(operand) => validate_operand(operand, backend),
        Rvalue::Binary { lhs, rhs, .. } => {
            validate_operand(lhs, backend)?;
            validate_operand(rhs, backend)
        }
        Rvalue::Unary { operand, .. } => validate_operand(operand, backend),
        Rvalue::NullCoalesce { lhs, rhs } => {
            validate_operand(lhs, backend)?;
            validate_operand(rhs, backend)
        }
        Rvalue::Call { callee, args } => {
            validate_callee(callee, backend)?;
            validate_operands(args, backend)
        }
        Rvalue::Construct { ty, fields } => {
            backend.symbol_name(*ty)?;
            for (field, operand) in fields {
                backend.symbol_name(*field)?;
                validate_operand(operand, backend)?;
            }
            Ok(())
        }
        Rvalue::List(items) | Rvalue::Tuple(items) => validate_operands(items, backend),
        Rvalue::Dict(entries) => {
            for (key, value) in entries {
                validate_operand(key, backend)?;
                validate_operand(value, backend)?;
            }
            Ok(())
        }
        Rvalue::StringInterp(parts) => {
            for part in parts {
                if let MirStringPart::Operand(operand) = part {
                    validate_operand(operand, backend)?;
                }
            }
            Ok(())
        }
        Rvalue::Literal(_) | Rvalue::CatchException => Ok(()),
        Rvalue::MakeClosure { fn_id, captures } => {
            validate_function_ref(FunctionId(*fn_id), backend)?;
            validate_operands(captures, backend)
        }
        Rvalue::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            validate_operand(target, backend)?;
            if let Some(start) = start {
                validate_operand(start, backend)?;
            }
            if let Some(end) = end {
                validate_operand(end, backend)?;
            }
            if let Some(step) = step {
                validate_operand(step, backend)?;
            }
            Ok(())
        }
        Rvalue::ConstructEnum {
            tag,
            payload: values,
        } => {
            backend.symbol_name(*tag)?;
            validate_operands(values, backend)
        }
        Rvalue::EnumTagCheck {
            value,
            expected_tag,
        } => {
            validate_operand(value, backend)?;
            backend.symbol_name(*expected_tag).map(|_| ())
        }
        Rvalue::EnumPayload { value, .. } => validate_operand(value, backend),
    }
}

fn validate_callee(callee: &Callee, backend: &BackendContext<'_>) -> Result<()> {
    match callee {
        Callee::Fn(function_id) => validate_function_ref(*function_id, backend),
        Callee::Method { receiver, method } => {
            validate_operand(receiver, backend)?;
            backend.symbol_name(*method).map(|_| ())
        }
        Callee::Builtin(symbol) => backend.symbol_name(*symbol).map(|_| ()),
        Callee::Dynamic(operand) => validate_operand(operand, backend),
    }
}

fn validate_operand(operand: &Operand, _backend: &BackendContext<'_>) -> Result<()> {
    match operand {
        Operand::Local(LocalId(_)) | Operand::Const(_) => Ok(()),
    }
}

fn validate_type(ty: &MirTy, backend: &BackendContext<'_>) -> Result<()> {
    match ty {
        MirTy::List(inner)
        | MirTy::Shared(inner)
        | MirTy::WeakShared(inner)
        | MirTy::Pending(inner) => validate_type(inner, backend),
        MirTy::Dict(key, value) => {
            validate_type(key, backend)?;
            validate_type(value, backend)
        }
        MirTy::Tuple(items) => {
            for item in items {
                validate_type(item, backend)?;
            }
            Ok(())
        }
        MirTy::Object(symbol) | MirTy::Enum(symbol) => backend.symbol_name(*symbol).map(|_| ()),
        MirTy::Integer
        | MirTy::Float
        | MirTy::Boolean
        | MirTy::Handle
        | MirTy::String
        | MirTy::Nothing
        | MirTy::Dynamic
        | MirTy::Function
        | MirTy::Error => Ok(()),
    }
}

fn validate_object(object: &MirObjectInfo, backend: &BackendContext<'_>) -> Result<()> {
    backend.symbol_name(object.name)?;
    if let Some(parent) = object.parent {
        backend.symbol_name(parent)?;
    }
    for field_name in &object.field_names {
        backend.symbol_name(*field_name)?;
    }
    for (method, function_id) in &object.methods {
        backend.symbol_name(*method)?;
        validate_function_ref(*function_id, backend)?;
    }
    if let Some(init_fn) = object.init_fn {
        validate_function_ref(init_fn, backend)?;
    }
    Ok(())
}

fn validate_global(global: &MirGlobal, backend: &BackendContext<'_>) -> Result<()> {
    backend.symbol_name(global.name)?;
    validate_type(&global.ty, backend)
}

fn validate_enum(enum_info: &MirEnumInfo, backend: &BackendContext<'_>) -> Result<()> {
    backend.symbol_name(enum_info.name)?;
    for (variant, _) in &enum_info.variants {
        backend.symbol_name(*variant)?;
    }
    Ok(())
}

fn validate_function_ref(function_id: FunctionId, backend: &BackendContext<'_>) -> Result<()> {
    backend.function(function_id).map(|_| ())
}

fn validate_global_ref(global_id: GlobalId, backend: &BackendContext<'_>) -> Result<()> {
    backend.global(global_id).map(|_| ())
}

fn validate_operands(operands: &[Operand], backend: &BackendContext<'_>) -> Result<()> {
    for operand in operands {
        validate_operand(operand, backend)?;
    }
    Ok(())
}
