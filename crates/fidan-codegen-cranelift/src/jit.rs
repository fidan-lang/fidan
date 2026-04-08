// fidan-codegen-cranelift/src/jit.rs
//
// Cranelift JIT backend for Fidan.
//
// Compiles `@precompile`-annotated `MirFunction`s (and hot interpreter functions
// above the JIT threshold) to native machine code via the Cranelift JIT.
//
// # ABI Convention
//
// All compiled functions use a unified i64 boundary:
//   - Integer params  → passed as i64 (the value itself)
//   - Float params    → passed as i64 (f64 bit pattern)
//   - Boolean params  → passed as i64 (0 or 1)
//   - Boxed values    → passed as i64 (scoped `*mut FidanValue`)
//   - Return value    → same encoding as above
//
// This keeps the Rust trampoline simple while still allowing native JIT
// compilation for tuple-valued functions. Boxed values are tracked in a
// thread-local scope for the duration of the active native JIT call chain and
// are released automatically when the outermost call returns.

use cranelift_codegen::ir::{
    AbiParam, Block, BlockArg, Function, InstBuilder, MemFlags, TrapCode, UserFuncName, Value,
    condcodes::{FloatCC, IntCC},
    types::{F64, I8, I64},
};
use cranelift_codegen::{Context, settings, settings::Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, FuncId, Linkage, Module as CraneliftModule};
use fidan_lexer::SymbolInterner;
use fidan_mir::{
    BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirLit, MirProgram, MirTy,
    Operand, Rvalue, Terminator, collect_effective_local_types,
};
use fidan_runtime::FidanValue;
use fidan_stdlib::{
    MathIntrinsic, StdlibIntrinsic, StdlibValueKind, infer_stdlib_method, is_stdlib_module,
};
use libffi::middle::{Cif, CodePtr, Type, arg};
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;

type JitLoadGlobalRawFn = unsafe fn(ctx: *mut c_void, global_id: u32) -> i64;
type JitStoreGlobalRawFn = unsafe fn(ctx: *mut c_void, global_id: u32, raw: i64);
type JitCallFnRawFn =
    unsafe fn(ctx: *mut c_void, fn_id: u32, args_ptr: *const i64, arg_cnt: i64) -> i64;

#[derive(Clone, Copy)]
pub struct JitRuntimeHooks {
    pub load_global_raw: JitLoadGlobalRawFn,
    pub store_global_raw: JitStoreGlobalRawFn,
    pub call_fn_raw: JitCallFnRawFn,
}

static JIT_RUNTIME_HOOKS: OnceLock<JitRuntimeHooks> = OnceLock::new();

thread_local! {
    static ACTIVE_JIT_CONTEXT: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    static ACTIVE_JIT_ABI_DEPTH: Cell<u32> = const { Cell::new(0) };
    static ACTIVE_JIT_ABI_VALUES: RefCell<Vec<*mut FidanValue>> = const { RefCell::new(Vec::new()) };
}

pub fn register_jit_runtime_hooks(hooks: JitRuntimeHooks) {
    let _ = JIT_RUNTIME_HOOKS.set(hooks);
}

pub fn with_jit_runtime_context<T>(ctx: *mut c_void, f: impl FnOnce() -> T) -> T {
    ACTIVE_JIT_CONTEXT.with(|cell| {
        let previous = cell.replace(ctx);
        enter_jit_abi_scope();
        let result = f();
        exit_jit_abi_scope();
        cell.set(previous);
        result
    })
}

fn active_jit_runtime_hooks() -> &'static JitRuntimeHooks {
    JIT_RUNTIME_HOOKS
        .get()
        .expect("JIT runtime hooks must be registered before executing JIT code")
}

fn active_jit_context() -> *mut c_void {
    ACTIVE_JIT_CONTEXT.with(Cell::get)
}

fn enter_jit_abi_scope() {
    ACTIVE_JIT_ABI_DEPTH.with(|depth| {
        let current = depth.get();
        if current == 0 {
            ACTIVE_JIT_ABI_VALUES.with(|values| values.borrow_mut().clear());
        }
        depth.set(current + 1);
    });
}

fn exit_jit_abi_scope() {
    ACTIVE_JIT_ABI_DEPTH.with(|depth| {
        let current = depth.get();
        let next = current.saturating_sub(1);
        depth.set(next);
        if next == 0 {
            ACTIVE_JIT_ABI_VALUES.with(|values| {
                for ptr in values.borrow_mut().drain(..).rev() {
                    if !ptr.is_null() {
                        unsafe { drop(Box::from_raw(ptr)) };
                    }
                }
            });
        }
    });
}

fn register_jit_abi_ptr(ptr: *mut FidanValue) -> i64 {
    if !ptr.is_null() {
        ACTIVE_JIT_ABI_VALUES.with(|values| values.borrow_mut().push(ptr));
    }
    ptr as i64
}

pub fn encode_jit_abi_value(value: &FidanValue, ty: &MirTy) -> i64 {
    match (value, ty) {
        (FidanValue::Integer(n), MirTy::Integer) => *n,
        (FidanValue::Float(f), MirTy::Float) => f.to_bits() as i64,
        (FidanValue::Boolean(b), MirTy::Boolean) => i64::from(*b),
        _ => register_jit_abi_ptr(Box::into_raw(Box::new(value.clone()))),
    }
}

pub fn decode_jit_abi_value(raw: i64, ty: &MirTy) -> FidanValue {
    match ty {
        MirTy::Integer => FidanValue::Integer(raw),
        MirTy::Float => FidanValue::Float(f64::from_bits(raw as u64)),
        MirTy::Boolean => FidanValue::Boolean(raw != 0),
        _ if raw == 0 => FidanValue::Nothing,
        _ => unsafe { (*(raw as *mut FidanValue)).clone() },
    }
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_box_int_scoped(v: i64) -> i64 {
    register_jit_abi_ptr(fidan_runtime::ffi::fdn_box_int(v))
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_box_float_scoped(v: f64) -> i64 {
    register_jit_abi_ptr(fidan_runtime::ffi::fdn_box_float(v))
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_box_bool_scoped(v: i8) -> i64 {
    register_jit_abi_ptr(fidan_runtime::ffi::fdn_box_bool(v))
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_box_handle_scoped(v: i64) -> i64 {
    register_jit_abi_ptr(fidan_runtime::ffi::fdn_box_handle(v as usize))
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_box_nothing_scoped() -> i64 {
    register_jit_abi_ptr(fidan_runtime::ffi::fdn_box_nothing())
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fdn_jit_box_str_scoped(bytes: *const u8, len: i64) -> i64 {
    register_jit_abi_ptr(unsafe { fidan_runtime::ffi::fdn_box_str(bytes, len) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fdn_jit_tuple_pack_scoped(values_ptr: *const i64, values_count: i64) -> i64 {
    register_jit_abi_ptr(unsafe {
        fidan_runtime::ffi::fdn_tuple_pack(values_ptr as *const *mut FidanValue, values_count)
    })
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_load_global_raw(global_id: i64) -> i64 {
    let hooks = active_jit_runtime_hooks();
    let ctx = active_jit_context();
    unsafe { (hooks.load_global_raw)(ctx, global_id as u32) }
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_store_global_raw(global_id: i64, raw: i64) {
    let hooks = active_jit_runtime_hooks();
    let ctx = active_jit_context();
    unsafe { (hooks.store_global_raw)(ctx, global_id as u32, raw) }
}

#[unsafe(no_mangle)]
extern "C" fn fdn_jit_call_fn_raw(fn_id: i64, args_ptr: *const i64, arg_cnt: i64) -> i64 {
    let hooks = active_jit_runtime_hooks();
    let ctx = active_jit_context();
    unsafe { (hooks.call_fn_raw)(ctx, fn_id as u32, args_ptr, arg_cnt) }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Map a `MirTy` to a Cranelift type.
/// Returns `None` for types that cannot be JIT-compiled (strings, objects, etc.).
fn mir_ty_to_cl(ty: &MirTy) -> Option<cranelift_codegen::ir::Type> {
    Some(match ty {
        MirTy::Float => F64,
        MirTy::Boolean => I8,
        _ => I64,
    })
}

/// Returns `true` for MIR types that can cross the native JIT ABI boundary.
fn is_jit_abi_supported(ty: &MirTy) -> bool {
    mir_ty_to_cl(ty).is_some()
}

/// Cranelift type to use for the I64-ABI boundary.
const ABI_TY: cranelift_codegen::ir::Type = I64;

// ── Build a local-type map ─────────────────────────────────────────────────────

fn build_local_type_map(
    func: &MirFunction,
    program: &MirProgram,
    interner: &SymbolInterner,
) -> HashMap<LocalId, MirTy> {
    collect_effective_local_types(func, program, |symbol| {
        Some(interner.resolve(symbol).to_string())
    })
    .into_iter()
    .map(|(local, ty)| (LocalId(local), ty))
    .collect()
}

// ── JitCompiler ────────────────────────────────────────────────────────────────

/// Metadata about a compiled JIT function needed to call it from the Rust trampoline.
#[derive(Clone)]
pub struct JitFnEntry {
    /// Raw function pointer (unsafe to call — use `call_jit_fn` below).
    pub fn_ptr: Option<*const u8>,
    /// Logical parameter types (in same order as `MirFunction::params`).
    pub param_tys: Vec<MirTy>,
    /// Logical return type.
    pub return_ty: MirTy,
}

impl JitFnEntry {
    fn fallback(func: &MirFunction) -> Self {
        Self {
            fn_ptr: None,
            param_tys: func.params.iter().map(|p| p.ty.clone()).collect(),
            return_ty: func.return_ty.clone(),
        }
    }

    pub fn is_native(&self) -> bool {
        self.fn_ptr.is_some()
    }
}

// SAFETY: `fn_ptr` is a pointer into mmap-backed JIT memory that lives as long
// as the `JITModule` that produced it (owned by `JitCompiler`).  Since
// `JitCompiler` is only used from a single thread (compilation + runtime
// dispatch use the same thread), this is sound.
unsafe impl Send for JitFnEntry {}
unsafe impl Sync for JitFnEntry {}

/// The Cranelift JIT backend.
///
/// Owns the `JITModule` which must outlive all `JitFnEntry::fn_ptr` values it
/// has produced.  Callers must not drop this struct while JIT functions are
/// still being executed.
pub struct JitCompiler {
    module: JITModule,
    ctx: Context,
    builder_ctx: FunctionBuilderContext,
    load_global_raw_id: FuncId,
    store_global_raw_id: FuncId,
    call_fn_raw_id: FuncId,
    box_int_scoped_id: FuncId,
    box_float_scoped_id: FuncId,
    box_bool_scoped_id: FuncId,
    box_handle_scoped_id: FuncId,
    box_nothing_scoped_id: FuncId,
    box_str_scoped_id: FuncId,
    tuple_pack_scoped_id: FuncId,
    /// Counter used to generate unique function names.
    fn_counter: u32,
}

impl Default for JitCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl JitCompiler {
    pub fn new() -> Self {
        let mut flag_builder = settings::builder();
        // Enable Cranelift speed optimizations — without this the JIT output is
        // essentially unoptimised and often slower than the release-mode interpreter.
        flag_builder
            .set("opt_level", "speed")
            .expect("Cranelift: unknown opt_level flag");
        let flags = settings::Flags::new(flag_builder);
        let isa = cranelift_native::builder()
            .expect("cranelift-native: unsupported host")
            .finish(flags)
            .expect("cranelift-native: failed to build ISA");
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol(
            "fdn_jit_load_global_raw",
            fdn_jit_load_global_raw as *const u8,
        );
        builder.symbol(
            "fdn_jit_store_global_raw",
            fdn_jit_store_global_raw as *const u8,
        );
        builder.symbol("fdn_jit_call_fn_raw", fdn_jit_call_fn_raw as *const u8);
        builder.symbol(
            "fdn_jit_box_int_scoped",
            fdn_jit_box_int_scoped as *const u8,
        );
        builder.symbol(
            "fdn_jit_box_float_scoped",
            fdn_jit_box_float_scoped as *const u8,
        );
        builder.symbol(
            "fdn_jit_box_bool_scoped",
            fdn_jit_box_bool_scoped as *const u8,
        );
        builder.symbol(
            "fdn_jit_box_handle_scoped",
            fdn_jit_box_handle_scoped as *const u8,
        );
        builder.symbol(
            "fdn_jit_box_nothing_scoped",
            fdn_jit_box_nothing_scoped as *const u8,
        );
        builder.symbol(
            "fdn_jit_box_str_scoped",
            fdn_jit_box_str_scoped as *const u8,
        );
        builder.symbol(
            "fdn_jit_tuple_pack_scoped",
            fdn_jit_tuple_pack_scoped as *const u8,
        );
        let mut module = JITModule::new(builder);
        let load_global_raw_id = {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(I64));
            sig.returns.push(AbiParam::new(I64));
            module
                .declare_function("fdn_jit_load_global_raw", Linkage::Import, &sig)
                .expect("declare fdn_jit_load_global_raw")
        };
        let store_global_raw_id = {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(I64));
            sig.params.push(AbiParam::new(I64));
            module
                .declare_function("fdn_jit_store_global_raw", Linkage::Import, &sig)
                .expect("declare fdn_jit_store_global_raw")
        };
        let call_fn_raw_id = {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(I64));
            sig.params.push(AbiParam::new(I64));
            sig.params.push(AbiParam::new(I64));
            sig.returns.push(AbiParam::new(I64));
            module
                .declare_function("fdn_jit_call_fn_raw", Linkage::Import, &sig)
                .expect("declare fdn_jit_call_fn_raw")
        };
        let box_int_scoped_id =
            declare_import_fn(&mut module, "fdn_jit_box_int_scoped", &[I64], Some(I64));
        let box_float_scoped_id =
            declare_import_fn(&mut module, "fdn_jit_box_float_scoped", &[F64], Some(I64));
        let box_bool_scoped_id =
            declare_import_fn(&mut module, "fdn_jit_box_bool_scoped", &[I8], Some(I64));
        let box_handle_scoped_id =
            declare_import_fn(&mut module, "fdn_jit_box_handle_scoped", &[I64], Some(I64));
        let box_nothing_scoped_id =
            declare_import_fn(&mut module, "fdn_jit_box_nothing_scoped", &[], Some(I64));
        let box_str_scoped_id = declare_import_fn(
            &mut module,
            "fdn_jit_box_str_scoped",
            &[I64, I64],
            Some(I64),
        );
        let tuple_pack_scoped_id = declare_import_fn(
            &mut module,
            "fdn_jit_tuple_pack_scoped",
            &[I64, I64],
            Some(I64),
        );
        let ctx = module.make_context();
        let builder_ctx = FunctionBuilderContext::new();
        Self {
            module,
            ctx,
            builder_ctx,
            load_global_raw_id,
            store_global_raw_id,
            call_fn_raw_id,
            box_int_scoped_id,
            box_float_scoped_id,
            box_bool_scoped_id,
            box_handle_scoped_id,
            box_nothing_scoped_id,
            box_str_scoped_id,
            tuple_pack_scoped_id,
            fn_counter: 0,
        }
    }

    pub fn compile_function(
        &mut self,
        func: &MirFunction,
        program: &MirProgram,
        interner: &SymbolInterner,
    ) -> JitFnEntry {
        self.try_compile_native(func, program, interner)
            .unwrap_or_else(|| JitFnEntry::fallback(func))
    }

    /// Attempt to JIT-compile `func` to native code.
    ///
    /// Returns `None` when the function uses constructs that the native
    /// Cranelift lowering does not yet support. The public `compile_function`
    /// API wraps that case in a fallback entry so the hot-path manager still
    /// handles the function cleanly.
    fn try_compile_native(
        &mut self,
        func: &MirFunction,
        program: &MirProgram,
        interner: &SymbolInterner,
    ) -> Option<JitFnEntry> {
        // ── Eligibility check ─────────────────────────────────────────────────
        for p in &func.params {
            if !is_jit_abi_supported(&p.ty) {
                return None;
            }
        }
        if !is_jit_abi_supported(&func.return_ty) {
            return None;
        }

        // ── Build auxiliary maps ──────────────────────────────────────────────
        let local_types = build_local_type_map(func, program, interner);

        // Map GlobalId → stdlib module name, identified via use_decls
        let mut global_ns_map: HashMap<GlobalId, String> = HashMap::new();
        for (i, g) in program.globals.iter().enumerate() {
            let g_name = interner.resolve(g.name);
            for decl in &program.use_decls {
                if decl.is_stdlib
                    && decl.specific_names.is_none()
                    && g_name.as_ref() == decl.alias.as_str()
                {
                    global_ns_map.insert(GlobalId(i as u32), decl.module.clone());
                }
            }
        }

        // ── Set up the Cranelift function ─────────────────────────────────────
        let func_name = format!("jit_fn_{}", self.fn_counter);
        self.fn_counter += 1;

        // Signature: all params as I64, return as I64
        let mut sig = self.module.make_signature();
        for _ in &func.params {
            sig.params.push(AbiParam::new(ABI_TY));
        }
        sig.returns.push(AbiParam::new(ABI_TY));

        self.ctx.func =
            Function::with_name_signature(UserFuncName::testcase(func_name.as_str()), sig.clone());

        // ── Build the IR ──────────────────────────────────────────────────────
        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);

            // Pre-create Cranelift blocks
            let cl_blocks: Vec<Block> =
                func.blocks.iter().map(|_| builder.create_block()).collect();

            // For blocks with phi nodes (non-entry), prepend block params.
            // phi_param_vals[(bi, pi)] = Cranelift Value for bb_i's pi-th phi.
            let mut phi_param_vals: HashMap<(usize, usize), Value> = HashMap::new();
            for (bi, mir_bb) in func.blocks.iter().enumerate() {
                if bi == 0 {
                    // Entry block: add function signature params.
                    builder.append_block_params_for_function_params(cl_blocks[0]);
                } else {
                    for (pi, phi) in mir_bb.phis.iter().enumerate() {
                        // Use local_types (which infers from operands) rather than phi.ty
                        // directly — phi.ty is often Dynamic for loop-carried variables.
                        let phi_cl_ty = local_types
                            .get(&phi.result)
                            .and_then(mir_ty_to_cl)
                            .unwrap_or(I64);
                        let pval = builder.append_block_param(cl_blocks[bi], phi_cl_ty);
                        phi_param_vals.insert((bi, pi), pval);
                    }
                }
            }

            // Per-function namespace tracking: LocalId → stdlib module name
            let mut namespace_locals: HashMap<LocalId, String> = HashMap::new();
            let load_global_raw_ref = self
                .module
                .declare_func_in_func(self.load_global_raw_id, builder.func);
            let store_global_raw_ref = self
                .module
                .declare_func_in_func(self.store_global_raw_id, builder.func);
            let call_fn_raw_ref = self
                .module
                .declare_func_in_func(self.call_fn_raw_id, builder.func);
            let rt = JitRuntimeRefs {
                box_int_scoped_ref: self
                    .module
                    .declare_func_in_func(self.box_int_scoped_id, builder.func),
                box_float_scoped_ref: self
                    .module
                    .declare_func_in_func(self.box_float_scoped_id, builder.func),
                box_bool_scoped_ref: self
                    .module
                    .declare_func_in_func(self.box_bool_scoped_id, builder.func),
                box_handle_scoped_ref: self
                    .module
                    .declare_func_in_func(self.box_handle_scoped_id, builder.func),
                box_nothing_scoped_ref: self
                    .module
                    .declare_func_in_func(self.box_nothing_scoped_id, builder.func),
                box_str_scoped_ref: self
                    .module
                    .declare_func_in_func(self.box_str_scoped_id, builder.func),
                tuple_pack_scoped_ref: self
                    .module
                    .declare_func_in_func(self.tuple_pack_scoped_id, builder.func),
            };

            // Declare Cranelift Variables for every MIR local.
            let num_locals = func.local_count as usize;
            let mut cl_vars: Vec<Variable> = Vec::with_capacity(num_locals);
            for i in 0..num_locals {
                let cl_ty = local_types
                    .get(&LocalId(i as u32))
                    .and_then(mir_ty_to_cl)
                    .unwrap_or(I64);
                let var = builder.declare_var(cl_ty);
                cl_vars.push(var);
            }

            // ── Entry block: bind function arguments ──────────────────────────
            builder.switch_to_block(cl_blocks[0]);
            {
                let entry_params: Vec<Value> = builder.block_params(cl_blocks[0]).to_vec();
                for (idx, param) in func.params.iter().enumerate() {
                    let raw_i64 = entry_params[idx];
                    let actual = abi_i64_to_native(&mut builder, raw_i64, &param.ty);
                    builder.def_var(cl_vars[param.local.0 as usize], actual);
                }
            }

            // ── Compile each basic block ──────────────────────────────────────
            for (bi, mir_bb) in func.blocks.iter().enumerate() {
                if bi > 0 {
                    builder.switch_to_block(cl_blocks[bi]);
                    // Bind phi block params to their SSA Variables
                    for (pi, phi) in mir_bb.phis.iter().enumerate() {
                        if let Some(&phi_val) = phi_param_vals.get(&(bi, pi)) {
                            builder.def_var(cl_vars[phi.result.0 as usize], phi_val);
                        }
                    }
                }

                // Emit instructions
                for instr in &mir_bb.instructions {
                    match instr {
                        Instr::Assign { dest, ty, rhs } => {
                            let effective_ty = local_types.get(dest).unwrap_or(ty);
                            let emit_ctx = RvalueEmitCtx {
                                vars: &cl_vars,
                                local_types: &local_types,
                                ns_locals: &namespace_locals,
                                interner,
                                call_fn_raw_ref,
                                rt: &rt,
                            };
                            let val = emit_rvalue(
                                &mut self.module,
                                &mut builder,
                                rhs,
                                effective_ty,
                                &emit_ctx,
                            )?;
                            builder.def_var(cl_vars[dest.0 as usize], val);
                        }

                        Instr::LoadGlobal { dest, global } => {
                            if let Some(ns) = global_ns_map.get(global) {
                                namespace_locals.insert(*dest, ns.clone());
                                let dummy = builder.ins().iconst(I64, 0);
                                builder.def_var(cl_vars[dest.0 as usize], dummy);
                            } else {
                                namespace_locals.remove(dest);
                                let raw = emit_load_global_raw(
                                    &mut builder,
                                    load_global_raw_ref,
                                    *global,
                                );
                                let dest_ty = local_types.get(dest).unwrap_or(&MirTy::Dynamic);
                                let actual = abi_i64_to_native(&mut builder, raw, dest_ty);
                                builder.def_var(cl_vars[dest.0 as usize], actual);
                            }
                        }

                        Instr::Call {
                            dest, callee, args, ..
                        } => {
                            let val = match callee {
                                Callee::Method { receiver, method } => {
                                    let ns = stdlib_namespace(receiver, &namespace_locals)?;
                                    let mname = interner.resolve(*method);
                                    emit_stdlib_method_call(
                                        &mut builder,
                                        &cl_vars,
                                        &local_types,
                                        ns.as_str(),
                                        mname.as_ref(),
                                        args,
                                    )?
                                }
                                Callee::Fn(fid) => {
                                    let result_ty = dest
                                        .and_then(|d| local_types.get(&d))
                                        .unwrap_or(&MirTy::Nothing);
                                    emit_call_fn_raw(
                                        &mut builder,
                                        &cl_vars,
                                        &local_types,
                                        call_fn_raw_ref,
                                        *fid,
                                        args,
                                        result_ty,
                                    )?
                                }
                                _ => return None,
                            };
                            if let Some(d) = dest {
                                builder.def_var(cl_vars[d.0 as usize], val);
                            }
                        }
                        Instr::StoreGlobal { global, value } => {
                            let native_value = load_operand(&mut builder, &cl_vars, value);
                            let global_ty = program
                                .globals
                                .get(global.0 as usize)
                                .map(|g| &g.ty)
                                .unwrap_or(&MirTy::Dynamic);
                            let raw = native_to_abi_i64(&mut builder, native_value, global_ty);
                            emit_store_global_raw(&mut builder, store_global_raw_ref, *global, raw);
                        }

                        // Abort native lowering on any unsupported instruction.
                        // The public JIT entry will transparently fall back to
                        // interpreter execution for this function.
                        Instr::PushCatch(_)
                        | Instr::PopCatch
                        | Instr::SpawnConcurrent { .. }
                        | Instr::SpawnParallel { .. }
                        | Instr::SpawnExpr { .. }
                        | Instr::SpawnDynamic { .. }
                        | Instr::AwaitPending { .. }
                        | Instr::JoinAll { .. }
                        | Instr::ParallelIter { .. }
                        | Instr::SetField { .. }
                        | Instr::GetField { .. }
                        | Instr::SetIndex { .. }
                        | Instr::GetIndex { .. } => return None,

                        Instr::CertainCheck { .. } => {}
                        Instr::Drop { .. } | Instr::Nop => {}
                    }
                }

                // ── Emit terminator ───────────────────────────────────────────
                match &mir_bb.terminator {
                    Terminator::Return(None) => {
                        let zero = builder.ins().iconst(ABI_TY, 0);
                        builder.ins().return_(&[zero]);
                    }

                    Terminator::Return(Some(op)) => {
                        let val = load_operand(&mut builder, &cl_vars, op);
                        let ret_ty = &func.return_ty;
                        let i64_val = native_to_abi_i64(&mut builder, val, ret_ty);
                        builder.ins().return_(&[i64_val]);
                    }

                    Terminator::Goto(target) => {
                        let args = collect_phi_args(
                            &mut builder,
                            &cl_vars,
                            &local_types,
                            func,
                            BlockId(bi as u32),
                            *target,
                        );
                        builder.ins().jump(cl_blocks[target.0 as usize], &args);
                    }

                    Terminator::Branch {
                        cond,
                        then_bb,
                        else_bb,
                    } => {
                        let cond_val = load_operand(&mut builder, &cl_vars, cond);
                        let cond_ty = builder.func.dfg.value_type(cond_val);
                        // brif needs any-int condition (non-zero = true)
                        let cond_i64 = match cond_ty {
                            t if t == I8 => builder.ins().uextend(I64, cond_val),
                            t if t == F64 => {
                                let zero = builder.ins().f64const(0.0);
                                let flag = builder.ins().fcmp(FloatCC::NotEqual, cond_val, zero);
                                builder.ins().uextend(I64, flag)
                            }
                            _ => cond_val,
                        };

                        let then_args = collect_phi_args(
                            &mut builder,
                            &cl_vars,
                            &local_types,
                            func,
                            BlockId(bi as u32),
                            *then_bb,
                        );
                        let else_args = collect_phi_args(
                            &mut builder,
                            &cl_vars,
                            &local_types,
                            func,
                            BlockId(bi as u32),
                            *else_bb,
                        );

                        builder.ins().brif(
                            cond_i64,
                            cl_blocks[then_bb.0 as usize],
                            &then_args,
                            cl_blocks[else_bb.0 as usize],
                            &else_args,
                        );
                    }

                    Terminator::Unreachable => {
                        builder.ins().trap(TrapCode::unwrap_user(1));
                    }

                    Terminator::Throw { .. } => return None,
                }
            }

            builder.seal_all_blocks();
            builder.finalize();
        }

        // ── Register and compile ──────────────────────────────────────────────
        let func_id = self
            .module
            .declare_function(&func_name, Linkage::Local, &sig)
            .ok()?;
        self.module.define_function(func_id, &mut self.ctx).ok()?;
        self.module.clear_context(&mut self.ctx);
        self.module.finalize_definitions().ok()?;

        let fn_ptr = self.module.get_finalized_function(func_id);

        Some(JitFnEntry {
            fn_ptr: Some(fn_ptr),
            param_tys: func.params.iter().map(|p| p.ty.clone()).collect(),
            return_ty: func.return_ty.clone(),
        })
    }
}

// ── Rvalue emission ────────────────────────────────────────────────────────────

struct RvalueEmitCtx<'a> {
    vars: &'a [Variable],
    local_types: &'a HashMap<LocalId, MirTy>,
    ns_locals: &'a HashMap<LocalId, String>,
    interner: &'a SymbolInterner,
    call_fn_raw_ref: cranelift_codegen::ir::FuncRef,
    rt: &'a JitRuntimeRefs,
}

struct JitRuntimeRefs {
    box_int_scoped_ref: cranelift_codegen::ir::FuncRef,
    box_float_scoped_ref: cranelift_codegen::ir::FuncRef,
    box_bool_scoped_ref: cranelift_codegen::ir::FuncRef,
    box_handle_scoped_ref: cranelift_codegen::ir::FuncRef,
    box_nothing_scoped_ref: cranelift_codegen::ir::FuncRef,
    box_str_scoped_ref: cranelift_codegen::ir::FuncRef,
    tuple_pack_scoped_ref: cranelift_codegen::ir::FuncRef,
}

fn emit_rvalue(
    module: &mut JITModule,
    builder: &mut FunctionBuilder,
    rhs: &Rvalue,
    dest_ty: &MirTy,
    ctx: &RvalueEmitCtx<'_>,
) -> Option<Value> {
    match rhs {
        Rvalue::Literal(lit) => Some(emit_literal(builder, lit)),

        Rvalue::Use(op) => Some(load_operand(builder, ctx.vars, op)),

        Rvalue::Binary { op, lhs, rhs } => {
            let lv = load_operand(builder, ctx.vars, lhs);
            let rv = load_operand(builder, ctx.vars, rhs);
            emit_binop(builder, *op, lv, rv, dest_ty)
        }

        Rvalue::Unary { op, operand } => {
            let v = load_operand(builder, ctx.vars, operand);
            emit_unop(builder, *op, v, dest_ty)
        }

        Rvalue::Call {
            callee: Callee::Fn(fid),
            args,
        } => emit_call_fn_raw(
            builder,
            ctx.vars,
            ctx.local_types,
            ctx.call_fn_raw_ref,
            *fid,
            args,
            dest_ty,
        ),

        Rvalue::Tuple(elems) => emit_tuple_rvalue(module, builder, elems, ctx),

        Rvalue::Call {
            callee:
                Callee::Method {
                    receiver: Operand::Local(recv),
                    method,
                },
            args,
        } => {
            let ns = ctx.ns_locals.get(recv)?;
            let mname = ctx.interner.resolve(*method);
            emit_stdlib_method_call(
                builder,
                ctx.vars,
                ctx.local_types,
                ns.as_ref(),
                mname.as_ref(),
                args,
            )
        }

        _ => None,
    }
}

fn emit_tuple_rvalue(
    module: &mut JITModule,
    builder: &mut FunctionBuilder,
    elems: &[Operand],
    ctx: &RvalueEmitCtx<'_>,
) -> Option<Value> {
    let mut items = Vec::with_capacity(elems.len());
    for elem in elems {
        items.push(box_operand_for_abi(module, builder, elem, ctx)?);
    }
    let (items_ptr, item_count) = stack_i64_array(builder, &items);
    call_runtime(
        builder,
        ctx.rt.tuple_pack_scoped_ref,
        &[items_ptr, item_count],
    )
}

fn box_operand_for_abi(
    module: &mut JITModule,
    builder: &mut FunctionBuilder,
    op: &Operand,
    ctx: &RvalueEmitCtx<'_>,
) -> Option<Value> {
    match op {
        Operand::Local(local) => {
            let value = builder.use_var(ctx.vars[local.0 as usize]);
            let ty = ctx.local_types.get(local).unwrap_or(&MirTy::Dynamic);
            box_value_for_abi(module, builder, value, ty, ctx)
        }
        Operand::Const(lit) => box_const_for_abi(module, builder, lit, ctx),
    }
}

fn box_value_for_abi(
    module: &mut JITModule,
    builder: &mut FunctionBuilder,
    value: Value,
    ty: &MirTy,
    ctx: &RvalueEmitCtx<'_>,
) -> Option<Value> {
    let _ = module;
    match ty {
        MirTy::Integer => call_runtime(builder, ctx.rt.box_int_scoped_ref, &[value]),
        MirTy::Float => call_runtime(builder, ctx.rt.box_float_scoped_ref, &[value]),
        MirTy::Boolean => call_runtime(builder, ctx.rt.box_bool_scoped_ref, &[value]),
        MirTy::Handle => call_runtime(builder, ctx.rt.box_handle_scoped_ref, &[value]),
        MirTy::Nothing => call_runtime(builder, ctx.rt.box_nothing_scoped_ref, &[]),
        _ => Some(value),
    }
}

fn box_const_for_abi(
    module: &mut JITModule,
    builder: &mut FunctionBuilder,
    lit: &MirLit,
    ctx: &RvalueEmitCtx<'_>,
) -> Option<Value> {
    match lit {
        MirLit::Int(n) => {
            let value = builder.ins().iconst(I64, *n);
            call_runtime(builder, ctx.rt.box_int_scoped_ref, &[value])
        }
        MirLit::Float(f) => {
            let value = builder.ins().f64const(*f);
            call_runtime(builder, ctx.rt.box_float_scoped_ref, &[value])
        }
        MirLit::Bool(b) => {
            let value = builder.ins().iconst(I8, i64::from(*b));
            call_runtime(builder, ctx.rt.box_bool_scoped_ref, &[value])
        }
        MirLit::Nothing => call_runtime(builder, ctx.rt.box_nothing_scoped_ref, &[]),
        MirLit::Str(text) => {
            let (ptr, len) = str_const(module, builder, text)?;
            call_runtime(builder, ctx.rt.box_str_scoped_ref, &[ptr, len])
        }
        _ => None,
    }
}

fn emit_load_global_raw(
    builder: &mut FunctionBuilder,
    load_global_raw_ref: cranelift_codegen::ir::FuncRef,
    global: GlobalId,
) -> Value {
    let raw = builder.ins().iconst(I64, global.0 as i64);
    let call = builder.ins().call(load_global_raw_ref, &[raw]);
    builder
        .inst_results(call)
        .first()
        .copied()
        .expect("fdn_jit_load_global_raw result")
}

fn emit_store_global_raw(
    builder: &mut FunctionBuilder,
    store_global_raw_ref: cranelift_codegen::ir::FuncRef,
    global: GlobalId,
    raw_value: Value,
) {
    let raw_global = builder.ins().iconst(I64, global.0 as i64);
    builder
        .ins()
        .call(store_global_raw_ref, &[raw_global, raw_value]);
}

fn emit_call_fn_raw(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    local_types: &HashMap<LocalId, MirTy>,
    call_fn_raw_ref: cranelift_codegen::ir::FuncRef,
    fn_id: FunctionId,
    args: &[Operand],
    dest_ty: &MirTy,
) -> Option<Value> {
    let arg_values = args
        .iter()
        .map(|arg| operand_to_abi_i64(builder, vars, local_types, arg))
        .collect::<Vec<_>>();
    let (args_ptr, args_cnt) = stack_i64_array(builder, &arg_values);
    let fn_raw = builder.ins().iconst(I64, fn_id.0 as i64);
    let call = builder
        .ins()
        .call(call_fn_raw_ref, &[fn_raw, args_ptr, args_cnt]);
    let raw = builder
        .inst_results(call)
        .first()
        .copied()
        .expect("fdn_jit_call_fn_raw result");
    Some(abi_i64_to_native(builder, raw, dest_ty))
}

fn emit_literal(builder: &mut FunctionBuilder, lit: &MirLit) -> Value {
    match lit {
        MirLit::Int(n) => builder.ins().iconst(I64, *n),
        MirLit::Float(f) => builder.ins().f64const(*f),
        MirLit::Bool(b) => builder.ins().iconst(I8, *b as i64),
        _ => builder.ins().iconst(I64, 0),
    }
}

fn declare_import_fn(
    module: &mut JITModule,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
    result: Option<cranelift_codegen::ir::Type>,
) -> FuncId {
    let mut sig = module.make_signature();
    for param in params {
        sig.params.push(AbiParam::new(*param));
    }
    if let Some(result) = result {
        sig.returns.push(AbiParam::new(result));
    }
    module
        .declare_function(name, Linkage::Import, &sig)
        .expect("declare jit runtime import")
}

fn call_runtime(
    builder: &mut FunctionBuilder,
    func_ref: cranelift_codegen::ir::FuncRef,
    args: &[Value],
) -> Option<Value> {
    let inst = builder.ins().call(func_ref, args);
    builder.inst_results(inst).first().copied()
}

fn str_const(
    module: &mut JITModule,
    builder: &mut FunctionBuilder<'_>,
    s: &str,
) -> Option<(Value, Value)> {
    let mut desc = DataDescription::new();
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    desc.define(bytes.into_boxed_slice());
    let data_id = module.declare_anonymous_data(false, false).ok()?;
    module.define_data(data_id, &desc).ok()?;
    let gref = module.declare_data_in_func(data_id, builder.func);
    let ptr = builder.ins().global_value(I64, gref);
    let len = builder.ins().iconst(I64, s.len() as i64);
    Some((ptr, len))
}

fn emit_binop(
    builder: &mut FunctionBuilder,
    op: fidan_ast::BinOp,
    lv: Value,
    rv: Value,
    dest_ty: &MirTy,
) -> Option<Value> {
    use fidan_ast::BinOp;

    let lv_ty = builder.func.dfg.value_type(lv);
    let rv_ty = builder.func.dfg.value_type(rv);

    // For arithmetic, coerce both operands to the destination type.
    let (lv2, rv2) = if dest_ty == &MirTy::Float {
        (
            ensure_f64(builder, lv, lv_ty),
            ensure_f64(builder, rv, rv_ty),
        )
    } else if dest_ty == &MirTy::Integer {
        (
            ensure_i64(builder, lv, lv_ty),
            ensure_i64(builder, rv, rv_ty),
        )
    } else {
        (lv, rv)
    };

    let val = match op {
        BinOp::Add if dest_ty == &MirTy::Integer => builder.ins().iadd(lv2, rv2),
        BinOp::Sub if dest_ty == &MirTy::Integer => builder.ins().isub(lv2, rv2),
        BinOp::Mul if dest_ty == &MirTy::Integer => builder.ins().imul(lv2, rv2),
        BinOp::Div if dest_ty == &MirTy::Integer => builder.ins().sdiv(lv2, rv2),
        BinOp::Rem if dest_ty == &MirTy::Integer => builder.ins().srem(lv2, rv2),

        BinOp::Add if dest_ty == &MirTy::Float => builder.ins().fadd(lv2, rv2),
        BinOp::Sub if dest_ty == &MirTy::Float => builder.ins().fsub(lv2, rv2),
        BinOp::Mul if dest_ty == &MirTy::Float => builder.ins().fmul(lv2, rv2),
        BinOp::Div if dest_ty == &MirTy::Float => builder.ins().fdiv(lv2, rv2),

        // Comparisons — coerce to matching types
        BinOp::Eq => {
            let (a, b) = coerce_same(builder, lv, rv, lv_ty, rv_ty);
            if builder.func.dfg.value_type(a) == F64 {
                builder.ins().fcmp(FloatCC::Equal, a, b)
            } else {
                builder.ins().icmp(IntCC::Equal, a, b)
            }
        }
        BinOp::NotEq => {
            let (a, b) = coerce_same(builder, lv, rv, lv_ty, rv_ty);
            if builder.func.dfg.value_type(a) == F64 {
                builder.ins().fcmp(FloatCC::NotEqual, a, b)
            } else {
                builder.ins().icmp(IntCC::NotEqual, a, b)
            }
        }
        BinOp::Lt => {
            let (a, b) = coerce_same(builder, lv, rv, lv_ty, rv_ty);
            if builder.func.dfg.value_type(a) == F64 {
                builder.ins().fcmp(FloatCC::LessThan, a, b)
            } else {
                builder.ins().icmp(IntCC::SignedLessThan, a, b)
            }
        }
        BinOp::LtEq => {
            let (a, b) = coerce_same(builder, lv, rv, lv_ty, rv_ty);
            if builder.func.dfg.value_type(a) == F64 {
                builder.ins().fcmp(FloatCC::LessThanOrEqual, a, b)
            } else {
                builder.ins().icmp(IntCC::SignedLessThanOrEqual, a, b)
            }
        }
        BinOp::Gt => {
            let (a, b) = coerce_same(builder, lv, rv, lv_ty, rv_ty);
            if builder.func.dfg.value_type(a) == F64 {
                builder.ins().fcmp(FloatCC::GreaterThan, a, b)
            } else {
                builder.ins().icmp(IntCC::SignedGreaterThan, a, b)
            }
        }
        BinOp::GtEq => {
            let (a, b) = coerce_same(builder, lv, rv, lv_ty, rv_ty);
            if builder.func.dfg.value_type(a) == F64 {
                builder.ins().fcmp(FloatCC::GreaterThanOrEqual, a, b)
            } else {
                builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, a, b)
            }
        }

        BinOp::And => {
            let li = ensure_i64(builder, lv, lv_ty);
            let ri = ensure_i64(builder, rv, rv_ty);
            let r = builder.ins().band(li, ri);
            builder.ins().ireduce(I8, r)
        }
        BinOp::Or => {
            let li = ensure_i64(builder, lv, lv_ty);
            let ri = ensure_i64(builder, rv, rv_ty);
            let r = builder.ins().bor(li, ri);
            builder.ins().ireduce(I8, r)
        }

        _ => return None,
    };

    Some(val)
}

fn emit_unop(
    builder: &mut FunctionBuilder,
    op: fidan_ast::UnOp,
    v: Value,
    _dest_ty: &MirTy,
) -> Option<Value> {
    use fidan_ast::UnOp;
    let vty = builder.func.dfg.value_type(v);
    match op {
        UnOp::Neg => {
            if vty == F64 {
                Some(builder.ins().fneg(v))
            } else {
                Some(builder.ins().ineg(v))
            }
        }
        UnOp::Not => {
            let vx = ensure_i64(builder, v, vty);
            let zero = builder.ins().iconst(I64, 0);
            Some(builder.ins().icmp(IntCC::Equal, vx, zero))
        }
        _ => None,
    }
}

fn emit_stdlib_method_call(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    _local_types: &HashMap<LocalId, MirTy>,
    ns: &str,
    method: &str,
    args: &[Operand],
) -> Option<Value> {
    let arg = load_operand(builder, vars, args.first()?);
    let arg_ty = builder.func.dfg.value_type(arg);
    let arg_kinds = [value_type_to_stdlib_kind(arg_ty)];
    let info = infer_stdlib_method(ns, method, &arg_kinds)?;
    match info.intrinsic {
        Some(StdlibIntrinsic::Math(MathIntrinsic::Sqrt)) => {
            let fval = ensure_f64(builder, arg, arg_ty);
            Some(builder.ins().sqrt(fval))
        }
        Some(StdlibIntrinsic::Math(MathIntrinsic::Abs)) => {
            if arg_ty == F64 {
                Some(builder.ins().fabs(arg))
            } else {
                Some(builder.ins().iabs(arg))
            }
        }
        Some(StdlibIntrinsic::Math(MathIntrinsic::Floor)) => {
            let fval = ensure_f64(builder, arg, arg_ty);
            let floored = builder.ins().floor(fval);
            Some(builder.ins().fcvt_to_sint(I64, floored))
        }
        Some(StdlibIntrinsic::Math(MathIntrinsic::Ceil)) => {
            let fval = ensure_f64(builder, arg, arg_ty);
            let ceiled = builder.ins().ceil(fval);
            Some(builder.ins().fcvt_to_sint(I64, ceiled))
        }
        Some(StdlibIntrinsic::Math(MathIntrinsic::Trunc)) => {
            let fval = ensure_f64(builder, arg, arg_ty);
            Some(builder.ins().trunc(fval))
        }
        None => None,
    }
}

fn stdlib_namespace(
    receiver: &Operand,
    namespace_locals: &HashMap<LocalId, String>,
) -> Option<String> {
    let namespace = match receiver {
        Operand::Local(local) => namespace_locals.get(local).cloned(),
        Operand::Const(MirLit::Namespace(namespace)) => Some(namespace.clone()),
        _ => None,
    }?;
    is_stdlib_module(namespace.as_str()).then_some(namespace)
}

// ── Operand loading ────────────────────────────────────────────────────────────

fn load_operand(builder: &mut FunctionBuilder, vars: &[Variable], op: &Operand) -> Value {
    match op {
        Operand::Local(l) => builder.use_var(vars[l.0 as usize]),
        Operand::Const(lit) => match lit {
            MirLit::Int(n) => builder.ins().iconst(I64, *n),
            MirLit::Float(f) => builder.ins().f64const(*f),
            MirLit::Bool(b) => builder.ins().iconst(I8, *b as i64),
            _ => builder.ins().iconst(I64, 0),
        },
    }
}

fn operand_to_abi_i64(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    local_types: &HashMap<LocalId, MirTy>,
    op: &Operand,
) -> Value {
    let val = load_operand(builder, vars, op);
    let ty = match op {
        Operand::Local(local) => local_types.get(local).unwrap_or(&MirTy::Dynamic),
        Operand::Const(MirLit::Float(_)) => &MirTy::Float,
        Operand::Const(MirLit::Bool(_)) => &MirTy::Boolean,
        Operand::Const(MirLit::Int(_)) => &MirTy::Integer,
        _ => &MirTy::Dynamic,
    };
    native_to_abi_i64(builder, val, ty)
}

fn stack_i64_array(builder: &mut FunctionBuilder, values: &[Value]) -> (Value, Value) {
    if values.is_empty() {
        return (builder.ins().iconst(I64, 0), builder.ins().iconst(I64, 0));
    }

    let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        (values.len() * 8) as u32,
        3u8,
    ));
    for (index, value) in values.iter().enumerate() {
        builder.ins().stack_store(*value, slot, (index as i32) * 8);
    }
    let ptr = builder.ins().stack_addr(I64, slot, 0);
    let cnt = builder.ins().iconst(I64, values.len() as i64);
    (ptr, cnt)
}

// ── Phi argument collection ────────────────────────────────────────────────────

fn collect_phi_args(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    local_types: &HashMap<LocalId, MirTy>,
    func: &MirFunction,
    src_block: BlockId,
    dst_block: BlockId,
) -> Vec<BlockArg> {
    let dst_bb = func.block(dst_block);
    dst_bb
        .phis
        .iter()
        .filter_map(|phi| {
            phi.operands
                .iter()
                .find(|(pred, _)| *pred == src_block)
                .map(|(_, op)| {
                    let val = load_operand(builder, vars, op);
                    let val_ty = builder.func.dfg.value_type(val);
                    // Coerce to the declared phi type so the jump type-checks.
                    let expected_ty = local_types
                        .get(&phi.result)
                        .and_then(mir_ty_to_cl)
                        .unwrap_or(I64);
                    let coerced = if val_ty == expected_ty {
                        val
                    } else {
                        // Type mismatch — coerce safely.
                        match (val_ty, expected_ty) {
                            (t, F64) if t != F64 => ensure_f64(builder, val, t),
                            (F64, t) if t != F64 => builder.ins().bitcast(t, MemFlags::new(), val),
                            (I8, I64) => builder.ins().uextend(I64, val),
                            (I64, I8) => builder.ins().ireduce(I8, val),
                            _ => val,
                        }
                    };
                    coerced.into()
                })
        })
        .collect()
}

// ── Type coercion helpers ──────────────────────────────────────────────────────

fn ensure_f64(
    builder: &mut FunctionBuilder,
    val: Value,
    vty: cranelift_codegen::ir::Type,
) -> Value {
    if vty == F64 {
        val
    } else if vty == I8 {
        let extended = builder.ins().uextend(I64, val);
        builder.ins().fcvt_from_uint(F64, extended)
    } else {
        builder.ins().fcvt_from_sint(F64, val)
    }
}

fn ensure_i64(
    builder: &mut FunctionBuilder,
    val: Value,
    vty: cranelift_codegen::ir::Type,
) -> Value {
    if vty == I64 {
        val
    } else if vty == I8 {
        builder.ins().uextend(I64, val)
    } else if vty == F64 {
        builder.ins().fcvt_to_sint(I64, val)
    } else {
        val
    }
}

fn coerce_same(
    builder: &mut FunctionBuilder,
    a: Value,
    b: Value,
    aty: cranelift_codegen::ir::Type,
    bty: cranelift_codegen::ir::Type,
) -> (Value, Value) {
    if aty == F64 || bty == F64 {
        (ensure_f64(builder, a, aty), ensure_f64(builder, b, bty))
    } else {
        (ensure_i64(builder, a, aty), ensure_i64(builder, b, bty))
    }
}

// ── ABI conversions ────────────────────────────────────────────────────────────

fn abi_i64_to_native(builder: &mut FunctionBuilder, raw: Value, ty: &MirTy) -> Value {
    match ty {
        MirTy::Float => builder.ins().bitcast(F64, MemFlags::new(), raw),
        MirTy::Boolean => builder.ins().ireduce(I8, raw),
        _ => raw,
    }
}

fn native_to_abi_i64(builder: &mut FunctionBuilder, val: Value, ty: &MirTy) -> Value {
    let vty = builder.func.dfg.value_type(val);
    match ty {
        MirTy::Float => builder.ins().bitcast(I64, MemFlags::new(), val),
        MirTy::Boolean => builder.ins().uextend(I64, val),
        _ => ensure_i64(builder, val, vty),
    }
}

// ── Trampoline ─────────────────────────────────────────────────────────────────

/// Call a JIT-compiled function from the interpreter.
pub fn call_jit_fn(
    entry: &JitFnEntry,
    args: &[fidan_runtime::FidanValue],
) -> fidan_runtime::FidanValue {
    let fn_ptr = entry
        .fn_ptr
        .expect("call_jit_fn requires a native JIT entry");
    let raw_args = args
        .iter()
        .zip(entry.param_tys.iter())
        .map(|(value, ty)| JitArgValue::from_fidan(value, ty))
        .collect::<Vec<_>>();
    let ffi_args = raw_args
        .iter()
        .map(JitArgValue::as_ffi_arg)
        .collect::<Vec<_>>();
    let cif = Cif::new(
        entry.param_tys.iter().map(jit_abi_ffi_type),
        jit_abi_ffi_type(&entry.return_ty),
    );
    let code_ptr = CodePtr(fn_ptr as *mut _);

    unsafe {
        let raw = cif.call::<i64>(code_ptr, &ffi_args);
        decode_jit_abi_value(raw, &entry.return_ty)
    }
}

enum JitArgValue {
    Integer(i64),
}

impl JitArgValue {
    fn from_fidan(value: &fidan_runtime::FidanValue, ty: &MirTy) -> Self {
        Self::Integer(encode_jit_abi_value(value, ty))
    }

    fn as_ffi_arg(&self) -> libffi::middle::Arg<'_> {
        match self {
            Self::Integer(value) => arg(value),
        }
    }
}

fn jit_abi_ffi_type(_ty: &MirTy) -> Type {
    Type::i64()
}

fn value_type_to_stdlib_kind(ty: cranelift_codegen::ir::Type) -> StdlibValueKind {
    if ty == F64 {
        StdlibValueKind::Float
    } else if ty == I8 {
        StdlibValueKind::Boolean
    } else if ty == I64 {
        StdlibValueKind::Integer
    } else {
        StdlibValueKind::Dynamic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::Lexer;
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    fn lower(src: &str) -> (MirProgram, Arc<SymbolInterner>) {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<test>", src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, _) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        let hir = fidan_hir::lower_module(&module, &typed, &interner);
        let mut mir = fidan_mir::lower_program(&hir, &interner, &[]);
        fidan_passes::run_all(&mut mir);
        (mir, interner)
    }

    #[test]
    fn unsupported_native_lowering_produces_fallback_entry() {
        let (mir, interner) = lower(
            r#"object Counter {
                var value oftype integer

                action bump returns integer {
                    this.value = this.value + 1
                    return this.value
                }
            }"#,
        );
        let func = mir
            .functions
            .iter()
            .find(|func| interner.resolve(func.name).as_ref() == "bump")
            .expect("missing bump method");
        let mut jit = JitCompiler::new();
        let entry = jit.compile_function(func, &mir, &interner);
        assert!(
            !entry.is_native(),
            "expected object-field method to use fallback JIT entry"
        );
    }

    #[test]
    fn primitive_global_function_compiles_natively() {
        let (mir, interner) = lower(
            r#"var base = 41

            action read returns integer {
                return base + 1
            }"#,
        );
        let func = mir
            .functions
            .iter()
            .find(|func| interner.resolve(func.name).as_ref() == "read")
            .expect("missing read action");
        let mut jit = JitCompiler::new();
        let entry = jit.compile_function(func, &mir, &interner);
        assert!(
            entry.is_native(),
            "expected primitive global reader to compile natively"
        );
    }

    #[test]
    fn primitive_direct_call_compiles_natively() {
        let (mir, interner) = lower(
            r#"action inc with (n oftype integer) returns integer {
                return n + 1
            }

            action read returns integer {
                return inc(41)
            }"#,
        );
        let func = mir
            .functions
            .iter()
            .find(|func| interner.resolve(func.name).as_ref() == "read")
            .expect("missing read action");
        let mut jit = JitCompiler::new();
        let entry = jit.compile_function(func, &mir, &interner);
        assert!(
            entry.is_native(),
            "expected primitive direct-call action to compile natively"
        );
    }

    #[test]
    fn tuple_identity_round_trip_compiles_and_executes_natively() {
        let (mir, interner) = lower(
            r#"action echo with (certain pair oftype (integer, string)) returns (integer, string) {
                return pair
            }"#,
        );
        let func = mir
            .functions
            .iter()
            .find(|func| interner.resolve(func.name).as_ref() == "echo")
            .expect("missing echo action");
        let mut jit = JitCompiler::new();
        let entry = jit.compile_function(func, &mir, &interner);
        assert!(
            entry.is_native(),
            "expected tuple identity action to compile natively"
        );

        let result = with_jit_runtime_context(std::ptr::null_mut(), || {
            call_jit_fn(
                &entry,
                &[FidanValue::Tuple(vec![
                    FidanValue::Integer(7),
                    FidanValue::String(fidan_runtime::FidanString::new("ok")),
                ])],
            )
        });

        match result {
            FidanValue::Tuple(items) => {
                assert!(matches!(items.first(), Some(FidanValue::Integer(7))));
                assert!(
                    matches!(items.get(1), Some(FidanValue::String(text)) if text.as_str() == "ok")
                );
            }
            other => panic!("expected tuple result, got {other:?}"),
        }
    }

    #[test]
    fn tuple_literal_with_string_element_executes_natively() {
        let (mir, interner) = lower(
            r#"action make_pair with (certain n oftype integer) returns (integer, string) {
                return (n, "ok")
            }"#,
        );
        let func = mir
            .functions
            .iter()
            .find(|func| interner.resolve(func.name).as_ref() == "make_pair")
            .expect("missing make_pair action");
        let mut jit = JitCompiler::new();
        let entry = jit.compile_function(func, &mir, &interner);
        assert!(
            entry.is_native(),
            "expected tuple literal action to compile natively"
        );

        let result = with_jit_runtime_context(std::ptr::null_mut(), || {
            call_jit_fn(&entry, &[FidanValue::Integer(42)])
        });

        match result {
            FidanValue::Tuple(items) => {
                assert!(matches!(items.first(), Some(FidanValue::Integer(42))));
                assert!(
                    matches!(items.get(1), Some(FidanValue::String(text)) if text.as_str() == "ok")
                );
            }
            other => panic!("expected tuple result, got {other:?}"),
        }
    }
}
