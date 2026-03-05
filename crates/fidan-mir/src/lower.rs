// fidan-mir/src/lower.rs
//
// HIR → MIR lowering.
//
// Translates the HIR tree into SSA-form MIR using a scope-based renaming
// scheme.  Braun et al.'s "Simple and Efficient Construction of SSA Form"
// algorithm is approximated here: for linear code and if/else we get exact
// SSA.  For loops we use a two-pass approach (placeholder φ-nodes that are
// back-patched after the body is lowered).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use fidan_ast::BinOp;
use fidan_hir::{
    HirArg, HirCatchClause, HirCheckArm, HirCheckExprArm, HirElseIf, HirExpr, HirExprKind,
    HirFunction, HirInterpPart, HirModule, HirStmt,
};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_typeck::FidanType;

use crate::mir::{
    BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirGlobal, MirLit,
    MirObjectInfo, MirParam, MirProgram, MirStringPart, MirTy, MirUseDecl, Operand, PhiNode,
    Rvalue, Terminator,
};

// ── Parallel-for deferred body ───────────────────────────────────────────────

/// A `parallel for` body that needs to be lowered into a synthetic function
/// AFTER the enclosing function finishes (because we only have one `&mut MirProgram`).
///
/// SAFETY: `body_ptr`/`body_len` form a raw slice pointing into the `HirModule`
/// that is alive for the entire duration of `lower_program`.  All accesses happen
/// within that same call, single-threaded.
struct PendingParallelFor {
    fn_id: FunctionId,
    /// Per-iteration binding for `parallel for`; `None` for `concurrent { task {} }` bodies.
    binding: Option<(Symbol, MirTy)>,
    /// Outer-scope variables captured by the body: become extra params after the binding.
    env_params: Vec<(Symbol, MirTy)>,
    body_ptr: *const HirStmt,
    body_len: usize,
}

// SAFETY: used only in single-threaded lower_program; raw ptrs into &HirModule.
unsafe impl Send for PendingParallelFor {}

// ── HIR free-variable visitors ────────────────────────────────────────────────

/// Collect every `Var` symbol referenced in `stmts` (not necessarily free:
/// the caller filters to symbols live in the current scope).
fn collect_hir_used_vars(stmts: &[HirStmt]) -> HashSet<Symbol> {
    let mut out = HashSet::new();
    hir_walk_stmts(stmts, &mut out);
    out
}

fn hir_walk_stmts(stmts: &[HirStmt], out: &mut HashSet<Symbol>) {
    for s in stmts {
        hir_walk_stmt(s, out);
    }
}

fn hir_walk_stmt(s: &HirStmt, out: &mut HashSet<Symbol>) {
    match s {
        HirStmt::VarDecl { init, .. } => {
            if let Some(e) = init {
                hir_walk_expr(e, out);
            }
        }
        HirStmt::Destructure { value, .. } => hir_walk_expr(value, out),
        HirStmt::Assign { target, value, .. } => {
            hir_walk_expr(target, out);
            hir_walk_expr(value, out);
        }
        HirStmt::Expr(e) => hir_walk_expr(e, out),
        HirStmt::Return { value, .. } => {
            if let Some(e) = value {
                hir_walk_expr(e, out);
            }
        }
        HirStmt::Panic { value, .. } => hir_walk_expr(value, out),
        HirStmt::If {
            condition,
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            hir_walk_expr(condition, out);
            hir_walk_stmts(then_body, out);
            for ei in else_ifs {
                hir_walk_expr(&ei.condition, out);
                hir_walk_stmts(&ei.body, out);
            }
            if let Some(eb) = else_body {
                hir_walk_stmts(eb, out);
            }
        }
        HirStmt::Check {
            scrutinee, arms, ..
        } => {
            hir_walk_expr(scrutinee, out);
            for arm in arms {
                hir_walk_stmts(&arm.body, out);
            }
        }
        HirStmt::For { iterable, body, .. } | HirStmt::ParallelFor { iterable, body, .. } => {
            hir_walk_expr(iterable, out);
            hir_walk_stmts(body, out);
        }
        HirStmt::While {
            condition, body, ..
        } => {
            hir_walk_expr(condition, out);
            hir_walk_stmts(body, out);
        }
        HirStmt::Attempt {
            body,
            catches,
            otherwise,
            finally,
            ..
        } => {
            hir_walk_stmts(body, out);
            for c in catches {
                hir_walk_stmts(&c.body, out);
            }
            if let Some(ob) = otherwise {
                hir_walk_stmts(ob, out);
            }
            if let Some(fb) = finally {
                hir_walk_stmts(fb, out);
            }
        }
        HirStmt::ConcurrentBlock { tasks, .. } => {
            for t in tasks {
                hir_walk_stmts(&t.body, out);
            }
        }
        HirStmt::Break { .. } | HirStmt::Continue { .. } | HirStmt::Error { .. } => {}
    }
}

fn hir_walk_expr(e: &HirExpr, out: &mut HashSet<Symbol>) {
    match &e.kind {
        HirExprKind::Var(sym) => {
            out.insert(*sym);
        }
        HirExprKind::Binary { lhs, rhs, .. } => {
            hir_walk_expr(lhs, out);
            hir_walk_expr(rhs, out);
        }
        HirExprKind::Assign { target, value } => {
            hir_walk_expr(target, out);
            hir_walk_expr(value, out);
        }
        HirExprKind::Unary { operand, .. } => hir_walk_expr(operand, out),
        HirExprKind::NullCoalesce { lhs, rhs } => {
            hir_walk_expr(lhs, out);
            hir_walk_expr(rhs, out);
        }
        HirExprKind::IfExpr {
            condition,
            then_val,
            else_val,
        } => {
            hir_walk_expr(condition, out);
            hir_walk_expr(then_val, out);
            hir_walk_expr(else_val, out);
        }
        HirExprKind::Call { callee, args } => {
            hir_walk_expr(callee, out);
            for a in args {
                hir_walk_expr(&a.value, out);
            }
        }
        HirExprKind::Field { object, .. } => hir_walk_expr(object, out),
        HirExprKind::Index { object, index } => {
            hir_walk_expr(object, out);
            hir_walk_expr(index, out);
        }
        HirExprKind::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            hir_walk_expr(target, out);
            if let Some(e) = start {
                hir_walk_expr(e, out);
            }
            if let Some(e) = end {
                hir_walk_expr(e, out);
            }
            if let Some(e) = step {
                hir_walk_expr(e, out);
            }
        }
        HirExprKind::List(items) => {
            for e in items {
                hir_walk_expr(e, out);
            }
        }
        HirExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                hir_walk_expr(k, out);
                hir_walk_expr(v, out);
            }
        }
        HirExprKind::Tuple(items) => {
            for e in items {
                hir_walk_expr(e, out);
            }
        }
        HirExprKind::StringInterp(parts) => {
            for p in parts {
                if let HirInterpPart::Expr(e) = p {
                    hir_walk_expr(e, out);
                }
            }
        }
        HirExprKind::Spawn(e) | HirExprKind::Await(e) => hir_walk_expr(e, out),
        HirExprKind::CheckExpr { scrutinee, arms } => {
            hir_walk_expr(scrutinee, out);
            for arm in arms {
                hir_walk_expr(&arm.pattern, out);
                hir_walk_stmts(&arm.body, out);
            }
        }
        HirExprKind::IntLit(_)
        | HirExprKind::FloatLit(_)
        | HirExprKind::StrLit(_)
        | HirExprKind::BoolLit(_)
        | HirExprKind::Nothing
        | HirExprKind::This
        | HirExprKind::Parent
        | HirExprKind::Error => {}
        HirExprKind::ListComp {
            element,
            iterable,
            filter,
            ..
        } => {
            hir_walk_expr(element, out);
            hir_walk_expr(iterable, out);
            if let Some(f) = filter {
                hir_walk_expr(f, out);
            }
        }
        HirExprKind::DictComp {
            key,
            value,
            iterable,
            filter,
            ..
        } => {
            hir_walk_expr(key, out);
            hir_walk_expr(value, out);
            hir_walk_expr(iterable, out);
            if let Some(f) = filter {
                hir_walk_expr(f, out);
            }
        }
    }
}

// ── Type conversion ────────────────────────────────────────────────────────────

fn fidan_ty_to_mir(ty: &FidanType) -> MirTy {
    match ty {
        FidanType::Integer => MirTy::Integer,
        FidanType::Float => MirTy::Float,
        FidanType::Boolean => MirTy::Boolean,
        FidanType::String => MirTy::String,
        FidanType::Nothing => MirTy::Nothing,
        FidanType::Dynamic => MirTy::Dynamic,
        FidanType::List(e) => MirTy::List(Box::new(fidan_ty_to_mir(e))),
        FidanType::Dict(k, v) => {
            MirTy::Dict(Box::new(fidan_ty_to_mir(k)), Box::new(fidan_ty_to_mir(v)))
        }
        FidanType::Tuple(elems) => MirTy::Tuple(elems.iter().map(fidan_ty_to_mir).collect()),
        FidanType::Object(s) => MirTy::Object(*s),
        FidanType::Shared(t) => MirTy::Shared(Box::new(fidan_ty_to_mir(t))),
        FidanType::Pending(t) => MirTy::Pending(Box::new(fidan_ty_to_mir(t))),
        FidanType::Function => MirTy::Function,
        FidanType::Unknown | FidanType::Error => MirTy::Error,
    }
}

// ── Variable environment ──────────────────────────────────────────────────────

/// Current SSA definitions: variable name → most recent `LocalId`.
type VarEnv = HashMap<Symbol, LocalId>;

/// Clone + diff: returns (`new_env`, `changed`) where `changed` lists symbols
/// that differ between `before` and `after`.
fn env_diff(before: &VarEnv, after: &VarEnv) -> Vec<Symbol> {
    after
        .iter()
        .filter(|(sym, id)| before.get(sym).map_or(true, |old| old != *id))
        .map(|(sym, _)| *sym)
        .collect()
}

// ── Function builder ───────────────────────────────────────────────────────────

struct FnCtx<'p> {
    /// The MIR program we're building into.
    prog: &'p mut MirProgram,
    /// The function we're currently building.
    fn_id: FunctionId,
    /// The block we're currently appending instructions to.
    cur_bb: BlockId,
    /// Current variable→local mapping (the "SSA current-def" table).
    env: VarEnv,
    /// Maps module-level global names → `GlobalId` (for `LoadGlobal`/`StoreGlobal`).
    global_map: HashMap<Symbol, GlobalId>,
    /// Maps function name → `FunctionId` (populated by pre-pass; top-level fns only).
    fn_map: HashMap<Symbol, FunctionId>,
    /// Maps class name → `FunctionId` of its `new` constructor (for `ClassName(args)` calls).
    obj_map: HashMap<Symbol, FunctionId>,
    /// Whether the current block has been terminated (Return / Goto etc.).
    terminated: bool,
    /// The local that holds `this` inside a method body (None for free functions).
    this_reg: Option<LocalId>,
    /// The class this method belongs to (None for free functions).
    owner_class: Option<Symbol>,
    /// Maps class → its parent class (for `parent.method()` resolution).
    parent_map: HashMap<Symbol, Symbol>,
    /// Maps (class, method_name) → FunctionId (for direct parent method calls).
    method_ids: HashMap<(Symbol, Symbol), FunctionId>,
    /// Symbol for `"new"` — the constructor method name.
    new_sym: Symbol,
    /// Symbol for `"len"` — used in for-loop length queries.
    len_sym: Symbol,
    /// Symbol for `"append"` — used in list comprehensions.
    append_sym: Symbol,
    /// Symbol for `"type"` — used in typed catch-clause dispatch.
    type_sym: Symbol,
    /// Set of function names that are extension actions.
    /// Free calls to these need an implicit-`this` = nothing prepended.
    fn_is_extension: HashSet<Symbol>,
    /// Pending parallel-for bodies discovered while lowering this function.
    /// Drained by lower_program after all named functions are done.
    par_for_pending: Rc<RefCell<VecDeque<PendingParallelFor>>>,
    /// Stack of (continue_bb, exit_bb) for break/continue targeting.
    /// Innermost loop is at the back.
    loop_stack: Vec<(BlockId, BlockId)>,
    /// Records all `continue` sites: maps continue_target_bb to a list of
    /// (source_bb, env_snapshot_at_that_point).
    continue_sites: HashMap<BlockId, Vec<(BlockId, HashMap<Symbol, LocalId>)>>,
    /// Symbol for `"_"` — the wildcard pattern in `check` arms.
    wildcard_sym: Symbol,
    /// True only for the top-level init function (FunctionId(0)).  Used to
    /// decide whether a `VarDecl` should also write to the global table.
    is_init_fn: bool,
    /// Maps FunctionId → explicit-param names in declaration order.  Used at
    /// call sites to reorder named args before emitting `Instr::Call`.
    fn_param_names: HashMap<FunctionId, Vec<Symbol>>,
}

impl<'p> FnCtx<'p> {
    #[allow(dead_code)]
    fn func(&self) -> &MirFunction {
        self.prog.function(self.fn_id)
    }
    fn func_mut(&mut self) -> &mut MirFunction {
        self.prog.function_mut(self.fn_id)
    }

    fn alloc_local(&mut self) -> LocalId {
        self.func_mut().alloc_local()
    }
    fn alloc_block(&mut self) -> BlockId {
        self.func_mut().alloc_block()
    }

    // ── Instruction emission ─────────────────────────────────────────────────

    fn emit(&mut self, instr: Instr) {
        if self.terminated {
            return;
        }
        let bb = self.cur_bb;
        self.func_mut().block_mut(bb).instructions.push(instr);
    }

    fn set_terminator(&mut self, term: Terminator) {
        if self.terminated {
            return;
        }
        let bb = self.cur_bb;
        self.func_mut().block_mut(bb).terminator = term;
        self.terminated = true;
    }

    fn goto(&mut self, target: BlockId) {
        self.set_terminator(Terminator::Goto(target));
    }

    fn switch_to(&mut self, bb: BlockId) {
        self.cur_bb = bb;
        self.terminated = false;
    }

    // ── Variable definition / lookup ─────────────────────────────────────────

    fn define_var(&mut self, name: Symbol, local: LocalId) {
        self.env.insert(name, local);
    }

    fn lookup_var(&self, name: Symbol) -> Option<LocalId> {
        self.env.get(&name).copied()
    }

    // ── φ-node insertion ─────────────────────────────────────────────────────

    fn add_phi(
        &mut self,
        join_bb: BlockId,
        result: LocalId,
        ty: MirTy,
        operands: Vec<(BlockId, Operand)>,
    ) {
        self.func_mut().block_mut(join_bb).phis.push(PhiNode {
            result,
            ty,
            operands,
        });
    }

    // ── Argument sorting ─────────────────────────────────────────────────────

    /// Lower `args` for a call to `fid`, reordering any named args to match
    /// the callee's parameter declaration order.  Positional args fill slots
    /// that have no named counterpart, left-to-right.
    fn sort_args_for_fn(&mut self, fid: FunctionId, args: &[HirArg]) -> Vec<Operand> {
        // Fast path: no named args → emit in call-site order.
        if !args.iter().any(|a| a.name.is_some()) {
            return args.iter().map(|a| self.lower_expr(&a.value)).collect();
        }
        let param_names = self.fn_param_names.get(&fid).cloned().unwrap_or_default();
        // Named args keyed by their label symbol.
        let named: HashMap<Symbol, &HirExpr> = args
            .iter()
            .filter_map(|a| a.name.map(|n| (n, &a.value)))
            .collect();
        // Positional args (no label), in call-site order.
        let positional: Vec<&HirExpr> = args
            .iter()
            .filter(|a| a.name.is_none())
            .map(|a| &a.value)
            .collect();
        let mut result: Vec<Operand> = Vec::with_capacity(param_names.len().max(args.len()));
        let mut pos_idx = 0usize;
        for &psym in &param_names {
            if let Some(expr) = named.get(&psym) {
                result.push(self.lower_expr(expr));
            } else if pos_idx < positional.len() {
                result.push(self.lower_expr(positional[pos_idx]));
                pos_idx += 1;
            } else {
                result.push(Operand::Const(MirLit::Nothing));
            }
        }
        // Extra positional args beyond declared params (defensive).
        while pos_idx < positional.len() {
            result.push(self.lower_expr(positional[pos_idx]));
            pos_idx += 1;
        }
        result
    }

    // ── Expression lowering ──────────────────────────────────────────────────

    fn lower_expr(&mut self, expr: &HirExpr) -> Operand {
        let ty = fidan_ty_to_mir(&expr.ty);

        match &expr.kind {
            // ── Literals ──────────────────────────────────────────────────────
            HirExprKind::IntLit(v) => Operand::Const(MirLit::Int(*v)),
            HirExprKind::FloatLit(v) => Operand::Const(MirLit::Float(*v)),
            HirExprKind::StrLit(s) => Operand::Const(MirLit::Str(s.clone())),
            HirExprKind::BoolLit(b) => Operand::Const(MirLit::Bool(*b)),
            HirExprKind::Nothing => Operand::Const(MirLit::Nothing),

            // ── Variables ─────────────────────────────────────────────────────
            HirExprKind::Var(name) => {
                if let Some(local) = self.lookup_var(*name) {
                    Operand::Local(local)
                } else if let Some(fid) = self.fn_map.get(name).copied() {
                    // Function/action referenced as a first-class value.
                    Operand::Const(MirLit::FunctionRef(fid.0))
                } else if let Some(&gid) = self.global_map.get(name) {
                    // Module-level global: load from the globals table.
                    let dest = self.alloc_local();
                    self.emit(Instr::LoadGlobal { dest, global: gid });
                    Operand::Local(dest)
                } else {
                    // Unknown/builtin — represent as Nothing for now.
                    Operand::Const(MirLit::Nothing)
                }
            }
            HirExprKind::This => {
                if let Some(tr) = self.this_reg {
                    Operand::Local(tr)
                } else {
                    Operand::Const(MirLit::Nothing) // free function — shouldn't happen
                }
            }
            HirExprKind::Parent => {
                // `parent.field` / `parent.method()` — the receiver is still `this`;
                // the distinction for method calls is handled in the Call branch below.
                if let Some(tr) = self.this_reg {
                    Operand::Local(tr)
                } else {
                    Operand::Const(MirLit::Nothing)
                }
            }

            // ── Binary / Unary ────────────────────────────────────────────────
            HirExprKind::Assign { target, value } => {
                // Assignment-as-expression. We lower the value and store it.
                let val = self.lower_expr(value);
                match &target.kind {
                    HirExprKind::Var(name) => {
                        let dest = self.alloc_local();
                        self.emit(Instr::Assign {
                            dest,
                            ty: fidan_ty_to_mir(&target.ty),
                            rhs: Rvalue::Use(val.clone()),
                        });
                        // If the target is an unshadowed global, route writes
                        // through StoreGlobal so other functions see the update.
                        let is_unshadowed_global =
                            self.global_map.contains_key(name) && self.lookup_var(*name).is_none();
                        if is_unshadowed_global {
                            let gid = self.global_map[name];
                            self.emit(Instr::StoreGlobal {
                                global: gid,
                                value: Operand::Local(dest),
                            });
                        } else {
                            self.define_var(*name, dest);
                        }
                    }
                    HirExprKind::Field { object, field } => {
                        let recv = self.lower_expr(object);
                        self.emit(Instr::SetField {
                            object: recv,
                            field: *field,
                            value: val.clone(),
                        });
                    }
                    HirExprKind::Index { object, index } => {
                        let obj = self.lower_expr(object);
                        let idx = self.lower_expr(index);
                        self.emit(Instr::SetIndex {
                            object: obj,
                            index: idx,
                            value: val.clone(),
                        });
                    }
                    _ => {}
                }
                val
            }

            HirExprKind::Binary { op, lhs, rhs } => {
                let l = self.lower_expr(lhs);
                let r = self.lower_expr(rhs);
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty: ty.clone(),
                    rhs: Rvalue::Binary {
                        op: *op,
                        lhs: l,
                        rhs: r,
                    },
                });
                Operand::Local(dest)
            }
            HirExprKind::Unary { op, operand } => {
                let inner = self.lower_expr(operand);
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty: ty.clone(),
                    rhs: Rvalue::Unary {
                        op: *op,
                        operand: inner,
                    },
                });
                Operand::Local(dest)
            }

            HirExprKind::NullCoalesce { lhs, rhs } => {
                let l = self.lower_expr(lhs);
                let r = self.lower_expr(rhs);
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty: ty.clone(),
                    rhs: Rvalue::NullCoalesce { lhs: l, rhs: r },
                });
                Operand::Local(dest)
            }

            // ── Ternary (already desugared to IfExpr in HIR) ──────────────────
            HirExprKind::IfExpr {
                condition,
                then_val,
                else_val,
            } => {
                let cond = self.lower_expr(condition);
                let then_bb = self.alloc_block();
                let else_bb = self.alloc_block();
                let join_bb = self.alloc_block();
                let _entry_bb = self.cur_bb;

                self.set_terminator(Terminator::Branch {
                    cond: cond,
                    then_bb,
                    else_bb,
                });

                // then branch
                self.switch_to(then_bb);
                let then_op = self.lower_expr(then_val);
                let then_end = self.cur_bb;
                if !self.terminated {
                    self.goto(join_bb);
                }

                // else branch
                self.switch_to(else_bb);
                let else_op = self.lower_expr(else_val);
                let else_end = self.cur_bb;
                if !self.terminated {
                    self.goto(join_bb);
                }

                // join
                self.switch_to(join_bb);
                let dest = self.alloc_local();
                self.add_phi(
                    join_bb,
                    dest,
                    ty,
                    vec![(then_end, then_op), (else_end, else_op)],
                );
                Operand::Local(dest)
            }

            // ── Calls ─────────────────────────────────────────────────────────
            HirExprKind::Call { callee, args } => {
                // ① parent(args) → call parent's `new` constructor directly.
                if let HirExprKind::Parent = &callee.kind {
                    if let (Some(owner), Some(tr)) = (self.owner_class, self.this_reg) {
                        if let Some(&parent_cls) = self.parent_map.get(&owner) {
                            if let Some(&pfid) = self.method_ids.get(&(parent_cls, self.new_sym)) {
                                let dest = self.alloc_local();
                                let mut arg_ops = vec![Operand::Local(tr)];
                                arg_ops.extend(args.iter().map(|a| self.lower_expr(&a.value)));
                                self.emit(Instr::Call {
                                    dest: Some(dest),
                                    callee: Callee::Fn(pfid),
                                    args: arg_ops,
                                    span: expr.span,
                                });
                                return Operand::Local(dest);
                            }
                        }
                    }
                }

                // ② ClassName(args)  →  Construct + call `new`.
                if let HirExprKind::Var(name) = &callee.kind {
                    if let Some(init_fid) = self.obj_map.get(name).copied() {
                        let this_local = self.alloc_local();
                        self.emit(Instr::Assign {
                            dest: this_local,
                            ty: MirTy::Object(*name),
                            rhs: Rvalue::Construct {
                                ty: *name,
                                fields: vec![],
                            },
                        });
                        let mut arg_ops = vec![Operand::Local(this_local)];
                        arg_ops.extend(self.sort_args_for_fn(init_fid, args));
                        self.emit(Instr::Call {
                            dest: None,
                            callee: Callee::Fn(init_fid),
                            args: arg_ops,
                            span: expr.span,
                        });
                        return Operand::Local(this_local);
                    }
                }

                let callee_op = match &callee.kind {
                    HirExprKind::Field { object, field } => {
                        // ③ parent.method(args) → direct call to parent class's fn.
                        if let HirExprKind::Parent = &object.kind {
                            if let (Some(owner), Some(tr)) = (self.owner_class, self.this_reg) {
                                if let Some(&parent_cls) = self.parent_map.get(&owner) {
                                    if let Some(&pfid) = self.method_ids.get(&(parent_cls, *field))
                                    {
                                        let mut arg_ops = vec![Operand::Local(tr)];
                                        arg_ops
                                            .extend(args.iter().map(|a| self.lower_expr(&a.value)));
                                        let dest = self.alloc_local();
                                        self.emit(Instr::Call {
                                            dest: Some(dest),
                                            callee: Callee::Fn(pfid),
                                            args: arg_ops,
                                            span: expr.span,
                                        });
                                        return Operand::Local(dest);
                                    }
                                }
                            }
                        }
                        // ④ ObjType.new(args) → constructor call (explicit form).
                        if *field == self.new_sym {
                            if let HirExprKind::Var(cls_name) = &object.kind {
                                if let Some(&init_fid) = self.obj_map.get(cls_name) {
                                    let this_local = self.alloc_local();
                                    self.emit(Instr::Assign {
                                        dest: this_local,
                                        ty: MirTy::Object(*cls_name),
                                        rhs: Rvalue::Construct {
                                            ty: *cls_name,
                                            fields: vec![],
                                        },
                                    });
                                    let mut arg_ops = vec![Operand::Local(this_local)];
                                    arg_ops.extend(self.sort_args_for_fn(init_fid, args));
                                    self.emit(Instr::Call {
                                        dest: None,
                                        callee: Callee::Fn(init_fid),
                                        args: arg_ops,
                                        span: expr.span,
                                    });
                                    return Operand::Local(this_local);
                                }
                            }
                        }
                        let recv = self.lower_expr(object);
                        Callee::Method {
                            receiver: recv,
                            method: *field,
                        }
                    }
                    HirExprKind::Var(name) => {
                        if let Some(fid) = self.fn_map.get(name).copied() {
                            // ⑤ Extension action free call: prepend nothing for implicit `this`.
                            if self.fn_is_extension.contains(name) {
                                let dest = self.alloc_local();
                                let mut call_args = vec![Operand::Const(MirLit::Nothing)];
                                call_args.extend(self.sort_args_for_fn(fid, args));
                                self.emit(Instr::Call {
                                    dest: Some(dest),
                                    callee: Callee::Fn(fid),
                                    args: call_args,
                                    span: expr.span,
                                });
                                return Operand::Local(dest);
                            }
                            Callee::Fn(fid)
                        } else if let Some(&local) = self.env.get(name) {
                            // Local variable holding a function/action value — call dynamically.
                            Callee::Dynamic(Operand::Local(local))
                        } else {
                            Callee::Builtin(*name)
                        }
                    }
                    _ => {
                        let op = self.lower_expr(callee);
                        Callee::Dynamic(op)
                    }
                };
                let arg_ops: Vec<Operand> = if let Callee::Fn(fid) = &callee_op {
                    self.sort_args_for_fn(*fid, args)
                } else {
                    args.iter().map(|a| self.lower_expr(&a.value)).collect()
                };
                let dest = self.alloc_local();
                self.emit(Instr::Call {
                    dest: Some(dest),
                    callee: callee_op,
                    args: arg_ops,
                    span: expr.span,
                });
                Operand::Local(dest)
            }

            // ── Field access ──────────────────────────────────────────────────
            HirExprKind::Field { object, field } => {
                let recv = self.lower_expr(object);
                let dest = self.alloc_local();
                self.emit(Instr::GetField {
                    dest,
                    object: recv,
                    field: *field,
                });
                Operand::Local(dest)
            }

            HirExprKind::Index { object, index } => {
                let obj = self.lower_expr(object);
                let idx = self.lower_expr(index);
                let dest = self.alloc_local();
                self.emit(Instr::GetIndex {
                    dest,
                    object: obj,
                    index: idx,
                });
                Operand::Local(dest)
            }

            HirExprKind::Slice {
                target,
                start,
                end,
                inclusive,
                step,
            } => {
                let tgt = self.lower_expr(target);
                let s = start.as_deref().map(|e| self.lower_expr(e));
                let e = end.as_deref().map(|e| self.lower_expr(e));
                let st = step.as_deref().map(|e| self.lower_expr(e));
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty,
                    rhs: Rvalue::Slice {
                        target: tgt,
                        start: s,
                        end: e,
                        inclusive: *inclusive,
                        step: st,
                    },
                });
                Operand::Local(dest)
            }

            // ── Collections ───────────────────────────────────────────────────
            HirExprKind::List(elems) => {
                let ops: Vec<Operand> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty,
                    rhs: Rvalue::List(ops),
                });
                Operand::Local(dest)
            }
            HirExprKind::Dict(entries) => {
                let pairs: Vec<(Operand, Operand)> = entries
                    .iter()
                    .map(|(k, v)| (self.lower_expr(k), self.lower_expr(v)))
                    .collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty,
                    rhs: Rvalue::Dict(pairs),
                });
                Operand::Local(dest)
            }

            // ── Comprehensions ────────────────────────────────────────────────
            HirExprKind::ListComp {
                element,
                binding,
                iterable,
                filter,
            } => self.lower_list_comp(*binding, iterable, element, filter.as_deref()),
            HirExprKind::DictComp {
                key,
                value,
                binding,
                iterable,
                filter,
            } => self.lower_dict_comp(*binding, iterable, key, value, filter.as_deref()),
            HirExprKind::Tuple(elems) => {
                let ops: Vec<Operand> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty,
                    rhs: Rvalue::Tuple(ops),
                });
                Operand::Local(dest)
            }

            // ── String interpolation ──────────────────────────────────────────
            HirExprKind::StringInterp(parts) => {
                let mir_parts: Vec<MirStringPart> = parts
                    .iter()
                    .map(|p| match p {
                        HirInterpPart::Literal(s) => MirStringPart::Literal(s.clone()),
                        HirInterpPart::Expr(e) => MirStringPart::Operand(self.lower_expr(e)),
                    })
                    .collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty: MirTy::String,
                    rhs: Rvalue::StringInterp(mir_parts),
                });
                Operand::Local(dest)
            }

            // ── Concurrency ───────────────────────────────────────────────────
            HirExprKind::Spawn(inner) => {
                if let HirExprKind::Call { callee, args } = &inner.kind {
                    match &callee.kind {
                        // Direct free-function: `spawn fn(args)` — statically resolved.
                        HirExprKind::Var(name) => {
                            if let Some(&fn_id) = self.fn_map.get(name) {
                                let arg_ops: Vec<Operand> =
                                    args.iter().map(|a| self.lower_expr(&a.value)).collect();
                                let dest = self.alloc_local();
                                self.emit(Instr::SpawnExpr {
                                    dest,
                                    task_fn: fn_id,
                                    args: arg_ops,
                                });
                                return Operand::Local(dest);
                            }
                            // Named var that isn't in fn_map → treat as a function-value.
                            let fn_op = self.lower_expr(callee);
                            let mut spawn_args = vec![fn_op];
                            spawn_args.extend(args.iter().map(|a| self.lower_expr(&a.value)));
                            let dest = self.alloc_local();
                            self.emit(Instr::SpawnDynamic {
                                dest,
                                method: None,
                                args: spawn_args,
                            });
                            return Operand::Local(dest);
                        }
                        // Method call: `spawn obj.method(args)`.
                        HirExprKind::Field { object, field } => {
                            let recv_op = self.lower_expr(object);
                            let mut spawn_args = vec![recv_op];
                            spawn_args.extend(args.iter().map(|a| self.lower_expr(&a.value)));
                            let dest = self.alloc_local();
                            self.emit(Instr::SpawnDynamic {
                                dest,
                                method: Some(*field),
                                args: spawn_args,
                            });
                            return Operand::Local(dest);
                        }
                        // Any other callee shape: evaluate and dispatch dynamically.
                        _ => {
                            let fn_op = self.lower_expr(callee);
                            let mut spawn_args = vec![fn_op];
                            spawn_args.extend(args.iter().map(|a| self.lower_expr(&a.value)));
                            let dest = self.alloc_local();
                            self.emit(Instr::SpawnDynamic {
                                dest,
                                method: None,
                                args: spawn_args,
                            });
                            return Operand::Local(dest);
                        }
                    }
                }
                // Non-call spawn (unusual, e.g. `spawn someExpr`) → synchronous fallback.
                let op = self.lower_expr(inner);
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty,
                    rhs: Rvalue::Use(op),
                });
                Operand::Local(dest)
            }
            HirExprKind::Await(inner) => {
                let op = self.lower_expr(inner);
                let dest = self.alloc_local();
                self.emit(Instr::AwaitPending { dest, handle: op });
                Operand::Local(dest)
            }

            // ── Check expression ──────────────────────────────────────────────
            HirExprKind::CheckExpr { scrutinee, arms } => {
                self.lower_check_expr(scrutinee, arms, &expr.ty)
            }

            HirExprKind::Error => Operand::Const(MirLit::Nothing),
        }
    }

    fn lower_check_expr(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirCheckExprArm],
        result_ty: &FidanType,
    ) -> Operand {
        let scrut = self.lower_expr(scrutinee);
        let join_bb = self.alloc_block();
        let result_local = self.alloc_local();
        let ty = fidan_ty_to_mir(result_ty);

        let mut phi_ops: Vec<(BlockId, Operand)> = vec![];

        for arm in arms {
            let arm_bb = self.alloc_block();
            let next_bb = self.alloc_block();

            // Wildcard `_` pattern: unconditionally enter the arm.
            let is_wildcard =
                matches!(&arm.pattern.kind, HirExprKind::Var(s) if *s == self.wildcard_sym);

            if is_wildcard {
                self.goto(arm_bb);
            } else {
                // Condition: scrutinee == pattern
                let pat = self.lower_expr(&arm.pattern);
                let match_local = self.alloc_local();
                self.emit(Instr::Assign {
                    dest: match_local,
                    ty: MirTy::Boolean,
                    rhs: Rvalue::Binary {
                        op: BinOp::Eq,
                        lhs: scrut.clone(),
                        rhs: pat,
                    },
                });
                self.set_terminator(Terminator::Branch {
                    cond: Operand::Local(match_local),
                    then_bb: arm_bb,
                    else_bb: next_bb,
                });
            }

            // Arm body — lower all stmts, capturing the last expression value.
            self.switch_to(arm_bb);
            let arm_val = if let Some((last, rest)) = arm.body.split_last() {
                self.lower_stmts(rest);
                match last {
                    HirStmt::Expr(e) => self.lower_expr(e),
                    HirStmt::Return {
                        value: Some(ret_expr),
                        ..
                    } => self.lower_expr(ret_expr),
                    _ => {
                        self.lower_stmt(last);
                        Operand::Const(MirLit::Nothing)
                    }
                }
            } else {
                Operand::Const(MirLit::Nothing)
            };
            let arm_end = self.cur_bb;
            if !self.terminated {
                self.goto(join_bb);
            }
            phi_ops.push((arm_end, arm_val));

            if is_wildcard {
                self.switch_to(next_bb);
                break;
            }
            self.switch_to(next_bb);
        }

        // Fallthrough (no match) → join
        if !self.terminated {
            self.goto(join_bb);
        }
        phi_ops.push((self.cur_bb, Operand::Const(MirLit::Nothing)));

        self.switch_to(join_bb);
        self.add_phi(join_bb, result_local, ty, phi_ops);
        Operand::Local(result_local)
    }

    // ── Statement lowering ────────────────────────────────────────────────────

    fn lower_stmts(&mut self, stmts: &[HirStmt]) {
        for stmt in stmts {
            if self.terminated {
                break;
            }
            self.lower_stmt(stmt);
        }
    }

    fn lower_stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            // ── Variable declaration ────────────────────────────────────────────
            HirStmt::VarDecl { name, ty, init, .. } => {
                let mir_ty = fidan_ty_to_mir(ty);
                let dest = self.alloc_local();
                if let Some(init_expr) = init {
                    let val = self.lower_expr(init_expr);
                    self.emit(Instr::Assign {
                        dest,
                        ty: mir_ty,
                        rhs: Rvalue::Use(val),
                    });
                } else {
                    self.emit(Instr::Assign {
                        dest,
                        ty: mir_ty,
                        rhs: Rvalue::Literal(MirLit::Nothing),
                    });
                }
                // In the top-level init function a `VarDecl` IS the global
                // initialisation.  Write to the globals table but do NOT add
                // the name to the local SSA `env`; this forces all subsequent
                // reads in `init_stmts` to go through `LoadGlobal`, so they
                // always reflect mutations made by called functions.
                if self.is_init_fn {
                    if let Some(&gid) = self.global_map.get(name) {
                        self.emit(Instr::StoreGlobal {
                            global: gid,
                            value: Operand::Local(dest),
                        });
                        return; // do NOT define_var — keep globals out of the SSA env
                    }
                }
                self.define_var(*name, dest);
            }

            HirStmt::Destructure {
                bindings,
                binding_tys,
                value,
                ..
            } => {
                let tuple_op = self.lower_expr(value);
                // Unpack each element into its own local.
                for (i, (name, ty)) in bindings.iter().zip(binding_tys.iter()).enumerate() {
                    let idx_local = self.alloc_local();
                    self.emit(Instr::Assign {
                        dest: idx_local,
                        ty: MirTy::Integer,
                        rhs: Rvalue::Literal(MirLit::Int(i as i64)),
                    });
                    let elem_local = self.alloc_local();
                    self.emit(Instr::GetIndex {
                        dest: elem_local,
                        object: tuple_op.clone(),
                        index: Operand::Local(idx_local),
                    });
                    let dest = self.alloc_local();
                    self.emit(Instr::Assign {
                        dest,
                        ty: fidan_ty_to_mir(ty),
                        rhs: Rvalue::Use(Operand::Local(elem_local)),
                    });
                    self.define_var(*name, dest);
                }
            }

            // ── Assignment ──────────────────────────────────────────────────────
            HirStmt::Assign { target, value, .. } => {
                let val = self.lower_expr(value);
                match &target.kind {
                    HirExprKind::Var(name) => {
                        // Re-assign: new SSA name.
                        let dest = self.alloc_local();
                        self.emit(Instr::Assign {
                            dest,
                            ty: fidan_ty_to_mir(&target.ty),
                            rhs: Rvalue::Use(val),
                        });
                        // Only route through the global store when the name
                        // isn’t currently a local variable (i.e. no VarDecl
                        // in this function shadowed the global).  If it IS
                        // a local, just update the SSA env as usual.
                        let is_unshadowed_global =
                            self.global_map.contains_key(name) && self.lookup_var(*name).is_none();
                        if is_unshadowed_global {
                            let gid = self.global_map[name];
                            self.emit(Instr::StoreGlobal {
                                global: gid,
                                value: Operand::Local(dest),
                            });
                            // Do NOT define_var — next read will use LoadGlobal.
                        } else {
                            self.define_var(*name, dest);
                        }
                    }
                    HirExprKind::Field { object, field } => {
                        let recv = self.lower_expr(object);
                        self.emit(Instr::SetField {
                            object: recv,
                            field: *field,
                            value: val,
                        });
                    }
                    HirExprKind::Index { object, index } => {
                        let obj = self.lower_expr(object);
                        let idx = self.lower_expr(index);
                        self.emit(Instr::SetIndex {
                            object: obj,
                            index: idx,
                            value: val,
                        });
                    }
                    _ => {
                        // Unsupported target: emit as a no-op.
                    }
                }
            }

            // ── Bare expression ─────────────────────────────────────────────────
            HirStmt::Expr(expr) => {
                match &expr.kind {
                    // Calls as statements: dest = None
                    HirExprKind::Call { callee, args } => {
                        // ① parent(args) → call parent's `new` constructor (statement form).
                        if let HirExprKind::Parent = &callee.kind {
                            if let (Some(owner), Some(tr)) = (self.owner_class, self.this_reg) {
                                if let Some(&parent_cls) = self.parent_map.get(&owner) {
                                    if let Some(&pfid) =
                                        self.method_ids.get(&(parent_cls, self.new_sym))
                                    {
                                        let mut arg_ops = vec![Operand::Local(tr)];
                                        arg_ops
                                            .extend(args.iter().map(|a| self.lower_expr(&a.value)));
                                        self.emit(Instr::Call {
                                            dest: None,
                                            callee: Callee::Fn(pfid),
                                            args: arg_ops,
                                            span: expr.span,
                                        });
                                        return;
                                    }
                                }
                            }
                        }
                        // ② ClassName(args) constructor call as a statement.
                        if let HirExprKind::Var(name) = &callee.kind {
                            if let Some(init_fid) = self.obj_map.get(name).copied() {
                                let this_local = self.alloc_local();
                                self.emit(Instr::Assign {
                                    dest: this_local,
                                    ty: MirTy::Object(*name),
                                    rhs: Rvalue::Construct {
                                        ty: *name,
                                        fields: vec![],
                                    },
                                });
                                let mut arg_ops = vec![Operand::Local(this_local)];
                                arg_ops.extend(args.iter().map(|a| self.lower_expr(&a.value)));
                                self.emit(Instr::Call {
                                    dest: None,
                                    callee: Callee::Fn(init_fid),
                                    args: arg_ops,
                                    span: expr.span,
                                });
                                return;
                            }
                        }
                        let callee_op = match &callee.kind {
                            HirExprKind::Field { object, field } => {
                                // ③ parent.method(args) → direct call to parent class fn.
                                if let HirExprKind::Parent = &object.kind {
                                    if let (Some(owner), Some(tr)) =
                                        (self.owner_class, self.this_reg)
                                    {
                                        if let Some(&parent_cls) = self.parent_map.get(&owner) {
                                            if let Some(&pfid) =
                                                self.method_ids.get(&(parent_cls, *field))
                                            {
                                                let mut arg_ops = vec![Operand::Local(tr)];
                                                arg_ops.extend(
                                                    args.iter().map(|a| self.lower_expr(&a.value)),
                                                );
                                                self.emit(Instr::Call {
                                                    dest: None,
                                                    callee: Callee::Fn(pfid),
                                                    args: arg_ops,
                                                    span: expr.span,
                                                });
                                                return;
                                            }
                                        }
                                    }
                                }
                                // ④ ObjType.new(args) constructor (explicit, statement form).
                                if *field == self.new_sym {
                                    if let HirExprKind::Var(cls_name) = &object.kind {
                                        if let Some(&init_fid) = self.obj_map.get(cls_name) {
                                            let this_local = self.alloc_local();
                                            self.emit(Instr::Assign {
                                                dest: this_local,
                                                ty: MirTy::Object(*cls_name),
                                                rhs: Rvalue::Construct {
                                                    ty: *cls_name,
                                                    fields: vec![],
                                                },
                                            });
                                            let mut arg_ops = vec![Operand::Local(this_local)];
                                            arg_ops.extend(
                                                args.iter().map(|a| self.lower_expr(&a.value)),
                                            );
                                            self.emit(Instr::Call {
                                                dest: None,
                                                callee: Callee::Fn(init_fid),
                                                args: arg_ops,
                                                span: expr.span,
                                            });
                                            return;
                                        }
                                    }
                                }
                                let recv = self.lower_expr(object);
                                Callee::Method {
                                    receiver: recv,
                                    method: *field,
                                }
                            }
                            HirExprKind::Var(name) => {
                                if let Some(fid) = self.fn_map.get(name).copied() {
                                    // ⑤ Extension action free call as statement: prepend nothing for `this`.
                                    if self.fn_is_extension.contains(name) {
                                        let mut call_args = vec![Operand::Const(MirLit::Nothing)];
                                        call_args
                                            .extend(args.iter().map(|a| self.lower_expr(&a.value)));
                                        self.emit(Instr::Call {
                                            dest: None,
                                            callee: Callee::Fn(fid),
                                            args: call_args,
                                            span: expr.span,
                                        });
                                        return;
                                    }
                                    Callee::Fn(fid)
                                } else if let Some(&local) = self.env.get(name) {
                                    // Local variable holding a function/action value — call dynamically.
                                    Callee::Dynamic(Operand::Local(local))
                                } else {
                                    Callee::Builtin(*name)
                                }
                            }
                            _ => {
                                let op = self.lower_expr(callee);
                                Callee::Dynamic(op)
                            }
                        };
                        let arg_ops: Vec<Operand> =
                            args.iter().map(|a| self.lower_expr(&a.value)).collect();
                        self.emit(Instr::Call {
                            dest: None,
                            callee: callee_op,
                            args: arg_ops,
                            span: expr.span,
                        });
                    }
                    _ => {
                        // Non-call expression used as statement: evaluate for side effects.
                        self.lower_expr(expr);
                    }
                }
            }

            // ── Return ──────────────────────────────────────────────────────────
            HirStmt::Return { value, .. } => {
                let op = value.as_ref().map(|e| self.lower_expr(e));
                self.set_terminator(Terminator::Return(op));
            }

            HirStmt::Break { .. } => {
                if let Some(&(_, exit_bb)) = self.loop_stack.last() {
                    self.goto(exit_bb);
                } else {
                    self.set_terminator(Terminator::Unreachable);
                }
            }
            HirStmt::Continue { .. } => {
                if let Some(&(continue_bb, _)) = self.loop_stack.last() {
                    // Snapshot env so we can build step_bb phis later.
                    let snap = self.env.clone();
                    let from_bb = self.cur_bb;
                    self.continue_sites
                        .entry(continue_bb)
                        .or_default()
                        .push((from_bb, snap));
                    self.goto(continue_bb);
                } else {
                    self.set_terminator(Terminator::Unreachable);
                }
            }

            // ── Panic / throw ───────────────────────────────────────────────────
            HirStmt::Panic { value, .. } => {
                let val = self.lower_expr(value);
                self.set_terminator(Terminator::Throw { value: val });
            }

            // ── If / else ────────────────────────────────────────────────────────
            HirStmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                self.lower_if(condition, then_body, else_ifs, else_body.as_deref());
            }

            // ── Check statement ──────────────────────────────────────────────────
            HirStmt::Check {
                scrutinee, arms, ..
            } => {
                self.lower_check_stmt(scrutinee, arms);
            }

            // ── For loop ─────────────────────────────────────────────────────────
            HirStmt::For {
                binding,
                binding_ty,
                iterable,
                body,
                ..
            } => {
                self.lower_for_loop(*binding, binding_ty, iterable, body);
            }

            // ── While loop ───────────────────────────────────────────────────────
            HirStmt::While {
                condition, body, ..
            } => {
                self.lower_while_loop(condition, body);
            }

            // ── Try / attempt ────────────────────────────────────────────────────
            HirStmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                self.lower_attempt(body, catches, otherwise.as_deref(), finally.as_deref());
            }

            // ── Parallel for ─────────────────────────────────────────────────
            // Emit a real `ParallelIter` MIR instruction. The body gets its
            // own synthetic function that is lowered after the current function
            // is complete (via `par_for_pending` drain loop in lower_program).
            HirStmt::ParallelFor {
                binding,
                binding_ty,
                iterable,
                body,
                ..
            } => {
                // 1. Lower the iterable into an operand in the current function.
                let collection = self.lower_expr(iterable);

                // 2. Collect every variable referenced inside the body that is
                //    already defined in the current function's environment.
                //    These become extra parameters of the synthetic body function
                //    (the "closure args").
                let used = collect_hir_used_vars(body);
                let mut env_params: Vec<(Symbol, MirTy)> = Vec::new();
                let mut closure_args: Vec<Operand> = Vec::new();
                for (&sym, &local) in &self.env {
                    // Only include symbols that are actually referenced in the body
                    // and that are NOT the loop binding itself.
                    if sym != *binding && used.contains(&sym) {
                        env_params.push((sym, MirTy::Dynamic));
                        closure_args.push(Operand::Local(local));
                    }
                }

                // 3. Pre-allocate a FunctionId + placeholder entry in prog.functions.
                let body_fn_id = FunctionId(self.prog.functions.len() as u32);
                // The name is purely informational (for dumps/debug).
                let par_sym = *binding; // reuse the binding symbol as a hint
                self.prog
                    .functions
                    .push(MirFunction::new(body_fn_id, par_sym, MirTy::Nothing));

                // 4. Emit the ParallelIter instruction in the current function.
                self.emit(Instr::ParallelIter {
                    collection,
                    body_fn: body_fn_id,
                    closure_args,
                });

                // 5. Defer lowering of the body to after the current function
                //    body finishes (only one &mut MirProgram borrow at a time).
                self.par_for_pending
                    .borrow_mut()
                    .push_back(PendingParallelFor {
                        fn_id: body_fn_id,
                        binding: Some((*binding, fidan_ty_to_mir(binding_ty))),
                        env_params,
                        body_ptr: body.as_ptr(),
                        body_len: body.len(),
                    });
            }

            // ── Concurrent block ─────────────────────────────────────────────
            // Each task body becomes a synthetic function, spawned on a real OS
            // thread via SpawnConcurrent.  JoinAll waits for all of them.
            HirStmt::ConcurrentBlock { tasks, .. } => {
                let mut handles: Vec<LocalId> = Vec::new();
                for task in tasks {
                    // Collect variables captured from the outer scope.
                    let used = collect_hir_used_vars(&task.body);
                    let mut env_params: Vec<(Symbol, MirTy)> = Vec::new();
                    let mut closure_args: Vec<Operand> = Vec::new();
                    for (&sym, &local) in &self.env {
                        if used.contains(&sym) {
                            env_params.push((sym, MirTy::Dynamic));
                            closure_args.push(Operand::Local(local));
                        }
                    }

                    // Pre-allocate a synthetic function for this task body.
                    let task_fn_id = FunctionId(self.prog.functions.len() as u32);
                    self.prog.functions.push(MirFunction::new(
                        task_fn_id,
                        task.name.unwrap_or(self.new_sym), // use task label when available
                        MirTy::Nothing,
                    ));

                    // Spawn the task and collect its handle.
                    let handle = self.alloc_local();
                    self.emit(Instr::SpawnConcurrent {
                        handle,
                        task_fn: task_fn_id,
                        args: closure_args,
                    });
                    handles.push(handle);

                    // Defer body lowering via the shared pending queue.
                    self.par_for_pending
                        .borrow_mut()
                        .push_back(PendingParallelFor {
                            fn_id: task_fn_id,
                            binding: None, // no per-iteration binding for tasks
                            env_params,
                            body_ptr: task.body.as_ptr(),
                            body_len: task.body.len(),
                        });
                }
                // Wait for all tasks to complete.
                if !handles.is_empty() {
                    self.emit(Instr::JoinAll { handles });
                }
            }

            HirStmt::Error { .. } => {} // skip error placeholders
        }
    }

    // ── if / else lowering ────────────────────────────────────────────────────

    fn lower_if(
        &mut self,
        condition: &HirExpr,
        then_body: &[HirStmt],
        else_ifs: &[HirElseIf],
        else_body: Option<&[HirStmt]>,
    ) {
        let cond = self.lower_expr(condition);

        let then_bb = self.alloc_block();
        let else_bb = self.alloc_block();
        let join_bb = self.alloc_block();

        self.set_terminator(Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        });

        // ── then branch ───────────────────────────────────────────────────────
        let env_before = self.env.clone();
        self.switch_to(then_bb);
        self.lower_stmts(then_body);
        let env_after_then = self.env.clone();
        let then_end = self.cur_bb;
        if !self.terminated {
            self.goto(join_bb);
        }

        // ── else-ifs + plain else ─────────────────────────────────────────────
        self.env = env_before.clone();
        self.switch_to(else_bb);

        if !else_ifs.is_empty() {
            // Chain else-ifs recursively.
            let first_ei = &else_ifs[0];
            let rest = &else_ifs[1..];
            self.lower_if(&first_ei.condition, &first_ei.body, rest, else_body);
        } else if let Some(else_stmts) = else_body {
            self.lower_stmts(else_stmts);
        }

        let env_after_else = self.env.clone();
        let else_end = self.cur_bb;
        if !self.terminated {
            self.goto(join_bb);
        }

        // ── join: φ-nodes for variables changed in either branch ──────────────
        self.switch_to(join_bb);
        self.env = env_before.clone();

        let changed_then = env_diff(&env_before, &env_after_then);
        let changed_else = env_diff(&env_before, &env_after_else);
        let mut changed: Vec<Symbol> = changed_then;
        for s in changed_else {
            if !changed.contains(&s) {
                changed.push(s);
            }
        }

        for sym in changed {
            let then_op = env_after_then
                .get(&sym)
                .map(|&l| Operand::Local(l))
                .unwrap_or_else(|| {
                    env_before
                        .get(&sym)
                        .map(|&l| Operand::Local(l))
                        .unwrap_or(Operand::Const(MirLit::Nothing))
                });
            let else_op = env_after_else
                .get(&sym)
                .map(|&l| Operand::Local(l))
                .unwrap_or_else(|| {
                    env_before
                        .get(&sym)
                        .map(|&l| Operand::Local(l))
                        .unwrap_or(Operand::Const(MirLit::Nothing))
                });
            let phi_local = self.alloc_local();
            self.add_phi(
                join_bb,
                phi_local,
                MirTy::Dynamic, // conservative type for merged vars
                vec![(then_end, then_op), (else_end, else_op)],
            );
            self.define_var(sym, phi_local);
        }
    }

    // ── check statement lowering ───────────────────────────────────────────────

    fn lower_check_stmt(&mut self, scrutinee: &HirExpr, arms: &[HirCheckArm]) {
        let scrut = self.lower_expr(scrutinee);
        let join_bb = self.alloc_block();
        let env_before = self.env.clone();

        // Track (arm_end_bb, env_after_arm) for phi-node merging at the join.
        let mut arm_end_envs: Vec<(BlockId, VarEnv)> = vec![];

        for arm in arms {
            let arm_bb = self.alloc_block();
            let next_bb = self.alloc_block();

            // Wildcard `_` arm: unconditionally enters the arm body.
            let is_wildcard =
                matches!(&arm.pattern.kind, HirExprKind::Var(s) if *s == self.wildcard_sym);

            if is_wildcard {
                self.goto(arm_bb);
            } else {
                self.env = env_before.clone();
                let pat = self.lower_expr(&arm.pattern);
                let cmp = self.alloc_local();
                self.emit(Instr::Assign {
                    dest: cmp,
                    ty: MirTy::Boolean,
                    rhs: Rvalue::Binary {
                        op: BinOp::Eq,
                        lhs: scrut.clone(),
                        rhs: pat,
                    },
                });
                self.set_terminator(Terminator::Branch {
                    cond: Operand::Local(cmp),
                    then_bb: arm_bb,
                    else_bb: next_bb,
                });
            }

            self.env = env_before.clone();
            self.switch_to(arm_bb);
            self.lower_stmts(&arm.body);
            let arm_end = self.cur_bb;
            arm_end_envs.push((arm_end, self.env.clone()));
            if !self.terminated {
                self.goto(join_bb);
            }

            if is_wildcard {
                // Wildcard catches everything; remaining arms are unreachable.
                self.switch_to(next_bb);
                break;
            }
            self.env = env_before.clone();
            self.switch_to(next_bb);
        }

        // Fallthrough — no arm matched, env stays as env_before.
        let fallthrough_bb = self.cur_bb;
        arm_end_envs.push((fallthrough_bb, env_before.clone()));
        if !self.terminated {
            self.goto(join_bb);
        }

        // Switch to join and emit phi nodes for variables changed in any arm.
        self.switch_to(join_bb);
        self.env = env_before.clone();

        let mut changed: Vec<Symbol> = vec![];
        for (_, arm_env) in &arm_end_envs {
            for sym in env_diff(&env_before, arm_env) {
                if !changed.contains(&sym) {
                    changed.push(sym);
                }
            }
        }
        for sym in changed {
            let phi_local = self.alloc_local();
            let phi_ops: Vec<(BlockId, Operand)> = arm_end_envs
                .iter()
                .map(|(bb, arm_env)| {
                    let op = arm_env
                        .get(&sym)
                        .or_else(|| env_before.get(&sym))
                        .map(|&l| Operand::Local(l))
                        .unwrap_or(Operand::Const(MirLit::Nothing));
                    (*bb, op)
                })
                .collect();
            self.add_phi(join_bb, phi_local, MirTy::Dynamic, phi_ops);
            self.define_var(sym, phi_local);
        }
    }

    // ── for-loop lowering ─────────────────────────────────────────────────────

    fn lower_for_loop(
        &mut self,
        binding: Symbol,
        binding_ty: &FidanType,
        iterable: &HirExpr,
        body: &[HirStmt],
    ) {
        let list_op = self.lower_expr(iterable);

        // idx = 0
        let idx0 = self.alloc_local();
        self.emit(Instr::Assign {
            dest: idx0,
            ty: MirTy::Integer,
            rhs: Rvalue::Literal(MirLit::Int(0)),
        });

        // len = list.len
        let len_local = self.alloc_local();
        self.emit(Instr::Call {
            dest: Some(len_local),
            callee: Callee::Method {
                receiver: list_op.clone(),
                method: self.len_sym,
            },
            args: vec![],
            span: fidan_source::Span::default(),
        });

        let pre_bb = self.cur_bb;
        let header_bb = self.alloc_block();
        let body_bb = self.alloc_block();
        let step_bb = self.alloc_block(); // where `continue` jumps to; increments idx
        let exit_bb = self.alloc_block();

        self.goto(header_bb);
        self.switch_to(header_bb);

        // ── Phi nodes for variables mutated in the loop body ──────────────────
        let env_before = self.env.clone();
        let written = collect_assigned_vars(body);
        let mut phi_vars: Vec<(Symbol, LocalId)> = Vec::new();
        for sym in &written {
            if let Some(&pre_local) = env_before.get(sym) {
                let phi_local = self.alloc_local();
                self.add_phi(
                    header_bb,
                    phi_local,
                    MirTy::Dynamic,
                    vec![(pre_bb, Operand::Local(pre_local))],
                );
                self.define_var(*sym, phi_local);
                phi_vars.push((*sym, phi_local));
            }
        }

        // ── Phi node for the loop index ───────────────────────────────────────
        let idx_phi = self.alloc_local();
        self.add_phi(
            header_bb,
            idx_phi,
            MirTy::Integer,
            vec![(pre_bb, Operand::Local(idx0))],
        );

        // Condition: idx_phi < len_local
        let cond = self.alloc_local();
        self.emit(Instr::Assign {
            dest: cond,
            ty: MirTy::Boolean,
            rhs: Rvalue::Binary {
                op: fidan_ast::BinOp::Lt,
                lhs: Operand::Local(idx_phi),
                rhs: Operand::Local(len_local),
            },
        });
        self.set_terminator(Terminator::Branch {
            cond: Operand::Local(cond),
            then_bb: body_bb,
            else_bb: exit_bb,
        });

        // ── Loop body ─────────────────────────────────────────────────────────
        self.switch_to(body_bb);

        // binding = list[idx_phi]
        let elem = self.alloc_local();
        self.emit(Instr::GetIndex {
            dest: elem,
            object: list_op.clone(),
            index: Operand::Local(idx_phi),
        });
        let binding_local = self.alloc_local();
        self.emit(Instr::Assign {
            dest: binding_local,
            ty: fidan_ty_to_mir(binding_ty),
            rhs: Rvalue::Use(Operand::Local(elem)),
        });
        self.define_var(binding, binding_local);

        // `continue` targets step_bb (index increment), not the header directly.
        self.loop_stack.push((step_bb, exit_bb));
        self.lower_stmts(body);
        self.loop_stack.pop();

        // Snapshot the env at body-end (for the normal fallthrough path).
        let body_end_bb = self.cur_bb;
        let body_end_terminated = self.terminated;
        let body_end_env = self.env.clone();
        if !self.terminated {
            self.goto(step_bb);
        }

        // Collect all continue sites that targeted step_bb.
        let cont_sites = self.continue_sites.remove(&step_bb).unwrap_or_default();

        // ── step_bb: build phi nodes for mutable vars, increment index ─────────
        self.switch_to(step_bb);

        // For each phi var: create a phi in step_bb merging body_end + all continue sites.
        // step_bb phi output → used as the back-edge value for header_bb phi.
        let mut step_phi_locals: Vec<LocalId> = Vec::new();
        for (sym, _) in &phi_vars {
            let mut phi_operands: Vec<(BlockId, Operand)> = Vec::new();
            // body-end fallthrough (only if it wasn't terminated by break/continue)
            if !body_end_terminated {
                let local = body_end_env
                    .get(sym)
                    .copied()
                    .unwrap_or_else(|| env_before[sym]);
                phi_operands.push((body_end_bb, Operand::Local(local)));
            }
            // each continue site
            for (from_bb, env_snap) in &cont_sites {
                let local = env_snap
                    .get(sym)
                    .copied()
                    .unwrap_or_else(|| env_before[sym]);
                phi_operands.push((*from_bb, Operand::Local(local)));
            }
            match phi_operands.len() {
                0 => {
                    // No predecessors — use the initial (header phi) value.
                    step_phi_locals.push(env_before[sym]);
                }
                1 => {
                    // Single predecessor — no phi needed.
                    let local = match phi_operands[0].1 {
                        Operand::Local(l) => l,
                        _ => env_before[sym],
                    };
                    step_phi_locals.push(local);
                }
                _ => {
                    let phi_local = self.alloc_local();
                    self.add_phi(step_bb, phi_local, MirTy::Dynamic, phi_operands);
                    step_phi_locals.push(phi_local);
                }
            }
        }

        // idx_next = idx_phi + 1
        let idx_next = self.alloc_local();
        self.emit(Instr::Assign {
            dest: idx_next,
            ty: MirTy::Integer,
            rhs: Rvalue::Binary {
                op: fidan_ast::BinOp::Add,
                lhs: Operand::Local(idx_phi),
                rhs: Operand::Const(MirLit::Int(1)),
            },
        });
        let step_end = self.cur_bb;
        self.goto(header_bb);

        // ── Back-patch header_bb phis ─────────────────────────────────────────
        // All back-edges to header come from step_end only.
        let idx_phi_pos = phi_vars.len();
        self.func_mut().block_mut(header_bb).phis[idx_phi_pos]
            .operands
            .push((step_end, Operand::Local(idx_next)));

        for (i, step_local) in step_phi_locals.iter().enumerate() {
            self.func_mut().block_mut(header_bb).phis[i]
                .operands
                .push((step_end, Operand::Local(*step_local)));
        }

        // ── Exit ──────────────────────────────────────────────────────────────
        self.switch_to(exit_bb);
        // After the loop the phi-merged value is the observable state of each var.
        for (sym, phi_local) in &phi_vars {
            self.define_var(*sym, *phi_local);
        }
    }

    // ── Comprehension desugaring ──────────────────────────────────────────────

    /// Desugar `[element for binding in iterable (if filter)]` to an inline loop.
    /// Returns an `Operand::Local` holding the freshly-built list.
    fn lower_list_comp(
        &mut self,
        binding: fidan_lexer::Symbol,
        iterable: &HirExpr,
        element: &HirExpr,
        filter: Option<&HirExpr>,
    ) -> Operand {
        // result = []
        let result = self.alloc_local();
        self.emit(Instr::Assign {
            dest: result,
            ty: MirTy::Dynamic,
            rhs: Rvalue::List(vec![]),
        });
        self.lower_comp_loop(binding, iterable, filter, |ctx, elem_op| {
            // Compute element expression value.
            let elem_val = ctx.lower_expr(element);
            // Discard elem_op (already stored as `binding` via define_var); use elem_val.
            let _ = elem_op;
            // result.append(elem_val)
            let append_sym = ctx.append_sym;
            ctx.emit(Instr::Call {
                dest: None,
                callee: Callee::Method {
                    receiver: Operand::Local(result),
                    method: append_sym,
                },
                args: vec![elem_val],
                span: fidan_source::Span::default(),
            });
        });
        Operand::Local(result)
    }

    /// Desugar `{key: value for binding in iterable (if filter)}` to an inline loop.
    /// Returns an `Operand::Local` holding the freshly-built dict.
    fn lower_dict_comp(
        &mut self,
        binding: fidan_lexer::Symbol,
        iterable: &HirExpr,
        key: &HirExpr,
        value: &HirExpr,
        filter: Option<&HirExpr>,
    ) -> Operand {
        // result = {}
        let result = self.alloc_local();
        self.emit(Instr::Assign {
            dest: result,
            ty: MirTy::Dynamic,
            rhs: Rvalue::Dict(vec![]),
        });
        self.lower_comp_loop(binding, iterable, filter, |ctx, _elem_op| {
            let key_val = ctx.lower_expr(key);
            let val_val = ctx.lower_expr(value);
            // result[key] = value  (SetIndex converts any key to string via display)
            ctx.emit(Instr::SetIndex {
                object: Operand::Local(result),
                index: key_val,
                value: val_val,
            });
        });
        Operand::Local(result)
    }

    /// Shared inner-loop scaffold for both comprehension types.
    ///
    /// Emits:
    /// ```text
    /// idx = 0
    /// len = iterable.len()
    /// header:
    ///   idx_phi = φ(pre: idx, step: idx_next)
    ///   cond = idx_phi < len
    ///   branch cond → body | exit
    /// body:
    ///   elem = iterable[idx_phi]
    ///   binding = elem
    ///   [filter branch if present]
    ///   emit_body(ctx, elem_op)
    ///   goto step
    /// step:
    ///   idx_next = idx_phi + 1
    ///   goto header
    /// exit:
    /// ```
    /// The `emit_body` closure is called with `&mut self` and the elem operand.
    fn lower_comp_loop<F>(
        &mut self,
        binding: fidan_lexer::Symbol,
        iterable: &HirExpr,
        filter: Option<&HirExpr>,
        emit_body: F,
    ) where
        F: Fn(&mut Self, Operand),
    {
        let list_op = self.lower_expr(iterable);

        // idx = 0
        let idx0 = self.alloc_local();
        self.emit(Instr::Assign {
            dest: idx0,
            ty: MirTy::Integer,
            rhs: Rvalue::Literal(MirLit::Int(0)),
        });

        // len = list_op.len()
        let len_local = self.alloc_local();
        let len_sym = self.len_sym;
        self.emit(Instr::Call {
            dest: Some(len_local),
            callee: Callee::Method {
                receiver: list_op.clone(),
                method: len_sym,
            },
            args: vec![],
            span: fidan_source::Span::default(),
        });

        let pre_bb = self.cur_bb;
        let header_bb = self.alloc_block();
        let body_bb = self.alloc_block();
        let step_bb = self.alloc_block();
        let exit_bb = self.alloc_block();

        self.goto(header_bb);
        self.switch_to(header_bb);

        // idx_phi = φ(pre: idx0)  — back-edge added after step_bb is lowered.
        let idx_phi = self.alloc_local();
        self.add_phi(
            header_bb,
            idx_phi,
            MirTy::Integer,
            vec![(pre_bb, Operand::Local(idx0))],
        );

        // cond = idx_phi < len
        let cond = self.alloc_local();
        self.emit(Instr::Assign {
            dest: cond,
            ty: MirTy::Boolean,
            rhs: Rvalue::Binary {
                op: fidan_ast::BinOp::Lt,
                lhs: Operand::Local(idx_phi),
                rhs: Operand::Local(len_local),
            },
        });
        self.set_terminator(Terminator::Branch {
            cond: Operand::Local(cond),
            then_bb: body_bb,
            else_bb: exit_bb,
        });

        // ── body_bb ───────────────────────────────────────────────────────────
        self.switch_to(body_bb);

        // elem = list_op[idx_phi]
        let elem = self.alloc_local();
        self.emit(Instr::GetIndex {
            dest: elem,
            object: list_op,
            index: Operand::Local(idx_phi),
        });
        // binding = elem  (so the element/key/value exprs can reference `binding`)
        let binding_local = self.alloc_local();
        self.emit(Instr::Assign {
            dest: binding_local,
            ty: MirTy::Dynamic,
            rhs: Rvalue::Use(Operand::Local(elem)),
        });
        self.define_var(binding, binding_local);

        if let Some(filter_expr) = filter {
            // Evaluate filter; only emit the body if truthy.
            let filter_val = self.lower_expr(filter_expr);
            let do_bb = self.alloc_block();
            let skip_bb = self.alloc_block(); // re-use step_bb is fine too
            self.set_terminator(Terminator::Branch {
                cond: filter_val,
                then_bb: do_bb,
                else_bb: skip_bb,
            });

            // do_bb: emit accumulation, then goto step_bb
            self.switch_to(do_bb);
            emit_body(self, Operand::Local(binding_local));
            if !self.terminated {
                self.goto(step_bb);
            }

            // skip_bb: goto step_bb
            self.switch_to(skip_bb);
            self.goto(step_bb);
        } else {
            emit_body(self, Operand::Local(binding_local));
            if !self.terminated {
                self.goto(step_bb);
            }
        }

        // ── step_bb ───────────────────────────────────────────────────────────
        self.switch_to(step_bb);
        let idx_next = self.alloc_local();
        self.emit(Instr::Assign {
            dest: idx_next,
            ty: MirTy::Integer,
            rhs: Rvalue::Binary {
                op: fidan_ast::BinOp::Add,
                lhs: Operand::Local(idx_phi),
                rhs: Operand::Const(MirLit::Int(1)),
            },
        });
        let step_end = self.cur_bb;
        self.goto(header_bb);

        // Back-patch the idx_phi in header_bb with the step back-edge.
        self.func_mut().block_mut(header_bb).phis[0]
            .operands
            .push((step_end, Operand::Local(idx_next)));

        // ── exit_bb ───────────────────────────────────────────────────────────
        self.switch_to(exit_bb);
    }

    // ── while-loop lowering ───────────────────────────────────────────────────

    fn lower_while_loop(&mut self, condition: &HirExpr, body: &[HirStmt]) {
        let pre_bb = self.cur_bb;
        let header_bb = self.alloc_block();
        let body_bb = self.alloc_block();
        let exit_bb = self.alloc_block();

        self.goto(header_bb);
        self.switch_to(header_bb);

        // ── Phi nodes for variables mutated in the loop body ──────────────────
        let env_before = self.env.clone();
        let written = collect_assigned_vars(body);
        let mut phi_vars: Vec<(Symbol, LocalId)> = Vec::new();
        for sym in &written {
            if let Some(&pre_local) = env_before.get(sym) {
                let phi_local = self.alloc_local();
                self.add_phi(
                    header_bb,
                    phi_local,
                    MirTy::Dynamic,
                    vec![(pre_bb, Operand::Local(pre_local))],
                );
                self.define_var(*sym, phi_local);
                phi_vars.push((*sym, phi_local));
            }
        }

        // Condition (reads phi locals so each iteration sees the updated value).
        let cond = self.lower_expr(condition);
        self.set_terminator(Terminator::Branch {
            cond,
            then_bb: body_bb,
            else_bb: exit_bb,
        });

        self.switch_to(body_bb);
        self.loop_stack.push((header_bb, exit_bb));
        self.lower_stmts(body);
        self.loop_stack.pop();

        let body_end = self.cur_bb;
        if !self.terminated {
            self.goto(header_bb);
        }

        // ── Back-patch phis ───────────────────────────────────────────────────
        for (i, (sym, _)) in phi_vars.iter().enumerate() {
            let body_local = self
                .env
                .get(sym)
                .copied()
                .unwrap_or_else(|| env_before[sym]);
            self.func_mut().block_mut(header_bb).phis[i]
                .operands
                .push((body_end, Operand::Local(body_local)));
        }

        // ── Exit ──────────────────────────────────────────────────────────────
        self.switch_to(exit_bb);
        for (sym, phi_local) in &phi_vars {
            self.define_var(*sym, *phi_local);
        }
    }

    // ── attempt / catch lowering ──────────────────────────────────────────────

    fn lower_attempt(
        &mut self,
        body: &[HirStmt],
        catches: &[HirCatchClause],
        otherwise: Option<&[HirStmt]>,
        finally: Option<&[HirStmt]>,
    ) {
        let catch_dispatch_bb = self.alloc_block();
        let otherwise_bb = self.alloc_block();
        let finally_bb = self.alloc_block();
        let join_bb = self.alloc_block();

        let env_before = self.env.clone();

        // Collect (end_bb, env_snapshot) for each path that exits normally
        // (no throw). These are used to build phi nodes at the join point.
        let mut join_arms: Vec<(BlockId, VarEnv)> = Vec::new();

        // ── Try body ──────────────────────────────────────────────────────────
        self.emit(Instr::PushCatch(catch_dispatch_bb));
        self.lower_stmts(body);
        if !self.terminated {
            self.emit(Instr::PopCatch);
        }
        if !self.terminated {
            self.goto(otherwise_bb);
        }

        // ── Catch dispatch ────────────────────────────────────────────────────
        // Reset env to before-state for catch dispatch (it's a separate path).
        self.env = env_before.clone();
        // Read + save the exception exactly once (CatchException consumes it).
        self.switch_to(catch_dispatch_bb);
        let err_save = self.alloc_local();
        self.emit(Instr::Assign {
            dest: err_save,
            ty: MirTy::Dynamic,
            rhs: Rvalue::CatchException,
        });

        // If `finally` is present, wrap the clause chain so that any re-throw
        // from a catch body still runs the finally block.
        let rethrow_bb: Option<BlockId> = if finally.is_some() {
            Some(self.alloc_block())
        } else {
            None
        };
        if let Some(rt_bb) = rethrow_bb {
            self.emit(Instr::PushCatch(rt_bb));
        }

        // Emit one block per clause; each typed clause has a dispatch branch.
        let n = catches.len();
        for (i, clause) in catches.iter().enumerate() {
            let clause_bb = self.alloc_block();
            let no_match_bb = self.alloc_block();

            let ty_tag = fidan_type_tag(&clause.ty);
            if let Some(tag) = ty_tag {
                // type_val = type(err_save);  matches = (type_val == tag)
                let type_sym = self.type_sym;
                let type_local = self.alloc_local();
                self.emit(Instr::Call {
                    dest: Some(type_local),
                    callee: Callee::Builtin(type_sym),
                    args: vec![Operand::Local(err_save)],
                    span: fidan_source::Span::default(),
                });
                let tag_local = self.alloc_local();
                self.emit(Instr::Assign {
                    dest: tag_local,
                    ty: MirTy::String,
                    rhs: Rvalue::Literal(MirLit::Str(tag.into())),
                });
                let matches = self.alloc_local();
                self.emit(Instr::Assign {
                    dest: matches,
                    ty: MirTy::Boolean,
                    rhs: Rvalue::Binary {
                        op: fidan_ast::BinOp::Eq,
                        lhs: Operand::Local(type_local),
                        rhs: Operand::Local(tag_local),
                    },
                });
                self.set_terminator(Terminator::Branch {
                    cond: Operand::Local(matches),
                    then_bb: clause_bb,
                    else_bb: no_match_bb,
                });
            } else {
                // Dynamic / untyped: unconditional catch-all.
                self.goto(clause_bb);
            }

            // ── Clause body ────────────────────────────────────────────────────
            self.env = env_before.clone(); // Each clause starts from the same env
            self.switch_to(clause_bb);
            if let Some(binding) = clause.binding {
                self.define_var(binding, err_save);
            }
            self.lower_stmts(&clause.body);
            // Normal exit from the clause: pop the rethrow guard, jump to finally.
            if rethrow_bb.is_some() && !self.terminated {
                self.emit(Instr::PopCatch);
            }
            if !self.terminated {
                join_arms.push((self.cur_bb, self.env.clone()));
                self.goto(finally_bb);
            }

            // Advance the "current block" to the next clause's entry.
            self.env = env_before.clone();
            self.switch_to(no_match_bb);

            // After the last clause, if nothing matched → rethrow original error.
            if i == n - 1 && !self.terminated {
                if rethrow_bb.is_some() {
                    self.emit(Instr::PopCatch);
                }
                self.set_terminator(Terminator::Throw {
                    value: Operand::Local(err_save),
                });
            }
        }

        // ── Re-throw landing pad: run finally, then propagate ─────────────────
        if let Some(rt_bb) = rethrow_bb {
            self.env = env_before.clone();
            self.switch_to(rt_bb);
            let reexc = self.alloc_local();
            self.emit(Instr::Assign {
                dest: reexc,
                ty: MirTy::Dynamic,
                rhs: Rvalue::CatchException,
            });
            if let Some(stmts) = finally {
                self.lower_stmts(stmts);
            }
            if !self.terminated {
                self.set_terminator(Terminator::Throw {
                    value: Operand::Local(reexc),
                });
            }
        }

        // ── Otherwise block (no exception) ────────────────────────────────────
        self.env = env_before.clone();
        self.switch_to(otherwise_bb);
        if let Some(stmts) = otherwise {
            self.lower_stmts(stmts);
        }
        if !self.terminated {
            join_arms.push((self.cur_bb, self.env.clone()));
            self.goto(finally_bb);
        }

        // ── Finally block ──────────────────────────────────────────────────────
        // The finally block runs for all normal exit paths.
        // We need phi nodes at finally_bb to merge vars from all arms.
        self.env = env_before.clone();
        self.switch_to(finally_bb);

        // Build phi nodes at finally_bb for all variables changed in any arm.
        // Only consider variables that existed before the attempt (in env_before).
        // Variables declared inside catch clauses are not merged.
        if !join_arms.is_empty() {
            let mut changed: Vec<Symbol> = Vec::new();
            for (_, env_arm) in &join_arms {
                for sym in env_diff(&env_before, env_arm) {
                    // Only merge variables that existed before the attempt.
                    if env_before.contains_key(&sym) && !changed.contains(&sym) {
                        changed.push(sym);
                    }
                }
            }
            for sym in changed {
                let operands: Vec<(BlockId, Operand)> = join_arms
                    .iter()
                    .map(|(end_bb, env_arm)| {
                        let local = env_arm
                            .get(&sym)
                            .or_else(|| env_before.get(&sym))
                            .copied()
                            .expect("variable should exist in either arm env or env_before");
                        (*end_bb, Operand::Local(local))
                    })
                    .collect();
                if operands.len() == 1 {
                    // Only one incoming arm — no phi needed, just use the value.
                    self.define_var(
                        sym,
                        match operands[0].1 {
                            Operand::Local(l) => l,
                            _ => env_before[&sym],
                        },
                    );
                } else {
                    let phi_local = self.alloc_local();
                    self.add_phi(finally_bb, phi_local, MirTy::Dynamic, operands);
                    self.define_var(sym, phi_local);
                }
            }
        }

        if let Some(stmts) = finally {
            self.lower_stmts(stmts);
        }
        if !self.terminated {
            self.goto(join_bb);
        }

        self.switch_to(join_bb);
    }
}

// ── Helper: map a FidanType to its runtime type-name string, for typed `catch` dispatch ─
// Returns None for `Dynamic` (= catch-all, no check needed).
fn fidan_type_tag(ty: &FidanType) -> Option<&'static str> {
    match ty {
        FidanType::String => Some("string"),
        FidanType::Integer => Some("integer"),
        FidanType::Float => Some("float"),
        FidanType::Boolean => Some("boolean"),
        FidanType::Object(_) => Some("object"),
        _ => None, // Dynamic and others → catch-all
    }
}

// ── Helper: collect all directly-assigned variable names in a stmt list ────────
//
// Used to compute loop phi-node candidates (Braun et al. two-pass approach).
fn collect_assigned_vars(stmts: &[HirStmt]) -> HashSet<Symbol> {
    let mut result = HashSet::new();
    collect_assigned_vars_impl(stmts, &mut result);
    result
}

fn collect_assigned_vars_impl(stmts: &[HirStmt], out: &mut HashSet<Symbol>) {
    for stmt in stmts {
        match stmt {
            HirStmt::Assign { target, .. } => {
                if let HirExprKind::Var(name) = &target.kind {
                    out.insert(*name);
                }
            }
            HirStmt::Destructure { bindings, .. } => {
                for &sym in bindings {
                    out.insert(sym);
                }
            }
            HirStmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_assigned_vars_impl(then_body, out);
                for ei in else_ifs {
                    collect_assigned_vars_impl(&ei.body, out);
                }
                if let Some(b) = else_body {
                    collect_assigned_vars_impl(b, out);
                }
            }
            HirStmt::For { body, .. }
            | HirStmt::While { body, .. }
            | HirStmt::ParallelFor { body, .. } => {
                collect_assigned_vars_impl(body, out);
            }
            HirStmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_assigned_vars_impl(body, out);
                for c in catches {
                    collect_assigned_vars_impl(&c.body, out);
                }
                if let Some(b) = otherwise {
                    collect_assigned_vars_impl(b, out);
                }
                if let Some(b) = finally {
                    collect_assigned_vars_impl(b, out);
                }
            }
            HirStmt::Check { arms, .. } => {
                for arm in arms {
                    collect_assigned_vars_impl(&arm.body, out);
                }
            }
            HirStmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_assigned_vars_impl(&task.body, out);
                }
            }
            _ => {}
        }
    }
}

// ── Top-level lowering ────────────────────────────────────────────────────────

/// Lower an entire `HirModule` into a `MirProgram`.
///
/// Functions are numbered sequentially.  The first function (`FunctionId(0)`)
/// is always the top-level initialisation function (globals + init_stmts).
///
/// `existing_globals` is the persistent GID registry maintained by the REPL
/// (`MirReplState::persistent_global_names`).  The lowerer pre-populates
/// `global_map` from this slice so every symbol always gets the same `GlobalId`
/// across recompilations.  Pass `&[]` for non-REPL (file / batch) compilation.
pub fn lower_program(
    hir: &HirModule,
    interner: &SymbolInterner,
    existing_globals: &[String],
) -> MirProgram {
    let mut prog = MirProgram::new();

    // `new` is the constructor method name; `this` is the implicit first param.
    let new_sym = interner.intern("new");
    let len_sym = interner.intern("len");
    let append_sym = interner.intern("append");
    let type_sym = interner.intern("type");
    let wildcard_sym = interner.intern("_");
    let this_name = interner.intern("this");

    // ── Pre-pass ①: sentinel top-level init fn ───────────────────────────────
    let init_sym = Symbol(0);
    prog.functions
        .push(MirFunction::new(FunctionId(0), init_sym, MirTy::Nothing));

    // ── Pre-pass ②: allocate FunctionIds for top-level / extension functions ─
    let mut fn_map: HashMap<Symbol, FunctionId> = HashMap::new();
    // fn_name → class it extends (extension actions only)
    let mut ext_fn_map: HashMap<Symbol, Symbol> = HashMap::new();

    for func in &hir.functions {
        let id = FunctionId(prog.functions.len() as u32);
        fn_map.insert(func.name, id);
        prog.functions.push(MirFunction::new(
            id,
            func.name,
            fidan_ty_to_mir(&func.return_ty),
        ));
        prog.functions.last_mut().unwrap().precompile = func.precompile;
        if let Some(cls) = func.extends {
            ext_fn_map.insert(func.name, cls);
        }
    }

    let fn_is_extension: HashSet<Symbol> = ext_fn_map.keys().copied().collect();

    // ── Pre-pass ③: object methods + per-class metadata ─────────────────────
    let mut obj_map: HashMap<Symbol, FunctionId> = HashMap::new();
    let mut method_ids: HashMap<(Symbol, Symbol), FunctionId> = HashMap::new();
    let mut parent_map: HashMap<Symbol, Symbol> = HashMap::new();

    for obj in &hir.objects {
        if let Some(p) = obj.parent {
            parent_map.insert(obj.name, p);
        }
        let mut obj_info = MirObjectInfo {
            name: obj.name,
            parent: obj.parent,
            field_names: obj.fields.iter().map(|f| f.name).collect(),
            methods: HashMap::new(),
            init_fn: None,
        };

        // Own methods — each gets a new FunctionId.
        for method in &obj.methods {
            let id = FunctionId(prog.functions.len() as u32);
            method_ids.insert((obj.name, method.name), id);
            prog.functions.push(MirFunction::new(
                id,
                method.name,
                fidan_ty_to_mir(&method.return_ty),
            ));
            prog.functions.last_mut().unwrap().precompile = method.precompile;
            obj_info.methods.insert(method.name, id);
            if method.name == new_sym {
                obj_info.init_fn = Some(id);
                obj_map.insert(obj.name, id);
            }
        }

        // Extension actions that target this class — reuse their fn_map FunctionId.
        for (&fn_name, &ext_cls) in &ext_fn_map {
            if ext_cls == obj.name {
                if let Some(&fid) = fn_map.get(&fn_name) {
                    method_ids.insert((obj.name, fn_name), fid);
                    obj_info.methods.insert(fn_name, fid);
                }
            }
        }

        prog.objects.push(obj_info);
    }

    // Shared queue for deferred parallel-for body functions.
    let pending_par_fors: Rc<RefCell<VecDeque<PendingParallelFor>>> =
        Rc::new(RefCell::new(VecDeque::new()));

    // ── Pre-pass ④: register module-level globals ─────────────────────────────────
    // Stable GID assignment: pre-populate from the persistent REPL registry
    // first so every symbol retains its GID across recompilations.
    // For non-REPL compilation `existing_globals` is empty and this is a no-op.
    let mut global_map: HashMap<Symbol, GlobalId> = HashMap::new();
    for (i, name) in existing_globals.iter().enumerate() {
        let sym = interner.intern(name.as_str());
        let gid = GlobalId(i as u32);
        prog.globals.push(MirGlobal {
            name: sym,
            ty: MirTy::Dynamic,
        });
        global_map.insert(sym, gid);
    }

    // Register `use std.MODULE` / `use usermod` namespace aliases and specific-name
    // stdlib imports as module-level globals.  Skip any already registered above.
    // Count ALL namespace globals (existing + new) so the REPL can compute the
    // boundary between the namespace init section and the body in bb0.
    let mut namespace_global_count: usize = 0;
    for decl in &hir.use_decls {
        if decl.module_path.len() >= 2 && decl.specific_names.is_none() {
            let module = &decl.module_path[1];
            let ns_alias = decl.alias.clone().unwrap_or_else(|| module.clone());
            let alias_sym = interner.intern(&ns_alias);
            namespace_global_count += 1;
            if !global_map.contains_key(&alias_sym) {
                let gid = GlobalId(prog.globals.len() as u32);
                prog.globals.push(MirGlobal {
                    name: alias_sym,
                    ty: MirTy::Dynamic,
                });
                global_map.insert(alias_sym, gid);
            }
        } else if decl.module_path.len() == 1 && decl.specific_names.is_none() {
            // User module: `use test2` -> module_path = ["test2"].
            let ns_alias = decl
                .alias
                .clone()
                .unwrap_or_else(|| decl.module_path[0].clone());
            let alias_sym = interner.intern(&ns_alias);
            namespace_global_count += 1;
            if !global_map.contains_key(&alias_sym) {
                let gid = GlobalId(prog.globals.len() as u32);
                prog.globals.push(MirGlobal {
                    name: alias_sym,
                    ty: MirTy::Dynamic,
                });
                global_map.insert(alias_sym, gid);
            }
        } else if let Some(ref names) = decl.specific_names {
            // Specific-name stdlib import: `use std.io.{readFile}` -> each name is a global.
            if decl.module_path.len() >= 2 {
                for fn_name in names {
                    let fn_sym = interner.intern(fn_name);
                    namespace_global_count += 1;
                    if !global_map.contains_key(&fn_sym) {
                        let gid = GlobalId(prog.globals.len() as u32);
                        prog.globals.push(MirGlobal {
                            name: fn_sym,
                            ty: MirTy::Dynamic,
                        });
                        global_map.insert(fn_sym, gid);
                    }
                }
            }
        }
    }
    prog.namespace_global_count = namespace_global_count;

    // Module-level `var` declarations -- registered after namespace globals.
    // With stable GIDs, symbols already in the registry are skipped.
    for stmt in &hir.init_stmts {
        if let HirStmt::VarDecl { name, ty, .. } = stmt {
            if !global_map.contains_key(name) {
                let gid = GlobalId(prog.globals.len() as u32);
                prog.globals.push(MirGlobal {
                    name: *name,
                    ty: fidan_ty_to_mir(ty),
                });
                global_map.insert(*name, gid);
            }
        }
    }
    // Build FunctionId → param-name-order map for sorting named call args.
    let mut fn_param_names: HashMap<FunctionId, Vec<Symbol>> = HashMap::new();
    for func in &hir.functions {
        if let Some(&fid) = fn_map.get(&func.name) {
            fn_param_names.insert(fid, func.params.iter().map(|p| p.name).collect());
        }
    }
    for obj in &hir.objects {
        for method in &obj.methods {
            if let Some(&fid) = method_ids.get(&(obj.name, method.name)) {
                // Explicit params only — `this` is always prepended at call sites separately.
                fn_param_names.insert(fid, method.params.iter().map(|p| p.name).collect());
            }
        }
    }

    // ── Closure: lower one HirFunction body into an already-allocated fn ─────
    // `pending_par_fors` is captured by clone (Rc is cheap to clone).
    let lower_hir_fn = |prog: &mut MirProgram,
                        fn_map: &HashMap<Symbol, FunctionId>,
                        obj_map: &HashMap<Symbol, FunctionId>,
                        parent_map: &HashMap<Symbol, Symbol>,
                        method_ids: &HashMap<(Symbol, Symbol), FunctionId>,
                        fn_is_extension: &HashSet<Symbol>,
                        new_sym: Symbol,
                        len_sym: Symbol,
                        append_sym: Symbol,
                        type_sym: Symbol,
                        wildcard_sym: Symbol,
                        global_map: &HashMap<Symbol, GlobalId>,
                        func: &HirFunction,
                        fn_id: FunctionId,
                        owner_class: Option<Symbol>,
                        pending: Rc<RefCell<VecDeque<PendingParallelFor>>>| {
        let entry_bb = prog.function_mut(fn_id).alloc_block();
        let mut ctx = FnCtx {
            prog,
            fn_id,
            cur_bb: entry_bb,
            env: VarEnv::new(),
            global_map: global_map.clone(),
            fn_map: fn_map.clone(),
            obj_map: obj_map.clone(),
            terminated: false,
            this_reg: None,
            owner_class,
            parent_map: parent_map.clone(),
            method_ids: method_ids.clone(),
            new_sym,
            len_sym,
            append_sym,
            type_sym,
            fn_is_extension: fn_is_extension.clone(),
            loop_stack: vec![],
            continue_sites: HashMap::new(),
            wildcard_sym,
            par_for_pending: pending,
            is_init_fn: false,
            fn_param_names: fn_param_names.clone(),
        };
        if owner_class.is_some() {
            let this_local = ctx.alloc_local();
            ctx.this_reg = Some(this_local);
            ctx.func_mut().params.push(MirParam {
                local: this_local,
                name: this_name,
                ty: MirTy::Dynamic,
                certain: false,
            });
        }

        // Explicit parameters.
        for param in &func.params {
            let local = ctx.alloc_local();
            ctx.define_var(param.name, local);
            ctx.func_mut().params.push(MirParam {
                local,
                name: param.name,
                ty: fidan_ty_to_mir(&param.ty),
                certain: param.certain,
            });
            // Emit a certain-param null guard as a real MIR instruction so it
            // survives inlining without any special-casing in the inliner.
            if param.certain {
                ctx.emit(Instr::CertainCheck {
                    operand: Operand::Local(local),
                    name: param.name,
                });
            }
        }

        ctx.lower_stmts(&func.body);
        if !ctx.terminated {
            ctx.set_terminator(Terminator::Return(None));
        }
    };

    // ── Lower top-level functions ─────────────────────────────────────────────
    for func in &hir.functions {
        let fn_id = fn_map[&func.name];
        let owner_class = ext_fn_map.get(&func.name).copied();
        lower_hir_fn(
            &mut prog,
            &fn_map,
            &obj_map,
            &parent_map,
            &method_ids,
            &fn_is_extension,
            new_sym,
            len_sym,
            append_sym,
            type_sym,
            wildcard_sym,
            &global_map,
            func,
            fn_id,
            owner_class,
            Rc::clone(&pending_par_fors),
        );
    }
    // ── Lower object methods ──────────────────────────────────────────────────
    for obj in &hir.objects {
        for method in &obj.methods {
            let fn_id = method_ids[&(obj.name, method.name)];
            lower_hir_fn(
                &mut prog,
                &fn_map,
                &obj_map,
                &parent_map,
                &method_ids,
                &fn_is_extension,
                new_sym,
                len_sym,
                append_sym,
                type_sym,
                wildcard_sym,
                &global_map,
                method,
                fn_id,
                Some(obj.name),
                Rc::clone(&pending_par_fors),
            );
        }
    }

    // ── Top-level initialisation function (FunctionId(0)) ────────────────────
    {
        let fn_id = FunctionId(0);
        let entry_bb = prog.function_mut(fn_id).alloc_block();
        let mut ctx = FnCtx {
            prog: &mut prog,
            fn_id,
            cur_bb: entry_bb,
            env: VarEnv::new(),
            global_map: global_map.clone(),
            fn_map: fn_map.clone(),
            obj_map: obj_map.clone(),
            terminated: false,
            this_reg: None,
            owner_class: None,
            parent_map: parent_map.clone(),
            method_ids: method_ids.clone(),
            new_sym,
            len_sym,
            append_sym,
            type_sym,
            fn_is_extension: fn_is_extension.clone(),
            loop_stack: vec![],
            continue_sites: HashMap::new(),
            wildcard_sym,
            par_for_pending: Rc::clone(&pending_par_fors),
            is_init_fn: true,
            fn_param_names: fn_param_names.clone(),
        };

        // ── Emit namespace variable bindings for `use std.MODULE` / `use usermod` ──
        // Initialise each namespace global in the init fn by storing a
        // `MirLit::Namespace` value into the pre-registered GlobalId slot.
        // Named action bodies can then read the namespace via `LoadGlobal`
        // without depending on the init fn's SSA scope.
        for decl in &hir.use_decls {
            if decl.module_path.len() >= 2 && decl.specific_names.is_none() {
                let module = decl.module_path[1].clone();
                let ns_alias = decl.alias.clone().unwrap_or_else(|| module.clone());
                let alias_sym = interner.intern(&ns_alias);
                let dest = ctx.alloc_local();
                ctx.emit(Instr::Assign {
                    dest,
                    ty: MirTy::Dynamic,
                    rhs: Rvalue::Literal(MirLit::Namespace(module)),
                });
                // Store as a global so all functions can read it via LoadGlobal.
                if let Some(&gid) = ctx.global_map.get(&alias_sym) {
                    ctx.emit(Instr::StoreGlobal {
                        global: gid,
                        value: Operand::Local(dest),
                    });
                } else {
                    // Grouped-import or other edge case — fall back to SSA local.
                    ctx.define_var(alias_sym, dest);
                }
            } else if decl.module_path.len() == 1 && decl.specific_names.is_none() {
                // User module: `use test2` → module_path = ["test2"].
                let module = decl.module_path[0].clone();
                let ns_alias = decl.alias.clone().unwrap_or_else(|| module.clone());
                let alias_sym = interner.intern(&ns_alias);
                let dest = ctx.alloc_local();
                ctx.emit(Instr::Assign {
                    dest,
                    ty: MirTy::Dynamic,
                    rhs: Rvalue::Literal(MirLit::Namespace(module)),
                });
                if let Some(&gid) = ctx.global_map.get(&alias_sym) {
                    ctx.emit(Instr::StoreGlobal {
                        global: gid,
                        value: Operand::Local(dest),
                    });
                } else {
                    ctx.define_var(alias_sym, dest);
                }
            } else if let Some(ref names) = decl.specific_names {
                // Specific-name stdlib imports: `use std.io.{readFile, print}` →
                // each name becomes a `StdlibFn` value stored into a global.
                if decl.module_path.len() >= 2 {
                    let module = decl.module_path[1].clone();
                    for fn_name in names {
                        let fn_sym = interner.intern(fn_name);
                        let dest = ctx.alloc_local();
                        ctx.emit(Instr::Assign {
                            dest,
                            ty: MirTy::Dynamic,
                            rhs: Rvalue::Literal(MirLit::StdlibFn {
                                module: module.clone(),
                                name: fn_name.clone(),
                            }),
                        });
                        if let Some(&gid) = ctx.global_map.get(&fn_sym) {
                            ctx.emit(Instr::StoreGlobal {
                                global: gid,
                                value: Operand::Local(dest),
                            });
                        } else {
                            ctx.define_var(fn_sym, dest);
                        }
                    }
                }
            }
        }

        ctx.lower_stmts(&hir.init_stmts);
        if !ctx.terminated {
            ctx.set_terminator(Terminator::Return(None));
        }
    }

    // ── Lower pending parallel-for body functions ─────────────────────────────
    // New entries can appear during this loop (nested parallel-for), so we
    // keep processing until the queue is fully drained.
    loop {
        let Some(pf) = pending_par_fors.borrow_mut().pop_front() else {
            break;
        };
        // SAFETY: raw ptrs point into HirStmt slices owned by `hir`, which
        // lives for the entire duration of lower_program.
        let body: &[HirStmt] = unsafe { std::slice::from_raw_parts(pf.body_ptr, pf.body_len) };
        let entry_bb = prog.function_mut(pf.fn_id).alloc_block();
        let mut ctx = FnCtx {
            prog: &mut prog,
            fn_id: pf.fn_id,
            cur_bb: entry_bb,
            env: VarEnv::new(),
            global_map: global_map.clone(),
            fn_map: fn_map.clone(),
            obj_map: obj_map.clone(),
            terminated: false,
            this_reg: None,
            owner_class: None,
            parent_map: parent_map.clone(),
            method_ids: method_ids.clone(),
            new_sym,
            len_sym,
            append_sym,
            type_sym,
            fn_is_extension: fn_is_extension.clone(),
            loop_stack: vec![],
            continue_sites: HashMap::new(),
            wildcard_sym,
            par_for_pending: Rc::clone(&pending_par_fors),
            is_init_fn: false,
            fn_param_names: fn_param_names.clone(),
        };
        // First param (parallel for only): the per-iteration loop binding.
        if let Some((binding_sym, binding_ty)) = pf.binding {
            let binding_local = ctx.alloc_local();
            ctx.define_var(binding_sym, binding_local);
            ctx.func_mut().params.push(MirParam {
                local: binding_local,
                name: binding_sym,
                ty: binding_ty,
                certain: false,
            });
        }
        // Subsequent params: captured env variables (parallel for + concurrent tasks).
        for (sym, ty) in pf.env_params {
            let local = ctx.alloc_local();
            ctx.define_var(sym, local);
            ctx.func_mut().params.push(MirParam {
                local,
                name: sym,
                ty,
                certain: false,
            });
        }
        ctx.lower_stmts(body);
        if !ctx.terminated {
            ctx.set_terminator(Terminator::Return(None));
        }
    }

    // ── Lower test blocks ─────────────────────────────────────────────────────
    // Each `test "name" { body }` becomes an anonymous parameterless function.
    // The (name, fn_id) pair is recorded in `prog.test_functions` so the CLI
    // test runner can call them one-by-one and report pass/fail per test.
    for test_decl in &hir.tests {
        let fn_sym = interner.intern(&format!("__test__:{}", test_decl.name));
        let test_fn = MirFunction::new(
            FunctionId(prog.functions.len() as u32),
            fn_sym,
            MirTy::Nothing,
        );
        let test_fn_id = prog.add_function(test_fn);
        fn_param_names.insert(test_fn_id, vec![]);

        let entry_bb = prog.function_mut(test_fn_id).alloc_block();
        let mut ctx = FnCtx {
            prog: &mut prog,
            fn_id: test_fn_id,
            cur_bb: entry_bb,
            env: VarEnv::new(),
            global_map: global_map.clone(),
            fn_map: fn_map.clone(),
            obj_map: obj_map.clone(),
            terminated: false,
            this_reg: None,
            owner_class: None,
            parent_map: parent_map.clone(),
            method_ids: method_ids.clone(),
            new_sym,
            len_sym,
            append_sym,
            type_sym,
            fn_is_extension: fn_is_extension.clone(),
            loop_stack: vec![],
            continue_sites: HashMap::new(),
            wildcard_sym,
            par_for_pending: Rc::clone(&pending_par_fors),
            is_init_fn: false,
            fn_param_names: fn_param_names.clone(),
        };
        ctx.lower_stmts(&test_decl.body);
        if !ctx.terminated {
            ctx.set_terminator(Terminator::Return(None));
        }

        prog.test_functions
            .push((test_decl.name.clone(), test_fn_id));
    }

    // ── Propagate use_decls from HIR ──────────────────────────────────────────
    for decl in &hir.use_decls {
        if decl.module_path.len() >= 2 {
            // Stdlib use_decl: `use std.io` / `use std.io.{fn}`.
            let module = decl.module_path[1].clone();
            let alias = decl.alias.clone().unwrap_or_else(|| module.clone());
            prog.use_decls.push(MirUseDecl {
                module,
                alias,
                specific_names: decl.specific_names.clone(),
                re_export: decl.re_export,
                is_stdlib: true,
            });
        } else if decl.module_path.len() == 1 && decl.specific_names.is_none() && decl.re_export {
            // User-module re-export: `export use mymod`.
            // Stored so re-export chaining (`lib.mymod.fn()`) resolves in `get_field`.
            // `is_stdlib: false` prevents the alias from entering `stdlib_modules`,
            // keeping dispatch routed through `user_fn_map`.
            let module = decl.module_path[0].clone();
            let alias = decl.alias.clone().unwrap_or_else(|| module.clone());
            prog.use_decls.push(MirUseDecl {
                module,
                alias,
                specific_names: None,
                re_export: true,
                is_stdlib: false,
            });
        }
    }

    prog
}
