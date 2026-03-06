//! Statement emitter.

use crate::emit_expr::{emit_expr, emit_type};
use crate::printer::Printer;
use fidan_ast::{Stmt, StmtId};

// ── Public entry ──────────────────────────────────────────────────────────────

/// Emit a single statement. The caller is responsible for positioning the
/// cursor at the start of a fresh indented line before calling this.
pub fn emit_stmt(p: &mut Printer<'_>, id: StmtId) {
    let stmt = p.arena.get_stmt(id).clone();
    match stmt {
        // ── Variable / const declarations ─────────────────────────────────
        Stmt::VarDecl {
            name,
            ty,
            init,
            is_const,
            ..
        } => {
            if is_const {
                p.w("const var ");
            } else {
                p.w("var ");
            }
            let n = p.sym_s(name);
            p.w(&n);
            if let Some(ref t) = ty {
                p.w(" oftype ");
                emit_type(p, t);
            }
            if let Some(eid) = init {
                p.w(" = ");
                emit_expr(p, eid);
            }
        }

        // ── Tuple destructure ─────────────────────────────────────────────
        Stmt::Destructure {
            bindings, value, ..
        } => {
            p.w("var (");
            for (i, sym) in bindings.iter().enumerate() {
                if i > 0 {
                    p.w(", ");
                }
                let s = p.sym_s(*sym);
                p.w(&s);
            }
            p.w(") = ");
            emit_expr(p, value);
        }

        // ── Assignment ────────────────────────────────────────────────────
        Stmt::Assign { target, value, .. } => {
            emit_expr(p, target);
            p.w(" = ");
            emit_expr(p, value);
        }

        // ── Expression statement ──────────────────────────────────────────
        Stmt::Expr { expr, .. } => {
            emit_expr(p, expr);
        }

        // ── Control flow ──────────────────────────────────────────────────
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                p.w("return ");
                emit_expr(p, v);
            } else {
                p.w("return");
            }
        }
        Stmt::Break { .. } => p.w("break"),
        Stmt::Continue { .. } => p.w("continue"),

        // ── If / otherwise when / else ────────────────────────────────────
        Stmt::If {
            condition,
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            p.w("if ");
            emit_expr(p, condition);
            p.w(" {");
            emit_block(p, &then_body);
            p.w("}");
            for ei in &else_ifs {
                p.w(" otherwise when ");
                emit_expr(p, ei.condition);
                p.w(" {");
                emit_block(p, &ei.body);
                p.w("}");
            }
            if let Some(ref else_b) = else_body {
                p.w(" else {");
                emit_block(p, else_b);
                p.w("}");
            }
        }

        // ── Check statement ───────────────────────────────────────────────
        Stmt::Check {
            scrutinee, arms, ..
        } => {
            p.w("check ");
            emit_expr(p, scrutinee);
            p.w(" {");
            p.indent_in();
            for arm in &arms {
                p.nl();
                emit_expr(p, arm.pattern);
                p.w(" => {");
                emit_block(p, &arm.body);
                p.w("}");
            }
            p.indent_out();
            p.nl();
            p.w("}");
        }

        // ── For loop ─────────────────────────────────────────────────────
        Stmt::For {
            binding,
            iterable,
            body,
            ..
        } => {
            p.w("for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr(p, iterable);
            p.w(" {");
            emit_block(p, &body);
            p.w("}");
        }

        // ── While loop ───────────────────────────────────────────────────
        Stmt::While {
            condition, body, ..
        } => {
            p.w("while ");
            emit_expr(p, condition);
            p.w(" {");
            emit_block(p, &body);
            p.w("}");
        }

        // ── Attempt / catch / otherwise / finally ─────────────────────────
        Stmt::Attempt {
            body,
            catches,
            otherwise,
            finally,
            ..
        } => {
            p.w("attempt {");
            emit_block(p, &body);
            p.w("}");
            for catch in &catches {
                p.w(" catch");
                if catch.binding.is_some() || catch.ty.is_some() {
                    p.w(" ");
                    if let Some(b) = catch.binding {
                        let s = p.sym_s(b);
                        p.w(&s);
                    }
                    if let Some(ref t) = catch.ty {
                        p.w(" oftype ");
                        emit_type(p, t);
                    }
                }
                p.w(" {");
                emit_block(p, &catch.body);
                p.w("}");
            }
            if let Some(ref ow) = otherwise {
                p.w(" otherwise {");
                emit_block(p, ow);
                p.w("}");
            }
            if let Some(ref fin) = finally {
                p.w(" finally {");
                emit_block(p, fin);
                p.w("}");
            }
        }

        // ── Parallel for ─────────────────────────────────────────────────
        Stmt::ParallelFor {
            binding,
            iterable,
            body,
            ..
        } => {
            p.w("parallel for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr(p, iterable);
            p.w(" {");
            emit_block(p, &body);
            p.w("}");
        }

        // ── Concurrent / parallel block ───────────────────────────────────
        Stmt::ConcurrentBlock {
            is_parallel, tasks, ..
        } => {
            if is_parallel {
                p.w("parallel {");
            } else {
                p.w("concurrent {");
            }
            p.indent_in();
            for task in &tasks {
                p.nl();
                p.w("task");
                if let Some(name) = task.name {
                    p.w(" ");
                    let n = p.sym_s(name);
                    p.w(&n);
                }
                p.w(" {");
                emit_block(p, &task.body);
                p.w("}");
            }
            p.indent_out();
            p.nl();
            p.w("}");
        }

        // ── Panic / throw ─────────────────────────────────────────────────
        Stmt::Panic { value, .. } => {
            p.w("panic(");
            emit_expr(p, value);
            p.w(")");
        }

        // ── Error placeholder ─────────────────────────────────────────────
        Stmt::Error { .. } => {
            p.w("# <parse error>");
        }
    }
}

// ── Block helper ──────────────────────────────────────────────────────────────

/// Emit a `{ body }` block: indents, emits each statement on its own line,
/// then de-indents.  The opening `{` and closing `}` are NOT emitted here —
/// the caller wrote `{` just before calling this and writes `}` afterwards.
pub fn emit_block(p: &mut Printer<'_>, stmts: &[StmtId]) {
    if stmts.is_empty() {
        // Empty block: just a single space so it formats as `{ }`
        // (actually most formatters put nothing, resulting in `{}`)
        return;
    }
    p.indent_in();
    for &sid in stmts {
        p.nl();
        emit_stmt(p, sid);
    }
    p.indent_out();
    p.nl();
}
