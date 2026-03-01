//! Human-readable AST pretty-printer.
//!
//! `print_module` walks the full tree and writes one line per node to stdout,
//! indented to reflect nesting.  Useful during development with `--emit ast`.

use crate::{AstArena, Expr, ExprId, Item, Module, Stmt, TypeExpr};
use fidan_lexer::SymbolInterner;

// ── Public entry ─────────────────────────────────────────────────────────────

pub fn print_module(module: &Module, interner: &SymbolInterner) {
    let p = Printer {
        arena: &module.arena,
        interner,
    };
    for &id in &module.items {
        let item = module.arena.get_item(id);
        p.print_item(item, 0);
    }
}

// ── Internals ─────────────────────────────────────────────────────────────────

struct Printer<'a> {
    arena: &'a AstArena,
    interner: &'a SymbolInterner,
}

impl<'a> Printer<'a> {
    fn sym(&self, s: fidan_lexer::Symbol) -> String {
        self.interner.resolve(s).to_string()
    }

    fn ty(&self, t: &TypeExpr) -> String {
        match t {
            TypeExpr::Named { name, .. } => self.sym(*name),
            TypeExpr::Oftype { base, param, .. } => {
                format!("{} oftype {}", self.ty(base), self.ty(param))
            }
            TypeExpr::Dynamic { .. } => "flexible".into(),
            TypeExpr::Nothing { .. } => "nothing".into(),
            TypeExpr::Tuple { elements, .. } => {
                if elements.is_empty() {
                    "tuple".into()
                } else {
                    let parts: Vec<String> = elements.iter().map(|e| self.ty(e)).collect();
                    format!("({})", parts.join(", "))
                }
            }
        }
    }

    fn expr_hint(&self, id: ExprId) -> String {
        let e = self.arena.get_expr(id);
        match e {
            Expr::IntLit { value, .. } => format!("{value}"),
            Expr::FloatLit { value, .. } => format!("{value}"),
            Expr::StrLit { value, .. } => format!("{value:?}"),
            Expr::BoolLit { value, .. } => format!("{value}"),
            Expr::Nothing { .. } => "nothing".into(),
            Expr::Ident { name, .. } => self.sym(*name),
            Expr::Call { callee, .. } => format!("{}(…)", self.expr_hint(*callee)),
            Expr::Field { object, field, .. } => {
                format!("{}.{}", self.expr_hint(*object), self.sym(*field))
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                format!("{} {:?} {}", self.expr_hint(*lhs), op, self.expr_hint(*rhs))
            }
            Expr::Unary { op, operand, .. } => format!("{:?}({})", op, self.expr_hint(*operand)),
            Expr::List { elements, .. } => format!("[…{}]", elements.len()),
            Expr::Dict { entries, .. } => format!("{{…{}}}", entries.len()),
            Expr::Tuple { elements, .. } => format!("(…{})", elements.len()),
            Expr::Ternary { .. } => "<ternary>".into(),
            Expr::Assign { .. } => "<assign>".into(),
            Expr::Spawn { .. } => "spawn …".into(),
            Expr::Await { .. } => "await …".into(),
            Expr::This { .. } => "this".into(),
            Expr::Parent { .. } => "parent".into(),
            Expr::StringInterp { .. } => "<interp-string>".into(),
            Expr::NullCoalesce { .. } => "<??> ".into(),
            Expr::CompoundAssign { .. } => "<compound-assign>".into(),
            Expr::Check { .. } => "<check-expr>".into(),
            Expr::Error { .. } => "<error>".into(),
            Expr::Index { object, index, .. } => {
                format!("{}[{}]", self.expr_hint(*object), self.expr_hint(*index))
            }
        }
    }

    fn pad(depth: usize) -> String {
        "  ".repeat(depth)
    }

    fn print_item(&self, item: &Item, depth: usize) {
        let p = Self::pad(depth);
        match item {
            Item::VarDecl { name, ty, init, is_const, .. } => {
                let kw = if *is_const { "const var" } else { "var" };
                let ty_s = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.ty(t)))
                    .unwrap_or_default();
                let ini_s = init
                    .map(|id| format!(" = {}", self.expr_hint(id)))
                    .unwrap_or_default();
                println!("{p}VarDecl({kw})  {}{ty_s}{ini_s}", self.sym(*name));
            }

            Item::ExprStmt(id) => {
                println!("{p}ExprStmt  {}", self.expr_hint(*id));
            }

            Item::Assign { target, value, .. } => {
                println!("{p}Assign  {} = {}", self.expr_hint(*target), self.expr_hint(*value));
            }
            Item::Destructure { bindings, value, .. } => {
                let names: Vec<String> = bindings.iter().map(|s| self.sym(*s)).collect();
                println!("{p}Destructure  ({}) = {}", names.join(", "), self.expr_hint(*value));
            }
            Item::Stmt(sid) => {
                self.print_stmt(*sid, depth);
            }
            Item::Use {
                path,
                alias,
                re_export,
                ..
            } => {
                let path_s = path
                    .iter()
                    .map(|s| self.sym(*s))
                    .collect::<Vec<_>>()
                    .join(".");
                let alias_s = alias
                    .map(|a| format!(" as {}", self.sym(a)))
                    .unwrap_or_default();
                let re_s = if *re_export { "export " } else { "" };
                println!("{p}Use  {re_s}{path_s}{alias_s}");
            }

            Item::ObjectDecl {
                name,
                parent,
                fields,
                methods,
                ..
            } => {
                let parent_s = parent
                    .map(|p| format!(" extends {}", self.sym(p)))
                    .unwrap_or_default();
                println!("{p}ObjectDecl  {}{parent_s}", self.sym(*name));
                for f in fields {
                    let req = if f.required { "  required" } else { "" };
                    println!("{p}  field  {}: {}{req}", self.sym(f.name), self.ty(&f.ty));
                }
                for &mid in methods {
                    let m = self.arena.get_item(mid);
                    self.print_item(m, depth + 1);
                }
            }

            Item::ActionDecl {
                name,
                params,
                return_ty,
                body,
                is_parallel,
                ..
            } => {
                let params_s = params
                    .iter()
                    .map(|p| {
                        let req = if p.required { "" } else { "?" };
                        format!("{}{}: {}", self.sym(p.name), req, self.ty(&p.ty))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret_s = return_ty
                    .as_ref()
                    .map(|t| format!(" -> {}", self.ty(t)))
                    .unwrap_or_default();
                let par_s = if *is_parallel { " [parallel]" } else { "" };
                println!(
                    "{p}ActionDecl  {}({params_s}){ret_s}{par_s}",
                    self.sym(*name)
                );
                for &sid in body {
                    self.print_stmt(sid, depth + 1);
                }
            }

            Item::ExtensionAction {
                name,
                extends,
                params,
                return_ty,
                body,
                is_parallel,
                ..
            } => {
                let params_s = params
                    .iter()
                    .map(|p| format!("{}: {}", self.sym(p.name), self.ty(&p.ty)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret_s = return_ty
                    .as_ref()
                    .map(|t| format!(" -> {}", self.ty(t)))
                    .unwrap_or_default();
                let par_s = if *is_parallel { " [parallel]" } else { "" };
                println!(
                    "{p}ExtensionAction  {} extends {}({params_s}){ret_s}{par_s}",
                    self.sym(*name),
                    self.sym(*extends)
                );
                for &sid in body {
                    self.print_stmt(sid, depth + 1);
                }
            }
        }
    }

    fn print_stmt(&self, sid: crate::StmtId, depth: usize) {
        let p = Self::pad(depth);
        let s = self.arena.get_stmt(sid);
        match s {
            Stmt::VarDecl { name, ty, init, is_const, .. } => {
                let kw = if *is_const { "const var" } else { "var" };
                let ty_s = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.ty(t)))
                    .unwrap_or_default();
                let ini_s = init
                    .map(|id| format!(" = {}", self.expr_hint(id)))
                    .unwrap_or_default();
                println!("{p}{kw}  {}{ty_s}{ini_s}", self.sym(*name));
            }
            Stmt::Assign { target, value, .. } => println!(
                "{p}assign  {} = {}",
                self.expr_hint(*target),
                self.expr_hint(*value)
            ),
            Stmt::Destructure { bindings, value, .. } => {
                let names: Vec<String> = bindings.iter().map(|s| self.sym(*s)).collect();
                println!("{p}destructure  ({}) = {}", names.join(", "), self.expr_hint(*value));
            }
            Stmt::Expr { expr, .. } => println!("{p}expr  {}", self.expr_hint(*expr)),
            Stmt::Return { value, .. } => {
                let v = value
                    .map(|id| format!(" {}", self.expr_hint(id)))
                    .unwrap_or_default();
                println!("{p}return{v}");
            }
            Stmt::Break { .. } => println!("{p}break"),
            Stmt::Continue { .. } => println!("{p}continue"),
            Stmt::Panic { value, .. } => println!("{p}panic  {}", self.expr_hint(*value)),
            Stmt::Error { .. } => println!("{p}<parse-error>"),
            Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                println!("{p}if  {}", self.expr_hint(*condition));
                for &s in then_body {
                    self.print_stmt(s, depth + 1);
                }
                for ei in else_ifs {
                    println!("{p}else-if  {}", self.expr_hint(ei.condition));
                    for &s in &ei.body {
                        self.print_stmt(s, depth + 1);
                    }
                }
                if let Some(eb) = else_body {
                    println!("{p}else");
                    for &s in eb {
                        self.print_stmt(s, depth + 1);
                    }
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                println!("{p}while  {}", self.expr_hint(*condition));
                for &s in body {
                    self.print_stmt(s, depth + 1);
                }
            }
            Stmt::For {
                binding,
                iterable,
                body,
                ..
            } => {
                println!(
                    "{p}for  {} in {}",
                    self.sym(*binding),
                    self.expr_hint(*iterable)
                );
                for &s in body {
                    self.print_stmt(s, depth + 1);
                }
            }
            Stmt::ParallelFor {
                binding,
                iterable,
                body,
                ..
            } => {
                println!(
                    "{p}parallel-for  {} in {}",
                    self.sym(*binding),
                    self.expr_hint(*iterable)
                );
                for &s in body {
                    self.print_stmt(s, depth + 1);
                }
            }
            Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                println!("{p}attempt");
                for &s in body {
                    self.print_stmt(s, depth + 1);
                }
                for c in catches {
                    let bind = c
                        .binding
                        .map(|b| format!(" {}", self.sym(b)))
                        .unwrap_or_default();
                    println!("{p}catch{bind}");
                    for &s in &c.body {
                        self.print_stmt(s, depth + 1);
                    }
                }
                if let Some(ow) = otherwise {
                    println!("{p}otherwise");
                    for &s in ow {
                        self.print_stmt(s, depth + 1);
                    }
                }
                if let Some(fin) = finally {
                    println!("{p}finally");
                    for &s in fin {
                        self.print_stmt(s, depth + 1);
                    }
                }
            }
            Stmt::ConcurrentBlock {
                is_parallel, tasks, ..
            } => {
                let kw = if *is_parallel {
                    "parallel"
                } else {
                    "concurrent"
                };
                println!("{p}{kw}-block");
                for t in tasks {
                    let name = t
                        .name
                        .map(|n| format!(" task {}", self.sym(n)))
                        .unwrap_or_default();
                    println!("{p}  task{name}");
                    for &s in &t.body {
                        self.print_stmt(s, depth + 2);
                    }
                }
            }
            Stmt::Check {
                scrutinee, arms, ..
            } => {
                println!("{p}check  {}", self.expr_hint(*scrutinee));
                for arm in arms {
                    println!("{p}  arm  {}", self.expr_hint(arm.pattern));
                    for &s in &arm.body {
                        self.print_stmt(s, depth + 2);
                    }
                }
            }
        }
    }
}
