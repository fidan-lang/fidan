#![allow(dead_code)]
use crate::scope::{Initialized, ScopeKind, SymbolInfo, SymbolKind, SymbolTable};
use crate::types::FidanType;
use fidan_ast::{AstArena, BinOp, Expr, ExprId, Item, Module, Param, Stmt, StmtId, TypeExpr, UnOp};
use fidan_diagnostics::{Confidence, Diagnostic, FixEngine, Label, Suggestion};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::{FileId, Span};
use rustc_hash::FxHashMap;
use std::sync::Arc;

// ── Data structures ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: Symbol,
    pub ty: FidanType,
    pub required: bool,
    pub has_default: bool,
}

#[derive(Debug, Clone)]
pub struct ActionInfo {
    pub params: Vec<ParamInfo>,
    pub return_ty: FidanType,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub fields: FxHashMap<Symbol, FidanType>,
    pub methods: FxHashMap<Symbol, ActionInfo>,
    pub parent: Option<Symbol>,
    pub span: Span,
}

// ── TypeChecker ───────────────────────────────────────────────────────────────

pub struct TypeChecker {
    pub(crate) interner: Arc<SymbolInterner>,
    table: SymbolTable,
    objects: FxHashMap<Symbol, ObjectInfo>,
    diags: Vec<Diagnostic>,
    /// Expected return type of the action currently being checked.
    current_return_ty: Option<FidanType>,
    /// Type of `this` in the current object / extension-action scope.
    this_ty: Option<FidanType>,
    /// `FileId` used to synthesise dummy spans for injected symbols.
    file_id: FileId,
    /// When `true` the checker is running inside a REPL session.
    /// Re-declarations (`var x` when `x` already exists) are silently allowed
    /// in the REPL so the user can freely change a binding's type.
    is_repl: bool,
    /// When `true` the checker is in the registration pass (Pass 1).
    /// `resolve_type_expr` will **not** emit E0105 in this mode — Pass 2 is
    /// responsible for emitting type-annotation errors so they fire exactly once.
    registering: bool,
    /// Type of every expression, keyed by `ExprId`.  Populated during type inference.
    /// Used by HIR lowering to annotate HIR nodes with concrete types.
    pub(crate) expr_types: FxHashMap<ExprId, FidanType>,
    /// Top-level action signatures (name → ActionInfo).  Populated during Pass 1.
    pub(crate) actions: FxHashMap<Symbol, ActionInfo>,
}

impl TypeChecker {
    pub fn new(interner: Arc<SymbolInterner>, file_id: FileId) -> Self {
        let mut tc = Self {
            interner,
            table: SymbolTable::new(),
            objects: FxHashMap::default(),
            diags: vec![],
            current_return_ty: None,
            this_ty: None,
            file_id,
            is_repl: false,
            registering: false,
            expr_types: FxHashMap::default(),
            actions: FxHashMap::default(),
        };
        tc.register_builtins();
        tc
    }

    /// Mark this checker as operating in REPL mode.
    /// Re-declarations of existing variables are silently allowed in the REPL.
    pub fn set_repl(&mut self, repl: bool) {
        self.is_repl = repl;
    }

    // ── Built-in registration ─────────────────────────────────────────────

    fn register_builtins(&mut self) {
        let dummy = self.dummy_span();
        let builtins: &[(&str, SymbolKind)] = &[
            ("print", SymbolKind::BuiltinAction),
            ("println", SymbolKind::BuiltinAction),
            ("eprint", SymbolKind::BuiltinAction),
            ("input", SymbolKind::BuiltinAction),
            ("len", SymbolKind::BuiltinAction),
            ("type", SymbolKind::BuiltinAction),
            ("string", SymbolKind::BuiltinAction),
            ("integer", SymbolKind::BuiltinAction),
            ("float", SymbolKind::BuiltinAction),
            ("boolean", SymbolKind::BuiltinAction),
            // Math free-functions
            ("abs", SymbolKind::BuiltinAction),
            ("sqrt", SymbolKind::BuiltinAction),
            ("floor", SymbolKind::BuiltinAction),
            ("ceil", SymbolKind::BuiltinAction),
            ("round", SymbolKind::BuiltinAction),
            ("max", SymbolKind::BuiltinAction),
            ("min", SymbolKind::BuiltinAction),
            // Concurrency helpers
            ("wait", SymbolKind::BuiltinAction),
            // Type constructors
            ("Shared", SymbolKind::BuiltinAction),
        ];
        for &(name, kind) in builtins {
            let sym = self.interner.intern(name);
            self.table.define(
                sym,
                SymbolInfo {
                    kind,
                    ty: FidanType::Function,
                    span: dummy,
                    is_mutable: false,
                    initialized: Initialized::Yes,
                },
            );
        }
    }

    // ── Public entry point ────────────────────────────────────────────────

    /// Run the full type checker over `module`.  Returns all diagnostics.
    pub fn check_module(&mut self, module: &Module) {
        // Pass 1: register every top-level declaration so forward references work.
        // Suppress E0105 diagnostics here — Pass 2 re-resolves and emits them.
        self.registering = true;
        for &item_id in &module.items {
            let item = module.arena.get_item(item_id);
            self.register_top_level(item, &module.arena);
        }

        // Pass 1b: register extension actions as methods on their target objects.
        for &item_id in &module.items {
            let item = module.arena.get_item(item_id).clone();
            if let Item::ExtensionAction {
                name,
                extends,
                ref params,
                ref return_ty,
                span,
                ..
            } = item
            {
                let info = self.build_action_info(params, return_ty, span);
                if let Some(obj) = self.objects.get_mut(&extends) {
                    obj.methods.insert(name, info);
                }
            }
        }
        self.registering = false;

        // Pass 2: full type check.
        for &item_id in &module.items {
            let item = module.arena.get_item(item_id).clone();
            self.check_item(&item, module);
        }
    }

    pub fn finish(self) -> Vec<Diagnostic> {
        self.diags
    }

    /// Consume the checker and return full type-information alongside diagnostics.
    /// Used by HIR lowering which needs to annotate every node with its inferred type.
    pub fn finish_typed(self) -> crate::TypedModule {
        crate::TypedModule {
            diagnostics: self.diags,
            expr_types: self.expr_types,
            objects: self.objects,
            actions: self.actions,
        }
    }

    /// Drain accumulated diagnostics without consuming the checker.
    ///
    /// Used by the REPL after each line so the symbol-table state (defined
    /// names, object registry) survives into the next line while diagnostic
    /// history is cleared for fresh reporting.
    pub fn drain_diags(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diags)
    }

    /// Infer the type of the last bare expression in `module` and return its
    /// human-readable name.  Used by the REPL `:type <expr>` command.
    ///
    /// Runs the registration pass so forward references work, then infers the
    /// last `ExprStmt` item.  Diagnostics accumulate in `self.diags`; call
    /// `drain_diags()` afterwards to suppress or print them.
    pub fn infer_snippet_type(&mut self, module: &Module) -> Option<String> {
        // Pass 1: register top-level declarations in case the snippet contains
        // `object` or `action` items (rare but consistent).
        self.registering = true;
        for &item_id in &module.items {
            let item = module.arena.get_item(item_id);
            self.register_top_level(item, &module.arena);
        }
        self.registering = false;

        // Find the last top-level ExprStmt — that is the expression the user
        // wants to know the type of.
        for &item_id in module.items.iter().rev() {
            let item = module.arena.get_item(item_id).clone();
            if let Item::ExprStmt(expr_id) = item {
                let ty = self.infer_expr(expr_id, module);
                let interner = Arc::clone(&self.interner);
                return Some(ty.display_name(&|sym| interner.resolve(sym).to_string()));
            }
            // Stop at the first non-ExprStmt from the end so that
            //   `:type var x = 5` reports nothing rather than panicking.
            break;
        }
        None
    }

    // ── Registration (pass 1) ─────────────────────────────────────────────

    fn register_top_level(&mut self, item: &Item, arena: &AstArena) {
        let _dummy = self.dummy_span();
        match item {
            Item::ObjectDecl {
                name,
                parent,
                fields,
                methods,
                span,
            } => {
                let mut obj = ObjectInfo {
                    fields: FxHashMap::default(),
                    methods: FxHashMap::default(),
                    parent: *parent,
                    span: *span,
                };
                for field in fields {
                    obj.fields
                        .insert(field.name, self.resolve_type_expr(&field.ty));
                }
                for &mid in methods {
                    if let Item::ActionDecl {
                        name: mname,
                        params,
                        return_ty,
                        span: mspan,
                        ..
                    } = arena.get_item(mid)
                    {
                        let info = self.build_action_info(params, return_ty, *mspan);
                        obj.methods.insert(*mname, info);
                    }
                }
                self.objects.insert(*name, obj);
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Object,
                        ty: FidanType::Object(*name),
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                    },
                );
            }
            Item::ActionDecl {
                name,
                params,
                return_ty,
                span,
                ..
            } => {
                // Record the action's full signature for HIR lowering.
                let info = self.build_action_info(params, return_ty, *span);
                self.actions.insert(*name, info);
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Action,
                        ty: FidanType::Function,
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                    },
                );
            }
            Item::ExtensionAction {
                name,
                params: _,
                return_ty: _,
                span,
                ..
            } => {
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Action,
                        ty: FidanType::Function,
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                    },
                );
            }
            Item::VarDecl {
                name,
                ty,
                init: _,
                is_const,
                span,
            } => {
                // Redeclaration check at pass 1 — fires exactly once on the
                // duplicate `var`, before pass 2 ever runs `check_var_decl`.
                if !self.is_repl {
                    if let Some(prev) = self.table.lookup_current_scope(*name) {
                        if prev.kind != SymbolKind::BuiltinAction {
                            let n = self.interner.resolve(*name).to_string();
                            let prev_span = prev.span;
                            // High-confidence fix: remove the leading `var ` (4 bytes)
                            // to turn the redeclaration into a plain assignment.
                            let var_kw = Span::new(span.file, span.start, span.start + 4);
                            self.diags.push(
                                Diagnostic::error(
                                    fidan_diagnostics::diag_code!("E0102"),
                                    format!("`{n}` is already declared in this scope — use `{n} = value` to reassign"),
                                    *span,
                                )
                                .with_label(Label::secondary(prev_span, "first declared here"))
                                .with_suggestion(Suggestion::fix(
                                    format!("remove `var` to reassign `{n}`"),
                                    var_kw,
                                    "",
                                    Confidence::High,
                                )),
                            );
                            return; // do not redefine; leave old binding intact
                        }
                    }
                }
                let var_ty = ty
                    .as_ref()
                    .map(|t| self.resolve_type_expr(t))
                    .unwrap_or(FidanType::Unknown);
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Var,
                        ty: var_ty,
                        span: *span,
                        is_mutable: !is_const,
                        initialized: Initialized::No,
                    },
                );
            }
            Item::ExprStmt(_)
            | Item::Assign { .. }
            | Item::Use { .. }
            | Item::Stmt(_)
            | Item::Destructure { .. } => {}
        }
    }

    // ── Item checking (pass 2) ────────────────────────────────────────────

    fn check_item(&mut self, item: &Item, module: &Module) {
        match item {
            // ── object ──────────────────────────────────────────────────
            Item::ObjectDecl {
                name,
                parent,
                methods,
                span,
                ..
            } => {
                if let Some(p) = parent {
                    if !self.objects.contains_key(p) {
                        let pname = self.interner.resolve(*p).to_string();
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0100"),
                            format!("undefined object `{pname}` in `extends` clause"),
                            *span,
                        );
                    }
                }

                let obj_ty = FidanType::Object(*name);
                let prev_this = self.this_ty.replace(obj_ty.clone());

                self.table.push_scope(ScopeKind::Object);
                self.inject_this_and_parent(obj_ty, *parent, module.file);

                for &mid in methods {
                    let method = module.arena.get_item(mid).clone();
                    self.check_item(&method, module);
                }

                self.table.pop_scope();
                self.this_ty = prev_this;
            }

            // ── action / extension action ────────────────────────────────
            Item::ActionDecl {
                params,
                return_ty,
                body,
                ..
            } => {
                // `this_ty` is already set if we're inside an ObjectDecl scope.
                self.check_action_body(params, return_ty, body, None, module);
            }

            Item::ExtensionAction {
                extends,
                params,
                return_ty,
                body,
                ..
            } => {
                let ext_ty = FidanType::Object(*extends);
                let prev_this = self.this_ty.replace(ext_ty.clone());
                self.check_action_body(params, return_ty, body, Some(ext_ty), module);
                self.this_ty = prev_this;
            }

            // ── module-level var ─────────────────────────────────────────
            Item::VarDecl {
                name,
                ty,
                init,
                is_const,
                span,
            } => {
                self.check_var_decl(*name, ty, *init, *is_const, *span, module);
            }

            // ── module-level expression ──────────────────────────────────
            Item::ExprStmt(expr_id) => {
                self.warn_bare_literal(*expr_id, module);
                self.infer_expr(*expr_id, module);
            }

            // ── module-level assignment ──────────────────────────────────
            Item::Assign {
                target,
                value,
                span,
            } => {
                self.check_const_assign(*target, *span, module);
                let rhs = self.infer_expr(*value, module);
                let lhs = self.infer_expr(*target, module);
                if !lhs.is_assignable_from(&rhs) {
                    let (l, r) = (self.ty_name(&lhs), self.ty_name(&rhs));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0201"),
                        format!("type mismatch: cannot assign `{r}` to `{l}`"),
                        *span,
                    );
                }
            }

            Item::Use { .. } => {}

            // ── module-level statement (for, while, if, check, attempt, etc.) ──
            Item::Stmt(stmt_id) => {
                self.check_stmt(*stmt_id, module);
            }

            // ── module-level destructure ─────────────────────────────────
            Item::Destructure {
                bindings,
                value,
                span,
            } => {
                let val_ty = self.infer_expr(*value, module);
                let elem_types: Vec<FidanType> = match &val_ty {
                    FidanType::Tuple(elems) => elems.clone(),
                    _ => vec![FidanType::Dynamic; bindings.len()],
                };
                for (i, &binding) in bindings.iter().enumerate() {
                    let bty = elem_types.get(i).cloned().unwrap_or(FidanType::Dynamic);
                    self.table.define(
                        binding,
                        SymbolInfo {
                            kind: SymbolKind::Var,
                            ty: bty,
                            span: *span,
                            is_mutable: true,
                            initialized: Initialized::Yes,
                        },
                    );
                }
            }
        }
    }

    fn check_action_body(
        &mut self,
        params: &[fidan_ast::Param],
        return_ty: &Option<TypeExpr>,
        body: &[StmtId],
        // If Some, inject a `this` binding for extension actions (object scope already
        // provides `this` for regular methods).
        inject_this: Option<FidanType>,
        module: &Module,
    ) {
        let ret = return_ty
            .as_ref()
            .map(|t| self.resolve_type_expr(t))
            .unwrap_or(FidanType::Nothing);
        let prev_ret = self.current_return_ty.replace(ret);

        self.table.push_scope(ScopeKind::Action);

        // Inject `this` if needed (extension action or method with explicit this).
        if let Some(this_ty) = inject_this {
            self.inject_this_binding(this_ty, self.file_id);
        } else if let Some(ref t) = self.this_ty.clone() {
            // Inside an object scope — propagate existing this into the action scope.
            let this_sym = self.interner.intern("this");
            let dummy = self.dummy_span();
            self.table.define(
                this_sym,
                SymbolInfo {
                    kind: SymbolKind::Var,
                    ty: t.clone(),
                    span: dummy,
                    is_mutable: false,
                    initialized: Initialized::Yes,
                },
            );
        }

        for param in params {
            let param_ty = self.resolve_type_expr(&param.ty);
            self.table.define(
                param.name,
                SymbolInfo {
                    kind: SymbolKind::Param,
                    ty: param_ty,
                    span: param.span,
                    is_mutable: false,
                    initialized: if param.required {
                        Initialized::Yes
                    } else {
                        Initialized::Maybe
                    },
                },
            );
        }

        for &sid in body {
            self.check_stmt(sid, module);
        }

        self.table.pop_scope();
        self.current_return_ty = prev_ret;
    }

    // ── Statement checking ────────────────────────────────────────────────

    fn check_stmt(&mut self, stmt_id: StmtId, module: &Module) {
        let stmt = module.arena.get_stmt(stmt_id).clone();
        match stmt {
            Stmt::VarDecl {
                name,
                ty,
                init,
                is_const,
                span,
            } => {
                // Local redeclaration check (action bodies have no pass 1).
                if !self.is_repl {
                    if let Some(prev) = self.table.lookup_current_scope(name) {
                        if prev.kind != SymbolKind::BuiltinAction {
                            let n = self.interner.resolve(name).to_string();
                            let prev_span = prev.span;
                            let var_kw = Span::new(span.file, span.start, span.start + 4);
                            self.diags.push(
                                Diagnostic::error(
                                    fidan_diagnostics::diag_code!("E0102"),
                                    format!("`{n}` is already declared in this scope — use `{n} = value` to reassign"),
                                    span,
                                )
                                .with_label(Label::secondary(prev_span, "first declared here"))
                                .with_suggestion(Suggestion::fix(
                                    format!("remove `var` to reassign `{n}`"),
                                    var_kw,
                                    "",
                                    Confidence::High,
                                )),
                            );
                            return;
                        }
                    }
                }
                self.check_var_decl(name, &ty, init, is_const, span, module);
            }

            Stmt::Assign {
                target,
                value,
                span,
            } => {
                self.check_const_assign(target, span, module);
                let rhs = self.infer_expr(value, module);
                let lhs = self.infer_expr(target, module);
                if !lhs.is_assignable_from(&rhs) {
                    let (l, r) = (self.ty_name(&lhs), self.ty_name(&rhs));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0201"),
                        format!("type mismatch: cannot assign `{r}` to `{l}`"),
                        span,
                    );
                }
            }

            Stmt::Destructure {
                bindings,
                value,
                span,
            } => {
                let val_ty = self.infer_expr(value, module);
                let elem_types: Vec<FidanType> = match &val_ty {
                    FidanType::Tuple(elems) => elems.clone(),
                    FidanType::Unknown | FidanType::Error | FidanType::Dynamic => {
                        vec![FidanType::Dynamic; bindings.len()]
                    }
                    _ => {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0201"),
                            format!(
                                "cannot destructure non-tuple type `{}`",
                                self.ty_name(&val_ty)
                            ),
                            span,
                        );
                        vec![FidanType::Error; bindings.len()]
                    }
                };
                for (i, &binding) in bindings.iter().enumerate() {
                    let bty = elem_types.get(i).cloned().unwrap_or(FidanType::Dynamic);
                    self.table.define(
                        binding,
                        SymbolInfo {
                            kind: SymbolKind::Var,
                            ty: bty,
                            span,
                            is_mutable: true,
                            initialized: Initialized::Yes,
                        },
                    );
                }
            }

            Stmt::Expr { expr, .. } => {
                self.warn_bare_literal(expr, module);
                self.infer_expr(expr, module);
            }

            Stmt::Return { value, span } => {
                let ret = value
                    .map(|id| self.infer_expr(id, module))
                    .unwrap_or(FidanType::Nothing);
                if let Some(expected) = self.current_return_ty.clone() {
                    if !expected.is_assignable_from(&ret) {
                        let (e, a) = (self.ty_name(&expected), self.ty_name(&ret));
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0202"),
                            format!("return type mismatch: expected `{e}`, found `{a}`"),
                            span,
                        );
                    }
                }
            }

            Stmt::Break { .. } | Stmt::Continue { .. } => {}

            Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                self.infer_expr(condition, module);
                self.check_block(&then_body, module);
                for ei in &else_ifs {
                    self.infer_expr(ei.condition, module);
                    self.check_block(&ei.body, module);
                }
                if let Some(body) = &else_body {
                    self.check_block(body, module);
                }
            }

            Stmt::For {
                binding,
                iterable,
                body,
                span,
            } => {
                let iter_ty = self.infer_expr(iterable, module);
                let elem_ty = match iter_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::String | FidanType::Dynamic => FidanType::Dynamic,
                    FidanType::Unknown | FidanType::Error => FidanType::Unknown,
                    _ => FidanType::Dynamic,
                };
                self.table.push_scope(ScopeKind::Block);
                self.table.define(
                    binding,
                    SymbolInfo {
                        kind: SymbolKind::Var,
                        ty: elem_ty,
                        span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                    },
                );
                for &s in &body {
                    self.check_stmt(s, module);
                }
                self.table.pop_scope();
            }

            Stmt::While {
                condition, body, ..
            } => {
                self.infer_expr(condition, module);
                self.check_block(&body, module);
            }

            Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                self.check_block(&body, module);
                for catch in &catches {
                    self.table.push_scope(ScopeKind::Block);
                    if let Some(binding) = catch.binding {
                        let dummy = self.dummy_span();
                        self.table.define(
                            binding,
                            SymbolInfo {
                                kind: SymbolKind::Var,
                                ty: FidanType::Dynamic, // exceptions are untyped in MVP
                                span: dummy,
                                is_mutable: false,
                                initialized: Initialized::Yes,
                            },
                        );
                    }
                    for &s in &catch.body {
                        self.check_stmt(s, module);
                    }
                    self.table.pop_scope();
                }
                if let Some(b) = &otherwise {
                    self.check_block(b, module);
                }
                if let Some(b) = &finally {
                    self.check_block(b, module);
                }
            }

            Stmt::Panic { value, .. } => {
                self.infer_expr(value, module);
            }

            Stmt::ParallelFor {
                binding,
                iterable,
                body,
                span,
            } => {
                self.infer_expr(iterable, module);
                self.table.push_scope(ScopeKind::Block);
                self.table.define(
                    binding,
                    SymbolInfo {
                        kind: SymbolKind::Var,
                        ty: FidanType::Dynamic,
                        span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                    },
                );
                for &s in &body {
                    self.check_stmt(s, module);
                }
                self.table.pop_scope();
            }

            Stmt::ConcurrentBlock { tasks, .. } => {
                for task in &tasks {
                    self.check_block(&task.body, module);
                }
            }

            Stmt::Check {
                scrutinee, arms, ..
            } => {
                self.infer_expr(scrutinee, module);
                for arm in &arms {
                    self.infer_expr(arm.pattern, module);
                    self.check_arm_body(&arm.body, module);
                }
            }

            Stmt::Error { .. } => {}
        }
    }

    fn check_block(&mut self, stmts: &[StmtId], module: &Module) {
        self.table.push_scope(ScopeKind::Block);
        for &s in stmts {
            self.check_stmt(s, module);
        }
        self.table.pop_scope();
    }

    /// Like `check_block`, but suppresses `W2002` on the final statement when
    /// it is a bare expression — that expression is the *result value* of the
    /// check arm, not a discarded side-effect.
    fn check_arm_body(&mut self, stmts: &[StmtId], module: &Module) {
        self.table.push_scope(ScopeKind::Block);
        let (last, rest) = match stmts.split_last() {
            Some(pair) => pair,
            None => {
                self.table.pop_scope();
                return;
            }
        };
        for &s in rest {
            self.check_stmt(s, module);
        }
        // For the final statement: if it's a bare expression, infer its type
        // directly — skipping the bare-literal warning — because it is the
        // arm's result value, not a discarded statement.
        let final_stmt = module.arena.get_stmt(*last).clone();
        match final_stmt {
            Stmt::Expr { expr, .. } => {
                self.infer_expr(expr, module);
            }
            _ => self.check_stmt(*last, module),
        }
        self.table.pop_scope();
    }

    fn check_var_decl(
        &mut self,
        name: Symbol,
        ty: &Option<TypeExpr>,
        init: Option<ExprId>,
        is_const: bool,
        span: Span,
        module: &Module,
    ) {
        // A `const var` with no initialiser is always `nothing` and can never
        // be changed — that is never useful.
        if is_const && init.is_none() {
            let n = self.interner.resolve(name).to_string();
            self.emit_error(
                fidan_diagnostics::diag_code!("E0104"),
                format!("constant `{n}` must have an initializer"),
                span,
            );
        }

        let declared = ty.as_ref().map(|t| self.resolve_type_expr(t));

        let inferred = if let Some(init_id) = init {
            let actual = self.infer_expr(init_id, module);
            if let Some(ref dt) = declared {
                if !dt.is_assignable_from(&actual) {
                    let (d, a) = (self.ty_name(dt), self.ty_name(&actual));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0201"),
                        format!("type mismatch: expected `{d}`, found `{a}`"),
                        span,
                    );
                }
            }
            actual
        } else {
            declared.clone().unwrap_or(FidanType::Nothing)
        };

        // When the annotation is the bare `tuple` keyword (no element types),
        // prefer the more-specific type inferred from the initializer.
        let final_ty = match declared {
            Some(FidanType::Tuple(ref e)) if e.is_empty() => inferred,
            Some(d) => d,
            None => inferred,
        };
        self.table.define(
            name,
            SymbolInfo {
                kind: SymbolKind::Var,
                ty: final_ty,
                span,
                is_mutable: !is_const,
                initialized: if init.is_some() {
                    Initialized::Yes
                } else {
                    Initialized::No
                },
            },
        );
    }

    /// Emit E0103 if `target` resolves to an immutable (`const var`) symbol.
    fn check_const_assign(&mut self, target_id: ExprId, span: Span, module: &Module) {
        let expr = module.arena.get_expr(target_id).clone();
        if let Expr::Ident { name, .. } = expr {
            if let Some(info) = self.table.lookup(name) {
                if !info.is_mutable {
                    let n = self.interner.resolve(name).to_string();
                    let def_span = info.span;
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0103"),
                            format!("cannot assign to constant `{n}`"),
                            span,
                        )
                        .with_label(Label::secondary(def_span, "defined as `const var` here")),
                    );
                }
            }
        }
    }

    // ── Expression inference ──────────────────────────────────────────────

    /// Infer the type of `expr_id`, record the result in `expr_types`, and return it.
    pub(crate) fn infer_expr(&mut self, expr_id: ExprId, module: &Module) -> FidanType {
        let ty = self.infer_expr_inner(expr_id, module);
        self.expr_types.insert(expr_id, ty.clone());
        ty
    }

    fn infer_expr_inner(&mut self, expr_id: ExprId, module: &Module) -> FidanType {
        let expr = module.arena.get_expr(expr_id).clone();
        match expr {
            Expr::IntLit { .. } => FidanType::Integer,
            Expr::FloatLit { .. } => FidanType::Float,
            Expr::BoolLit { .. } => FidanType::Boolean,
            Expr::Nothing { .. } => FidanType::Nothing,
            Expr::StrLit { .. } | Expr::StringInterp { .. } => FidanType::String,

            Expr::Ident { name, span } => {
                // `_` is the universal wildcard — it matches any type and is never
                // declared as a variable (used in check-arm patterns).
                let resolved = self.interner.resolve(name);
                if resolved.as_ref() == "_" {
                    return FidanType::Dynamic;
                }
                match self.table.lookup(name) {
                    Some(info) => info.ty.clone(),
                    None => {
                        let s = resolved.to_string();
                        // Collect every known symbol name for "did you mean?" suggestion.
                        let candidates: Vec<String> = self
                            .table
                            .all_names()
                            .map(|sym| self.interner.resolve(sym).to_string())
                            .collect();
                        let candidate_refs: Vec<&str> =
                            candidates.iter().map(String::as_str).collect();
                        let mut diag = Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0101"),
                            format!("undefined name `{s}`"),
                            span,
                        )
                        .with_label(Label::primary(span, "unknown name"));
                        if let Some(best) = FixEngine::suggest_name(&s, candidate_refs.into_iter())
                        {
                            diag = diag.with_suggestion(Suggestion::fix(
                                format!("did you mean `{best}`?"),
                                span,
                                best,
                                Confidence::High,
                            ));
                        }
                        self.diags.push(diag);
                        FidanType::Error
                    }
                }
            }

            Expr::This { .. } => self.this_ty.clone().unwrap_or(FidanType::Dynamic),

            Expr::Parent { .. } => {
                if let Some(FidanType::Object(sym)) = self.this_ty.clone() {
                    if let Some(parent) = self.objects.get(&sym).and_then(|o| o.parent) {
                        return FidanType::Object(parent);
                    }
                }
                FidanType::Dynamic
            }

            Expr::Field {
                object,
                field,
                span,
            } => {
                let obj_ty = self.infer_expr(object, module);
                self.resolve_field(&obj_ty, field, span)
            }

            Expr::Call {
                callee,
                ref args,
                span,
            } => {
                // Infer arg types first (for side effects / nested errors)
                let args_clone: Vec<_> = args.iter().map(|a| (a.name, a.value)).collect();
                for (_, val) in &args_clone {
                    self.infer_expr(*val, module);
                }
                self.infer_call(callee, &args_clone, span, module)
            }

            Expr::Index { object, index, .. } => {
                let obj_ty = self.infer_expr(object, module);
                self.infer_expr(index, module);
                match obj_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::Dict(_, v) => *v,
                    FidanType::String => FidanType::String,
                    _ => FidanType::Dynamic,
                }
            }

            Expr::Binary { op, lhs, rhs, span } => {
                let l = self.infer_expr(lhs, module);
                let r = self.infer_expr(rhs, module);
                self.binary_result(op, &l, &r, span)
            }

            Expr::Unary { op, operand, .. } => {
                let inner = self.infer_expr(operand, module);
                match op {
                    UnOp::Neg => inner,
                    UnOp::Not => FidanType::Boolean,
                }
            }

            Expr::NullCoalesce { lhs, rhs, .. } => {
                let l = self.infer_expr(lhs, module);
                let r = self.infer_expr(rhs, module);
                // If lhs is definitely Nothing, result is rhs type.
                if l.is_nothing() { r } else { l }
            }

            Expr::Ternary {
                condition,
                then_val,
                else_val,
                ..
            } => {
                self.infer_expr(condition, module);
                let then_ty = self.infer_expr(then_val, module);
                self.infer_expr(else_val, module);
                then_ty
            }

            Expr::Assign {
                target,
                value,
                span,
            } => {
                let rhs = self.infer_expr(value, module);
                let lhs = self.infer_expr(target, module);
                if !lhs.is_assignable_from(&rhs) && !lhs.is_error() {
                    let (l, r) = (self.ty_name(&lhs), self.ty_name(&rhs));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0201"),
                        format!("type mismatch: cannot assign `{r}` to `{l}`"),
                        span,
                    );
                }
                rhs
            }

            Expr::CompoundAssign {
                op,
                target,
                value,
                span,
            } => {
                let rhs = self.infer_expr(value, module);
                let lhs = self.infer_expr(target, module);
                self.binary_result(op, &lhs, &rhs, span)
            }

            Expr::Spawn { expr, .. } => {
                let inner = self.infer_expr(expr, module);
                FidanType::Pending(Box::new(inner))
            }

            Expr::Await { expr, .. } => {
                let inner = self.infer_expr(expr, module);
                match inner {
                    FidanType::Pending(t) => *t,
                    other => other,
                }
            }

            Expr::List { elements, .. } => {
                let elem = elements
                    .first()
                    .map(|&id| self.infer_expr(id, module))
                    .unwrap_or(FidanType::Dynamic);
                for &id in elements.iter().skip(1) {
                    self.infer_expr(id, module);
                }
                FidanType::List(Box::new(elem))
            }

            Expr::Dict { entries, .. } => {
                for (k, v) in &entries {
                    self.infer_expr(*k, module);
                    self.infer_expr(*v, module);
                }
                FidanType::Dict(Box::new(FidanType::String), Box::new(FidanType::Dynamic))
            }

            Expr::Tuple { elements, .. } => {
                let types = elements
                    .iter()
                    .map(|&e| self.infer_expr(e, module))
                    .collect();
                FidanType::Tuple(types)
            }

            Expr::Check {
                scrutinee, arms, ..
            } => {
                self.infer_expr(scrutinee, module);
                for arm in arms {
                    self.infer_expr(arm.pattern, module);
                    self.check_arm_body(&arm.body, module);
                }
                FidanType::Dynamic
            }

            Expr::Error { .. } => FidanType::Error,
        }
    }

    // ── Field resolution ──────────────────────────────────────────────────

    fn resolve_field(&mut self, ty: &FidanType, field: Symbol, span: Span) -> FidanType {
        match ty {
            FidanType::Object(sym) => {
                let sym = *sym;
                // Try field, then method, then walk inheritance chain.
                if let Some(ft) = self
                    .objects
                    .get(&sym)
                    .and_then(|o| o.fields.get(&field))
                    .cloned()
                {
                    return ft;
                }
                if let Some(_) = self.objects.get(&sym).and_then(|o| o.methods.get(&field)) {
                    return FidanType::Function;
                }
                if let Some(parent_sym) = self.objects.get(&sym).and_then(|o| o.parent) {
                    return self.resolve_field(&FidanType::Object(parent_sym), field, span);
                }
                // Unknown field — return Dynamic to avoid spam for now.
                FidanType::Dynamic
            }
            FidanType::String => {
                let f = self.interner.resolve(field);
                if matches!(f.as_ref(), "length" | "len") {
                    FidanType::Integer
                } else {
                    FidanType::Dynamic
                }
            }
            FidanType::List(_) => {
                let f = self.interner.resolve(field);
                if matches!(f.as_ref(), "length" | "len") {
                    FidanType::Integer
                } else {
                    FidanType::Dynamic
                }
            }
            FidanType::Dynamic | FidanType::Unknown | FidanType::Error => FidanType::Dynamic,
            _ => FidanType::Dynamic,
        }
    }

    // ── Call return-type inference ────────────────────────────────────────

    fn infer_call(
        &mut self,
        callee_id: ExprId,
        args: &[(Option<Symbol>, ExprId)],
        span: Span,
        module: &Module,
    ) -> FidanType {
        let callee = module.arena.get_expr(callee_id).clone();
        match callee {
            Expr::Ident {
                name,
                span: callee_span,
            } => {
                let name_str = self.interner.resolve(name).to_string();
                // Built-in return types
                match name_str.as_str() {
                    "print" | "println" | "eprint" => return FidanType::Nothing,
                    "input" => return FidanType::String,
                    "len" => return FidanType::Integer,
                    "type" => return FidanType::String,
                    "string" => return FidanType::String,
                    "integer" => return FidanType::Integer,
                    "float" | "sqrt" => return FidanType::Float,
                    "boolean" => return FidanType::Boolean,
                    "floor" | "ceil" | "round" => return FidanType::Integer,
                    "abs" | "max" | "min" => return FidanType::Dynamic,
                    _ => {}
                }
                // Look up in symbol table: Object constructor, user action, or builtin.
                match self.table.lookup(name).map(|i| i.kind) {
                    Some(SymbolKind::Object) => {
                        self.check_required_params(name, args, span);
                        return FidanType::Object(name);
                    }
                    Some(_) => {
                        // Action, Var holding a function, etc. — valid call, return type unknown.
                        return FidanType::Dynamic;
                    }
                    None => {
                        // Not a builtin, not declared — undefined callee.
                        let s = name_str.clone();
                        let candidates: Vec<String> = self
                            .table
                            .all_names()
                            .map(|sym| self.interner.resolve(sym).to_string())
                            .collect();
                        let candidate_refs: Vec<&str> =
                            candidates.iter().map(String::as_str).collect();
                        let mut diag = Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0101"),
                            format!("undefined name `{s}`"),
                            callee_span,
                        )
                        .with_label(Label::primary(callee_span, "unknown name"));
                        if let Some(best) = FixEngine::suggest_name(&s, candidate_refs.into_iter())
                        {
                            diag = diag.with_suggestion(Suggestion::fix(
                                format!("did you mean `{best}`?"),
                                callee_span,
                                best,
                                Confidence::High,
                            ));
                        }
                        self.diags.push(diag);
                        return FidanType::Error;
                    }
                }
            }

            Expr::Field { object, field, .. } => {
                let recv = self.infer_expr(object, module);
                match recv {
                    FidanType::Object(sym) => {
                        // Walk inheritance for method return type
                        self.method_return(&sym, field)
                    }
                    _ => FidanType::Dynamic,
                }
            }

            _ => {
                self.infer_expr(callee_id, module);
                FidanType::Dynamic
            }
        }
    }

    fn method_return(&self, obj_sym: &Symbol, method: Symbol) -> FidanType {
        if let Some(info) = self.objects.get(obj_sym) {
            if let Some(m) = info.methods.get(&method) {
                return m.return_ty.clone();
            }
            if let Some(parent) = info.parent {
                return self.method_return(&parent, method);
            }
        }
        FidanType::Dynamic
    }

    /// Check that all `required` params of `initialize` for `obj_sym` are supplied.
    fn check_required_params(
        &mut self,
        obj_sym: Symbol,
        args: &[(Option<Symbol>, ExprId)],
        span: Span,
    ) {
        // Intern "initialize" before borrowing self.objects
        let init_sym = self.interner.intern("initialize");
        let params: Option<Vec<ParamInfo>> = self
            .objects
            .get(&obj_sym)
            .and_then(|o| o.methods.get(&init_sym))
            .map(|m| m.params.clone());

        if let Some(params) = params {
            let has_positional = args.iter().any(|(n, _)| n.is_none());
            for p in &params {
                if p.required {
                    let named_ok = args.iter().any(|(n, _)| *n == Some(p.name));
                    if !named_ok && !has_positional {
                        let pname = self.interner.resolve(p.name).to_string();
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0301"),
                            format!("required parameter `{pname}` not provided"),
                            span,
                        );
                    }
                }
            }
        }
    }

    // ── Binary type rules ─────────────────────────────────────────────────

    fn binary_result(
        &mut self,
        op: BinOp,
        lhs: &FidanType,
        rhs: &FidanType,
        span: Span,
    ) -> FidanType {
        // Return Dynamic for operands that are themselves Dynamic/Unknown
        // (we can't know the type yet, so don't warn).
        let either_dynamic = matches!(lhs, FidanType::Dynamic | FidanType::Unknown)
            || matches!(rhs, FidanType::Dynamic | FidanType::Unknown);

        let op_sym = match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Pow => "**",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            _ => "",
        };

        match op {
            BinOp::Add => match (lhs, rhs) {
                (FidanType::String, _) | (_, FidanType::String) => FidanType::String,
                (FidanType::Float, _) | (_, FidanType::Float) => FidanType::Float,
                (FidanType::Integer, FidanType::Integer) => FidanType::Integer,
                _ if either_dynamic => FidanType::Dynamic,
                _ => {
                    let (l, r) = (self.ty_name(lhs), self.ty_name(rhs));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0203"),
                        format!("operator `{op_sym}` cannot be applied to `{l}` and `{r}`"),
                        span,
                    );
                    FidanType::Dynamic
                }
            },
            BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::Pow
            | BinOp::BitXor
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::Shl
            | BinOp::Shr => match (lhs, rhs) {
                (FidanType::Float, _) | (_, FidanType::Float) => FidanType::Float,
                (FidanType::Integer, FidanType::Integer) => FidanType::Integer,
                _ if either_dynamic => FidanType::Dynamic,
                _ => {
                    let (l, r) = (self.ty_name(lhs), self.ty_name(rhs));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0203"),
                        format!("operator `{op_sym}` cannot be applied to `{l}` and `{r}`"),
                        span,
                    );
                    FidanType::Dynamic
                }
            },
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::LtEq
            | BinOp::Gt
            | BinOp::GtEq
            | BinOp::And
            | BinOp::Or => FidanType::Boolean,
            BinOp::Range | BinOp::RangeInclusive => FidanType::List(Box::new(FidanType::Integer)),
        }
    }

    // ── TypeExpr resolution ───────────────────────────────────────────────

    pub(crate) fn resolve_type_expr(&mut self, te: &TypeExpr) -> FidanType {
        match te {
            TypeExpr::Named { name, span } => self.resolve_named_type(*name, *span),
            TypeExpr::Oftype { base, param, span } => {
                let (base_str, base_span) = match base.as_ref() {
                    TypeExpr::Named { name, span: bspan } => (
                        self.interner.resolve(*name).to_lowercase().to_string(),
                        *bspan,
                    ),
                    _ => return FidanType::Unknown,
                };
                let inner = self.resolve_type_expr(param);
                match base_str.as_str() {
                    "list" => FidanType::List(Box::new(inner)),
                    "dict" | "map" => FidanType::Dict(Box::new(FidanType::String), Box::new(inner)),
                    "shared" => FidanType::Shared(Box::new(inner)),
                    "pending" => FidanType::Pending(Box::new(inner)),
                    _ => {
                        // Unknown container base (e.g. `lis oftype integer`)
                        if self.registering {
                            return FidanType::Error;
                        }
                        let candidates = ["list", "dict", "map", "shared", "pending"];
                        let mut diag = Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0105"),
                            format!("undefined type `{base_str}`"),
                            *span,
                        )
                        .with_label(Label::primary(base_span, "unknown type name"));
                        if let Some(best) =
                            FixEngine::suggest_name(&base_str, candidates.iter().copied())
                        {
                            diag = diag.with_suggestion(Suggestion::fix(
                                format!("did you mean `{best}`?"),
                                base_span,
                                best.to_string(),
                                Confidence::High,
                            ));
                        }
                        self.diags.push(diag);
                        FidanType::Error
                    }
                }
            }
            TypeExpr::Dynamic { .. } => FidanType::Dynamic,
            TypeExpr::Nothing { .. } => FidanType::Nothing,
            TypeExpr::Tuple { elements, .. } => {
                let types = elements.iter().map(|e| self.resolve_type_expr(e)).collect();
                FidanType::Tuple(types)
            }
        }
    }

    fn resolve_named_type(&mut self, sym: Symbol, span: Span) -> FidanType {
        let s = self.interner.resolve(sym);
        match s.to_lowercase().as_str() {
            "integer" | "int" => FidanType::Integer,
            "float" | "decimal" => FidanType::Float,
            "boolean" | "bool" => FidanType::Boolean,
            "string" | "text" => FidanType::String,
            "nothing" | "null" | "none" => FidanType::Nothing,
            "dynamic" | "any" | "flexible" => FidanType::Dynamic,
            // Bare container keywords without `oftype` — treat as dynamic rather than erroring
            "list" | "dict" | "map" | "shared" | "pending" | "tuple" => FidanType::Dynamic,
            _ => {
                // Might be a user-defined object type
                if self.objects.contains_key(&sym) {
                    return FidanType::Object(sym);
                }
                // Unknown type — emit a diagnostic and suppress cascades
                let bad = s.to_string();
                if self.registering {
                    // In Pass 1 we just return Error as a placeholder;
                    // Pass 2 will emit the real E0105 diagnostic.
                    return FidanType::Error;
                }
                let builtin_names = [
                    "integer", "float", "boolean", "string", "nothing", "dynamic", "list", "dict",
                    "map", "shared", "pending",
                ];
                let obj_names: Vec<String> = self
                    .objects
                    .keys()
                    .map(|k| self.interner.resolve(*k).to_string())
                    .collect();
                let mut candidates: Vec<&str> = builtin_names.iter().copied().collect();
                for n in &obj_names {
                    candidates.push(n.as_str());
                }
                let mut diag = Diagnostic::error(
                    fidan_diagnostics::diag_code!("E0105"),
                    format!("undefined type `{bad}`"),
                    span,
                )
                .with_label(Label::primary(span, "unknown type name"));
                if let Some(best) = FixEngine::suggest_name(&bad, candidates.into_iter()) {
                    diag = diag.with_suggestion(Suggestion::fix(
                        format!("did you mean `{best}`?"),
                        span,
                        best,
                        Confidence::High,
                    ));
                }
                self.diags.push(diag);
                FidanType::Error
            }
        }
    }

    fn build_action_info(
        &mut self,
        params: &[Param],
        return_ty: &Option<TypeExpr>,
        span: Span,
    ) -> ActionInfo {
        let param_infos: Vec<ParamInfo> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_type_expr(&p.ty.clone());
                ParamInfo {
                    name: p.name,
                    ty,
                    required: p.required,
                    has_default: p.default.is_some(),
                }
            })
            .collect();
        let ret_ty = return_ty
            .as_ref()
            .map(|t| self.resolve_type_expr(&t.clone()))
            .unwrap_or(FidanType::Nothing);
        ActionInfo {
            params: param_infos,
            return_ty: ret_ty,
            span,
        }
    }

    // ── Scope helpers ─────────────────────────────────────────────────────

    fn inject_this_and_parent(
        &mut self,
        this_ty: FidanType,
        parent_sym: Option<Symbol>,
        file: FileId,
    ) {
        self.inject_this_binding(this_ty, file);
        if let Some(p) = parent_sym {
            let dummy = self.dummy_span();
            let parent = self.interner.intern("parent");
            self.table.define(
                parent,
                SymbolInfo {
                    kind: SymbolKind::Var,
                    ty: FidanType::Object(p),
                    span: dummy,
                    is_mutable: false,
                    initialized: Initialized::Yes,
                },
            );
        }
    }

    fn inject_this_binding(&mut self, ty: FidanType, _file: FileId) {
        let dummy = self.dummy_span();
        let this = self.interner.intern("this");
        self.table.define(
            this,
            SymbolInfo {
                kind: SymbolKind::Var,
                ty,
                span: dummy,
                is_mutable: false,
                initialized: Initialized::Yes,
            },
        );
    }

    // ── Utility ───────────────────────────────────────────────────────────

    fn dummy_span(&self) -> Span {
        Span::new(self.file_id, 0, 0)
    }

    fn emit_error(
        &mut self,
        code: fidan_diagnostics::DiagCode,
        message: impl Into<String>,
        span: Span,
    ) {
        self.diags.push(Diagnostic::error(code, message, span));
    }

    fn emit_warning(
        &mut self,
        code: fidan_diagnostics::DiagCode,
        message: impl Into<String>,
        span: Span,
    ) {
        self.diags.push(Diagnostic::warning(code, message, span));
    }

    /// Emit W2002 if `expr_id` is a bare literal with no side effects.
    ///
    /// Bare literals (`42`, `"hello"`, `true`, `nothing`) as standalone
    /// statements are almost always a mistake — either a typo or leftover
    /// debug code.  Identifiers and calls are intentional and not warned.
    fn warn_bare_literal(&mut self, expr_id: ExprId, module: &Module) {
        match module.arena.get_expr(expr_id) {
            Expr::IntLit { span, .. }
            | Expr::FloatLit { span, .. }
            | Expr::BoolLit { span, .. }
            | Expr::StrLit { span, .. }
            | Expr::Nothing { span } => {
                self.emit_warning(
                    fidan_diagnostics::diag_code!("W2002"),
                    "bare literal statement has no effect — did you mean to assign or print it?",
                    *span,
                );
            }
            Expr::Ident { name, span } => {
                let name = *name;
                let span = *span;
                if let Some(info) = self.table.lookup(name) {
                    if matches!(info.kind, SymbolKind::BuiltinAction | SymbolKind::Action) {
                        let n = self.interner.resolve(name).to_string();
                        self.emit_warning(
                            fidan_diagnostics::diag_code!("W2003"),
                            format!("bare reference to action `{n}` has no effect — did you mean to call it with `{n}(...)`?"),
                            span,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn ty_name(&self, ty: &FidanType) -> String {
        ty.display_name(&|sym| self.interner.resolve(sym).to_string())
    }
}
