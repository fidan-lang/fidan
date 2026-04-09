use crate::context::BackendContext;
use crate::model::{CompileRequest, LtoMode, OptLevel, ToolchainLayout};
use crate::tool::link_codegen_input;
use crate::{dump_ir, env_flag_enabled, trace};
use anyhow::{Context as _, Result, anyhow, bail};
use fidan_ast::{BinOp, UnOp};
use fidan_lexer::Symbol;
use fidan_mir::{
    BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirExternAbi, MirFunction, MirLit,
    MirStringPart, MirTy, Operand, Rvalue, Terminator,
};
use fidan_stdlib::{
    MathIntrinsic, ReceiverBuiltinKind, ReceiverMethodOp, StdlibIntrinsic, StdlibValueKind,
    infer_receiver_member, infer_stdlib_method,
};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::OptimizationLevel;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue, GlobalValue,
    IntValue, PointerValue,
};
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::bit_writer::LLVMWriteBitcodeToMemoryBuffer;
use llvm_sys::core::{LLVMDisposeMessage, LLVMSetTarget};
use llvm_sys::target::{LLVMDisposeTargetData, LLVMSetModuleDataLayout};
use llvm_sys::target_machine::{
    LLVMCreateTargetDataLayout, LLVMCreateTargetMachine, LLVMGetHostCPUFeatures,
    LLVMGetHostCPUName, LLVMGetTargetFromTriple,
};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString};
use std::fs;
use std::path::PathBuf;
use std::ptr;

#[derive(Debug, Clone)]
struct TargetCpuSpec {
    cpu: String,
    features: String,
}

fn resolve_target_cpu(request: &CompileRequest, target_triple: &str) -> Result<TargetCpuSpec> {
    match request.target_cpu.as_deref().map(str::trim) {
        Some(spec) if spec.eq_ignore_ascii_case("native") => native_target_cpu(target_triple),
        Some(spec) if spec.len() >= 7 && spec[..7].eq_ignore_ascii_case("native,") => {
            let mut native = native_target_cpu(target_triple)?;
            native.features = merge_feature_strings(&native.features, &spec[7..])?;
            Ok(native)
        }
        Some(spec) if !spec.is_empty() => parse_custom_cpu_spec(spec),
        _ => Ok(TargetCpuSpec {
            cpu: "generic".to_string(),
            features: String::new(),
        }),
    }
}

fn parse_custom_cpu_spec(spec: &str) -> Result<TargetCpuSpec> {
    let (cpu, feature_suffix) = split_cpu_and_features(spec)?;
    Ok(TargetCpuSpec {
        cpu: cpu.to_string(),
        features: normalize_feature_string(feature_suffix)?,
    })
}

fn native_target_cpu(target_triple: &str) -> Result<TargetCpuSpec> {
    let host_triple = current_host_triple()?;
    if host_triple != target_triple {
        bail!(
            "`--target-cpu native` targets the current compiler host (`{host_triple}`), but the active LLVM target triple is `{target_triple}`"
        );
    }
    let raw_cpu = unsafe { LLVMGetHostCPUName() };
    let cpu = llvm_host_string(raw_cpu, "host CPU name")?;
    let raw_features = unsafe { LLVMGetHostCPUFeatures() };
    let features = llvm_host_string(raw_features, "host CPU features")?;
    Ok(TargetCpuSpec { cpu, features })
}

fn split_cpu_and_features(spec: &str) -> Result<(&str, &str)> {
    let trimmed = spec.trim();
    let (cpu, features) = match trimmed.find(',') {
        Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
        None => (trimmed, ""),
    };
    let cpu = cpu.trim();
    if cpu.is_empty() {
        bail!("target CPU spec `{trimmed}` is missing a CPU name");
    }
    Ok((cpu, features))
}

fn normalize_feature_string(features: &str) -> Result<String> {
    if features.trim().is_empty() {
        return Ok(String::new());
    }

    let mut ordered = Vec::new();
    let mut latest = HashMap::new();
    for raw in features.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        if token.len() < 2 || !matches!(token.as_bytes()[0], b'+' | b'-') {
            bail!(
                "target CPU feature `{token}` must start with `+` or `-` (for example `+avx2` or `-avx512f`)"
            );
        }
        let feature_name = token[1..].trim();
        if feature_name.is_empty() {
            bail!("target CPU feature override `{token}` is missing a feature name");
        }
        let normalized = format!("{}{}", &token[..1], feature_name);
        if !latest.contains_key(feature_name) {
            ordered.push(feature_name.to_string());
        }
        latest.insert(feature_name.to_string(), normalized);
    }

    Ok(ordered
        .into_iter()
        .filter_map(|name| latest.remove(&name))
        .collect::<Vec<_>>()
        .join(","))
}

fn merge_feature_strings(base: &str, overrides: &str) -> Result<String> {
    if base.trim().is_empty() {
        return normalize_feature_string(overrides);
    }
    if overrides.trim().is_empty() {
        return normalize_feature_string(base);
    }
    normalize_feature_string(&format!("{base},{overrides}"))
}

fn llvm_host_string(raw: *mut core::ffi::c_char, label: &str) -> Result<String> {
    if raw.is_null() {
        bail!("LLVM returned a null {label}");
    }
    let value = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .trim()
        .to_string();
    let skip_dispose = should_skip_host_cpu_dispose(label);
    if skip_dispose {
    } else {
        unsafe {
            LLVMDisposeMessage(raw);
        }
    }
    if value.is_empty() {
        bail!("LLVM returned an empty {label}");
    }
    Ok(value)
}

fn should_skip_host_cpu_dispose(label: &str) -> bool {
    // LLVM's host-CPU C API returns heap-owned strings, but on Windows the
    // dispose path currently crashes inside the helper for both
    // `LLVMGetHostCPUName()` and `LLVMGetHostCPUFeatures()`. The helper is a
    // short-lived process, so we intentionally keep these two tiny buffers
    // alive until process exit rather than crashing native AOT builds.
    if cfg!(windows) && matches!(label, "host CPU name" | "host CPU features") {
        return true;
    }
    false
}

fn current_host_triple() -> Result<String> {
    let os = match std::env::consts::OS {
        "windows" => "pc-windows-msvc",
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        other => bail!("unsupported operating system `{other}`"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("unsupported architecture `{other}`"),
    };
    Ok(format!("{arch}-{os}"))
}

#[cfg(test)]
mod tests {
    use super::{merge_feature_strings, normalize_feature_string, parse_custom_cpu_spec};

    #[test]
    fn parse_custom_cpu_spec_splits_cpu_and_features() {
        let spec = parse_custom_cpu_spec("znver4,+avx2,-avx512f").unwrap();
        assert_eq!(spec.cpu, "znver4");
        assert_eq!(spec.features, "+avx2,-avx512f");
    }

    #[test]
    fn normalize_feature_string_last_override_wins() {
        let normalized = normalize_feature_string("+avx2,-fma,+fma").unwrap();
        assert_eq!(normalized, "+avx2,+fma");
    }

    #[test]
    fn merge_feature_strings_preserves_base_order_and_overrides() {
        let merged = merge_feature_strings("+avx2,-fma", "+fma,+bmi2").unwrap();
        assert_eq!(merged, "+avx2,+fma,+bmi2");
    }
}

pub fn compile_and_link_module(
    layout: &ToolchainLayout,
    backend: &BackendContext<'_>,
    request: &CompileRequest,
) -> Result<PathBuf> {
    trace("inkwell:init_targets");
    Target::initialize_all(&InitializationConfig::default());

    trace("inkwell:create_context");
    let context = Context::create();
    let module = context.create_module("fidan");
    let builder = context.create_builder();
    let target_triple = CString::new(layout.metadata.host_triple.as_str())
        .context("host triple contains an interior NUL byte")?;
    unsafe {
        LLVMSetTarget(module.as_mut_ptr(), target_triple.as_ptr());
    }

    trace("inkwell:resolve_target");
    let mut target_ref = ptr::null_mut();
    let mut target_error = ptr::null_mut();
    let target_code = unsafe {
        LLVMGetTargetFromTriple(target_triple.as_ptr(), &mut target_ref, &mut target_error)
    };
    if target_code == 1 {
        let error = unsafe {
            let error = CStr::from_ptr(target_error).to_string_lossy().into_owned();
            LLVMDisposeMessage(target_error);
            error
        };
        bail!(
            "failed to resolve LLVM target `{}`: {error}",
            layout.metadata.host_triple
        );
    }
    let target = unsafe { Target::new(target_ref) };
    trace("inkwell:resolve_target_cpu");
    let target_cpu = resolve_target_cpu(request, layout.metadata.host_triple.as_str())?;
    trace("inkwell:resolved_target_cpu");
    let cpu = CString::new(target_cpu.cpu.as_str())
        .context("target CPU string contains an interior NUL byte")?;
    let features = CString::new(target_cpu.features.as_str())
        .context("target CPU features contain an interior NUL byte")?;
    trace("inkwell:create_target_machine");
    let machine_ref = unsafe {
        LLVMCreateTargetMachine(
            target.as_mut_ptr(),
            target_triple.as_ptr(),
            cpu.as_ptr(),
            features.as_ptr(),
            map_opt_level(request.opt_level).into(),
            RelocMode::Default.into(),
            CodeModel::Default.into(),
        )
    };
    trace("inkwell:created_target_machine");
    if machine_ref.is_null() {
        bail!(
            "failed to create target machine for `{}`",
            layout.metadata.host_triple
        );
    }
    let machine = unsafe { TargetMachine::new(machine_ref) };
    trace("inkwell:set_data_layout");
    let module_target_data = unsafe {
        let target_data = LLVMCreateTargetDataLayout(machine.as_mut_ptr());
        LLVMSetModuleDataLayout(module.as_mut_ptr(), target_data);
        target_data
    };

    trace("inkwell:lower_program");
    let mut codegen = ModuleCodegen::new(&context, module, builder, backend)?;
    codegen.lower_program()?;
    if env_flag_enabled("FIDAN_LLVM_DUMP_IR") {
        trace("inkwell:dump_ir");
        let ir_path = std::env::temp_dir().join(format!(
            "fidan-llvm-dump-{}-{}.ll",
            std::process::id(),
            request
                .output
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("module")
        ));
        codegen
            .module
            .print_to_file(&ir_path)
            .map_err(|err| anyhow!("{err}"))?;
        let module_ir = fs::read_to_string(&ir_path)
            .with_context(|| format!("failed to read `{}`", ir_path.display()))?;
        let _ = fs::remove_file(&ir_path);
        dump_ir(&module_ir);
    }
    trace("inkwell:verify_module");
    let normalized_context = Context::create();
    trace("inkwell:normalize_module");
    let normalized_module = normalize_verified_module(&codegen.module, &normalized_context)?;
    optimize_module(&normalized_module, &machine, request.opt_level)?;

    let intermediate_object_path = request
        .output
        .with_extension(if cfg!(target_os = "windows") {
            "obj"
        } else {
            "o"
        });
    let mut emitted_paths = Vec::new();
    let link_input_path = match request.lto {
        LtoMode::Off => {
            trace("inkwell:write_object");
            machine
                .write_to_file(
                    &normalized_module,
                    FileType::Object,
                    &intermediate_object_path,
                )
                .with_context(|| {
                    format!(
                        "failed to emit object file `{}`",
                        intermediate_object_path.display()
                    )
                })?;
            if !request.emit_obj {
                emitted_paths.push(intermediate_object_path.clone());
            }
            intermediate_object_path.clone()
        }
        LtoMode::Full => {
            if request.emit_obj {
                trace("inkwell:write_object");
                machine
                    .write_to_file(
                        &normalized_module,
                        FileType::Object,
                        &intermediate_object_path,
                    )
                    .with_context(|| {
                        format!(
                            "failed to emit object file `{}`",
                            intermediate_object_path.display()
                        )
                    })?;
            }
            let bitcode_path = std::env::temp_dir().join(format!(
                "fidan-llvm-lto-{}-{}.bc",
                std::process::id(),
                request
                    .output
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("module")
            ));
            trace("inkwell:write_bitcode");
            write_bitcode_to_path(&normalized_module, &bitcode_path)?;
            trace("inkwell:wrote_bitcode");
            if !bitcode_path.is_file() {
                bail!(
                    "failed to emit LLVM bitcode to `{}`",
                    bitcode_path.display()
                );
            }
            emitted_paths.push(bitcode_path.clone());
            bitcode_path
        }
    };
    trace("inkwell:link_codegen_input");
    link_codegen_input(layout, request, &link_input_path, &intermediate_object_path)?;

    for path in emitted_paths {
        let _ = std::fs::remove_file(path);
    }

    let output = request.output.clone();
    trace("inkwell:done");

    trace("inkwell:drop_normalized_module");
    drop(normalized_module);
    trace("inkwell:drop_normalized_context");
    drop(normalized_context);
    trace("inkwell:drop_codegen");
    drop(codegen);
    trace("inkwell:dispose_target_data");
    unsafe {
        LLVMDisposeTargetData(module_target_data);
    }
    trace("inkwell:drop_machine");
    drop(machine);
    trace("inkwell:drop_target");
    drop(target);
    trace("inkwell:drop_target_triple");
    drop(target_triple);
    trace("inkwell:drop_context");
    drop(context);
    trace("inkwell:return");

    Ok(output)
}

fn write_bitcode_to_path(module: &Module<'_>, path: &std::path::Path) -> Result<()> {
    let buffer = unsafe {
        let memory_buffer = LLVMWriteBitcodeToMemoryBuffer(module.as_mut_ptr());
        MemoryBuffer::new(memory_buffer)
    };
    let bytes = buffer.as_slice();
    let payload = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    std::fs::write(path, payload)
        .with_context(|| format!("failed to write LLVM bitcode to `{}`", path.display()))?;
    Ok(())
}

struct ModuleCodegen<'ctx, 'a> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    backend: &'a BackendContext<'a>,
    ptr_type: inkwell::types::PointerType<'ctx>,
    i8_type: inkwell::types::IntType<'ctx>,
    i32_type: inkwell::types::IntType<'ctx>,
    i64_type: inkwell::types::IntType<'ctx>,
    f64_type: inkwell::types::FloatType<'ctx>,
    runtime: HashMap<&'static str, FunctionValue<'ctx>>,
    functions: HashMap<u32, FunctionValue<'ctx>>,
    externs: HashMap<u32, FunctionValue<'ctx>>,
    trampolines: HashMap<u32, FunctionValue<'ctx>>,
    globals: HashMap<u32, GlobalValue<'ctx>>,
    function_throw_map: HashMap<FunctionId, bool>,
    strings: BTreeMap<String, GlobalValue<'ctx>>,
    next_string_id: usize,
    next_temp_id: usize,
}

struct FunctionState<'m, 'ctx, 'a> {
    module: &'m mut ModuleCodegen<'ctx, 'a>,
    mir_function: MirFunction,
    llvm_function: FunctionValue<'ctx>,
    blocks: HashMap<u32, LlvmBlock<'ctx>>,
    locals: HashMap<u32, PointerValue<'ctx>>,
    local_types: HashMap<u32, MirTy>,
    global_namespace_map: HashMap<GlobalId, String>,
    namespace_locals: HashMap<LocalId, String>,
    current_block_id: u32,
    current_block_name: String,
    temp_index: usize,
}

impl<'ctx, 'a> ModuleCodegen<'ctx, 'a> {
    fn new(
        context: &'ctx Context,
        module: Module<'ctx>,
        builder: Builder<'ctx>,
        backend: &'a BackendContext<'a>,
    ) -> Result<Self> {
        let ptr_type = context.ptr_type(AddressSpace::default());
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let f64_type = context.f64_type();

        let mut this = Self {
            context,
            module,
            builder,
            backend,
            ptr_type,
            i8_type,
            i32_type,
            i64_type,
            f64_type,
            runtime: HashMap::new(),
            functions: HashMap::new(),
            externs: HashMap::new(),
            trampolines: HashMap::new(),
            globals: HashMap::new(),
            function_throw_map: backend.build_function_throw_map(),
            strings: BTreeMap::new(),
            next_string_id: 0,
            next_temp_id: 0,
        };
        this.declare_runtime();
        Ok(this)
    }

    fn lower_program(&mut self) -> Result<()> {
        trace("inkwell:declare_globals");
        self.declare_globals();
        trace("inkwell:declare_functions");
        self.declare_functions()?;

        let functions = self.backend.program().functions.clone();
        for function in &functions {
            trace(&format!("inkwell:lower_function:{}", function.id.0));
            self.lower_function(function)?;
        }
        trace("inkwell:emit_trampolines");
        self.emit_trampolines()?;
        trace("inkwell:emit_entry_main");
        self.emit_entry_main()?;
        Ok(())
    }

    fn declare_globals(&mut self) {
        for index in 0..self.backend.program().globals.len() {
            let global =
                self.module
                    .add_global(self.ptr_type, None, &format!("__fidan_global_{index}"));
            global.set_linkage(Linkage::Internal);
            global.set_initializer(&self.ptr_type.const_null());
            self.globals.insert(index as u32, global);
        }
    }

    fn declare_functions(&mut self) -> Result<()> {
        for function in &self.backend.program().functions {
            let name = self.backend.mangled_function_name(function)?;
            let params = vec![self.ptr_type.into(); function.params.len()];
            let fn_type = self.ptr_type.fn_type(&params, false);
            let value = self.module.add_function(&name, fn_type, None);
            self.functions.insert(function.id.0, value);
            if function.extern_decl.is_some() {
                let imported = self.declare_extern_import(function)?;
                self.externs.insert(function.id.0, imported);
            }
        }
        Ok(())
    }

    fn declare_extern_import(&mut self, function: &MirFunction) -> Result<FunctionValue<'ctx>> {
        let decl = function
            .extern_decl
            .as_ref()
            .ok_or_else(|| anyhow!("missing extern metadata for function {}", function.id.0))?;

        if let Some(existing) = self.module.get_function(&decl.symbol) {
            return Ok(existing);
        }

        let imported = match decl.abi {
            MirExternAbi::Native => {
                let params = function
                    .params
                    .iter()
                    .map(|param| self.native_extern_param_type(&param.ty))
                    .collect::<Result<Vec<_>>>()?;
                let fn_type = match function.return_ty {
                    MirTy::Nothing | MirTy::Error => {
                        self.context.void_type().fn_type(&params, false)
                    }
                    _ => self
                        .native_extern_return_type(&function.return_ty)?
                        .fn_type(&params, false),
                };
                self.module
                    .add_function(&decl.symbol, fn_type, Some(Linkage::External))
            }
            MirExternAbi::Fidan => self.module.add_function(
                &decl.symbol,
                self.ptr_type
                    .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
                Some(Linkage::External),
            ),
        };
        Ok(imported)
    }

    fn native_extern_param_type(&self, ty: &MirTy) -> Result<BasicMetadataTypeEnum<'ctx>> {
        Ok(match ty {
            MirTy::Integer | MirTy::Handle => self.i64_type.into(),
            MirTy::Float => self.f64_type.into(),
            MirTy::Boolean => self.i8_type.into(),
            other => bail!("unsupported native @extern parameter type in LLVM backend: {other:?}"),
        })
    }

    fn native_extern_return_type(&self, ty: &MirTy) -> Result<inkwell::types::BasicTypeEnum<'ctx>> {
        Ok(match ty {
            MirTy::Integer | MirTy::Handle => self.i64_type.into(),
            MirTy::Float => self.f64_type.into(),
            MirTy::Boolean => self.i8_type.into(),
            other => bail!("unsupported native @extern return type in LLVM backend: {other:?}"),
        })
    }

    fn emit_entry_main(&mut self) -> Result<()> {
        trace("inkwell:emit_entry_main:create_wrapper");
        let fn_type = self.i32_type.fn_type(&[], false);
        let main = self.module.add_function("main", fn_type, None);
        let entry = self.context.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);

        let functions = self.backend.program().functions.clone();
        let function_count = functions.len();
        if function_count > 0 {
            trace("inkwell:emit_entry_main:init_fn_table");
            self.call_runtime_void(
                "fdn_fn_table_init",
                &[self.i64_type.const_int(function_count as u64, false).into()],
            )?;

            for (index, function) in functions.iter().enumerate() {
                let trampoline = self
                    .trampolines
                    .get(&function.id.0)
                    .copied()
                    .ok_or_else(|| anyhow!("missing trampoline for function {}", function.id.0))?;
                let ptr_name = format!("trampoline.addr.{}", function.id.0);
                let trampoline_ptr = self
                    .builder
                    .build_ptr_to_int(
                        trampoline.as_global_value().as_pointer_value(),
                        self.i64_type,
                        &ptr_name,
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                self.call_runtime_void(
                    "fdn_fn_table_set",
                    &[
                        self.i64_type.const_int(index as u64, false).into(),
                        trampoline_ptr.into(),
                    ],
                )?;

                if index > 0 {
                    let name = self.backend.symbol_name(function.name)?.to_owned();
                    let (name_ptr, name_len) = self.module_string_bytes(&name);
                    self.call_runtime_void(
                        "fdn_fn_name_register",
                        &[
                            name_ptr.into(),
                            name_len.into(),
                            self.i64_type.const_int(index as u64, false).into(),
                        ],
                    )?;
                }
            }
        }

        if let Some(init) = self.backend.init_function() {
            trace(&format!(
                "inkwell:emit_entry_main:init_candidate:{}",
                init.id.0
            ));
            let function = self
                .functions
                .get(&init.id.0)
                .copied()
                .ok_or_else(|| anyhow!("missing declared init function"))?;
            trace("inkwell:emit_entry_main:call_init");
            let _ = self
                .builder
                .build_call(function, &[], "init")
                .map_err(|err| anyhow!("{err}"))?;
            trace("inkwell:emit_entry_main:called_init");

            let has_exception = self.call_runtime_i8("fdn_has_exception", &[])?;
            let has_exception = self
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    has_exception,
                    self.i8_type.const_zero(),
                    "init.has_exception",
                )
                .map_err(|err| anyhow!("{err}"))?;
            let throw_block = self.context.append_basic_block(main, "init.throw");
            let continue_block = self.context.append_basic_block(main, "init.cont");
            self.builder
                .build_conditional_branch(has_exception, throw_block, continue_block)
                .map_err(|err| anyhow!("{err}"))?;

            self.builder.position_at_end(throw_block);
            let exception = self.call_runtime_ptr("fdn_catch_exception", &[])?;
            self.call_runtime_void("fdn_throw_unhandled", &[exception.into()])?;
            self.builder
                .build_return(Some(&self.i32_type.const_int(1, false)))
                .map_err(|err| anyhow!("{err}"))?;

            self.builder.position_at_end(continue_block);
        }

        trace("inkwell:emit_entry_main:return_zero");
        self.builder
            .build_return(Some(&self.i32_type.const_zero()))
            .map_err(|err| anyhow!("{err}"))?;
        trace("inkwell:emit_entry_main:done");
        Ok(())
    }

    fn lower_function(&mut self, function: &MirFunction) -> Result<()> {
        let function_value = self
            .functions
            .get(&function.id.0)
            .copied()
            .ok_or_else(|| anyhow!("missing declared function {}", function.id.0))?;
        if function.extern_decl.is_some() {
            return self.lower_extern_wrapper(function, function_value);
        }
        let entry = self.context.append_basic_block(function_value, "entry");
        let mut blocks = HashMap::new();
        for block in &function.blocks {
            blocks.insert(
                block.id.0,
                self.context
                    .append_basic_block(function_value, &format!("bb{}", block.id.0)),
            );
        }

        self.builder.position_at_end(entry);
        let global_namespace_map = build_global_namespace_map(self.backend);
        let mut state = FunctionState::new(
            self,
            function.clone(),
            function_value,
            blocks,
            global_namespace_map,
        );
        state.initialize_entry()?;
        state.lower_blocks()?;
        Ok(())
    }

    fn lower_extern_wrapper(
        &mut self,
        function: &MirFunction,
        wrapper: FunctionValue<'ctx>,
    ) -> Result<()> {
        let decl = function
            .extern_decl
            .as_ref()
            .ok_or_else(|| anyhow!("missing extern metadata for function {}", function.id.0))?;
        let imported = self
            .externs
            .get(&function.id.0)
            .copied()
            .ok_or_else(|| anyhow!("missing imported extern function {}", function.id.0))?;

        let entry = self.context.append_basic_block(wrapper, "entry");
        self.builder.position_at_end(entry);

        let boxed_params = (0..function.params.len())
            .map(|index| {
                wrapper
                    .get_nth_param(index as u32)
                    .ok_or_else(|| anyhow!("missing extern wrapper param {index}"))
                    .map(BasicValueEnum::into_pointer_value)
            })
            .collect::<Result<Vec<_>>>()?;

        let result = match decl.abi {
            MirExternAbi::Native => self.call_native_extern(imported, function, &boxed_params)?,
            MirExternAbi::Fidan => self.call_fidan_extern(imported, &boxed_params)?,
        };

        self.builder
            .build_return(Some(&result))
            .map_err(|err| anyhow!("{err}"))?;
        Ok(())
    }

    fn call_native_extern(
        &mut self,
        imported: FunctionValue<'ctx>,
        function: &MirFunction,
        boxed_params: &[PointerValue<'ctx>],
    ) -> Result<PointerValue<'ctx>> {
        let mut args = Vec::with_capacity(function.params.len());
        for (param, boxed) in function.params.iter().zip(boxed_params.iter().copied()) {
            let value: BasicMetadataValueEnum<'ctx> = match param.ty {
                MirTy::Integer => self
                    .call_runtime_i64("fdn_unbox_int", &[boxed.into()])?
                    .into(),
                MirTy::Float => self
                    .call_runtime_f64("fdn_unbox_float", &[boxed.into()])?
                    .into(),
                MirTy::Boolean => self
                    .call_runtime_i8("fdn_unbox_bool", &[boxed.into()])?
                    .into(),
                MirTy::Handle => self
                    .call_runtime_i64("fdn_unbox_handle", &[boxed.into()])?
                    .into(),
                ref other => {
                    bail!("unsupported native @extern parameter type `{other:?}` in LLVM backend")
                }
            };
            args.push(value);
        }

        let call_name = self.temp("extern.native");
        let call = self
            .builder
            .build_call(imported, &args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;

        match function.return_ty {
            MirTy::Nothing | MirTy::Error => self.call_runtime_ptr("fdn_box_nothing", &[]),
            MirTy::Integer => {
                let value = call
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| anyhow!("expected integer native @extern result"))?
                    .into_int_value();
                self.call_runtime_ptr("fdn_box_int", &[value.into()])
            }
            MirTy::Float => {
                let value = call
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| anyhow!("expected float native @extern result"))?
                    .into_float_value();
                self.call_runtime_ptr("fdn_box_float", &[value.into()])
            }
            MirTy::Boolean => {
                let value = call
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| anyhow!("expected boolean native @extern result"))?
                    .into_int_value();
                self.call_runtime_ptr("fdn_box_bool", &[value.into()])
            }
            MirTy::Handle => {
                let value = call
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| anyhow!("expected handle native @extern result"))?
                    .into_int_value();
                self.call_runtime_ptr("fdn_box_handle", &[value.into()])
            }
            ref other => {
                bail!("unsupported native @extern return type `{other:?}` in LLVM backend")
            }
        }
    }

    fn call_fidan_extern(
        &mut self,
        imported: FunctionValue<'ctx>,
        boxed_params: &[PointerValue<'ctx>],
    ) -> Result<PointerValue<'ctx>> {
        let (args_ptr, args_cnt) = self.build_ptr_array(boxed_params)?;
        let call_name = self.temp("extern.fidan");
        let call_args: [BasicMetadataValueEnum<'ctx>; 2] = [args_ptr.into(), args_cnt.into()];
        let call = self
            .builder
            .build_call(imported, &call_args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        let raw = call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected raw Fidan ABI extern result"))?
            .into_pointer_value();
        let is_null_name = self.temp("extern.fidan.is_null");
        let is_null = self
            .builder
            .build_is_null(raw, &is_null_name)
            .map_err(|err| anyhow!("{err}"))?;
        let nothing = self.call_runtime_ptr("fdn_box_nothing", &[])?;
        let select_name = self.temp("extern.fidan.value");
        let select = self
            .builder
            .build_select(is_null, nothing, raw, &select_name)
            .map_err(|err| anyhow!("{err}"))?;
        Ok(select.into_pointer_value())
    }

    fn emit_trampolines(&mut self) -> Result<()> {
        let trampoline_type = self
            .ptr_type
            .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
        let functions = self.backend.program().functions.clone();

        for function in &functions {
            let trampoline = self.module.add_function(
                &format!("fdn_trampoline_{}", function.id.0),
                trampoline_type,
                None,
            );
            let entry = self.context.append_basic_block(trampoline, "entry");
            self.builder.position_at_end(entry);

            let args_ptr = trampoline
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("missing trampoline args pointer"))?
                .into_pointer_value();
            let args_cnt = trampoline
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("missing trampoline args count"))?
                .into_int_value();

            let mut call_args = Vec::with_capacity(function.params.len());
            for (index, param) in function.params.iter().enumerate() {
                let present_bb = self
                    .context
                    .append_basic_block(trampoline, &format!("arg{index}.present"));
                let missing_bb = self
                    .context
                    .append_basic_block(trampoline, &format!("arg{index}.missing"));
                let cont_bb = self
                    .context
                    .append_basic_block(trampoline, &format!("arg{index}.cont"));

                let has_arg = self
                    .builder
                    .build_int_compare(
                        IntPredicate::UGT,
                        args_cnt,
                        self.i64_type.const_int(index as u64, false),
                        &format!("arg{index}.has"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                self.builder
                    .build_conditional_branch(has_arg, present_bb, missing_bb)
                    .map_err(|err| anyhow!("{err}"))?;

                self.builder.position_at_end(present_bb);
                let arg_ptr = unsafe {
                    self.builder.build_in_bounds_gep(
                        self.ptr_type,
                        args_ptr,
                        &[self.i64_type.const_int(index as u64, false)],
                        &format!("arg{index}.ptr"),
                    )
                }
                .map_err(|err| anyhow!("{err}"))?;
                let present_value = self
                    .builder
                    .build_load(self.ptr_type, arg_ptr, &format!("arg{index}.value"))
                    .map_err(|err| anyhow!("{err}"))?
                    .into_pointer_value();
                let present_end = self
                    .builder
                    .get_insert_block()
                    .ok_or_else(|| anyhow!("missing present trampoline block"))?;
                self.builder
                    .build_unconditional_branch(cont_bb)
                    .map_err(|err| anyhow!("{err}"))?;

                self.builder.position_at_end(missing_bb);
                let missing_value = self.trampoline_default_value(param.default.as_ref())?;
                let missing_end = self
                    .builder
                    .get_insert_block()
                    .ok_or_else(|| anyhow!("missing missing trampoline block"))?;
                self.builder
                    .build_unconditional_branch(cont_bb)
                    .map_err(|err| anyhow!("{err}"))?;

                self.builder.position_at_end(cont_bb);
                let phi = self
                    .builder
                    .build_phi(self.ptr_type, &format!("arg{index}.phi"))
                    .map_err(|err| anyhow!("{err}"))?;
                let present_basic = present_value.as_basic_value_enum();
                let missing_basic = missing_value.as_basic_value_enum();
                phi.add_incoming(&[(&present_basic, present_end), (&missing_basic, missing_end)]);
                call_args.push(phi.as_basic_value().into_pointer_value());
            }

            let callee = self
                .functions
                .get(&function.id.0)
                .copied()
                .ok_or_else(|| anyhow!("missing declared function {}", function.id.0))?;
            let call_args = call_args
                .iter()
                .copied()
                .map(Into::into)
                .collect::<Vec<BasicMetadataValueEnum<'ctx>>>();
            let result = self.call_decl_value(callee, &call_args)?;
            self.builder
                .build_return(Some(&result))
                .map_err(|err| anyhow!("{err}"))?;

            self.trampolines.insert(function.id.0, trampoline);
        }

        Ok(())
    }

    fn trampoline_default_value(&mut self, default: Option<&MirLit>) -> Result<PointerValue<'ctx>> {
        if let Some(default) = default {
            self.box_literal(default)
        } else {
            self.call_runtime_ptr("fdn_box_nothing", &[])
        }
    }

    fn declare_runtime(&mut self) {
        self.declare_runtime_fn(
            "fdn_box_int",
            self.ptr_type.fn_type(&[self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_float",
            self.ptr_type.fn_type(&[self.f64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_bool",
            self.ptr_type.fn_type(&[self.i8_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_handle",
            self.ptr_type.fn_type(&[self.i64_type.into()], false),
        );
        self.declare_runtime_fn("fdn_box_nothing", self.ptr_type.fn_type(&[], false));
        self.declare_runtime_fn(
            "fdn_box_str",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_namespace",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_fn_ref",
            self.ptr_type.fn_type(&[self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_stdlib_fn",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_box_enum_type",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_box_class_type",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_unbox_int",
            self.i64_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_unbox_float",
            self.f64_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_unbox_bool",
            self.i8_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_unbox_handle",
            self.i64_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_truthy",
            self.i8_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_null_coalesce",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );

        for name in [
            "fdn_dyn_add",
            "fdn_dyn_sub",
            "fdn_dyn_mul",
            "fdn_dyn_div",
            "fdn_dyn_rem",
            "fdn_dyn_pow",
            "fdn_dyn_and",
            "fdn_dyn_or",
            "fdn_dyn_bit_xor",
            "fdn_dyn_bit_and",
            "fdn_dyn_bit_or",
            "fdn_dyn_shl",
            "fdn_dyn_shr",
        ] {
            self.declare_runtime_fn(
                name,
                self.ptr_type
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
            );
        }

        for name in [
            "fdn_clone",
            "fdn_dyn_not",
            "fdn_dyn_neg",
            "fdn_to_string",
            "fdn_to_integer",
            "fdn_to_float",
            "fdn_to_boolean",
            "fdn_type_name",
            "fdn_input",
        ] {
            self.declare_runtime_fn(name, self.ptr_type.fn_type(&[self.ptr_type.into()], false));
        }

        self.declare_runtime_fn(
            "fdn_drop",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into()], false),
        );

        for name in [
            "fdn_dyn_eq",
            "fdn_dyn_ne",
            "fdn_dyn_lt",
            "fdn_dyn_le",
            "fdn_dyn_gt",
            "fdn_dyn_ge",
        ] {
            self.declare_runtime_fn(
                name,
                self.i8_type
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
            );
        }

        self.declare_runtime_fn(
            "fdn_fn_name_register",
            self.context.void_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_fn_table_init",
            self.context
                .void_type()
                .fn_type(&[self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_fn_table_set",
            self.context
                .void_type()
                .fn_type(&[self.i64_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_store_exception",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_throw_unhandled",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn("fdn_has_exception", self.i8_type.fn_type(&[], false));
        self.declare_runtime_fn("fdn_catch_exception", self.ptr_type.fn_type(&[], false));
        self.declare_runtime_fn(
            "fdn_make_closure",
            self.ptr_type.fn_type(
                &[
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_spawn_expr",
            self.ptr_type.fn_type(
                &[
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_spawn_dynamic",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_spawn_task",
            self.ptr_type.fn_type(
                &[
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_spawn_concurrent",
            self.ptr_type.fn_type(
                &[
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_pending_join",
            self.ptr_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_parallel_iter_seq",
            self.context.void_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_make_range",
            self.ptr_type.fn_type(
                &[
                    self.i64_type.into(),
                    self.i64_type.into(),
                    self.i8_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_slice",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i8_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn("fdn_list_new", self.ptr_type.fn_type(&[], false));
        self.declare_runtime_fn(
            "fdn_list_push",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_tuple_pack",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_list_get",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_list_set",
            self.context.void_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn("fdn_dict_new", self.ptr_type.fn_type(&[], false));
        self.declare_runtime_fn(
            "fdn_dict_get",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_dict_set",
            self.context.void_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_dict_len",
            self.i64_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_dict_contains_key",
            self.i8_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_dict_remove",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_dict_keys",
            self.ptr_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_dict_values",
            self.ptr_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_dict_entries",
            self.ptr_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_insert",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_remove",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_contains",
            self.i8_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_to_list",
            self.ptr_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_union",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_intersect",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_hashset_diff",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_println",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_print_many",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_len",
            self.i64_type.fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_assert",
            self.context
                .void_type()
                .fn_type(&[self.i64_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_assert_eq",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_assert_ne",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_panic",
            self.context
                .void_type()
                .fn_type(&[self.ptr_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_certain_check",
            self.context.void_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_obj_new",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_obj_get_field",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_obj_set_field",
            self.context.void_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_enum_variant",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_enum_tag_check",
            self.i8_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_enum_payload",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_stdlib_call",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_call_dynamic",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
        self.declare_runtime_fn(
            "fdn_str_interp",
            self.ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false),
        );
        self.declare_runtime_fn(
            "fdn_obj_invoke",
            self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            ),
        );
    }

    fn declare_runtime_fn(
        &mut self,
        name: &'static str,
        fn_type: inkwell::types::FunctionType<'ctx>,
    ) {
        let value = self.module.add_function(name, fn_type, None);
        self.runtime.insert(name, value);
    }

    fn runtime_fn(&self, name: &'static str) -> Result<FunctionValue<'ctx>> {
        self.runtime
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("missing runtime declaration `{name}`"))
    }

    fn module_string_bytes(&mut self, value: &str) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        trace(&format!("inkwell:string_bytes:{}", value.len()));
        let global = self.intern_string_global(value);
        let array_type = self.context.const_string(value.as_bytes(), true).get_type();
        let ptr = unsafe {
            self.builder.build_in_bounds_gep(
                array_type,
                global.as_pointer_value(),
                &[self.i32_type.const_zero(), self.i32_type.const_zero()],
                "module.str.ptr",
            )
        }
        .expect("valid string gep");
        (ptr, self.i64_type.const_int(value.len() as u64, false))
    }

    fn call_runtime_ptr(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<PointerValue<'ctx>> {
        let function = self.runtime_fn(name)?;
        self.call_decl_value(function, args)
    }

    fn call_runtime_i8(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<IntValue<'ctx>> {
        let function = self.runtime_fn(name)?;
        let call_name = self.temp("module.call.i8");
        let call = self
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected i8 call result"))
            .map(BasicValueEnum::into_int_value)
    }

    fn call_runtime_i64(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<IntValue<'ctx>> {
        let function = self.runtime_fn(name)?;
        let call_name = self.temp("module.call.i64");
        let call = self
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected i64 call result"))
            .map(BasicValueEnum::into_int_value)
    }

    fn call_runtime_f64(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<inkwell::values::FloatValue<'ctx>> {
        let function = self.runtime_fn(name)?;
        let call_name = self.temp("module.call.f64");
        let call = self
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected f64 call result"))
            .map(BasicValueEnum::into_float_value)
    }

    fn call_decl_value(
        &mut self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<PointerValue<'ctx>> {
        let call = self
            .builder
            .build_call(function, args, "module.call")
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected non-void call result"))
            .map(BasicValueEnum::into_pointer_value)
    }

    fn call_runtime_void(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<()> {
        let function = self.runtime_fn(name)?;
        self.builder
            .build_call(function, args, "")
            .map_err(|err| anyhow!("{err}"))?;
        Ok(())
    }

    fn build_ptr_array(
        &mut self,
        values: &[PointerValue<'ctx>],
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>)> {
        if values.is_empty() {
            return Ok((self.ptr_type.const_null(), self.i64_type.const_zero()));
        }

        let array_type = self.ptr_type.array_type(values.len() as u32);
        let array_name = self.temp("extern.arr");
        let array = self
            .builder
            .build_alloca(array_type, &array_name)
            .map_err(|err| anyhow!("{err}"))?;
        let zero = self.i32_type.const_zero();

        for (index, value) in values.iter().enumerate() {
            let elt_name = self.temp("extern.arr.elt");
            let gep = unsafe {
                self.builder.build_in_bounds_gep(
                    array_type,
                    array,
                    &[zero, self.i32_type.const_int(index as u64, false)],
                    &elt_name,
                )
            }
            .map_err(|err| anyhow!("{err}"))?;
            self.builder
                .build_store(gep, *value)
                .map_err(|err| anyhow!("{err}"))?;
        }

        let first_name = self.temp("extern.arr.ptr");
        let first = unsafe {
            self.builder
                .build_in_bounds_gep(array_type, array, &[zero, zero], &first_name)
        }
        .map_err(|err| anyhow!("{err}"))?;

        Ok((first, self.i64_type.const_int(values.len() as u64, false)))
    }

    fn box_literal(&mut self, literal: &MirLit) -> Result<PointerValue<'ctx>> {
        match literal {
            MirLit::Int(value) => self.call_runtime_ptr(
                "fdn_box_int",
                &[self.i64_type.const_int(*value as u64, true).into()],
            ),
            MirLit::Float(value) => {
                self.call_runtime_ptr("fdn_box_float", &[self.f64_type.const_float(*value).into()])
            }
            MirLit::Bool(value) => self.call_runtime_ptr(
                "fdn_box_bool",
                &[self
                    .i8_type
                    .const_int(if *value { 1 } else { 0 }, false)
                    .into()],
            ),
            MirLit::Str(value) => {
                let (ptr, len) = self.module_string_bytes(value);
                self.call_runtime_ptr("fdn_box_str", &[ptr.into(), len.into()])
            }
            MirLit::Nothing => self.call_runtime_ptr("fdn_box_nothing", &[]),
            MirLit::FunctionRef(function_id) => self.call_runtime_ptr(
                "fdn_box_fn_ref",
                &[self.i64_type.const_int(*function_id as u64, false).into()],
            ),
            MirLit::Namespace(value) => {
                let (ptr, len) = self.module_string_bytes(value);
                self.call_runtime_ptr("fdn_box_namespace", &[ptr.into(), len.into()])
            }
            MirLit::StdlibFn { module, name } => {
                let (module_ptr, module_len) = self.module_string_bytes(module);
                let (fn_ptr, fn_len) = self.module_string_bytes(name);
                self.call_runtime_ptr(
                    "fdn_box_stdlib_fn",
                    &[
                        module_ptr.into(),
                        module_len.into(),
                        fn_ptr.into(),
                        fn_len.into(),
                    ],
                )
            }
            MirLit::EnumType(value) => {
                let (ptr, len) = self.module_string_bytes(value);
                self.call_runtime_ptr("fdn_box_enum_type", &[ptr.into(), len.into()])
            }
            MirLit::ClassType(value) => {
                let (ptr, len) = self.module_string_bytes(value);
                self.call_runtime_ptr("fdn_box_class_type", &[ptr.into(), len.into()])
            }
        }
    }

    fn intern_string_global(&mut self, value: &str) -> GlobalValue<'ctx> {
        if let Some(existing) = self.strings.get(value) {
            return *existing;
        }
        let name = format!(".str.{}", self.next_string_id);
        self.next_string_id += 1;
        let constant = self.context.const_string(value.as_bytes(), true);
        let global = self.module.add_global(constant.get_type(), None, &name);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_initializer(&constant);
        self.strings.insert(value.to_owned(), global);
        global
    }

    fn temp(&mut self, prefix: &str) -> String {
        let name = format!("{prefix}.{}", self.next_temp_id);
        self.next_temp_id += 1;
        name
    }
}

impl<'m, 'ctx, 'a> FunctionState<'m, 'ctx, 'a> {
    fn new(
        module: &'m mut ModuleCodegen<'ctx, 'a>,
        mir_function: MirFunction,
        llvm_function: FunctionValue<'ctx>,
        blocks: HashMap<u32, LlvmBlock<'ctx>>,
        global_namespace_map: HashMap<GlobalId, String>,
    ) -> Self {
        let local_types = module.backend.build_local_type_map(&mir_function);
        Self {
            module,
            mir_function,
            llvm_function,
            blocks,
            locals: HashMap::new(),
            local_types,
            global_namespace_map,
            namespace_locals: HashMap::new(),
            current_block_id: 0,
            current_block_name: "entry".to_owned(),
            temp_index: 0,
        }
    }

    fn initialize_entry(&mut self) -> Result<()> {
        for local in 0..self.mir_function.local_count {
            let local_id = LocalId(local);
            let slot_ty = self.local_storage_type(local_id);
            let slot = self
                .module
                .builder
                .build_alloca(slot_ty, &format!("local{local}"))
                .map_err(|err| anyhow!("{err}"))?;
            self.store_slot_default(local_id, slot)?;
            self.locals.insert(local, slot);
        }

        let params = self.mir_function.params.clone();
        for (index, param) in params.iter().enumerate() {
            let arg = self
                .llvm_function
                .get_nth_param(index as u32)
                .ok_or_else(|| anyhow!("missing LLVM param {index}"))?
                .into_pointer_value();
            self.store_local_boxed(param.local, arg)?;
        }

        if let Some(first) = self.mir_function.blocks.first() {
            let block = self.block(first.id.0)?;
            self.module
                .builder
                .build_unconditional_branch(block)
                .map_err(|err| anyhow!("{err}"))?;
        } else {
            let nothing = self.call_ptr("fdn_box_nothing", &[])?;
            self.module
                .builder
                .build_return(Some(&nothing))
                .map_err(|err| anyhow!("{err}"))?;
        }
        Ok(())
    }

    fn lower_blocks(&mut self) -> Result<()> {
        let blocks = self.mir_function.blocks.clone();
        let entry_catch_stacks = compute_catch_stacks(&self.mir_function);
        for block in &blocks {
            self.current_block_id = block.id.0;
            self.current_block_name = format!("bb{}", block.id.0);
            self.namespace_locals.clear();
            let llvm_block = self.block(block.id.0)?;
            self.module.builder.position_at_end(llvm_block);
            let mut current_catch_stack = entry_catch_stacks[block.id.0 as usize].clone();

            for instruction in &block.instructions {
                match instruction {
                    Instr::PushCatch(target) => {
                        current_catch_stack.push(*target);
                        continue;
                    }
                    Instr::PopCatch => {
                        current_catch_stack.pop();
                        continue;
                    }
                    _ => {}
                }
                self.lower_instruction(instruction, &current_catch_stack)?;
            }
            self.lower_terminator(&block.terminator, &current_catch_stack)?;
        }
        Ok(())
    }

    fn lower_instruction(
        &mut self,
        instruction: &Instr,
        current_catch_stack: &[BlockId],
    ) -> Result<()> {
        match instruction {
            Instr::Assign { dest, rhs, ty: _ } => {
                let effective_ty = self.local_type(*dest);
                if is_native_scalar_ty(&effective_ty) {
                    let value = self.lower_native_rvalue(rhs, &effective_ty)?;
                    self.store_local_native(*dest, value)?;
                } else {
                    let value = self.lower_rvalue(rhs)?;
                    self.store_local_boxed(*dest, value)?;
                }
                self.namespace_locals.remove(dest);
                match rhs {
                    Rvalue::Literal(MirLit::Namespace(namespace)) => {
                        self.namespace_locals.insert(*dest, namespace.clone());
                    }
                    Rvalue::Use(Operand::Local(source)) => {
                        if let Some(namespace) = self.namespace_locals.get(source).cloned() {
                            self.namespace_locals.insert(*dest, namespace);
                        }
                    }
                    _ => {}
                }
                if let Rvalue::Call { callee, args } = rhs
                    && self.call_may_throw(callee, args)?
                {
                    self.emit_pending_exception_check(current_catch_stack)?;
                }
                Ok(())
            }
            Instr::Call {
                dest, callee, args, ..
            } => {
                if let Some(dest) = dest {
                    let effective_ty = self.local_type(*dest);
                    if is_native_scalar_ty(&effective_ty) {
                        if let Some(value) = self.lower_native_call(callee, args, &effective_ty)? {
                            self.store_local_native(*dest, value)?;
                        } else {
                            let value = self.lower_call(callee, args)?;
                            self.store_local_boxed(*dest, value)?;
                        }
                    } else {
                        let value = self.lower_call(callee, args)?;
                        self.store_local_boxed(*dest, value)?;
                    }
                    self.namespace_locals.remove(dest);
                } else {
                    let _ = self.lower_call(callee, args)?;
                }
                if self.call_may_throw(callee, args)? {
                    self.emit_pending_exception_check(current_catch_stack)?;
                }
                Ok(())
            }
            Instr::SetField {
                object,
                field,
                value,
            } => {
                let object = self.lower_operand(object)?;
                let value = self.lower_operand(value)?;
                let (field_ptr, field_len) = self.string_ptr(*field)?;
                self.call_void(
                    "fdn_obj_set_field",
                    &[
                        object.into(),
                        field_ptr.into(),
                        field_len.into(),
                        value.into(),
                    ],
                )
            }
            Instr::GetField {
                dest,
                object,
                field,
            } => {
                let field_name = self.module.backend.symbol_name(*field)?;
                let stdlib_namespace = match object {
                    Operand::Local(local) => self.namespace_locals.get(local).cloned(),
                    Operand::Const(MirLit::Namespace(namespace)) => Some(namespace.clone()),
                    _ => None,
                };
                let value = if let Some(namespace) = stdlib_namespace
                    && fidan_stdlib::module_exports(namespace.as_str()).contains(&field_name)
                {
                    let (module_ptr, module_len) = self.string_bytes(namespace.as_str());
                    let (field_ptr, field_len) = self.string_ptr(*field)?;
                    self.call_ptr(
                        "fdn_box_stdlib_fn",
                        &[
                            module_ptr.into(),
                            module_len.into(),
                            field_ptr.into(),
                            field_len.into(),
                        ],
                    )?
                } else {
                    let object = self.lower_operand(object)?;
                    let (field_ptr, field_len) = self.string_ptr(*field)?;
                    self.call_ptr(
                        "fdn_obj_get_field",
                        &[object.into(), field_ptr.into(), field_len.into()],
                    )?
                };
                self.store_local_boxed(*dest, value)?;
                self.namespace_locals.remove(dest);
                Ok(())
            }
            Instr::GetIndex {
                dest,
                object,
                index,
            } => {
                let runtime_fn = match self.operand_type(object) {
                    MirTy::Dict(_, _) => "fdn_dict_get",
                    _ => "fdn_list_get",
                };
                let object = self.lower_operand(object)?;
                let index = self.lower_operand(index)?;
                let value = self.call_ptr(runtime_fn, &[object.into(), index.into()])?;
                self.store_local_boxed(*dest, value)?;
                self.namespace_locals.remove(dest);
                Ok(())
            }
            Instr::SetIndex {
                object,
                index,
                value,
            } => {
                let runtime_fn = match self.operand_type(object) {
                    MirTy::Dict(_, _) => "fdn_dict_set",
                    _ => "fdn_list_set",
                };
                let object = self.lower_operand(object)?;
                let index = self.lower_operand(index)?;
                let value = self.lower_operand(value)?;
                self.call_void(runtime_fn, &[object.into(), index.into(), value.into()])
            }
            Instr::Drop { local } => {
                let local_ty = self.local_type(*local);
                if !is_native_scalar_ty(&local_ty)
                    && !matches!(local_ty, MirTy::Nothing | MirTy::Error)
                {
                    let value = self.lower_operand(&Operand::Local(*local))?;
                    self.call_void("fdn_drop", &[value.into()])?;
                }
                let slot = self.slot(*local)?;
                self.store_slot_default(*local, slot)
            }
            Instr::Nop | Instr::PushCatch(..) | Instr::PopCatch => Ok(()),
            Instr::CertainCheck { operand, name } => {
                let operand_ty = self.operand_type(operand);
                if !matches!(operand_ty, MirTy::Dynamic | MirTy::Error | MirTy::Nothing) {
                    return Ok(());
                }
                let operand = self.lower_operand(operand)?;
                let (name_ptr, name_len) = self.string_ptr(*name)?;
                self.call_void(
                    "fdn_certain_check",
                    &[operand.into(), name_ptr.into(), name_len.into()],
                )
            }
            Instr::LoadGlobal { dest, global } => {
                let global_id = *global;
                let global = self.global(global_id)?;
                let name = self.temp("gload");
                let value = self
                    .module
                    .builder
                    .build_load(self.module.ptr_type, global.as_pointer_value(), &name)
                    .map_err(|err| anyhow!("{err}"))?
                    .into_pointer_value();
                let cloned = self.call_ptr("fdn_clone", &[value.into()])?;
                self.store_local_boxed(*dest, cloned)?;
                if let Some(namespace) = self.global_namespace_map.get(&global_id).cloned() {
                    self.namespace_locals.insert(*dest, namespace);
                } else {
                    self.namespace_locals.remove(dest);
                }
                Ok(())
            }
            Instr::StoreGlobal { global, value } => {
                let value = self.lower_operand(value)?;
                let global = self.global(*global)?;
                let current_name = self.temp("gcurrent");
                let current = self
                    .module
                    .builder
                    .build_load(
                        self.module.ptr_type,
                        global.as_pointer_value(),
                        &current_name,
                    )
                    .map_err(|err| anyhow!("{err}"))?
                    .into_pointer_value();
                self.call_void("fdn_drop", &[current.into()])?;
                let cloned = self.call_ptr("fdn_clone", &[value.into()])?;
                self.module
                    .builder
                    .build_store(global.as_pointer_value(), cloned)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::SpawnExpr {
                dest,
                task_fn,
                args,
            } => {
                let values = args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&values)?;
                let pending = self.call_ptr(
                    "fdn_spawn_expr",
                    &[
                        self.module
                            .i64_type
                            .const_int(task_fn.0 as u64, false)
                            .into(),
                        array_ptr.into(),
                        count.into(),
                    ],
                )?;
                self.store_local_boxed(*dest, pending)?;
                Ok(())
            }
            Instr::SpawnConcurrent {
                handle,
                task_fn,
                args,
            } => {
                let values = args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&values)?;
                let task_name = self
                    .module
                    .backend
                    .symbol_name(self.module.backend.program().function(*task_fn).name)?
                    .to_owned();
                let (name_ptr, name_len) = self.string_bytes(&task_name);
                let pending = self.call_ptr(
                    "fdn_spawn_concurrent",
                    &[
                        self.module
                            .i64_type
                            .const_int(task_fn.0 as u64, false)
                            .into(),
                        name_ptr.into(),
                        name_len.into(),
                        array_ptr.into(),
                        count.into(),
                    ],
                )?;
                self.store_local_boxed(*handle, pending)?;
                Ok(())
            }
            Instr::SpawnParallel {
                handle,
                task_fn,
                args,
            } => {
                let values = args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&values)?;
                let task_name = self
                    .module
                    .backend
                    .symbol_name(self.module.backend.program().function(*task_fn).name)?
                    .to_owned();
                let (name_ptr, name_len) = self.string_bytes(&task_name);
                let pending = self.call_ptr(
                    "fdn_spawn_task",
                    &[
                        self.module
                            .i64_type
                            .const_int(task_fn.0 as u64, false)
                            .into(),
                        name_ptr.into(),
                        name_len.into(),
                        array_ptr.into(),
                        count.into(),
                    ],
                )?;
                self.store_local_boxed(*handle, pending)?;
                Ok(())
            }
            Instr::JoinAll { handles } => {
                for handle in handles {
                    let handle_value = self.lower_operand(&Operand::Local(*handle))?;
                    let resolved = self.call_ptr("fdn_pending_join", &[handle_value.into()])?;
                    self.store_local_boxed(*handle, resolved)?;
                    self.emit_pending_exception_check(current_catch_stack)?;
                }
                Ok(())
            }
            Instr::AwaitPending { dest, handle } => {
                let handle_value = self.lower_operand(handle)?;
                let resolved = self.call_ptr("fdn_pending_join", &[handle_value.into()])?;
                self.store_local_boxed(*dest, resolved)?;
                self.emit_pending_exception_check(current_catch_stack)?;
                Ok(())
            }
            Instr::SpawnDynamic { dest, method, args } => {
                let (first, rest, method_ptr, method_len) = if let Some(method) = method {
                    let receiver = self.lower_operand(&args[0])?;
                    let (method_ptr, method_len) = self.string_ptr(*method)?;
                    (receiver, &args[1..], method_ptr, method_len)
                } else {
                    (
                        self.lower_operand(&args[0])?,
                        &args[1..],
                        self.module.ptr_type.const_null(),
                        self.module.i64_type.const_zero(),
                    )
                };
                let call_args = rest
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<PointerValue<'ctx>>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&call_args)?;
                let result = self.call_ptr(
                    "fdn_spawn_dynamic",
                    &[
                        first.into(),
                        method_ptr.into(),
                        method_len.into(),
                        array_ptr.into(),
                        count.into(),
                    ],
                )?;
                self.store_local_boxed(*dest, result)?;
                Ok(())
            }
            Instr::ParallelIter {
                collection,
                body_fn,
                closure_args,
            } => {
                let collection = self.lower_operand(collection)?;
                let closure_values = closure_args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (env_ptr, env_count) = self.build_ptr_array(&closure_values)?;
                self.call_void(
                    "fdn_parallel_iter_seq",
                    &[
                        collection.into(),
                        self.module
                            .i64_type
                            .const_int(body_fn.0 as u64, false)
                            .into(),
                        env_ptr.into(),
                        env_count.into(),
                    ],
                )?;
                self.emit_pending_exception_check(current_catch_stack)?;
                Ok(())
            }
        }
    }

    fn lower_terminator(
        &mut self,
        terminator: &Terminator,
        current_catch_stack: &[BlockId],
    ) -> Result<()> {
        match terminator {
            Terminator::Return(None) => {
                let nothing = self.call_ptr("fdn_box_nothing", &[])?;
                self.module
                    .builder
                    .build_return(Some(&nothing))
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Terminator::Return(Some(operand)) => {
                let value = self.lower_operand(operand)?;
                self.module
                    .builder
                    .build_return(Some(&value))
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Terminator::Goto(target) => {
                let target = self.prepare_edge_to(*target)?;
                self.module
                    .builder
                    .build_unconditional_branch(target)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            } => {
                let cmp = self.lower_branch_condition(cond)?;
                let then_target = self.prepare_edge_to(*then_bb)?;
                let else_target = self.prepare_edge_to(*else_bb)?;
                self.module
                    .builder
                    .build_conditional_branch(cmp, then_target, else_target)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Terminator::Unreachable => {
                self.module
                    .builder
                    .build_unreachable()
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Terminator::Throw { value } => {
                let value = self.lower_operand(value)?;
                self.call_void("fdn_store_exception", &[value.into()])?;
                if let Some(catch_block) = current_catch_stack.last() {
                    let target = self.prepare_edge_to(*catch_block)?;
                    self.module
                        .builder
                        .build_unconditional_branch(target)
                        .map_err(|err| anyhow!("{err}"))?;
                } else {
                    self.emit_exceptional_return()?;
                }
                Ok(())
            }
        }
    }

    fn emit_pending_exception_check(&mut self, current_catch_stack: &[BlockId]) -> Result<()> {
        let has_exception = self.call_i8("fdn_has_exception", &[])?;
        let cmp_name = self.temp("has_exception");
        let cmp = self
            .module
            .builder
            .build_int_compare(
                IntPredicate::NE,
                has_exception,
                self.module.i8_type.const_zero(),
                &cmp_name,
            )
            .map_err(|err| anyhow!("{err}"))?;

        let pending_block = self.module.context.append_basic_block(
            self.llvm_function,
            &format!("{}._pending_exn", self.current_block_name),
        );
        let cont_block = self.module.context.append_basic_block(
            self.llvm_function,
            &format!("{}._cont", self.current_block_name),
        );
        self.module
            .builder
            .build_conditional_branch(cmp, pending_block, cont_block)
            .map_err(|err| anyhow!("{err}"))?;

        self.module.builder.position_at_end(pending_block);
        if let Some(catch_block) = current_catch_stack.last() {
            let target = self.prepare_edge_to(*catch_block)?;
            self.module
                .builder
                .build_unconditional_branch(target)
                .map_err(|err| anyhow!("{err}"))?;
        } else {
            self.emit_exceptional_return()?;
        }

        self.module.builder.position_at_end(cont_block);
        Ok(())
    }

    fn emit_exceptional_return(&mut self) -> Result<()> {
        let nothing = self.call_ptr("fdn_box_nothing", &[])?;
        self.module
            .builder
            .build_return(Some(&nothing))
            .map_err(|err| anyhow!("{err}"))?;
        Ok(())
    }

    fn lower_branch_condition(
        &mut self,
        cond: &Operand,
    ) -> Result<inkwell::values::IntValue<'ctx>> {
        match self.operand_type(cond) {
            MirTy::Boolean => {
                let value = self
                    .lower_native_operand(cond, &MirTy::Boolean)?
                    .into_int_value();
                let name = self.temp("br.bool");
                self.module
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        value,
                        self.module.i8_type.const_zero(),
                        &name,
                    )
                    .map_err(|err| anyhow!("{err}"))
            }
            MirTy::Integer | MirTy::Handle => {
                let value = self
                    .lower_native_operand(cond, &MirTy::Integer)?
                    .into_int_value();
                let name = self.temp("br.int");
                self.module
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        value,
                        self.module.i64_type.const_zero(),
                        &name,
                    )
                    .map_err(|err| anyhow!("{err}"))
            }
            MirTy::Float => {
                let value = self
                    .lower_native_operand(cond, &MirTy::Float)?
                    .into_float_value();
                let name = self.temp("br.float");
                self.module
                    .builder
                    .build_float_compare(
                        inkwell::FloatPredicate::ONE,
                        value,
                        self.module.f64_type.const_zero(),
                        &name,
                    )
                    .map_err(|err| anyhow!("{err}"))
            }
            _ => {
                let cond = self.lower_operand(cond)?;
                let truthy = self.call_i8("fdn_truthy", &[cond.into()])?;
                let name = self.temp("br.dyn");
                self.module
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        truthy,
                        self.module.i8_type.const_zero(),
                        &name,
                    )
                    .map_err(|err| anyhow!("{err}"))
            }
        }
    }

    fn prepare_edge_to(&mut self, target: BlockId) -> Result<LlvmBlock<'ctx>> {
        if !self.target_has_phis(target) {
            return self.block(target.0);
        }

        let current = self
            .module
            .builder
            .get_insert_block()
            .ok_or_else(|| anyhow!("missing current LLVM block while preparing phi edge"))?;
        let target_block = self.block(target.0)?;
        let edge = self.module.context.append_basic_block(
            self.llvm_function,
            &format!("edge{}_to_{}", self.current_block_id, target.0),
        );
        self.module.builder.position_at_end(edge);
        self.emit_phi_assignments(target)?;
        self.module
            .builder
            .build_unconditional_branch(target_block)
            .map_err(|err| anyhow!("{err}"))?;
        self.module.builder.position_at_end(current);
        Ok(edge)
    }

    fn target_has_phis(&self, target: BlockId) -> bool {
        self.mir_function
            .blocks
            .iter()
            .find(|block| block.id == target)
            .map(|block| !block.phis.is_empty())
            .unwrap_or(false)
    }

    fn emit_phi_assignments(&mut self, target: BlockId) -> Result<()> {
        let target_block = self
            .mir_function
            .blocks
            .iter()
            .find(|block| block.id == target)
            .cloned()
            .ok_or_else(|| anyhow!("missing MIR target block {}", target.0))?;

        for phi in &target_block.phis {
            let incoming = phi
                .operands
                .iter()
                .find(|(pred, _)| pred.0 == self.current_block_id)
                .map(|(_, operand)| operand.clone());
            if is_native_scalar_ty(&phi.ty) {
                let value = match incoming {
                    Some(operand) => self.lower_native_operand(&operand, &phi.ty)?,
                    None => native_zero(self.module, &phi.ty)?.into(),
                };
                self.store_local_native(phi.result, value)?;
            } else {
                let value = match incoming {
                    Some(operand) => self.lower_operand(&operand)?,
                    None => self.call_ptr("fdn_box_nothing", &[])?,
                };
                self.store_local_boxed(phi.result, value)?;
            }
        }

        Ok(())
    }

    fn lower_rvalue(&mut self, rhs: &Rvalue) -> Result<PointerValue<'ctx>> {
        match rhs {
            Rvalue::Use(operand) => {
                let value = self.lower_operand(operand)?;
                match operand {
                    Operand::Local(local) if !is_native_scalar_ty(&self.local_type(*local)) => {
                        self.call_ptr("fdn_clone", &[value.into()])
                    }
                    _ => Ok(value),
                }
            }
            Rvalue::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            Rvalue::Unary { op, operand } => self.lower_unary(*op, operand),
            Rvalue::NullCoalesce { lhs, rhs } => {
                let lhs = self.lower_operand(lhs)?;
                let rhs = self.lower_operand(rhs)?;
                self.call_ptr("fdn_null_coalesce", &[lhs.into(), rhs.into()])
            }
            Rvalue::Call { callee, args } => self.lower_call(callee, args),
            Rvalue::Construct { ty, fields } => self.lower_construct(*ty, fields),
            Rvalue::List(values) => self.lower_list(values),
            Rvalue::Dict(pairs) => self.lower_dict(pairs),
            Rvalue::Tuple(values) => self.lower_tuple(values),
            Rvalue::StringInterp(parts) => self.lower_string_interp(parts),
            Rvalue::Literal(literal) => self.lower_literal(literal),
            Rvalue::CatchException => self.call_ptr("fdn_catch_exception", &[]),
            Rvalue::MakeClosure { fn_id, captures } => {
                let fn_id = self.module.i64_type.const_int(*fn_id as u64, false);
                if captures.is_empty() {
                    self.call_ptr("fdn_box_fn_ref", &[fn_id.into()])
                } else {
                    let captures = captures
                        .iter()
                        .map(|capture| self.lower_operand(capture))
                        .collect::<Result<Vec<_>>>()?;
                    let (array_ptr, count) = self.build_ptr_array(&captures)?;
                    self.call_ptr(
                        "fdn_make_closure",
                        &[fn_id.into(), array_ptr.into(), count.into()],
                    )
                }
            }
            Rvalue::Slice {
                target,
                start,
                end,
                inclusive,
                step,
            } => self.lower_slice(
                target,
                start.as_ref(),
                end.as_ref(),
                *inclusive,
                step.as_ref(),
            ),
            Rvalue::ConstructEnum { tag, payload } => self.lower_enum_construct(*tag, payload),
            Rvalue::EnumTagCheck {
                value,
                expected_tag,
            } => self.lower_enum_tag_check(value, *expected_tag),
            Rvalue::EnumPayload { value, index } => self.lower_enum_payload(value, *index),
        }
    }

    fn container_receiver_kind(&self, receiver: &Operand) -> Option<ReceiverBuiltinKind> {
        match self.operand_type(receiver) {
            MirTy::Dict(_, _) => Some(ReceiverBuiltinKind::Dict),
            MirTy::HashSet(_) => Some(ReceiverBuiltinKind::HashSet),
            _ => None,
        }
    }

    fn lower_container_method_call(
        &mut self,
        receiver: &Operand,
        method_name: &str,
        args: &[Operand],
    ) -> Result<Option<PointerValue<'ctx>>> {
        let Some(receiver_kind) = self.container_receiver_kind(receiver) else {
            return Ok(None);
        };
        let Some(operation) =
            infer_receiver_member(receiver_kind, method_name).and_then(|info| info.operation)
        else {
            return Ok(None);
        };

        let receiver = self.lower_operand(receiver)?;
        let boxed = match (receiver_kind, operation) {
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Len) => {
                let len = self.call_i64("fdn_dict_len", &[receiver.into()])?;
                self.call_ptr("fdn_box_int", &[len.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::IsEmpty) => {
                let len = self.call_i64("fdn_dict_len", &[receiver.into()])?;
                let is_empty = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        len,
                        self.i64_type.const_zero(),
                        &self.temp("dict.is_empty"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                let is_empty = self
                    .builder
                    .build_int_z_extend(is_empty, self.i8_type, &self.temp("dict.is_empty.i8"))
                    .map_err(|err| anyhow!("{err}"))?;
                self.call_ptr("fdn_box_bool", &[is_empty.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Get) => {
                let Some(key) = args.first() else {
                    return self.call_ptr("fdn_box_nothing", &[]).map(Some);
                };
                let key = self.lower_operand(key)?;
                self.call_ptr("fdn_dict_get", &[receiver.into(), key.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Set) => {
                if let (Some(key), Some(value)) = (args.first(), args.get(1)) {
                    let key = self.lower_operand(key)?;
                    let value = self.lower_operand(value)?;
                    self.call_void("fdn_dict_set", &[receiver.into(), key.into(), value.into()])?;
                }
                self.call_ptr("fdn_box_nothing", &[])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Contains) => {
                let contains = if let Some(key) = args.first() {
                    let key = self.lower_operand(key)?;
                    self.call_i8("fdn_dict_contains_key", &[receiver.into(), key.into()])?
                } else {
                    self.i8_type.const_zero()
                };
                self.call_ptr("fdn_box_bool", &[contains.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Remove) => {
                if let Some(key) = args.first() {
                    let key = self.lower_operand(key)?;
                    self.call_void("fdn_dict_remove", &[receiver.into(), key.into()])?;
                }
                self.call_ptr("fdn_box_nothing", &[])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Keys) => {
                self.call_ptr("fdn_dict_keys", &[receiver.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Values) => {
                self.call_ptr("fdn_dict_values", &[receiver.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Entries) => {
                self.call_ptr("fdn_dict_entries", &[receiver.into()])?
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::ToString) => {
                self.call_ptr("fdn_to_string", &[receiver.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Len) => {
                let len = self.call_i64("fdn_len", &[receiver.into()])?;
                self.call_ptr("fdn_box_int", &[len.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::IsEmpty) => {
                let len = self.call_i64("fdn_len", &[receiver.into()])?;
                let is_empty = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        len,
                        self.i64_type.const_zero(),
                        &self.temp("hashset.is_empty"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                let is_empty = self
                    .builder
                    .build_int_z_extend(is_empty, self.i8_type, &self.temp("hashset.is_empty.i8"))
                    .map_err(|err| anyhow!("{err}"))?;
                self.call_ptr("fdn_box_bool", &[is_empty.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Insert) => {
                if let Some(value) = args.first() {
                    let value = self.lower_operand(value)?;
                    self.call_void("fdn_hashset_insert", &[receiver.into(), value.into()])?;
                }
                self.call_ptr("fdn_box_nothing", &[])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Remove) => {
                if let Some(value) = args.first() {
                    let value = self.lower_operand(value)?;
                    self.call_void("fdn_hashset_remove", &[receiver.into(), value.into()])?;
                }
                self.call_ptr("fdn_box_nothing", &[])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Contains) => {
                let contains = if let Some(value) = args.first() {
                    let value = self.lower_operand(value)?;
                    self.call_i8("fdn_hashset_contains", &[receiver.into(), value.into()])?
                } else {
                    self.i8_type.const_zero()
                };
                self.call_ptr("fdn_box_bool", &[contains.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::ToList) => {
                self.call_ptr("fdn_hashset_to_list", &[receiver.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Union) => {
                let Some(other) = args.first() else {
                    return self.call_ptr("fdn_box_nothing", &[]).map(Some);
                };
                let other = self.lower_operand(other)?;
                self.call_ptr("fdn_hashset_union", &[receiver.into(), other.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Intersect) => {
                let Some(other) = args.first() else {
                    return self.call_ptr("fdn_box_nothing", &[]).map(Some);
                };
                let other = self.lower_operand(other)?;
                self.call_ptr("fdn_hashset_intersect", &[receiver.into(), other.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Diff) => {
                let Some(other) = args.first() else {
                    return self.call_ptr("fdn_box_nothing", &[]).map(Some);
                };
                let other = self.lower_operand(other)?;
                self.call_ptr("fdn_hashset_diff", &[receiver.into(), other.into()])?
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::ToString) => {
                self.call_ptr("fdn_to_string", &[receiver.into()])?
            }
            _ => return Ok(None),
        };

        Ok(Some(boxed))
    }

    fn lower_native_container_method_call(
        &mut self,
        receiver: &Operand,
        method_name: &str,
        args: &[Operand],
        ty: &MirTy,
    ) -> Result<Option<BasicValueEnum<'ctx>>> {
        let Some(receiver_kind) = self.container_receiver_kind(receiver) else {
            return Ok(None);
        };
        let Some(operation) =
            infer_receiver_member(receiver_kind, method_name).and_then(|info| info.operation)
        else {
            return Ok(None);
        };
        let receiver = self.lower_operand(receiver)?;

        match (receiver_kind, operation, ty) {
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Len, MirTy::Integer) => Ok(Some(
                self.call_i64("fdn_dict_len", &[receiver.into()])?.into(),
            )),
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::IsEmpty, MirTy::Boolean) => {
                let len = self.call_i64("fdn_dict_len", &[receiver.into()])?;
                let is_empty = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        len,
                        self.i64_type.const_zero(),
                        &self.temp("dict.is_empty.native"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                let is_empty = self
                    .builder
                    .build_int_z_extend(
                        is_empty,
                        self.i8_type,
                        &self.temp("dict.is_empty.native.i8"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(Some(is_empty.into()))
            }
            (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Contains, MirTy::Boolean) => {
                let contains = if let Some(key) = args.first() {
                    let key = self.lower_operand(key)?;
                    self.call_i8("fdn_dict_contains_key", &[receiver.into(), key.into()])?
                } else {
                    self.i8_type.const_zero()
                };
                Ok(Some(contains.into()))
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Len, MirTy::Integer) => {
                Ok(Some(self.call_i64("fdn_len", &[receiver.into()])?.into()))
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::IsEmpty, MirTy::Boolean) => {
                let len = self.call_i64("fdn_len", &[receiver.into()])?;
                let is_empty = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        len,
                        self.i64_type.const_zero(),
                        &self.temp("hashset.is_empty.native"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                let is_empty = self
                    .builder
                    .build_int_z_extend(
                        is_empty,
                        self.i8_type,
                        &self.temp("hashset.is_empty.native.i8"),
                    )
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(Some(is_empty.into()))
            }
            (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Contains, MirTy::Boolean) => {
                let contains = if let Some(value) = args.first() {
                    let value = self.lower_operand(value)?;
                    self.call_i8("fdn_hashset_contains", &[receiver.into(), value.into()])?
                } else {
                    self.i8_type.const_zero()
                };
                Ok(Some(contains.into()))
            }
            _ => Ok(None),
        }
    }

    fn lower_call(&mut self, callee: &Callee, args: &[Operand]) -> Result<PointerValue<'ctx>> {
        match callee {
            Callee::Fn(function_id) => {
                let function = self
                    .module
                    .functions
                    .get(&function_id.0)
                    .copied()
                    .ok_or_else(|| anyhow!("missing declared function {}", function_id.0))?;
                let mir_function = self.module.backend.program().function(*function_id);
                let mut call_args =
                    Vec::<BasicMetadataValueEnum<'ctx>>::with_capacity(mir_function.params.len());
                for (index, param) in mir_function.params.iter().enumerate() {
                    let value = if let Some(arg) = args.get(index) {
                        self.lower_operand(arg)?
                    } else {
                        self.module
                            .trampoline_default_value(param.default.as_ref())?
                    };
                    call_args.push(value.into());
                }
                self.call_decl(function, &call_args)
            }
            Callee::Method { receiver, method } => {
                let method_name = self.module.backend.symbol_name(*method)?.to_owned();
                if let Some(namespace) = self.stdlib_namespace(receiver) {
                    if let Some(boxed) = self.lower_boxed_stdlib_method_intrinsic(
                        namespace.as_str(),
                        method_name.as_str(),
                        args,
                    )? {
                        return Ok(boxed);
                    }
                    let args = args
                        .iter()
                        .map(|arg| self.lower_operand(arg))
                        .collect::<Result<Vec<_>>>()?;
                    let (array_ptr, count) = self.build_ptr_array(&args)?;
                    let (module_ptr, module_len) = self.string_bytes(namespace.as_str());
                    let (method_ptr, method_len) = self.string_ptr(*method)?;
                    return self.call_ptr(
                        "fdn_stdlib_call",
                        &[
                            module_ptr.into(),
                            module_len.into(),
                            method_ptr.into(),
                            method_len.into(),
                            array_ptr.into(),
                            count.into(),
                        ],
                    );
                }
                if let Some(boxed) =
                    self.lower_container_method_call(receiver, method_name.as_str(), args)?
                {
                    return Ok(boxed);
                }
                let receiver = self.lower_operand(receiver)?;
                let args = args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&args)?;
                let (method_ptr, method_len) = self.string_ptr(*method)?;
                self.call_ptr(
                    "fdn_obj_invoke",
                    &[
                        receiver.into(),
                        method_ptr.into(),
                        method_len.into(),
                        array_ptr.into(),
                        count.into(),
                    ],
                )
            }
            Callee::Builtin(symbol) => self.lower_builtin(*symbol, args),
            Callee::Dynamic(function_value) => {
                let function_value = self.lower_operand(function_value)?;
                let args = args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&args)?;
                self.call_ptr(
                    "fdn_call_dynamic",
                    &[function_value.into(), array_ptr.into(), count.into()],
                )
            }
        }
    }

    fn lower_native_call(
        &mut self,
        callee: &Callee,
        args: &[Operand],
        ty: &MirTy,
    ) -> Result<Option<BasicValueEnum<'ctx>>> {
        match callee {
            Callee::Method { receiver, method } => {
                let method_name = self.module.backend.symbol_name(*method)?.to_owned();
                if let Some(namespace) = self.stdlib_namespace(receiver) {
                    return self.lower_native_stdlib_method_intrinsic(
                        namespace.as_str(),
                        method_name.as_str(),
                        args,
                        ty,
                    );
                }
                self.lower_native_container_method_call(receiver, method_name.as_str(), args, ty)
            }
            _ => Ok(None),
        }
    }

    fn call_may_throw(&self, callee: &Callee, args: &[Operand]) -> Result<bool> {
        match callee {
            Callee::Fn(function_id) => Ok(self
                .module
                .function_throw_map
                .get(function_id)
                .copied()
                .unwrap_or(true)),
            Callee::Method { receiver, method } => {
                let Some(namespace) = self.stdlib_namespace(receiver) else {
                    return Ok(true);
                };
                let method_name = self.module.backend.symbol_name(*method)?.to_owned();
                let arg_kinds = args
                    .iter()
                    .map(|arg| self.operand_stdlib_kind(arg))
                    .collect::<Vec<_>>();
                Ok(
                    infer_stdlib_method(namespace.as_str(), method_name.as_str(), &arg_kinds)
                        .and_then(|info| info.intrinsic)
                        .is_none(),
                )
            }
            _ => Ok(true),
        }
    }

    fn lower_boxed_stdlib_method_intrinsic(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<PointerValue<'ctx>>> {
        let arg_kinds = args
            .iter()
            .map(|arg| self.operand_stdlib_kind(arg))
            .collect::<Vec<_>>();
        let Some(info) = infer_stdlib_method(namespace, method, &arg_kinds) else {
            return Ok(None);
        };
        let result_ty = match info.return_kind {
            StdlibValueKind::Integer => MirTy::Integer,
            StdlibValueKind::Float => MirTy::Float,
            StdlibValueKind::Boolean => MirTy::Boolean,
            _ => return Ok(None),
        };
        let Some(value) =
            self.lower_native_stdlib_method_intrinsic(namespace, method, args, &result_ty)?
        else {
            return Ok(None);
        };
        Ok(Some(self.box_native_value(value, &result_ty)?))
    }

    fn lower_native_stdlib_method_intrinsic(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[Operand],
        ty: &MirTy,
    ) -> Result<Option<BasicValueEnum<'ctx>>> {
        let Some(first_arg) = args.first() else {
            return Ok(None);
        };
        let arg_kinds = args
            .iter()
            .map(|arg| self.operand_stdlib_kind(arg))
            .collect::<Vec<_>>();
        let Some(info) = infer_stdlib_method(namespace, method, &arg_kinds) else {
            return Ok(None);
        };
        let Some(intrinsic) = info.intrinsic else {
            return Ok(None);
        };

        let input = match self.operand_type(first_arg) {
            MirTy::Float => self
                .lower_native_operand(first_arg, &MirTy::Float)?
                .into_float_value(),
            MirTy::Integer | MirTy::Handle => {
                let value = self
                    .lower_native_operand(first_arg, &MirTy::Integer)?
                    .into_int_value();
                let name = self.temp("sitofp");
                self.module
                    .builder
                    .build_signed_int_to_float(value, self.module.f64_type, &name)
                    .map_err(|err| anyhow!("{err}"))?
            }
            _ => return Ok(None),
        };

        let value = match intrinsic {
            StdlibIntrinsic::Math(MathIntrinsic::Sqrt) => {
                let intrinsic = self.llvm_unary_f64_intrinsic("llvm.sqrt.f64");
                self.call_f64_decl(intrinsic, &[input.into()])?.into()
            }
            StdlibIntrinsic::Math(MathIntrinsic::Abs) => match ty {
                MirTy::Integer => {
                    let value = self
                        .lower_native_operand(first_arg, &MirTy::Integer)?
                        .into_int_value();
                    let cmp_name = self.temp("iabs.neg");
                    let is_negative = self
                        .module
                        .builder
                        .build_int_compare(
                            IntPredicate::SLT,
                            value,
                            self.module.i64_type.const_zero(),
                            &cmp_name,
                        )
                        .map_err(|err| anyhow!("{err}"))?;
                    let neg_name = self.temp("iabs.negv");
                    let negated = self
                        .module
                        .builder
                        .build_int_neg(value, &neg_name)
                        .map_err(|err| anyhow!("{err}"))?;
                    let select_name = self.temp("iabs");
                    self.module
                        .builder
                        .build_select(is_negative, negated, value, &select_name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into_int_value()
                        .into()
                }
                MirTy::Float => {
                    let intrinsic = self.llvm_unary_f64_intrinsic("llvm.fabs.f64");
                    self.call_f64_decl(intrinsic, &[input.into()])?.into()
                }
                _ => return Ok(None),
            },
            StdlibIntrinsic::Math(MathIntrinsic::Floor) => {
                let intrinsic = self.llvm_unary_f64_intrinsic("llvm.floor.f64");
                let floored = self.call_f64_decl(intrinsic, &[input.into()])?;
                if matches!(ty, MirTy::Integer) {
                    let name = self.temp("floor_i64");
                    self.module
                        .builder
                        .build_float_to_signed_int(floored, self.module.i64_type, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                } else {
                    floored.into()
                }
            }
            StdlibIntrinsic::Math(MathIntrinsic::Ceil) => {
                let intrinsic = self.llvm_unary_f64_intrinsic("llvm.ceil.f64");
                let ceiled = self.call_f64_decl(intrinsic, &[input.into()])?;
                if matches!(ty, MirTy::Integer) {
                    let name = self.temp("ceil_i64");
                    self.module
                        .builder
                        .build_float_to_signed_int(ceiled, self.module.i64_type, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                } else {
                    ceiled.into()
                }
            }
            StdlibIntrinsic::Math(MathIntrinsic::Trunc) => {
                let intrinsic = self.llvm_unary_f64_intrinsic("llvm.trunc.f64");
                self.call_f64_decl(intrinsic, &[input.into()])?.into()
            }
        };
        Ok(Some(value))
    }

    fn lower_builtin(&mut self, symbol: Symbol, args: &[Operand]) -> Result<PointerValue<'ctx>> {
        let name = self.module.backend.symbol_name(symbol)?.to_owned();
        trace(&format!("inkwell:lower_builtin:{name}"));
        match name.as_str() {
            "print" => {
                if args.len() <= 1 {
                    let arg = if let Some(arg) = args.first() {
                        self.lower_operand(arg)?
                    } else {
                        self.call_ptr("fdn_box_nothing", &[])?
                    };
                    self.call_void("fdn_println", &[arg.into()])?;
                } else {
                    let values = args
                        .iter()
                        .map(|arg| self.lower_operand(arg))
                        .collect::<Result<Vec<_>>>()?;
                    let (array_ptr, count) = self.build_ptr_array(&values)?;
                    self.call_void("fdn_print_many", &[array_ptr.into(), count.into()])?;
                }
                self.call_ptr("fdn_box_nothing", &[])
            }
            "input" => {
                let prompt = if let Some(arg) = args.first() {
                    self.lower_operand(arg)?
                } else {
                    self.call_ptr("fdn_box_nothing", &[])?
                };
                self.call_ptr("fdn_input", &[prompt.into()])
            }
            "len" => {
                let arg = self.lower_operand(&args[0])?;
                let raw = self.call_i64("fdn_len", &[arg.into()])?;
                self.call_ptr("fdn_box_int", &[raw.into()])
            }
            "type" => {
                let arg = self.lower_operand(&args[0])?;
                self.call_ptr("fdn_type_name", &[arg.into()])
            }
            "assert" => {
                let cond = self.lower_operand(&args[0])?;
                let truthy = self.call_i8("fdn_truthy", &[cond.into()])?;
                let truthy_i64 = self.module.builder.build_int_z_extend(
                    truthy,
                    self.module.i64_type,
                    "assert_truthy_i64",
                )?;
                let msg = if let Some(arg) = args.get(1) {
                    self.lower_operand(arg)?
                } else {
                    self.lower_literal(&MirLit::Str("assertion failed".to_owned()))?
                };
                self.call_void("fdn_assert", &[truthy_i64.into(), msg.into()])?;
                self.call_ptr("fdn_box_nothing", &[])
            }
            "assertEq" | "assert_eq" => {
                let lhs = self.lower_operand(&args[0])?;
                let rhs = self.lower_operand(&args[1])?;
                self.call_void("fdn_assert_eq", &[lhs.into(), rhs.into()])?;
                self.call_ptr("fdn_box_nothing", &[])
            }
            "assertNe" | "assert_ne" => {
                let lhs = self.lower_operand(&args[0])?;
                let rhs = self.lower_operand(&args[1])?;
                self.call_void("fdn_assert_ne", &[lhs.into(), rhs.into()])?;
                self.call_ptr("fdn_box_nothing", &[])
            }
            "panic" => {
                let msg = if let Some(arg) = args.first() {
                    self.lower_operand(arg)?
                } else {
                    self.call_ptr("fdn_box_nothing", &[])?
                };
                self.call_void("fdn_panic", &[msg.into()])?;
                self.module
                    .builder
                    .build_unreachable()
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(self.module.ptr_type.const_null())
            }
            "string" | "str" => {
                let arg = self.lower_operand(&args[0])?;
                self.call_ptr("fdn_to_string", &[arg.into()])
            }
            "integer" | "int" => {
                let arg = self.lower_operand(&args[0])?;
                self.call_ptr("fdn_to_integer", &[arg.into()])
            }
            "float" => {
                let arg = self.lower_operand(&args[0])?;
                self.call_ptr("fdn_to_float", &[arg.into()])
            }
            "boolean" | "bool" => {
                let arg = self.lower_operand(&args[0])?;
                self.call_ptr("fdn_to_boolean", &[arg.into()])
            }
            _ => {
                let (module_ptr, module_len) = self.string_bytes("__builtin__");
                let (name_ptr, name_len) = self.string_bytes(&name);
                let values = args
                    .iter()
                    .map(|arg| self.lower_operand(arg))
                    .collect::<Result<Vec<_>>>()?;
                let (array_ptr, count) = self.build_ptr_array(&values)?;
                self.call_ptr(
                    "fdn_stdlib_call",
                    &[
                        module_ptr.into(),
                        module_len.into(),
                        name_ptr.into(),
                        name_len.into(),
                        array_ptr.into(),
                        count.into(),
                    ],
                )
            }
        }
    }

    fn lower_binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
    ) -> Result<PointerValue<'ctx>> {
        let lhs = self.lower_operand(lhs)?;
        let rhs = self.lower_operand(rhs)?;
        match op {
            BinOp::Add => self.call_ptr("fdn_dyn_add", &[lhs.into(), rhs.into()]),
            BinOp::Sub => self.call_ptr("fdn_dyn_sub", &[lhs.into(), rhs.into()]),
            BinOp::Mul => self.call_ptr("fdn_dyn_mul", &[lhs.into(), rhs.into()]),
            BinOp::Div => self.call_ptr("fdn_dyn_div", &[lhs.into(), rhs.into()]),
            BinOp::Rem => self.call_ptr("fdn_dyn_rem", &[lhs.into(), rhs.into()]),
            BinOp::Pow => self.call_ptr("fdn_dyn_pow", &[lhs.into(), rhs.into()]),
            BinOp::Eq => self.lower_cmp("fdn_dyn_eq", lhs, rhs),
            BinOp::NotEq => self.lower_cmp("fdn_dyn_ne", lhs, rhs),
            BinOp::Lt => self.lower_cmp("fdn_dyn_lt", lhs, rhs),
            BinOp::LtEq => self.lower_cmp("fdn_dyn_le", lhs, rhs),
            BinOp::Gt => self.lower_cmp("fdn_dyn_gt", lhs, rhs),
            BinOp::GtEq => self.lower_cmp("fdn_dyn_ge", lhs, rhs),
            BinOp::And => self.call_ptr("fdn_dyn_and", &[lhs.into(), rhs.into()]),
            BinOp::Or => self.call_ptr("fdn_dyn_or", &[lhs.into(), rhs.into()]),
            BinOp::BitXor => self.call_ptr("fdn_dyn_bit_xor", &[lhs.into(), rhs.into()]),
            BinOp::BitAnd => self.call_ptr("fdn_dyn_bit_and", &[lhs.into(), rhs.into()]),
            BinOp::BitOr => self.call_ptr("fdn_dyn_bit_or", &[lhs.into(), rhs.into()]),
            BinOp::Shl => self.call_ptr("fdn_dyn_shl", &[lhs.into(), rhs.into()]),
            BinOp::Shr => self.call_ptr("fdn_dyn_shr", &[lhs.into(), rhs.into()]),
            BinOp::Range | BinOp::RangeInclusive => {
                let lhs_raw = self.call_i64("fdn_unbox_int", &[lhs.into()])?;
                let rhs_raw = self.call_i64("fdn_unbox_int", &[rhs.into()])?;
                let inclusive = self.module.i8_type.const_int(
                    if matches!(op, BinOp::RangeInclusive) {
                        1
                    } else {
                        0
                    },
                    false,
                );
                self.call_ptr(
                    "fdn_make_range",
                    &[lhs_raw.into(), rhs_raw.into(), inclusive.into()],
                )
            }
        }
    }

    fn lower_native_rvalue(&mut self, rhs: &Rvalue, ty: &MirTy) -> Result<BasicValueEnum<'ctx>> {
        match rhs {
            Rvalue::Use(operand) => self.lower_native_operand(operand, ty),
            Rvalue::Literal(literal) => self.lower_native_literal(literal, ty),
            Rvalue::Binary { op, lhs, rhs } => self.lower_native_binary(*op, lhs, rhs, ty),
            Rvalue::Unary { op, operand } => self.lower_native_unary(*op, operand, ty),
            Rvalue::Call { callee, args } => {
                if let Some(value) = self.lower_native_call(callee, args, ty)? {
                    Ok(value)
                } else {
                    let boxed = self.lower_call(callee, args)?;
                    self.unbox_to_native(boxed, ty)
                }
            }
            _ => {
                let boxed = self.lower_rvalue(rhs)?;
                self.unbox_to_native(boxed, ty)
            }
        }
    }

    fn lower_native_operand(
        &mut self,
        operand: &Operand,
        ty: &MirTy,
    ) -> Result<BasicValueEnum<'ctx>> {
        match operand {
            Operand::Local(local) if self.local_type(*local) == *ty => {
                self.load_local_native(*local)
            }
            Operand::Const(literal) => self.lower_native_literal(literal, ty),
            _ => {
                let boxed = self.lower_operand(operand)?;
                self.unbox_to_native(boxed, ty)
            }
        }
    }

    fn lower_native_literal(
        &mut self,
        literal: &MirLit,
        ty: &MirTy,
    ) -> Result<BasicValueEnum<'ctx>> {
        match (literal, ty) {
            (MirLit::Int(value), MirTy::Integer | MirTy::Handle) => {
                Ok(self.module.i64_type.const_int(*value as u64, true).into())
            }
            (MirLit::Float(value), MirTy::Float) => {
                Ok(self.module.f64_type.const_float(*value).into())
            }
            (MirLit::Bool(value), MirTy::Boolean) => Ok(self
                .module
                .i8_type
                .const_int(if *value { 1 } else { 0 }, false)
                .into()),
            _ => {
                let boxed = self.lower_literal(literal)?;
                self.unbox_to_native(boxed, ty)
            }
        }
    }

    fn lower_native_binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        ty: &MirTy,
    ) -> Result<BasicValueEnum<'ctx>> {
        let lhs_ty = self.operand_type(lhs);
        let rhs_ty = self.operand_type(rhs);
        if lhs_ty == MirTy::Integer && rhs_ty == MirTy::Integer {
            let lhs_value = self
                .lower_native_operand(lhs, &MirTy::Integer)?
                .into_int_value();
            let rhs_value = self
                .lower_native_operand(rhs, &MirTy::Integer)?
                .into_int_value();
            let value = match op {
                BinOp::Add if matches!(ty, MirTy::Integer) => {
                    let name = self.temp("iadd");
                    self.module
                        .builder
                        .build_int_add(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Sub if matches!(ty, MirTy::Integer) => {
                    let name = self.temp("isub");
                    self.module
                        .builder
                        .build_int_sub(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Mul if matches!(ty, MirTy::Integer) => {
                    let name = self.temp("imul");
                    self.module
                        .builder
                        .build_int_mul(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Div if matches!(ty, MirTy::Integer) => {
                    let name = self.temp("idiv");
                    self.module
                        .builder
                        .build_int_signed_div(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Rem if matches!(ty, MirTy::Integer) => {
                    let name = self.temp("irem");
                    self.module
                        .builder
                        .build_int_signed_rem(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Eq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("ieq");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::EQ, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::NotEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("ine");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::NE, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Lt if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("ilt");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::SLT, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::LtEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("ile");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::SLE, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Gt if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("igt");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::SGT, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::GtEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("ige");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::SGE, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                _ => {
                    let boxed = self.lower_binary(op, lhs, rhs)?;
                    return self.unbox_to_native(boxed, ty);
                }
            };
            return Ok(value);
        }
        if lhs_ty == MirTy::Float && rhs_ty == MirTy::Float {
            let lhs_value = self
                .lower_native_operand(lhs, &MirTy::Float)?
                .into_float_value();
            let rhs_value = self
                .lower_native_operand(rhs, &MirTy::Float)?
                .into_float_value();
            let value = match op {
                BinOp::Add if matches!(ty, MirTy::Float) => {
                    let name = self.temp("fadd");
                    self.module
                        .builder
                        .build_float_add(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Sub if matches!(ty, MirTy::Float) => {
                    let name = self.temp("fsub");
                    self.module
                        .builder
                        .build_float_sub(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Mul if matches!(ty, MirTy::Float) => {
                    let name = self.temp("fmul");
                    self.module
                        .builder
                        .build_float_mul(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Div if matches!(ty, MirTy::Float) => {
                    let name = self.temp("fdiv");
                    self.module
                        .builder
                        .build_float_div(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Eq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("feq");
                    self.module
                        .builder
                        .build_float_compare(
                            inkwell::FloatPredicate::OEQ,
                            lhs_value,
                            rhs_value,
                            &name,
                        )
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::NotEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("fne");
                    self.module
                        .builder
                        .build_float_compare(
                            inkwell::FloatPredicate::ONE,
                            lhs_value,
                            rhs_value,
                            &name,
                        )
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Lt if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("flt");
                    self.module
                        .builder
                        .build_float_compare(
                            inkwell::FloatPredicate::OLT,
                            lhs_value,
                            rhs_value,
                            &name,
                        )
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::LtEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("fle");
                    self.module
                        .builder
                        .build_float_compare(
                            inkwell::FloatPredicate::OLE,
                            lhs_value,
                            rhs_value,
                            &name,
                        )
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Gt if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("fgt");
                    self.module
                        .builder
                        .build_float_compare(
                            inkwell::FloatPredicate::OGT,
                            lhs_value,
                            rhs_value,
                            &name,
                        )
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::GtEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("fge");
                    self.module
                        .builder
                        .build_float_compare(
                            inkwell::FloatPredicate::OGE,
                            lhs_value,
                            rhs_value,
                            &name,
                        )
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                _ => {
                    let boxed = self.lower_binary(op, lhs, rhs)?;
                    return self.unbox_to_native(boxed, ty);
                }
            };
            return Ok(value);
        }
        if lhs_ty == MirTy::Boolean && rhs_ty == MirTy::Boolean {
            let lhs_value = self
                .lower_native_operand(lhs, &MirTy::Boolean)?
                .into_int_value();
            let rhs_value = self
                .lower_native_operand(rhs, &MirTy::Boolean)?
                .into_int_value();
            let value = match op {
                BinOp::And if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("band");
                    self.module
                        .builder
                        .build_and(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Or if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("bor");
                    self.module
                        .builder
                        .build_or(lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::Eq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("beq");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::EQ, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                BinOp::NotEq if matches!(ty, MirTy::Boolean) => {
                    let name = self.temp("bne");
                    self.module
                        .builder
                        .build_int_compare(IntPredicate::NE, lhs_value, rhs_value, &name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into()
                }
                _ => {
                    let boxed = self.lower_binary(op, lhs, rhs)?;
                    return self.unbox_to_native(boxed, ty);
                }
            };
            return Ok(value);
        }

        let boxed = self.lower_binary(op, lhs, rhs)?;
        self.unbox_to_native(boxed, ty)
    }

    fn lower_native_unary(
        &mut self,
        op: UnOp,
        operand: &Operand,
        ty: &MirTy,
    ) -> Result<BasicValueEnum<'ctx>> {
        match (op, ty) {
            (UnOp::Pos, MirTy::Integer | MirTy::Float | MirTy::Boolean | MirTy::Handle) => {
                self.lower_native_operand(operand, ty)
            }
            (UnOp::Neg, MirTy::Integer) => {
                let value = self.lower_native_operand(operand, ty)?.into_int_value();
                let name = self.temp("ineg");
                Ok(self
                    .module
                    .builder
                    .build_int_neg(value, &name)
                    .map_err(|err| anyhow!("{err}"))?
                    .into())
            }
            (UnOp::Neg, MirTy::Float) => {
                let value = self.lower_native_operand(operand, ty)?.into_float_value();
                let name = self.temp("fneg");
                Ok(self
                    .module
                    .builder
                    .build_float_neg(value, &name)
                    .map_err(|err| anyhow!("{err}"))?
                    .into())
            }
            (UnOp::Not, MirTy::Boolean) => {
                let value = self.lower_native_operand(operand, ty)?.into_int_value();
                let name = self.temp("bnot");
                Ok(self
                    .module
                    .builder
                    .build_not(value, &name)
                    .map_err(|err| anyhow!("{err}"))?
                    .into())
            }
            _ => {
                let boxed = self.lower_unary(op, operand)?;
                self.unbox_to_native(boxed, ty)
            }
        }
    }

    fn lower_unary(&mut self, op: UnOp, operand: &Operand) -> Result<PointerValue<'ctx>> {
        let operand = self.lower_operand(operand)?;
        match op {
            UnOp::Pos => Ok(operand),
            UnOp::Neg => self.call_ptr("fdn_dyn_neg", &[operand.into()]),
            UnOp::Not => self.call_ptr("fdn_dyn_not", &[operand.into()]),
        }
    }

    fn lower_cmp(
        &mut self,
        function: &'static str,
        lhs: PointerValue<'ctx>,
        rhs: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>> {
        let raw = self.call_i8(function, &[lhs.into(), rhs.into()])?;
        self.call_ptr("fdn_box_bool", &[raw.into()])
    }

    fn lower_operand(&mut self, operand: &Operand) -> Result<PointerValue<'ctx>> {
        match operand {
            Operand::Local(local) => {
                if is_native_scalar_ty(&self.local_type(*local)) {
                    let value = self.load_local_native(*local)?;
                    self.box_native_value(value, &self.local_type(*local))
                } else {
                    let slot = self.slot(*local)?;
                    let load_name = self.temp("load");
                    Ok(self
                        .module
                        .builder
                        .build_load(self.module.ptr_type, slot, &load_name)
                        .map_err(|err| anyhow!("{err}"))?
                        .into_pointer_value())
                }
            }
            Operand::Const(literal) => self.lower_literal(literal),
        }
    }

    fn lower_literal(&mut self, literal: &MirLit) -> Result<PointerValue<'ctx>> {
        trace(&format!("inkwell:lower_literal:{}", literal_name(literal)));
        match literal {
            MirLit::Int(value) => self.call_ptr(
                "fdn_box_int",
                &[self.module.i64_type.const_int(*value as u64, true).into()],
            ),
            MirLit::Float(value) => self.call_ptr(
                "fdn_box_float",
                &[self.module.f64_type.const_float(*value).into()],
            ),
            MirLit::Bool(value) => self.call_ptr(
                "fdn_box_bool",
                &[self
                    .module
                    .i8_type
                    .const_int(if *value { 1 } else { 0 }, false)
                    .into()],
            ),
            MirLit::Str(value) => {
                let (ptr, len) = self.string_bytes(value);
                self.call_ptr("fdn_box_str", &[ptr.into(), len.into()])
            }
            MirLit::Nothing => self.call_ptr("fdn_box_nothing", &[]),
            MirLit::FunctionRef(function_id) => self.call_ptr(
                "fdn_box_fn_ref",
                &[self
                    .module
                    .i64_type
                    .const_int(*function_id as u64, false)
                    .into()],
            ),
            MirLit::Namespace(value) => {
                let (ptr, len) = self.string_bytes(value);
                self.call_ptr("fdn_box_namespace", &[ptr.into(), len.into()])
            }
            MirLit::StdlibFn { module, name } => {
                let (module_ptr, module_len) = self.string_bytes(module);
                let (fn_ptr, fn_len) = self.string_bytes(name);
                self.call_ptr(
                    "fdn_box_stdlib_fn",
                    &[
                        module_ptr.into(),
                        module_len.into(),
                        fn_ptr.into(),
                        fn_len.into(),
                    ],
                )
            }
            MirLit::EnumType(value) => {
                let (ptr, len) = self.string_bytes(value);
                self.call_ptr("fdn_box_enum_type", &[ptr.into(), len.into()])
            }
            MirLit::ClassType(value) => {
                let (ptr, len) = self.string_bytes(value);
                self.call_ptr("fdn_box_class_type", &[ptr.into(), len.into()])
            }
        }
    }

    fn lower_construct(
        &mut self,
        class_symbol: Symbol,
        fields: &[(Symbol, Operand)],
    ) -> Result<PointerValue<'ctx>> {
        let class_name = self.module.backend.symbol_name(class_symbol)?.to_owned();
        let (class_ptr, class_len) = self.string_bytes(&class_name);
        let object = self.call_ptr("fdn_obj_new", &[class_ptr.into(), class_len.into()])?;
        for (field_symbol, operand) in fields {
            let value = self.lower_operand(operand)?;
            let (field_ptr, field_len) = self.string_ptr(*field_symbol)?;
            self.call_void(
                "fdn_obj_set_field",
                &[
                    object.into(),
                    field_ptr.into(),
                    field_len.into(),
                    value.into(),
                ],
            )?;
        }

        let mut method_map: HashMap<Symbol, FunctionId> = HashMap::new();
        let mut current_symbol = Some(class_symbol);
        while let Some(symbol) = current_symbol {
            if let Some(object_info) = self
                .module
                .backend
                .program()
                .objects
                .iter()
                .find(|object| object.name == symbol)
            {
                for (&method_symbol, &function_id) in &object_info.methods {
                    method_map.entry(method_symbol).or_insert(function_id);
                }
                current_symbol = object_info.parent;
            } else {
                break;
            }
        }

        for (method_symbol, function_id) in method_map {
            let method_name = self.module.backend.symbol_name(method_symbol)?;
            let key = format!("__method__{method_name}");
            let (key_ptr, key_len) = self.string_bytes(&key);
            let function_ref = self.call_ptr(
                "fdn_box_fn_ref",
                &[self
                    .module
                    .i64_type
                    .const_int(function_id.0 as u64, false)
                    .into()],
            )?;
            self.call_void(
                "fdn_obj_set_field",
                &[
                    object.into(),
                    key_ptr.into(),
                    key_len.into(),
                    function_ref.into(),
                ],
            )?;
        }

        Ok(object)
    }

    fn lower_list(&mut self, values: &[Operand]) -> Result<PointerValue<'ctx>> {
        let list = self.call_ptr("fdn_list_new", &[])?;
        for value in values {
            let value = self.lower_operand(value)?;
            self.call_void("fdn_list_push", &[list.into(), value.into()])?;
        }
        Ok(list)
    }

    fn lower_tuple(&mut self, values: &[Operand]) -> Result<PointerValue<'ctx>> {
        let values = values
            .iter()
            .map(|value| self.lower_operand(value))
            .collect::<Result<Vec<_>>>()?;
        let (array_ptr, count) = self.build_ptr_array(&values)?;
        self.call_ptr("fdn_tuple_pack", &[array_ptr.into(), count.into()])
    }

    fn lower_dict(&mut self, pairs: &[(Operand, Operand)]) -> Result<PointerValue<'ctx>> {
        let dict = self.call_ptr("fdn_dict_new", &[])?;
        for (key, value) in pairs {
            let key = self.lower_operand(key)?;
            let value = self.lower_operand(value)?;
            self.call_void("fdn_dict_set", &[dict.into(), key.into(), value.into()])?;
        }
        Ok(dict)
    }

    fn lower_string_interp(&mut self, parts: &[MirStringPart]) -> Result<PointerValue<'ctx>> {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            let value = match part {
                MirStringPart::Literal(text) => self.lower_literal(&MirLit::Str(text.clone()))?,
                MirStringPart::Operand(operand) => {
                    let value = self.lower_operand(operand)?;
                    self.call_ptr("fdn_to_string", &[value.into()])?
                }
            };
            values.push(value);
        }
        let (array_ptr, count) = self.build_ptr_array(&values)?;
        self.call_ptr("fdn_str_interp", &[array_ptr.into(), count.into()])
    }

    fn lower_slice(
        &mut self,
        target: &Operand,
        start: Option<&Operand>,
        end: Option<&Operand>,
        inclusive: bool,
        step: Option<&Operand>,
    ) -> Result<PointerValue<'ctx>> {
        let target = self.lower_operand(target)?;
        let start = match start {
            Some(operand) => self.lower_operand(operand)?,
            None => self.call_ptr("fdn_box_nothing", &[])?,
        };
        let end = match end {
            Some(operand) => self.lower_operand(operand)?,
            None => self.call_ptr("fdn_box_nothing", &[])?,
        };
        let step = match step {
            Some(operand) => self.lower_operand(operand)?,
            None => self.call_ptr("fdn_box_nothing", &[])?,
        };
        let inclusive = self
            .module
            .i8_type
            .const_int(if inclusive { 1 } else { 0 }, false);
        self.call_ptr(
            "fdn_slice",
            &[
                target.into(),
                start.into(),
                end.into(),
                inclusive.into(),
                step.into(),
            ],
        )
    }

    fn lower_enum_construct(
        &mut self,
        tag: Symbol,
        payload: &[Operand],
    ) -> Result<PointerValue<'ctx>> {
        let (tag_ptr, tag_len) = self.string_ptr(tag)?;
        let values = payload
            .iter()
            .map(|operand| self.lower_operand(operand))
            .collect::<Result<Vec<_>>>()?;
        let (array_ptr, count) = self.build_ptr_array(&values)?;
        self.call_ptr(
            "fdn_enum_variant",
            &[
                tag_ptr.into(),
                tag_len.into(),
                array_ptr.into(),
                count.into(),
            ],
        )
    }

    fn lower_enum_tag_check(
        &mut self,
        value: &Operand,
        expected_tag: Symbol,
    ) -> Result<PointerValue<'ctx>> {
        let value = self.lower_operand(value)?;
        let (tag_ptr, tag_len) = self.string_ptr(expected_tag)?;
        let raw = self.call_i8(
            "fdn_enum_tag_check",
            &[value.into(), tag_ptr.into(), tag_len.into()],
        )?;
        self.call_ptr("fdn_box_bool", &[raw.into()])
    }

    fn lower_enum_payload(&mut self, value: &Operand, index: usize) -> Result<PointerValue<'ctx>> {
        let value = self.lower_operand(value)?;
        let index = self.module.i64_type.const_int(index as u64, false);
        self.call_ptr("fdn_enum_payload", &[value.into(), index.into()])
    }

    fn build_ptr_array(
        &mut self,
        values: &[PointerValue<'ctx>],
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>)> {
        if values.is_empty() {
            return Ok((
                self.module.ptr_type.const_null(),
                self.module.i64_type.const_zero(),
            ));
        }

        let array_type = self.module.ptr_type.array_type(values.len() as u32);
        let array_name = self.temp("arr");
        let entry = self
            .llvm_function
            .get_first_basic_block()
            .ok_or_else(|| anyhow!("missing entry block for LLVM function"))?;
        let alloca_builder = self.module.context.create_builder();
        if let Some(first_instruction) = entry.get_first_instruction() {
            alloca_builder.position_before(&first_instruction);
        } else {
            alloca_builder.position_at_end(entry);
        }
        let array = alloca_builder
            .build_alloca(array_type, &array_name)
            .map_err(|err| anyhow!("{err}"))?;

        let zero = self.module.i32_type.const_zero();
        for (index, value) in values.iter().enumerate() {
            let elt_name = self.temp("elt");
            let gep = unsafe {
                self.module.builder.build_in_bounds_gep(
                    array_type,
                    array,
                    &[zero, self.module.i32_type.const_int(index as u64, false)],
                    &elt_name,
                )
            }
            .map_err(|err| anyhow!("{err}"))?;
            self.module
                .builder
                .build_store(gep, *value)
                .map_err(|err| anyhow!("{err}"))?;
        }

        let arr_ptr_name = self.temp("arr.ptr");
        let first = unsafe {
            self.module
                .builder
                .build_in_bounds_gep(array_type, array, &[zero, zero], &arr_ptr_name)
        }
        .map_err(|err| anyhow!("{err}"))?;
        Ok((
            first,
            self.module.i64_type.const_int(values.len() as u64, false),
        ))
    }

    fn string_ptr(&mut self, symbol: Symbol) -> Result<(PointerValue<'ctx>, IntValue<'ctx>)> {
        let text = self.module.backend.symbol_name(symbol)?.to_owned();
        Ok(self.string_bytes(&text))
    }

    fn string_bytes(&mut self, value: &str) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        trace(&format!("inkwell:string_bytes:{}", value.len()));
        let global = self.module.intern_string_global(value);
        let array_type = self
            .module
            .context
            .const_string(value.as_bytes(), true)
            .get_type();
        let str_ptr_name = self.temp("str.ptr");
        let ptr = unsafe {
            self.module.builder.build_in_bounds_gep(
                array_type,
                global.as_pointer_value(),
                &[
                    self.module.i32_type.const_zero(),
                    self.module.i32_type.const_zero(),
                ],
                &str_ptr_name,
            )
        }
        .expect("valid string gep");
        (
            ptr,
            self.module.i64_type.const_int(value.len() as u64, false),
        )
    }

    fn call_ptr(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<PointerValue<'ctx>> {
        let function = self.module.runtime_fn(name)?;
        self.call_decl(function, args)
    }

    fn call_decl(
        &mut self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<PointerValue<'ctx>> {
        let call_name = self.temp("call");
        let call = self
            .module
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected non-void call result"))
            .map(BasicValueEnum::into_pointer_value)
    }

    fn call_f64_decl(
        &mut self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<FloatValue<'ctx>> {
        let call_name = self.temp("call");
        let call = self
            .module
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected f64 call result"))
            .map(BasicValueEnum::into_float_value)
    }

    fn call_i8(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<IntValue<'ctx>> {
        let function = self.module.runtime_fn(name)?;
        let call_name = self.temp("call");
        let call = self
            .module
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected i8 call result"))
            .map(BasicValueEnum::into_int_value)
    }

    fn call_i64(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<IntValue<'ctx>> {
        let function = self.module.runtime_fn(name)?;
        let call_name = self.temp("call");
        let call = self
            .module
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected i64 call result"))
            .map(BasicValueEnum::into_int_value)
    }

    fn call_f64(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<FloatValue<'ctx>> {
        let function = self.module.runtime_fn(name)?;
        let call_name = self.temp("call");
        let call = self
            .module
            .builder
            .build_call(function, args, &call_name)
            .map_err(|err| anyhow!("{err}"))?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow!("expected f64 call result"))
            .map(BasicValueEnum::into_float_value)
    }

    fn call_void(
        &mut self,
        name: &'static str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<()> {
        let function = self.module.runtime_fn(name)?;
        self.module
            .builder
            .build_call(function, args, "")
            .map_err(|err| anyhow!("{err}"))?;
        Ok(())
    }

    fn slot(&self, local: LocalId) -> Result<PointerValue<'ctx>> {
        self.locals
            .get(&local.0)
            .copied()
            .ok_or_else(|| anyhow!("missing local slot for {}", local.0))
    }

    fn local_type(&self, local: LocalId) -> MirTy {
        self.local_types
            .get(&local.0)
            .cloned()
            .unwrap_or(MirTy::Dynamic)
    }

    fn local_storage_type(&self, local: LocalId) -> inkwell::types::BasicTypeEnum<'ctx> {
        llvm_storage_type(self.module, &self.local_type(local))
    }

    fn store_slot_default(&mut self, local: LocalId, slot: PointerValue<'ctx>) -> Result<()> {
        let zero = native_zero(self.module, &self.local_type(local))?;
        self.module
            .builder
            .build_store(slot, zero)
            .map_err(|err| anyhow!("{err}"))?;
        Ok(())
    }

    fn load_local_native(&mut self, local: LocalId) -> Result<BasicValueEnum<'ctx>> {
        let slot = self.slot(local)?;
        let ty = self.local_type(local);
        let load_name = self.temp("load.native");
        self.module
            .builder
            .build_load(llvm_storage_type(self.module, &ty), slot, &load_name)
            .map_err(|err| anyhow!("{err}"))
    }

    fn store_local_native(&mut self, local: LocalId, value: BasicValueEnum<'ctx>) -> Result<()> {
        let slot = self.slot(local)?;
        self.module
            .builder
            .build_store(slot, value)
            .map_err(|err| anyhow!("{err}"))?;
        Ok(())
    }

    fn store_local_boxed(&mut self, local: LocalId, boxed: PointerValue<'ctx>) -> Result<()> {
        let local_ty = self.local_type(local);
        if is_native_scalar_ty(&local_ty) {
            let value = self.unbox_to_native(boxed, &local_ty)?;
            self.store_local_native(local, value)
        } else {
            let slot = self.slot(local)?;
            let load_name = self.temp("old.boxed");
            let previous = self
                .module
                .builder
                .build_load(self.module.ptr_type, slot, &load_name)
                .map_err(|err| anyhow!("{err}"))?
                .into_pointer_value();
            self.call_void("fdn_drop", &[previous.into()])?;
            self.module
                .builder
                .build_store(slot, boxed)
                .map_err(|err| anyhow!("{err}"))?;
            Ok(())
        }
    }

    fn unbox_to_native(
        &mut self,
        boxed: PointerValue<'ctx>,
        ty: &MirTy,
    ) -> Result<BasicValueEnum<'ctx>> {
        match ty {
            MirTy::Integer => Ok(self.call_i64("fdn_unbox_int", &[boxed.into()])?.into()),
            MirTy::Float => Ok(self.call_f64("fdn_unbox_float", &[boxed.into()])?.into()),
            MirTy::Boolean => Ok(self.call_i8("fdn_unbox_bool", &[boxed.into()])?.into()),
            MirTy::Handle => Ok(self.call_i64("fdn_unbox_handle", &[boxed.into()])?.into()),
            _ => bail!("cannot unbox non-scalar LLVM local type `{ty:?}`"),
        }
    }

    fn box_native_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &MirTy,
    ) -> Result<PointerValue<'ctx>> {
        match ty {
            MirTy::Integer => self.call_ptr("fdn_box_int", &[value.into_int_value().into()]),
            MirTy::Float => self.call_ptr("fdn_box_float", &[value.into_float_value().into()]),
            MirTy::Boolean => self.call_ptr("fdn_box_bool", &[value.into_int_value().into()]),
            MirTy::Handle => self.call_ptr("fdn_box_handle", &[value.into_int_value().into()]),
            _ => bail!("cannot box non-scalar LLVM local type `{ty:?}`"),
        }
    }

    fn stdlib_namespace(&self, receiver: &Operand) -> Option<String> {
        let namespace = match receiver {
            Operand::Local(local) => self.namespace_locals.get(local).cloned(),
            Operand::Const(MirLit::Namespace(namespace)) => Some(namespace.clone()),
            _ => None,
        }?;
        fidan_stdlib::is_stdlib_module(namespace.as_str()).then_some(namespace)
    }

    fn operand_stdlib_kind(&self, operand: &Operand) -> StdlibValueKind {
        match self.operand_type(operand) {
            MirTy::Integer | MirTy::Handle => StdlibValueKind::Integer,
            MirTy::Float => StdlibValueKind::Float,
            MirTy::Boolean => StdlibValueKind::Boolean,
            MirTy::String => StdlibValueKind::String,
            MirTy::List(_) => StdlibValueKind::List,
            MirTy::Dict(_, _) => StdlibValueKind::Dict,
            MirTy::HashSet(_) => StdlibValueKind::HashSet,
            MirTy::Nothing => StdlibValueKind::Nothing,
            _ => StdlibValueKind::Dynamic,
        }
    }

    fn llvm_unary_f64_intrinsic(&mut self, name: &str) -> FunctionValue<'ctx> {
        self.module.module.get_function(name).unwrap_or_else(|| {
            let fn_ty = self
                .module
                .f64_type
                .fn_type(&[self.module.f64_type.into()], false);
            self.module.module.add_function(name, fn_ty, None)
        })
    }

    fn operand_type(&self, operand: &Operand) -> MirTy {
        match operand {
            Operand::Local(local) => self.local_type(*local),
            Operand::Const(MirLit::Int(_)) => MirTy::Integer,
            Operand::Const(MirLit::Float(_)) => MirTy::Float,
            Operand::Const(MirLit::Bool(_)) => MirTy::Boolean,
            Operand::Const(MirLit::Nothing) => MirTy::Nothing,
            Operand::Const(MirLit::Str(_)) | Operand::Const(MirLit::Namespace(_)) => MirTy::String,
            Operand::Const(MirLit::FunctionRef(_)) | Operand::Const(MirLit::StdlibFn { .. }) => {
                MirTy::Function
            }
            Operand::Const(MirLit::EnumType(_)) => MirTy::Dynamic,
            Operand::Const(MirLit::ClassType(_)) => MirTy::Dynamic,
        }
    }

    fn block(&self, id: u32) -> Result<LlvmBlock<'ctx>> {
        self.blocks
            .get(&id)
            .copied()
            .ok_or_else(|| anyhow!("missing basic block {}", id))
    }

    fn global(&self, global: GlobalId) -> Result<GlobalValue<'ctx>> {
        self.module
            .globals
            .get(&global.0)
            .copied()
            .ok_or_else(|| anyhow!("missing global {}", global.0))
    }

    fn temp(&mut self, prefix: &str) -> String {
        let name = format!("{prefix}.{}.{}", self.current_block_name, self.temp_index);
        self.temp_index += 1;
        name
    }
}

fn map_opt_level(level: OptLevel) -> OptimizationLevel {
    match level {
        OptLevel::O0 => OptimizationLevel::None,
        OptLevel::O1 => OptimizationLevel::Less,
        OptLevel::O2 | OptLevel::O3 | OptLevel::Os | OptLevel::Oz => OptimizationLevel::Aggressive,
    }
}

fn optimize_module(module: &Module<'_>, machine: &TargetMachine, level: OptLevel) -> Result<()> {
    let pipeline = match level {
        OptLevel::O0 => return Ok(()),
        OptLevel::O1 => "default<O1>",
        OptLevel::O2 => "default<O2>",
        OptLevel::O3 => "default<O3>",
        OptLevel::Os => "default<Os>",
        OptLevel::Oz => "default<Oz>",
    };
    let options = PassBuilderOptions::create();
    module
        .run_passes(pipeline, machine, options)
        .map_err(|err| anyhow!("failed to run LLVM optimization pipeline `{pipeline}`: {err}"))
}

fn is_native_scalar_ty(ty: &MirTy) -> bool {
    matches!(
        ty,
        MirTy::Integer | MirTy::Float | MirTy::Boolean | MirTy::Handle
    )
}

fn build_global_namespace_map(backend: &BackendContext<'_>) -> HashMap<GlobalId, String> {
    let mut map = HashMap::new();
    for (index, global) in backend.program().globals.iter().enumerate() {
        let Ok(global_name) = backend.symbol_name(global.name) else {
            continue;
        };
        for decl in &backend.program().use_decls {
            if decl.is_stdlib && decl.specific_names.is_none() && global_name == decl.alias.as_str()
            {
                map.insert(GlobalId(index as u32), decl.module.clone());
            }
        }
    }
    map
}

fn llvm_storage_type<'ctx>(
    module: &ModuleCodegen<'ctx, '_>,
    ty: &MirTy,
) -> inkwell::types::BasicTypeEnum<'ctx> {
    match ty {
        MirTy::Integer | MirTy::Handle => module.i64_type.into(),
        MirTy::Float => module.f64_type.into(),
        MirTy::Boolean => module.i8_type.into(),
        _ => module.ptr_type.into(),
    }
}

fn native_zero<'ctx>(module: &ModuleCodegen<'ctx, '_>, ty: &MirTy) -> Result<BasicValueEnum<'ctx>> {
    match ty {
        MirTy::Integer | MirTy::Handle => Ok(module.i64_type.const_zero().into()),
        MirTy::Float => Ok(module.f64_type.const_float(0.0).into()),
        MirTy::Boolean => Ok(module.i8_type.const_zero().into()),
        _ => Ok(module.ptr_type.const_null().into()),
    }
}

fn normalize_verified_module<'ctx>(
    module: &Module<'_>,
    normalized_context: &'ctx Context,
) -> Result<Module<'ctx>> {
    trace("inkwell:verify_module:write_ir");
    let verify_ir_path = std::env::temp_dir().join(format!(
        "fidan-llvm-verify-{}-{}.ll",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    module
        .print_to_file(&verify_ir_path)
        .map_err(|err| anyhow!("failed to write LLVM IR for verification: {err}"))?;
    trace("inkwell:verify_module:parse_ir");
    let memory_buffer = MemoryBuffer::create_from_file(&verify_ir_path)
        .map_err(|err| anyhow!("failed to load LLVM IR for verification: {err}"))?;
    let verify_module = normalized_context
        .create_module_from_ir(memory_buffer)
        .map_err(|err| anyhow!("failed to reparse LLVM IR for verification: {err}"))?;
    let _ = fs::remove_file(&verify_ir_path);
    trace("inkwell:verify_module:call_llvm_verify");
    let mut err_str = ptr::null_mut();
    let code = unsafe {
        LLVMVerifyModule(
            verify_module.as_mut_ptr(),
            LLVMVerifierFailureAction::LLVMReturnStatusAction,
            &mut err_str,
        )
    };
    trace(&format!("inkwell:verify_module:llvm_verify_result:{code}"));

    if code == 1 {
        let detail = if err_str.is_null() {
            "unknown LLVM verifier error".to_owned()
        } else {
            unsafe {
                let detail = CStr::from_ptr(err_str).to_string_lossy().into_owned();
                LLVMDisposeMessage(err_str);
                detail
            }
        };
        trace("inkwell:verify_module:llvm_verify_failed");
        bail!("LLVM verifier rejected generated IR: {detail}");
    }

    trace("inkwell:verify_module:ok");

    Ok(verify_module)
}

fn compute_catch_stacks(function: &MirFunction) -> Vec<Vec<BlockId>> {
    let block_count = function.blocks.len();
    let mut entry_stacks: Vec<Option<Vec<BlockId>>> = vec![None; block_count];
    entry_stacks[0] = Some(Vec::new());

    let mut worklist = std::collections::VecDeque::new();
    worklist.push_back(0usize);

    while let Some(block_index) = worklist.pop_front() {
        let Some(entry_stack) = entry_stacks[block_index].clone() else {
            continue;
        };
        let mut state = entry_stack;

        for instruction in &function.blocks[block_index].instructions {
            match instruction {
                Instr::PushCatch(target) => state.push(*target),
                Instr::PopCatch => {
                    state.pop();
                }
                _ => {}
            }
        }

        let propagate = |dst: usize,
                         stack: Vec<BlockId>,
                         stacks: &mut Vec<Option<Vec<BlockId>>>,
                         queue: &mut std::collections::VecDeque<usize>| {
            if stacks[dst].is_none() {
                stacks[dst] = Some(stack);
                queue.push_back(dst);
            }
        };

        match &function.blocks[block_index].terminator {
            Terminator::Goto(target) => {
                propagate(target.0 as usize, state, &mut entry_stacks, &mut worklist);
            }
            Terminator::Branch {
                then_bb, else_bb, ..
            } => {
                propagate(
                    then_bb.0 as usize,
                    state.clone(),
                    &mut entry_stacks,
                    &mut worklist,
                );
                propagate(else_bb.0 as usize, state, &mut entry_stacks, &mut worklist);
            }
            Terminator::Throw { .. } => {
                if let Some(catch_block) = state.last().copied() {
                    let mut after_pop = state.clone();
                    after_pop.pop();
                    propagate(
                        catch_block.0 as usize,
                        after_pop,
                        &mut entry_stacks,
                        &mut worklist,
                    );
                }
            }
            Terminator::Return(_) | Terminator::Unreachable => {}
        }
    }

    entry_stacks
        .into_iter()
        .map(|stack| stack.unwrap_or_default())
        .collect()
}

fn literal_name(literal: &MirLit) -> &'static str {
    match literal {
        MirLit::Int(_) => "int",
        MirLit::Float(_) => "float",
        MirLit::Bool(_) => "bool",
        MirLit::Str(_) => "str",
        MirLit::Nothing => "nothing",
        MirLit::FunctionRef(_) => "function_ref",
        MirLit::Namespace(_) => "namespace",
        MirLit::StdlibFn { .. } => "stdlib_fn",
        MirLit::EnumType(_) => "enum_type",
        MirLit::ClassType(_) => "class_type",
    }
}
