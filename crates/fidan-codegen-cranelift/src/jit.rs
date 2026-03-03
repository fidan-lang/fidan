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
    AbiParam, Block, Function, InstBuilder, MemFlags, TrapCode, UserFuncName, Value,
    condcodes::{FloatCC, IntCC},
    types::{F64, I8, I64},
};
use cranelift_codegen::{Context, settings, settings::Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module as CraneliftModule};
use fidan_lexer::SymbolInterner;
use fidan_mir::{
    BlockId, Callee, GlobalId, Instr, LocalId, MirFunction, MirLit, MirProgram, MirTy, Operand,
    Rvalue, Terminator,
};
use std::collections::HashMap;

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

fn build_local_type_map(func: &MirFunction) -> HashMap<LocalId, MirTy> {
    let mut map: HashMap<LocalId, MirTy> = HashMap::new();

    // Params
    for p in &func.params {
        map.insert(p.local, p.ty.clone());
    }

    for bb in &func.blocks {
        // Instructions only on the first pass — phi types are resolved below.
        for phi in &bb.phis {
            // Seed phi results with their declared type (may be Dynamic/non-primitive).
            map.insert(phi.result, phi.ty.clone());
        }
        // Instructions
        for instr in &bb.instructions {
            match instr {
                Instr::Assign { dest, ty, .. } => {
                    map.insert(*dest, ty.clone());
                }
                Instr::LoadGlobal { dest, .. } => {
                    // Treat namespace slots as Integer (placeholder value)
                    map.insert(*dest, MirTy::Integer);
                }
                Instr::GetField { dest, .. } | Instr::GetIndex { dest, .. } => {
                    map.insert(*dest, MirTy::Dynamic);
                }
                // Stdlib method calls (the only Call variant the JIT handles)
                // always return a float value (math.sqrt / abs / floor / ceil / trunc).
                Instr::Call { dest: Some(d), .. } => {
                    map.insert(*d, MirTy::Float);
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
                        Operand::Local(l) => map.get(l).filter(|t| is_jit_primitive(*t)).cloned(),
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

// ── JitCompiler ────────────────────────────────────────────────────────────────

/// Metadata about a compiled JIT function needed to call it from the Rust trampoline.
#[derive(Clone)]
pub struct JitFnEntry {
    /// Raw function pointer (unsafe to call — use `call_jit_fn` below).
    pub fn_ptr: *const u8,
    /// Logical parameter types (in same order as `MirFunction::params`).
    pub param_tys: Vec<MirTy>,
    /// Logical return type.
    pub return_ty: MirTy,
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
    /// Counter used to generate unique function names.
    fn_counter: u32,
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
        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);
        let ctx = module.make_context();
        let builder_ctx = FunctionBuilderContext::new();
        Self {
            module,
            ctx,
            builder_ctx,
            fn_counter: 0,
        }
    }

    /// Attempt to JIT-compile `func`.
    ///
    /// Returns `Some(entry)` if compilation succeeded, `None` if the function
    /// contains constructs the JIT cannot handle (non-primitive types, dynamic
    /// calls, exception handling, etc.).
    pub fn compile_function(
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
        let local_types = build_local_type_map(func);

        // Map GlobalId → stdlib module name, identified via use_decls
        let mut global_ns_map: HashMap<GlobalId, String> = HashMap::new();
        for (i, g) in program.globals.iter().enumerate() {
            let g_name = interner.resolve(g.name);
            for decl in &program.use_decls {
                if decl.is_stdlib && decl.specific_names.is_none() {
                    if g_name.as_ref() == decl.alias.as_str() {
                        global_ns_map.insert(GlobalId(i as u32), decl.module.clone());
                    }
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
                            .and_then(|t| mir_ty_to_cl(t))
                            .unwrap_or(I64);
                        let pval = builder.append_block_param(cl_blocks[bi], phi_cl_ty);
                        phi_param_vals.insert((bi, pi), pval);
                    }
                }
            }

            // Per-function namespace tracking: LocalId → stdlib module name
            let mut namespace_locals: HashMap<LocalId, String> = HashMap::new();

            // Declare Cranelift Variables for every MIR local.
            let num_locals = func.local_count as usize;
            let mut cl_vars: Vec<Variable> = Vec::with_capacity(num_locals);
            for i in 0..num_locals {
                let var = Variable::from_u32(i as u32);
                let cl_ty = local_types
                    .get(&LocalId(i as u32))
                    .and_then(|t| mir_ty_to_cl(t))
                    .unwrap_or(I64);
                builder.declare_var(var, cl_ty);
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
                            let val = emit_rvalue(
                                &mut builder,
                                &cl_vars,
                                &local_types,
                                rhs,
                                &namespace_locals,
                                interner,
                                ty,
                            )?;
                            builder.def_var(cl_vars[dest.0 as usize], val);
                        }

                        Instr::LoadGlobal { dest, global } => {
                            if let Some(ns) = global_ns_map.get(global) {
                                namespace_locals.insert(*dest, ns.clone());
                            }
                            let dummy = builder.ins().iconst(I64, 0);
                            builder.def_var(cl_vars[dest.0 as usize], dummy);
                        }

                        Instr::Call {
                            dest, callee, args, ..
                        } => {
                            if let Callee::Method {
                                receiver: Operand::Local(recv),
                                method,
                            } = callee
                            {
                                let ns = namespace_locals.get(recv)?;
                                let mname = interner.resolve(*method);
                                let val = emit_stdlib_method_call(
                                    &mut builder,
                                    &cl_vars,
                                    &local_types,
                                    ns.as_ref(),
                                    mname.as_ref(),
                                    args,
                                )?;
                                if let Some(d) = dest {
                                    builder.def_var(cl_vars[d.0 as usize], val);
                                }
                            } else {
                                return None;
                            }
                        }

                        // Abort JIT on any unsupported instruction
                        Instr::PushCatch(_)
                        | Instr::PopCatch
                        | Instr::SpawnConcurrent { .. }
                        | Instr::SpawnParallel { .. }
                        | Instr::SpawnExpr { .. }
                        | Instr::SpawnDynamic { .. }
                        | Instr::AwaitPending { .. }
                        | Instr::JoinAll { .. }
                        | Instr::ParallelIter { .. }
                        | Instr::CertainCheck { .. }
                        | Instr::SetField { .. }
                        | Instr::GetField { .. }
                        | Instr::SetIndex { .. }
                        | Instr::GetIndex { .. }
                        | Instr::StoreGlobal { .. } => return None,

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
            fn_ptr: fn_ptr as *const u8,
            param_tys: func.params.iter().map(|p| p.ty.clone()).collect(),
            return_ty: func.return_ty.clone(),
        })
    }
}

// ── Rvalue emission ────────────────────────────────────────────────────────────

fn emit_rvalue(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    local_types: &HashMap<LocalId, MirTy>,
    rhs: &Rvalue,
    ns_locals: &HashMap<LocalId, String>,
    interner: &SymbolInterner,
    dest_ty: &MirTy,
) -> Option<Value> {
    match rhs {
        Rvalue::Literal(lit) => Some(emit_literal(builder, lit)),

        Rvalue::Use(op) => Some(load_operand(builder, vars, op)),

        Rvalue::Binary { op, lhs, rhs } => {
            let lv = load_operand(builder, vars, lhs);
            let rv = load_operand(builder, vars, rhs);
            emit_binop(builder, *op, lv, rv, dest_ty)
        }

        Rvalue::Unary { op, operand } => {
            let v = load_operand(builder, vars, operand);
            emit_unop(builder, *op, v, dest_ty)
        }

        Rvalue::Call {
            callee:
                Callee::Method {
                    receiver: Operand::Local(recv),
                    method,
                },
            args,
        } => {
            let ns = ns_locals.get(recv)?;
            let mname = interner.resolve(*method);
            emit_stdlib_method_call(
                builder,
                vars,
                local_types,
                ns.as_ref(),
                mname.as_ref(),
                args,
            )
        }

        _ => None,
    }
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
    match (ns, method) {
        ("math", "sqrt") => {
            let arg = load_operand(builder, vars, args.first()?);
            let arg_ty = builder.func.dfg.value_type(arg);
            let fval = ensure_f64(builder, arg, arg_ty);
            Some(builder.ins().sqrt(fval))
        }
        ("math", "abs") => {
            let arg = load_operand(builder, vars, args.first()?);
            let arg_ty = builder.func.dfg.value_type(arg);
            if arg_ty == F64 {
                Some(builder.ins().fabs(arg))
            } else {
                Some(builder.ins().iabs(arg))
            }
        }
        ("math", "floor") => {
            let arg = load_operand(builder, vars, args.first()?);
            let arg_ty = builder.func.dfg.value_type(arg);
            let fval = ensure_f64(builder, arg, arg_ty);
            Some(builder.ins().floor(fval))
        }
        ("math", "ceil") => {
            let arg = load_operand(builder, vars, args.first()?);
            let arg_ty = builder.func.dfg.value_type(arg);
            let fval = ensure_f64(builder, arg, arg_ty);
            Some(builder.ins().ceil(fval))
        }
        ("math", "trunc") => {
            let arg = load_operand(builder, vars, args.first()?);
            let arg_ty = builder.func.dfg.value_type(arg);
            let fval = ensure_f64(builder, arg, arg_ty);
            Some(builder.ins().trunc(fval))
        }
        _ => None,
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

// ── Phi argument collection ────────────────────────────────────────────────────

fn collect_phi_args(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    local_types: &HashMap<LocalId, MirTy>,
    func: &MirFunction,
    src_block: BlockId,
    dst_block: BlockId,
) -> Vec<Value> {
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
                        .and_then(|t| mir_ty_to_cl(t))
                        .unwrap_or(I64);
                    if val_ty == expected_ty {
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
                    }
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
    let mut raw_args = [0i64; 16];
    let n = args.len().min(16);
    for (i, (v, ty)) in args.iter().zip(entry.param_tys.iter()).enumerate().take(n) {
        raw_args[i] = fidan_value_to_abi(v, ty);
    }
    let result_i64: i64 = unsafe { dispatch_native(entry.fn_ptr, n, &raw_args) };
    abi_to_fidan_value(result_i64, &entry.return_ty)
}

fn fidan_value_to_abi(v: &fidan_runtime::FidanValue, _ty: &MirTy) -> i64 {
    match v {
        fidan_runtime::FidanValue::Integer(n) => *n,
        fidan_runtime::FidanValue::Float(f) => f.to_bits() as i64,
        fidan_runtime::FidanValue::Boolean(b) => *b as i64,
        _ => 0,
    }
}

fn abi_to_fidan_value(raw: i64, ty: &MirTy) -> fidan_runtime::FidanValue {
    match ty {
        MirTy::Integer => fidan_runtime::FidanValue::Integer(raw),
        MirTy::Float => fidan_runtime::FidanValue::Float(f64::from_bits(raw as u64)),
        MirTy::Boolean => fidan_runtime::FidanValue::Boolean(raw != 0),
        _ => fidan_runtime::FidanValue::Nothing,
    }
}

/// Dispatch to a native JIT function with 0–8 I64 arguments.
///
/// # Safety
/// `fn_ptr` must point to valid JIT-compiled code that follows the ABI
/// convention (all I64 params, I64 return, C calling convention).
unsafe fn dispatch_native(fn_ptr: *const u8, n: usize, args: &[i64; 16]) -> i64 {
    type F0 = unsafe extern "C" fn() -> i64;
    type F1 = unsafe extern "C" fn(i64) -> i64;
    type F2 = unsafe extern "C" fn(i64, i64) -> i64;
    type F3 = unsafe extern "C" fn(i64, i64, i64) -> i64;
    type F4 = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;
    type F5 = unsafe extern "C" fn(i64, i64, i64, i64, i64) -> i64;
    type F6 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64;
    type F7 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64) -> i64;
    type F8 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64) -> i64;
    let p = fn_ptr as usize;
    // Rust 2024: explicit unsafe blocks required even inside unsafe fn.
    match n {
        0 => unsafe {
            let f: F0 = std::mem::transmute(p);
            f()
        },
        1 => unsafe {
            let f: F1 = std::mem::transmute(p);
            f(args[0])
        },
        2 => unsafe {
            let f: F2 = std::mem::transmute(p);
            f(args[0], args[1])
        },
        3 => unsafe {
            let f: F3 = std::mem::transmute(p);
            f(args[0], args[1], args[2])
        },
        4 => unsafe {
            let f: F4 = std::mem::transmute(p);
            f(args[0], args[1], args[2], args[3])
        },
        5 => unsafe {
            let f: F5 = std::mem::transmute(p);
            f(args[0], args[1], args[2], args[3], args[4])
        },
        6 => unsafe {
            let f: F6 = std::mem::transmute(p);
            f(args[0], args[1], args[2], args[3], args[4], args[5])
        },
        7 => unsafe {
            let f: F7 = std::mem::transmute(p);
            f(
                args[0], args[1], args[2], args[3], args[4], args[5], args[6],
            )
        },
        _ => unsafe {
            let f: F8 = std::mem::transmute(p);
            f(
                args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
            )
        },
    }
}
