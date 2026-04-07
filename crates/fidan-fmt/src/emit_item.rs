//! Top-level item emitter.

use crate::emit_expr::{
    binop_str, emit_expr, emit_expr_after_prefix, emit_expr_maybe_wrapped, emit_type,
};
use crate::emit_stmt::{compound_assign_parts, emit_block, emit_stmt};
use crate::printer::Printer;
use fidan_ast::{Decorator, Item, ItemId, Module, Param};
use fidan_lexer::Symbol;
use fidan_source::Span;

fn has_extern_decorator(p: &Printer<'_>, decorators: &[fidan_ast::Decorator]) -> bool {
    decorators
        .iter()
        .any(|decorator| p.sym_s(decorator.name).as_ref() == "extern")
}

pub(crate) fn emit_decorators(p: &mut Printer<'_>, decorators: &[Decorator]) {
    for dec in decorators {
        p.w("@");
        let dn = p.sym_s(dec.name);
        p.w(&dn);
        if !dec.args.is_empty() {
            p.w("(");
            for (i, arg) in dec.args.iter().enumerate() {
                if i > 0 {
                    p.w(", ");
                }
                if let Some(name) = arg.name {
                    let n = p.sym_s(name);
                    p.w(&n);
                    p.w(" = ");
                }
                emit_expr(p, arg.value);
            }
            p.w(")");
        }
        p.nl();
    }
}

// ── Top-level module ──────────────────────────────────────────────────────────

/// Returns `true` for items that warrant surrounding blank lines (actions,
/// objects, tests).  Simple items (var, use, assignments, expression
/// statements) are grouped together without extra spacing.
fn is_block_item(item: &Item) -> bool {
    matches!(
        item,
        Item::ActionDecl { .. }
            | Item::ExtensionAction { .. }
            | Item::ObjectDecl { .. }
            | Item::TestDecl { .. }
            | Item::EnumDecl { .. }
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemGroup {
    Import,
    Declaration,
    Other,
    Block,
}

fn item_group(item: &Item) -> ItemGroup {
    if is_block_item(item) {
        return ItemGroup::Block;
    }

    match item {
        Item::Use { .. } => ItemGroup::Import,
        Item::VarDecl { .. } | Item::Destructure { .. } => ItemGroup::Declaration,
        Item::ExprStmt(_) | Item::Assign { .. } | Item::Stmt(_) => ItemGroup::Other,
        Item::ObjectDecl { .. }
        | Item::ActionDecl { .. }
        | Item::ExtensionAction { .. }
        | Item::TestDecl { .. }
        | Item::EnumDecl { .. } => ItemGroup::Block,
    }
}

/// Emit all items in a `Module`, inserting blank lines only around block-level
/// items (actions, objects, tests).  Consecutive simple items (var, use,
/// assignments) are emitted without blank lines between them.
pub fn emit_module(p: &mut Printer<'_>, module: &Module) {
    let items: Vec<ItemId> = module.items.clone();
    if let Some(&first) = items.first() {
        let span = item_span(p, module.arena.get_item(first));
        p.emit_comments_before(span.start);
        if !p.is_empty() && item_group(module.arena.get_item(first)) == ItemGroup::Import {
            p.blank();
        }
    } else {
        p.emit_remaining_comments();
        return;
    }
    let mut i = 0usize;
    while i < items.len() {
        let iid = items[i];
        if i > 0 {
            let prev_group = item_group(module.arena.get_item(items[i - 1]));
            let curr_group = item_group(module.arena.get_item(iid));
            if prev_group == ItemGroup::Block
                || curr_group == ItemGroup::Block
                || prev_group != curr_group
            {
                // `blank()` ends the current line then adds the blank separator.
                for _ in 0..p.opts.blank_lines_between_items {
                    p.blank();
                }
            } else {
                // Simple items are separated by a single newline only.
                p.nl();
            }
        }
        let item = module.arena.get_item(iid).clone();
        if let Some((re_export, prefix, first_name)) = grouped_use_parts(&item) {
            let mut names = vec![first_name];
            let mut last_end = item_span(p, &item).end;
            let mut j = i + 1;
            while j < items.len() {
                let next = module.arena.get_item(items[j]);
                if item_group(next) != ItemGroup::Import {
                    break;
                }
                let Some((next_re_export, next_prefix, next_name)) = grouped_use_parts(next) else {
                    break;
                };
                if next_re_export != re_export || next_prefix != prefix {
                    break;
                }
                names.push(next_name);
                last_end = item_span(p, next).end;
                j += 1;
            }
            emit_grouped_use_cluster(p, re_export, &prefix, &names);
            p.emit_trailing_comments_for(last_end);
            i = j;
            continue;
        }
        emit_item(p, &item, false);
        i += 1;
    }
    p.emit_remaining_comments();
    // Final newline is handled by Printer::finish().
}

fn grouped_use_parts(item: &Item) -> Option<(bool, Vec<Symbol>, Symbol)> {
    match item {
        Item::Use {
            path,
            alias,
            re_export,
            grouped,
            ..
        } if *grouped && alias.is_none() && !path.is_empty() => {
            let (last, prefix) = path.split_last().unwrap();
            Some((*re_export, prefix.to_vec(), *last))
        }
        _ => None,
    }
}

fn emit_grouped_use_cluster(
    p: &mut Printer<'_>,
    re_export: bool,
    prefix: &[Symbol],
    names: &[Symbol],
) {
    if re_export {
        p.w("export ");
    }
    p.w("use ");
    for (idx, seg) in prefix.iter().enumerate() {
        if idx > 0 {
            p.w(".");
        }
        let s = p.sym_s(*seg);
        p.w(&s);
    }
    if !prefix.is_empty() {
        p.w(".");
    }
    p.w("{");
    for (idx, name) in names.iter().enumerate() {
        if idx > 0 {
            p.w(", ");
        }
        let s = p.sym_s(*name);
        p.w(&s);
    }
    p.w("}");
}

// ── Item dispatcher ───────────────────────────────────────────────────────────

/// Emit a single item.
///
/// `inside_object` is `true` when the item is a method inside an `object`
/// body — this changes `action` emission (no leading blank line, `new`
/// constructors use the `new` keyword).
pub fn emit_item(p: &mut Printer<'_>, item: &Item, inside_object: bool) {
    let item_span = item_span(p, item);
    p.emit_comments_before(item_span.start);

    match item {
        // ── Module-level var / const var ──────────────────────────────────
        Item::VarDecl {
            name,
            ty,
            init,
            is_const,
            ..
        } => {
            if *is_const {
                p.w("const var ");
            } else {
                p.w("var ");
            }
            let n = p.sym_s(*name);
            p.w(&n);
            if let Some(t) = ty {
                p.w(" oftype ");
                emit_type(p, t);
            }
            if let Some(eid) = init {
                p.w(" = ");
                emit_expr_after_prefix(p, *eid);
            }
        }

        // ── Module-level expression statement ────────────────────────────
        Item::ExprStmt(eid) => {
            emit_expr_maybe_wrapped(p, *eid);
        }

        // ── Module-level assignment ───────────────────────────────────────
        Item::Assign { target, value, .. } => {
            if let Some((op, rhs)) = compound_assign_parts(p, *target, *value) {
                emit_expr(p, *target);
                p.w(" ");
                p.w(binop_str(op));
                p.w("= ");
                emit_expr_after_prefix(p, rhs);
            } else {
                emit_expr(p, *target);
                p.w(" = ");
                emit_expr_after_prefix(p, *value);
            }
        }

        // ── Module-level tuple destructure ────────────────────────────────
        Item::Destructure {
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
            emit_expr_after_prefix(p, *value);
        }

        // ── Use / export use ─────────────────────────────────────────────
        Item::Use {
            path,
            alias,
            re_export,
            grouped,
            ..
        } => {
            if *re_export {
                p.w("export ");
            }
            p.w("use ");
            if *grouped && !path.is_empty() {
                // `use mod.{name}` — last segment is inside braces
                let (last, prefix) = path.split_last().unwrap();
                if !prefix.is_empty() {
                    for (i, seg) in prefix.iter().enumerate() {
                        if i > 0 {
                            p.w(".");
                        }
                        let s = p.sym_s(*seg);
                        p.w(&s);
                    }
                    p.w(".");
                }
                p.w("{");
                let last_s = p.sym_s(*last);
                p.w(&last_s);
                p.w("}");
            } else {
                // File-path imports store the raw path string (without quotes) as a
                // single symbol, e.g. `use "test.fdn"` is interned as `test.fdn`.
                // Detect them and re-wrap in double-quotes on output.
                let is_file_path = path.len() == 1 && {
                    let s = p.sym_s(path[0]);
                    s.ends_with(".fdn")
                        || s.starts_with("./")
                        || s.starts_with("../")
                        || s.starts_with('/')
                };
                if is_file_path {
                    let s = p.sym_s(path[0]);
                    p.w("\"");
                    p.w(&s);
                    p.w("\"");
                } else {
                    for (i, seg) in path.iter().enumerate() {
                        if i > 0 {
                            p.w(".");
                        }
                        let s = p.sym_s(*seg);
                        p.w(&s);
                    }
                }
            }
            if let Some(a) = alias {
                p.w(" as ");
                let a_s = p.sym_s(*a);
                p.w(&a_s);
            }
        }

        // ── Object declaration ────────────────────────────────────────────
        Item::ObjectDecl {
            name,
            parent,
            fields,
            methods,
            span,
        } => {
            p.w("object ");
            let n = p.sym_s(*name);
            p.w(&n);
            if let Some(parts) = parent {
                p.w(" extends ");
                for (i, &seg) in parts.iter().enumerate() {
                    if i > 0 {
                        p.w(".");
                    }
                    let s = p.sym_s(seg);
                    p.w(&s);
                }
            }
            p.w(" {");

            let has_content = !fields.is_empty() || !methods.is_empty();
            if has_content {
                p.indent_in();

                // Fields
                for field in fields {
                    p.emit_comments_before(field.span.start);
                    p.nl();
                    if field.certain {
                        p.w("certain ");
                    }
                    p.w("var ");
                    let fn_ = p.sym_s(field.name);
                    p.w(&fn_);
                    p.w(" oftype ");
                    emit_type(p, &field.ty);
                    if let Some(default) = field.default {
                        p.w(" = ");
                        emit_expr_after_prefix(p, default);
                    }
                    p.emit_trailing_comments_for(field.span.end);
                }

                // Methods (separated by a blank line from fields if any exist)
                let methods_vec: Vec<ItemId> = methods.clone();
                for (i, &mid) in methods_vec.iter().enumerate() {
                    if i > 0 {
                        // blank line between consecutive methods
                        p.blank();
                    } else if !fields.is_empty() {
                        // one blank line separating the field block from the first method
                        p.blank();
                    } else {
                        // no fields — just a plain newline before the first method
                        p.nl();
                    }
                    let method = p.arena.get_item(mid).clone();
                    emit_item(p, &method, true);
                }

                p.emit_comments_before(span.end);
                p.indent_out();
            }
            p.nl();
            p.w("}");
        }

        // ── Action declaration ────────────────────────────────────────────
        Item::ActionDecl {
            name,
            params,
            return_ty,
            body,
            decorators,
            is_parallel,
            span,
        } => {
            emit_decorators(p, decorators);

            // `parallel action` vs `action` vs `new`
            let name_str = p.sym_s(*name);
            let is_constructor = inside_object && &*name_str == "new";

            if is_constructor {
                p.w("new");
            } else {
                if *is_parallel {
                    p.w("parallel action ");
                } else {
                    p.w("action ");
                }
                p.w(&name_str);
            }

            // Parameters
            if !params.is_empty() {
                emit_params_clause(p, " with ", params);
            }

            // Return type
            if let Some(rt) = return_ty {
                p.w(" returns ");
                emit_type(p, rt);
            }

            if has_extern_decorator(p, decorators) && body.is_empty() {
                p.emit_trailing_comments_for(span.end);
                return;
            }

            p.w(" {");
            emit_block(p, body, Some(span.end));
            p.w("}");
        }

        // ── Extension action ──────────────────────────────────────────────
        Item::ExtensionAction {
            name,
            extends,
            params,
            return_ty,
            body,
            decorators,
            is_parallel,
            span,
        } => {
            emit_decorators(p, decorators);

            if *is_parallel {
                p.w("parallel action ");
            } else {
                p.w("action ");
            }
            let n = p.sym_s(*name);
            p.w(&n);
            p.w(" extends ");
            let ext = p.sym_s(*extends);
            p.w(&ext);
            if !params.is_empty() {
                emit_params_clause(p, " with ", params);
            }
            if let Some(rt) = return_ty {
                p.w(" returns ");
                emit_type(p, rt);
            }
            p.w(" {");
            emit_block(p, body, Some(span.end));
            p.w("}");
        }

        // ── Wrapped statement ─────────────────────────────────────────────
        Item::Stmt(sid) => {
            emit_stmt(p, *sid);
            return;
        }

        // ── Test block ────────────────────────────────────────────────────
        Item::TestDecl {
            name, body, span, ..
        } => {
            p.w("test ");
            p.w(&escape_string_lit(name));
            p.w(" {");
            emit_block(p, body, Some(span.end));
            p.w("}");
        }

        // ── Enum declaration ──────────────────────────────────────────────
        Item::EnumDecl { name, variants, .. } => {
            p.w("enum ");
            let n = p.sym_s(*name);
            p.w(&n);
            p.w(" {");
            p.indent_in();
            for v in variants {
                p.nl();
                let vs = p.sym_s(v.name);
                p.w(&vs);
                if !v.payload_types.is_empty() {
                    p.w("(");
                    for (i, payload_ty) in v.payload_types.iter().enumerate() {
                        if i > 0 {
                            p.w(", ");
                        }
                        emit_type(p, payload_ty);
                    }
                    p.w(")");
                }
            }
            p.indent_out();
            p.nl();
            p.w("}");
        }
    }

    p.emit_trailing_comments_for(item_span.end);
}

// ── Parameter list ────────────────────────────────────────────────────────────

fn emit_params(p: &mut Printer<'_>, params: &[Param]) {
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            p.w(", ");
        }
        if param.certain {
            p.w("certain ");
        } else if param.optional {
            p.w("optional ");
        }
        // fall-through: no qualifier keyword (accept inferred as certain)

        let pn = p.sym_s(param.name);
        p.w(&pn);
        p.w(" oftype ");
        emit_type(p, &param.ty);

        if let Some(default) = param.default {
            p.w(" = ");
            emit_expr_after_prefix(p, default);
        }
    }
}

fn emit_params_clause(p: &mut Printer<'_>, prefix: &str, params: &[Param]) {
    p.w(prefix);
    if should_break_params(p, params, prefix.len()) {
        p.w("(");
        p.indent_in();
        for (i, param) in params.iter().enumerate() {
            p.nl();
            emit_single_param(p, param);
            if i + 1 < params.len() || p.opts.trailing_comma {
                p.w(",");
            }
        }
        p.indent_out();
        p.nl();
        p.w(")");
    } else {
        p.w("(");
        emit_params(p, params);
        p.w(")");
    }
}

fn emit_single_param(p: &mut Printer<'_>, param: &Param) {
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
        emit_expr_after_prefix(p, default);
    }
}

fn should_break_params(p: &Printer<'_>, params: &[Param], prefix_len: usize) -> bool {
    if params.is_empty() {
        return false;
    }

    let estimated = prefix_len + 2 + estimated_param_list_len(p, params);
    estimated > p.opts.max_line_len
}

fn estimated_param_list_len(p: &Printer<'_>, params: &[Param]) -> usize {
    params
        .iter()
        .enumerate()
        .map(|(i, param)| estimated_param_len(p, param) + if i > 0 { 2 } else { 0 })
        .sum()
}

fn estimated_param_len(p: &Printer<'_>, param: &Param) -> usize {
    let qualifier_len = if param.certain {
        "certain ".len()
    } else if param.optional {
        "optional ".len()
    } else {
        0
    };
    let name_len = p.sym_s(param.name).len();
    let type_len = estimated_type_len(p, &param.ty);
    let default_len = param
        .default
        .map(|expr| 3 + estimated_expr_len(p, expr))
        .unwrap_or(0);
    qualifier_len + name_len + " oftype ".len() + type_len + default_len
}

fn estimated_type_len(p: &Printer<'_>, ty: &fidan_ast::TypeExpr) -> usize {
    match ty {
        fidan_ast::TypeExpr::Named { name, .. } => p.sym_s(*name).len(),
        fidan_ast::TypeExpr::Oftype { base, param, .. } => {
            estimated_type_len(p, base) + " oftype ".len() + estimated_type_len(p, param)
        }
        fidan_ast::TypeExpr::Tuple { elements, .. } => {
            if elements.is_empty() {
                "tuple".len()
            } else {
                2 + elements
                    .iter()
                    .enumerate()
                    .map(|(i, elem)| estimated_type_len(p, elem) + if i > 0 { 2 } else { 0 })
                    .sum::<usize>()
            }
        }
        fidan_ast::TypeExpr::Dynamic { .. } => "dynamic".len(),
        fidan_ast::TypeExpr::Nothing { .. } => "nothing".len(),
    }
}

fn estimated_expr_len(p: &Printer<'_>, expr: fidan_ast::ExprId) -> usize {
    match p.arena.get_expr(expr) {
        fidan_ast::Expr::IntLit { value, .. } => value.to_string().len(),
        fidan_ast::Expr::FloatLit { value, .. } => {
            if value.fract() == 0.0 {
                format!("{value:.1}").len()
            } else {
                format!("{value}").len()
            }
        }
        fidan_ast::Expr::StrLit { value, .. } => escape_string_lit(value).len(),
        fidan_ast::Expr::BoolLit { value, .. } => {
            if *value {
                4
            } else {
                5
            }
        }
        fidan_ast::Expr::Nothing { .. } => "nothing".len(),
        fidan_ast::Expr::Ident { name, .. } => p.sym_s(*name).len(),
        _ => p.opts.max_line_len,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Escape a plain Rust string back into a Fidan string literal with quotes.
fn escape_string_lit(s: &str) -> String {
    use crate::emit_expr::escape_str_inner;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&escape_str_inner(s));
    out.push('"');
    out
}

fn item_span(p: &Printer<'_>, item: &Item) -> Span {
    match item {
        Item::VarDecl { span, .. } => *span,
        Item::ExprStmt(expr) => p.arena.get_expr(*expr).span(),
        Item::Assign { span, .. } => *span,
        Item::Destructure { span, .. } => *span,
        Item::ObjectDecl { span, .. } => *span,
        Item::ActionDecl { span, .. } => *span,
        Item::ExtensionAction { span, .. } => *span,
        Item::Use { span, .. } => *span,
        Item::Stmt(stmt) => stmt_span(p.arena.get_stmt(*stmt)),
        Item::TestDecl { span, .. } => *span,
        Item::EnumDecl { span, .. } => *span,
    }
}

fn stmt_span(stmt: &fidan_ast::Stmt) -> Span {
    match stmt {
        fidan_ast::Stmt::VarDecl { span, .. } => *span,
        fidan_ast::Stmt::Destructure { span, .. } => *span,
        fidan_ast::Stmt::Assign { span, .. } => *span,
        fidan_ast::Stmt::Expr { span, .. } => *span,
        fidan_ast::Stmt::ActionDecl { span, .. } => *span,
        fidan_ast::Stmt::Return { span, .. } => *span,
        fidan_ast::Stmt::Break { span } => *span,
        fidan_ast::Stmt::Continue { span } => *span,
        fidan_ast::Stmt::If { span, .. } => *span,
        fidan_ast::Stmt::Check { span, .. } => *span,
        fidan_ast::Stmt::For { span, .. } => *span,
        fidan_ast::Stmt::While { span, .. } => *span,
        fidan_ast::Stmt::Attempt { span, .. } => *span,
        fidan_ast::Stmt::ParallelFor { span, .. } => *span,
        fidan_ast::Stmt::ConcurrentBlock { span, .. } => *span,
        fidan_ast::Stmt::Panic { span, .. } => *span,
        fidan_ast::Stmt::Error { span } => *span,
    }
}
