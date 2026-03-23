//! Top-level item emitter.

use crate::emit_expr::{emit_expr, emit_type};
use crate::emit_stmt::{emit_block, emit_stmt};
use crate::printer::Printer;
use fidan_ast::{Item, ItemId, Module, Param};

fn has_extern_decorator(p: &Printer<'_>, decorators: &[fidan_ast::Decorator]) -> bool {
    decorators
        .iter()
        .any(|decorator| p.sym_s(decorator.name).as_ref() == "extern")
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
    )
}

/// Emit all items in a `Module`, inserting blank lines only around block-level
/// items (actions, objects, tests).  Consecutive simple items (var, use,
/// assignments) are emitted without blank lines between them.
pub fn emit_module(p: &mut Printer<'_>, module: &Module) {
    let items: Vec<ItemId> = module.items.clone();
    for (i, &iid) in items.iter().enumerate() {
        if i > 0 {
            let prev_is_block = is_block_item(module.arena.get_item(items[i - 1]));
            let curr_is_block = is_block_item(module.arena.get_item(iid));
            if prev_is_block || curr_is_block {
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
        emit_item(p, &item, false);
    }
    // Final newline is handled by Printer::finish().
}

// ── Item dispatcher ───────────────────────────────────────────────────────────

/// Emit a single item.
///
/// `inside_object` is `true` when the item is a method inside an `object`
/// body — this changes `action` emission (no leading blank line, `new`
/// constructors use the `new` keyword).
pub fn emit_item(p: &mut Printer<'_>, item: &Item, inside_object: bool) {
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
                emit_expr(p, *eid);
            }
        }

        // ── Module-level expression statement ────────────────────────────
        Item::ExprStmt(eid) => {
            emit_expr(p, *eid);
        }

        // ── Module-level assignment ───────────────────────────────────────
        Item::Assign { target, value, .. } => {
            emit_expr(p, *target);
            p.w(" = ");
            emit_expr(p, *value);
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
            emit_expr(p, *value);
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
            ..
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
                        emit_expr(p, default);
                    }
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
            ..
        } => {
            // Decorators
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
                p.w(" with (");
                emit_params(p, params);
                p.w(")");
            }

            // Return type
            if let Some(rt) = return_ty {
                p.w(" returns ");
                emit_type(p, rt);
            }

            if has_extern_decorator(p, decorators) && body.is_empty() {
                return;
            }

            p.w(" {");
            emit_block(p, body);
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
            ..
        } => {
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
                p.w(" with (");
                emit_params(p, params);
                p.w(")");
            }
            if let Some(rt) = return_ty {
                p.w(" returns ");
                emit_type(p, rt);
            }
            p.w(" {");
            emit_block(p, body);
            p.w("}");
        }

        // ── Wrapped statement ─────────────────────────────────────────────
        Item::Stmt(sid) => {
            emit_stmt(p, *sid);
        }

        // ── Test block ────────────────────────────────────────────────────
        Item::TestDecl { name, body, .. } => {
            p.w("test ");
            p.w(&escape_string_lit(name));
            p.w(" {");
            emit_block(p, body);
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
                    p.w(&format!("({})", v.payload_types.len()));
                }
            }
            p.indent_out();
            p.nl();
            p.w("}");
        }
    }
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
            emit_expr(p, default);
        }
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
