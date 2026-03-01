// fidan-mir/src/lower.rs
//
// HIR → MIR lowering.
//
// Translates the HIR tree into SSA-form MIR using a scope-based renaming
// scheme.  Braun et al.'s "Simple and Efficient Construction of SSA Form"
// algorithm is approximated here: for linear code and if/else we get exact
// SSA.  For loops we use a two-pass approach (placeholder φ-nodes that are
// back-patched after the body is lowered).

use std::collections::HashMap;

use fidan_ast::BinOp;
use fidan_hir::{
    HirCatchClause, HirCheckArm, HirCheckExprArm, HirElseIf, HirExpr, HirExprKind, HirFunction,
    HirInterpPart, HirModule, HirStmt,
};
use fidan_lexer::Symbol;
use fidan_typeck::FidanType;

use crate::mir::{
    BlockId, Callee, FunctionId, Instr, LocalId, MirFunction, MirLit, MirParam,
    MirProgram, MirStringPart, MirTy, Operand, PhiNode, Rvalue, Terminator,
};

// ── Type conversion ────────────────────────────────────────────────────────────

fn fidan_ty_to_mir(ty: &FidanType) -> MirTy {
    match ty {
        FidanType::Integer  => MirTy::Integer,
        FidanType::Float    => MirTy::Float,
        FidanType::Boolean  => MirTy::Boolean,
        FidanType::String   => MirTy::String,
        FidanType::Nothing  => MirTy::Nothing,
        FidanType::Dynamic  => MirTy::Dynamic,
        FidanType::List(e)  => MirTy::List(Box::new(fidan_ty_to_mir(e))),
        FidanType::Dict(k, v) => MirTy::Dict(
            Box::new(fidan_ty_to_mir(k)),
            Box::new(fidan_ty_to_mir(v)),
        ),
        FidanType::Tuple(elems) => MirTy::Tuple(elems.iter().map(fidan_ty_to_mir).collect()),
        FidanType::Object(s)  => MirTy::Object(*s),
        FidanType::Shared(t)  => MirTy::Shared(Box::new(fidan_ty_to_mir(t))),
        FidanType::Pending(t) => MirTy::Pending(Box::new(fidan_ty_to_mir(t))),
        FidanType::Function   => MirTy::Function,
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
    prog:       &'p mut MirProgram,
    /// The function we're currently building.
    fn_id:      FunctionId,
    /// The block we're currently appending instructions to.
    cur_bb:     BlockId,
    /// Current variable→local mapping (the "SSA current-def" table).
    env:        VarEnv,
    /// Maps function name → `FunctionId` (populated by pre-pass).
    fn_map:     HashMap<Symbol, FunctionId>,
    /// Whether the current block has been terminated (Return / Goto etc.).
    terminated: bool,
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
        if self.terminated { return; }
        let bb = self.cur_bb;
        self.func_mut().block_mut(bb).instructions.push(instr);
    }

    fn set_terminator(&mut self, term: Terminator) {
        if self.terminated { return; }
        let bb = self.cur_bb;
        self.func_mut().block_mut(bb).terminator = term;
        self.terminated = true;
    }

    fn goto(&mut self, target: BlockId) {
        self.set_terminator(Terminator::Goto(target));
    }

    fn switch_to(&mut self, bb: BlockId) {
        self.cur_bb    = bb;
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
        join_bb:   BlockId,
        result:    LocalId,
        ty:        MirTy,
        operands:  Vec<(BlockId, Operand)>,
    ) {
        self.func_mut().block_mut(join_bb).phis.push(PhiNode { result, ty, operands });
    }

    // ── Expression lowering ──────────────────────────────────────────────────

    fn lower_expr(&mut self, expr: &HirExpr) -> Operand {
        let ty = fidan_ty_to_mir(&expr.ty);

        match &expr.kind {
            // ── Literals ──────────────────────────────────────────────────────
            HirExprKind::IntLit(v)  => Operand::Const(MirLit::Int(*v)),
            HirExprKind::FloatLit(v) => Operand::Const(MirLit::Float(*v)),
            HirExprKind::StrLit(s)  => Operand::Const(MirLit::Str(s.clone())),
            HirExprKind::BoolLit(b) => Operand::Const(MirLit::Bool(*b)),
            HirExprKind::Nothing    => Operand::Const(MirLit::Nothing),

            // ── Variables ─────────────────────────────────────────────────────
            HirExprKind::Var(name) => {
                if let Some(local) = self.lookup_var(*name) {
                    Operand::Local(local)
                } else {
                    // Unknown/builtin — represent as Nothing for now.
                    Operand::Const(MirLit::Nothing)
                }
            }
            HirExprKind::This   => Operand::Const(MirLit::Nothing), // TODO: `this` binding
            HirExprKind::Parent => Operand::Const(MirLit::Nothing), // TODO: `parent` binding

            // ── Binary / Unary ────────────────────────────────────────────────
            HirExprKind::Binary { op, lhs, rhs } => {
                // Sentinel: BinOp::Eq used for assignment-as-expression in HIR.
                // Lower as a no-op here; assignments are handled in statements.
                if *op == BinOp::Eq {
                    return self.lower_expr(rhs);
                }
                let l = self.lower_expr(lhs);
                let r = self.lower_expr(rhs);
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty: ty.clone(),
                    rhs: Rvalue::Binary { op: *op, lhs: l, rhs: r },
                });
                Operand::Local(dest)
            }
            HirExprKind::Unary { op, operand } => {
                let inner = self.lower_expr(operand);
                let dest = self.alloc_local();
                self.emit(Instr::Assign {
                    dest,
                    ty: ty.clone(),
                    rhs: Rvalue::Unary { op: *op, operand: inner },
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
            HirExprKind::IfExpr { condition, then_val, else_val } => {
                let cond   = self.lower_expr(condition);
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
                if !self.terminated { self.goto(join_bb); }

                // else branch
                self.switch_to(else_bb);
                let else_op = self.lower_expr(else_val);
                let else_end = self.cur_bb;
                if !self.terminated { self.goto(join_bb); }

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
                let callee_op = match &callee.kind {
                    HirExprKind::Field { object, field } => {
                        let recv = self.lower_expr(object);
                        Callee::Method { receiver: recv, method: *field }
                    }
                    HirExprKind::Var(name) => {
                        if let Some(fid) = self.fn_map.get(name).copied() {
                            Callee::Fn(fid)
                        } else {
                            // Builtin or unknown — use dynamic dispatch.
                            let op = self.lower_expr(callee);
                            Callee::Dynamic(op)
                        }
                    }
                    _ => {
                        let op = self.lower_expr(callee);
                        Callee::Dynamic(op)
                    }
                };
                let arg_ops: Vec<Operand> = args.iter().map(|a| self.lower_expr(&a.value)).collect();
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
                self.emit(Instr::GetField { dest, object: recv, field: *field });
                Operand::Local(dest)
            }

            HirExprKind::Index { object, index } => {
                let obj = self.lower_expr(object);
                let idx = self.lower_expr(index);
                let dest = self.alloc_local();
                self.emit(Instr::GetIndex { dest, object: obj, index: idx });
                Operand::Local(dest)
            }

            // ── Collections ───────────────────────────────────────────────────
            HirExprKind::List(elems) => {
                let ops: Vec<Operand> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign { dest, ty, rhs: Rvalue::List(ops) });
                Operand::Local(dest)
            }
            HirExprKind::Dict(entries) => {
                let pairs: Vec<(Operand, Operand)> = entries
                    .iter()
                    .map(|(k, v)| (self.lower_expr(k), self.lower_expr(v)))
                    .collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign { dest, ty, rhs: Rvalue::Dict(pairs) });
                Operand::Local(dest)
            }
            HirExprKind::Tuple(elems) => {
                let ops: Vec<Operand> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let dest = self.alloc_local();
                self.emit(Instr::Assign { dest, ty, rhs: Rvalue::Tuple(ops) });
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
                // For now: treat `spawn expr` as a regular call (Phase 5.5 will add real async)
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
            let arm_bb  = self.alloc_block();
            let next_bb = self.alloc_block();

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

            // Arm body (stmts) — produce a value operand from the last expr stmt
            self.switch_to(arm_bb);
            self.lower_stmts(&arm.body);
            let arm_val = Operand::Const(MirLit::Nothing); // placeholder for now
            let arm_end = self.cur_bb;
            if !self.terminated { self.goto(join_bb); }
            phi_ops.push((arm_end, arm_val));

            self.switch_to(next_bb);
        }

        // Fallthrough (no match) → join
        if !self.terminated { self.goto(join_bb); }
        phi_ops.push((self.cur_bb, Operand::Const(MirLit::Nothing)));

        self.switch_to(join_bb);
        self.add_phi(join_bb, result_local, ty, phi_ops);
        Operand::Local(result_local)
    }

    // ── Statement lowering ────────────────────────────────────────────────────

    fn lower_stmts(&mut self, stmts: &[HirStmt]) {
        for stmt in stmts {
            if self.terminated { break; }
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
                self.define_var(*name, dest);
            }

            HirStmt::Destructure { bindings, binding_tys, value, .. } => {
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
                        self.define_var(*name, dest);
                    }
                    HirExprKind::Field { object, field } => {
                        let recv = self.lower_expr(object);
                        self.emit(Instr::SetField { object: recv, field: *field, value: val });
                    }
                    HirExprKind::Index { object, index } => {
                        let obj = self.lower_expr(object);
                        let idx = self.lower_expr(index);
                        self.emit(Instr::SetIndex { object: obj, index: idx, value: val });
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
                        let callee_op = match &callee.kind {
                            HirExprKind::Field { object, field } => {
                                let recv = self.lower_expr(object);
                                Callee::Method { receiver: recv, method: *field }
                            }
                            HirExprKind::Var(name) => {
                                if let Some(fid) = self.fn_map.get(name).copied() {
                                    Callee::Fn(fid)
                                } else {
                                    let op = self.lower_expr(callee);
                                    Callee::Dynamic(op)
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

            HirStmt::Break    { .. } => { self.set_terminator(Terminator::Unreachable); }
            HirStmt::Continue { .. } => { self.set_terminator(Terminator::Unreachable); }

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
            HirStmt::Check { scrutinee, arms, .. } => {
                self.lower_check_stmt(scrutinee, arms);
            }

            // ── For loop ─────────────────────────────────────────────────────────
            HirStmt::For { binding, binding_ty, iterable, body, .. } => {
                self.lower_for_loop(*binding, binding_ty, iterable, body);
            }

            // ── While loop ───────────────────────────────────────────────────────
            HirStmt::While { condition, body, .. } => {
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
            // Phase 5.5: emit linearised for-loop (real parallel lowering later).
            HirStmt::ParallelFor { binding, binding_ty, iterable, body, .. } => {
                self.lower_for_loop(*binding, binding_ty, iterable, body);
            }

            // ── Concurrent block ─────────────────────────────────────────────
            // Phase 5.5: emit sequential lowering (real concurrent lowering later).
            HirStmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    self.lower_stmts(&task.body);
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
        else_ifs:  &[HirElseIf],
        else_body: Option<&[HirStmt]>,
    ) {
        let cond = self.lower_expr(condition);

        let then_bb = self.alloc_block();
        let else_bb = self.alloc_block();
        let join_bb = self.alloc_block();

        self.set_terminator(Terminator::Branch { cond, then_bb, else_bb });

        // ── then branch ───────────────────────────────────────────────────────
        let env_before = self.env.clone();
        self.switch_to(then_bb);
        self.lower_stmts(then_body);
        let env_after_then = self.env.clone();
        let then_end = self.cur_bb;
        if !self.terminated { self.goto(join_bb); }

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
        if !self.terminated { self.goto(join_bb); }

        // ── join: φ-nodes for variables changed in either branch ──────────────
        self.switch_to(join_bb);
        self.env = env_before.clone();

        let changed_then = env_diff(&env_before, &env_after_then);
        let changed_else = env_diff(&env_before, &env_after_else);
        let mut changed: Vec<Symbol> = changed_then;
        for s in changed_else {
            if !changed.contains(&s) { changed.push(s); }
        }

        for sym in changed {
            let then_op = env_after_then
                .get(&sym)
                .map(|&l| Operand::Local(l))
                .unwrap_or_else(|| {
                    env_before.get(&sym).map(|&l| Operand::Local(l))
                        .unwrap_or(Operand::Const(MirLit::Nothing))
                });
            let else_op = env_after_else
                .get(&sym)
                .map(|&l| Operand::Local(l))
                .unwrap_or_else(|| {
                    env_before.get(&sym).map(|&l| Operand::Local(l))
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

        for arm in arms {
            let arm_bb  = self.alloc_block();
            let next_bb = self.alloc_block();

            let pat     = self.lower_expr(&arm.pattern);
            let cmp     = self.alloc_local();
            self.emit(Instr::Assign {
                dest: cmp,
                ty: MirTy::Boolean,
                rhs: Rvalue::Binary { op: BinOp::Eq, lhs: scrut.clone(), rhs: pat },
            });
            self.set_terminator(Terminator::Branch {
                cond: Operand::Local(cmp),
                then_bb: arm_bb,
                else_bb: next_bb,
            });

            self.switch_to(arm_bb);
            self.lower_stmts(&arm.body);
            if !self.terminated { self.goto(join_bb); }

            self.switch_to(next_bb);
        }

        if !self.terminated { self.goto(join_bb); }
        self.switch_to(join_bb);
    }

    // ── for-loop lowering ─────────────────────────────────────────────────────

    fn lower_for_loop(
        &mut self,
        binding:    Symbol,
        binding_ty: &FidanType,
        iterable:   &HirExpr,
        body:       &[HirStmt],
    ) {
        // Emit: iter_list = lower(iterable); idx = 0; len = len(iter_list)
        let list_op = self.lower_expr(iterable);

        // idx = 0
        let idx0 = self.alloc_local();
        self.emit(Instr::Assign {
            dest: idx0,
            ty: MirTy::Integer,
            rhs: Rvalue::Literal(MirLit::Int(0)),
        });

        // len = list.len (represented as a method call placeholder; MIR walker fills it)
        let len_local = self.alloc_local();
        self.emit(Instr::Call {
            dest: Some(len_local),
            callee: Callee::Method {
                receiver: list_op.clone(),
                method: _builtin_sym_len(),
            },
            args: vec![],
            span: fidan_source::Span::default(),
        });

        let pre_bb    = self.cur_bb;
        let header_bb = self.alloc_block();
        let body_bb   = self.alloc_block();
        let exit_bb   = self.alloc_block();

        self.goto(header_bb);

        // ── Loop header ───────────────────────────────────────────────────────
        self.switch_to(header_bb);

        // φ for idx: φ(idx0 from pre, idx_next from body_end)
        let idx_phi = self.alloc_local();
        // Placeholder: operands to be back-patched after body is lowered.
        self.add_phi(
            header_bb,
            idx_phi,
            MirTy::Integer,
            vec![(pre_bb, Operand::Local(idx0))], // back-patch body side later
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

        // ── Loop body ──────────────────────────────────────────────────────────
        self.switch_to(body_bb);

        // binding = list[idx]
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

        self.lower_stmts(body);

        // idx_next = idx + 1
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

        let body_end = self.cur_bb;
        if !self.terminated { self.goto(header_bb); }

        // Back-patch the idx φ-node with the body-end operand.
        self.func_mut()
            .block_mut(header_bb)
            .phis[0]
            .operands
            .push((body_end, Operand::Local(idx_next)));

        self.switch_to(exit_bb);
    }

    // ── while-loop lowering ───────────────────────────────────────────────────

    fn lower_while_loop(&mut self, condition: &HirExpr, body: &[HirStmt]) {
        let _pre_bb   = self.cur_bb;
        let header_bb = self.alloc_block();
        let body_bb   = self.alloc_block();
        let exit_bb   = self.alloc_block();

        self.goto(header_bb);
        self.switch_to(header_bb);

        let cond = self.lower_expr(condition);
        self.set_terminator(Terminator::Branch {
            cond,
            then_bb: body_bb,
            else_bb: exit_bb,
        });

        self.switch_to(body_bb);
        self.lower_stmts(body);
        if !self.terminated { self.goto(header_bb); }

        self.switch_to(exit_bb);
    }

    // ── attempt / catch lowering ──────────────────────────────────────────────

    fn lower_attempt(
        &mut self,
        body:      &[HirStmt],
        catches:   &[HirCatchClause],
        otherwise: Option<&[HirStmt]>,
        finally:   Option<&[HirStmt]>,
    ) {
        let catch_bb     = self.alloc_block();
        let otherwise_bb = self.alloc_block();
        let finally_bb   = self.alloc_block();

        // Lower body — `throw` unwinds to `catch_bb`.
        self.lower_stmts(body);
        let _normal_end = self.cur_bb;
        if !self.terminated { self.goto(otherwise_bb); }

        // Catch block (landing pad).
        self.switch_to(catch_bb);
        for clause in catches {
            if let Some(binding) = clause.binding {
                let err_local = self.alloc_local();
                self.emit(Instr::Assign {
                    dest: err_local,
                    ty: MirTy::Dynamic,
                    rhs: Rvalue::Literal(MirLit::Nothing), // filled by runtime
                });
                self.define_var(binding, err_local);
            }
            self.lower_stmts(&clause.body);
        }
        if !self.terminated { self.goto(finally_bb); }

        // `otherwise` block (runs only if NO exception was thrown).
        self.switch_to(otherwise_bb);
        if let Some(stmts) = otherwise {
            self.lower_stmts(stmts);
        }
        if !self.terminated { self.goto(finally_bb); }

        // `finally` block.
        self.switch_to(finally_bb);
        if let Some(stmts) = finally {
            self.lower_stmts(stmts);
        }
    }
}

// Dummy helper: a Symbol representing the built-in `len` method.
// In a full implementation this would use the shared interner.
fn _builtin_sym_len() -> Symbol {
    // Use a sentinel value; the interpreter knows to resolve this.
    Symbol(u32::MAX)
}

// ── Top-level lowering ────────────────────────────────────────────────────────

/// Lower an entire `HirModule` into a `MirProgram`.
///
/// Functions are numbered sequentially.  The first function (`FunctionId(0)`)
/// is always the top-level initialisation function (globals + init_stmts).
pub fn lower_program(hir: &HirModule) -> MirProgram {
    let mut prog = MirProgram::new();

    // ── Pre-pass: allocate FunctionIds for all named actions ────────────────
    // Top-level init function always gets FunctionId(0).
    let init_sym = Symbol(0); // sentinel — resolved to module name at runtime
    let init_fn = MirFunction::new(FunctionId(0), init_sym, MirTy::Nothing);
    prog.functions.push(init_fn);

    // Allocate IDs for all user-defined functions.
    let mut fn_map: HashMap<Symbol, FunctionId> = HashMap::new();
    for func in &hir.functions {
        let id = FunctionId(prog.functions.len() as u32);
        fn_map.insert(func.name, id);
        let mfn = MirFunction::new(id, func.name, fidan_ty_to_mir(&func.return_ty));
        prog.functions.push(mfn);
    }
    // Object methods.
    for obj in &hir.objects {
        for method in &obj.methods {
            let id = FunctionId(prog.functions.len() as u32);
            fn_map.insert(method.name, id);
            let mfn = MirFunction::new(id, method.name, fidan_ty_to_mir(&method.return_ty));
            prog.functions.push(mfn);
        }
    }

    // ── Lower each function body ─────────────────────────────────────────────
    let lower_hir_fn = |prog: &mut MirProgram, fn_map: &HashMap<Symbol, FunctionId>, func: &HirFunction| {
        let fn_id = fn_map[&func.name];
        let entry_bb = prog.function_mut(fn_id).alloc_block();

        let mut ctx = FnCtx {
            prog,
            fn_id,
            cur_bb: entry_bb,
            env: VarEnv::new(),
            fn_map: fn_map.clone(),
            terminated: false,
        };

        // Define params as initial locals.
        for param in &func.params {
            let local = ctx.alloc_local();
            ctx.emit(Instr::Assign {
                dest: local,
                ty: fidan_ty_to_mir(&param.ty),
                rhs: Rvalue::Literal(MirLit::Nothing), // filled by call ABI
            });
            ctx.define_var(param.name, local);
            ctx.func_mut().params.push(MirParam {
                local,
                name: param.name,
                ty: fidan_ty_to_mir(&param.ty),
            });
        }

        ctx.lower_stmts(&func.body);

        if !ctx.terminated {
            ctx.set_terminator(Terminator::Return(None));
        }
    };

    // Lower each HirFunction.
    for func in &hir.functions {
        lower_hir_fn(&mut prog, &fn_map, func);
    }
    // Lower object methods.
    for obj in &hir.objects {
        for method in &obj.methods {
            lower_hir_fn(&mut prog, &fn_map, method);
        }
    }

    // ── Lower top-level initialisation function (FunctionId(0)) ─────────────
    {
        let fn_id = FunctionId(0);
        let entry_bb = prog.function_mut(fn_id).alloc_block();

        let mut ctx = FnCtx {
            prog: &mut prog,
            fn_id,
            cur_bb: entry_bb,
            env: VarEnv::new(),
            fn_map: fn_map.clone(),
            terminated: false,
        };

        // Lower globals.
        for g in &hir.globals {
            let mir_ty = fidan_ty_to_mir(&g.ty);
            let dest = ctx.alloc_local();
            if let Some(init) = &g.init {
                let val = ctx.lower_expr(init);
                ctx.emit(Instr::Assign { dest, ty: mir_ty, rhs: Rvalue::Use(val) });
            } else {
                ctx.emit(Instr::Assign { dest, ty: mir_ty, rhs: Rvalue::Literal(MirLit::Nothing) });
            }
            ctx.define_var(g.name, dest);
        }

        // Lower top-level init statements.
        ctx.lower_stmts(&hir.init_stmts);

        if !ctx.terminated {
            ctx.set_terminator(Terminator::Return(None));
        }
    }

    prog
}
