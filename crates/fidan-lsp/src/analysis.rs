//! Lightweight analysis pass: lex + parse + type-check a source text and
//! collect LSP diagnostics, semantic tokens, and the per-document symbol table.

use crate::convert::span_to_range;
use crate::{semantic, symbols};
use fidan_ast::{Expr, Item, Module, TypeExpr};
use fidan_diagnostics::{Diagnostic as FidanDiag, Severity};
use fidan_lexer::{Lexer, SymbolInterner, TokenKind};
use fidan_source::{FileId, SourceFile, Span};
use fidan_typeck::CrossModuleCallSite;
use std::sync::Arc;
use tower_lsp::lsp_types::{self as lsp, DiagnosticSeverity, SemanticToken};

// ── Inlay hint site ───────────────────────────────────────────────────────────

/// A position in the source where the LSP should show a synthetic label.
#[derive(Debug, Clone)]
pub struct InlayHintSite {
    /// Byte offset in the source at which to insert the label
    /// (placed immediately *after* the relevant identifier).
    pub byte_offset: u32,
    /// Text to display, e.g. `": integer"`.
    pub label: String,
    /// `true` → type annotation, `false` → parameter name.
    pub is_type_hint: bool,
}

#[derive(Debug, Clone)]
pub struct MemberAccessSite {
    pub member_span: Span,
    pub receiver_type: String,
    pub member_name: String,
}

#[derive(Debug, Clone)]
pub struct DynamicVarCallSite {
    pub var_name: String,
    pub decl_span: Span,
    pub receiver_type: String,
    pub method_name: String,
}

#[derive(Debug, Clone)]
pub struct StdlibVarCallSite {
    pub var_name: String,
    pub decl_span: Span,
    pub module_name: String,
    pub member_name: String,
}

#[derive(Debug, Clone)]
pub struct StdlibCallSite {
    pub callee_span: Span,
    pub module_name: String,
    pub member_name: String,
    pub arg_tys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportedConstructorCallSite {
    pub var_name: String,
    pub decl_span: Span,
    pub import_binding: String,
    pub constructor_name: String,
    pub is_namespace_alias: bool,
}

#[derive(Debug, Clone)]
pub struct ReceiverChainMethodCallSite {
    pub receiver_segments: Vec<String>,
    pub receiver_offset: u32,
    pub method_name: String,
    pub arg_tys: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileImport {
    pub path: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserModuleImport {
    pub path: Vec<String>,
    pub alias: Option<String>,
    pub grouped: bool,
}

/// Output of a single analysis run.
pub struct AnalysisResult {
    pub diagnostics: Vec<lsp::Diagnostic>,
    pub semantic_tokens: Vec<SemanticToken>,
    /// Every identifier token: (span, resolved name). Used for hover / go-to-def.
    pub identifier_spans: Vec<(Span, String)>,
    /// Per-document symbol table built from declarations. Used for hover / completion.
    pub symbol_table: symbols::SymbolTable,
    /// File-path imports declared in this document.
    /// `alias = None` means `use "file.fdn"` wildcard/flat import semantics.
    /// `alias = Some(name)` means `use "file.fdn" as name` namespace import semantics.
    pub imports: Vec<FileImport>,
    /// Non-stdlib user-module imports declared in this document.
    pub user_module_imports: Vec<UserModuleImport>,
    /// Stdlib imports: `(alias_name, module_name)`.
    /// E.g. `use std.io` → `("io", "io")`; `use std.math as m` → `("m", "math")`.
    pub stdlib_imports: Vec<(String, String)>,
    /// Grouped stdlib imports flattened into local scope.
    /// E.g. `use std.collections.{enumerate}` → `("enumerate", "collections", "enumerate")`.
    pub stdlib_direct_imports: Vec<(String, String, String)>,
    /// Non-call member accesses where the target type has a cross-module parent.
    pub cross_module_field_accesses: Vec<(String, String, Span)>,
    /// Method call sites on cross-module receivers, with inferred arg types.
    pub cross_module_call_sites: Vec<CrossModuleCallSite>,
    /// Top-level `var x = recv.method()` where `method` resolved to Dynamic (cross-module).
    /// Stored as `(var_name, receiver_type_name, method_name)` so the server can patch
    /// the symbol-table entry after loading cross-module docs.
    pub dynamic_var_call_sites: Vec<DynamicVarCallSite>,
    /// Top-level `var x = std_alias.member()` calls where the alias resolves to `std.<module>`.
    /// Stored as `(var_name, module_name, member_name)` so the server can patch the
    /// symbol-table entry using shared stdlib metadata.
    pub stdlib_var_call_sites: Vec<StdlibVarCallSite>,
    /// All stdlib call sites with typed argument metadata.
    pub stdlib_call_sites: Vec<StdlibCallSite>,
    /// `var x = Foo()` or `var x = module.Foo()` sites that may resolve to an
    /// imported object constructor once imported documents are loaded.
    pub imported_constructor_call_sites: Vec<ImportedConstructorCallSite>,
    /// Method calls on receiver chains. Used after imported-constructor
    /// variable patching so calls like `storage.tasks.contains(...)` can be validated.
    pub receiver_chain_method_call_sites: Vec<ReceiverChainMethodCallSite>,
    /// Positions where the editor should display synthetic type labels.
    pub inlay_hint_sites: Vec<InlayHintSite>,
    /// Typed member-access spans used for hover on literal/computed receivers.
    pub member_access_sites: Vec<MemberAccessSite>,
}

/// Lex, parse and type-check `text`, returning all diagnostics as LSP
/// `Diagnostic` objects and a full set of semantic tokens for the document.
///
/// The `uri` string is used as the "file name" inside `SourceFile` so that
/// diagnostics printed to stderr (if any) show a meaningful path.
pub fn analyze(text: &str, uri_str: &str) -> AnalysisResult {
    let file = SourceFile::new(FileId(0), uri_str, text);
    let interner = Arc::new(SymbolInterner::new());

    // ── Lex ──────────────────────────────────────────────────────────────────
    let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();

    // ── Parse ─────────────────────────────────────────────────────────────────
    let (module, parse_diags) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));

    // ── Identifier-span index (for hover / go-to-def positional lookup) ────────
    let mut identifier_spans: Vec<(Span, String)> = tokens
        .iter()
        .filter_map(|tok| match &tok.kind {
            TokenKind::Ident(sym) => Some((tok.span, interner.resolve(*sym).to_string())),
            TokenKind::This => Some((tok.span, "this".to_string())),
            TokenKind::Parent => Some((tok.span, "parent".to_string())),
            TokenKind::Shared => Some((tok.span, "Shared".to_string())),
            TokenKind::Pending => Some((tok.span, "Pending".to_string())),
            TokenKind::Weak => Some((tok.span, "WeakShared".to_string())),
            _ => None,
        })
        .collect();
    augment_identifier_spans_with_ast(&module, &interner, text, &mut identifier_spans);

    // ── Type-check (full — needed to build hover/completion symbol table) ────
    let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    let symbol_table = symbols::build(&module, &typed, &interner);

    // ── File-path import extraction ─────────────────────────────────────
    let imports = extract_imports(&module, &interner);

    // ── User-module import extraction ───────────────────────────────────
    let user_module_imports = extract_user_module_imports(&module, &interner);

    // ── Stdlib import extraction (`use std.<module>`) ────────────────────
    let stdlib_imports = extract_stdlib_imports(&module, &interner);
    let stdlib_direct_imports = extract_stdlib_direct_imports(&module, &interner);

    // ── Dynamic var call sites (cross-module method return type patching) ──
    let dynamic_var_call_sites = extract_dynamic_var_calls(&module, &typed, &interner);
    let stdlib_var_call_sites = extract_stdlib_var_call_sites(&module, &interner, &stdlib_imports);
    let stdlib_call_sites = extract_stdlib_call_sites(
        &module,
        &typed,
        &interner,
        text,
        &stdlib_imports,
        &stdlib_direct_imports,
    );
    let imported_constructor_call_sites =
        extract_imported_constructor_call_sites(&module, &interner);
    let receiver_chain_method_call_sites =
        extract_receiver_chain_method_call_sites(&module, &typed, &interner);

    // ── Inlay hints (untyped var declarations) ────────────────────────────────
    let inlay_hint_sites =
        collect_inlay_hints(&module, &interner, &symbol_table, &identifier_spans);
    let member_access_sites = collect_member_access_sites(&module, &typed, &interner, text);

    // Consume typed fields now (after all borrows of `typed` are done).
    let typeck_diags = typed.diagnostics;
    let cross_module_field_accesses = typed.cross_module_field_accesses;
    let cross_module_call_sites = typed.cross_module_call_sites;

    // ── Diagnostics ───────────────────────────────────────────────────────────
    let diagnostics = lex_diags
        .into_iter()
        .chain(parse_diags)
        .chain(typeck_diags)
        .map(|d| fidan_to_lsp(&d, &file))
        .collect();

    // ── Semantic tokens ───────────────────────────────────────────────────────
    let semantic_tokens = semantic::compute(&tokens, &file, &interner, &module, &symbol_table);

    AnalysisResult {
        diagnostics,
        semantic_tokens,
        identifier_spans,
        symbol_table,
        imports,
        user_module_imports,
        stdlib_imports,
        stdlib_direct_imports,
        cross_module_field_accesses,
        cross_module_call_sites,
        dynamic_var_call_sites,
        stdlib_var_call_sites,
        stdlib_call_sites,
        imported_constructor_call_sites,
        receiver_chain_method_call_sites,
        inlay_hint_sites,
        member_access_sites,
    }
}

/// Extract file-path imports, preserving wildcard-vs-alias semantics.
fn extract_imports(module: &Module, interner: &SymbolInterner) -> Vec<FileImport> {
    module
        .items
        .iter()
        .filter_map(|&iid| {
            if let Item::Use { path, alias, .. } = module.arena.get_item(iid) {
                if path.len() != 1 {
                    return None;
                }
                let path_str = interner.resolve(path[0]).to_string();
                // Only treat as a file-path import if it looks like a path.
                let is_file = path_str.ends_with(".fdn")
                    || path_str.starts_with("./")
                    || path_str.starts_with("../")
                    || path_str.starts_with('/');
                if !is_file {
                    return None;
                }
                Some(FileImport {
                    path: path_str,
                    alias: alias.map(|a| interner.resolve(a).to_string()),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Extract non-stdlib user-module imports (`use mymod`, `use mymod.{name}`),
/// preserving whether they bind a namespace or a direct imported symbol.
fn extract_user_module_imports(
    module: &Module,
    interner: &SymbolInterner,
) -> Vec<UserModuleImport> {
    module
        .items
        .iter()
        .filter_map(|&iid| {
            if let Item::Use {
                path,
                alias,
                grouped,
                ..
            } = module.arena.get_item(iid)
            {
                if path.is_empty() {
                    return None;
                }
                let first = interner.resolve(path[0]);
                let is_stdlib = first.as_ref() == "std";
                let is_file = first.starts_with("./")
                    || first.starts_with("../")
                    || first.starts_with('/')
                    || first.ends_with(".fdn");
                if is_stdlib || is_file {
                    return None;
                }
                Some(UserModuleImport {
                    path: path
                        .iter()
                        .map(|sym| interner.resolve(*sym).to_string())
                        .collect(),
                    alias: alias.map(|a| interner.resolve(a).to_string()),
                    grouped: *grouped,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Extract `(alias, module_name)` pairs from `use std.<module>` items (namespace imports only).
/// Grouped/destructured imports (`use std.io.{fn}`) are excluded — those inject free names.
fn extract_stdlib_imports(module: &Module, interner: &SymbolInterner) -> Vec<(String, String)> {
    module
        .items
        .iter()
        .filter_map(|&iid| {
            if let Item::Use {
                path,
                alias,
                grouped,
                ..
            } = module.arena.get_item(iid)
            {
                // Need at least `std` + one module segment.
                if path.len() < 2 {
                    return None;
                }
                // Must start with `std`.
                if interner.resolve(path[0]).as_ref() != "std" {
                    return None;
                }
                // Skip grouped imports — they flatten names into scope, not a namespace.
                if *grouped {
                    return None;
                }
                // Module name is the second segment (e.g. "io", "math").
                let module_name = interner.resolve(path[1]).to_string();
                // Alias: explicit `as name` or implicit last segment.
                let alias_str = alias
                    .map(|a| interner.resolve(a).to_string())
                    .unwrap_or_else(|| module_name.clone());
                Some((alias_str, module_name))
            } else {
                None
            }
        })
        .collect()
}

fn extract_stdlib_direct_imports(
    module: &Module,
    interner: &SymbolInterner,
) -> Vec<(String, String, String)> {
    module
        .items
        .iter()
        .filter_map(|&iid| {
            if let Item::Use {
                path,
                alias,
                grouped,
                ..
            } = module.arena.get_item(iid)
            {
                if path.len() < 3 || !grouped {
                    return None;
                }
                if interner.resolve(path[0]).as_ref() != "std" {
                    return None;
                }
                let module_name = interner.resolve(path[1]).to_string();
                let member_name = interner.resolve(*path.last()?).to_string();
                let binding_name = alias
                    .map(|sym| interner.resolve(sym).to_string())
                    .unwrap_or_else(|| member_name.clone());
                Some((binding_name, module_name, member_name))
            } else {
                None
            }
        })
        .collect()
}

/// Collect top-level `var x = recv.method()` sites where the call's return type
/// resolved to `Dynamic` (cross-module receiver).  The server uses these to
/// retrospectively patch `x`'s symbol-table entry once the imported doc is loaded.
fn extract_dynamic_var_calls(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
) -> Vec<DynamicVarCallSite> {
    let mut out = Vec::new();

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                name,
                span,
                init: Some(init_eid),
                ..
            } => maybe_push_dynamic_var_call(
                module, typed, interner, *name, *span, *init_eid, &mut out,
            ),
            Item::ActionDecl { body, .. }
            | Item::ExtensionAction { body, .. }
            | Item::TestDecl { body, .. } => {
                collect_stmt_dynamic_var_calls(module, body, typed, interner, &mut out)
            }
            Item::Stmt(stmt_id) => {
                collect_stmt_dynamic_var_calls(module, &[*stmt_id], typed, interner, &mut out)
            }
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl { body, .. } = module.arena.get_item(mid) {
                        collect_stmt_dynamic_var_calls(module, body, typed, interner, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn maybe_push_dynamic_var_call(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    var_name_sym: fidan_lexer::Symbol,
    decl_span: Span,
    init_eid: fidan_ast::ExprId,
    out: &mut Vec<DynamicVarCallSite>,
) {
    if !matches!(
        typed.expr_types.get(&init_eid),
        Some(fidan_typeck::FidanType::Dynamic) | None
    ) {
        return;
    }
    if let Expr::Call { callee, .. } = module.arena.get_expr(init_eid)
        && let Expr::Field { object, field, .. } = module.arena.get_expr(*callee)
        && let Some(fidan_typeck::FidanType::Object(obj_sym)) = typed.expr_types.get(object)
    {
        out.push(DynamicVarCallSite {
            var_name: interner.resolve(var_name_sym).to_string(),
            decl_span,
            receiver_type: interner.resolve(*obj_sym).to_string(),
            method_name: interner.resolve(*field).to_string(),
        });
    }
}

fn extract_stdlib_var_call_sites(
    module: &Module,
    interner: &SymbolInterner,
    stdlib_imports: &[(String, String)],
) -> Vec<StdlibVarCallSite> {
    let stdlib_aliases: std::collections::HashMap<&str, &str> = stdlib_imports
        .iter()
        .map(|(alias, module_name)| (alias.as_str(), module_name.as_str()))
        .collect();
    let mut out = Vec::new();

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                name,
                span,
                init: Some(init_eid),
                ..
            } => maybe_push_stdlib_var_call(
                module,
                interner,
                &stdlib_aliases,
                *name,
                *span,
                *init_eid,
                &mut out,
            ),
            Item::ActionDecl { body, .. }
            | Item::ExtensionAction { body, .. }
            | Item::TestDecl { body, .. } => {
                collect_stmt_stdlib_var_calls(module, body, interner, &stdlib_aliases, &mut out)
            }
            Item::Stmt(stmt_id) => collect_stmt_stdlib_var_calls(
                module,
                &[*stmt_id],
                interner,
                &stdlib_aliases,
                &mut out,
            ),
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl { body, .. } = module.arena.get_item(mid) {
                        collect_stmt_stdlib_var_calls(
                            module,
                            body,
                            interner,
                            &stdlib_aliases,
                            &mut out,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    out
}

fn extract_stdlib_call_sites(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    text: &str,
    stdlib_imports: &[(String, String)],
    stdlib_direct_imports: &[(String, String, String)],
) -> Vec<StdlibCallSite> {
    let stdlib_aliases: std::collections::HashMap<&str, &str> = stdlib_imports
        .iter()
        .map(|(alias, module_name)| (alias.as_str(), module_name.as_str()))
        .collect();
    let stdlib_direct_bindings: std::collections::HashMap<&str, (&str, &str)> =
        stdlib_direct_imports
            .iter()
            .map(|(binding, module_name, member_name)| {
                (
                    binding.as_str(),
                    (module_name.as_str(), member_name.as_str()),
                )
            })
            .collect();
    let mut collector = StdlibCallSiteCollector {
        module,
        typed,
        interner,
        text,
        stdlib_aliases,
        stdlib_direct_bindings,
        out: Vec::new(),
    };

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                init: Some(init_eid),
                ..
            } => collector.collect_expr(*init_eid),
            Item::ActionDecl { body, .. }
            | Item::ExtensionAction { body, .. }
            | Item::TestDecl { body, .. } => collector.collect_stmts(body),
            Item::Stmt(stmt_id) => collector.collect_stmts(&[*stmt_id]),
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl { body, .. } = module.arena.get_item(mid) {
                        collector.collect_stmts(body);
                    }
                }
            }
            _ => {}
        }
    }

    collector.out
}

struct StdlibCallSiteCollector<'a> {
    module: &'a Module,
    typed: &'a fidan_typeck::TypedModule,
    interner: &'a SymbolInterner,
    text: &'a str,
    stdlib_aliases: std::collections::HashMap<&'a str, &'a str>,
    stdlib_direct_bindings: std::collections::HashMap<&'a str, (&'a str, &'a str)>,
    out: Vec<StdlibCallSite>,
}

impl StdlibCallSiteCollector<'_> {
    fn collect_stmts(&mut self, stmts: &[fidan_ast::StmtId]) {
        for &sid in stmts {
            match self.module.arena.get_stmt(sid) {
                fidan_ast::Stmt::VarDecl { init, .. } => {
                    if let Some(init_eid) = init {
                        self.collect_expr(*init_eid);
                    }
                }
                fidan_ast::Stmt::Destructure { value, .. }
                | fidan_ast::Stmt::Return {
                    value: Some(value), ..
                }
                | fidan_ast::Stmt::Expr { expr: value, .. }
                | fidan_ast::Stmt::Panic { value, .. } => {
                    self.collect_expr(*value);
                }
                fidan_ast::Stmt::Assign { target, value, .. } => {
                    self.collect_expr(*target);
                    self.collect_expr(*value);
                }
                fidan_ast::Stmt::ActionDecl {
                    params,
                    body,
                    decorators,
                    ..
                } => {
                    for param in params {
                        if let Some(default) = param.default {
                            self.collect_expr(default);
                        }
                    }
                    for decorator in decorators {
                        for arg in &decorator.args {
                            self.collect_expr(arg.value);
                        }
                    }
                    self.collect_stmts(body);
                }
                fidan_ast::Stmt::If {
                    condition,
                    then_body,
                    else_ifs,
                    else_body,
                    ..
                } => {
                    self.collect_expr(*condition);
                    self.collect_stmts(then_body);
                    for else_if in else_ifs {
                        self.collect_expr(else_if.condition);
                        self.collect_stmts(&else_if.body);
                    }
                    if let Some(else_body) = else_body {
                        self.collect_stmts(else_body);
                    }
                }
                fidan_ast::Stmt::Check {
                    scrutinee, arms, ..
                } => {
                    self.collect_expr(*scrutinee);
                    for arm in arms {
                        self.collect_expr(arm.pattern);
                        self.collect_stmts(&arm.body);
                    }
                }
                fidan_ast::Stmt::For { iterable, body, .. }
                | fidan_ast::Stmt::ParallelFor { iterable, body, .. } => {
                    self.collect_expr(*iterable);
                    self.collect_stmts(body);
                }
                fidan_ast::Stmt::While {
                    condition, body, ..
                } => {
                    self.collect_expr(*condition);
                    self.collect_stmts(body);
                }
                fidan_ast::Stmt::Attempt {
                    body,
                    catches,
                    otherwise,
                    finally,
                    ..
                } => {
                    self.collect_stmts(body);
                    for catch in catches {
                        self.collect_stmts(&catch.body);
                    }
                    if let Some(otherwise) = otherwise {
                        self.collect_stmts(otherwise);
                    }
                    if let Some(finally) = finally {
                        self.collect_stmts(finally);
                    }
                }
                fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                    for task in tasks {
                        self.collect_stmts(&task.body);
                    }
                }
                fidan_ast::Stmt::Return { value: None, .. }
                | fidan_ast::Stmt::Break { .. }
                | fidan_ast::Stmt::Continue { .. }
                | fidan_ast::Stmt::Error { .. } => {}
            }
        }
    }

    fn collect_expr(&mut self, expr_id: fidan_ast::ExprId) {
        match self.module.arena.get_expr(expr_id) {
            Expr::Call { callee, args, .. } => {
                let arg_tys = args
                    .iter()
                    .map(|arg| {
                        self.typed
                            .expr_types
                            .get(&arg.value)
                            .map(|ty| {
                                ty.display_name(&|sym| self.interner.resolve(sym).to_string())
                            })
                            .unwrap_or_else(|| "dynamic".to_string())
                    })
                    .collect::<Vec<_>>();

                if let Expr::Ident {
                    name,
                    span: callee_span,
                } = self.module.arena.get_expr(*callee)
                {
                    if let Some((module_name, member_name)) = self
                        .stdlib_direct_bindings
                        .get(self.interner.resolve(*name).as_ref())
                    {
                        self.out.push(StdlibCallSite {
                            callee_span: *callee_span,
                            module_name: (*module_name).to_string(),
                            member_name: (*member_name).to_string(),
                            arg_tys: arg_tys.clone(),
                        });
                    }
                } else if let Expr::Field {
                    object,
                    field,
                    span: field_span,
                } = self.module.arena.get_expr(*callee)
                    && let Expr::Ident { name, .. } = self.module.arena.get_expr(*object)
                {
                    let alias_name = self.interner.resolve(*name);
                    if let Some(module_name) = self.stdlib_aliases.get(alias_name.as_ref()) {
                        let member_name = self.interner.resolve(*field).to_string();
                        let callee_span =
                            find_identifier_span_in_range(self.text, *field_span, &member_name)
                                .unwrap_or(*field_span);
                        self.out.push(StdlibCallSite {
                            callee_span,
                            module_name: (*module_name).to_string(),
                            member_name,
                            arg_tys: arg_tys.clone(),
                        });
                    }
                }

                self.collect_expr(*callee);
                for arg in args {
                    self.collect_expr(arg.value);
                }
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::NullCoalesce { lhs, rhs, .. }
            | Expr::Assign {
                target: lhs,
                value: rhs,
                ..
            }
            | Expr::CompoundAssign {
                target: lhs,
                value: rhs,
                ..
            } => {
                self.collect_expr(*lhs);
                self.collect_expr(*rhs);
            }
            Expr::Unary { operand, .. }
            | Expr::Spawn { expr: operand, .. }
            | Expr::Await { expr: operand, .. } => self.collect_expr(*operand),
            Expr::Field { object, .. } => self.collect_expr(*object),
            Expr::Index { object, index, .. } => {
                self.collect_expr(*object);
                self.collect_expr(*index);
            }
            Expr::StringInterp { parts, .. } => {
                for part in parts {
                    if let fidan_ast::InterpPart::Expr(expr_id) = part {
                        self.collect_expr(*expr_id);
                    }
                }
            }
            Expr::Ternary {
                condition,
                then_val,
                else_val,
                ..
            } => {
                self.collect_expr(*condition);
                self.collect_expr(*then_val);
                self.collect_expr(*else_val);
            }
            Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
                for &element in elements {
                    self.collect_expr(element);
                }
            }
            Expr::Dict { entries, .. } => {
                for &(key, value) in entries {
                    self.collect_expr(key);
                    self.collect_expr(value);
                }
            }
            Expr::Check {
                scrutinee, arms, ..
            } => {
                self.collect_expr(*scrutinee);
                for arm in arms {
                    self.collect_expr(arm.pattern);
                }
            }
            Expr::Slice {
                target,
                start,
                end,
                step,
                ..
            } => {
                self.collect_expr(*target);
                if let Some(start) = start {
                    self.collect_expr(*start);
                }
                if let Some(end) = end {
                    self.collect_expr(*end);
                }
                if let Some(step) = step {
                    self.collect_expr(*step);
                }
            }
            Expr::ListComp {
                element,
                iterable,
                filter,
                ..
            } => {
                self.collect_expr(*element);
                self.collect_expr(*iterable);
                if let Some(filter) = filter {
                    self.collect_expr(*filter);
                }
            }
            Expr::DictComp {
                key,
                value,
                iterable,
                filter,
                ..
            } => {
                self.collect_expr(*key);
                self.collect_expr(*value);
                self.collect_expr(*iterable);
                if let Some(filter) = filter {
                    self.collect_expr(*filter);
                }
            }
            Expr::Lambda { params, body, .. } => {
                for param in params {
                    if let Some(default) = param.default {
                        self.collect_expr(default);
                    }
                }
                self.collect_stmts(body);
            }
            Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::StrLit { .. }
            | Expr::BoolLit { .. }
            | Expr::Nothing { .. }
            | Expr::Ident { .. }
            | Expr::This { .. }
            | Expr::Parent { .. }
            | Expr::Error { .. } => {}
        }
    }
}

fn collect_stmt_dynamic_var_calls(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    out: &mut Vec<DynamicVarCallSite>,
) {
    for &sid in stmts {
        match module.arena.get_stmt(sid) {
            fidan_ast::Stmt::VarDecl {
                name,
                span,
                init: Some(init_eid),
                ..
            } => maybe_push_dynamic_var_call(module, typed, interner, *name, *span, *init_eid, out),
            fidan_ast::Stmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_stmt_dynamic_var_calls(module, then_body, typed, interner, out);
                for else_if in else_ifs {
                    collect_stmt_dynamic_var_calls(module, &else_if.body, typed, interner, out);
                }
                if let Some(else_body) = else_body {
                    collect_stmt_dynamic_var_calls(module, else_body, typed, interner, out);
                }
            }
            fidan_ast::Stmt::Check { arms, .. } => {
                for arm in arms {
                    collect_stmt_dynamic_var_calls(module, &arm.body, typed, interner, out);
                }
            }
            fidan_ast::Stmt::For { body, .. }
            | fidan_ast::Stmt::While { body, .. }
            | fidan_ast::Stmt::ParallelFor { body, .. } => {
                collect_stmt_dynamic_var_calls(module, body, typed, interner, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_dynamic_var_calls(module, body, typed, interner, out);
                for catch in catches {
                    collect_stmt_dynamic_var_calls(module, &catch.body, typed, interner, out);
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_dynamic_var_calls(module, otherwise, typed, interner, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_dynamic_var_calls(module, finally, typed, interner, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_dynamic_var_calls(module, &task.body, typed, interner, out);
                }
            }
            _ => {}
        }
    }
}

fn collect_stmt_stdlib_var_calls(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    interner: &SymbolInterner,
    stdlib_aliases: &std::collections::HashMap<&str, &str>,
    out: &mut Vec<StdlibVarCallSite>,
) {
    for &sid in stmts {
        match module.arena.get_stmt(sid) {
            fidan_ast::Stmt::VarDecl {
                name,
                span,
                init: Some(init_eid),
                ..
            } => maybe_push_stdlib_var_call(
                module,
                interner,
                stdlib_aliases,
                *name,
                *span,
                *init_eid,
                out,
            ),
            fidan_ast::Stmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_stmt_stdlib_var_calls(module, then_body, interner, stdlib_aliases, out);
                for else_if in else_ifs {
                    collect_stmt_stdlib_var_calls(
                        module,
                        &else_if.body,
                        interner,
                        stdlib_aliases,
                        out,
                    );
                }
                if let Some(else_body) = else_body {
                    collect_stmt_stdlib_var_calls(module, else_body, interner, stdlib_aliases, out);
                }
            }
            fidan_ast::Stmt::Check { arms, .. } => {
                for arm in arms {
                    collect_stmt_stdlib_var_calls(module, &arm.body, interner, stdlib_aliases, out);
                }
            }
            fidan_ast::Stmt::For { body, .. }
            | fidan_ast::Stmt::While { body, .. }
            | fidan_ast::Stmt::ParallelFor { body, .. } => {
                collect_stmt_stdlib_var_calls(module, body, interner, stdlib_aliases, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_stdlib_var_calls(module, body, interner, stdlib_aliases, out);
                for catch in catches {
                    collect_stmt_stdlib_var_calls(
                        module,
                        &catch.body,
                        interner,
                        stdlib_aliases,
                        out,
                    );
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_stdlib_var_calls(module, otherwise, interner, stdlib_aliases, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_stdlib_var_calls(module, finally, interner, stdlib_aliases, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_stdlib_var_calls(
                        module,
                        &task.body,
                        interner,
                        stdlib_aliases,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

fn maybe_push_stdlib_var_call(
    module: &Module,
    interner: &SymbolInterner,
    stdlib_aliases: &std::collections::HashMap<&str, &str>,
    var_name_sym: fidan_lexer::Symbol,
    decl_span: Span,
    init_eid: fidan_ast::ExprId,
    out: &mut Vec<StdlibVarCallSite>,
) {
    if let Expr::Call { callee, .. } = module.arena.get_expr(init_eid)
        && let Expr::Field { object, field, .. } = module.arena.get_expr(*callee)
        && let Expr::Ident {
            name: recv_name, ..
        } = module.arena.get_expr(*object)
        && let Some(module_name) = stdlib_aliases.get(interner.resolve(*recv_name).as_ref())
    {
        out.push(StdlibVarCallSite {
            var_name: interner.resolve(var_name_sym).to_string(),
            decl_span,
            module_name: (*module_name).to_string(),
            member_name: interner.resolve(*field).to_string(),
        });
    }
}

fn extract_imported_constructor_call_sites(
    module: &Module,
    interner: &SymbolInterner,
) -> Vec<ImportedConstructorCallSite> {
    let mut out = Vec::new();

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                name,
                span,
                init: Some(init_eid),
                ..
            } => maybe_push_imported_constructor_call(
                module, interner, *name, *span, *init_eid, &mut out,
            ),
            Item::ActionDecl { body, .. }
            | Item::ExtensionAction { body, .. }
            | Item::TestDecl { body, .. } => {
                collect_stmt_imported_constructor_calls(module, body, interner, &mut out)
            }
            Item::Stmt(stmt_id) => {
                collect_stmt_imported_constructor_calls(module, &[*stmt_id], interner, &mut out)
            }
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl { body, .. } = module.arena.get_item(mid) {
                        collect_stmt_imported_constructor_calls(module, body, interner, &mut out);
                    }
                }
            }
            _ => {}
        }
    }

    out
}

fn collect_stmt_imported_constructor_calls(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    interner: &SymbolInterner,
    out: &mut Vec<ImportedConstructorCallSite>,
) {
    for &sid in stmts {
        match module.arena.get_stmt(sid) {
            fidan_ast::Stmt::VarDecl {
                name,
                span,
                init: Some(init_eid),
                ..
            } => {
                maybe_push_imported_constructor_call(module, interner, *name, *span, *init_eid, out)
            }
            fidan_ast::Stmt::If {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_stmt_imported_constructor_calls(module, then_body, interner, out);
                for else_if in else_ifs {
                    collect_stmt_imported_constructor_calls(module, &else_if.body, interner, out);
                }
                if let Some(else_body) = else_body {
                    collect_stmt_imported_constructor_calls(module, else_body, interner, out);
                }
            }
            fidan_ast::Stmt::Check { arms, .. } => {
                for arm in arms {
                    collect_stmt_imported_constructor_calls(module, &arm.body, interner, out);
                }
            }
            fidan_ast::Stmt::For { body, .. }
            | fidan_ast::Stmt::While { body, .. }
            | fidan_ast::Stmt::ParallelFor { body, .. } => {
                collect_stmt_imported_constructor_calls(module, body, interner, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_imported_constructor_calls(module, body, interner, out);
                for catch in catches {
                    collect_stmt_imported_constructor_calls(module, &catch.body, interner, out);
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_imported_constructor_calls(module, otherwise, interner, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_imported_constructor_calls(module, finally, interner, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_imported_constructor_calls(module, &task.body, interner, out);
                }
            }
            _ => {}
        }
    }
}

fn maybe_push_imported_constructor_call(
    module: &Module,
    interner: &SymbolInterner,
    var_name_sym: fidan_lexer::Symbol,
    decl_span: Span,
    init_eid: fidan_ast::ExprId,
    out: &mut Vec<ImportedConstructorCallSite>,
) {
    let Expr::Call { callee, .. } = module.arena.get_expr(init_eid) else {
        return;
    };

    match module.arena.get_expr(*callee) {
        Expr::Ident { name, .. } => out.push(ImportedConstructorCallSite {
            var_name: interner.resolve(var_name_sym).to_string(),
            decl_span,
            import_binding: interner.resolve(*name).to_string(),
            constructor_name: interner.resolve(*name).to_string(),
            is_namespace_alias: false,
        }),
        Expr::Field { object, field, .. }
            if matches!(module.arena.get_expr(*object), Expr::Ident { .. }) =>
        {
            let Expr::Ident { name, .. } = module.arena.get_expr(*object) else {
                return;
            };
            out.push(ImportedConstructorCallSite {
                var_name: interner.resolve(var_name_sym).to_string(),
                decl_span,
                import_binding: interner.resolve(*name).to_string(),
                constructor_name: interner.resolve(*field).to_string(),
                is_namespace_alias: true,
            });
        }
        _ => {}
    }
}

fn extract_receiver_chain_method_call_sites(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
) -> Vec<ReceiverChainMethodCallSite> {
    let mut out = Vec::new();

    for &iid in &module.items {
        match module.arena.get_item(iid) {
            Item::VarDecl {
                init: Some(init), ..
            } => {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *init, &mut out,
                );
            }
            Item::ActionDecl {
                params,
                body,
                decorators,
                ..
            }
            | Item::ExtensionAction {
                params,
                body,
                decorators,
                ..
            } => {
                for param in params {
                    if let Some(default) = param.default {
                        collect_expr_receiver_chain_method_call_sites(
                            module, typed, interner, default, &mut out,
                        );
                    }
                }
                for decorator in decorators {
                    for arg in &decorator.args {
                        collect_expr_receiver_chain_method_call_sites(
                            module, typed, interner, arg.value, &mut out,
                        );
                    }
                }
                collect_stmt_receiver_chain_method_call_sites(
                    module, typed, interner, body, &mut out,
                );
            }
            Item::TestDecl { body, .. } => {
                collect_stmt_receiver_chain_method_call_sites(
                    module, typed, interner, body, &mut out,
                );
            }
            Item::Stmt(stmt_id) => {
                collect_stmt_receiver_chain_method_call_sites(
                    module,
                    typed,
                    interner,
                    std::slice::from_ref(stmt_id),
                    &mut out,
                );
            }
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    if let Item::ActionDecl {
                        params,
                        body,
                        decorators,
                        ..
                    } = module.arena.get_item(mid)
                    {
                        for param in params {
                            if let Some(default) = param.default {
                                collect_expr_receiver_chain_method_call_sites(
                                    module, typed, interner, default, &mut out,
                                );
                            }
                        }
                        for decorator in decorators {
                            for arg in &decorator.args {
                                collect_expr_receiver_chain_method_call_sites(
                                    module, typed, interner, arg.value, &mut out,
                                );
                            }
                        }
                        collect_stmt_receiver_chain_method_call_sites(
                            module, typed, interner, body, &mut out,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    out
}

fn collect_stmt_receiver_chain_method_call_sites(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    stmts: &[fidan_ast::StmtId],
    out: &mut Vec<ReceiverChainMethodCallSite>,
) {
    for &sid in stmts {
        match module.arena.get_stmt(sid) {
            fidan_ast::Stmt::VarDecl { init, .. } => {
                if let Some(init) = init {
                    collect_expr_receiver_chain_method_call_sites(
                        module, typed, interner, *init, out,
                    );
                }
            }
            fidan_ast::Stmt::Destructure { value, .. }
            | fidan_ast::Stmt::Return {
                value: Some(value), ..
            }
            | fidan_ast::Stmt::Expr { expr: value, .. }
            | fidan_ast::Stmt::Panic { value, .. } => {
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, *value, out);
            }
            fidan_ast::Stmt::Assign { target, value, .. } => {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *target, out,
                );
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, *value, out);
            }
            fidan_ast::Stmt::ActionDecl {
                params,
                body,
                decorators,
                ..
            } => {
                for param in params {
                    if let Some(default) = param.default {
                        collect_expr_receiver_chain_method_call_sites(
                            module, typed, interner, default, out,
                        );
                    }
                }
                for decorator in decorators {
                    for arg in &decorator.args {
                        collect_expr_receiver_chain_method_call_sites(
                            module, typed, interner, arg.value, out,
                        );
                    }
                }
                collect_stmt_receiver_chain_method_call_sites(module, typed, interner, body, out);
            }
            fidan_ast::Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *condition, out,
                );
                collect_stmt_receiver_chain_method_call_sites(
                    module, typed, interner, then_body, out,
                );
                for else_if in else_ifs {
                    collect_expr_receiver_chain_method_call_sites(
                        module,
                        typed,
                        interner,
                        else_if.condition,
                        out,
                    );
                    collect_stmt_receiver_chain_method_call_sites(
                        module,
                        typed,
                        interner,
                        &else_if.body,
                        out,
                    );
                }
                if let Some(else_body) = else_body {
                    collect_stmt_receiver_chain_method_call_sites(
                        module, typed, interner, else_body, out,
                    );
                }
            }
            fidan_ast::Stmt::Check {
                scrutinee, arms, ..
            } => {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *scrutinee, out,
                );
                for arm in arms {
                    collect_expr_receiver_chain_method_call_sites(
                        module,
                        typed,
                        interner,
                        arm.pattern,
                        out,
                    );
                    collect_stmt_receiver_chain_method_call_sites(
                        module, typed, interner, &arm.body, out,
                    );
                }
            }
            fidan_ast::Stmt::For { iterable, body, .. }
            | fidan_ast::Stmt::ParallelFor { iterable, body, .. } => {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *iterable, out,
                );
                collect_stmt_receiver_chain_method_call_sites(module, typed, interner, body, out);
            }
            fidan_ast::Stmt::While {
                condition, body, ..
            } => {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *condition, out,
                );
                collect_stmt_receiver_chain_method_call_sites(module, typed, interner, body, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_receiver_chain_method_call_sites(module, typed, interner, body, out);
                for catch in catches {
                    collect_stmt_receiver_chain_method_call_sites(
                        module,
                        typed,
                        interner,
                        &catch.body,
                        out,
                    );
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_receiver_chain_method_call_sites(
                        module, typed, interner, otherwise, out,
                    );
                }
                if let Some(finally) = finally {
                    collect_stmt_receiver_chain_method_call_sites(
                        module, typed, interner, finally, out,
                    );
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_receiver_chain_method_call_sites(
                        module, typed, interner, &task.body, out,
                    );
                }
            }
            fidan_ast::Stmt::Return { value: None, .. }
            | fidan_ast::Stmt::Break { .. }
            | fidan_ast::Stmt::Continue { .. }
            | fidan_ast::Stmt::Error { .. } => {}
        }
    }
}

fn receiver_chain_segments(
    module: &Module,
    interner: &SymbolInterner,
    expr_id: fidan_ast::ExprId,
) -> Option<(Vec<String>, u32)> {
    match module.arena.get_expr(expr_id) {
        Expr::Ident { name, span } => Some((vec![interner.resolve(*name).to_string()], span.start)),
        Expr::This { span } => Some((vec!["this".to_string()], span.start)),
        Expr::Field { object, field, .. } => {
            let (mut segments, offset) = receiver_chain_segments(module, interner, *object)?;
            segments.push(interner.resolve(*field).to_string());
            Some((segments, offset))
        }
        _ => None,
    }
}

fn collect_expr_receiver_chain_method_call_sites(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    expr_id: fidan_ast::ExprId,
    out: &mut Vec<ReceiverChainMethodCallSite>,
) {
    match module.arena.get_expr(expr_id) {
        Expr::Call { callee, args, span } => {
            if let Expr::Field { object, field, .. } = module.arena.get_expr(*callee)
                && let Some((receiver_segments, receiver_offset)) =
                    receiver_chain_segments(module, interner, *object)
            {
                let arg_tys = args
                    .iter()
                    .map(|arg| {
                        typed
                            .expr_types
                            .get(&arg.value)
                            .map(|ty| ty.display_name(&|sym| interner.resolve(sym).to_string()))
                            .unwrap_or_else(|| "dynamic".to_string())
                    })
                    .collect();
                out.push(ReceiverChainMethodCallSite {
                    receiver_segments,
                    receiver_offset,
                    method_name: interner.resolve(*field).to_string(),
                    arg_tys,
                    span: *span,
                });
            }
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *callee, out);
            for arg in args {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, arg.value, out,
                );
            }
        }
        Expr::Binary { lhs, rhs, .. }
        | Expr::NullCoalesce { lhs, rhs, .. }
        | Expr::Assign {
            target: lhs,
            value: rhs,
            ..
        }
        | Expr::CompoundAssign {
            target: lhs,
            value: rhs,
            ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *lhs, out);
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *rhs, out);
        }
        Expr::Unary { operand, .. }
        | Expr::Spawn { expr: operand, .. }
        | Expr::Await { expr: operand, .. }
        | Expr::Field {
            object: operand, ..
        }
        | Expr::Index {
            object: operand, ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *operand, out);
            if let Expr::Index { index, .. } = module.arena.get_expr(expr_id) {
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, *index, out);
            }
        }
        Expr::StringInterp { parts, .. } => {
            for part in parts {
                if let fidan_ast::InterpPart::Expr(expr_id) = part {
                    collect_expr_receiver_chain_method_call_sites(
                        module, typed, interner, *expr_id, out,
                    );
                }
            }
        }
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *condition, out);
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *then_val, out);
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *else_val, out);
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            for &element in elements {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, element, out,
                );
            }
        }
        Expr::Dict { entries, .. } => {
            for &(key, value) in entries {
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, key, out);
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, value, out);
            }
        }
        Expr::Check {
            scrutinee, arms, ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *scrutinee, out);
            for arm in arms {
                collect_expr_receiver_chain_method_call_sites(
                    module,
                    typed,
                    interner,
                    arm.pattern,
                    out,
                );
                collect_stmt_receiver_chain_method_call_sites(
                    module, typed, interner, &arm.body, out,
                );
            }
        }
        Expr::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *target, out);
            if let Some(start) = start {
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, *start, out);
            }
            if let Some(end) = end {
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, *end, out);
            }
            if let Some(step) = step {
                collect_expr_receiver_chain_method_call_sites(module, typed, interner, *step, out);
            }
        }
        Expr::ListComp {
            element,
            iterable,
            filter,
            ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *element, out);
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *iterable, out);
            if let Some(filter) = filter {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *filter, out,
                );
            }
        }
        Expr::DictComp {
            key,
            value,
            iterable,
            filter,
            ..
        } => {
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *key, out);
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *value, out);
            collect_expr_receiver_chain_method_call_sites(module, typed, interner, *iterable, out);
            if let Some(filter) = filter {
                collect_expr_receiver_chain_method_call_sites(
                    module, typed, interner, *filter, out,
                );
            }
        }
        Expr::Lambda { params, body, .. } => {
            for param in params {
                if let Some(default) = param.default {
                    collect_expr_receiver_chain_method_call_sites(
                        module, typed, interner, default, out,
                    );
                }
            }
            collect_stmt_receiver_chain_method_call_sites(module, typed, interner, body, out);
        }
        Expr::Ident { .. }
        | Expr::This { .. }
        | Expr::Parent { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::StrLit { .. }
        | Expr::BoolLit { .. }
        | Expr::Nothing { .. }
        | Expr::Error { .. } => {}
    }
}

fn fidan_to_lsp(d: &FidanDiag, file: &SourceFile) -> lsp::Diagnostic {
    // Encode machine-applicable suggestions as JSON in `data` so the
    // `code_action` handler can offer quick-fix actions without re-running
    // the full analysis pipeline.
    let data: Option<serde_json::Value> = if d.suggestions.is_empty() {
        None
    } else {
        let fixes: Vec<serde_json::Value> = d
            .suggestions
            .iter()
            .filter_map(|s| {
                let edit = s.edit.as_ref()?;
                Some(serde_json::json!({
                    "message": s.message,
                    "start":   edit.span.start,
                    "end":     edit.span.end,
                    "replacement": edit.replacement,
                }))
            })
            .collect();
        if fixes.is_empty() {
            None
        } else {
            Some(serde_json::json!(fixes))
        }
    };

    lsp::Diagnostic {
        range: span_to_range(file, d.span),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Note => DiagnosticSeverity::INFORMATION,
        }),
        code: Some(lsp::NumberOrString::String(d.code.clone())),
        source: Some("fidan".to_string()),
        message: d.message.clone(),
        related_information: None,
        tags: if d.code == "W1006" {
            Some(vec![lsp::DiagnosticTag::UNNECESSARY])
        } else {
            None
        },
        code_description: None,
        data,
    }
}

/// Collect inlay hint sites from a module.
///
/// Currently emits a `": type"` label after the name of every `var`/`const var`
/// declaration that has **no explicit type annotation** but whose type was
/// successfully inferred during the type-check pass.
fn collect_inlay_hints(
    module: &Module,
    interner: &SymbolInterner,
    symbol_table: &symbols::SymbolTable,
    identifier_spans: &[(Span, String)],
) -> Vec<InlayHintSite> {
    let mut hints = Vec::new();
    for &iid in &module.items {
        if let Item::VarDecl { name, ty: None, .. } = module.arena.get_item(iid) {
            let name_str = interner.resolve(*name).to_string();
            // Only emit if the symbol table resolved a concrete type.
            let entry = match symbol_table.get(&name_str) {
                Some(e) => e,
                None => continue,
            };
            // Extract type from hover detail: `"```fidan\nvar x: integer\n```"`.
            // Grab the part after the `:` on the middle line.
            let type_label = extract_type_from_detail(&entry.detail);
            if type_label == "?" {
                continue; // unresolved — don't clutter the editor
            }
            // Find the identifier token span for the variable name in the source.
            // Use the first occurrence that matches the declared name exactly.
            if let Some((span, _)) = identifier_spans.iter().find(|(_, n)| n == &name_str) {
                hints.push(InlayHintSite {
                    byte_offset: span.end,
                    label: format!(" -> {}", type_label),
                    is_type_hint: true,
                });
            }
        }
    }
    hints
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn find_identifier_span_in_range(text: &str, search_span: Span, target: &str) -> Option<Span> {
    let bytes = text.as_bytes();
    let target_bytes = target.as_bytes();
    let start = search_span.start as usize;
    let end = search_span.end as usize;
    if start >= end
        || end > bytes.len()
        || target_bytes.is_empty()
        || target_bytes.len() > end - start
    {
        return None;
    }

    for offset in (0..=end - start - target_bytes.len()).rev() {
        let candidate_start = start + offset;
        let candidate_end = candidate_start + target_bytes.len();
        if &bytes[candidate_start..candidate_end] != target_bytes {
            continue;
        }
        let left_ok = candidate_start == start || !is_ident_byte(bytes[candidate_start - 1]);
        let right_ok = candidate_end == end || !is_ident_byte(bytes[candidate_end]);
        if left_ok && right_ok {
            return Some(Span::new(
                search_span.file,
                candidate_start as u32,
                candidate_end as u32,
            ));
        }
    }

    None
}

fn augment_identifier_spans_with_ast(
    module: &Module,
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<(Span, String)>,
) {
    for &item_id in &module.items {
        collect_item_identifier_spans(module, item_id, interner, text, out);
    }
    out.sort_by(|(left_span, left_name), (right_span, right_name)| {
        left_span
            .start
            .cmp(&right_span.start)
            .then(left_span.end.cmp(&right_span.end))
            .then(left_name.cmp(right_name))
    });
    out.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
}

fn collect_item_identifier_spans(
    module: &Module,
    item_id: fidan_ast::ItemId,
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<(Span, String)>,
) {
    match module.arena.get_item(item_id) {
        Item::VarDecl { ty, init, .. } => {
            if let Some(ty) = ty {
                collect_type_expr_identifier_spans(ty, interner, out);
            }
            if let Some(init) = init {
                collect_expr_identifier_spans(module, *init, interner, text, out);
            }
        }
        Item::ExprStmt(expr_id) => {
            collect_expr_identifier_spans(module, *expr_id, interner, text, out)
        }
        Item::Assign { target, value, .. } => {
            collect_expr_identifier_spans(module, *target, interner, text, out);
            collect_expr_identifier_spans(module, *value, interner, text, out);
        }
        Item::Destructure { value, .. } => {
            collect_expr_identifier_spans(module, *value, interner, text, out)
        }
        Item::ObjectDecl {
            fields, methods, ..
        } => {
            for field in fields {
                collect_type_expr_identifier_spans(&field.ty, interner, out);
                if let Some(default) = field.default {
                    collect_expr_identifier_spans(module, default, interner, text, out);
                }
            }
            for &method_id in methods {
                collect_item_identifier_spans(module, method_id, interner, text, out);
            }
        }
        Item::ActionDecl {
            params,
            return_ty,
            body,
            decorators,
            ..
        }
        | Item::ExtensionAction {
            params,
            return_ty,
            body,
            decorators,
            ..
        } => {
            for param in params {
                collect_type_expr_identifier_spans(&param.ty, interner, out);
                if let Some(default) = param.default {
                    collect_expr_identifier_spans(module, default, interner, text, out);
                }
            }
            if let Some(return_ty) = return_ty {
                collect_type_expr_identifier_spans(return_ty, interner, out);
            }
            for decorator in decorators {
                for arg in &decorator.args {
                    collect_expr_identifier_spans(module, arg.value, interner, text, out);
                }
            }
            collect_stmt_identifier_spans(module, body, interner, text, out);
        }
        Item::Stmt(stmt_id) => {
            collect_stmt_identifier_spans(module, &[*stmt_id], interner, text, out)
        }
        Item::TestDecl { body, .. } => {
            collect_stmt_identifier_spans(module, body, interner, text, out)
        }
        Item::EnumDecl { .. } | Item::Use { .. } => {}
    }
}

fn collect_stmt_identifier_spans(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<(Span, String)>,
) {
    for &stmt_id in stmts {
        match module.arena.get_stmt(stmt_id) {
            fidan_ast::Stmt::VarDecl { ty, init, .. } => {
                if let Some(ty) = ty {
                    collect_type_expr_identifier_spans(ty, interner, out);
                }
                if let Some(init) = init {
                    collect_expr_identifier_spans(module, *init, interner, text, out);
                }
            }
            fidan_ast::Stmt::Destructure { value, .. } => {
                collect_expr_identifier_spans(module, *value, interner, text, out)
            }
            fidan_ast::Stmt::Assign { target, value, .. } => {
                collect_expr_identifier_spans(module, *target, interner, text, out);
                collect_expr_identifier_spans(module, *value, interner, text, out);
            }
            fidan_ast::Stmt::Expr { expr, .. } | fidan_ast::Stmt::Panic { value: expr, .. } => {
                collect_expr_identifier_spans(module, *expr, interner, text, out)
            }
            fidan_ast::Stmt::ActionDecl {
                params,
                return_ty,
                body,
                decorators,
                ..
            } => {
                for param in params {
                    collect_type_expr_identifier_spans(&param.ty, interner, out);
                    if let Some(default) = param.default {
                        collect_expr_identifier_spans(module, default, interner, text, out);
                    }
                }
                if let Some(return_ty) = return_ty {
                    collect_type_expr_identifier_spans(return_ty, interner, out);
                }
                for decorator in decorators {
                    for arg in &decorator.args {
                        collect_expr_identifier_spans(module, arg.value, interner, text, out);
                    }
                }
                collect_stmt_identifier_spans(module, body, interner, text, out);
            }
            fidan_ast::Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    collect_expr_identifier_spans(module, *value, interner, text, out);
                }
            }
            fidan_ast::Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_expr_identifier_spans(module, *condition, interner, text, out);
                collect_stmt_identifier_spans(module, then_body, interner, text, out);
                for else_if in else_ifs {
                    collect_expr_identifier_spans(module, else_if.condition, interner, text, out);
                    collect_stmt_identifier_spans(module, &else_if.body, interner, text, out);
                }
                if let Some(else_body) = else_body {
                    collect_stmt_identifier_spans(module, else_body, interner, text, out);
                }
            }
            fidan_ast::Stmt::Check {
                scrutinee, arms, ..
            } => {
                collect_expr_identifier_spans(module, *scrutinee, interner, text, out);
                for arm in arms {
                    collect_expr_identifier_spans(module, arm.pattern, interner, text, out);
                    collect_stmt_identifier_spans(module, &arm.body, interner, text, out);
                }
            }
            fidan_ast::Stmt::For { iterable, body, .. }
            | fidan_ast::Stmt::ParallelFor { iterable, body, .. } => {
                collect_expr_identifier_spans(module, *iterable, interner, text, out);
                collect_stmt_identifier_spans(module, body, interner, text, out);
            }
            fidan_ast::Stmt::While {
                condition, body, ..
            } => {
                collect_expr_identifier_spans(module, *condition, interner, text, out);
                collect_stmt_identifier_spans(module, body, interner, text, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_identifier_spans(module, body, interner, text, out);
                for catch in catches {
                    collect_stmt_identifier_spans(module, &catch.body, interner, text, out);
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_identifier_spans(module, otherwise, interner, text, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_identifier_spans(module, finally, interner, text, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_identifier_spans(module, &task.body, interner, text, out);
                }
            }
            fidan_ast::Stmt::Break { .. }
            | fidan_ast::Stmt::Continue { .. }
            | fidan_ast::Stmt::Error { .. } => {}
        }
    }
}

fn collect_type_expr_identifier_spans(
    ty: &TypeExpr,
    interner: &SymbolInterner,
    out: &mut Vec<(Span, String)>,
) {
    match ty {
        TypeExpr::Named { name, span } => {
            out.push((*span, interner.resolve(*name).to_string()));
        }
        TypeExpr::Tuple { elements, .. } => {
            for element in elements {
                collect_type_expr_identifier_spans(element, interner, out);
            }
        }
        TypeExpr::Oftype { base, param, .. } => {
            collect_type_expr_identifier_spans(base, interner, out);
            collect_type_expr_identifier_spans(param, interner, out);
        }
        TypeExpr::Dynamic { .. } | TypeExpr::Nothing { .. } => {}
    }
}

fn collect_expr_identifier_spans(
    module: &Module,
    expr_id: fidan_ast::ExprId,
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<(Span, String)>,
) {
    match module.arena.get_expr(expr_id) {
        Expr::Ident { name, span } => {
            out.push((*span, interner.resolve(*name).to_string()));
        }
        Expr::This { span } => out.push((*span, "this".to_string())),
        Expr::Parent { span } => out.push((*span, "parent".to_string())),
        Expr::Binary { lhs, rhs, .. } | Expr::NullCoalesce { lhs, rhs, .. } => {
            collect_expr_identifier_spans(module, *lhs, interner, text, out);
            collect_expr_identifier_spans(module, *rhs, interner, text, out);
        }
        Expr::Unary { operand, .. }
        | Expr::Spawn { expr: operand, .. }
        | Expr::Await { expr: operand, .. } => {
            collect_expr_identifier_spans(module, *operand, interner, text, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_identifier_spans(module, *callee, interner, text, out);
            for arg in args {
                collect_expr_identifier_spans(module, arg.value, interner, text, out);
            }
        }
        Expr::Field {
            object,
            field,
            span,
        } => {
            collect_expr_identifier_spans(module, *object, interner, text, out);
            let field_name = interner.resolve(*field).to_string();
            if let Some(field_span) = find_identifier_span_in_range(text, *span, &field_name) {
                out.push((field_span, field_name));
            }
        }
        Expr::Index { object, index, .. } => {
            collect_expr_identifier_spans(module, *object, interner, text, out);
            collect_expr_identifier_spans(module, *index, interner, text, out);
        }
        Expr::Assign { target, value, .. } | Expr::CompoundAssign { target, value, .. } => {
            collect_expr_identifier_spans(module, *target, interner, text, out);
            collect_expr_identifier_spans(module, *value, interner, text, out);
        }
        Expr::StringInterp { parts, .. } => {
            for part in parts {
                if let fidan_ast::InterpPart::Expr(expr_id) = part {
                    collect_expr_identifier_spans(module, *expr_id, interner, text, out);
                }
            }
        }
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            collect_expr_identifier_spans(module, *condition, interner, text, out);
            collect_expr_identifier_spans(module, *then_val, interner, text, out);
            collect_expr_identifier_spans(module, *else_val, interner, text, out);
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            for &element in elements {
                collect_expr_identifier_spans(module, element, interner, text, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for &(key, value) in entries {
                collect_expr_identifier_spans(module, key, interner, text, out);
                collect_expr_identifier_spans(module, value, interner, text, out);
            }
        }
        Expr::Check {
            scrutinee, arms, ..
        } => {
            collect_expr_identifier_spans(module, *scrutinee, interner, text, out);
            for arm in arms {
                collect_expr_identifier_spans(module, arm.pattern, interner, text, out);
                collect_stmt_identifier_spans(module, &arm.body, interner, text, out);
            }
        }
        Expr::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            collect_expr_identifier_spans(module, *target, interner, text, out);
            if let Some(start) = start {
                collect_expr_identifier_spans(module, *start, interner, text, out);
            }
            if let Some(end) = end {
                collect_expr_identifier_spans(module, *end, interner, text, out);
            }
            if let Some(step) = step {
                collect_expr_identifier_spans(module, *step, interner, text, out);
            }
        }
        Expr::ListComp {
            element,
            iterable,
            filter,
            ..
        } => {
            collect_expr_identifier_spans(module, *element, interner, text, out);
            collect_expr_identifier_spans(module, *iterable, interner, text, out);
            if let Some(filter) = filter {
                collect_expr_identifier_spans(module, *filter, interner, text, out);
            }
        }
        Expr::DictComp {
            key,
            value,
            iterable,
            filter,
            ..
        } => {
            collect_expr_identifier_spans(module, *key, interner, text, out);
            collect_expr_identifier_spans(module, *value, interner, text, out);
            collect_expr_identifier_spans(module, *iterable, interner, text, out);
            if let Some(filter) = filter {
                collect_expr_identifier_spans(module, *filter, interner, text, out);
            }
        }
        Expr::Lambda { params, body, .. } => {
            for param in params {
                if let Some(default) = param.default {
                    collect_expr_identifier_spans(module, default, interner, text, out);
                }
            }
            collect_stmt_identifier_spans(module, body, interner, text, out);
        }
        Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::StrLit { .. }
        | Expr::BoolLit { .. }
        | Expr::Nothing { .. }
        | Expr::Error { .. } => {}
    }
}

fn collect_member_access_sites(
    module: &Module,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    text: &str,
) -> Vec<MemberAccessSite> {
    let mut sites = Vec::new();

    for &item_id in &module.items {
        collect_item_member_access_sites(module, item_id, typed, interner, text, &mut sites);
    }

    sites
}

fn collect_item_member_access_sites(
    module: &Module,
    item_id: fidan_ast::ItemId,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<MemberAccessSite>,
) {
    match module.arena.get_item(item_id) {
        Item::VarDecl { init, .. } => {
            if let Some(init) = init {
                collect_expr_member_access_sites(module, *init, typed, interner, text, out);
            }
        }
        Item::ExprStmt(expr_id) => {
            collect_expr_member_access_sites(module, *expr_id, typed, interner, text, out)
        }
        Item::Assign { target, value, .. } => {
            collect_expr_member_access_sites(module, *target, typed, interner, text, out);
            collect_expr_member_access_sites(module, *value, typed, interner, text, out);
        }
        Item::Destructure { value, .. } => {
            collect_expr_member_access_sites(module, *value, typed, interner, text, out)
        }
        Item::ObjectDecl {
            fields, methods, ..
        } => {
            for field in fields {
                if let Some(default) = field.default {
                    collect_expr_member_access_sites(module, default, typed, interner, text, out);
                }
            }
            for &method_id in methods {
                collect_item_member_access_sites(module, method_id, typed, interner, text, out);
            }
        }
        Item::ActionDecl {
            params,
            body,
            decorators,
            ..
        }
        | Item::ExtensionAction {
            params,
            body,
            decorators,
            ..
        } => {
            for param in params {
                if let Some(default) = param.default {
                    collect_expr_member_access_sites(module, default, typed, interner, text, out);
                }
            }
            for decorator in decorators {
                for arg in &decorator.args {
                    collect_expr_member_access_sites(module, arg.value, typed, interner, text, out);
                }
            }
            collect_stmt_member_access_sites(module, body, typed, interner, text, out);
        }
        Item::Stmt(stmt_id) => {
            collect_stmt_member_access_sites(module, &[*stmt_id], typed, interner, text, out)
        }
        Item::TestDecl { body, .. } => {
            collect_stmt_member_access_sites(module, body, typed, interner, text, out)
        }
        Item::EnumDecl { .. } | Item::Use { .. } => {}
    }
}

fn collect_stmt_member_access_sites(
    module: &Module,
    stmts: &[fidan_ast::StmtId],
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<MemberAccessSite>,
) {
    for &stmt_id in stmts {
        match module.arena.get_stmt(stmt_id) {
            fidan_ast::Stmt::VarDecl { init, .. } => {
                if let Some(init) = init {
                    collect_expr_member_access_sites(module, *init, typed, interner, text, out);
                }
            }
            fidan_ast::Stmt::Destructure { value, .. } => {
                collect_expr_member_access_sites(module, *value, typed, interner, text, out)
            }
            fidan_ast::Stmt::Assign { target, value, .. } => {
                collect_expr_member_access_sites(module, *target, typed, interner, text, out);
                collect_expr_member_access_sites(module, *value, typed, interner, text, out);
            }
            fidan_ast::Stmt::Expr { expr, .. } | fidan_ast::Stmt::Panic { value: expr, .. } => {
                collect_expr_member_access_sites(module, *expr, typed, interner, text, out)
            }
            fidan_ast::Stmt::ActionDecl {
                params,
                body,
                decorators,
                ..
            } => {
                for param in params {
                    if let Some(default) = param.default {
                        collect_expr_member_access_sites(
                            module, default, typed, interner, text, out,
                        );
                    }
                }
                for decorator in decorators {
                    for arg in &decorator.args {
                        collect_expr_member_access_sites(
                            module, arg.value, typed, interner, text, out,
                        );
                    }
                }
                collect_stmt_member_access_sites(module, body, typed, interner, text, out);
            }
            fidan_ast::Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    collect_expr_member_access_sites(module, *value, typed, interner, text, out);
                }
            }
            fidan_ast::Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_expr_member_access_sites(module, *condition, typed, interner, text, out);
                collect_stmt_member_access_sites(module, then_body, typed, interner, text, out);
                for else_if in else_ifs {
                    collect_expr_member_access_sites(
                        module,
                        else_if.condition,
                        typed,
                        interner,
                        text,
                        out,
                    );
                    collect_stmt_member_access_sites(
                        module,
                        &else_if.body,
                        typed,
                        interner,
                        text,
                        out,
                    );
                }
                if let Some(else_body) = else_body {
                    collect_stmt_member_access_sites(module, else_body, typed, interner, text, out);
                }
            }
            fidan_ast::Stmt::Check {
                scrutinee, arms, ..
            } => {
                collect_expr_member_access_sites(module, *scrutinee, typed, interner, text, out);
                for arm in arms {
                    collect_expr_member_access_sites(
                        module,
                        arm.pattern,
                        typed,
                        interner,
                        text,
                        out,
                    );
                    collect_stmt_member_access_sites(module, &arm.body, typed, interner, text, out);
                }
            }
            fidan_ast::Stmt::For { iterable, body, .. }
            | fidan_ast::Stmt::ParallelFor { iterable, body, .. } => {
                collect_expr_member_access_sites(module, *iterable, typed, interner, text, out);
                collect_stmt_member_access_sites(module, body, typed, interner, text, out);
            }
            fidan_ast::Stmt::While {
                condition, body, ..
            } => {
                collect_expr_member_access_sites(module, *condition, typed, interner, text, out);
                collect_stmt_member_access_sites(module, body, typed, interner, text, out);
            }
            fidan_ast::Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                collect_stmt_member_access_sites(module, body, typed, interner, text, out);
                for catch in catches {
                    collect_stmt_member_access_sites(
                        module,
                        &catch.body,
                        typed,
                        interner,
                        text,
                        out,
                    );
                }
                if let Some(otherwise) = otherwise {
                    collect_stmt_member_access_sites(module, otherwise, typed, interner, text, out);
                }
                if let Some(finally) = finally {
                    collect_stmt_member_access_sites(module, finally, typed, interner, text, out);
                }
            }
            fidan_ast::Stmt::ConcurrentBlock { tasks, .. } => {
                for task in tasks {
                    collect_stmt_member_access_sites(
                        module, &task.body, typed, interner, text, out,
                    );
                }
            }
            fidan_ast::Stmt::Break { .. }
            | fidan_ast::Stmt::Continue { .. }
            | fidan_ast::Stmt::Error { .. } => {}
        }
    }
}

fn collect_expr_member_access_sites(
    module: &Module,
    expr_id: fidan_ast::ExprId,
    typed: &fidan_typeck::TypedModule,
    interner: &SymbolInterner,
    text: &str,
    out: &mut Vec<MemberAccessSite>,
) {
    match module.arena.get_expr(expr_id) {
        Expr::Binary { lhs, rhs, .. } | Expr::NullCoalesce { lhs, rhs, .. } => {
            collect_expr_member_access_sites(module, *lhs, typed, interner, text, out);
            collect_expr_member_access_sites(module, *rhs, typed, interner, text, out);
        }
        Expr::Unary { operand, .. }
        | Expr::Spawn { expr: operand, .. }
        | Expr::Await { expr: operand, .. } => {
            collect_expr_member_access_sites(module, *operand, typed, interner, text, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_member_access_sites(module, *callee, typed, interner, text, out);
            for arg in args {
                collect_expr_member_access_sites(module, arg.value, typed, interner, text, out);
            }
        }
        Expr::Field {
            object,
            field,
            span,
        } => {
            collect_expr_member_access_sites(module, *object, typed, interner, text, out);

            let field_name = interner.resolve(*field).to_string();
            let receiver_type = typed
                .expr_types
                .get(object)
                .and_then(|ty| symbols::resolved_type_name(ty, interner));
            let member_span = find_identifier_span_in_range(text, *span, &field_name);

            if let (Some(receiver_type), Some(member_span)) = (receiver_type, member_span) {
                out.push(MemberAccessSite {
                    member_span,
                    receiver_type,
                    member_name: field_name,
                });
            }
        }
        Expr::Index { object, index, .. } => {
            collect_expr_member_access_sites(module, *object, typed, interner, text, out);
            collect_expr_member_access_sites(module, *index, typed, interner, text, out);
        }
        Expr::Assign { target, value, .. } | Expr::CompoundAssign { target, value, .. } => {
            collect_expr_member_access_sites(module, *target, typed, interner, text, out);
            collect_expr_member_access_sites(module, *value, typed, interner, text, out);
        }
        Expr::StringInterp { parts, .. } => {
            for part in parts {
                if let fidan_ast::InterpPart::Expr(expr_id) = part {
                    collect_expr_member_access_sites(module, *expr_id, typed, interner, text, out);
                }
            }
        }
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            collect_expr_member_access_sites(module, *condition, typed, interner, text, out);
            collect_expr_member_access_sites(module, *then_val, typed, interner, text, out);
            collect_expr_member_access_sites(module, *else_val, typed, interner, text, out);
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            for &element in elements {
                collect_expr_member_access_sites(module, element, typed, interner, text, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for &(key, value) in entries {
                collect_expr_member_access_sites(module, key, typed, interner, text, out);
                collect_expr_member_access_sites(module, value, typed, interner, text, out);
            }
        }
        Expr::Check {
            scrutinee, arms, ..
        } => {
            collect_expr_member_access_sites(module, *scrutinee, typed, interner, text, out);
            for arm in arms {
                collect_expr_member_access_sites(module, arm.pattern, typed, interner, text, out);
            }
        }
        Expr::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            collect_expr_member_access_sites(module, *target, typed, interner, text, out);
            if let Some(start) = start {
                collect_expr_member_access_sites(module, *start, typed, interner, text, out);
            }
            if let Some(end) = end {
                collect_expr_member_access_sites(module, *end, typed, interner, text, out);
            }
            if let Some(step) = step {
                collect_expr_member_access_sites(module, *step, typed, interner, text, out);
            }
        }
        Expr::ListComp {
            element,
            iterable,
            filter,
            ..
        } => {
            collect_expr_member_access_sites(module, *element, typed, interner, text, out);
            collect_expr_member_access_sites(module, *iterable, typed, interner, text, out);
            if let Some(filter) = filter {
                collect_expr_member_access_sites(module, *filter, typed, interner, text, out);
            }
        }
        Expr::DictComp {
            key,
            value,
            iterable,
            filter,
            ..
        } => {
            collect_expr_member_access_sites(module, *key, typed, interner, text, out);
            collect_expr_member_access_sites(module, *value, typed, interner, text, out);
            collect_expr_member_access_sites(module, *iterable, typed, interner, text, out);
            if let Some(filter) = filter {
                collect_expr_member_access_sites(module, *filter, typed, interner, text, out);
            }
        }
        Expr::Lambda { params, body, .. } => {
            for param in params {
                if let Some(default) = param.default {
                    collect_expr_member_access_sites(module, default, typed, interner, text, out);
                }
            }
            collect_stmt_member_access_sites(module, body, typed, interner, text, out);
        }
        Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::StrLit { .. }
        | Expr::BoolLit { .. }
        | Expr::Nothing { .. }
        | Expr::Ident { .. }
        | Expr::This { .. }
        | Expr::Parent { .. }
        | Expr::Error { .. } => {}
    }
}

/// Extract the type string from a hover detail like `"```fidan\nvar x: integer\n```"`.
fn extract_type_from_detail(detail: &str) -> &str {
    // The detail for variables looks like:  `\`\`\`fidan\nvar x -> type\n\`\`\``
    // We want everything after the last `->` on the declaration line, trimmed.
    for line in detail.lines() {
        if let Some(colon_pos) = line.rfind("->") {
            let candidate = line[colon_pos + 2..].trim();
            if !candidate.is_empty() && !candidate.contains('`') {
                return candidate;
            }
        }
    }
    "?"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn workspace_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn assert_file_analyzes_without_errors(rel_path: &str) {
        let path = workspace_root().join(rel_path);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let uri = format!("file:///{}", path.display().to_string().replace('\\', "/"));
        let result = analyze(&src, &uri);
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|diag| diag.severity == Some(DiagnosticSeverity::ERROR))
            .collect();
        assert!(
            errors.is_empty(),
            "expected no analysis errors for {} but got: {errors:#?}",
            rel_path
        );
    }

    #[test]
    fn analyze_recent_feature_surface_without_errors() {
        let src = r#"use std.async
use std.collections as collections
use std.regex

enum Result {
    Ok(string)
    Err(integer, dynamic)
}

@extern("kernel32", symbol = "Beep")
action beep with (certain freq oftype integer, certain ms oftype integer) returns nothing

action work with (optional name oftype dynamic = r"{guest}") returns dynamic {
    return name
}

action main {
    var raw = r"\n {literal}"
    var grouped = collections.groupBy([1, 1, 2])
    var parts = collections.chunk([1, 2, 3, 4], 2)
    var windows = collections.window([1, 2, 3], 2)
    var rows = collections.enumerate([10, 20])
    concurrent {
        task reader {
            print(raw)
        }
        task writer {
            print(await async.gather([spawn work("Ada")]))
        }
    }
}
"#;

        let result = analyze(src, "file:///feature_surface.fdn");
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|diag| diag.severity == Some(DiagnosticSeverity::ERROR))
            .collect();
        assert!(
            errors.is_empty(),
            "expected no analysis errors, got: {errors:#?}"
        );
        assert!(
            result
                .stdlib_imports
                .iter()
                .any(|(alias, module)| alias == "async" && module == "async")
        );
        assert!(
            result
                .stdlib_imports
                .iter()
                .any(|(alias, module)| alias == "collections" && module == "collections")
        );
        assert!(
            result
                .stdlib_imports
                .iter()
                .any(|(alias, module)| alias == "regex" && module == "regex")
        );
        assert!(
            result
                .stdlib_var_call_sites
                .iter()
                .any(|site| site.var_name == "rows"
                    && site.module_name == "collections"
                    && site.member_name == "enumerate")
        );
    }

    #[test]
    fn analyze_records_stdlib_namespace_call_sites_for_lsp_type_patching() {
        let src = r#"use std.env

var argv = env.args()
"#;

        let result = analyze(src, "file:///stdlib_type_patch.fdn");
        assert!(
            result.stdlib_var_call_sites.iter().any(|site| {
                site.var_name == "argv" && site.module_name == "env" && site.member_name == "args"
            }),
            "expected env.args() to be recorded for stdlib type patching"
        );
    }

    #[test]
    fn interpolation_method_error_points_at_interpolation_site() {
        let src = r#"const var commands oftype list oftype string = ["help"]

action main {
    print("Available commands: {commands.joins(", ")}")
}
"#;

        let result = analyze(src, "file:///interp_error_site.fdn");
        let diag = result
            .diagnostics
            .iter()
            .find(|diag| diag.message.contains("has no method `joins`"))
            .expect("missing joins diagnostic");

        assert_eq!(diag.range.start.line, 3);
        assert!(diag.range.start.character >= 31);
    }

    #[test]
    fn tuple_index_assignment_surfaces_as_lsp_error() {
        let src = r#"var coords = (1, 2, 3)
coords[0] = 9
"#;

        let result = analyze(src, "file:///tuple_assign_diag.fdn");
        let diag = result
            .diagnostics
            .iter()
            .find(|diag| {
                diag.severity == Some(DiagnosticSeverity::ERROR)
                    && diag
                        .message
                        .contains("cannot assign through index into `(integer, integer, integer)`")
            })
            .expect("missing tuple indexed-assignment diagnostic");

        assert_eq!(diag.range.start.line, 1);
    }

    #[test]
    fn analyze_collects_grouped_stdlib_import_bindings() {
        let src = r#"use std.collections.{enumerate}
use std.json.{parse}
"#;

        let result = analyze(src, "file:///stdlib_grouped_imports.fdn");
        assert!(
            result
                .stdlib_direct_imports
                .iter()
                .any(|(binding, module, member)| binding == "enumerate"
                    && module == "collections"
                    && member == "enumerate")
        );
        assert!(
            result
                .stdlib_direct_imports
                .iter()
                .any(|(binding, module, member)| binding == "parse"
                    && module == "json"
                    && member == "parse")
        );
    }

    #[test]
    fn analyze_preserves_file_import_modes() {
        let src = r#"use "./utils.fdn"
use "./utils_lib.fdn" as lib
"#;

        let result = analyze(src, "file:///import_modes.fdn");
        assert_eq!(
            result.imports,
            vec![
                FileImport {
                    path: "./utils.fdn".to_string(),
                    alias: None,
                },
                FileImport {
                    path: "./utils_lib.fdn".to_string(),
                    alias: Some("lib".to_string()),
                },
            ]
        );
    }

    #[test]
    fn analyze_preserves_user_module_import_modes() {
        let src = r#"use utils_lib
use utils_flat.{sub_ints}
use nested.tools as tools
"#;

        let result = analyze(src, "file:///user_import_modes.fdn");
        assert_eq!(
            result.user_module_imports,
            vec![
                UserModuleImport {
                    path: vec!["utils_lib".to_string()],
                    alias: None,
                    grouped: false,
                },
                UserModuleImport {
                    path: vec!["utils_flat".to_string(), "sub_ints".to_string()],
                    alias: None,
                    grouped: true,
                },
                UserModuleImport {
                    path: vec!["nested".to_string(), "tools".to_string()],
                    alias: Some("tools".to_string()),
                    grouped: false,
                },
            ]
        );
    }

    #[test]
    fn analyze_collects_typed_member_access_sites_for_builtin_receivers() {
        let src = r#"var parts = "hello".split()"#;

        let result = analyze(src, "file:///builtin_member_sites.fdn");
        let site = result
            .member_access_sites
            .iter()
            .find(|site| site.receiver_type == "string" && site.member_name == "split")
            .expect("expected typed member-access site for string.split");

        assert_eq!(
            &src[site.member_span.start as usize..site.member_span.end as usize],
            "split"
        );
    }

    #[test]
    fn analyze_collects_precise_stdlib_call_sites_for_namespace_and_direct_imports() {
        let src = "use std.math\nuse std.math.{abs}\nvar left = math.max(1, 2.0)\nvar right = abs(-4.0)\n";
        let result = analyze(src, "file:///stdlib_calls.fdn");

        assert!(result.stdlib_call_sites.iter().any(|site| {
            site.module_name == "math"
                && site.member_name == "max"
                && site.arg_tys == ["integer", "float"]
        }));
        assert!(result.stdlib_call_sites.iter().any(|site| {
            site.module_name == "math" && site.member_name == "abs" && site.arg_tys == ["float"]
        }));
    }

    #[test]
    fn analyze_indexes_identifiers_inside_string_interpolation() {
        let src = "action main {\n    var name = \"Ada\"\n    print(\"Hello {name}\")\n}\n";

        let result = analyze(src, "file:///interp_identifier_sites.fdn");
        let interp_offset = src.find("{name}").expect("interpolation name") as u32 + 1;

        let span = result
            .identifier_spans
            .iter()
            .find(|(span, name)| *name == "name" && span.start == interp_offset)
            .map(|(span, _)| *span)
            .expect("expected interpolation identifier span");

        assert_eq!(&src[span.start as usize..span.end as usize], "name");
    }

    #[test]
    fn analyze_collects_member_access_sites_inside_string_interpolation() {
        let src = "action main {\n    var name = \"Ada\"\n    print(\"Hello {name.upper()}\")\n}\n";

        let result = analyze(src, "file:///interp_member_sites.fdn");
        let site = result
            .member_access_sites
            .iter()
            .find(|site| site.receiver_type == "string" && site.member_name == "upper")
            .expect("expected interpolation member-access site for string.upper");

        assert_eq!(
            &src[site.member_span.start as usize..site.member_span.end as usize],
            "upper"
        );
    }

    #[test]
    fn analyze_indexes_builtin_wrapper_tokens_for_hover() {
        let src = "var counter oftype Shared oftype integer = Shared(0)\nvar weak oftype WeakShared\nvar pending oftype Pending oftype string\n";
        let result = analyze(src, "file:///builtin_wrappers.fdn");
        assert!(
            result
                .identifier_spans
                .iter()
                .any(|(_, name)| name == "Shared")
        );
        assert!(
            result
                .identifier_spans
                .iter()
                .any(|(_, name)| name == "WeakShared")
        );
        assert!(
            result
                .identifier_spans
                .iter()
                .any(|(_, name)| name == "Pending")
        );
    }

    #[test]
    fn analyze_reports_certain_param_nothing_errors() {
        let src = r#"action approx_equal with (
    certain a oftype float,
    certain b oftype float,
    optional rel_tol oftype float = 0.0000001,
    optional abs_tol oftype float = 0.0001,
) returns boolean {
    return true
}

const var x = nothing
approx_equal(x, x)
"#;

        let result = analyze(src, "file:///certain_param_nothing.fdn");
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|diag| diag.severity == Some(DiagnosticSeverity::ERROR))
            .collect();
        assert!(
            errors.iter().any(|diag| diag.code.as_ref().is_some_and(
                |code| code == &tower_lsp::lsp_types::NumberOrString::String("E0302".into())
            ) && diag
                .message
                .contains("certain parameter `a` cannot receive `nothing`")),
            "expected LSP to surface certain-param `a` error, got {errors:#?}"
        );
        assert!(
            errors.iter().any(|diag| diag.code.as_ref().is_some_and(
                |code| code == &tower_lsp::lsp_types::NumberOrString::String("E0302".into())
            ) && diag
                .message
                .contains("certain parameter `b` cannot receive `nothing`")),
            "expected LSP to surface certain-param `b` error, got {errors:#?}"
        );
    }

    #[test]
    fn unreachable_warning_is_tagged_unnecessary_for_editor_dimming() {
        let file = SourceFile::new(FileId(0), "<test>", "return 1\nprint(2)\n");
        let diag = FidanDiag::warning(
            fidan_diagnostics::diag_code!("W1006"),
            "unreachable statement; this code can never execute",
            Span::new(FileId(0), 9, 17),
        );
        let lsp = fidan_to_lsp(&diag, &file);
        assert_eq!(
            lsp.tags,
            Some(vec![lsp::DiagnosticTag::UNNECESSARY]),
            "expected unreachable diagnostics to be dimmed"
        );
    }

    #[test]
    fn analyze_current_feature_examples_without_errors() {
        for rel_path in [
            "test/examples/check_val.fdn",
            "test/examples/async_demo.fdn",
            "test/examples/concurrency_showcase.fdn",
            "test/examples/comprehensive.fdn",
            "test/examples/enum_test.fdn",
            "test/examples/parallel_demo.fdn",
            "test/examples/release_mega_1_0.fdn",
            "test/examples/test.fdn",
            "test/examples/trace_demo.fdn",
            "test/examples/spawn_method_test.fdn",
            "test/syntax.fdn",
        ] {
            assert_file_analyzes_without_errors(rel_path);
        }
    }
}
