//! Statement emitter.

use crate::emit_expr::{emit_expr, emit_type};
use crate::printer::Printer;
use fidan_ast::{Stmt, StmtId};
use fidan_source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StmtGroup {
    Declaration,
    Simple,
    Block,
}

fn stmt_group(stmt: &Stmt) -> StmtGroup {
    match stmt {
        Stmt::VarDecl { .. } | Stmt::Destructure { .. } | Stmt::ActionDecl { .. } => {
            StmtGroup::Declaration
        }
        Stmt::If { .. }
        | Stmt::Check { .. }
        | Stmt::For { .. }
        | Stmt::While { .. }
        | Stmt::Attempt { .. }
        | Stmt::ParallelFor { .. }
        | Stmt::ConcurrentBlock { .. } => StmtGroup::Block,
        Stmt::Assign { .. }
        | Stmt::Expr { .. }
        | Stmt::Return { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Panic { .. }
        | Stmt::Error { .. } => StmtGroup::Simple,
    }
}

// ── Public entry ──────────────────────────────────────────────────────────────

/// Emit a single statement. The caller is responsible for positioning the
/// cursor at the start of a fresh indented line before calling this.
pub fn emit_stmt(p: &mut Printer<'_>, id: StmtId) {
    let stmt = p.arena.get_stmt(id).clone();
    let stmt_span = stmt_span(&stmt);
    p.emit_comments_before(stmt_span.start);
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

        Stmt::ActionDecl {
            name,
            params,
            return_ty,
            body,
            is_parallel,
            ..
        } => {
            if is_parallel {
                p.w("parallel action ");
            } else {
                p.w("action ");
            }
            let n = p.sym_s(name);
            p.w(&n);
            if !params.is_empty() {
                p.w(" with (");
                for (i, param) in params.iter().enumerate() {
                    if i > 0 {
                        p.w(", ");
                    }
                    if param.certain {
                        p.w("certain ");
                    } else if param.optional {
                        p.w("optional ");
                    }
                    let pn = p.sym_s(param.name);
                    p.w(&pn);
                    p.w(" oftype ");
                    emit_type(p, &param.ty);
                    if let Some(default) = param.default {
                        p.w(" = ");
                        emit_expr(p, default);
                    }
                }
                p.w(")");
            }
            if let Some(ref ty) = return_ty {
                p.w(" returns ");
                emit_type(p, ty);
            }
            p.w(" {");
            emit_block(p, &body, Some(stmt_span.end));
            p.w("}");
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
            span,
        } => {
            p.w("if ");
            emit_expr(p, condition);
            p.w(" {");
            emit_block(p, &then_body, Some(span.end));
            p.w("}");
            for ei in &else_ifs {
                p.emit_comments_before(ei.span.start);
                p.w(" otherwise when ");
                emit_expr(p, ei.condition);
                p.w(" {");
                emit_block(p, &ei.body, Some(ei.span.end));
                p.w("}");
                p.emit_trailing_comments_for(ei.span.end);
            }
            if let Some(ref else_b) = else_body {
                p.w(" else {");
                emit_block(p, else_b, Some(span.end));
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
                p.emit_comments_before(arm.span.start);
                p.nl();
                emit_expr(p, arm.pattern);
                p.w(" => {");
                emit_block(p, &arm.body, Some(arm.span.end));
                p.w("}");
                p.emit_trailing_comments_for(arm.span.end);
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
            span,
        } => {
            p.w("for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr(p, iterable);
            p.w(" {");
            emit_block(p, &body, Some(span.end));
            p.w("}");
        }

        // ── While loop ───────────────────────────────────────────────────
        Stmt::While {
            condition, body, ..
        } => {
            p.w("while ");
            emit_expr(p, condition);
            p.w(" {");
            emit_block(p, &body, Some(stmt_span.end));
            p.w("}");
        }

        // ── Attempt / catch / otherwise / finally ─────────────────────────
        Stmt::Attempt {
            body,
            catches,
            otherwise,
            finally,
            span,
        } => {
            p.w("attempt {");
            emit_block(p, &body, Some(span.end));
            p.w("}");
            for catch in &catches {
                p.emit_comments_before(catch.span.start);
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
                emit_block(p, &catch.body, Some(catch.span.end));
                p.w("}");
                p.emit_trailing_comments_for(catch.span.end);
            }
            if let Some(ref ow) = otherwise {
                p.w(" otherwise {");
                emit_block(p, ow, Some(span.end));
                p.w("}");
            }
            if let Some(ref fin) = finally {
                p.w(" finally {");
                emit_block(p, fin, Some(span.end));
                p.w("}");
            }
        }

        // ── Parallel for ─────────────────────────────────────────────────
        Stmt::ParallelFor {
            binding,
            iterable,
            body,
            span,
        } => {
            p.w("parallel for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr(p, iterable);
            p.w(" {");
            emit_block(p, &body, Some(span.end));
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
                p.emit_comments_before(task.span.start);
                p.nl();
                p.w("task");
                if let Some(name) = task.name {
                    p.w(" ");
                    let n = p.sym_s(name);
                    p.w(&n);
                }
                p.w(" {");
                emit_block(p, &task.body, Some(task.span.end));
                p.w("}");
                p.emit_trailing_comments_for(task.span.end);
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
    p.emit_trailing_comments_for(stmt_span.end);
}

// ── Block helper ──────────────────────────────────────────────────────────────

/// Emit a `{ body }` block: indents, emits each statement on its own line,
/// then de-indents.  The opening `{` and closing `}` are NOT emitted here —
/// the caller wrote `{` just before calling this and writes `}` afterwards.
pub fn emit_block(p: &mut Printer<'_>, stmts: &[StmtId], block_end: Option<u32>) {
    if stmts.is_empty() {
        if let Some(end) = block_end {
            p.emit_comments_before(end);
        }
        return;
    }
    p.indent_in();
    let mut prev_group: Option<StmtGroup> = None;
    for &sid in stmts {
        let stmt = p.arena.get_stmt(sid);
        let curr_group = stmt_group(stmt);
        p.nl();
        if let Some(prev) = prev_group
            && prev != curr_group
        {
            p.blank();
        }
        emit_stmt(p, sid);
        prev_group = Some(curr_group);
    }
    if let Some(end) = block_end {
        p.emit_comments_before(end);
    }
    p.indent_out();
    p.nl();
}

fn stmt_span(stmt: &Stmt) -> Span {
    match stmt {
        Stmt::VarDecl { span, .. } => *span,
        Stmt::Destructure { span, .. } => *span,
        Stmt::Assign { span, .. } => *span,
        Stmt::Expr { span, .. } => *span,
        Stmt::ActionDecl { span, .. } => *span,
        Stmt::Return { span, .. } => *span,
        Stmt::Break { span } => *span,
        Stmt::Continue { span } => *span,
        Stmt::If { span, .. } => *span,
        Stmt::Check { span, .. } => *span,
        Stmt::For { span, .. } => *span,
        Stmt::While { span, .. } => *span,
        Stmt::Attempt { span, .. } => *span,
        Stmt::ParallelFor { span, .. } => *span,
        Stmt::ConcurrentBlock { span, .. } => *span,
        Stmt::Panic { span, .. } => *span,
        Stmt::Error { span } => *span,
    }
}
