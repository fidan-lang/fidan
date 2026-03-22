use crate::context::BackendContext;
use crate::model::{CompileRequest, LtoMode, OptLevel, ToolchainLayout};
use crate::tool::link_codegen_input;
use crate::{dump_ir, env_flag_enabled, trace};
use anyhow::{Context as _, Result, anyhow, bail};
use fidan_ast::{BinOp, UnOp};
use fidan_lexer::Symbol;
use fidan_mir::{
    BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirLit, MirStringPart,
    Operand, Rvalue, Terminator,
};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::OptimizationLevel;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, IntValue,
    PointerValue,
};
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::bit_writer::LLVMWriteBitcodeToMemoryBuffer;
use llvm_sys::core::{LLVMDisposeMessage, LLVMSetTarget};
use llvm_sys::target::{LLVMDisposeTargetData, LLVMSetModuleDataLayout};
use llvm_sys::target_machine::{
    LLVMCreateTargetDataLayout, LLVMCreateTargetMachine, LLVMGetTargetFromTriple,
};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString};
use std::fs;
use std::path::PathBuf;
use std::ptr;

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
    trace("inkwell:create_target_machine");
    let cpu = CString::new("generic").expect("hard-coded CPU string should be valid");
    let features = CString::new("").expect("hard-coded feature string should be valid");
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
            if !cfg!(target_os = "windows") {
                bail!("LLVM LTO is currently supported only on Windows");
            }
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
    trampolines: HashMap<u32, FunctionValue<'ctx>>,
    globals: HashMap<u32, GlobalValue<'ctx>>,
    strings: BTreeMap<String, GlobalValue<'ctx>>,
    next_string_id: usize,
}

struct FunctionState<'m, 'ctx, 'a> {
    module: &'m mut ModuleCodegen<'ctx, 'a>,
    mir_function: MirFunction,
    llvm_function: FunctionValue<'ctx>,
    blocks: HashMap<u32, LlvmBlock<'ctx>>,
    locals: HashMap<u32, PointerValue<'ctx>>,
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
            trampolines: HashMap::new(),
            globals: HashMap::new(),
            strings: BTreeMap::new(),
            next_string_id: 0,
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
        }
        Ok(())
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
        let mut state = FunctionState::new(self, function.clone(), function_value, blocks);
        state.initialize_entry()?;
        state.lower_blocks()?;
        Ok(())
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
        self.declare_runtime_fn("fdn_has_exception", self.i8_type.fn_type(&[], false));
        self.declare_runtime_fn("fdn_catch_exception", self.ptr_type.fn_type(&[], false));
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
                .fn_type(&[self.i8_type.into(), self.ptr_type.into()], false),
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
}

impl<'m, 'ctx, 'a> FunctionState<'m, 'ctx, 'a> {
    fn new(
        module: &'m mut ModuleCodegen<'ctx, 'a>,
        mir_function: MirFunction,
        llvm_function: FunctionValue<'ctx>,
        blocks: HashMap<u32, LlvmBlock<'ctx>>,
    ) -> Self {
        Self {
            module,
            mir_function,
            llvm_function,
            blocks,
            locals: HashMap::new(),
            current_block_id: 0,
            current_block_name: "entry".to_owned(),
            temp_index: 0,
        }
    }

    fn initialize_entry(&mut self) -> Result<()> {
        for local in 0..self.mir_function.local_count {
            let slot = self
                .module
                .builder
                .build_alloca(self.module.ptr_type, &format!("local{local}"))
                .map_err(|err| anyhow!("{err}"))?;
            self.module
                .builder
                .build_store(slot, self.module.ptr_type.const_null())
                .map_err(|err| anyhow!("{err}"))?;
            self.locals.insert(local, slot);
        }

        for (index, param) in self.mir_function.params.iter().enumerate() {
            let arg = self
                .llvm_function
                .get_nth_param(index as u32)
                .ok_or_else(|| anyhow!("missing LLVM param {index}"))?
                .into_pointer_value();
            let slot = self.slot(param.local)?;
            self.module
                .builder
                .build_store(slot, arg)
                .map_err(|err| anyhow!("{err}"))?;
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
            Instr::Assign { dest, rhs, .. } => {
                let value = self.lower_rvalue(rhs)?;
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, value)
                    .map_err(|err| anyhow!("{err}"))?;
                if matches!(rhs, Rvalue::Call { .. }) {
                    self.emit_pending_exception_check(current_catch_stack)?;
                }
                Ok(())
            }
            Instr::Call {
                dest, callee, args, ..
            } => {
                let value = self.lower_call(callee, args)?;
                if let Some(dest) = dest {
                    let slot = self.slot(*dest)?;
                    self.module
                        .builder
                        .build_store(slot, value)
                        .map_err(|err| anyhow!("{err}"))?;
                }
                self.emit_pending_exception_check(current_catch_stack)?;
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
                let object = self.lower_operand(object)?;
                let (field_ptr, field_len) = self.string_ptr(*field)?;
                let value = self.call_ptr(
                    "fdn_obj_get_field",
                    &[object.into(), field_ptr.into(), field_len.into()],
                )?;
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, value)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::GetIndex {
                dest,
                object,
                index,
            } => {
                let object = self.lower_operand(object)?;
                let index = self.lower_operand(index)?;
                let value = self.call_ptr("fdn_list_get", &[object.into(), index.into()])?;
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, value)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::SetIndex {
                object,
                index,
                value,
            } => {
                let object = self.lower_operand(object)?;
                let index = self.lower_operand(index)?;
                let value = self.lower_operand(value)?;
                self.call_void("fdn_list_set", &[object.into(), index.into(), value.into()])
            }
            Instr::Drop { .. } | Instr::Nop | Instr::PushCatch(..) | Instr::PopCatch => Ok(()),
            Instr::CertainCheck { operand, name } => {
                let operand = self.lower_operand(operand)?;
                let (name_ptr, name_len) = self.string_ptr(*name)?;
                self.call_void(
                    "fdn_certain_check",
                    &[operand.into(), name_ptr.into(), name_len.into()],
                )
            }
            Instr::LoadGlobal { dest, global } => {
                let global = self.global(*global)?;
                let name = self.temp("gload");
                let value = self
                    .module
                    .builder
                    .build_load(self.module.ptr_type, global.as_pointer_value(), &name)
                    .map_err(|err| anyhow!("{err}"))?
                    .into_pointer_value();
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, value)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::StoreGlobal { global, value } => {
                let value = self.lower_operand(value)?;
                let global = self.global(*global)?;
                self.module
                    .builder
                    .build_store(global.as_pointer_value(), value)
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
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, pending)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::SpawnConcurrent {
                handle,
                task_fn,
                args,
            }
            | Instr::SpawnParallel {
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
                let slot = self.slot(*handle)?;
                self.module
                    .builder
                    .build_store(slot, pending)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::JoinAll { handles } => {
                for handle in handles {
                    let handle_value = self.lower_operand(&Operand::Local(*handle))?;
                    let resolved = self.call_ptr("fdn_pending_join", &[handle_value.into()])?;
                    let slot = self.slot(*handle)?;
                    self.module
                        .builder
                        .build_store(slot, resolved)
                        .map_err(|err| anyhow!("{err}"))?;
                }
                Ok(())
            }
            Instr::AwaitPending { dest, handle } => {
                let handle_value = self.lower_operand(handle)?;
                let resolved = self.call_ptr("fdn_pending_join", &[handle_value.into()])?;
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, resolved)
                    .map_err(|err| anyhow!("{err}"))?;
                Ok(())
            }
            Instr::SpawnDynamic { dest, method, args } => {
                let result = if let Some(method) = method {
                    let receiver = self.lower_operand(&args[0])?;
                    let call_args = args[1..]
                        .iter()
                        .map(|arg| self.lower_operand(arg))
                        .collect::<Result<Vec<_>>>()?;
                    let (array_ptr, count) = self.build_ptr_array(&call_args)?;
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
                    )?
                } else {
                    let function_value = self.lower_operand(&args[0])?;
                    let call_args = args[1..]
                        .iter()
                        .map(|arg| self.lower_operand(arg))
                        .collect::<Result<Vec<_>>>()?;
                    let (array_ptr, count) = self.build_ptr_array(&call_args)?;
                    self.call_ptr(
                        "fdn_call_dynamic",
                        &[function_value.into(), array_ptr.into(), count.into()],
                    )?
                };
                let slot = self.slot(*dest)?;
                self.module
                    .builder
                    .build_store(slot, result)
                    .map_err(|err| anyhow!("{err}"))?;
                self.emit_pending_exception_check(current_catch_stack)?;
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
                )
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
                let cond = self.lower_operand(cond)?;
                let truthy = self.call_i8("fdn_truthy", &[cond.into()])?;
                let cmp_name = self.temp("cmp");
                let cmp = self
                    .module
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        truthy,
                        self.module.i8_type.const_zero(),
                        &cmp_name,
                    )
                    .map_err(|err| anyhow!("{err}"))?;
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
            let value = match incoming {
                Some(operand) => self.lower_operand(&operand)?,
                None => self.call_ptr("fdn_box_nothing", &[])?,
            };
            let slot = self.slot(phi.result)?;
            self.module
                .builder
                .build_store(slot, value)
                .map_err(|err| anyhow!("{err}"))?;
        }

        Ok(())
    }

    fn lower_rvalue(&mut self, rhs: &Rvalue) -> Result<PointerValue<'ctx>> {
        match rhs {
            Rvalue::Use(operand) => self.lower_operand(operand),
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
            Rvalue::Tuple(values) => self.lower_list(values),
            Rvalue::StringInterp(parts) => self.lower_string_interp(parts),
            Rvalue::Literal(literal) => self.lower_literal(literal),
            Rvalue::CatchException => self.call_ptr("fdn_catch_exception", &[]),
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
            _ => bail!(
                "LLVM backend subset does not support rvalue `{}` yet in function `{}`",
                rvalue_name(rhs),
                self.module.backend.symbol_name(self.mir_function.name)?
            ),
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
                let args = args
                    .iter()
                    .map(|arg| self.lower_operand(arg).map(Into::into))
                    .collect::<Result<Vec<BasicMetadataValueEnum<'ctx>>>>()?;
                self.call_decl(function, &args)
            }
            Callee::Method { receiver, method } => {
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
                let msg = if let Some(arg) = args.get(1) {
                    self.lower_operand(arg)?
                } else {
                    self.lower_literal(&MirLit::Str("assertion failed".to_owned()))?
                };
                self.call_void("fdn_assert", &[truthy.into(), msg.into()])?;
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
                let slot = self.slot(*local)?;
                let load_name = self.temp("load");
                Ok(self
                    .module
                    .builder
                    .build_load(self.module.ptr_type, slot, &load_name)
                    .map_err(|err| anyhow!("{err}"))?
                    .into_pointer_value())
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

fn rvalue_name(rvalue: &Rvalue) -> &'static str {
    match rvalue {
        Rvalue::Use(..) => "use",
        Rvalue::Binary { .. } => "binary",
        Rvalue::Unary { .. } => "unary",
        Rvalue::NullCoalesce { .. } => "null-coalesce",
        Rvalue::Call { .. } => "call",
        Rvalue::Construct { .. } => "construct",
        Rvalue::List(..) => "list",
        Rvalue::Dict(..) => "dict",
        Rvalue::Tuple(..) => "tuple",
        Rvalue::StringInterp(..) => "string-interp",
        Rvalue::Literal(..) => "literal",
        Rvalue::CatchException => "catch-exception",
        Rvalue::MakeClosure { .. } => "make-closure",
        Rvalue::Slice { .. } => "slice",
        Rvalue::ConstructEnum { .. } => "construct-enum",
        Rvalue::EnumTagCheck { .. } => "enum-tag-check",
        Rvalue::EnumPayload { .. } => "enum-payload",
    }
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
