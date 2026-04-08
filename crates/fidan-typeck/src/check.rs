#![allow(dead_code)]
use crate::scope::{ConstValue, Initialized, ScopeKind, SymbolInfo, SymbolKind, SymbolTable};
use crate::types::FidanType;
use fidan_ast::{
    AstArena, BinOp, Decorator, Expr, ExprId, Item, Module, Param, Stmt, StmtId, TypeExpr, UnOp,
};
use fidan_config::{BUILTIN_BINDINGS, BUILTIN_DECORATORS, BuiltinReturnKind, builtin_return_kind};
use fidan_diagnostics::{Confidence, Diagnostic, FixEngine, Label, Suggestion};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::{FileId, Span};
use fidan_stdlib::{ReceiverBuiltinKind, ReceiverReturnKind, StdlibTypeSpec};
use rustc_hash::FxHashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternAbiKind {
    Native,
    Fidan,
}

#[derive(Debug, Clone)]
struct ExternSpec {
    lib: String,
    symbol: String,
    link: Option<String>,
    abi: ExternAbiKind,
    span: Span,
}

// ── Data structures ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: Symbol,
    pub ty: FidanType,
    pub certain: bool,
    /// `true` when the `optional` keyword was written — the param may be omitted at the call site.
    pub optional: bool,
    pub has_default: bool,
}

#[derive(Debug, Clone)]
pub struct ActionInfo {
    pub params: Vec<ParamInfo>,
    pub return_ty: FidanType,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumInfo {
    /// `(variant_name, payload_arity)` pairs.
    pub variants: Vec<(Symbol, usize)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub fields: FxHashMap<Symbol, FidanType>,
    pub methods: FxHashMap<Symbol, ActionInfo>,
    pub parent: Option<Symbol>,
    pub span: Span,
}

struct ActionBody<'a> {
    params: &'a [fidan_ast::Param],
    return_ty: &'a Option<TypeExpr>,
    body: &'a [StmtId],
    inject_this: Option<FidanType>,
    implicit_return_ty: Option<FidanType>,
    span: Span,
}

#[derive(Debug, Clone, Copy)]
struct CallArgInfo {
    name: Option<Symbol>,
    value: ExprId,
    span: Span,
}

struct ExternActionContext<'a> {
    params: &'a [Param],
    return_ty: &'a Option<TypeExpr>,
    body: &'a [StmtId],
    decorators: &'a [Decorator],
    is_parallel: bool,
    has_receiver: bool,
    is_extension: bool,
    span: Span,
}

#[derive(Debug, Clone, Copy)]
struct ImportBinding {
    sym: Symbol,
    span: Span,
    grouped: bool,
}

#[derive(Debug, Clone, Copy)]
struct SeenImport {
    span: Span,
    grouped: bool,
    re_export: bool,
}

#[derive(Debug, Clone, Copy)]
struct StdlibImportInfo {
    module: Symbol,
    export: Symbol,
}

#[derive(Debug, Clone)]
struct FlowSummary {
    falls_through: bool,
    return_ty: Option<FidanType>,
}

// ── TypeChecker ───────────────────────────────────────────────────────────────

pub struct TypeChecker {
    pub(crate) interner: Arc<SymbolInterner>,
    table: SymbolTable,
    objects: FxHashMap<Symbol, ObjectInfo>,
    enums: FxHashMap<Symbol, EnumInfo>,
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
    /// Lexically-scoped nested action signatures, one map per active scope.
    local_actions: Vec<FxHashMap<Symbol, ActionInfo>>,
    /// `@deprecated` markers for lexically-scoped nested actions.
    local_deprecated_actions: Vec<rustc_hash::FxHashSet<Symbol>>,
    /// Set of action names decorated with `@deprecated`.
    /// Used by `infer_call` to emit W2005 at call sites.
    deprecated_actions: rustc_hash::FxHashSet<Symbol>,
    /// Non-call field / method accesses that hit a cross-module parent during
    /// `resolve_field`.  The LSP validates them cross-document.
    pub cross_module_field_accesses: Vec<(String, String, Span)>,
    /// Method call sites where the method lives in a cross-module parent.
    /// The LSP validates argument types against the cross-document signature.
    pub cross_module_call_sites: Vec<crate::CrossModuleCallSite>,
    /// Non-export import bindings registered during Pass 1.
    /// Checked after Pass 2 to detect unused imports.
    import_bindings: Vec<ImportBinding>,
    /// Set of all symbols bound by import statements, including `export use`.
    import_syms: rustc_hash::FxHashSet<Symbol>,
    /// First kept import for each bound symbol in the current module.
    seen_imports: FxHashMap<Symbol, SeenImport>,
    /// Specific `use std.module.member` imports keyed by the bound local symbol.
    stdlib_imports: FxHashMap<Symbol, StdlibImportInfo>,
    /// `use std.module` namespace bindings keyed by the bound local symbol.
    stdlib_namespace_imports: FxHashMap<Symbol, Symbol>,
    /// Every symbol that was successfully resolved by an `Expr::Ident` node.
    /// Used to determine which import bindings are unreferenced.
    referenced_names: rustc_hash::FxHashSet<Symbol>,
}

impl TypeChecker {
    pub fn new(interner: Arc<SymbolInterner>, file_id: FileId) -> Self {
        let mut tc = Self {
            interner,
            table: SymbolTable::new(),
            objects: FxHashMap::default(),
            enums: FxHashMap::default(),
            diags: vec![],
            current_return_ty: None,
            this_ty: None,
            file_id,
            is_repl: false,
            registering: false,
            expr_types: FxHashMap::default(),
            actions: FxHashMap::default(),
            local_actions: vec![FxHashMap::default()],
            local_deprecated_actions: vec![rustc_hash::FxHashSet::default()],
            deprecated_actions: rustc_hash::FxHashSet::default(),
            cross_module_field_accesses: vec![],
            cross_module_call_sites: vec![],
            import_bindings: vec![],
            import_syms: rustc_hash::FxHashSet::default(),
            seen_imports: FxHashMap::default(),
            stdlib_imports: FxHashMap::default(),
            stdlib_namespace_imports: FxHashMap::default(),
            referenced_names: rustc_hash::FxHashSet::default(),
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
        for &name in BUILTIN_BINDINGS {
            let sym = self.interner.intern(name);
            self.table.define(
                sym,
                SymbolInfo {
                    kind: SymbolKind::BuiltinAction,
                    ty: FidanType::Function,
                    span: dummy,
                    is_mutable: false,
                    initialized: Initialized::Yes,
                    const_value: None,
                },
            );
        }
    }

    // ── Pre-registration (for cross-file imports) ─────────────────────────

    /// Pre-register a top-level action from an already-lowered imported file.
    ///
    /// Must be called before `check_module` so the main file's type-checker
    /// sees the imported function as a known binding.
    pub fn pre_register_action(&mut self, name: Symbol, info: ActionInfo) {
        let span = info.span;
        self.actions.insert(name, info);
        self.table.define(
            name,
            SymbolInfo {
                kind: SymbolKind::Action,
                ty: FidanType::Function,
                span,
                is_mutable: false,
                initialized: Initialized::Yes,
                const_value: None,
            },
        );
    }

    /// Pre-register an object type from an already-lowered imported file.
    pub fn pre_register_object(&mut self, name: Symbol, info: ObjectInfo) {
        let span = info.span;
        self.objects.insert(name, info);
        self.table.define(
            name,
            SymbolInfo {
                kind: SymbolKind::Object,
                ty: FidanType::ClassType(name),
                span,
                is_mutable: false,
                initialized: Initialized::Yes,
                const_value: None,
            },
        );
    }

    /// Pre-register an object from raw field/method iterators (avoids requiring
    /// `FxHashMap` in the caller).
    pub fn pre_register_object_data(
        &mut self,
        name: Symbol,
        parent: Option<Symbol>,
        span: Span,
        fields: impl IntoIterator<Item = (Symbol, FidanType)>,
        methods: impl IntoIterator<Item = (Symbol, ActionInfo)>,
    ) {
        let info = ObjectInfo {
            fields: fields.into_iter().collect(),
            methods: methods.into_iter().collect(),
            parent,
            span,
        };
        self.pre_register_object(name, info);
    }

    /// Pre-register a module-level global variable from an already-lowered imported file.
    pub fn pre_register_global(&mut self, name: Symbol, ty: FidanType, is_const: bool, span: Span) {
        self.table.define(
            name,
            SymbolInfo {
                kind: SymbolKind::Var,
                ty,
                span,
                is_mutable: !is_const,
                initialized: Initialized::Yes,
                const_value: None,
            },
        );
    }

    /// Pre-register a stdlib namespace or free-function binding coming from an
    /// `export use std.X` declaration in an imported file.
    ///
    /// This is the cross-file equivalent of the `Item::Use` branch in
    /// `register_top_level` — it binds the alias symbol as `Dynamic` so the
    /// main file's typechecker doesn't emit E0101 for accesses like `math.sqrt`.
    pub fn pre_register_namespace(&mut self, alias: &str) {
        let sym = self.interner.intern(alias);
        let dummy = self.dummy_span();
        self.table.define(
            sym,
            SymbolInfo {
                kind: SymbolKind::Var,
                ty: FidanType::Dynamic,
                span: dummy,
                is_mutable: false,
                initialized: Initialized::Yes,
                const_value: None,
            },
        );
    }

    // ── Public entry point ────────────────────────────────────────────────

    /// Run the full type checker over `module`.  Returns all diagnostics.
    pub fn check_module(&mut self, module: &Module) {
        // Clear per-module state so that multi-module programs don't bleed
        // @deprecated registrations from one module into another.
        self.deprecated_actions.clear();
        self.import_bindings.clear();
        self.import_syms.clear();
        self.seen_imports.clear();
        self.referenced_names.clear();

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

        // Pass 3: detect unused imports (skipped in REPL — imports persist
        // across lines and would spuriously fire on every input).
        if !self.is_repl {
            self.check_unused_imports();
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
            enums: self.enums,
            actions: self.actions,
            cross_module_field_accesses: self.cross_module_field_accesses,
            cross_module_call_sites: self.cross_module_call_sites,
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
        if let Some(&item_id) = module.items.iter().next_back() {
            let item = module.arena.get_item(item_id).clone();
            if let Item::ExprStmt(expr_id) = item {
                let ty = self.infer_expr(expr_id, module);
                let interner = Arc::clone(&self.interner);
                return Some(ty.display_name(&|sym| interner.resolve(sym).to_string()));
            }
            // Stop at the first non-ExprStmt from the end so that
            //   `:type var x = 5` reports nothing rather than panicking.
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
                if self.reject_reserved_builtin_binding(*name, *span) {
                    return;
                }
                let mut obj = ObjectInfo {
                    fields: FxHashMap::default(),
                    methods: FxHashMap::default(),
                    // Store only the last segment (local object name).
                    // For cross-module paths like `module.Foo`, this is `Foo`.
                    parent: parent.as_ref().and_then(|p| p.last().copied()),
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
                // Duplicate / import-conflict check (E0109).
                if let Some(existing) = self.objects.get(name) {
                    let n = self.interner.resolve(*name).to_string();
                    let first_span = existing.span;
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!("object `{n}` is already defined"),
                            *span,
                        )
                        .with_label(Label::secondary(first_span, "first defined here")),
                    );
                } else if let Some(prev) = self.table.lookup_current_scope(*name) {
                    let n = self.interner.resolve(*name).to_string();
                    let is_import = self.seen_imports.contains_key(name);
                    let note = if is_import {
                        "imported here — use an alias: `use ... as other_name`"
                    } else {
                        "first bound here"
                    };
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!(
                                "object `{n}` conflicts with an existing binding in this scope"
                            ),
                            *span,
                        )
                        .with_label(Label::secondary(prev.span, note)),
                    );
                }
                self.objects.insert(*name, obj);
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Object,
                        ty: FidanType::ClassType(*name),
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
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
                if self.reject_reserved_builtin_binding(*name, *span) {
                    return;
                }
                // Record the action's full signature for HIR lowering.
                let info = self.build_action_info(params, return_ty, *span);
                // Duplicate / import-conflict check (E0109).
                if let Some(existing) = self.actions.get(name) {
                    let n = self.interner.resolve(*name).to_string();
                    let first_span = existing.span;
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!("action `{n}` is already defined"),
                            *span,
                        )
                        .with_label(Label::secondary(first_span, "first defined here")),
                    );
                } else if let Some(prev) = self.table.lookup_current_scope(*name) {
                    let n = self.interner.resolve(*name).to_string();
                    let is_import = self.seen_imports.contains_key(name);
                    let note = if is_import {
                        "imported here — use an alias: `use ... as other_name`"
                    } else {
                        "first bound here"
                    };
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!(
                                "action `{n}` conflicts with an existing binding in this scope"
                            ),
                            *span,
                        )
                        .with_label(Label::secondary(prev.span, note)),
                    );
                }
                self.actions.insert(*name, info);
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Action,
                        ty: FidanType::Function,
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
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
                if self.reject_reserved_builtin_binding(*name, *span) {
                    return;
                }
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Action,
                        ty: FidanType::Function,
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
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
                if self.reject_reserved_builtin_binding(*name, *span) {
                    return;
                }
                // Redeclaration check at pass 1 — fires exactly once on the
                // duplicate `var`, before pass 2 ever runs `check_var_decl`.
                if !self.is_repl
                    && let Some(prev) = self.table.lookup_current_scope(*name)
                {
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
                        const_value: None,
                    },
                );
            }
            // Register stdlib namespace / free-function imports so the type
            // checker doesn't emit E0101 for `use std.io` → `io` usage.
            Item::Use {
                path,
                alias,
                re_export,
                grouped,
                span,
            } => {
                let std_sym = self.interner.intern("std");
                if path.first() == Some(&std_sym) && path.len() >= 2 {
                    // Validate specific-name imports: `use std.io.{name}` or `use std.io.name`.
                    // If the module is known but the name is not exported, emit E0108.
                    let is_valid_export = if path.len() >= 3 {
                        let module_str = self.interner.resolve(path[1]).to_string();
                        let export_str = self.interner.resolve(*path.last().unwrap()).to_string();
                        let exports = fidan_stdlib::module_exports(&module_str);
                        if !exports.is_empty() && !exports.contains(&export_str.as_str()) {
                            self.diags.push(
                                Diagnostic::error(
                                    fidan_diagnostics::diag_code!("E0108"),
                                    format!(
                                        "`{}` is not exported by `std.{}`",
                                        export_str, module_str
                                    ),
                                    *span,
                                )
                                .with_label(Label::primary(*span, "no such export")),
                            );
                            false
                        } else {
                            true
                        }
                    } else {
                        true
                    };
                    if is_valid_export {
                        // Determine which symbol to bind in the user's scope:
                        //   `use std.io`          → bind `io`  (namespace)
                        //   `use std.io.readFile`  → bind `readFile` (free fn)
                        //   `use std.io as myIo`  → bind `myIo`
                        let binding_sym = if let Some(&a) = alias.as_ref() {
                            a
                        } else if path.len() == 2 {
                            path[1]
                        } else {
                            *path.last().unwrap()
                        };
                        if self.report_duplicate_import_binding(
                            binding_sym,
                            *span,
                            *grouped,
                            *re_export,
                        ) {
                            return;
                        }
                        // Import-vs-declaration conflict check (E0109, Case B).
                        if let Some(prev) = self.table.lookup_current_scope(binding_sym)
                            && matches!(prev.kind, SymbolKind::Object | SymbolKind::Action)
                            && prev.span.file == span.file
                        {
                            let n = self.interner.resolve(binding_sym).to_string();
                            let kind_word = if prev.kind == SymbolKind::Object {
                                "object"
                            } else {
                                "action"
                            };
                            self.diags.push(
                                Diagnostic::error(
                                    fidan_diagnostics::diag_code!("E0109"),
                                    format!("import `{n}` conflicts with a top-level {kind_word} declaration — use an alias: `use ... as other_name`"),
                                    *span,
                                )
                                .with_label(Label::secondary(prev.span, "declared here")),
                            );
                        }
                        self.table.define(
                            binding_sym,
                            SymbolInfo {
                                kind: SymbolKind::Var,
                                ty: FidanType::Dynamic,
                                span: *span,
                                is_mutable: false,
                                initialized: Initialized::Yes,
                                const_value: None,
                            },
                        );
                        if path.len() >= 3 {
                            self.stdlib_imports.insert(
                                binding_sym,
                                StdlibImportInfo {
                                    module: path[1],
                                    export: *path.last().unwrap(),
                                },
                            );
                            self.stdlib_namespace_imports.remove(&binding_sym);
                        } else {
                            self.stdlib_imports.remove(&binding_sym);
                            self.stdlib_namespace_imports.insert(binding_sym, path[1]);
                        }
                        self.record_import_binding(binding_sym, *span, *grouped, *re_export);
                    }
                } else if !path.is_empty() && path.first() != Some(&std_sym) {
                    // User module import: `use mymod` / `use mymod.sub` /
                    // `use mymod.{name}` (grouped, folded into path by parser).
                    // Bind the alias, or the last segment for multi-segment paths
                    // (mirrors stdlib: `use std.io.print` binds `print`), or the
                    // sole segment for single-segment paths.
                    let binding_sym = if let Some(&a) = alias.as_ref() {
                        a
                    } else if path.len() >= 2 {
                        *path.last().unwrap()
                    } else {
                        path[0]
                    };
                    let first_str = self.interner.resolve(path[0]);
                    let is_file_path = first_str.starts_with("./")
                        || first_str.starts_with("../")
                        || first_str.starts_with('/')
                        || first_str.ends_with(".fdn");
                    if is_file_path {
                        // File-path import: only bind if an explicit alias was given.
                        // `use "./utils.fdn" as utils` → bind `utils` as Dynamic.
                        // Plain `use "./utils.fdn"` exposes everything flat — no binding.
                        if let Some(&a) = alias.as_ref() {
                            if self.report_duplicate_import_binding(a, *span, false, *re_export) {
                                return;
                            }
                            if let Some(prev) = self.table.lookup_current_scope(a)
                                && matches!(prev.kind, SymbolKind::Object | SymbolKind::Action)
                                && prev.span.file == span.file
                            {
                                let n = self.interner.resolve(a).to_string();
                                let kind_word = if prev.kind == SymbolKind::Object {
                                    "object"
                                } else {
                                    "action"
                                };
                                self.diags.push(
                                    Diagnostic::error(
                                        fidan_diagnostics::diag_code!("E0109"),
                                        format!("import `{n}` conflicts with a top-level {kind_word} declaration — use a different alias"),
                                        *span,
                                    )
                                    .with_label(Label::secondary(prev.span, "declared here")),
                                );
                            }
                            self.table.define(
                                a,
                                SymbolInfo {
                                    kind: SymbolKind::Var,
                                    ty: FidanType::Dynamic,
                                    span: *span,
                                    is_mutable: false,
                                    initialized: Initialized::Yes,
                                    const_value: None,
                                },
                            );
                            self.record_import_binding(a, *span, false, *re_export);
                        }
                    } else {
                        if self.report_duplicate_import_binding(
                            binding_sym,
                            *span,
                            *grouped,
                            *re_export,
                        ) {
                            return;
                        }
                        // Import-vs-declaration conflict check (E0109, Case B).
                        if let Some(prev) = self.table.lookup_current_scope(binding_sym)
                            && matches!(prev.kind, SymbolKind::Object | SymbolKind::Action)
                            && prev.span.file == span.file
                        {
                            let n = self.interner.resolve(binding_sym).to_string();
                            let kind_word = if prev.kind == SymbolKind::Object {
                                "object"
                            } else {
                                "action"
                            };
                            self.diags.push(
                                Diagnostic::error(
                                    fidan_diagnostics::diag_code!("E0109"),
                                    format!("import `{n}` conflicts with a top-level {kind_word} declaration — use an alias: `use ... as other_name`"),
                                    *span,
                                )
                                .with_label(Label::secondary(prev.span, "declared here")),
                            );
                        }
                        self.table.define(
                            binding_sym,
                            SymbolInfo {
                                kind: SymbolKind::Var,
                                ty: FidanType::Dynamic,
                                span: *span,
                                is_mutable: false,
                                initialized: Initialized::Yes,
                                const_value: None,
                            },
                        );
                        self.record_import_binding(binding_sym, *span, *grouped, *re_export);
                    }
                }
            }
            Item::EnumDecl {
                name,
                variants,
                span,
            } => {
                let info = EnumInfo {
                    variants: variants
                        .iter()
                        .map(|v| (v.name, v.payload_types.len()))
                        .collect(),
                    span: *span,
                };
                // Duplicate / import-conflict check (E0109).
                if let Some(existing) = self.enums.get(name) {
                    let n = self.interner.resolve(*name).to_string();
                    let first_span = existing.span;
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!("enum `{n}` is already defined"),
                            *span,
                        )
                        .with_label(Label::secondary(first_span, "first defined here")),
                    );
                } else if let Some(prev) = self.table.lookup_current_scope(*name) {
                    let n = self.interner.resolve(*name).to_string();
                    let is_import = self.seen_imports.contains_key(name);
                    let note = if is_import {
                        "imported here — use an alias: `use ... as other_name`"
                    } else {
                        "first bound here"
                    };
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!("enum `{n}` conflicts with an existing binding in this scope"),
                            *span,
                        )
                        .with_label(Label::secondary(prev.span, note)),
                    );
                }
                self.enums.insert(*name, info);
                self.table.define(
                    *name,
                    SymbolInfo {
                        kind: SymbolKind::Object,
                        ty: FidanType::Enum(*name),
                        span: *span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
                    },
                );
            }
            Item::ExprStmt(_) | Item::Assign { .. } | Item::Stmt(_) | Item::Destructure { .. } => {}
            // Test blocks are not registered in the symbol table.
            Item::TestDecl { .. } => {}
        }
    }

    // ── Item checking (pass 2) ────────────────────────────────────────────

    fn check_item(&mut self, item: &Item, module: &Module) {
        match item {
            // ── object ──────────────────────────────────────────────────
            Item::ObjectDecl {
                name,
                parent,
                fields,
                methods,
                span,
                ..
            } => {
                if let Some(path) = parent {
                    if path.len() == 1 && path[0] == *name {
                        let pname = self.interner.resolve(*name).to_string();
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0107"),
                            format!("object `{pname}` cannot extend itself"),
                            *span,
                        );
                    } else if path.len() == 1 && !self.objects.contains_key(&path[0]) {
                        let pname = self.interner.resolve(path[0]).to_string();
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0100"),
                            format!("undefined object `{pname}` in `extends` clause"),
                            *span,
                        );
                    } else if path.len() > 1 {
                        // Qualified path (e.g. `module.Foo`): the leading segment is a
                        // module alias imported via `use`.  Mark it as referenced so the
                        // unused-import pass (W1005) doesn't false-positive on it.
                        self.referenced_names.insert(path[0]);
                    }
                    // Single-segment qualified paths that match a known object are fine;
                    // multi-segment cross-module extends cannot be fully verified.
                }

                let obj_ty = FidanType::Object(*name);
                let prev_this = self.this_ty.replace(obj_ty.clone());

                // Determine parent type for `parent` keyword binding inside methods.
                let parent_ty = match parent.as_ref() {
                    Some(path) if path.len() == 1 => Some(FidanType::Object(path[0])),
                    Some(_) => Some(FidanType::Dynamic), // cross-module
                    None => None,
                };

                for field in fields {
                    let _ = self.resolve_type_expr(&field.ty);
                }

                self.push_scope(ScopeKind::Object);
                self.inject_this_and_parent(obj_ty, parent_ty, module.file);

                for &mid in methods {
                    let method = module.arena.get_item(mid).clone();
                    self.check_item(&method, module);
                }

                self.pop_scope();
                self.this_ty = prev_this;
            }

            // ── action / extension action ────────────────────────────────
            Item::ActionDecl {
                name,
                params,
                return_ty,
                body,
                decorators,
                span,
                is_parallel,
                ..
            } => {
                self.check_decorators(decorators, params);
                self.validate_extern_action(
                    module,
                    *name,
                    ExternActionContext {
                        params,
                        return_ty,
                        body,
                        decorators,
                        is_parallel: *is_parallel,
                        has_receiver: self.this_ty.is_some(),
                        is_extension: false,
                        span: *span,
                    },
                );
                // Track @deprecated actions for call-site warnings (W2005).
                if decorators
                    .iter()
                    .any(|d| self.interner.resolve(d.name).as_ref() == "deprecated")
                {
                    self.deprecated_actions.insert(*name);
                }
                if self.has_marker_decorator(decorators, "extern") {
                    if return_ty.is_none()
                        && let Some(info) = self.actions.get_mut(name)
                    {
                        info.return_ty = FidanType::Nothing;
                    }
                    return;
                }
                // A `new` constructor inside an object always returns nothing — the
                // runtime constructs the object itself and discards any return value.
                let sym_new = self.interner.intern("new");
                let is_ctor = *name == sym_new && self.this_ty.is_some();
                let implicit_ret = if is_ctor {
                    Some(FidanType::Nothing)
                } else {
                    None
                };
                // `this_ty` is already set if we're inside an ObjectDecl scope.
                let inferred_return = self.check_action_body(
                    ActionBody {
                        params,
                        return_ty,
                        body,
                        inject_this: None,
                        implicit_return_ty: implicit_ret,
                        span: *span,
                    },
                    module,
                );
                if return_ty.is_none()
                    && let Some(info) = self.actions.get_mut(name)
                {
                    info.return_ty = inferred_return;
                }
            }

            Item::ExtensionAction {
                name,
                extends,
                params,
                return_ty,
                body,
                decorators,
                span,
                is_parallel,
                ..
            } => {
                self.check_decorators(decorators, params);
                self.validate_extern_action(
                    module,
                    *name,
                    ExternActionContext {
                        params,
                        return_ty,
                        body,
                        decorators,
                        is_parallel: *is_parallel,
                        has_receiver: false,
                        is_extension: true,
                        span: *span,
                    },
                );
                // Track @deprecated extension actions.
                if decorators
                    .iter()
                    .any(|d| self.interner.resolve(d.name).as_ref() == "deprecated")
                {
                    self.deprecated_actions.insert(*name);
                }
                let ext_ty = FidanType::Object(*extends);
                let prev_this = self.this_ty.replace(ext_ty.clone());
                if self.has_marker_decorator(decorators, "extern") {
                    self.this_ty = prev_this;
                    return;
                }
                let inferred_return = self.check_action_body(
                    ActionBody {
                        params,
                        return_ty,
                        body,
                        inject_this: Some(ext_ty),
                        implicit_return_ty: None,
                        span: *span,
                    },
                    module,
                );
                if return_ty.is_none()
                    && let Some(obj) = self.objects.get_mut(extends)
                    && let Some(info) = obj.methods.get_mut(name)
                {
                    info.return_ty = inferred_return;
                }
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
                self.check_assignment_target(*target, *span, module);
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

            // Enum declarations are fully processed during Pass 1 (register_top_level).
            Item::EnumDecl { .. } => {}

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
                    if self.reject_reserved_builtin_binding(binding, *span) {
                        continue;
                    }
                    let bty = elem_types.get(i).cloned().unwrap_or(FidanType::Dynamic);
                    self.table.define(
                        binding,
                        SymbolInfo {
                            kind: SymbolKind::Var,
                            ty: bty,
                            span: *span,
                            is_mutable: true,
                            initialized: Initialized::Yes,
                            const_value: None,
                        },
                    );
                }
            }

            // ── test block ───────────────────────────────────────────────────────
            // Type-checked like a parameterless action body.  `assert` / `assert_eq`
            // are already registered as builtins so no special handling is needed.
            Item::TestDecl { body, .. } => {
                self.push_scope(ScopeKind::Block);
                for &sid in body {
                    self.check_stmt(sid, module);
                }
                self.pop_scope();
            }
        }
    }

    fn check_action_body(&mut self, action: ActionBody<'_>, module: &Module) -> FidanType {
        let ActionBody {
            params,
            return_ty,
            body,
            inject_this,
            implicit_return_ty,
            span: action_span,
        } = action;

        let ret = if let Some(implicit) = implicit_return_ty {
            Some(implicit)
        } else {
            return_ty.as_ref().map(|t| self.resolve_type_expr(t))
        };
        let declared_ret = ret.clone();
        // `None` means no annotation → return type is inferred / unconstrained.
        // Only set `current_return_ty` to `Some(T)` when an explicit annotation
        // was written; otherwise any `return value` is accepted.
        let prev_ret = std::mem::replace(&mut self.current_return_ty, ret);

        self.push_scope(ScopeKind::Action);

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
                    const_value: None,
                },
            );
        }

        for param in params {
            if self.reject_reserved_builtin_binding(param.name, param.span) {
                continue;
            }
            let param_ty = self.resolve_type_expr(&param.ty);
            self.table.define(
                param.name,
                SymbolInfo {
                    kind: SymbolKind::Param,
                    ty: param_ty,
                    span: param.span,
                    is_mutable: false,
                    initialized: if param.certain || param.default.is_some() {
                        Initialized::Yes
                    } else {
                        Initialized::Maybe
                    },
                    const_value: None,
                },
            );
        }

        self.check_statements_in_current_scope(body, module, false);

        // Emit E0202 if a non-Nothing return type was declared but the action body
        // has no `return` statement at all.
        if let Some(ref declared) = declared_ret
            && !matches!(declared, FidanType::Nothing | FidanType::Dynamic)
            && !self.all_paths_return(body, module)
        {
            let ret_name = self.ty_name(declared);
            self.emit_error(
                fidan_diagnostics::diag_code!("E0202"),
                format!("not all code paths return a value of type `{ret_name}`"),
                action_span,
            );
        }

        let inferred_return = declared_ret
            .clone()
            .unwrap_or_else(|| self.infer_action_return_type(body, module));

        self.pop_scope();
        self.current_return_ty = prev_ret;
        inferred_return
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
                if self.reject_reserved_builtin_binding(name, span) {
                    return;
                }
                // Local redeclaration check (action bodies have no pass 1).
                if !self.is_repl
                    && let Some(prev) = self.table.lookup_current_scope(name)
                {
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
                self.check_var_decl(name, &ty, init, is_const, span, module);
            }

            Stmt::Assign {
                target,
                value,
                span,
            } => {
                self.check_assignment_target(target, span, module);
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
                            const_value: None,
                        },
                    );
                }
            }

            Stmt::Expr { expr, .. } => {
                self.warn_bare_literal(expr, module);
                self.infer_expr(expr, module);
            }

            Stmt::ActionDecl {
                name,
                params,
                return_ty,
                body,
                decorators,
                is_parallel,
                span,
                ..
            } => {
                if self.reject_reserved_builtin_binding(name, span) {
                    return;
                }

                let info = self.build_action_info(&params, &return_ty, span);
                if let Some(prev) = self.table.lookup_current_scope(name) {
                    let n = self.interner.resolve(name).to_string();
                    self.diags.push(
                        Diagnostic::error(
                            fidan_diagnostics::diag_code!("E0109"),
                            format!(
                                "action `{n}` conflicts with an existing binding in this scope"
                            ),
                            span,
                        )
                        .with_label(Label::secondary(prev.span, "first bound here")),
                    );
                    return;
                }

                let is_deprecated = decorators
                    .iter()
                    .any(|d| self.interner.resolve(d.name).as_ref() == "deprecated");

                self.define_local_action(name, info, is_deprecated);
                self.table.define(
                    name,
                    SymbolInfo {
                        kind: SymbolKind::Action,
                        ty: FidanType::Function,
                        span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
                    },
                );

                self.check_decorators(&decorators, &params);
                self.validate_extern_action(
                    module,
                    name,
                    ExternActionContext {
                        params: &params,
                        return_ty: &return_ty,
                        body: &body,
                        decorators: &decorators,
                        is_parallel,
                        has_receiver: false,
                        is_extension: false,
                        span,
                    },
                );
                if self.has_marker_decorator(&decorators, "extern") {
                    if return_ty.is_none()
                        && let Some(scope) = self.local_actions.last_mut()
                        && let Some(info) = scope.get_mut(&name)
                    {
                        info.return_ty = FidanType::Nothing;
                    }
                    return;
                }

                let inferred_return = self.check_action_body(
                    ActionBody {
                        params: &params,
                        return_ty: &return_ty,
                        body: &body,
                        inject_this: None,
                        implicit_return_ty: None,
                        span,
                    },
                    module,
                );
                if return_ty.is_none()
                    && let Some(scope) = self.local_actions.last_mut()
                    && let Some(info) = scope.get_mut(&name)
                {
                    info.return_ty = inferred_return;
                }
            }

            Stmt::Return { value, span } => {
                let ret = value
                    .map(|id| self.infer_expr(id, module))
                    .unwrap_or(FidanType::Nothing);
                if let Some(expected) = self.current_return_ty.clone()
                    && !expected.is_assignable_from(&ret)
                {
                    let (e, a) = (self.ty_name(&expected), self.ty_name(&ret));
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0202"),
                        format!("return type mismatch: expected `{e}`, found `{a}`"),
                        span,
                    );
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
                let mut unreachable_else_chain = false;
                match self.eval_const_bool(condition, module) {
                    Some(true) => {
                        unreachable_else_chain = true;
                    }
                    Some(false) => {
                        self.warn_unreachable_stmt_ids(&then_body, module);
                    }
                    None => {}
                }
                self.check_block(&then_body, module);
                for ei in &else_ifs {
                    self.infer_expr(ei.condition, module);
                    if unreachable_else_chain {
                        self.warn_unreachable_stmt_ids(&ei.body, module);
                    } else {
                        match self.eval_const_bool(ei.condition, module) {
                            Some(true) => {
                                unreachable_else_chain = true;
                            }
                            Some(false) => {
                                self.warn_unreachable_stmt_ids(&ei.body, module);
                            }
                            None => {}
                        }
                    }
                    self.check_block(&ei.body, module);
                }
                if let Some(body) = &else_body {
                    if unreachable_else_chain {
                        self.warn_unreachable_stmt_ids(body, module);
                    }
                    self.check_block(body, module);
                }
            }

            Stmt::For {
                binding,
                iterable,
                body,
                span,
            } => {
                // E0205: iterable must not be possibly-nothing.
                self.require_non_nullable(
                    iterable,
                    "for-loop iterable (requires list or string)",
                    module,
                );
                let iter_ty = self.infer_expr(iterable, module);
                let elem_ty = match iter_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::String | FidanType::Dynamic => FidanType::Dynamic,
                    FidanType::Unknown | FidanType::Error => FidanType::Unknown,
                    _ => FidanType::Dynamic,
                };
                self.push_scope(ScopeKind::Block);
                if !self.reject_reserved_builtin_binding(binding, span) {
                    self.table.define(
                        binding,
                        SymbolInfo {
                            kind: SymbolKind::Var,
                            ty: elem_ty,
                            span,
                            is_mutable: false,
                            initialized: Initialized::Yes,
                            const_value: None,
                        },
                    );
                }
                for &s in &body {
                    self.check_stmt(s, module);
                }
                self.pop_scope();
            }

            Stmt::While {
                condition, body, ..
            } => {
                self.infer_expr(condition, module);
                if matches!(self.eval_const_bool(condition, module), Some(false)) {
                    self.warn_unreachable_stmt_ids(&body, module);
                }
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
                    self.push_scope(ScopeKind::Block);
                    if let Some(binding) = catch.binding
                        && !self.reject_reserved_builtin_binding(binding, catch.span)
                    {
                        let dummy = self.dummy_span();
                        self.table.define(
                            binding,
                            SymbolInfo {
                                kind: SymbolKind::Var,
                                ty: FidanType::Dynamic, // exceptions are untyped in MVP
                                span: dummy,
                                is_mutable: false,
                                initialized: Initialized::Yes,
                                const_value: None,
                            },
                        );
                    }
                    for &s in &catch.body {
                        self.check_stmt(s, module);
                    }
                    self.pop_scope();
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
                // E0205: iterable must not be possibly-nothing.
                self.require_non_nullable(
                    iterable,
                    "parallel-for iterable (requires list or string)",
                    module,
                );
                let iter_ty = self.infer_expr(iterable, module);
                let elem_ty = match iter_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::String | FidanType::Dynamic => FidanType::Dynamic,
                    FidanType::Unknown | FidanType::Error => FidanType::Unknown,
                    _ => FidanType::Dynamic,
                };
                self.push_scope(ScopeKind::Block);
                if !self.reject_reserved_builtin_binding(binding, span) {
                    self.table.define(
                        binding,
                        SymbolInfo {
                            kind: SymbolKind::Var,
                            ty: elem_ty,
                            span,
                            is_mutable: false,
                            initialized: Initialized::Yes,
                            const_value: None,
                        },
                    );
                }
                for &s in &body {
                    self.check_stmt(s, module);
                }
                self.pop_scope();
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
        self.push_scope(ScopeKind::Block);
        self.check_statements_in_current_scope(stmts, module, false);
        self.pop_scope();
    }

    /// Like `check_block`, but suppresses `W2002` on the final statement when
    /// it is a bare expression — that expression is the *result value* of the
    /// check arm, not a discarded side-effect.
    fn check_arm_body(&mut self, stmts: &[StmtId], module: &Module) {
        self.push_scope(ScopeKind::Block);
        self.check_statements_in_current_scope(stmts, module, true);
        self.pop_scope();
    }

    fn infer_check_arm_body_type(&mut self, stmts: &[StmtId], module: &Module) -> FidanType {
        self.push_scope(ScopeKind::Block);
        self.check_statements_in_current_scope(stmts, module, true);
        let result = stmts
            .last()
            .and_then(|sid| match module.arena.get_stmt(*sid) {
                Stmt::Expr { expr, .. } => self.expr_types.get(expr).cloned(),
                _ => None,
            })
            .unwrap_or(FidanType::Nothing);
        self.pop_scope();
        result
    }

    fn check_statements_in_current_scope(
        &mut self,
        stmts: &[StmtId],
        module: &Module,
        final_expr_is_value: bool,
    ) {
        let last_index = stmts.len().saturating_sub(1);
        let mut dead_code = false;
        for (idx, &sid) in stmts.iter().enumerate() {
            let stmt = module.arena.get_stmt(sid).clone();
            if dead_code {
                self.warn_unreachable_stmt(&stmt);
            }

            let final_expr_result = final_expr_is_value && idx == last_index && !dead_code;
            if final_expr_result {
                match &stmt {
                    Stmt::Expr { expr, .. } => {
                        self.infer_expr(*expr, module);
                    }
                    _ => self.check_stmt(sid, module),
                }
            } else {
                self.check_stmt(sid, module);
            }

            if !dead_code && self.stmt_terminates_all_paths(&stmt, module) {
                dead_code = true;
            }
        }
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

        // Always hide `name` while evaluating its own initializer to prevent
        // self-referential declarations like `var x = x` from silently returning
        // `nothing`.
        //
        // REPL exception: if `x` is already `initialized: Yes` at this point it
        // means a previous check_var_decl in the same pass already assigned a real
        // value (e.g. `var x = 5` processed before `var x = x + 1` in the same
        // accumulated source).  In that case we keep `x` in scope so the
        // re-declaration can reference the old value.
        let hide = !self.is_repl
            || self
                .table
                .lookup_current_scope(name)
                .map(|i| i.initialized != Initialized::Yes)
                .unwrap_or(true);
        if hide {
            let _ = self.table.remove_from_current_scope(name);
        }

        let const_value = if is_const {
            init.and_then(|init_id| self.eval_const_value(init_id, module))
        } else {
            None
        };

        let inferred = if let Some(init_id) = init {
            let actual = self.infer_expr(init_id, module);
            if let Some(ref dt) = declared
                && !dt.is_assignable_from(&actual)
            {
                let (d, a) = (self.ty_name(dt), self.ty_name(&actual));
                self.emit_error(
                    fidan_diagnostics::diag_code!("E0201"),
                    format!("type mismatch: expected `{d}`, found `{a}`"),
                    span,
                );
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
                const_value,
            },
        );
    }

    fn is_reserved_builtin_name(&self, name: Symbol) -> bool {
        let resolved = self.interner.resolve(name);
        BUILTIN_BINDINGS.contains(&resolved.as_ref())
    }

    fn reject_reserved_builtin_binding(&mut self, name: Symbol, span: Span) -> bool {
        if self.is_reserved_builtin_name(name) {
            let resolved = self.interner.resolve(name).to_string();
            self.emit_error(
                fidan_diagnostics::diag_code!("E0109"),
                format!("reserved builtin name `{resolved}` cannot be shadowed"),
                span,
            );
            true
        } else {
            false
        }
    }

    // ── Unused import detection ───────────────────────────────────────────────

    /// Called after Pass 2. Emits W1005 (Note) for every import binding that
    /// was never referenced in any `Expr::Ident` node.
    ///
    /// Non-grouped imports get a `Confidence::High` machine-applicable fix
    /// (delete the statement span).  Grouped-import members get a hint only,
    /// since automatic removal would require rewriting the brace list.
    fn check_unused_imports(&mut self) {
        use fidan_diagnostics::{Label, Suggestion};
        // Drain import_bindings so we don't need to clone referenced_names.
        let bindings = std::mem::take(&mut self.import_bindings);
        for ImportBinding { sym, span, grouped } in bindings {
            if self.referenced_names.contains(&sym) {
                continue;
            }
            let name = self.interner.resolve(sym).to_string();
            let mut diag = Diagnostic::note(
                fidan_diagnostics::diag_code!("W1005"),
                format!("unused import `{name}`"),
                span,
            )
            .with_label(Label::primary(span, "imported here but never used"));
            if !grouped {
                diag.add_suggestion(Suggestion::fix(
                    "remove unused import",
                    span,
                    "",
                    fidan_diagnostics::Confidence::High,
                ));
            } else {
                diag.add_suggestion(Suggestion::hint(
                    "remove this member from the grouped import",
                ));
            }
            self.diags.push(diag);
        }
    }

    fn report_duplicate_import_binding(
        &mut self,
        binding_sym: Symbol,
        span: Span,
        grouped: bool,
        re_export: bool,
    ) -> bool {
        let Some(prev_import) = self.seen_imports.get(&binding_sym).copied() else {
            return false;
        };
        if prev_import.span.file != span.file {
            return false;
        }

        use fidan_diagnostics::{Confidence, Label, Suggestion};

        let name = self.interner.resolve(binding_sym).to_string();
        let keep_current = re_export && !prev_import.re_export;
        let target_span = if keep_current { prev_import.span } else { span };
        let target_grouped = if keep_current {
            prev_import.grouped
        } else {
            grouped
        };
        let mut diag = Diagnostic::warning(
            fidan_diagnostics::diag_code!("W1007"),
            format!("duplicate import `{name}`"),
            target_span,
        )
        .with_label(Label::secondary(prev_import.span, "first imported here"));

        if keep_current {
            diag = diag
                .with_label(Label::primary(
                    prev_import.span,
                    "duplicate plain import here",
                ))
                .with_label(Label::secondary(span, "keep the exported import here"));
        } else {
            diag = diag.with_label(Label::primary(span, "duplicate import here"));
        }

        if !target_grouped {
            diag.add_suggestion(Suggestion::fix(
                "remove duplicate import",
                target_span,
                "",
                Confidence::High,
            ));
        } else {
            diag.add_suggestion(Suggestion::hint(
                "remove this member from the grouped import",
            ));
        }

        self.diags.push(diag);

        if keep_current {
            self.import_bindings.retain(|binding| {
                !(binding.sym == binding_sym
                    && binding.span == prev_import.span
                    && binding.grouped == prev_import.grouped)
            });
            self.seen_imports.insert(
                binding_sym,
                SeenImport {
                    span,
                    grouped,
                    re_export,
                },
            );
            return false;
        }

        true
    }

    fn record_import_binding(&mut self, sym: Symbol, span: Span, grouped: bool, re_export: bool) {
        self.import_syms.insert(sym);
        self.seen_imports.insert(
            sym,
            SeenImport {
                span,
                grouped,
                re_export,
            },
        );
        if !re_export && !self.is_repl {
            self.import_bindings
                .push(ImportBinding { sym, span, grouped });
        }
    }

    /// Emit E0103 if `target` resolves to an immutable (`const var`) symbol.
    fn check_const_assign(&mut self, target_id: ExprId, span: Span, module: &Module) {
        let expr = module.arena.get_expr(target_id).clone();
        if let Expr::Ident { name, .. } = expr
            && let Some(info) = self.table.lookup(name)
            && !info.is_mutable
        {
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

    /// Reject assignment targets that are syntactically valid but semantically
    /// immutable, such as tuple/string/range indexed writes.
    fn check_assignment_target(&mut self, target_id: ExprId, span: Span, module: &Module) {
        self.check_const_assign(target_id, span, module);

        let expr = module.arena.get_expr(target_id).clone();
        if let Expr::Index { object, .. } = expr {
            let object_ty = self.infer_expr(object, module);
            match object_ty {
                FidanType::List(_)
                | FidanType::Dict(_, _)
                | FidanType::Dynamic
                | FidanType::Unknown
                | FidanType::Error => {}
                other => {
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0201"),
                        format!(
                            "cannot assign through index into `{}`; only `list` and `dict` support indexed assignment",
                            self.ty_name(&other)
                        ),
                        span,
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
            Expr::StrLit { .. } => FidanType::String,
            Expr::StringInterp { parts, .. } => {
                for part in parts {
                    if let fidan_ast::InterpPart::Expr(expr) = part {
                        let _ = self.infer_expr(expr, module);
                    }
                }
                FidanType::String
            }

            Expr::Ident { name, span } => {
                // `_` is the universal wildcard — it matches any type and is never
                // declared as a variable (used in check-arm patterns).
                let resolved = self.interner.resolve(name);
                if resolved.as_ref() == "_" {
                    return FidanType::Dynamic;
                }
                match self.table.lookup(name) {
                    Some(info) => {
                        // Record this name as referenced (for unused-import detection).
                        self.referenced_names.insert(name);
                        info.ty.clone()
                    }
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

            Expr::This { span } => {
                if self.this_ty.is_none() {
                    let diag = Diagnostic::error(
                        fidan_diagnostics::diag_code!("E0306"),
                        "`this` can only be used inside an object body, one of its methods, \
                         or an `action … extends ObjectName` extension",
                        span,
                    );
                    self.diags.push(diag);
                    return FidanType::Error;
                }
                self.this_ty.clone().unwrap_or(FidanType::Dynamic)
            }

            Expr::Parent { span } => {
                if self.this_ty.is_none() {
                    let diag = Diagnostic::error(
                        fidan_diagnostics::diag_code!("E0307"),
                        "`parent` can only be used inside an object that extends another object, \
                         or an `action … extends` extension — no object context here",
                        span,
                    );
                    self.diags.push(diag);
                    return FidanType::Error;
                }
                if let Some(FidanType::Object(sym)) = self.this_ty.clone() {
                    if let Some(parent) = self.objects.get(&sym).and_then(|o| o.parent) {
                        return FidanType::Object(parent);
                    }
                    // Object exists but has no parent.
                    let obj_name = self.interner.resolve(sym);
                    let diag = Diagnostic::error(
                        fidan_diagnostics::diag_code!("E0307"),
                        format!(
                            "`parent` used inside `{}`, which does not extend any object",
                            obj_name
                        ),
                        span,
                    );
                    self.diags.push(diag);
                    return FidanType::Error;
                }
                // Extension-action with an inlined this_ty that isn't Object(sym) (edge case).
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
                let args_clone: Vec<_> = args
                    .iter()
                    .map(|a| CallArgInfo {
                        name: a.name,
                        value: a.value,
                        span: a.span,
                    })
                    .collect();
                for arg in &args_clone {
                    self.infer_expr(arg.value, module);
                }
                self.infer_call(callee, &args_clone, span, module)
            }

            Expr::Index { object, index, .. } => {
                // E0205: the collection being indexed must not be possibly-nothing.
                self.require_non_nullable(
                    object,
                    "index target (requires list, dict, or string)",
                    module,
                );
                let obj_ty = self.infer_expr(object, module);
                let index_ty = self.infer_expr(index, module);
                match obj_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::Dict(_, v) => *v,
                    FidanType::String => FidanType::String,
                    FidanType::Tuple(elements) => match (module.arena.get_expr(index), index_ty) {
                        (Expr::IntLit { value, .. }, FidanType::Integer)
                            if *value >= 0 && (*value as usize) < elements.len() =>
                        {
                            elements[*value as usize].clone()
                        }
                        _ => FidanType::Dynamic,
                    },
                    _ => FidanType::Dynamic,
                }
            }

            Expr::Slice {
                target,
                start,
                end,
                step,
                ..
            } => {
                let tgt_ty = self.infer_expr(target, module);
                if let Some(e) = start {
                    self.infer_expr(e, module);
                }
                if let Some(e) = end {
                    self.infer_expr(e, module);
                }
                if let Some(e) = step {
                    self.infer_expr(e, module);
                }
                // A slice of a list is still a list of the same element type;
                // a slice of a string is a string; anything else is dynamic.
                match tgt_ty {
                    FidanType::List(_) => tgt_ty,
                    FidanType::String => FidanType::String,
                    _ => FidanType::Dynamic,
                }
            }

            Expr::Binary { op, lhs, rhs, span } => {
                let l = self.infer_expr(lhs, module);
                let r = self.infer_expr(rhs, module);
                // E0205: check each operand for possibly-nothing values before
                // binary_result sees the types.  Comparisons and logical ops are
                // excluded because they are valid null-guard patterns.
                // String concatenation (`+` where either side is String) is also
                // excluded — any value safely coerces to a string at runtime.
                let is_string_concat = matches!(op, BinOp::Add)
                    && matches!((&l, &r), (FidanType::String, _) | (_, FidanType::String));
                if !is_string_concat {
                    let op_desc: Option<&str> = match op {
                        BinOp::Range | BinOp::RangeInclusive => {
                            Some("range operand (requires `integer`)")
                        }
                        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                            Some("bitwise operand (requires `integer`)")
                        }
                        BinOp::Add
                        | BinOp::Sub
                        | BinOp::Mul
                        | BinOp::Div
                        | BinOp::Rem
                        | BinOp::Pow => Some("arithmetic operand"),
                        // Eq/NotEq/Lt/LtEq/Gt/GtEq/And/Or — valid with nullable values.
                        _ => None,
                    };
                    if let Some(desc) = op_desc {
                        self.require_non_nullable(lhs, desc, module);
                        self.require_non_nullable(rhs, desc, module);
                    }
                }
                self.binary_result(op, &l, &r, span)
            }

            Expr::Unary { op, operand, .. } => {
                // E0205: unary +/- require a concrete number.
                if matches!(op, UnOp::Pos | UnOp::Neg) {
                    self.require_non_nullable(operand, "arithmetic operand", module);
                }
                let inner = self.infer_expr(operand, module);
                match op {
                    UnOp::Pos => inner,
                    UnOp::Neg => inner,
                    UnOp::Not => FidanType::Boolean,
                }
            }

            Expr::NullCoalesce { lhs, rhs, .. } => {
                let l = self.infer_expr(lhs, module);
                let r = self.infer_expr(rhs, module);
                // If lhs is definitely Nothing, result is rhs type.
                if l.is_nothing() {
                    r
                } else {
                    self.merge_possible_types([l, r])
                }
            }

            Expr::Ternary {
                condition,
                then_val,
                else_val,
                ..
            } => {
                self.infer_expr(condition, module);
                let then_ty = self.infer_expr(then_val, module);
                let else_ty = self.infer_expr(else_val, module);
                self.merge_possible_types([then_ty, else_ty])
            }

            Expr::Assign {
                target,
                value,
                span,
            } => {
                self.check_assignment_target(target, span, module);
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
                self.check_assignment_target(target, span, module);
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
                let mut elem = FidanType::Dynamic;
                let mut saw_element = false;
                for id in elements {
                    let ty = self.infer_expr(id, module);
                    elem = if saw_element {
                        self.merge_two_types(&elem, &ty)
                    } else {
                        saw_element = true;
                        ty
                    };
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
                let mut arm_types = Vec::with_capacity(arms.len());
                for arm in arms {
                    self.infer_expr(arm.pattern, module);
                    arm_types.push(self.infer_check_arm_body_type(&arm.body, module));
                }
                self.merge_possible_types(arm_types)
            }

            Expr::Error { .. } => FidanType::Error,

            Expr::ListComp {
                element,
                binding,
                iterable,
                filter,
                span,
            } => {
                let iter_ty = self.infer_expr(iterable, module);
                let elem_ty = match iter_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::String | FidanType::Dynamic => FidanType::Dynamic,
                    _ => FidanType::Dynamic,
                };
                self.push_scope(ScopeKind::Block);
                self.table.define(
                    binding,
                    SymbolInfo {
                        kind: SymbolKind::Var,
                        ty: elem_ty,
                        span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
                    },
                );
                self.infer_expr(element, module);
                if let Some(f) = filter {
                    self.infer_expr(f, module);
                }
                self.pop_scope();
                FidanType::Dynamic
            }
            Expr::DictComp {
                key,
                value,
                binding,
                iterable,
                filter,
                span,
            } => {
                let iter_ty = self.infer_expr(iterable, module);
                let elem_ty = match iter_ty {
                    FidanType::List(inner) => *inner,
                    FidanType::String | FidanType::Dynamic => FidanType::Dynamic,
                    _ => FidanType::Dynamic,
                };
                self.push_scope(ScopeKind::Block);
                self.table.define(
                    binding,
                    SymbolInfo {
                        kind: SymbolKind::Var,
                        ty: elem_ty,
                        span,
                        is_mutable: false,
                        initialized: Initialized::Yes,
                        const_value: None,
                    },
                );
                self.infer_expr(key, module);
                self.infer_expr(value, module);
                if let Some(f) = filter {
                    self.infer_expr(f, module);
                }
                self.pop_scope();
                FidanType::Dynamic
            }

            Expr::Lambda {
                params,
                return_ty,
                body,
                span,
            } => {
                // Typecheck the lambda body in its own action scope.
                self.check_action_body(
                    ActionBody {
                        params: &params,
                        return_ty: &return_ty,
                        body: &body,
                        inject_this: None,
                        implicit_return_ty: None,
                        span,
                    },
                    module,
                );
                FidanType::Function
            }
        }
    }

    // ── Field resolution ──────────────────────────────────────────────────

    fn emit_unknown_member_error(
        &mut self,
        ty: &FidanType,
        field: Symbol,
        span: Span,
        expected_kind: &str,
    ) {
        let type_name = self.ty_name(ty);
        let field_name = self.interner.resolve(field).to_string();
        self.emit_error(
            fidan_diagnostics::diag_code!("E0204"),
            format!("type `{type_name}` has no {expected_kind} `{field_name}`"),
            span,
        );
    }

    fn emit_not_callable_error(&mut self, ty: &FidanType, span: Span) {
        self.emit_error(
            fidan_diagnostics::diag_code!("E0308"),
            format!("type `{}` is not callable", self.ty_name(ty)),
            span,
        );
    }

    fn receiver_builtin_kind(&self, ty: &FidanType) -> Option<ReceiverBuiltinKind> {
        Some(match ty {
            FidanType::Integer => ReceiverBuiltinKind::Integer,
            FidanType::Float => ReceiverBuiltinKind::Float,
            FidanType::Boolean => ReceiverBuiltinKind::Boolean,
            FidanType::String => ReceiverBuiltinKind::String,
            FidanType::List(_) => ReceiverBuiltinKind::List,
            FidanType::Dict(_, _) => ReceiverBuiltinKind::Dict,
            FidanType::Handle => ReceiverBuiltinKind::Handle,
            FidanType::Nothing => ReceiverBuiltinKind::Nothing,
            FidanType::Dynamic | FidanType::Unknown | FidanType::Error => {
                ReceiverBuiltinKind::Dynamic
            }
            FidanType::Shared(_) => ReceiverBuiltinKind::Shared,
            FidanType::WeakShared(_) => ReceiverBuiltinKind::WeakShared,
            FidanType::Pending(_) => ReceiverBuiltinKind::Pending,
            FidanType::Function => ReceiverBuiltinKind::Function,
            FidanType::Tuple(_)
            | FidanType::Object(_)
            | FidanType::Enum(_)
            | FidanType::ClassType(_) => {
                return None;
            }
        })
    }

    fn builtin_return_kind_to_type(&self, kind: BuiltinReturnKind) -> FidanType {
        match kind {
            BuiltinReturnKind::Nothing => FidanType::Nothing,
            BuiltinReturnKind::String => FidanType::String,
            BuiltinReturnKind::Integer => FidanType::Integer,
            BuiltinReturnKind::Float => FidanType::Float,
            BuiltinReturnKind::Boolean => FidanType::Boolean,
            BuiltinReturnKind::Dynamic => FidanType::Dynamic,
        }
    }

    fn resolve_receiver_return_kind(
        &self,
        receiver_ty: &FidanType,
        return_kind: ReceiverReturnKind,
    ) -> FidanType {
        match return_kind {
            ReceiverReturnKind::Integer => FidanType::Integer,
            ReceiverReturnKind::Float => FidanType::Float,
            ReceiverReturnKind::Boolean => FidanType::Boolean,
            ReceiverReturnKind::String => FidanType::String,
            ReceiverReturnKind::Dynamic => FidanType::Dynamic,
            ReceiverReturnKind::Nothing => FidanType::Nothing,
            ReceiverReturnKind::ReceiverElement => match receiver_ty {
                FidanType::List(inner) => (**inner).clone(),
                _ => FidanType::Dynamic,
            },
            ReceiverReturnKind::DictValue => match receiver_ty {
                FidanType::Dict(_, value) => (**value).clone(),
                _ => FidanType::Dynamic,
            },
            ReceiverReturnKind::ListOfString => FidanType::List(Box::new(FidanType::String)),
            ReceiverReturnKind::ListOfInteger => FidanType::List(Box::new(FidanType::Integer)),
            ReceiverReturnKind::ListOfDynamic => FidanType::List(Box::new(FidanType::Dynamic)),
            ReceiverReturnKind::ListOfReceiverElement => match receiver_ty {
                FidanType::List(inner) => FidanType::List(inner.clone()),
                _ => FidanType::List(Box::new(FidanType::Dynamic)),
            },
            ReceiverReturnKind::ListOfDictValue => match receiver_ty {
                FidanType::Dict(_, value) => FidanType::List(value.clone()),
                _ => FidanType::List(Box::new(FidanType::Dynamic)),
            },
            ReceiverReturnKind::ListOfDynamicPairs => {
                FidanType::List(Box::new(FidanType::List(Box::new(FidanType::Dynamic))))
            }
            ReceiverReturnKind::SharedInnerValue => match receiver_ty {
                FidanType::Shared(inner) => (**inner).clone(),
                _ => FidanType::Dynamic,
            },
            ReceiverReturnKind::SharedOfInner => match receiver_ty {
                FidanType::WeakShared(inner) => FidanType::Shared(inner.clone()),
                _ => FidanType::Shared(Box::new(FidanType::Dynamic)),
            },
            ReceiverReturnKind::WeakSharedOfInner => match receiver_ty {
                FidanType::Shared(inner) => FidanType::WeakShared(inner.clone()),
                _ => FidanType::WeakShared(Box::new(FidanType::Dynamic)),
            },
        }
    }

    fn builtin_field_type(&self, ty: &FidanType, field: Symbol) -> Option<FidanType> {
        let receiver_kind = self.receiver_builtin_kind(ty)?;
        let member = fidan_stdlib::infer_receiver_member(
            receiver_kind,
            self.interner.resolve(field).as_ref(),
        )?;
        member
            .field_return
            .map(|return_kind| self.resolve_receiver_return_kind(ty, return_kind))
    }

    fn builtin_method_return(&self, ty: &FidanType, field: Symbol) -> Option<FidanType> {
        let receiver_kind = self.receiver_builtin_kind(ty)?;
        let member = fidan_stdlib::infer_receiver_member(
            receiver_kind,
            self.interner.resolve(field).as_ref(),
        )?;
        member
            .method_return
            .map(|return_kind| self.resolve_receiver_return_kind(ty, return_kind))
    }

    fn fidan_type_to_stdlib_spec(&self, ty: &FidanType) -> StdlibTypeSpec {
        match ty {
            FidanType::Integer => StdlibTypeSpec::Integer,
            FidanType::Float => StdlibTypeSpec::Float,
            FidanType::Boolean => StdlibTypeSpec::Boolean,
            FidanType::String => StdlibTypeSpec::String,
            FidanType::Handle => StdlibTypeSpec::Handle,
            FidanType::Nothing => StdlibTypeSpec::Nothing,
            FidanType::Dynamic | FidanType::Unknown | FidanType::Error => StdlibTypeSpec::Dynamic,
            FidanType::List(inner) => {
                StdlibTypeSpec::List(Box::new(self.fidan_type_to_stdlib_spec(inner)))
            }
            FidanType::Dict(key, value) => StdlibTypeSpec::Dict(
                Box::new(self.fidan_type_to_stdlib_spec(key)),
                Box::new(self.fidan_type_to_stdlib_spec(value)),
            ),
            FidanType::Tuple(elements) => StdlibTypeSpec::Tuple(
                elements
                    .iter()
                    .map(|element| self.fidan_type_to_stdlib_spec(element))
                    .collect(),
            ),
            FidanType::Shared(inner) => {
                StdlibTypeSpec::Shared(Box::new(self.fidan_type_to_stdlib_spec(inner)))
            }
            FidanType::WeakShared(inner) => {
                StdlibTypeSpec::WeakShared(Box::new(self.fidan_type_to_stdlib_spec(inner)))
            }
            FidanType::Pending(inner) => {
                StdlibTypeSpec::Pending(Box::new(self.fidan_type_to_stdlib_spec(inner)))
            }
            FidanType::Function => StdlibTypeSpec::Function,
            FidanType::Object(_) | FidanType::Enum(_) | FidanType::ClassType(_) => {
                StdlibTypeSpec::Dynamic
            }
        }
    }

    fn stdlib_spec_to_fidan_type(&self, spec: &StdlibTypeSpec) -> FidanType {
        match spec {
            StdlibTypeSpec::Integer => FidanType::Integer,
            StdlibTypeSpec::Float => FidanType::Float,
            StdlibTypeSpec::Boolean => FidanType::Boolean,
            StdlibTypeSpec::String => FidanType::String,
            StdlibTypeSpec::Handle => FidanType::Handle,
            StdlibTypeSpec::Dynamic => FidanType::Dynamic,
            StdlibTypeSpec::Nothing => FidanType::Nothing,
            StdlibTypeSpec::List(inner) => {
                FidanType::List(Box::new(self.stdlib_spec_to_fidan_type(inner)))
            }
            StdlibTypeSpec::Dict(key, value) => FidanType::Dict(
                Box::new(self.stdlib_spec_to_fidan_type(key)),
                Box::new(self.stdlib_spec_to_fidan_type(value)),
            ),
            StdlibTypeSpec::Tuple(elements) => FidanType::Tuple(
                elements
                    .iter()
                    .map(|element| self.stdlib_spec_to_fidan_type(element))
                    .collect(),
            ),
            StdlibTypeSpec::Shared(inner) => {
                FidanType::Shared(Box::new(self.stdlib_spec_to_fidan_type(inner)))
            }
            StdlibTypeSpec::WeakShared(inner) => {
                FidanType::WeakShared(Box::new(self.stdlib_spec_to_fidan_type(inner)))
            }
            StdlibTypeSpec::Pending(inner) => {
                FidanType::Pending(Box::new(self.stdlib_spec_to_fidan_type(inner)))
            }
            StdlibTypeSpec::Function => FidanType::Function,
        }
    }

    fn stdlib_import_return_type(
        &mut self,
        import: StdlibImportInfo,
        args: &[CallArgInfo],
        module: &Module,
    ) -> Option<FidanType> {
        let module_name = self.interner.resolve(import.module);
        let export_name = self.interner.resolve(import.export);
        let mut inferred_args = Vec::with_capacity(args.len());
        for arg in args {
            let inferred = self.infer_expr(arg.value, module);
            inferred_args.push(self.fidan_type_to_stdlib_spec(&inferred));
        }
        fidan_stdlib::infer_precise_stdlib_return_type(
            module_name.as_ref(),
            export_name.as_ref(),
            &inferred_args,
        )
        .map(|spec| self.stdlib_spec_to_fidan_type(&spec))
    }

    fn stdlib_namespace_return_type(
        &mut self,
        namespace: Symbol,
        export: Symbol,
        args: &[CallArgInfo],
        module: &Module,
    ) -> Option<FidanType> {
        let module_name = self.interner.resolve(namespace);
        let export_name = self.interner.resolve(export);
        let mut inferred_args = Vec::with_capacity(args.len());
        for arg in args {
            let inferred = self.infer_expr(arg.value, module);
            inferred_args.push(self.fidan_type_to_stdlib_spec(&inferred));
        }
        fidan_stdlib::infer_precise_stdlib_return_type(
            module_name.as_ref(),
            export_name.as_ref(),
            &inferred_args,
        )
        .map(|spec| self.stdlib_spec_to_fidan_type(&spec))
    }

    fn should_emit_member_error(&self, ty: &FidanType) -> bool {
        !matches!(
            ty,
            FidanType::Dynamic | FidanType::Unknown | FidanType::Error
        )
    }

    fn resolve_field(&mut self, ty: &FidanType, field: Symbol, span: Span) -> FidanType {
        match ty {
            FidanType::Enum(sym) => {
                let sym = *sym;
                if let Some(arity) = self.enum_variant_arity(sym, field) {
                    if arity == 0 {
                        return FidanType::Enum(sym);
                    }
                    return FidanType::Function;
                }
                self.emit_unknown_member_error(
                    &FidanType::Enum(sym),
                    field,
                    span,
                    "field or method",
                );
                FidanType::Error
            }
            FidanType::ClassType(sym) => {
                let sym = *sym;
                let new_sym = self.interner.intern("new");
                if field == new_sym {
                    return FidanType::Function;
                }
                self.emit_unknown_member_error(
                    &FidanType::ClassType(sym),
                    field,
                    span,
                    "field or method",
                );
                FidanType::Error
            }
            FidanType::Object(sym) => {
                let sym = *sym;
                // If the root object is not locally known, record the access
                // for LSP-level cross-document validation.
                if !self.objects.contains_key(&sym) {
                    let tn = self.interner.resolve(sym).to_string();
                    let fn_ = self.interner.resolve(field).to_string();
                    self.cross_module_field_accesses.push((tn, fn_, span));
                    return FidanType::Dynamic;
                }
                // Walk the local inheritance chain iteratively.
                let mut cur = sym;
                loop {
                    // ── own field ───────────────────────────────────────────
                    let found_field = self
                        .objects
                        .get(&cur)
                        .and_then(|o| o.fields.get(&field))
                        .cloned();
                    if let Some(ft) = found_field {
                        return ft;
                    }
                    // ── own method ──────────────────────────────────────────
                    let found_method = self
                        .objects
                        .get(&cur)
                        .map(|o| o.methods.contains_key(&field))
                        .unwrap_or(false);
                    if found_method {
                        return FidanType::Function;
                    }
                    // ── parent ──────────────────────────────────────────────
                    let parent = self.objects.get(&cur).and_then(|o| o.parent);
                    match parent {
                        None => {
                            // Chain exhausted with no match — emit diagnostic.
                            let type_name = self.ty_name(&FidanType::Object(sym));
                            let field_name = self.interner.resolve(field).to_string();
                            self.emit_error(
                                fidan_diagnostics::diag_code!("E0204"),
                                format!(
                                    "object `{type_name}` has no field or method `{field_name}`"
                                ),
                                span,
                            );
                            return FidanType::Error;
                        }
                        Some(p) if !self.objects.contains_key(&p) => {
                            // Parent is from another module — record for LSP
                            // cross-document validation instead of silently dropping.
                            let tn = self.interner.resolve(sym).to_string();
                            let fn_ = self.interner.resolve(field).to_string();
                            self.cross_module_field_accesses.push((tn, fn_, span));
                            return FidanType::Dynamic;
                        }
                        Some(p) => {
                            cur = p;
                        }
                    }
                }
            }
            _ => {
                if let Some(field_ty) = self.builtin_field_type(ty, field) {
                    field_ty
                } else if self.should_emit_member_error(ty) {
                    self.emit_unknown_member_error(ty, field, span, "field or method");
                    FidanType::Error
                } else {
                    FidanType::Dynamic
                }
            }
        }
    }

    // ── Call return-type inference ────────────────────────────────────────

    fn infer_call(
        &mut self,
        callee_id: ExprId,
        args: &[CallArgInfo],
        span: Span,
        module: &Module,
    ) -> FidanType {
        let callee = module.arena.get_expr(callee_id).clone();
        match callee {
            Expr::Ident {
                name,
                span: callee_span,
            } => {
                if self.table.lookup(name).is_some() {
                    self.referenced_names.insert(name);
                }
                let name_str = self.interner.resolve(name).to_string();
                if name_str.as_str() == "Shared" {
                    let inner = args
                        .first()
                        .map(|arg| self.infer_expr(arg.value, module))
                        .unwrap_or(FidanType::Dynamic);
                    return FidanType::Shared(Box::new(inner));
                }
                if name_str.as_str() == "WeakShared" {
                    let inferred_args: Vec<FidanType> = args
                        .iter()
                        .map(|arg| self.infer_expr(arg.value, module))
                        .collect();
                    if inferred_args.is_empty() {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0301"),
                            "WeakShared(shared) requires a Shared argument",
                            span,
                        );
                        return FidanType::WeakShared(Box::new(FidanType::Dynamic));
                    }
                    if inferred_args.len() > 1 {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0302"),
                            "WeakShared(shared) accepts exactly one argument",
                            span,
                        );
                    }
                    return match inferred_args.first() {
                        Some(FidanType::Shared(inner)) => {
                            FidanType::WeakShared(Box::new((**inner).clone()))
                        }
                        Some(FidanType::WeakShared(inner)) => {
                            FidanType::WeakShared(Box::new((**inner).clone()))
                        }
                        Some(FidanType::Dynamic | FidanType::Unknown | FidanType::Error) => {
                            FidanType::WeakShared(Box::new(FidanType::Dynamic))
                        }
                        Some(other) => {
                            self.emit_error(
                                fidan_diagnostics::diag_code!("E0302"),
                                format!(
                                    "WeakShared(shared) expects a Shared value, found `{}`",
                                    self.ty_name(other)
                                ),
                                span,
                            );
                            FidanType::WeakShared(Box::new(FidanType::Dynamic))
                        }
                        None => FidanType::WeakShared(Box::new(FidanType::Dynamic)),
                    };
                }
                if let Some(return_kind) = builtin_return_kind(&name_str) {
                    return self.builtin_return_kind_to_type(return_kind);
                }
                if let Some(import) = self.stdlib_imports.get(&name).copied()
                    && let Some(return_ty) = self.stdlib_import_return_type(import, args, module)
                {
                    return return_ty;
                }
                // Look up in symbol table: Object constructor, user action, or builtin.
                match self.table.lookup(name).map(|i| i.kind) {
                    Some(SymbolKind::Object) => {
                        self.check_required_params(name, args, span, module);
                        FidanType::Object(name)
                    }
                    Some(_) => {
                        // User action — validate call arguments against the declared signature.
                        if let Some(info) = self.lookup_action_info(name) {
                            self.check_call_arguments(&info.params, args, span, module);
                            // The checker keeps the action signature up to date for
                            // unannotated actions once their body has been analyzed.
                            // Use that return type here instead of discarding it.
                            if self.is_action_deprecated(name) {
                                let n = self.interner.resolve(name).to_string();
                                self.emit_warning(
                                    fidan_diagnostics::diag_code!("W2005"),
                                    format!("`{n}` is marked `@deprecated` and may be removed in a future version"),
                                    callee_span,
                                );
                            }
                            return info.return_ty;
                        }
                        // Emit W2005 if the action is marked @deprecated.
                        if self.is_action_deprecated(name) {
                            let n = self.interner.resolve(name).to_string();
                            self.emit_warning(
                                fidan_diagnostics::diag_code!("W2005"),
                                format!("`{n}` is marked `@deprecated` and may be removed in a future version"),
                                callee_span,
                            );
                        }
                        FidanType::Dynamic
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
                        FidanType::Error
                    }
                }
            }

            Expr::Field { object, field, .. } => {
                if let Expr::Ident {
                    name: namespace, ..
                } = module.arena.get_expr(object)
                    && let Some(module_sym) = self.stdlib_namespace_imports.get(namespace).copied()
                    && let Some(return_ty) =
                        self.stdlib_namespace_return_type(module_sym, field, args, module)
                {
                    return return_ty;
                }

                let recv = self.infer_expr(object, module);
                match recv {
                    FidanType::Enum(sym) => {
                        let Some(arity) = self.enum_variant_arity(sym, field) else {
                            self.emit_unknown_member_error(
                                &FidanType::Enum(sym),
                                field,
                                span,
                                "method",
                            );
                            return FidanType::Error;
                        };
                        if args.len() != arity {
                            self.emit_error(
                                fidan_diagnostics::diag_code!("E0305"),
                                format!(
                                    "expected {} argument{}, got {}",
                                    arity,
                                    if arity == 1 { "" } else { "s" },
                                    args.len()
                                ),
                                span,
                            );
                        }
                        FidanType::Enum(sym)
                    }
                    FidanType::ClassType(sym) => {
                        let new_sym = self.interner.intern("new");
                        if field == new_sym {
                            self.check_required_params(sym, args, span, module);
                            FidanType::Object(sym)
                        } else {
                            self.emit_unknown_member_error(
                                &FidanType::ClassType(sym),
                                field,
                                span,
                                "method",
                            );
                            FidanType::Error
                        }
                    }
                    FidanType::Object(sym) => {
                        let object_type = FidanType::Object(sym);
                        if let Some(field_ty) = self.resolve_object_field_only(sym, field) {
                            self.emit_unknown_member_error(&object_type, field, span, "method");
                            let _ = field_ty;
                            return FidanType::Error;
                        }

                        let ret = self.method_return(&sym, field);
                        if matches!(ret, FidanType::Dynamic) {
                            if self.object_method_may_be_cross_module(sym) {
                                let recv_ty = self.interner.resolve(sym).to_string();
                                let mname = self.interner.resolve(field).to_string();
                                let arg_tys: Vec<String> = args
                                    .iter()
                                    .map(|arg| {
                                        self.expr_types
                                            .get(&arg.value)
                                            .map(|t| {
                                                t.display_name(&|s| {
                                                    self.interner.resolve(s).to_string()
                                                })
                                            })
                                            .unwrap_or_else(|| "?".to_string())
                                    })
                                    .collect();
                                self.cross_module_call_sites
                                    .push(crate::CrossModuleCallSite {
                                        receiver_ty: recv_ty,
                                        method_name: mname,
                                        arg_tys,
                                        span,
                                    });
                            } else {
                                self.emit_unknown_member_error(&object_type, field, span, "method");
                                return FidanType::Error;
                            }
                        } else {
                            // Method found locally — validate the full local signature.
                            if let Some(minfo) = self.find_method_info(&sym, field) {
                                self.check_call_arguments(&minfo.params, args, span, module);
                            }
                        }
                        ret
                    }
                    _ => {
                        if let Some(ret) = self.builtin_method_return(&recv, field) {
                            ret
                        } else if self.should_emit_member_error(&recv) {
                            self.emit_unknown_member_error(&recv, field, span, "method");
                            FidanType::Error
                        } else {
                            FidanType::Dynamic
                        }
                    }
                }
            }

            Expr::Parent { span: callee_span } => {
                let Some(FidanType::Object(sym)) = self.this_ty.clone() else {
                    let callee_ty = self.infer_expr(callee_id, module);
                    self.emit_not_callable_error(&callee_ty, span);
                    return FidanType::Error;
                };
                let Some(parent_sym) = self.objects.get(&sym).and_then(|o| o.parent) else {
                    let callee_ty = self.infer_expr(callee_id, module);
                    self.emit_not_callable_error(&callee_ty, span);
                    return FidanType::Error;
                };
                let _ = callee_span;
                self.check_required_params(parent_sym, args, span, module);
                FidanType::Object(parent_sym)
            }

            _ => {
                let callee_ty = self.infer_expr(callee_id, module);
                match callee_ty {
                    FidanType::Function
                    | FidanType::Dynamic
                    | FidanType::Unknown
                    | FidanType::Error => FidanType::Dynamic,
                    other => {
                        self.emit_not_callable_error(&other, span);
                        FidanType::Error
                    }
                }
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

    fn resolve_object_field_only(&self, obj_sym: Symbol, field: Symbol) -> Option<FidanType> {
        if !self.objects.contains_key(&obj_sym) {
            return None;
        }

        let mut cur = obj_sym;
        loop {
            let info = self.objects.get(&cur)?;
            if let Some(field_ty) = info.fields.get(&field) {
                return Some(field_ty.clone());
            }
            match info.parent {
                Some(parent) if self.objects.contains_key(&parent) => cur = parent,
                _ => return None,
            }
        }
    }

    fn object_method_may_be_cross_module(&self, obj_sym: Symbol) -> bool {
        if !self.objects.contains_key(&obj_sym) {
            return true;
        }

        let mut cur = obj_sym;
        loop {
            let Some(info) = self.objects.get(&cur) else {
                return true;
            };
            match info.parent {
                Some(parent) if self.objects.contains_key(&parent) => cur = parent,
                Some(_) => return true,
                None => return false,
            }
        }
    }

    fn enum_variant_arity(&self, enum_sym: Symbol, variant: Symbol) -> Option<usize> {
        self.enums.get(&enum_sym).and_then(|info| {
            info.variants
                .iter()
                .find_map(|(name, arity)| (*name == variant).then_some(*arity))
        })
    }

    /// Walk the local object inheritance chain of `obj_sym` to find the [`ActionInfo`] for
    /// `method`.  Returns `None` when not found locally (may live in a cross-module parent).
    fn find_method_info(&self, obj_sym: &Symbol, method: Symbol) -> Option<ActionInfo> {
        let mut cur = *obj_sym;
        loop {
            let info = self.objects.get(&cur)?;
            if let Some(m) = info.methods.get(&method) {
                return Some(m.clone());
            }
            match info.parent {
                Some(p) if self.objects.contains_key(&p) => cur = p,
                _ => return None,
            }
        }
    }

    /// Returns `true` when every possible execution path through `body` is
    /// guaranteed to end with a `return` or `panic` — i.e. the function cannot
    /// "fall off the end".
    ///
    /// The algorithm scans forward through the statement list.  As soon as it
    /// finds a statement that terminates ALL paths (unconditional return/panic,
    /// or an if/else where every branch terminates, etc.) it returns `true`
    /// because any later statements would be unreachable.
    fn all_paths_return(&self, body: &[StmtId], module: &Module) -> bool {
        for &sid in body {
            let stmt = module.arena.get_stmt(sid).clone();
            if self.stmt_terminates_all_paths(&stmt, module) {
                return true;
            }
        }
        false
    }

    fn infer_action_return_type(&self, body: &[StmtId], module: &Module) -> FidanType {
        let summary = self.summarize_stmt_list(body, module);
        let mut outcomes = Vec::new();
        if let Some(return_ty) = summary.return_ty {
            outcomes.push(return_ty);
        }
        if summary.falls_through || outcomes.is_empty() {
            outcomes.push(FidanType::Nothing);
        }
        self.merge_possible_types(outcomes)
    }

    fn summarize_stmt_list(&self, stmts: &[StmtId], module: &Module) -> FlowSummary {
        let mut summary = FlowSummary {
            falls_through: true,
            return_ty: None,
        };

        for &sid in stmts {
            if !summary.falls_through {
                break;
            }
            let stmt = module.arena.get_stmt(sid).clone();
            let stmt_summary = self.summarize_stmt(&stmt, module);
            summary.return_ty =
                self.merge_optional_types(summary.return_ty, stmt_summary.return_ty);
            summary.falls_through = stmt_summary.falls_through;
        }

        summary
    }

    fn summarize_stmt(&self, stmt: &Stmt, module: &Module) -> FlowSummary {
        match stmt {
            Stmt::Return { value, .. } => FlowSummary {
                falls_through: false,
                return_ty: Some(
                    value
                        .and_then(|expr| self.expr_types.get(&expr).cloned())
                        .unwrap_or(FidanType::Nothing),
                ),
            },
            Stmt::Panic { .. } => FlowSummary {
                falls_through: false,
                return_ty: None,
            },
            Stmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                let mut branch_summaries = Vec::with_capacity(else_ifs.len() + 2);
                branch_summaries.push(self.summarize_stmt_list(then_body, module));
                branch_summaries.extend(
                    else_ifs
                        .iter()
                        .map(|branch| self.summarize_stmt_list(&branch.body, module)),
                );

                let has_else = else_body.is_some();
                if let Some(body) = else_body {
                    branch_summaries.push(self.summarize_stmt_list(body, module));
                }

                let mut return_ty = None;
                let mut falls_through = !has_else;
                for branch in branch_summaries {
                    return_ty = self.merge_optional_types(return_ty, branch.return_ty);
                    falls_through |= branch.falls_through;
                }

                FlowSummary {
                    falls_through,
                    return_ty,
                }
            }
            Stmt::Check { arms, .. } => {
                let mut return_ty = None;
                let mut falls_through = !self.check_has_catch_all_arm(arms, module);
                for arm in arms {
                    let arm_summary = self.summarize_stmt_list(&arm.body, module);
                    return_ty = self.merge_optional_types(return_ty, arm_summary.return_ty);
                    falls_through |= arm_summary.falls_through;
                }
                FlowSummary {
                    falls_through,
                    return_ty,
                }
            }
            Stmt::Attempt {
                body,
                catches,
                otherwise,
                ..
            } => {
                let mut return_ty = None;
                let body_summary = self.summarize_stmt_list(body, module);
                return_ty = self.merge_optional_types(return_ty, body_summary.return_ty);
                let mut falls_through = body_summary.falls_through;

                for catch in catches {
                    let catch_summary = self.summarize_stmt_list(&catch.body, module);
                    return_ty = self.merge_optional_types(return_ty, catch_summary.return_ty);
                    falls_through |= catch_summary.falls_through;
                }

                if let Some(otherwise_body) = otherwise {
                    let otherwise_summary = self.summarize_stmt_list(otherwise_body, module);
                    return_ty = self.merge_optional_types(return_ty, otherwise_summary.return_ty);
                    falls_through |= otherwise_summary.falls_through;
                } else {
                    falls_through = true;
                }

                FlowSummary {
                    falls_through,
                    return_ty,
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::ParallelFor { body, .. } => {
                let body_summary = self.summarize_stmt_list(body, module);
                FlowSummary {
                    falls_through: true,
                    return_ty: body_summary.return_ty,
                }
            }
            _ => FlowSummary {
                falls_through: true,
                return_ty: None,
            },
        }
    }

    fn check_has_catch_all_arm(&self, arms: &[fidan_ast::CheckArm], module: &Module) -> bool {
        arms.iter().any(|arm| {
            matches!(
                module.arena.get_expr(arm.pattern),
                Expr::Ident { name, .. } if self.interner.resolve(*name).as_ref() == "_"
            )
        })
    }

    fn merge_optional_types(
        &self,
        lhs: Option<FidanType>,
        rhs: Option<FidanType>,
    ) -> Option<FidanType> {
        match (lhs, rhs) {
            (Some(lhs), Some(rhs)) => Some(self.merge_possible_types([lhs, rhs])),
            (Some(lhs), None) => Some(lhs),
            (None, Some(rhs)) => Some(rhs),
            (None, None) => None,
        }
    }

    fn merge_possible_types<I>(&self, types: I) -> FidanType
    where
        I: IntoIterator<Item = FidanType>,
    {
        let mut iter = types.into_iter();
        let Some(first) = iter.next() else {
            return FidanType::Dynamic;
        };
        iter.fold(first, |acc, ty| self.merge_two_types(&acc, &ty))
    }

    fn merge_two_types(&self, lhs: &FidanType, rhs: &FidanType) -> FidanType {
        if lhs == rhs {
            return lhs.clone();
        }
        if lhs.is_error() {
            return rhs.clone();
        }
        if rhs.is_error() {
            return lhs.clone();
        }
        if lhs.is_dynamic() || rhs.is_dynamic() {
            return FidanType::Dynamic;
        }
        // Fidan types are nullable by default, so merging with `nothing`
        // preserves the non-`nothing` type instead of degrading to `dynamic`.
        if lhs.is_nothing() {
            return rhs.clone();
        }
        if rhs.is_nothing() {
            return lhs.clone();
        }

        match (lhs, rhs) {
            (FidanType::Integer, FidanType::Float) | (FidanType::Float, FidanType::Integer) => {
                FidanType::Float
            }
            (FidanType::List(lhs), FidanType::List(rhs)) => {
                FidanType::List(Box::new(self.merge_two_types(lhs, rhs)))
            }
            (FidanType::Dict(lhs_k, lhs_v), FidanType::Dict(rhs_k, rhs_v)) => FidanType::Dict(
                Box::new(self.merge_two_types(lhs_k, rhs_k)),
                Box::new(self.merge_two_types(lhs_v, rhs_v)),
            ),
            (FidanType::Tuple(lhs), FidanType::Tuple(rhs)) if lhs.len() == rhs.len() => {
                FidanType::Tuple(
                    lhs.iter()
                        .zip(rhs.iter())
                        .map(|(lhs, rhs)| self.merge_two_types(lhs, rhs))
                        .collect(),
                )
            }
            (FidanType::Shared(lhs), FidanType::Shared(rhs)) => {
                FidanType::Shared(Box::new(self.merge_two_types(lhs, rhs)))
            }
            (FidanType::WeakShared(lhs), FidanType::WeakShared(rhs)) => {
                FidanType::WeakShared(Box::new(self.merge_two_types(lhs, rhs)))
            }
            (FidanType::Pending(lhs), FidanType::Pending(rhs)) => {
                FidanType::Pending(Box::new(self.merge_two_types(lhs, rhs)))
            }
            _ => FidanType::Dynamic,
        }
    }

    /// Returns `true` if executing `stmt` guarantees that all subsequent control
    /// flow ends with a return or panic (i.e. no execution path falls through).
    fn stmt_terminates_all_paths(&self, stmt: &Stmt, module: &Module) -> bool {
        match stmt {
            // Unconditional exits.
            Stmt::Return { .. } | Stmt::Panic { .. } => true,

            // `if … else if … else` — only terminates all paths when there IS
            // an `else` branch AND every branch terminates.
            Stmt::If {
                then_body,
                else_ifs,
                else_body: Some(else_body),
                ..
            } => {
                self.all_paths_return(then_body, module)
                    && else_ifs
                        .iter()
                        .all(|ei| self.all_paths_return(&ei.body, module))
                    && self.all_paths_return(else_body, module)
            }

            // `check` — terminates if every arm terminates and there is at
            // least one arm (a check with zero arms is vacuously non-terminating
            // because no path is taken).
            Stmt::Check { arms, .. } => {
                !arms.is_empty()
                    && self.check_has_catch_all_arm(arms, module)
                    && arms.iter().all(|a| self.all_paths_return(&a.body, module))
            }

            // `attempt` — terminates if the try body AND every catch AND the
            // otherwise (else) branch all terminate.
            Stmt::Attempt {
                body,
                catches,
                otherwise,
                ..
            } => {
                self.all_paths_return(body, module)
                    && catches
                        .iter()
                        .all(|c| self.all_paths_return(&c.body, module))
                    && otherwise
                        .as_deref()
                        .map(|b| self.all_paths_return(b, module))
                        .unwrap_or(false)
            }

            // Loops and other compound statements don't guarantee a return
            // because their bodies might never execute (zero iterations, etc.).
            _ => false,
        }
    }

    fn warn_unreachable_stmt(&mut self, stmt: &Stmt) {
        self.emit_warning(
            fidan_diagnostics::diag_code!("W1006"),
            "unreachable statement; this code can never execute",
            self.stmt_span(stmt),
        );
    }

    fn warn_unreachable_stmt_ids(&mut self, stmts: &[StmtId], module: &Module) {
        for &sid in stmts {
            self.warn_unreachable_stmt(module.arena.get_stmt(sid));
        }
    }

    fn stmt_span(&self, stmt: &Stmt) -> Span {
        match stmt {
            Stmt::VarDecl { span, .. }
            | Stmt::Destructure { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::ActionDecl { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::If { span, .. }
            | Stmt::Check { span, .. }
            | Stmt::For { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Attempt { span, .. }
            | Stmt::ParallelFor { span, .. }
            | Stmt::ConcurrentBlock { span, .. }
            | Stmt::Panic { span, .. }
            | Stmt::Error { span } => *span,
        }
    }

    fn eval_const_bool(&self, expr_id: ExprId, module: &Module) -> Option<bool> {
        match self.eval_const_value(expr_id, module)? {
            ConstValue::Bool(value) => Some(value),
            _ => None,
        }
    }

    fn eval_const_value(&self, expr_id: ExprId, module: &Module) -> Option<ConstValue> {
        match module.arena.get_expr(expr_id) {
            Expr::BoolLit { value, .. } => Some(ConstValue::Bool(*value)),
            Expr::IntLit { value, .. } => Some(ConstValue::Int(*value)),
            Expr::FloatLit { value, .. } => Some(ConstValue::Float(*value)),
            Expr::StrLit { value, .. } => Some(ConstValue::String(value.clone())),
            Expr::Nothing { .. } => Some(ConstValue::Nothing),
            Expr::Ident { name, .. } => self.table.lookup(*name)?.const_value.clone(),
            Expr::Unary { op, operand, .. } => {
                let value = self.eval_const_value(*operand, module)?;
                match (op, value) {
                    (UnOp::Not, ConstValue::Bool(value)) => Some(ConstValue::Bool(!value)),
                    (UnOp::Pos, ConstValue::Int(value)) => Some(ConstValue::Int(value)),
                    (UnOp::Pos, ConstValue::Float(value)) => Some(ConstValue::Float(value)),
                    (UnOp::Neg, ConstValue::Int(value)) => Some(ConstValue::Int(-value)),
                    (UnOp::Neg, ConstValue::Float(value)) => Some(ConstValue::Float(-value)),
                    _ => None,
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let lhs = self.eval_const_value(*lhs, module)?;
                let rhs = self.eval_const_value(*rhs, module)?;
                self.eval_const_binary(*op, lhs, rhs)
            }
            _ => None,
        }
    }

    fn eval_const_binary(&self, op: BinOp, lhs: ConstValue, rhs: ConstValue) -> Option<ConstValue> {
        use ConstValue as C;

        match op {
            BinOp::And => match (lhs, rhs) {
                (C::Bool(lhs), C::Bool(rhs)) => Some(C::Bool(lhs && rhs)),
                _ => None,
            },
            BinOp::Or => match (lhs, rhs) {
                (C::Bool(lhs), C::Bool(rhs)) => Some(C::Bool(lhs || rhs)),
                _ => None,
            },
            BinOp::Eq => Some(C::Bool(lhs == rhs)),
            BinOp::NotEq => Some(C::Bool(lhs != rhs)),
            BinOp::Lt => self.eval_order_compare(lhs, rhs, |lhs, rhs| lhs < rhs),
            BinOp::LtEq => self.eval_order_compare(lhs, rhs, |lhs, rhs| lhs <= rhs),
            BinOp::Gt => self.eval_order_compare(lhs, rhs, |lhs, rhs| lhs > rhs),
            BinOp::GtEq => self.eval_order_compare(lhs, rhs, |lhs, rhs| lhs >= rhs),
            BinOp::Add => match (lhs, rhs) {
                (C::Int(lhs), C::Int(rhs)) => Some(C::Int(lhs + rhs)),
                (C::Float(lhs), C::Float(rhs)) => Some(C::Float(lhs + rhs)),
                (C::String(lhs), C::String(rhs)) => Some(C::String(lhs + &rhs)),
                _ => None,
            },
            BinOp::Sub => match (lhs, rhs) {
                (C::Int(lhs), C::Int(rhs)) => Some(C::Int(lhs - rhs)),
                (C::Float(lhs), C::Float(rhs)) => Some(C::Float(lhs - rhs)),
                _ => None,
            },
            BinOp::Mul => match (lhs, rhs) {
                (C::Int(lhs), C::Int(rhs)) => Some(C::Int(lhs * rhs)),
                (C::Float(lhs), C::Float(rhs)) => Some(C::Float(lhs * rhs)),
                _ => None,
            },
            BinOp::Div => match (lhs, rhs) {
                (C::Int(_), C::Int(0)) => None,
                (C::Int(lhs), C::Int(rhs)) => Some(C::Int(lhs / rhs)),
                (C::Float(_), C::Float(0.0)) => None,
                (C::Float(lhs), C::Float(rhs)) => Some(C::Float(lhs / rhs)),
                _ => None,
            },
            BinOp::Rem => match (lhs, rhs) {
                (C::Int(_), C::Int(0)) => None,
                (C::Int(lhs), C::Int(rhs)) => Some(C::Int(lhs % rhs)),
                _ => None,
            },
            _ => None,
        }
    }

    fn eval_order_compare(
        &self,
        lhs: ConstValue,
        rhs: ConstValue,
        cmp: impl FnOnce(f64, f64) -> bool,
    ) -> Option<ConstValue> {
        use ConstValue as C;

        match (lhs, rhs) {
            (C::Int(lhs), C::Int(rhs)) => Some(C::Bool(cmp(lhs as f64, rhs as f64))),
            (C::Float(lhs), C::Float(rhs)) => Some(C::Bool(cmp(lhs, rhs))),
            (C::Int(lhs), C::Float(rhs)) => Some(C::Bool(cmp(lhs as f64, rhs))),
            (C::Float(lhs), C::Int(rhs)) => Some(C::Bool(cmp(lhs, rhs as f64))),
            _ => None,
        }
    }

    /// Check that all non-optional params of `initialize` for `obj_sym` are supplied.
    fn check_required_params(
        &mut self,
        obj_sym: Symbol,
        args: &[CallArgInfo],
        span: Span,
        module: &Module,
    ) {
        let init_sym = self.interner.intern("initialize");
        let new_sym = self.interner.intern("new");
        let params: Option<Vec<ParamInfo>> = self
            .objects
            .get(&obj_sym)
            .and_then(|o| o.methods.get(&init_sym).or_else(|| o.methods.get(&new_sym)))
            .map(|m| m.params.clone());

        if let Some(params) = params {
            self.check_call_arguments(&params, args, span, module);
        }
    }

    /// Validate a call site against a declared parameter list.
    ///
    /// This is the shared enforcement path used by user actions, local methods,
    /// and object constructors so interpreter/JIT/AOT all see the same frontend rules.
    fn check_call_arguments(
        &mut self,
        params: &[ParamInfo],
        args: &[CallArgInfo],
        span: Span,
        module: &Module,
    ) {
        // Named args supplied at this call site.
        let named: std::collections::HashSet<Symbol> =
            args.iter().filter_map(|arg| arg.name).collect();
        let positional: Vec<&CallArgInfo> = args.iter().filter(|arg| arg.name.is_none()).collect();
        let positional_count = positional.len();

        // Count how many params will consume positional slots (those not covered
        // by a named arg at this call site).
        let positional_param_count = params.iter().filter(|p| !named.contains(&p.name)).count();

        // ── E0305: too many positional arguments ─────────────────────────────
        if positional_count > positional_param_count {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0305"),
                format!(
                    "expected {} argument{}, got {}",
                    positional_param_count,
                    if positional_param_count == 1 { "" } else { "s" },
                    positional_count,
                ),
                span,
            );
            // Still check for missing required params below so callers get
            // all relevant errors in one pass.
        }

        // Walk params in declaration order, assigning positional slots to those
        // not covered by a name. Any non-optional param left uncovered is an error.
        let mut pos_used = 0usize;
        for p in params {
            let bound_arg = args
                .iter()
                .find(|arg| arg.name == Some(p.name))
                .copied()
                .or_else(|| {
                    if pos_used < positional_count {
                        let arg = positional[pos_used];
                        pos_used += 1;
                        Some(*arg)
                    } else {
                        None
                    }
                });

            if let Some(arg) = bound_arg {
                self.check_bound_argument_compatibility(p, arg, module);
                continue;
            }

            // Not covered.
            if !p.optional {
                let pname = self.interner.resolve(p.name).to_string();
                let msg = if p.certain {
                    format!("certain parameter `{pname}` not provided")
                } else {
                    format!(
                        "parameter `{pname}` must be provided (use `optional` to make it omittable)"
                    )
                };
                self.emit_error(fidan_diagnostics::diag_code!("E0301"), msg, span);
            }
        }
    }

    fn check_bound_argument_compatibility(
        &mut self,
        param: &ParamInfo,
        arg: CallArgInfo,
        module: &Module,
    ) {
        let actual = self
            .expr_types
            .get(&arg.value)
            .cloned()
            .unwrap_or_else(|| self.infer_expr(arg.value, module));

        if param.certain && self.expr_is_statically_nothing(arg.value, module) {
            let pname = self.interner.resolve(param.name).to_string();
            self.emit_error(
                fidan_diagnostics::diag_code!("E0302"),
                format!("certain parameter `{pname}` cannot receive `nothing`"),
                arg.span,
            );
            return;
        }

        if !param.ty.is_assignable_from(&actual) {
            let pname = self.interner.resolve(param.name).to_string();
            let expected = self.ty_name(&param.ty);
            let found = self.ty_name(&actual);
            self.emit_error(
                fidan_diagnostics::diag_code!("E0302"),
                format!("argument `{pname}` expects type `{expected}`, found `{found}`"),
                arg.span,
            );
        }
    }

    fn expr_is_statically_nothing(&self, expr_id: ExprId, module: &Module) -> bool {
        matches!(
            self.eval_const_value(expr_id, module),
            Some(ConstValue::Nothing)
        )
    }

    // ── Null-safety helpers (E0205) ───────────────────────────────────────

    /// Returns `(name, use_span, decl_span, is_param)` when `expr_id` is a
    /// plain identifier that may hold `nothing` at runtime:
    ///   - a non-`certain` parameter  → `Initialized::Maybe`
    ///   - an uninitialised typed variable → `Initialized::No`
    ///
    /// `Dynamic`/`Error`/`Unknown`/`Nothing`-typed symbols are excluded.
    fn possibly_nothing_ident(
        &self,
        expr_id: ExprId,
        module: &Module,
    ) -> Option<(String, Span, Span, bool)> {
        let expr = module.arena.get_expr(expr_id).clone();
        let Expr::Ident {
            name,
            span: use_span,
        } = expr
        else {
            return None;
        };
        let info = self.table.lookup(name)?;
        if !matches!(info.initialized, Initialized::Maybe | Initialized::No) {
            return None;
        }
        if matches!(
            info.ty,
            FidanType::Dynamic | FidanType::Error | FidanType::Unknown | FidanType::Nothing
        ) {
            return None;
        }
        let name_str = self.interner.resolve(name).to_string();
        let is_param = info.kind == SymbolKind::Param;
        Some((name_str, use_span, info.span, is_param))
    }

    /// Emit E0205 if `expr_id` is a possibly-`nothing` value used as `context`.
    fn require_non_nullable(&mut self, expr_id: ExprId, context: &str, module: &Module) {
        if let Some((name, use_span, decl_span, is_param)) =
            self.possibly_nothing_ident(expr_id, module)
        {
            let (subject, hint) = if is_param {
                (
                    format!("parameter `{name}` is not `certain` and may be `nothing`"),
                    format!(
                        "add `certain` to the parameter declaration, \
                         or guard with `{name} ?? <default>`"
                    ),
                )
            } else {
                (
                    format!("variable `{name}` is not initialised and is implicitly `nothing`"),
                    format!("assign a value before use, or guard with `{name} ?? <default>`"),
                )
            };
            let mut diag = Diagnostic::error(
                fidan_diagnostics::diag_code!("E0205"),
                format!("{subject} — used as {context}"),
                use_span,
            )
            .with_label(Label::primary(use_span, "may be `nothing` here"))
            .with_note(hint);
            if decl_span != use_span {
                let decl_label = if is_param {
                    "declared without `certain`"
                } else {
                    "declared without an initialiser"
                };
                diag = diag.with_label(Label::secondary(decl_span, decl_label));
            }
            self.diags.push(diag);
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
                    "weakshared" => FidanType::WeakShared(Box::new(inner)),
                    "pending" => FidanType::Pending(Box::new(inner)),
                    _ => {
                        // Unknown container base (e.g. `lis oftype integer`)
                        if self.registering {
                            return FidanType::Error;
                        }
                        let candidates = ["list", "dict", "map", "shared", "weakshared", "pending"];
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
            "handle" => FidanType::Handle,
            "nothing" | "null" | "none" => FidanType::Nothing,
            "dynamic" | "any" | "flexible" => FidanType::Dynamic,
            // First-class action/callable type
            "action" | "callable" | "fn" => FidanType::Function,
            // Bare container keywords without `oftype` — treat as dynamic rather than erroring
            "list" | "dict" | "map" | "shared" | "weakshared" | "pending" | "tuple" => {
                FidanType::Dynamic
            }
            _ => {
                // Might be a user-defined object type
                if self.objects.contains_key(&sym) {
                    return FidanType::Object(sym);
                }
                // Might be a user-defined enum type
                if self.enums.contains_key(&sym) {
                    return FidanType::Enum(sym);
                }
                // Unknown type — emit a diagnostic and suppress cascades
                let bad = s.to_string();
                if self.registering {
                    // In Pass 1 we just return Error as a placeholder;
                    // Pass 2 will emit the real E0105 diagnostic.
                    return FidanType::Error;
                }
                let builtin_names = [
                    "integer",
                    "float",
                    "boolean",
                    "string",
                    "handle",
                    "nothing",
                    "dynamic",
                    "list",
                    "dict",
                    "map",
                    "shared",
                    "weakshared",
                    "pending",
                ];
                let obj_names: Vec<String> = self
                    .objects
                    .keys()
                    .map(|k| self.interner.resolve(*k).to_string())
                    .collect();
                let mut candidates: Vec<&str> = builtin_names.to_vec();
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
                    certain: p.certain,
                    optional: p.optional,
                    has_default: p.default.is_some(),
                }
            })
            .collect();
        let ret_ty = return_ty
            .as_ref()
            .map(|t| self.resolve_type_expr(&t.clone()))
            .unwrap_or(FidanType::Dynamic);
        ActionInfo {
            params: param_infos,
            return_ty: ret_ty,
            span,
        }
    }

    // ── Scope helpers ─────────────────────────────────────────────────────

    fn push_scope(&mut self, kind: ScopeKind) {
        self.table.push_scope(kind);
        self.local_actions.push(FxHashMap::default());
        self.local_deprecated_actions
            .push(rustc_hash::FxHashSet::default());
    }

    fn pop_scope(&mut self) {
        self.table.pop_scope();
        if self.local_actions.len() > 1 {
            self.local_actions.pop();
        }
        if self.local_deprecated_actions.len() > 1 {
            self.local_deprecated_actions.pop();
        }
    }

    fn define_local_action(&mut self, name: Symbol, info: ActionInfo, is_deprecated: bool) {
        if let Some(scope) = self.local_actions.last_mut() {
            scope.insert(name, info);
        }
        if is_deprecated && let Some(scope) = self.local_deprecated_actions.last_mut() {
            scope.insert(name);
        }
    }

    fn lookup_action_info(&self, name: Symbol) -> Option<ActionInfo> {
        self.local_actions
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).cloned())
            .or_else(|| self.actions.get(&name).cloned())
    }

    fn is_action_deprecated(&self, name: Symbol) -> bool {
        for (actions, deprecated) in self
            .local_actions
            .iter()
            .zip(self.local_deprecated_actions.iter())
            .rev()
        {
            if actions.contains_key(&name) {
                return deprecated.contains(&name);
            }
        }
        self.deprecated_actions.contains(&name)
    }

    fn inject_this_and_parent(
        &mut self,
        this_ty: FidanType,
        parent_ty: Option<FidanType>,
        file: FileId,
    ) {
        self.inject_this_binding(this_ty, file);
        if let Some(ty) = parent_ty {
            let dummy = self.dummy_span();
            let parent = self.interner.intern("parent");
            self.table.define(
                parent,
                SymbolInfo {
                    kind: SymbolKind::Var,
                    ty,
                    span: dummy,
                    is_mutable: false,
                    initialized: Initialized::Yes,
                    const_value: None,
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
                const_value: None,
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

    fn decorator_has_name(&self, decorator: &Decorator, name: &str) -> bool {
        self.interner.resolve(decorator.name).as_ref() == name
    }

    fn has_marker_decorator(&self, decorators: &[Decorator], name: &str) -> bool {
        decorators.iter().any(|d| self.decorator_has_name(d, name))
    }

    fn extract_string_literal(&self, module: &Module, expr_id: ExprId) -> Option<String> {
        match module.arena.get_expr(expr_id) {
            Expr::StrLit { value, .. } => Some(value.clone()),
            _ => None,
        }
    }

    fn parse_extern_spec(
        &mut self,
        module: &Module,
        function_name: Symbol,
        decorators: &[Decorator],
    ) -> Option<ExternSpec> {
        let decorator = decorators
            .iter()
            .find(|d| self.decorator_has_name(d, "extern"))?;
        let mut positional = decorator.args.iter().filter(|arg| arg.name.is_none());
        let Some(lib_arg) = positional.next() else {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern requires a library identifier string as its first positional argument",
                decorator.span,
            );
            return None;
        };
        let Some(lib) = self.extract_string_literal(module, lib_arg.value) else {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern library identifier must be a string literal",
                decorator.span,
            );
            return None;
        };
        if positional.next().is_some() {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern accepts at most one positional argument",
                decorator.span,
            );
        }

        let mut symbol: Option<String> = None;
        let mut link: Option<String> = None;
        let mut abi = ExternAbiKind::Native;
        for arg in decorator.args.iter().filter(|arg| arg.name.is_some()) {
            let Some(name) = arg.name else { continue };
            let key = self.interner.resolve(name);
            match key.as_ref() {
                "symbol" => {
                    if let Some(value) = self.extract_string_literal(module, arg.value) {
                        symbol = Some(value);
                    } else {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0304"),
                            "@extern `symbol` must be a string literal",
                            decorator.span,
                        );
                    }
                }
                "link" => {
                    if let Some(value) = self.extract_string_literal(module, arg.value) {
                        link = Some(value);
                    } else {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0304"),
                            "@extern `link` must be a string literal",
                            decorator.span,
                        );
                    }
                }
                "abi" => {
                    if let Some(value) = self.extract_string_literal(module, arg.value) {
                        if value.eq_ignore_ascii_case("native") {
                            abi = ExternAbiKind::Native;
                        } else if value.eq_ignore_ascii_case("fidan") {
                            abi = ExternAbiKind::Fidan;
                        } else {
                            self.emit_error(
                                fidan_diagnostics::diag_code!("E0304"),
                                format!(
                                    "@extern `abi` must be either \"native\" or \"fidan\", got `{value}`"
                                ),
                                decorator.span,
                            );
                        }
                    } else {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0304"),
                            "@extern `abi` must be a string literal",
                            decorator.span,
                        );
                    }
                }
                other => {
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0304"),
                        format!("@extern does not support named argument `{other}`"),
                        decorator.span,
                    );
                }
            }
        }

        Some(ExternSpec {
            lib,
            symbol: symbol.unwrap_or_else(|| self.interner.resolve(function_name).to_string()),
            link,
            abi,
            span: decorator.span,
        })
    }

    fn native_extern_type_allowed(ty: &FidanType) -> bool {
        matches!(
            ty,
            FidanType::Integer | FidanType::Float | FidanType::Boolean | FidanType::Handle
        )
    }

    fn native_extern_return_type_allowed(ty: &FidanType) -> bool {
        Self::native_extern_type_allowed(ty) || matches!(ty, FidanType::Nothing)
    }

    fn validate_extern_action(
        &mut self,
        module: &Module,
        name: Symbol,
        ctx: ExternActionContext<'_>,
    ) {
        let Some(spec) = self.parse_extern_spec(module, name, ctx.decorators) else {
            if self.has_marker_decorator(ctx.decorators, "unsafe")
                && !self.has_marker_decorator(ctx.decorators, "extern")
            {
                self.emit_warning(
                    fidan_diagnostics::diag_code!("W2004"),
                    "@unsafe has no effect without @extern",
                    ctx.span,
                );
            }
            return;
        };

        if ctx.has_receiver || ctx.is_extension {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern is not allowed on receiver actions",
                ctx.span,
            );
        }
        if ctx.is_parallel {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern actions cannot be declared `parallel`",
                ctx.span,
            );
        }
        if self.has_marker_decorator(ctx.decorators, "precompile") {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@precompile cannot be used together with @extern",
                ctx.span,
            );
        }
        if !ctx.body.is_empty() {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern actions must omit their body",
                ctx.span,
            );
        }
        if matches!(spec.abi, ExternAbiKind::Fidan)
            && !self.has_marker_decorator(ctx.decorators, "unsafe")
        {
            self.emit_error(
                fidan_diagnostics::diag_code!("E0304"),
                "@extern(..., abi = \"fidan\") requires the @unsafe decorator",
                spec.span,
            );
        }

        for param in ctx.params {
            if param.optional || param.default.is_some() {
                let pname = self.interner.resolve(param.name);
                self.emit_error(
                    fidan_diagnostics::diag_code!("E0304"),
                    format!(
                        "@extern parameter `{pname}` cannot be optional or have a default value"
                    ),
                    param.span,
                );
            }
            if matches!(spec.abi, ExternAbiKind::Native) {
                let ty = self.resolve_type_expr(&param.ty);
                if !Self::native_extern_type_allowed(&ty) {
                    let ty_name = self.ty_name(&ty);
                    self.emit_error(
                        fidan_diagnostics::diag_code!("E0304"),
                        format!(
                            "native @extern parameter `{}` has unsupported type `{ty_name}`; use integer, float, boolean, or handle",
                            self.interner.resolve(param.name)
                        ),
                        param.span,
                    );
                }
            }
        }

        if matches!(spec.abi, ExternAbiKind::Native)
            && let Some(return_ty) = ctx.return_ty.as_ref()
        {
            let resolved_return_ty = self.resolve_type_expr(return_ty);
            if !Self::native_extern_return_type_allowed(&resolved_return_ty) {
                let ty_name = self.ty_name(&resolved_return_ty);
                self.emit_error(
                    fidan_diagnostics::diag_code!("E0304"),
                    format!(
                        "native @extern action `{}` has unsupported return type `{ty_name}`; use integer, float, boolean, nothing, or handle",
                        self.interner.resolve(name)
                    ),
                    ctx.span,
                );
            }
        }
    }

    /// Validate a list of decorators, emitting W2004 for any that are not
    /// recognised by the compiler, and E0303 / E0304 for signature mismatches.
    ///
    /// Recognised decorators: `precompile`, `deprecated`, and any user-defined
    /// action that is in scope (custom decorator pattern from §22.11).
    ///
    /// For user-defined decorators the following are also checked:
    ///
    /// - **E0303**: the first parameter of the decorator action must be typed
    ///   `action` (`FidanType::Function`) or `flexible` (`FidanType::Dynamic`).
    ///   Any other concrete type (e.g. `string`, `integer`) is an error because
    ///   the runtime will pass a callable value there.
    ///
    /// - **E0304**: extra arguments passed to the decorator (`@dec(arg1, …)`)
    ///   must match the number of *remaining* parameters (params after the first
    ///   `action` param).  Too many or too few is an error.
    fn check_decorators(&mut self, decorators: &[Decorator], params: &[Param]) {
        for dec in decorators {
            let name = self.interner.resolve(dec.name);
            if BUILTIN_DECORATORS.contains(&name.as_ref()) {
                continue; // built-in decorator — always valid
            }
            // A user-defined action in scope is a valid custom decorator.
            let is_user_action = self
                .table
                .lookup(dec.name)
                .map(|i| matches!(i.kind, SymbolKind::Action))
                .unwrap_or(false);
            if !is_user_action {
                self.emit_warning(
                    fidan_diagnostics::diag_code!("W2004"),
                    format!("unknown decorator `@{name}` — will be ignored at runtime"),
                    dec.span,
                );
                continue;
            }

            // ── Signature checks for user-defined decorators ──────────────
            // Clone the ActionInfo to avoid a simultaneous borrow on &self.
            let info_opt = self.lookup_action_info(dec.name);
            if let Some(info) = info_opt {
                // E0303 — first param must accept an `action` value.
                if let Some(first) = info.params.first() {
                    let ok = matches!(
                        first.ty,
                        FidanType::Function | FidanType::Dynamic | FidanType::Unknown
                    );
                    if !ok {
                        let got = self.ty_name(&first.ty);
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0303"),
                            format!(
                                "decorator `@{name}` expects its first parameter to be `action` \
                                 but it is typed `{got}` — the runtime will pass a callable here"
                            ),
                            dec.span,
                        );
                    }
                    // E0304 — extra literal args must match remaining params.
                    let expected_extra = info.params.len().saturating_sub(1);
                    let got_extra = dec.args.len();
                    if got_extra != expected_extra {
                        self.emit_error(
                            fidan_diagnostics::diag_code!("E0304"),
                            format!(
                                "decorator `@{name}` expects {expected_extra} extra argument{} \
                                 after the `action` parameter, but {} {} provided",
                                if expected_extra == 1 { "" } else { "s" },
                                got_extra,
                                if got_extra == 1 { "was" } else { "were" },
                            ),
                            dec.span,
                        );
                    }
                } else {
                    // Decorator has no params at all — treat as marker-only; no checks.
                }
            }
        }

        let _ = params;
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
                if let Some(info) = self.table.lookup(name)
                    && matches!(info.kind, SymbolKind::BuiltinAction | SymbolKind::Action)
                {
                    let n = self.interner.resolve(name).to_string();
                    self.emit_warning(
                        fidan_diagnostics::diag_code!("W2003"),
                        format!("bare reference to action `{n}` has no effect — did you mean to call it with `{n}(...)`?"),
                        span,
                    );
                }
            }
            _ => {}
        }
    }

    fn ty_name(&self, ty: &FidanType) -> String {
        ty.display_name(&|sym| self.interner.resolve(sym).to_string())
    }
}
