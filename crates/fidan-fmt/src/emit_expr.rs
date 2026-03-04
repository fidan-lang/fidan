//! Expression emitter.
//!
//! All expression rendering is done inline (no line-wrapping in this version).
//! Operator precedence is properly tracked so redundant parentheses are
//! never emitted and necessary ones are never omitted.

use crate::printer::Printer;
use fidan_ast::{Arg, BinOp, Expr, ExprId, InterpPart, TypeExpr, UnOp};

// ── Precedence ────────────────────────────────────────────────────────────────

/// Precedence of a binary operator (higher = binds tighter).
fn binop_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Range | BinOp::RangeInclusive => 2,
        BinOp::Or => 3,
        BinOp::And => 4,
        BinOp::Eq
        | BinOp::NotEq
        | BinOp::Lt
        | BinOp::LtEq
        | BinOp::Gt
        | BinOp::GtEq => 5,
        BinOp::BitOr => 6,
        BinOp::BitXor => 7,
        BinOp::BitAnd => 8,
        BinOp::Shl | BinOp::Shr => 9,
        BinOp::Add | BinOp::Sub => 10,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 11,
        BinOp::Pow => 12,
    }
}

/// Textual representation of a binary operator.
fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Pow => "**",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::BitXor => "^",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Range => "..",
        BinOp::RangeInclusive => "...",
    }
}

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Pos => "+",
        UnOp::Neg => "-",
        UnOp::Not => "not ",
    }
}

// ── Public entry ──────────────────────────────────────────────────────────────

/// Emit an expression at precedence level 0 (outermost — never adds parens).
pub fn emit_expr(p: &mut Printer<'_>, id: ExprId) {
    emit_expr_prec(p, id, 0);
}

/// Emit an expression, parenthesising it if its own precedence is below `min_prec`.
pub fn emit_expr_prec(p: &mut Printer<'_>, id: ExprId, min_prec: u8) {
    let expr = p.arena.get_expr(id).clone();
    match expr {
        // ── Literals ──────────────────────────────────────────────────────
        Expr::IntLit { value, .. } => {
            p.w(&value.to_string());
        }
        Expr::FloatLit { value, .. } => {
            // Preserve at least one decimal digit so `1.0` doesn't become `1`.
            let s = if value.fract() == 0.0 {
                format!("{value:.1}")
            } else {
                // Use Rust's default float formatting; it's round-trip correct.
                format!("{value}")
            };
            p.w(&s);
        }
        Expr::StrLit { value, .. } => {
            p.w(&escape_str(&value));
        }
        Expr::BoolLit { value, .. } => {
            p.w(if value { "true" } else { "false" });
        }
        Expr::Nothing { .. } => {
            p.w("nothing");
        }

        // ── Names ─────────────────────────────────────────────────────────
        Expr::Ident { name, .. } => {
            let s = p.sym_s(name);
            p.w(&s);
        }
        Expr::This { .. } => p.w("this"),
        Expr::Parent { .. } => p.w("parent"),

        // ── Binary ────────────────────────────────────────────────────────
        Expr::Binary { op, lhs, rhs, .. } => {
            let prec = binop_prec(op);
            let needs_parens = prec < min_prec;
            if needs_parens {
                p.w("(");
            }
            // Right operand needs prec+1 for left-associative ops (all except Pow).
            // For Pow use the same prec so `a ** b ** c` parses as `a ** (b ** c)`.
            let rhs_min = match op {
                BinOp::Pow => prec,
                _ => prec + 1,
            };
            emit_expr_prec(p, lhs, prec);
            p.w(" ");
            p.w(binop_str(op));
            p.w(" ");
            emit_expr_prec(p, rhs, rhs_min);
            if needs_parens {
                p.w(")");
            }
        }

        // ── Unary ─────────────────────────────────────────────────────────
        Expr::Unary { op, operand, .. } => {
            // Unary operators bind very tightly (prec 14).
            let needs_parens = 14_u8 < min_prec;
            if needs_parens {
                p.w("(");
            }
            p.w(unop_str(op));
            emit_expr_prec(p, operand, 14);
            if needs_parens {
                p.w(")");
            }
        }

        // ── Null-coalesce ─────────────────────────────────────────────────
        Expr::NullCoalesce { lhs, rhs, .. } => {
            // Treat ?? as prec 1 (just above ternary).
            let prec = 1_u8;
            let needs_parens = prec < min_prec;
            if needs_parens {
                p.w("(");
            }
            emit_expr_prec(p, lhs, prec);
            p.w(" ?? ");
            emit_expr_prec(p, rhs, prec + 1);
            if needs_parens {
                p.w(")");
            }
        }

        // ── Postfix: call, field, index ────────────────────────────────────
        Expr::Call { callee, args, .. } => {
            emit_expr_prec(p, callee, 15);
            p.w("(");
            emit_arg_list(p, &args);
            p.w(")");
        }
        Expr::Field { object, field, .. } => {
            emit_expr_prec(p, object, 15);
            p.w(".");
            let s = p.sym_s(field);
            p.w(&s);
        }
        Expr::Index { object, index, .. } => {
            emit_expr_prec(p, object, 15);
            p.w("[");
            emit_expr(p, index);
            p.w("]");
        }

        // ── Assignment (expression form) ───────────────────────────────────
        Expr::Assign { target, value, .. } => {
            emit_expr_prec(p, target, 15);
            p.w(" = ");
            emit_expr(p, value);
        }
        Expr::CompoundAssign { op, target, value, .. } => {
            emit_expr_prec(p, target, 15);
            p.w(" ");
            p.w(binop_str(op));
            p.w("= ");
            emit_expr(p, value);
        }

        // ── String interpolation ───────────────────────────────────────────
        Expr::StringInterp { parts, .. } => {
            p.w("\"");
            for part in &parts {
                match part {
                    InterpPart::Literal(s) => {
                        // Re-escape the literal chunk (it was unescaped by the lexer).
                        p.w(&escape_str_inner(s));
                    }
                    InterpPart::Expr(eid) => {
                        p.w("{");
                        emit_expr(p, *eid);
                        p.w("}");
                    }
                }
            }
            p.w("\"");
        }

        // ── Spawn / await ─────────────────────────────────────────────────
        Expr::Spawn { expr, .. } => {
            p.w("spawn ");
            emit_expr_prec(p, expr, 13);
        }
        Expr::Await { expr, .. } => {
            p.w("await ");
            emit_expr_prec(p, expr, 13);
        }

        // ── Ternary ──────────────────────────────────────────────────────
        // Fidan ternary syntax: `then_val if condition else else_val`
        Expr::Ternary { condition, then_val, else_val, .. } => {
            let needs_parens = 0_u8 < min_prec;
            if needs_parens {
                p.w("(");
            }
            emit_expr_prec(p, then_val, 1);
            p.w(" if ");
            emit_expr_prec(p, condition, 1);
            p.w(" else ");
            emit_expr_prec(p, else_val, 1);
            if needs_parens {
                p.w(")");
            }
        }

        // ── Collection literals ───────────────────────────────────────────
        Expr::List { elements, .. } => {
            p.w("[");
            for (i, &eid) in elements.iter().enumerate() {
                if i > 0 {
                    p.w(", ");
                }
                emit_expr(p, eid);
            }
            p.w("]");
        }
        Expr::Dict { entries, .. } => {
            p.w("{");
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    p.w(", ");
                }
                emit_expr(p, *k);
                p.w(": ");
                emit_expr(p, *v);
            }
            p.w("}");
        }
        Expr::Tuple { elements, .. } => {
            p.w("(");
            for (i, &eid) in elements.iter().enumerate() {
                if i > 0 {
                    p.w(", ");
                }
                emit_expr(p, eid);
            }
            p.w(")");
        }

        // ── Check expression ─────────────────────────────────────────────
        Expr::Check { scrutinee, arms, .. } => {
            p.w("check ");
            emit_expr(p, scrutinee);
            p.w(" {");
            p.indent_in();
            for arm in &arms {
                p.nl();
                emit_expr(p, arm.pattern);
                p.w(" => {");
                emit_inline_stmts_or_block(p, &arm.body);
                p.w("}");
            }
            p.indent_out();
            p.nl();
            p.w("}");
        }

        // ── Slice ─────────────────────────────────────────────────────────
        Expr::Slice { target, start, end, inclusive, step, .. } => {
            emit_expr_prec(p, target, 15);
            p.w("[");
            if let Some(s) = start {
                emit_expr(p, s);
            }
            p.w(if inclusive { "..." } else { ".." });
            if let Some(e) = end {
                emit_expr(p, e);
            }
            if let Some(st) = step {
                p.w(" step ");
                emit_expr(p, st);
            }
            p.w("]");
        }

        // ── List comprehension ────────────────────────────────────────────
        Expr::ListComp { element, binding, iterable, filter, .. } => {
            p.w("[");
            emit_expr(p, element);
            p.w(" for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr(p, iterable);
            if let Some(f) = filter {
                p.w(" if ");
                emit_expr(p, f);
            }
            p.w("]");
        }

        // ── Dict comprehension ────────────────────────────────────────────
        Expr::DictComp { key, value, binding, iterable, filter, .. } => {
            p.w("{");
            emit_expr(p, key);
            p.w(": ");
            emit_expr(p, value);
            p.w(" for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr(p, iterable);
            if let Some(f) = filter {
                p.w(" if ");
                emit_expr(p, f);
            }
            p.w("}");
        }

        // ── Error placeholder ─────────────────────────────────────────────
        Expr::Error { .. } => {
            p.w("<error>");
        }
    }
}

// ── Argument list ─────────────────────────────────────────────────────────────

pub fn emit_arg_list(p: &mut Printer<'_>, args: &[Arg]) {
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            p.w(", ");
        }
        if let Some(name) = arg.name {
            let n = p.sym_s(name);
            p.w(&n);
            p.w(": ");
        }
        emit_expr(p, arg.value);
    }
}

// ── Type expression ───────────────────────────────────────────────────────────

pub fn emit_type(p: &mut Printer<'_>, ty: &TypeExpr) {
    match ty {
        TypeExpr::Named { name, .. } => {
            let s = p.sym_s(*name);
            p.w(&s);
        }
        TypeExpr::Oftype { base, param, .. } => {
            emit_type(p, base);
            p.w(" oftype ");
            emit_type(p, param);
        }
        TypeExpr::Tuple { elements, .. } => {
            if elements.is_empty() {
                p.w("tuple");
            } else {
                p.w("(");
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        p.w(", ");
                    }
                    emit_type(p, elem);
                }
                p.w(")");
            }
        }
        TypeExpr::Dynamic { .. } => p.w("flexible"),
        TypeExpr::Nothing { .. } => p.w("nothing"),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Emit a list of statements inline between braces **only** when there is
/// exactly one statement and it fits on one line.  Otherwise fall through to
/// a normal multi-line block.  Used by `check` expression arms.
fn emit_inline_stmts_or_block(p: &mut Printer<'_>, stmts: &[fidan_ast::StmtId]) {
    // For now always emit as multi-line block for simplicity.
    use crate::emit_stmt::emit_stmt;
    if stmts.is_empty() {
        // nothing between the braces
    } else {
        p.indent_in();
        for &sid in stmts {
            p.nl();
            emit_stmt(p, sid);
        }
        p.indent_out();
        p.nl();
    }
}

/// Escape a raw string value back into a Fidan `"..."` literal.
/// The returned string includes the surrounding double-quotes.
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&escape_str_inner(s));
    out.push('"');
    out
}

/// Escape a string fragment without surrounding quotes.
/// Used by both `StrLit` and the literal parts of `StringInterp`.
pub fn escape_str_inner(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            c => out.push(c),
        }
    }
    out
}
