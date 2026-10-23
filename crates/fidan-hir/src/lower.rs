// fidan-hir/src/lower.rs
//
// AST → HIR lowering.
//
// Walks the flat, arena-based AST produced by `fidan-parser` together with
// the type-annotation map produced by `fidan-typeck` and emits an owned,
// fully-typed HIR tree.

use fidan_ast::{Arg, AstArena, Expr, ExprId, InterpPart, Item, Module, Param, Stmt, StmtId};
use fidan_config::BUILTIN_DECORATORS;
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::Span;
use fidan_typeck::{ActionInfo, FidanType, TypedModule};

/// Decorator name for JIT pre-compilation.  Used in three places during lowering;
/// a single constant prevents typo-divergence.
const DECORATOR_PRECOMPILE: &str = "precompile";
const DECORATOR_EXTERN: &str = "extern";

use crate::hir::{
    HirArg, HirCatchClause, HirCheckArm, HirCheckExprArm, HirElseIf, HirExpr, HirExprKind,
    HirExternAbi, HirExternDecl, HirField, HirFunction, HirGlobal, HirInterpPart, HirModule,
    HirObject, HirParam, HirStmt, HirTask, HirTestDecl, HirUseDecl,
};

// ── Helpers ────────────────────────────────────────────────────────────────────

fn lower_extern_decl(
    arena: &AstArena,
    decorators: &[fidan_ast::Decorator],
    interner: &SymbolInterner,
    function_name: Symbol,
) -> Option<HirExternDecl> {
    let decorator = decorators
        .iter()
        .find(|d| interner.resolve(d.name).as_ref() == DECORATOR_EXTERN)?;
    let mut positional = decorator.args.iter().filter(|arg| arg.name.is_none());
    let lib = positional
        .next()
        .and_then(|arg| match arena.get_expr(arg.value) {
            Expr::StrLit { value, .. } => Some(value.clone()),
            _ => None,
        })?;

    let mut symbol: Option<String> = None;
    let mut link: Option<String> = None;
    let mut abi = HirExternAbi::Native;

    for arg in decorator.args.iter().filter(|arg| arg.name.is_some()) {
        let Some(name) = arg.name else { continue };
        let key = interner.resolve(name);
        match key.as_ref() {
            "symbol" => {
                if let Expr::StrLit { value, .. } = arena.get_expr(arg.value) {
                    symbol = Some(value.clone());
                }
            }
            "link" => {
                if let Expr::StrLit { value, .. } = arena.get_expr(arg.value) {
                    link = Some(value.clone());
                }
            }
            "abi" => {
                if let Expr::StrLit { value, .. } = arena.get_expr(arg.value)
                    && value.eq_ignore_ascii_case("fidan")
                {
                    abi = HirExternAbi::Fidan;
                }
            }
            _ => {}
        }
    }

    Some(HirExternDecl {
        lib,
        symbol: symbol.unwrap_or_else(|| interner.resolve(function_name).to_string()),
        link,
        abi,
    })
}

// ── Context ────────────────────────────────────────────────────────────────────

struct Ctx<'a> {
    arena: &'a AstArena,
    typed: &'a TypedModule,
    interner: &'a SymbolInterner,
}

struct LowerFunction<'a> {
    name: Symbol,
    extends: Option<Symbol>,
    params: &'a [Param],
    info: Option<&'a ActionInfo>,
    return_ty: FidanType,
    body: &'a [StmtId],
    is_parallel: bool,
    precompile: bool,
    extern_decl: Option<HirExternDecl>,
    span: Span,
}

impl<'a> Ctx<'a> {
    fn annotation_type(&self, annotation: &fidan_ast::TypeExpr) -> FidanType {
        self.typed
            .annotation_types
            .get(&annotation.span())
            .cloned()
            .unwrap_or(FidanType::Error)
    }

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
            Expr::Slice {
                target,
                start,
                end,
                inclusive,
                step,
                ..
            } => HirExprKind::Slice {
                target: Box::new(self.lower_expr(target)),
                start: start.map(|e| Box::new(self.lower_expr(e))),
                end: end.map(|e| Box::new(self.lower_expr(e))),
                inclusive,
                step: step.map(|e| Box::new(self.lower_expr(e))),
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

            Expr::ListComp {
                element,
                binding,
                iterable,
                filter,
                ..
            } => HirExprKind::ListComp {
                element: Box::new(self.lower_expr(element)),
                binding,
                iterable: Box::new(self.lower_expr(iterable)),
                filter: filter.map(|f| Box::new(self.lower_expr(f))),
            },
            Expr::DictComp {
                key,
                value,
                binding,
                iterable,
                filter,
                ..
            } => HirExprKind::DictComp {
                key: Box::new(self.lower_expr(key)),
                value: Box::new(self.lower_expr(value)),
                binding,
                iterable: Box::new(self.lower_expr(iterable)),
                filter: filter.map(|f| Box::new(self.lower_expr(f))),
            },

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
                        pattern: self.lower_check_pattern(arm.pattern),
                        body: self.lower_stmts(&arm.body),
                        span: arm.span,
                    })
                    .collect(),
            },

            Expr::Error { .. } => HirExprKind::Error,

            Expr::Lambda { params, body, .. } => {
                // Consume the checker's resolved signature, including aliases and enums.
                let hir_params = params
                    .iter()
                    .map(|p| HirParam {
                        name: p.name,
                        ty: self.annotation_type(&p.ty),
                        certain: p.certain,
                        optional: p.optional,
                        default: p.default.map(|e| self.lower_expr(e)),
                        span: p.span,
                    })
                    .collect();
                let hir_body = self.lower_stmts(&body);
                HirExprKind::Lambda {
                    name: None,
                    params: hir_params,
                    body: hir_body,
                    return_ty: self
                        .typed
                        .expr_actions
                        .get(&id)
                        .map(|info| info.return_ty.clone())
                        .unwrap_or(FidanType::Error),
                    precompile: false,
                    extern_decl: None,
                }
            }
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

    /// Lower a `check` arm pattern expression.
    ///
    /// When the pattern has the form `EnumType.Variant(binding1, binding2, ...)`
    /// (i.e. a call on a field of an enum-typed expression where all args are
    /// plain identifiers), we produce `HirExprKind::EnumDestructure` so that
    /// the MIR lowerer can emit `EnumTagCheck` + `EnumPayload` extractions.
    /// Everything else is lowered normally.
    fn lower_check_pattern(&self, id: ExprId) -> HirExpr {
        let expr = self.arena.get_expr(id).clone();
        let ty = self.ty(id);
        let span = expr.span();

        if let Expr::Call {
            callee: callee_id,
            args,
            ..
        } = &expr
        {
            let callee_expr = self.arena.get_expr(*callee_id).clone();
            if let Expr::Field {
                object: obj_id,
                field,
                ..
            } = callee_expr
            {
                let obj_ty = self.ty(obj_id);
                if let FidanType::Enum(enum_sym) = obj_ty {
                    let bindings: Vec<Symbol> = args
                        .iter()
                        .filter_map(|arg| {
                            let arg_expr = self.arena.get_expr(arg.value).clone();
                            if let Expr::Ident { name, .. } = arg_expr {
                                Some(name)
                            } else {
                                None
                            }
                        })
                        .collect();
                    return HirExpr {
                        kind: HirExprKind::EnumDestructure {
                            enum_sym,
                            tag: field,
                            bindings,
                        },
                        ty,
                        span,
                    };
                }
            }
        }

        self.lower_expr(id)
    }

    fn lower_interp_part(&self, part: &InterpPart) -> HirInterpPart {
        match part {
            InterpPart::Literal(s) => HirInterpPart::Literal(s.clone()),
            InterpPart::Expr(id) => HirInterpPart::Expr(self.lower_expr(*id)),
        }
    }

    // ── Statement lowering ────────────────────────────────────────────────────

    fn lower_stmts(&self, ids: &[StmtId]) -> Vec<HirStmt> {
        let mut lowered = Vec::with_capacity(ids.len());
        for &id in ids {
            self.lower_stmt_into(id, &mut lowered);
        }
        lowered
    }

    fn lower_stmt_into(&self, id: StmtId, out: &mut Vec<HirStmt>) {
        match self.arena.get_stmt(id).clone() {
            Stmt::VarDecl {
                name,
                ty: _,
                init,
                is_const,
                span,
            } => {
                let ty = init.map(|e| self.ty(e)).unwrap_or(FidanType::Nothing);
                out.push(HirStmt::VarDecl {
                    name,
                    ty,
                    init: init.map(|e| self.lower_expr(e)),
                    is_const,
                    span,
                });
            }

            Stmt::Destructure {
                bindings,
                value,
                span,
            } => {
                let value_ty = self.ty(value);
                let binding_tys = match &value_ty {
                    FidanType::Tuple(elems) => elems.clone(),
                    _ => vec![FidanType::Dynamic; bindings.len()],
                };
                out.push(HirStmt::Destructure {
                    bindings,
                    binding_tys,
                    value: self.lower_expr(value),
                    span,
                });
            }

            Stmt::Assign {
                target,
                value,
                span,
            } => out.push(HirStmt::Assign {
                target: self.lower_expr(target),
                value: self.lower_expr(value),
                span,
            }),

            Stmt::Expr { expr, .. } => out.push(HirStmt::Expr(self.lower_expr(expr))),

            Stmt::ActionDecl {
                name,
                params,
                return_ty,
                body,
                decorators,
                span,
                ..
            } => {
                let precompile = decorators
                    .iter()
                    .any(|d| self.interner.resolve(d.name).as_ref() == DECORATOR_PRECOMPILE);
                let extern_decl = lower_extern_decl(self.arena, &decorators, self.interner, name);

                out.push(HirStmt::VarDecl {
                    name,
                    ty: FidanType::Function,
                    init: Some(HirExpr {
                        ty: FidanType::Function,
                        span,
                        kind: HirExprKind::Lambda {
                            name: Some(name),
                            params: self.lower_params(&params, None),
                            body: self.lower_stmts(&body),
                            return_ty: return_ty
                                .as_ref()
                                .map(|ty| self.annotation_type(ty))
                                .unwrap_or(FidanType::Dynamic),
                            precompile,
                            extern_decl,
                        },
                    }),
                    is_const: true,
                    span,
                });

                for value in self.decorator_applications(name, &decorators) {
                    out.push(HirStmt::Assign {
                        target: HirExpr {
                            ty: FidanType::Function,
                            span,
                            kind: HirExprKind::Var(name),
                        },
                        value,
                        span,
                    });
                }
            }

            Stmt::Return { value, span } => out.push(HirStmt::Return {
                value: value.map(|e| self.lower_expr(e)),
                span,
            }),

            Stmt::Break { span } => out.push(HirStmt::Break { span }),
            Stmt::Continue { span } => out.push(HirStmt::Continue { span }),

            Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                span,
            } => out.push(HirStmt::If {
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
            }),

            Stmt::Check {
                scrutinee,
                arms,
                span,
            } => out.push(HirStmt::Check {
                scrutinee: self.lower_expr(scrutinee),
                arms: arms
                    .iter()
                    .map(|arm| HirCheckArm {
                        pattern: self.lower_check_pattern(arm.pattern),
                        body: self.lower_stmts(&arm.body),
                        span: arm.span,
                    })
                    .collect(),
                span,
            }),

            Stmt::For {
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
                out.push(HirStmt::For {
                    binding,
                    binding_ty,
                    iterable: self.lower_expr(iterable),
                    body: self.lower_stmts(&body),
                    span,
                });
            }

            Stmt::While {
                condition,
                body,
                span,
            } => out.push(HirStmt::While {
                condition: self.lower_expr(condition),
                body: self.lower_stmts(&body),
                span,
            }),

            Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                span,
            } => out.push(HirStmt::Attempt {
                body: self.lower_stmts(&body),
                catches: catches
                    .iter()
                    .map(|c| HirCatchClause {
                        binding: c.binding,
                        ty: c
                            .ty
                            .as_ref()
                            .map(|te| self.annotation_type(te))
                            .unwrap_or(FidanType::Dynamic),
                        body: self.lower_stmts(&c.body),
                        span: c.span,
                    })
                    .collect(),
                otherwise: otherwise.map(|b| self.lower_stmts(&b)),
                finally: finally.map(|b| self.lower_stmts(&b)),
                span,
            }),

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
                out.push(HirStmt::ParallelFor {
                    binding,
                    binding_ty,
                    iterable: self.lower_expr(iterable),
                    body: self.lower_stmts(&body),
                    span,
                });
            }

            Stmt::ConcurrentBlock {
                is_parallel,
                tasks,
                span,
            } => out.push(HirStmt::ConcurrentBlock {
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
            }),

            Stmt::Panic { value, span } => out.push(HirStmt::Panic {
                value: self.lower_expr(value),
                span,
            }),

            Stmt::Error { span } => out.push(HirStmt::Error { span }),
        }
    }

    /// Declaration-time calls share ordinary argument/default evaluation.
    fn decorator_applications(
        &self,
        binding: Symbol,
        decorators: &[fidan_ast::Decorator],
    ) -> Vec<HirExpr> {
        decorators
            .iter()
            .filter(|decorator| {
                !BUILTIN_DECORATORS.contains(&self.interner.resolve(decorator.name).as_ref())
            })
            .map(|decorator| {
                let span = decorator.span;
                let current = || HirExpr {
                    ty: FidanType::Function,
                    span,
                    kind: HirExprKind::Var(binding),
                };
                let mut args = vec![HirArg {
                    name: None,
                    value: current(),
                    span,
                }];
                args.extend(decorator.args.iter().map(|arg| self.lower_arg(arg)));
                let application = HirExpr {
                    ty: FidanType::Dynamic,
                    span,
                    kind: HirExprKind::Call {
                        callee: Box::new(self.lower_expr(decorator.callee)),
                        args,
                    },
                };
                let value = HirExpr {
                    ty: FidanType::Dynamic,
                    span,
                    kind: HirExprKind::NullCoalesce {
                        lhs: Box::new(application),
                        rhs: Box::new(current()),
                    },
                };
                // Existing dynamic argument validation rejects erased non-action results.
                // Nothing preserves legacy side-effect-only decorators.
                let result = self.interner.intern("__decorator_result");
                let validator = HirExpr {
                    ty: FidanType::Function,
                    span,
                    kind: HirExprKind::Lambda {
                        name: None,
                        params: vec![HirParam {
                            name: result,
                            ty: FidanType::Function,
                            certain: true,
                            optional: false,
                            default: None,
                            span,
                        }],
                        body: vec![HirStmt::Return {
                            value: Some(HirExpr {
                                ty: FidanType::Function,
                                span,
                                kind: HirExprKind::Var(result),
                            }),
                            span,
                        }],
                        return_ty: FidanType::Function,
                        precompile: false,
                        extern_decl: None,
                    },
                };
                HirExpr {
                    ty: FidanType::Function,
                    span,
                    kind: HirExprKind::Call {
                        callee: Box::new(validator),
                        args: vec![HirArg {
                            name: None,
                            value,
                            span,
                        }],
                    },
                }
            })
            .collect()
    }

    fn lower_decorators(
        &self,
        function: Symbol,
        owner: Option<Symbol>,
        span: Span,
        decorators: &[fidan_ast::Decorator],
        out: &mut Vec<HirStmt>,
    ) {
        let binding = self
            .interner
            .intern(&format!("__decorated_{}_{}", span.file.0, span.start));
        let applications = self.decorator_applications(binding, decorators);
        if !applications.is_empty() {
            out.push(HirStmt::Decorate {
                function,
                owner,
                binding,
                applications,
                span,
            });
        }
    }

    // ── Parameter lowering ────────────────────────────────────────────────────

    fn lower_params(&self, params: &[Param], info: Option<&ActionInfo>) -> Vec<HirParam> {
        params
            .iter()
            .enumerate()
            .map(|(index, p)| {
                let ty = info
                    .and_then(|action| action.params.get(index))
                    .map(|pi| pi.ty.clone())
                    .unwrap_or_else(|| self.annotation_type(&p.ty));
                HirParam {
                    name: p.name,
                    ty,
                    certain: p.certain,
                    optional: p.optional,
                    default: p.default.map(|e| self.lower_expr(e)),
                    span: p.span,
                }
            })
            .collect()
    }

    fn lower_function(&self, function: LowerFunction<'_>) -> HirFunction {
        let LowerFunction {
            name,
            extends,
            params,
            info,
            return_ty,
            body,
            is_parallel,
            precompile,
            extern_decl,
            span,
        } = function;

        HirFunction {
            name,
            extends,
            params: self.lower_params(params, info),
            return_ty,
            body: self.lower_stmts(body),
            is_parallel,
            precompile,
            extern_decl,
            span,
        }
    }
}

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
    let mut tests: Vec<HirTestDecl> = vec![];
    let mut enums: Vec<crate::hir::HirEnum> = vec![];

    for &item_id in &module.items {
        match ctx.arena.get_item(item_id).clone() {
            Item::ObjectDecl {
                name,
                parent,
                fields,
                methods,
                span,
            } => {
                // HIR only needs the last segment (the object name) for parent lookup.
                // For qualified paths like `module.Foo`, we use `Foo` as the parent symbol.
                let hir_parent = parent.as_ref().and_then(|p| p.last().copied());
                let hir_fields: Vec<HirField> = fields
                    .iter()
                    .map(|f| {
                        let ty = typed
                            .objects
                            .get(&name)
                            .and_then(|o| o.fields.get(&f.name))
                            .map(|field| field.ty.clone())
                            .unwrap_or(FidanType::Dynamic);
                        HirField {
                            name: f.name,
                            ty,
                            is_const: f.is_const,
                            certain: f.certain,
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
                            decorators,
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
                            let info = typed.objects.get(&name).and_then(|o| o.methods.get(&mname));
                            let precompile = decorators.iter().any(|d| {
                                ctx.interner.resolve(d.name).as_ref() == DECORATOR_PRECOMPILE
                            });
                            let extern_decl =
                                lower_extern_decl(ctx.arena, &decorators, ctx.interner, mname);
                            ctx.lower_decorators(
                                mname,
                                Some(name),
                                mspan,
                                &decorators,
                                &mut init_stmts,
                            );
                            Some(ctx.lower_function(LowerFunction {
                                name: mname,
                                extends: None,
                                params: &params,
                                info,
                                return_ty: ret,
                                body: &body,
                                is_parallel,
                                precompile,
                                extern_decl,
                                span: mspan,
                            }))
                        } else {
                            None
                        }
                    })
                    .collect();

                objects.push(HirObject {
                    name,
                    parent: hir_parent,
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
                decorators,
                span,
                ..
            } => {
                let ret = typed
                    .actions
                    .get(&name)
                    .map(|a| a.return_ty.clone())
                    .unwrap_or(FidanType::Nothing);
                let info = typed.actions.get(&name);
                let precompile = decorators
                    .iter()
                    .any(|d| ctx.interner.resolve(d.name).as_ref() == DECORATOR_PRECOMPILE);
                let extern_decl = lower_extern_decl(ctx.arena, &decorators, ctx.interner, name);
                ctx.lower_decorators(name, None, span, &decorators, &mut init_stmts);
                functions.push(ctx.lower_function(LowerFunction {
                    name,
                    extends: None,
                    params: &params,
                    info,
                    return_ty: ret,
                    body: &body,
                    is_parallel,
                    precompile,
                    extern_decl,
                    span,
                }));
            }

            Item::ExtensionAction {
                name,
                extends,
                params,
                return_ty: _,
                body,
                is_parallel,
                decorators,
                span,
                ..
            } => {
                let ret = typed
                    .objects
                    .get(&extends)
                    .and_then(|o| o.methods.get(&name))
                    .map(|a| a.return_ty.clone())
                    .unwrap_or(FidanType::Nothing);
                let info = typed
                    .objects
                    .get(&extends)
                    .and_then(|o| o.methods.get(&name));
                let precompile = decorators
                    .iter()
                    .any(|d| ctx.interner.resolve(d.name).as_ref() == DECORATOR_PRECOMPILE);
                let extern_decl = lower_extern_decl(ctx.arena, &decorators, ctx.interner, name);
                ctx.lower_decorators(name, Some(extends), span, &decorators, &mut init_stmts);
                functions.push(ctx.lower_function(LowerFunction {
                    name,
                    extends: Some(extends),
                    params: &params,
                    info,
                    return_ty: ret,
                    body: &body,
                    is_parallel,
                    precompile,
                    extern_decl,
                    span,
                }));
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
                ctx.lower_stmt_into(stmt_id, &mut init_stmts);
            }

            // Test blocks: lowered into named HirTestDecls, not into init_stmts.
            Item::TestDecl { name, body, span } => {
                tests.push(HirTestDecl {
                    name,
                    body: ctx.lower_stmts(&body),
                    span,
                });
            }

            // Enum declarations: lowered into HirEnum entries (MIR will create globals).
            Item::EnumDecl {
                name,
                variants,
                span,
            } => {
                enums.push(crate::hir::HirEnum {
                    name,
                    variants: variants
                        .iter()
                        .map(|v| (v.name, v.payload_types.len()))
                        .collect(),
                    span,
                });
            }

            // Module imports: capture stdlib imports and propagate to the interpreter.
            Item::Use {
                path,
                alias,
                re_export,
                grouped,
                span,
                ..
            } => {
                // Resolve all path symbols to strings.
                let parts: Vec<String> = path
                    .iter()
                    .map(|&s| interner.resolve(s).to_string())
                    .collect();

                // Process `std.*` stdlib paths.
                if parts.first().map(|s| s == "std").unwrap_or(false) && parts.len() >= 2 {
                    let module_name = parts[1].clone();

                    if parts.len() == 2 {
                        // `use std.MODULE` — namespace import: alias defaults to module name.
                        let ns_alias = alias
                            .map(|sym| interner.resolve(sym).to_string())
                            .unwrap_or_else(|| module_name.clone());
                        use_decls.push(HirUseDecl {
                            span,
                            target_path: None,
                            module_path: vec!["std".into(), module_name],
                            alias: Some(ns_alias),
                            specific_names: None,
                            re_export,
                        });
                    } else {
                        // `use std.MODULE.name` — specific name import.
                        let fn_name = parts[parts.len() - 1].clone();
                        use_decls.push(HirUseDecl {
                            span,
                            target_path: None,
                            module_path: vec!["std".into(), module_name],
                            alias: None,
                            specific_names: Some(vec![fn_name]),
                            re_export,
                        });
                    }
                } else if !parts.is_empty() && parts[0] != "std" {
                    let is_file_path = parts[0].starts_with("./")
                        || parts[0].starts_with("../")
                        || parts[0].starts_with('/')
                        || parts[0].ends_with(".fdn");
                    let specific_names = if is_file_path && alias.is_none() {
                        Some(vec![])
                    } else if grouped && parts.len() >= 2 {
                        Some(vec![parts.last().unwrap().clone()])
                    } else {
                        None
                    };
                    let ns_alias = alias
                        .map(|sym| interner.resolve(sym).to_string())
                        .or_else(|| parts.last().cloned());
                    use_decls.push(HirUseDecl {
                        span,
                        target_path: None,
                        module_path: vec![ns_alias.clone().unwrap_or_default()],
                        alias: ns_alias,
                        specific_names,
                        re_export,
                    });
                }
            }
        }
    }

    HirModule {
        file: module.file,
        file_paths: vec![],
        objects,
        enums,
        functions,
        globals,
        init_stmts,
        use_decls,
        tests,
    }
}

/// Merge `imported` into `base`, prepending the imported module's definitions
/// and statements so they are available (and run first) when the base module executes.
///
/// Used by the CLI multi-file pipeline to combine separately-lowered files
/// before MIR compilation.
pub fn merge_module(base: HirModule, imported: HirModule) -> HirModule {
    let mut merged = imported;
    merged.file = base.file;
    merged.file_paths.extend(base.file_paths);
    merged.objects.extend(base.objects);
    merged.enums.extend(base.enums);
    merged.functions.extend(base.functions);
    merged.globals.extend(base.globals);
    merged.init_stmts.extend(base.init_stmts);
    // All use_decls from the imported side are kept so the imported file's own
    // functions still have access to the stdlib namespaces they declared.
    // Isolation is enforced at the type-checking layer: `pre_register_hir_into_tc`
    // only exposes `re_export = true` use_decls to the importing file's typechecker.
    merged.use_decls.extend(base.use_decls);
    merged.tests.extend(base.tests);
    merged
}
