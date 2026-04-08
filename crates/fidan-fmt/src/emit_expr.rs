//! Expression emitter.
//!
//! Operator precedence is properly tracked so redundant parentheses are never
//! emitted and necessary ones are never omitted. Long call and member chains are
//! wrapped when they exceed the configured soft line-length limit.

use crate::printer::Printer;
use fidan_ast::{Arg, BinOp, Expr, ExprId, InterpPart, Param, StmtId, TypeExpr, UnOp};

#[derive(Clone)]
enum ChainSegment {
    Field(fidan_lexer::Symbol),
    Index(ExprId),
    Call(Vec<Arg>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Normal,
    InlineOnly,
}

// ── Precedence ────────────────────────────────────────────────────────────────

/// Precedence of a binary operator (higher = binds tighter).
fn binop_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Range | BinOp::RangeInclusive => 2,
        BinOp::Or => 3,
        BinOp::And => 4,
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => 5,
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
pub(crate) fn binop_str(op: BinOp) -> &'static str {
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
    emit_expr_prec_mode(p, id, 0, LayoutMode::Normal);
}

/// Emit an expression, wrapping long call/member chains when needed.
pub fn emit_expr_maybe_wrapped(p: &mut Printer<'_>, id: ExprId) {
    if should_wrap_expr(p, id) {
        emit_wrapped_expr(p, id);
    } else {
        emit_expr(p, id);
    }
}

/// Emit an expression after a prefix such as `var value = ` or `return `.
/// When the expression would overflow the soft line-length limit, it is moved
/// to a continuation line one indent level deeper.
pub fn emit_expr_after_prefix(p: &mut Printer<'_>, id: ExprId) {
    if should_wrap_expr(p, id) {
        if let Some((root, segments)) = decompose_chain_expr(p, id)
            && root_fits_after_prefix(p, root)
        {
            emit_expr_prec(p, root, 15);
            emit_wrapped_chain_segments(p, &segments);
        } else {
            p.indent_in();
            p.nl();
            emit_wrapped_expr(p, id);
            p.indent_out();
        }
    } else {
        emit_expr(p, id);
    }
}

/// Emit an expression, parenthesising it if its own precedence is below `min_prec`.
pub fn emit_expr_prec(p: &mut Printer<'_>, id: ExprId, min_prec: u8) {
    emit_expr_prec_mode(p, id, min_prec, LayoutMode::Normal);
}

fn emit_expr_prec_mode(p: &mut Printer<'_>, id: ExprId, min_prec: u8, mode: LayoutMode) {
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
            let rendered = if should_preserve_multiline_string(p, &value) {
                escape_str_multiline(&value)
            } else {
                escape_str(&value)
            };
            p.w(&rendered);
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
            emit_expr_prec_mode(p, lhs, prec, mode);
            p.w(" ");
            p.w(binop_str(op));
            p.w(" ");
            emit_expr_prec_mode(p, rhs, rhs_min, mode);
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
            emit_expr_prec_mode(p, operand, 14, mode);
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
            emit_expr_prec_mode(p, lhs, prec, mode);
            p.w(" ?? ");
            emit_expr_prec_mode(p, rhs, prec + 1, mode);
            if needs_parens {
                p.w(")");
            }
        }

        // ── Postfix: call, field, index ────────────────────────────────────
        Expr::Call { callee, args, .. } => {
            emit_expr_prec_mode(p, callee, 15, mode);
            p.w("(");
            if mode == LayoutMode::Normal && should_break_arg_list(p, &args) {
                emit_multiline_arg_list_mode(p, &args, mode);
            } else {
                emit_arg_list_mode(p, &args, mode);
            }
            p.w(")");
        }
        Expr::Field { object, field, .. } => {
            emit_expr_prec_mode(p, object, 15, mode);
            p.w(".");
            let s = p.sym_s(field);
            p.w(&s);
        }
        Expr::Index { object, index, .. } => {
            emit_expr_prec_mode(p, object, 15, mode);
            p.w("[");
            emit_expr_prec_mode(p, index, 0, mode);
            p.w("]");
        }

        // ── Assignment (expression form) ───────────────────────────────────
        Expr::Assign { target, value, .. } => {
            emit_expr_prec_mode(p, target, 15, mode);
            p.w(" = ");
            emit_expr_prec_mode(p, value, 0, mode);
        }
        Expr::CompoundAssign {
            op, target, value, ..
        } => {
            emit_expr_prec_mode(p, target, 15, mode);
            p.w(" ");
            p.w(binop_str(op));
            p.w("= ");
            emit_expr_prec_mode(p, value, 0, mode);
        }

        // ── String interpolation ───────────────────────────────────────────
        Expr::StringInterp { parts, .. } => {
            let rendered = if should_preserve_multiline_interp(p, &parts) {
                render_string_interp(p, &parts, true)
            } else {
                render_string_interp(p, &parts, false)
            };
            p.w(&rendered);
        }

        // ── Spawn / await ─────────────────────────────────────────────────
        Expr::Spawn { expr, .. } => {
            p.w("spawn ");
            emit_expr_prec_mode(p, expr, 13, mode);
        }
        Expr::Await { expr, .. } => {
            p.w("await ");
            emit_expr_prec_mode(p, expr, 13, mode);
        }

        // ── Ternary ──────────────────────────────────────────────────────
        // Fidan ternary syntax: `then_val if condition else else_val`
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            let needs_parens = 0_u8 < min_prec;
            if needs_parens {
                p.w("(");
            }
            emit_expr_prec_mode(p, then_val, 1, mode);
            p.w(" if ");
            emit_expr_prec_mode(p, condition, 1, mode);
            p.w(" else ");
            emit_expr_prec_mode(p, else_val, 1, mode);
            if needs_parens {
                p.w(")");
            }
        }

        // ── Collection literals ───────────────────────────────────────────
        Expr::List { elements, .. } => {
            emit_list_expr(p, &elements, mode);
        }
        Expr::Dict { entries, .. } => {
            emit_dict_expr(p, &entries, mode);
        }
        Expr::Tuple { elements, .. } => {
            emit_tuple_expr(p, &elements, mode);
        }

        // ── Check expression ─────────────────────────────────────────────
        Expr::Check {
            scrutinee, arms, ..
        } => {
            p.w("check ");
            emit_expr_prec_mode(p, scrutinee, 0, mode);
            p.w(" {");
            p.indent_in();
            for arm in &arms {
                p.nl();
                emit_expr_prec_mode(p, arm.pattern, 0, mode);
                if let Some(stmt_id) = crate::emit_stmt::inlineable_check_stmt(p, &arm.body) {
                    p.w(" => ");
                    crate::emit_stmt::emit_stmt(p, stmt_id);
                } else {
                    p.w(" => {");
                    emit_inline_stmts_or_block(p, &arm.body);
                    p.w("}");
                }
            }
            p.indent_out();
            p.nl();
            p.w("}");
        }

        // ── Slice ─────────────────────────────────────────────────────────
        Expr::Slice {
            target,
            start,
            end,
            inclusive,
            step,
            ..
        } => {
            emit_expr_prec_mode(p, target, 15, mode);
            p.w("[");
            if let Some(s) = start {
                emit_expr_prec_mode(p, s, 0, mode);
            }
            p.w(if inclusive { "..." } else { ".." });
            if let Some(e) = end {
                emit_expr_prec_mode(p, e, 0, mode);
            }
            if let Some(st) = step {
                p.w(" step ");
                emit_expr_prec_mode(p, st, 0, mode);
            }
            p.w("]");
        }

        // ── List comprehension ────────────────────────────────────────────
        Expr::ListComp {
            element,
            binding,
            iterable,
            filter,
            ..
        } => {
            p.w("[");
            emit_expr_prec_mode(p, element, 0, mode);
            p.w(" for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr_prec_mode(p, iterable, 0, mode);
            if let Some(f) = filter {
                p.w(" if ");
                emit_expr_prec_mode(p, f, 0, mode);
            }
            p.w("]");
        }

        // ── Dict comprehension ────────────────────────────────────────────
        Expr::DictComp {
            key,
            value,
            binding,
            iterable,
            filter,
            ..
        } => {
            p.w("{");
            emit_expr_prec_mode(p, key, 0, mode);
            p.w(": ");
            emit_expr_prec_mode(p, value, 0, mode);
            p.w(" for ");
            let b = p.sym_s(binding);
            p.w(&b);
            p.w(" in ");
            emit_expr_prec_mode(p, iterable, 0, mode);
            if let Some(f) = filter {
                p.w(" if ");
                emit_expr_prec_mode(p, f, 0, mode);
            }
            p.w("}");
        }

        // ── Error placeholder ─────────────────────────────────────────────
        Expr::Error { .. } => {
            p.w("<error>");
        }

        // ── Inline lambda ─────────────────────────────────────────────────
        Expr::Lambda {
            params,
            return_ty,
            body,
            ..
        } => {
            p.w("action");
            if !params.is_empty() {
                p.w(" with (");
                emit_params_mode(p, &params, mode);
                p.w(")");
            }
            if let Some(rt) = return_ty {
                p.w(" returns ");
                emit_type(p, &rt);
            }
            p.w(" {");
            emit_lambda_block(p, &body);
            p.w("}");
        }
    }
}

// ── Argument list ─────────────────────────────────────────────────────────────

pub fn emit_arg_list(p: &mut Printer<'_>, args: &[Arg]) {
    emit_arg_list_mode(p, args, LayoutMode::Normal);
}

fn emit_arg_list_mode(p: &mut Printer<'_>, args: &[Arg], mode: LayoutMode) {
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            p.w(", ");
        }
        if let Some(name) = arg.name {
            let n = p.sym_s(name);
            p.w(&n);
            p.w(" = ");
        }
        emit_expr_prec_mode(p, arg.value, 0, mode);
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
        TypeExpr::Dynamic { .. } => p.w("dynamic"),
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

fn emit_params_mode(p: &mut Printer<'_>, params: &[Param], mode: LayoutMode) {
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
            emit_expr_prec_mode(p, default, 0, mode);
        }
    }
}

fn emit_lambda_block(p: &mut Printer<'_>, stmts: &[StmtId]) {
    if stmts.is_empty() {
        return;
    }
    p.indent_in();
    for &sid in stmts {
        p.nl();
        crate::emit_stmt::emit_stmt(p, sid);
    }
    p.indent_out();
    p.nl();
}

fn should_wrap_expr(p: &Printer<'_>, id: ExprId) -> bool {
    can_wrap_expr(p, id) && inline_expr_overflows(p, id)
}

fn can_wrap_expr(p: &Printer<'_>, id: ExprId) -> bool {
    decompose_chain_expr(p, id).is_some()
}

fn inline_expr_overflows(p: &Printer<'_>, id: ExprId) -> bool {
    p.current_line_len() + render_expr_fragment(p, id, 0).len() > p.opts.max_line_len
}

fn emit_list_expr(p: &mut Printer<'_>, elements: &[ExprId], mode: LayoutMode) {
    if mode == LayoutMode::Normal && should_break_list_expr(p, elements) {
        emit_multiline_list_expr(p, elements);
        return;
    }

    p.w("[");
    for (i, &eid) in elements.iter().enumerate() {
        if i > 0 {
            p.w(", ");
        }
        emit_expr_prec_mode(p, eid, 0, mode);
    }
    p.w("]");
}

fn emit_multiline_list_expr(p: &mut Printer<'_>, elements: &[ExprId]) {
    p.w("[");
    if !elements.is_empty() {
        p.indent_in();
        for (i, &eid) in elements.iter().enumerate() {
            p.nl();
            emit_expr_maybe_wrapped(p, eid);
            if i + 1 < elements.len() || p.opts.trailing_comma {
                p.w(",");
            }
        }
        p.indent_out();
        p.nl();
    }
    p.w("]");
}

fn emit_dict_expr(p: &mut Printer<'_>, entries: &[(ExprId, ExprId)], mode: LayoutMode) {
    if mode == LayoutMode::Normal && should_break_dict_expr(p, entries) {
        emit_multiline_dict_expr(p, entries);
        return;
    }

    p.w("{");
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            p.w(", ");
        }
        emit_expr_prec_mode(p, *k, 0, mode);
        p.w(": ");
        emit_expr_prec_mode(p, *v, 0, mode);
    }
    p.w("}");
}

fn emit_multiline_dict_expr(p: &mut Printer<'_>, entries: &[(ExprId, ExprId)]) {
    p.w("{");
    if !entries.is_empty() {
        p.indent_in();
        for (i, (key, value)) in entries.iter().enumerate() {
            p.nl();
            emit_expr_maybe_wrapped(p, *key);
            p.w(": ");
            emit_expr_after_prefix(p, *value);
            if i + 1 < entries.len() || p.opts.trailing_comma {
                p.w(",");
            }
        }
        p.indent_out();
        p.nl();
    }
    p.w("}");
}

fn emit_tuple_expr(p: &mut Printer<'_>, elements: &[ExprId], mode: LayoutMode) {
    if mode == LayoutMode::Normal && should_break_tuple_expr(p, elements) {
        emit_multiline_tuple_expr(p, elements);
        return;
    }

    p.w("(");
    for (i, &eid) in elements.iter().enumerate() {
        if i > 0 {
            p.w(", ");
        }
        emit_expr_prec_mode(p, eid, 0, mode);
    }
    p.w(")");
}

fn emit_multiline_tuple_expr(p: &mut Printer<'_>, elements: &[ExprId]) {
    p.w("(");
    if !elements.is_empty() {
        p.indent_in();
        for (i, &eid) in elements.iter().enumerate() {
            p.nl();
            emit_expr_maybe_wrapped(p, eid);
            if i + 1 < elements.len() || p.opts.trailing_comma {
                p.w(",");
            }
        }
        p.indent_out();
        p.nl();
    }
    p.w(")");
}

fn should_break_list_expr(p: &Printer<'_>, elements: &[ExprId]) -> bool {
    p.current_line_len() + render_list_inline(p, elements).len() > p.opts.max_line_len
}

fn should_break_dict_expr(p: &Printer<'_>, entries: &[(ExprId, ExprId)]) -> bool {
    p.current_line_len() + render_dict_inline(p, entries).len() > p.opts.max_line_len
}

fn should_break_tuple_expr(p: &Printer<'_>, elements: &[ExprId]) -> bool {
    p.current_line_len() + render_tuple_inline(p, elements).len() > p.opts.max_line_len
}

fn emit_wrapped_expr(p: &mut Printer<'_>, id: ExprId) {
    if let Some((root, segments)) = decompose_chain_expr(p, id) {
        emit_wrapped_chain_expr(p, root, &segments);
    } else {
        emit_expr(p, id);
    }
}

fn decompose_chain_expr(p: &Printer<'_>, id: ExprId) -> Option<(ExprId, Vec<ChainSegment>)> {
    let mut segments = Vec::new();
    let root = collect_chain_segments(p, id, &mut segments);
    if segments.is_empty() {
        None
    } else {
        Some((root, segments))
    }
}

fn collect_chain_segments(p: &Printer<'_>, id: ExprId, segments: &mut Vec<ChainSegment>) -> ExprId {
    match p.arena.get_expr(id) {
        Expr::Field { object, field, .. } => {
            let root = collect_chain_segments(p, *object, segments);
            segments.push(ChainSegment::Field(*field));
            root
        }
        Expr::Index { object, index, .. } => {
            let root = collect_chain_segments(p, *object, segments);
            segments.push(ChainSegment::Index(*index));
            root
        }
        Expr::Call { callee, args, .. } => {
            let root = collect_chain_segments(p, *callee, segments);
            segments.push(ChainSegment::Call(args.clone()));
            root
        }
        _ => id,
    }
}

fn emit_wrapped_chain_expr(p: &mut Printer<'_>, root: ExprId, segments: &[ChainSegment]) {
    emit_expr_prec(p, root, 15);
    emit_wrapped_chain_segments(p, segments);
}

fn emit_wrapped_chain_segments(p: &mut Printer<'_>, segments: &[ChainSegment]) {
    p.indent_in();
    for segment in segments {
        match segment {
            ChainSegment::Field(field) => {
                let field_text = format!(".{}", p.sym_s(*field));
                p.nl();
                p.w(&field_text);
            }
            ChainSegment::Index(index) => {
                p.nl();
                p.w("[");
                emit_expr_maybe_wrapped(p, *index);
                p.w("]");
            }
            ChainSegment::Call(args) => {
                p.w("(");
                if should_break_arg_list(p, args) {
                    emit_multiline_arg_list(p, args);
                } else {
                    emit_arg_list(p, args);
                }
                p.w(")");
            }
        }
    }
    p.indent_out();
}

fn root_fits_after_prefix(p: &Printer<'_>, root: ExprId) -> bool {
    p.current_line_len() + render_expr_fragment(p, root, 15).len() <= p.opts.max_line_len
}

fn should_break_arg_list(p: &Printer<'_>, args: &[Arg]) -> bool {
    if args.is_empty() {
        return false;
    }

    p.current_line_len() + render_arg_list_inline(p, args).len() > p.opts.max_line_len
}

fn emit_multiline_arg_list(p: &mut Printer<'_>, args: &[Arg]) {
    emit_multiline_arg_list_mode(p, args, LayoutMode::Normal);
}

fn emit_multiline_arg_list_mode(p: &mut Printer<'_>, args: &[Arg], mode: LayoutMode) {
    if args.is_empty() {
        return;
    }

    p.indent_in();
    for (i, arg) in args.iter().enumerate() {
        p.nl();
        if let Some(name) = arg.name {
            let n = p.sym_s(name);
            p.w(&n);
            p.w(" = ");
            if mode == LayoutMode::Normal {
                emit_expr_after_prefix(p, arg.value);
            } else {
                emit_expr_prec_mode(p, arg.value, 0, mode);
            }
        } else {
            if mode == LayoutMode::Normal {
                emit_expr_maybe_wrapped(p, arg.value);
            } else {
                emit_expr_prec_mode(p, arg.value, 0, mode);
            }
        }
        if i + 1 < args.len() || p.opts.trailing_comma {
            p.w(",");
        }
    }
    p.indent_out();
    p.nl();
}

fn render_expr_fragment(p: &Printer<'_>, id: ExprId, min_prec: u8) -> String {
    let mut scratch = p.scratch();
    emit_expr_prec_mode(&mut scratch, id, min_prec, LayoutMode::InlineOnly);
    scratch.into_string()
}

fn render_arg_list_inline(p: &Printer<'_>, args: &[Arg]) -> String {
    let mut scratch = p.scratch();
    emit_arg_list_mode(&mut scratch, args, LayoutMode::InlineOnly);
    scratch.into_string()
}

fn render_list_inline(p: &Printer<'_>, elements: &[ExprId]) -> String {
    let mut scratch = p.scratch();
    emit_list_expr(&mut scratch, elements, LayoutMode::InlineOnly);
    scratch.into_string()
}

fn render_dict_inline(p: &Printer<'_>, entries: &[(ExprId, ExprId)]) -> String {
    let mut scratch = p.scratch();
    emit_dict_expr(&mut scratch, entries, LayoutMode::InlineOnly);
    scratch.into_string()
}

fn render_tuple_inline(p: &Printer<'_>, elements: &[ExprId]) -> String {
    let mut scratch = p.scratch();
    emit_tuple_expr(&mut scratch, elements, LayoutMode::InlineOnly);
    scratch.into_string()
}

/// Escape a raw string value back into a Fidan `"..."` literal.
/// The returned string includes the surrounding double-quotes.
fn escape_str(s: &str) -> String {
    wrap_string_literal(&escape_str_inner(s))
}

fn escape_str_multiline(s: &str) -> String {
    wrap_string_literal(&escape_str_inner_multiline(s))
}

fn wrap_string_literal(contents: &str) -> String {
    let mut out = String::with_capacity(contents.len() + 2);
    out.push('"');
    out.push_str(contents);
    out.push('"');
    out
}

/// Escape a string fragment without surrounding quotes.
/// Used by both `StrLit` and the literal parts of `StringInterp`.
pub fn escape_str_inner(s: &str) -> String {
    escape_str_inner_with_newlines(s, false)
}

fn escape_str_inner_multiline(s: &str) -> String {
    escape_str_inner_with_newlines(s, true)
}

fn escape_str_inner_with_newlines(s: &str, preserve_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' if preserve_newlines => out.push('\n'),
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

fn render_string_interp(p: &Printer<'_>, parts: &[InterpPart], preserve_newlines: bool) -> String {
    let mut out = String::from("\"");
    for part in parts {
        match part {
            InterpPart::Literal(s) => {
                if preserve_newlines {
                    out.push_str(&escape_str_inner_multiline(s));
                } else {
                    out.push_str(&escape_str_inner(s));
                }
            }
            InterpPart::Expr(eid) => {
                out.push('{');
                out.push_str(&render_interp_expr_fragment(p, *eid));
                out.push('}');
            }
        }
    }
    out.push('"');
    out
}

fn should_preserve_multiline_string(p: &Printer<'_>, value: &str) -> bool {
    value.contains('\n') && p.current_line_len() + escape_str(value).len() > p.opts.max_line_len
}

fn should_preserve_multiline_interp(p: &Printer<'_>, parts: &[InterpPart]) -> bool {
    parts
        .iter()
        .any(|part| matches!(part, InterpPart::Literal(s) if s.contains('\n')))
        && p.current_line_len() + render_string_interp(p, parts, false).len() > p.opts.max_line_len
}

fn render_interp_expr_fragment(p: &Printer<'_>, id: ExprId) -> String {
    let mut scratch = p.scratch();
    emit_expr(&mut scratch, id);
    scratch.finish().trim_end_matches('\n').to_string()
}
