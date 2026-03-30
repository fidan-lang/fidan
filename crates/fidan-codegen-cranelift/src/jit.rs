// fidan-codegen-cranelift/src/jit.rs
//
// Cranelift JIT backend for Fidan.
//
// Compiles `@precompile`-annotated `MirFunction`s (and hot interpreter functions
// above the JIT threshold) to native machine code via the Cranelift JIT.
//
// # ABI Convention
//
// All compiled functions use a unified "I64 everything" calling convention:
//   - Integer params  → passed as i64 (the value itself)
//   - Float params    → passed as i64 (f64 bit pattern)
//   - Boolean params  → passed as i64 (0 or 1)
//   - Return value    → same encoding as above
//
// This simplifies the Rust trampoline: it always deals with `i64` values and
// calls `fn(i64, i64, ...) -> i64` regardless of the logical parameter types.
//
// Inside the Cranelift IR we use native types (I64 / F64) and emit bitcasts at
// the function entry / exit to convert the ABI "all I64" representation.

use cranelift_codegen::ir::{
    AbiParam, Block, BlockArg, Function, InstBuilder, MemFlags, TrapCode, UserFuncName, Value,
    condcodes::{FloatCC, IntCC},
    types::{F64, I8, I64},
};
use cranelift_codegen::{Context, settings, settings::Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module as CraneliftModule};
use fidan_lexer::SymbolInterner;
use fidan_mir::{
    BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirLit, MirProgram, MirTy,
    Operand, Rvalue, Terminator,
};
use fidan_stdlib::{
    MathIntrinsic, StdlibIntrinsic, StdlibValueKind, infer_receiver_method, infer_stdlib_method,
};
use libffi::middle::{Cif, CodePtr, Type, arg};
use std::cell::Cell;
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
}

pub fn register_jit_runtime_hooks(hooks: JitRuntimeHooks) {
    let _ = JIT_RUNTIME_HOOKS.set(hooks);
}

pub fn with_jit_runtime_context<T>(ctx: *mut c_void, f: impl FnOnce() -> T) -> T {
    ACTIVE_JIT_CONTEXT.with(|cell| {
        let previous = cell.replace(ctx);
        let result = f();
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
    match ty {
        MirTy::Integer => Some(I64),
        MirTy::Float => Some(F64),
        MirTy::Boolean => Some(I8),
        _ => None,
    }
}

/// Returns `true` for the primitive `MirTy` variants that the JIT can handle.
fn is_jit_primitive(ty: &MirTy) -> bool {
    matches!(ty, MirTy::Integer | MirTy::Float | MirTy::Boolean)
}

/// Cranelift type to use for the I64-ABI boundary.
const ABI_TY: cranelift_codegen::ir::Type = I64;

// ── Build a local-type map ─────────────────────────────────────────────────────

fn build_local_type_map(
    func: &MirFunction,
    program: &MirProgram,
    interner: &SymbolInterner,
) -> HashMap<LocalId, MirTy> {
    let mut map: HashMap<LocalId, MirTy> = HashMap::new();
    let mut global_ns_map: HashMap<GlobalId, String> = HashMap::new();
    let mut namespace_locals: HashMap<LocalId, String> = HashMap::new();

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

    // Params
    for p in &func.params {
        map.insert(p.local, p.ty.clone());
    }

    for bb in &func.blocks {
        namespace_locals.clear();
        // Instructions only on the first pass — phi types are resolved below.
        for phi in &bb.phis {
            // Seed phi results with their declared type (may be Dynamic/non-primitive).
            map.insert(phi.result, phi.ty.clone());
        }
        // Instructions
        for instr in &bb.instructions {
            match instr {
                Instr::Assign { dest, ty, rhs } => {
                    let effective_ty = match rhs {
                        Rvalue::Call { callee, args }
                            if matches!(ty, MirTy::Dynamic | MirTy::Error) =>
                        {
                            infer_call_result_ty(callee, args, &map, &namespace_locals, interner)
                                .unwrap_or_else(|| ty.clone())
                        }
                        _ => ty.clone(),
                    };
                    map.insert(*dest, effective_ty);
                    namespace_locals.remove(dest);
                    match rhs {
                        Rvalue::Literal(MirLit::Namespace(ns)) => {
                            namespace_locals.insert(*dest, ns.clone());
                        }
                        Rvalue::Use(Operand::Local(src)) => {
                            if let Some(ns) = namespace_locals.get(src).cloned() {
                                namespace_locals.insert(*dest, ns);
                            }
                        }
                        _ => {}
                    }
                }
                Instr::LoadGlobal { dest, global } => {
                    if let Some(ns) = global_ns_map.get(global) {
                        // Namespace sentinels are only used for stdlib method dispatch.
                        map.insert(*dest, MirTy::Integer);
                        namespace_locals.insert(*dest, ns.clone());
                    } else {
                        let global_ty = program
                            .globals
                            .get(global.0 as usize)
                            .map(|g| g.ty.clone())
                            .unwrap_or(MirTy::Dynamic);
                        map.insert(*dest, global_ty);
                        namespace_locals.remove(dest);
                    }
                }
                Instr::GetField { dest, .. } | Instr::GetIndex { dest, .. } => {
                    map.insert(*dest, MirTy::Dynamic);
                    namespace_locals.remove(dest);
                }
                Instr::Call {
                    dest: Some(d),
                    result_ty,
                    callee,
                    args,
                    ..
                } => {
                    let inferred_ty = result_ty
                        .clone()
                        .filter(|ty| !matches!(ty, MirTy::Dynamic | MirTy::Error))
                        .or_else(|| {
                            infer_call_result_ty(callee, args, &map, &namespace_locals, interner)
                        })
                        .unwrap_or(MirTy::Dynamic);
                    map.insert(*d, inferred_ty);
                    namespace_locals.remove(d);
                }
                _ => {}
            }
        }
    }
    // Worklist phi-type inference: repeat until no phi type changes.
    // This handles loop-carried variables where a phi's operand is itself
    // another phi whose type is only known from a later block (e.g., a
    // float accumulator seeded with an integer constant on the back-edge).
    loop {
        let mut changed = false;
        for bb in &func.blocks {
            for phi in &bb.phis {
                if is_jit_primitive(map.get(&phi.result).unwrap_or(&MirTy::Dynamic)) {
                    continue; // already resolved — skip
                }
                let inferred = if is_jit_primitive(&phi.ty) {
                    Some(phi.ty.clone())
                } else {
                    phi.operands.iter().find_map(|(_, op)| match op {
                        Operand::Local(l) => map.get(l).filter(|t| is_jit_primitive(t)).cloned(),
                        Operand::Const(MirLit::Float(_)) => Some(MirTy::Float),
                        Operand::Const(MirLit::Int(_)) => Some(MirTy::Integer),
                        Operand::Const(MirLit::Bool(_)) => Some(MirTy::Boolean),
                        _ => None,
                    })
                };
                if let Some(ty) = inferred {
                    map.insert(phi.result, ty);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    map
}

fn infer_stdlib_method_return_ty(
    ns: &str,
    method: &str,
    arg_kinds: &[StdlibValueKind],
) -> Option<MirTy> {
    infer_stdlib_method(ns, method, arg_kinds).map(|info| stdlib_kind_to_mir_ty(info.return_kind))
}

fn infer_call_result_ty(
    callee: &Callee,
    args: &[Operand],
    map: &HashMap<LocalId, MirTy>,
    namespace_locals: &HashMap<LocalId, String>,
    interner: &SymbolInterner,
) -> Option<MirTy> {
    match callee {
        Callee::Method {
            receiver: Operand::Local(recv),
            method,
        } => {
            let method_name = interner.resolve(*method);
            let arg_kinds = args
                .iter()
                .map(|arg| operand_stdlib_kind(arg, map))
                .collect::<Vec<_>>();
            namespace_locals
                .get(recv)
                .and_then(|ns| {
                    infer_stdlib_method_return_ty(ns.as_str(), method_name.as_ref(), &arg_kinds)
                })
                .or_else(|| {
                    map.get(recv).and_then(|receiver_ty| {
                        infer_receiver_method_return_ty(
                            receiver_ty,
                            method_name.as_ref(),
                            &arg_kinds,
                        )
                    })
                })
        }
        _ => None,
    }
}

fn infer_receiver_method_return_ty(
    receiver_ty: &MirTy,
    method: &str,
    arg_kinds: &[StdlibValueKind],
) -> Option<MirTy> {
    infer_receiver_method(
        mir_ty_to_stdlib_kind(receiver_ty.clone()),
        method,
        arg_kinds,
    )
    .map(|info| stdlib_kind_to_mir_ty(info.return_kind))
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
        let ctx = module.make_context();
        let builder_ctx = FunctionBuilderContext::new();
        Self {
            module,
            ctx,
            builder_ctx,
            load_global_raw_id,
            store_global_raw_id,
            call_fn_raw_id,
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
            if !is_jit_primitive(&p.ty) {
                return None;
            }
        }
        if !is_jit_primitive(&func.return_ty) {
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
                            };
                            let val = emit_rvalue(&mut builder, rhs, effective_ty, &emit_ctx)?;
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
                                Callee::Method {
                                    receiver: Operand::Local(recv),
                                    method,
                                } => {
                                    let ns = namespace_locals.get(recv)?;
                                    let mname = interner.resolve(*method);
                                    emit_stdlib_method_call(
                                        &mut builder,
                                        &cl_vars,
                                        &local_types,
                                        ns.as_ref(),
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
}

fn emit_rvalue(
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
        match entry.return_ty {
            MirTy::Integer => fidan_runtime::FidanValue::Integer(cif.call(code_ptr, &ffi_args)),
            MirTy::Float => fidan_runtime::FidanValue::Float(f64::from_bits(
                cif.call::<i64>(code_ptr, &ffi_args) as u64,
            )),
            MirTy::Boolean => {
                fidan_runtime::FidanValue::Boolean(cif.call::<i64>(code_ptr, &ffi_args) != 0)
            }
            _ => fidan_runtime::FidanValue::Nothing,
        }
    }
}

enum JitArgValue {
    Integer(i64),
}

impl JitArgValue {
    fn from_fidan(value: &fidan_runtime::FidanValue, ty: &MirTy) -> Self {
        match (value, ty) {
            (fidan_runtime::FidanValue::Integer(n), MirTy::Integer) => Self::Integer(*n),
            (fidan_runtime::FidanValue::Float(f), MirTy::Float) => {
                Self::Integer(f.to_bits() as i64)
            }
            (fidan_runtime::FidanValue::Boolean(b), MirTy::Boolean) => Self::Integer(i64::from(*b)),
            _ => Self::Integer(0),
        }
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

fn stdlib_kind_to_mir_ty(kind: StdlibValueKind) -> MirTy {
    match kind {
        StdlibValueKind::Integer => MirTy::Integer,
        StdlibValueKind::Float => MirTy::Float,
        StdlibValueKind::Boolean => MirTy::Boolean,
        StdlibValueKind::String => MirTy::String,
        StdlibValueKind::List => MirTy::List(Box::new(MirTy::Dynamic)),
        StdlibValueKind::Dict => MirTy::Dict(Box::new(MirTy::Dynamic), Box::new(MirTy::Dynamic)),
        StdlibValueKind::Nothing => MirTy::Nothing,
        StdlibValueKind::Dynamic => MirTy::Dynamic,
    }
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

fn operand_stdlib_kind(op: &Operand, map: &HashMap<LocalId, MirTy>) -> StdlibValueKind {
    match op {
        Operand::Local(local) => map
            .get(local)
            .cloned()
            .map(mir_ty_to_stdlib_kind)
            .unwrap_or(StdlibValueKind::Dynamic),
        Operand::Const(MirLit::Int(_)) => StdlibValueKind::Integer,
        Operand::Const(MirLit::Float(_)) => StdlibValueKind::Float,
        Operand::Const(MirLit::Bool(_)) => StdlibValueKind::Boolean,
        Operand::Const(MirLit::Str(_)) => StdlibValueKind::String,
        Operand::Const(MirLit::Nothing) => StdlibValueKind::Nothing,
        _ => StdlibValueKind::Dynamic,
    }
}

fn mir_ty_to_stdlib_kind(ty: MirTy) -> StdlibValueKind {
    match ty {
        MirTy::Integer => StdlibValueKind::Integer,
        MirTy::Float => StdlibValueKind::Float,
        MirTy::Boolean => StdlibValueKind::Boolean,
        MirTy::String => StdlibValueKind::String,
        MirTy::List(_) => StdlibValueKind::List,
        MirTy::Dict(_, _) => StdlibValueKind::Dict,
        MirTy::Nothing => StdlibValueKind::Nothing,
        _ => StdlibValueKind::Dynamic,
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
}
