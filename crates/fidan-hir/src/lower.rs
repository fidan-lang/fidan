// fidan-hir/src/lower.rs
//
// AST → HIR lowering.
//
// Walks the flat, arena-based AST produced by `fidan-parser` together with
// the type-annotation map produced by `fidan-typeck` and emits an owned,
// fully-typed HIR tree.

use fidan_ast::{Arg, AstArena, Expr, ExprId, InterpPart, Item, Module, Param, Stmt, StmtId};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::Span;
use fidan_typeck::{FidanType, TypedModule};

use crate::hir::{
    HirArg, HirCatchClause, HirCheckArm, HirCheckExprArm, HirElseIf, HirExpr, HirExprKind,
    HirField, HirFunction, HirGlobal, HirInterpPart, HirModule, HirObject, HirParam, HirStmt,
    HirTask, HirUseDecl,
};

// ── Context ────────────────────────────────────────────────────────────────────

struct Ctx<'a> {
    arena: &'a AstArena,
    typed: &'a TypedModule,
    interner: &'a SymbolInterner,
}

impl<'a> Ctx<'a> {
    /// Look up the inferred type of an expression.
    fn ty(&self, id: ExprId) -> FidanType {
        self.typed
            .expr_types
            .get(&id)
            .cloned()
            .unwrap_or(FidanType::Error)
    }

    // ── Expression lowering ──────────────────────────────────────────────────

    fn lower_expr(&self, id: ExprId) -> HirExpr {
        let expr = self.arena.get_expr(id).clone();
        let ty = self.ty(id);
        let span = expr.span();

        let kind = match expr {
            Expr::IntLit { value, .. } => HirExprKind::IntLit(value),
            Expr::FloatLit { value, .. } => HirExprKind::FloatLit(value),
            Expr::StrLit { value, .. } => HirExprKind::StrLit(value),
            Expr::BoolLit { value, .. } => HirExprKind::BoolLit(value),
            Expr::Nothing { .. } => HirExprKind::Nothing,

            Expr::Ident { name, .. } => HirExprKind::Var(name),
            Expr::This { .. } => HirExprKind::This,
            Expr::Parent { .. } => HirExprKind::Parent,

            Expr::Binary { op, lhs, rhs, .. } => HirExprKind::Binary {
                op,
                lhs: Box::new(self.lower_expr(lhs)),
                rhs: Box::new(self.lower_expr(rhs)),
            },
            Expr::Unary { op, operand, .. } => HirExprKind::Unary {
                op,
                operand: Box::new(self.lower_expr(operand)),
            },
            Expr::NullCoalesce { lhs, rhs, .. } => HirExprKind::NullCoalesce {
                lhs: Box::new(self.lower_expr(lhs)),
                rhs: Box::new(self.lower_expr(rhs)),
            },

            // Desugar ternary: `then_val if condition else else_val`
            Expr::Ternary {
                condition,
                then_val,
                else_val,
                ..
            } => HirExprKind::IfExpr {
                condition: Box::new(self.lower_expr(condition)),
                then_val: Box::new(self.lower_expr(then_val)),
                else_val: Box::new(self.lower_expr(else_val)),
            },

            Expr::Call { callee, args, .. } => HirExprKind::Call {
                callee: Box::new(self.lower_expr(callee)),
                args: args.iter().map(|a| self.lower_arg(a)).collect(),
            },
            Expr::Field { object, field, .. } => HirExprKind::Field {
                object: Box::new(self.lower_expr(object)),
                field,
            },
            Expr::Index { object, index, .. } => HirExprKind::Index {
                object: Box::new(self.lower_expr(object)),
                index: Box::new(self.lower_expr(index)),
            },

            Expr::List { elements, .. } => {
                HirExprKind::List(elements.iter().map(|&e| self.lower_expr(e)).collect())
            }
            Expr::Dict { entries, .. } => HirExprKind::Dict(
                entries
                    .iter()
                    .map(|&(k, v)| (self.lower_expr(k), self.lower_expr(v)))
                    .collect(),
            ),
            Expr::Tuple { elements, .. } => {
                HirExprKind::Tuple(elements.iter().map(|&e| self.lower_expr(e)).collect())
            }

            Expr::StringInterp { parts, .. } => {
                HirExprKind::StringInterp(parts.iter().map(|p| self.lower_interp_part(p)).collect())
            }

            Expr::Spawn { expr, .. } => HirExprKind::Spawn(Box::new(self.lower_expr(expr))),
            Expr::Await { expr, .. } => HirExprKind::Await(Box::new(self.lower_expr(expr))),

            // Assignments as expressions (compound assign / plain assign)
            // In HIR we keep them as expressions; MIR lowering will handle them.
            Expr::Assign { target, value, .. } => HirExprKind::Assign {
                target: Box::new(self.lower_expr(target)),
                value: Box::new(self.lower_expr(value)),
            },
            Expr::CompoundAssign {
                op, target, value, ..
            } => HirExprKind::Binary {
                op,
                lhs: Box::new(self.lower_expr(target)),
                rhs: Box::new(self.lower_expr(value)),
            },

            Expr::Check {
                scrutinee, arms, ..
            } => HirExprKind::CheckExpr {
                scrutinee: Box::new(self.lower_expr(scrutinee)),
                arms: arms
                    .iter()
                    .map(|arm| HirCheckExprArm {
                        pattern: self.lower_expr(arm.pattern),
                        body: self.lower_stmts(&arm.body),
                        span: arm.span,
                    })
                    .collect(),
            },

            Expr::Error { .. } => HirExprKind::Error,
        };

        HirExpr { kind, ty, span }
    }

    fn lower_arg(&self, arg: &Arg) -> HirArg {
        HirArg {
            name: arg.name,
            value: self.lower_expr(arg.value),
            span: arg.span,
        }
    }

    fn lower_interp_part(&self, part: &InterpPart) -> HirInterpPart {
        match part {
            InterpPart::Literal(s) => HirInterpPart::Literal(s.clone()),
            InterpPart::Expr(id) => HirInterpPart::Expr(self.lower_expr(*id)),
        }
    }

    // ── Statement lowering ────────────────────────────────────────────────────

    fn lower_stmts(&self, ids: &[StmtId]) -> Vec<HirStmt> {
        ids.iter().map(|&id| self.lower_stmt(id)).collect()
    }

    fn lower_stmt(&self, id: StmtId) -> HirStmt {
        match self.arena.get_stmt(id).clone() {
            Stmt::VarDecl {
                name,
                ty: _,
                init,
                is_const,
                span,
            } => {
                // The resolved type of a `var` declaration is the type of the
                // initialiser expression, or FidanType::Nothing if no init.
                let ty = init.map(|e| self.ty(e)).unwrap_or(FidanType::Nothing);
                HirStmt::VarDecl {
                    name,
                    ty,
                    init: init.map(|e| self.lower_expr(e)),
                    is_const,
                    span,
                }
            }

            Stmt::Destructure {
                bindings,
                value,
                span,
            } => {
                let value_ty = self.ty(value);
                // Each binding type is the corresponding element of the tuple.
                let binding_tys = match &value_ty {
                    FidanType::Tuple(elems) => elems.clone(),
                    _ => vec![FidanType::Dynamic; bindings.len()],
                };
                HirStmt::Destructure {
                    bindings,
                    binding_tys,
                    value: self.lower_expr(value),
                    span,
                }
            }

            Stmt::Assign {
                target,
                value,
                span,
            } => HirStmt::Assign {
                target: self.lower_expr(target),
                value: self.lower_expr(value),
                span,
            },

            Stmt::Expr { expr, .. } => HirStmt::Expr(self.lower_expr(expr)),

            Stmt::Return { value, span } => HirStmt::Return {
                value: value.map(|e| self.lower_expr(e)),
                span,
            },

            Stmt::Break { span } => HirStmt::Break { span },
            Stmt::Continue { span } => HirStmt::Continue { span },

            Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                span,
            } => HirStmt::If {
                condition: self.lower_expr(condition),
                then_body: self.lower_stmts(&then_body),
                else_ifs: else_ifs
                    .iter()
                    .map(|ei| HirElseIf {
                        condition: self.lower_expr(ei.condition),
                        body: self.lower_stmts(&ei.body),
                        span: ei.span,
                    })
                    .collect(),
                else_body: else_body.map(|b| self.lower_stmts(&b)),
                span,
            },

            Stmt::Check {
                scrutinee,
                arms,
                span,
            } => HirStmt::Check {
                scrutinee: self.lower_expr(scrutinee),
                arms: arms
                    .iter()
                    .map(|arm| HirCheckArm {
                        pattern: self.lower_expr(arm.pattern),
                        body: self.lower_stmts(&arm.body),
                        span: arm.span,
                    })
                    .collect(),
                span,
            },

            Stmt::For {
                binding,
                iterable,
                body,
                span,
            } => {
                // Infer element type from the iterable.
                let iter_ty = self.ty(iterable);
                let binding_ty = match iter_ty {
                    FidanType::List(elem) => *elem,
                    _ => FidanType::Dynamic,
                };
                HirStmt::For {
                    binding,
                    binding_ty,
                    iterable: self.lower_expr(iterable),
                    body: self.lower_stmts(&body),
                    span,
                }
            }

            Stmt::While {
                condition,
                body,
                span,
            } => HirStmt::While {
                condition: self.lower_expr(condition),
                body: self.lower_stmts(&body),
                span,
            },

            Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                span,
            } => HirStmt::Attempt {
                body: self.lower_stmts(&body),
                catches: catches
                    .iter()
                    .map(|c| HirCatchClause {
                        binding: c.binding,
                        ty: c
                            .ty
                            .as_ref()
                            .map(|te| resolve_type_expr_simple(te, self.interner))
                            .unwrap_or(FidanType::Dynamic),
                        body: self.lower_stmts(&c.body),
                        span: c.span,
                    })
                    .collect(),
                otherwise: otherwise.map(|b| self.lower_stmts(&b)),
                finally: finally.map(|b| self.lower_stmts(&b)),
                span,
            },

            Stmt::Panic { value, span } => HirStmt::Panic {
                value: self.lower_expr(value),
                span,
            },

            Stmt::ParallelFor {
                binding,
                iterable,
                body,
                span,
            } => {
                let iter_ty = self.ty(iterable);
                let binding_ty = match iter_ty {
                    FidanType::List(elem) => *elem,
                    _ => FidanType::Dynamic,
                };
                HirStmt::ParallelFor {
                    binding,
                    binding_ty,
                    iterable: self.lower_expr(iterable),
                    body: self.lower_stmts(&body),
                    span,
                }
            }

            Stmt::ConcurrentBlock {
                is_parallel,
                tasks,
                span,
            } => HirStmt::ConcurrentBlock {
                is_parallel,
                tasks: tasks
                    .iter()
                    .map(|t| HirTask {
                        name: t.name,
                        body: self.lower_stmts(&t.body),
                        span: t.span,
                    })
                    .collect(),
                span,
            },

            Stmt::Error { span } => HirStmt::Error { span },
        }
    }

    // ── Parameter lowering ────────────────────────────────────────────────────

    fn lower_params(&self, params: &[Param]) -> Vec<HirParam> {
        params
            .iter()
            .map(|p| {
                // Resolve parameter type from typeck's type table (based on the
                // type annotation expression). Fall back to Dynamic if unknown.
                let ty = self
                    .typed
                    .actions
                    .values()
                    .flat_map(|a| a.params.iter())
                    .find(|pi| pi.name == p.name)
                    .map(|pi| pi.ty.clone())
                    .unwrap_or_else(|| {
                        // Resolve from the type expression directly.
                        // TypedModule doesn't directly expose a resolve_type_expr,
                        // so we make a best-effort map from named types here.
                        resolve_type_expr_simple(&p.ty, self.interner)
                    });
                HirParam {
                    name: p.name,
                    ty,
                    required: p.required,
                    default: p.default.map(|e| self.lower_expr(e)),
                    span: p.span,
                }
            })
            .collect()
    }

    fn lower_function(
        &self,
        name: Symbol,
        extends: Option<Symbol>,
        params: &[Param],
        return_ty: FidanType,
        body: &[StmtId],
        is_parallel: bool,
        span: Span,
    ) -> HirFunction {
        HirFunction {
            name,
            extends,
            params: self.lower_params(params),
            return_ty,
            body: self.lower_stmts(body),
            is_parallel,
            span,
        }
    }
}

// ── Utility: shallow type-expr resolver ───────────────────────────────────────

/// Resolve a `TypeExpr` to a `FidanType` using the symbol interner for named types.
///
/// Primitive names (`string`, `integer`, `float`, `boolean`, `nothing`, `dynamic`) map to
/// their corresponding `FidanType` variants.  All other names are treated as user-defined
/// object types (`FidanType::Object(sym)`).  Parameterised forms `list oftype T`,
/// `Shared oftype T`, and `Pending oftype T` produce the correct wrapper type.
fn resolve_type_expr_simple(te: &fidan_ast::TypeExpr, interner: &SymbolInterner) -> FidanType {
    match te {
        fidan_ast::TypeExpr::Named { name, .. } => match interner.resolve(*name).as_ref() {
            "string" => FidanType::String,
            "integer" => FidanType::Integer,
            "float" => FidanType::Float,
            "boolean" => FidanType::Boolean,
            "nothing" => FidanType::Nothing,
            "dynamic" => FidanType::Dynamic,
            _ => FidanType::Object(*name),
        },
        fidan_ast::TypeExpr::Dynamic { .. } => FidanType::Dynamic,
        fidan_ast::TypeExpr::Nothing { .. } => FidanType::Nothing,
        fidan_ast::TypeExpr::Oftype { base, param, .. } => {
            let p = resolve_type_expr_simple(param, interner);
            match resolve_type_expr_simple(base, interner) {
                FidanType::Object(sym) => match interner.resolve(sym).as_ref() {
                    "list" | "List" => FidanType::List(Box::new(p)),
                    "Shared" => FidanType::Shared(Box::new(p)),
                    "Pending" => FidanType::Pending(Box::new(p)),
                    _ => FidanType::Dynamic,
                },
                _ => FidanType::Dynamic,
            }
        }
        fidan_ast::TypeExpr::Tuple { elements, .. } => FidanType::Tuple(
            elements
                .iter()
                .map(|e| resolve_type_expr_simple(e, interner))
                .collect(),
        ),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower a parsed `Module` into HIR using the type annotations from `typed`.
///
/// This function is infallible: any unsupported construct is lowered to an
/// `HirStmt::Error` / `HirExprKind::Error` placeholder.
pub fn lower_module(module: &Module, typed: &TypedModule, interner: &SymbolInterner) -> HirModule {
    let ctx = Ctx {
        arena: &module.arena,
        typed,
        interner,
    };

    let mut objects: Vec<crate::hir::HirObject> = vec![];
    let mut functions: Vec<HirFunction> = vec![];
    let globals: Vec<HirGlobal> = vec![];
    let mut init_stmts: Vec<HirStmt> = vec![];
    let mut use_decls: Vec<HirUseDecl> = vec![];

    for &item_id in &module.items {
        match ctx.arena.get_item(item_id).clone() {
            Item::ObjectDecl {
                name,
                parent,
                fields,
                methods,
                span,
            } => {
                let hir_fields: Vec<HirField> = fields
                    .iter()
                    .map(|f| {
                        let ty = typed
                            .objects
                            .get(&name)
                            .and_then(|o| o.fields.get(&f.name))
                            .cloned()
                            .unwrap_or(FidanType::Dynamic);
                        HirField {
                            name: f.name,
                            ty,
                            required: f.required,
                            default: f.default.map(|e| ctx.lower_expr(e)),
                            span: f.span,
                        }
                    })
                    .collect();

                let hir_methods: Vec<HirFunction> = methods
                    .iter()
                    .filter_map(|&mid| {
                        if let Item::ActionDecl {
                            name: mname,
                            params,
                            return_ty: _,
                            body,
                            is_parallel,
                            span: mspan,
                            ..
                        } = ctx.arena.get_item(mid).clone()
                        {
                            // Determine return type from object's method registry.
                            let ret = typed
                                .objects
                                .get(&name)
                                .and_then(|o| o.methods.get(&mname))
                                .map(|a| a.return_ty.clone())
                                .unwrap_or(FidanType::Nothing);
                            Some(ctx.lower_function(
                                mname,
                                None,
                                &params,
                                ret,
                                &body,
                                is_parallel,
                                mspan,
                            ))
                        } else {
                            None
                        }
                    })
                    .collect();

                objects.push(HirObject {
                    name,
                    parent,
                    fields: hir_fields,
                    methods: hir_methods,
                    span,
                });
            }

            Item::ActionDecl {
                name,
                params,
                return_ty: _,
                body,
                is_parallel,
                span,
                ..
            } => {
                let ret = typed
                    .actions
                    .get(&name)
                    .map(|a| a.return_ty.clone())
                    .unwrap_or(FidanType::Nothing);
                functions.push(ctx.lower_function(
                    name,
                    None,
                    &params,
                    ret,
                    &body,
                    is_parallel,
                    span,
                ));
            }

            Item::ExtensionAction {
                name,
                extends,
                params,
                return_ty: _,
                body,
                is_parallel,
                span,
                ..
            } => {
                let ret = typed
                    .objects
                    .get(&extends)
                    .and_then(|o| o.methods.get(&name))
                    .map(|a| a.return_ty.clone())
                    .unwrap_or(FidanType::Nothing);
                functions.push(ctx.lower_function(
                    name,
                    Some(extends),
                    &params,
                    ret,
                    &body,
                    is_parallel,
                    span,
                ));
            }

            Item::VarDecl {
                name,
                ty: _,
                init,
                is_const,
                span,
            } => {
                let ty = init.map(|e| ctx.ty(e)).unwrap_or(FidanType::Nothing);
                // Push as an ordered init-statement so that variable declarations
                // appear in source order relative to expressions (not hoisted above all stmts).
                init_stmts.push(HirStmt::VarDecl {
                    name,
                    ty,
                    init: init.map(|e| ctx.lower_expr(e)),
                    is_const,
                    span,
                });
            }

            Item::Destructure {
                bindings,
                value,
                span,
            } => {
                let value_ty = ctx.ty(value);
                let binding_tys = match &value_ty {
                    FidanType::Tuple(elems) => elems.clone(),
                    _ => vec![FidanType::Dynamic; bindings.len()],
                };
                init_stmts.push(HirStmt::Destructure {
                    bindings,
                    binding_tys,
                    value: ctx.lower_expr(value),
                    span,
                });
            }

            Item::ExprStmt(expr_id) => {
                init_stmts.push(HirStmt::Expr(ctx.lower_expr(expr_id)));
            }

            Item::Assign {
                target,
                value,
                span,
            } => {
                init_stmts.push(HirStmt::Assign {
                    target: ctx.lower_expr(target),
                    value: ctx.lower_expr(value),
                    span,
                });
            }

            // Top-level control-flow statements (for, while, if, check, attempt, etc.)
            Item::Stmt(stmt_id) => {
                init_stmts.push(ctx.lower_stmt(stmt_id));
            }

            // Module imports: capture stdlib imports and propagate to the interpreter.
            Item::Use {
                path,
                alias,
                re_export,
                ..
            } => {
                // Resolve all path symbols to strings.
                let parts: Vec<String> = path
                    .iter()
                    .map(|&s| interner.resolve(s).to_string())
                    .collect();

                // Only process `std.*` paths.
                if parts.first().map(|s| s == "std").unwrap_or(false) && parts.len() >= 2 {
                    let module_name = parts[1].clone();

                    if parts.len() == 2 {
                        // `use std.MODULE` — namespace import: alias defaults to module name.
                        let ns_alias = alias
                            .map(|sym| interner.resolve(sym).to_string())
                            .unwrap_or_else(|| module_name.clone());
                        use_decls.push(HirUseDecl {
                            module_path: vec!["std".into(), module_name],
                            alias: Some(ns_alias),
                            specific_names: None,
                            re_export,
                        });
                    } else {
                        // `use std.MODULE.name` — specific name import.
                        let fn_name = parts[parts.len() - 1].clone();
                        use_decls.push(HirUseDecl {
                            module_path: vec!["std".into(), module_name],
                            alias: None,
                            specific_names: Some(vec![fn_name]),
                            re_export,
                        });
                    }
                }
            }
        }
    }

    HirModule {
        objects,
        functions,
        globals,
        init_stmts,
        use_decls,
    }
}

/// Merge `imported` into `base`, prepending the imported module's definitions
/// and statements so they are available (and run first) when the base module executes.
///
/// Used by the CLI multi-file pipeline to combine separately-lowered files
/// before MIR compilation.
pub fn merge_module(base: HirModule, imported: HirModule) -> HirModule {
    let mut merged = imported;
    merged.objects.extend(base.objects);
    merged.functions.extend(base.functions);
    merged.globals.extend(base.globals);
    merged.init_stmts.extend(base.init_stmts);
    // All use_decls from the imported side are kept so the imported file's own
    // functions still have access to the stdlib namespaces they declared.
    // Isolation is enforced at the type-checking layer: `pre_register_hir_into_tc`
    // only exposes `re_export = true` use_decls to the importing file's typechecker.
    merged.use_decls.extend(base.use_decls);
    merged
}
