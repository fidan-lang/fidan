//! Document-level symbol table — maps identifier names to their declaration
//! location and a human-readable signature string.
//!
//! Consumed by hover, go-to-definition and completion handlers.

use fidan_ast::{AstArena, ExprId, Item, Module, Param, Stmt, StmtId, TypeExpr};
use fidan_config::{ReceiverBuiltinKind, ReceiverReturnKind, receiver_member_specs};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::Span;
use fidan_typeck::{ActionInfo, FidanType, ObjectInfo, TypedModule};
use rustc_hash::{FxHashMap, FxHashSet};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymKind {
    Action,
    Object,
    Enum,
    EnumVariant,
    Variable { is_const: bool },
    Field,
    Method,
}

#[derive(Debug, Clone)]
pub struct SymbolEntry {
    /// What kind of symbol this is (for completion icon / hover header).
    pub kind: SymKind,
    /// Byte-span of the declaration — used for go-to-definition.
    pub span: Span,
    /// Pre-rendered Markdown displayed as hover text.
    pub detail: String,
    /// For variable declarations: the resolved type name, used to follow
    /// member accesses like `rex.name` → look up `TRex.name`.
    pub ty_name: Option<String>,
    /// Used by the LSP to validate cross-module call argument types.
    pub param_types: Vec<String>,
    /// For Method/Action entries: whether each parameter is required (`!optional`).
    /// Used by the LSP to emit E0301 when a required arg is not provided.
    pub param_required: Vec<bool>,
    /// For Method/Action entries: the declared return type name (e.g. `"string"`).
    /// Used by the server to patch `var x: dynamic` → `var x: string`.
    pub return_type: Option<String>,
    /// Parameter names with their declaration spans, for named-argument go-to-definition.
    /// E.g. `foo(times = 10)` — clicking `times` jumps to the `times` param span.
    pub param_names: Vec<(String, Span)>,
    /// `true` when this entry was created from an action parameter.
    /// Used to hide parameters from cross-module completion lists.
    pub is_param: bool,
}

#[derive(Debug, Clone)]
pub struct LexicalScope {
    pub span: Span,
    pub entries: FxHashMap<String, SymbolEntry>,
}

/// Per-document symbol registry built after every analysis pass.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    pub entries: FxHashMap<String, SymbolEntry>,
    /// Scope-aware symbols for action/method/test bodies and nested blocks.
    /// These are searched from inner to outer scope before consulting the
    /// flat top-level table.
    pub lexical_scopes: Vec<LexicalScope>,
}

impl SymbolTable {
    /// Look up an unqualified name.
    pub fn get(&self, name: &str) -> Option<&SymbolEntry> {
        self.entries.get(name)
    }

    /// Iterate over all entries (unqualified and qualified names alike).
    pub fn all(&self) -> impl Iterator<Item = (&String, &SymbolEntry)> {
        self.entries.iter()
    }

    fn visible_scopes_at(&self, offset: u32) -> Vec<&LexicalScope> {
        let mut scopes: Vec<&LexicalScope> = self
            .lexical_scopes
            .iter()
            .filter(|scope| offset >= scope.span.start && offset < scope.span.end)
            .collect();
        scopes.sort_by_key(|scope| (scope.span.len(), std::cmp::Reverse(scope.span.start)));
        scopes
    }

    pub fn is_lexical_visible(&self, offset: u32, name: &str) -> bool {
        self.visible_scopes_at(offset).into_iter().any(|scope| {
            scope
                .entries
                .get(name)
                .map(|entry| entry.span.start <= offset)
                .unwrap_or(false)
        })
    }

    /// Look up an unqualified name as it is visible at the given cursor offset.
    pub fn lookup_visible(&self, offset: u32, name: &str) -> Option<&SymbolEntry> {
        for scope in self.visible_scopes_at(offset) {
            if let Some(entry) = scope.entries.get(name)
                && entry.span.start <= offset
            {
                return Some(entry);
            }
        }
        self.entries.get(name)
    }

    /// Collect completion-visible unqualified symbols at `offset`.
    /// Scoped symbols come first, with globals appended as fallback.
    pub fn visible_unqualified_at(&self, offset: u32) -> Vec<(String, SymbolEntry)> {
        let mut seen = FxHashSet::default();
        let mut result = Vec::new();

        for scope in self.visible_scopes_at(offset) {
            let mut scope_entries: Vec<(String, SymbolEntry)> = scope
                .entries
                .iter()
                .filter(|(name, entry)| !name.contains('.') && entry.span.start <= offset)
                .map(|(name, entry)| (name.clone(), entry.clone()))
                .collect();
            scope_entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.span.start));
            for (name, entry) in scope_entries {
                if seen.insert(name.clone()) {
                    result.push((name, entry));
                }
            }
        }

        let mut global_entries: Vec<(String, SymbolEntry)> = self
            .entries
            .iter()
            .filter(|(name, entry)| !name.contains('.') && !entry.is_param)
            .map(|(name, entry)| (name.clone(), entry.clone()))
            .collect();
        global_entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (name, entry) in global_entries {
            if seen.insert(name.clone()) {
                result.push((name, entry));
            }
        }

        result
    }

    fn put(&mut self, name: String, entry: SymbolEntry) {
        // First declaration wins — avoids overwriting with re-declarations.
        self.entries.entry(name).or_insert(entry);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn type_name(ty: &FidanType, interner: &SymbolInterner) -> String {
    ty.display_name(&|sym| interner.resolve(sym).to_string())
}

fn receiver_builtin_type_name(kind: ReceiverBuiltinKind) -> Option<&'static str> {
    match kind {
        ReceiverBuiltinKind::Integer => Some("integer"),
        ReceiverBuiltinKind::Float => Some("float"),
        ReceiverBuiltinKind::Boolean => Some("boolean"),
        ReceiverBuiltinKind::String => Some("string"),
        ReceiverBuiltinKind::List => Some("list"),
        ReceiverBuiltinKind::Dict => Some("dict"),
        ReceiverBuiltinKind::Shared => Some("Shared"),
        ReceiverBuiltinKind::WeakShared => Some("WeakShared"),
        ReceiverBuiltinKind::Pending => Some("Pending"),
        ReceiverBuiltinKind::Function => Some("action"),
        ReceiverBuiltinKind::Nothing => Some("nothing"),
        _ => None,
    }
}

fn builtin_receiver_kind(ty: &FidanType) -> Option<ReceiverBuiltinKind> {
    Some(match ty {
        FidanType::Integer => ReceiverBuiltinKind::Integer,
        FidanType::Float => ReceiverBuiltinKind::Float,
        FidanType::Boolean => ReceiverBuiltinKind::Boolean,
        FidanType::String => ReceiverBuiltinKind::String,
        FidanType::List(_) => ReceiverBuiltinKind::List,
        FidanType::Dict(_, _) => ReceiverBuiltinKind::Dict,
        FidanType::Nothing => ReceiverBuiltinKind::Nothing,
        FidanType::Shared(_) => ReceiverBuiltinKind::Shared,
        FidanType::WeakShared(_) => ReceiverBuiltinKind::WeakShared,
        FidanType::Pending(_) => ReceiverBuiltinKind::Pending,
        FidanType::Function => ReceiverBuiltinKind::Function,
        _ => return None,
    })
}

fn receiver_return_summary(
    return_kind: ReceiverReturnKind,
) -> (&'static str, Option<&'static str>) {
    match return_kind {
        ReceiverReturnKind::Integer => ("integer", Some("integer")),
        ReceiverReturnKind::Float => ("float", Some("float")),
        ReceiverReturnKind::Boolean => ("boolean", Some("boolean")),
        ReceiverReturnKind::String => ("string", Some("string")),
        ReceiverReturnKind::Dynamic => ("dynamic", None),
        ReceiverReturnKind::Nothing => ("nothing", Some("nothing")),
        ReceiverReturnKind::ReceiverElement => ("receiver element", None),
        ReceiverReturnKind::DictValue => ("dict value", None),
        ReceiverReturnKind::ListOfString => ("list oftype string", Some("list")),
        ReceiverReturnKind::ListOfInteger => ("list oftype integer", Some("list")),
        ReceiverReturnKind::ListOfDynamic => ("list oftype dynamic", Some("list")),
        ReceiverReturnKind::ListOfReceiverElement => ("list", Some("list")),
        ReceiverReturnKind::ListOfDictValue => ("list", Some("list")),
        ReceiverReturnKind::ListOfDynamicPairs => ("list", Some("list")),
        ReceiverReturnKind::SharedInnerValue => ("Shared inner value", None),
        ReceiverReturnKind::SharedOfInner => ("Shared", Some("Shared")),
        ReceiverReturnKind::WeakSharedOfInner => ("WeakShared", Some("WeakShared")),
    }
}

pub(crate) fn resolved_type_name(ty: &FidanType, interner: &SymbolInterner) -> Option<String> {
    match ty {
        FidanType::Object(sym) | FidanType::Enum(sym) | FidanType::ClassType(sym) => {
            Some(interner.resolve(*sym).to_string())
        }
        _ => builtin_receiver_kind(ty)
            .and_then(receiver_builtin_type_name)
            .map(str::to_string),
    }
}

fn fmt_type_expr(te: &TypeExpr, interner: &SymbolInterner) -> String {
    match te {
        TypeExpr::Named { name, .. } => interner.resolve(*name).to_string(),
        TypeExpr::Dynamic { .. } => "dynamic".into(),
        TypeExpr::Nothing { .. } => "nothing".into(),
        TypeExpr::Tuple { elements, .. } => {
            if elements.is_empty() {
                "tuple".into()
            } else {
                let inner: Vec<_> = elements
                    .iter()
                    .map(|e| fmt_type_expr(e, interner))
                    .collect();
                format!("({})", inner.join(", "))
            }
        }
        TypeExpr::Oftype { base, param, .. } => {
            format!(
                "{} oftype {}",
                fmt_type_expr(base, interner),
                fmt_type_expr(param, interner)
            )
        }
    }
}

fn format_enum_variant_summary(name: &str, payload_types: &[String]) -> String {
    if payload_types.is_empty() {
        name.to_string()
    } else {
        format!("{}({})", name, payload_types.join(", "))
    }
}

fn inferred_var_type(
    ty: Option<&TypeExpr>,
    init: Option<ExprId>,
    typed: &TypedModule,
    interner: &SymbolInterner,
) -> (Option<String>, String) {
    let resolved_name = ty
        .and_then(|annotated| {
            if let TypeExpr::Named { name, .. } = annotated {
                Some(interner.resolve(*name).to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            init.and_then(|expr_id| match typed.expr_types.get(&expr_id) {
                Some(found_ty) => resolved_type_name(found_ty, interner),
                None => None,
            })
        });

    let rendered = ty
        .map(|annotated| fmt_type_expr(annotated, interner))
        .or_else(|| resolved_name.clone())
        .or_else(|| {
            init.and_then(|expr_id| match typed.expr_types.get(&expr_id) {
                Some(found_ty)
                    if !matches!(
                        found_ty,
                        FidanType::Unknown | FidanType::Error | FidanType::Object(_)
                    ) =>
                {
                    Some(type_name(found_ty, interner))
                }
                _ => None,
            })
        })
        .unwrap_or_else(|| "?".into());

    (resolved_name, rendered)
}

fn make_var_entry(
    name: String,
    span: Span,
    ty_name: Option<String>,
    ty_rendered: String,
    is_const: bool,
) -> SymbolEntry {
    let kw = if is_const { "const var" } else { "var" };
    SymbolEntry {
        kind: SymKind::Variable { is_const },
        span,
        detail: format!("```fidan\n{} {} -> {}\n```", kw, name, ty_rendered),
        ty_name,
        param_types: vec![],
        param_required: vec![],
        return_type: None,
        param_names: vec![],
        is_param: false,
    }
}

fn make_loop_binding_entry(
    name: String,
    span: Span,
    iterable: ExprId,
    typed: &TypedModule,
    interner: &SymbolInterner,
) -> SymbolEntry {
    let elem_ty_s = typed
        .expr_types
        .get(&iterable)
        .map(|iter_ty| match iter_ty {
            FidanType::List(inner) => type_name(inner, interner),
            _ => type_name(iter_ty, interner),
        })
        .unwrap_or_else(|| "dynamic".to_string());
    SymbolEntry {
        kind: SymKind::Variable { is_const: false },
        span,
        detail: format!("```fidan\nfor {} -> {}\n```", name, elem_ty_s),
        ty_name: typed
            .expr_types
            .get(&iterable)
            .and_then(|iter_ty| match iter_ty {
                FidanType::List(inner) => resolved_type_name(inner, interner),
                other => resolved_type_name(other, interner),
            }),
        param_types: vec![],
        param_required: vec![],
        return_type: None,
        param_names: vec![],
        is_param: false,
    }
}

fn make_param_entry_from_typed(
    param: &Param,
    info: &fidan_typeck::ParamInfo,
    interner: &SymbolInterner,
) -> (String, SymbolEntry) {
    let name = interner.resolve(param.name).to_string();
    let ty_s = type_name(&info.ty, interner);
    let prefix = if info.certain {
        "certain "
    } else if info.optional {
        "optional "
    } else {
        ""
    };
    let ty_name = resolved_type_name(&info.ty, interner);
    (
        name.clone(),
        SymbolEntry {
            kind: SymKind::Variable { is_const: false },
            span: param.span,
            detail: format!("```fidan\n{}{} -> {}\n```", prefix, name, ty_s),
            ty_name,
            param_types: vec![],
            param_required: vec![],
            return_type: None,
            param_names: vec![],
            is_param: true,
        },
    )
}

fn make_param_entry_from_ast(param: &Param, interner: &SymbolInterner) -> (String, SymbolEntry) {
    let name = interner.resolve(param.name).to_string();
    let ty_s = fmt_type_expr(&param.ty, interner);
    let prefix = if param.certain {
        "certain "
    } else if param.optional {
        "optional "
    } else {
        ""
    };
    let ty_name = if ty_s == "action" {
        Some("action".to_string())
    } else {
        None
    };
    (
        name.clone(),
        SymbolEntry {
            kind: SymKind::Variable { is_const: false },
            span: param.span,
            detail: format!("```fidan\n{}{} -> {}\n```", prefix, name, ty_s),
            ty_name,
            param_types: vec![],
            param_required: vec![],
            return_type: None,
            param_names: vec![],
            is_param: true,
        },
    )
}

fn build_builtin_receiver_member_entries(table: &mut SymbolTable) {
    let receiver_kinds = [
        ReceiverBuiltinKind::Integer,
        ReceiverBuiltinKind::Float,
        ReceiverBuiltinKind::String,
        ReceiverBuiltinKind::List,
        ReceiverBuiltinKind::Dict,
        ReceiverBuiltinKind::Shared,
        ReceiverBuiltinKind::WeakShared,
        ReceiverBuiltinKind::Function,
    ];

    for receiver_kind in receiver_kinds {
        let Some(receiver_name) = receiver_builtin_type_name(receiver_kind) else {
            continue;
        };
        for spec in receiver_member_specs(receiver_kind) {
            let return_kind = spec.info.method_return.or(spec.info.field_return);
            let (return_label, ty_name) = return_kind
                .map(receiver_return_summary)
                .unwrap_or(("dynamic", None));
            let callable_suffix = if spec.info.method_return.is_some() {
                "()"
            } else {
                ""
            };
            let detail = format!(
                "```fidan\n{}.{}{} -> {}\n```",
                receiver_name, spec.info.canonical_name, callable_suffix, return_label
            );
            table.put(
                format!("{}.{}", receiver_name, spec.info.canonical_name),
                SymbolEntry {
                    kind: if spec.info.method_return.is_some() {
                        SymKind::Method
                    } else {
                        SymKind::Field
                    },
                    span: Span::default(),
                    detail,
                    ty_name: ty_name.map(str::to_string),
                    param_types: vec![],
                    param_required: vec![],
                    return_type: Some(return_label.to_string()),
                    param_names: vec![],
                    is_param: false,
                },
            );
        }
    }
}

fn make_local_action_entry(
    name: String,
    span: Span,
    params: &[Param],
    return_ty: &Option<TypeExpr>,
    interner: &SymbolInterner,
) -> SymbolEntry {
    let param_parts: Vec<String> = params
        .iter()
        .map(|param| {
            let ty = fmt_type_expr(&param.ty, interner);
            let pname = interner.resolve(param.name).to_string();
            if param.certain {
                format!("certain {} -> {}", pname, ty)
            } else if param.optional {
                format!("optional {} -> {}", pname, ty)
            } else {
                format!("{} -> {}", pname, ty)
            }
        })
        .collect();
    let rendered_ret = return_ty
        .as_ref()
        .map(|ty| fmt_type_expr(ty, interner))
        .unwrap_or_else(|| "dynamic".to_string());
    let params_suffix = if param_parts.is_empty() {
        String::new()
    } else {
        format!(" with ({})", param_parts.join(", "))
    };
    SymbolEntry {
        kind: SymKind::Action,
        span,
        detail: format!(
            "```fidan\naction {}{} -> {}\n```",
            name, params_suffix, rendered_ret
        ),
        ty_name: Some("action".to_string()),
        param_types: params
            .iter()
            .map(|param| fmt_type_expr(&param.ty, interner))
            .collect(),
        param_required: params.iter().map(|param| !param.optional).collect(),
        return_type: Some(rendered_ret),
        param_names: params
            .iter()
            .map(|param| (interner.resolve(param.name).to_string(), param.span))
            .collect(),
        is_param: false,
    }
}

fn make_enum_entry(name: String, span: Span, variants: &[String]) -> SymbolEntry {
    let variant_lines = if variants.is_empty() {
        String::new()
    } else {
        format!("\n  {}", variants.join("\n  "))
    };
    SymbolEntry {
        kind: SymKind::Enum,
        span,
        detail: format!("```fidan\nenum {} {{{}\n}}\n```", name, variant_lines),
        ty_name: None,
        param_types: vec![],
        param_required: vec![],
        return_type: None,
        param_names: vec![],
        is_param: false,
    }
}

fn make_enum_variant_entry(
    enum_name: &str,
    variant_name: &str,
    span: Span,
    payload_types: &[String],
) -> SymbolEntry {
    let signature = if payload_types.is_empty() {
        format!("{}.{}", enum_name, variant_name)
    } else {
        format!(
            "{}.{}({})",
            enum_name,
            variant_name,
            payload_types.join(", ")
        )
    };
    SymbolEntry {
        kind: SymKind::EnumVariant,
        span,
        detail: format!("```fidan\n{}\n```", signature),
        ty_name: None,
        param_types: payload_types.to_vec(),
        param_required: vec![true; payload_types.len()],
        return_type: Some(enum_name.to_string()),
        param_names: vec![],
        is_param: false,
    }
}

fn collect_scope_entries(
    scope_span: Span,
    initial_entries: FxHashMap<String, SymbolEntry>,
    stmts: &[StmtId],
    arena: &AstArena,
    typed: &TypedModule,
    interner: &SymbolInterner,
    scopes: &mut Vec<LexicalScope>,
) {
    let mut entries = initial_entries;
    for &sid in stmts {
        collect_stmt_entries(
            &mut entries,
            arena.get_stmt(sid),
            arena,
            typed,
            interner,
            scopes,
        );
    }
    if !entries.is_empty() {
        scopes.push(LexicalScope {
            span: scope_span,
            entries,
        });
    }
}

fn stmt_span(stmt: &Stmt) -> Span {
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

fn block_scope_span(stmts: &[StmtId], arena: &AstArena, fallback: Span) -> Span {
    let Some((&first, rest)) = stmts.split_first() else {
        return fallback;
    };
    let first_span = stmt_span(arena.get_stmt(first));
    let last_span = rest
        .last()
        .map(|sid| stmt_span(arena.get_stmt(*sid)))
        .unwrap_or(first_span);
    Span {
        file: fallback.file,
        start: first_span.start,
        end: last_span.end,
    }
}

fn collect_block_scope_entries(
    initial_entries: FxHashMap<String, SymbolEntry>,
    stmts: &[StmtId],
    fallback_span: Span,
    arena: &AstArena,
    typed: &TypedModule,
    interner: &SymbolInterner,
    scopes: &mut Vec<LexicalScope>,
) {
    collect_scope_entries(
        block_scope_span(stmts, arena, fallback_span),
        initial_entries,
        stmts,
        arena,
        typed,
        interner,
        scopes,
    );
}

fn collect_stmt_entries(
    current_scope: &mut FxHashMap<String, SymbolEntry>,
    stmt: &Stmt,
    arena: &AstArena,
    typed: &TypedModule,
    interner: &SymbolInterner,
    scopes: &mut Vec<LexicalScope>,
) {
    let resolve = |sym: Symbol| interner.resolve(sym).to_string();
    match stmt {
        Stmt::VarDecl {
            name,
            ty,
            init,
            is_const,
            span,
        } => {
            let name = resolve(*name);
            let (ty_name, rendered) = inferred_var_type(ty.as_ref(), *init, typed, interner);
            current_scope
                .entry(name.clone())
                .or_insert_with(|| make_var_entry(name, *span, ty_name, rendered, *is_const));
        }
        Stmt::Destructure {
            bindings,
            value,
            span,
        } => {
            let tuple_members: Vec<FidanType> = match typed.expr_types.get(value) {
                Some(FidanType::Tuple(types)) => types.clone(),
                _ => vec![],
            };
            for (index, binding) in bindings.iter().enumerate() {
                let name = resolve(*binding);
                let ty = tuple_members
                    .get(index)
                    .map(|ty| type_name(ty, interner))
                    .unwrap_or_else(|| "dynamic".to_string());
                current_scope
                    .entry(name.clone())
                    .or_insert_with(|| make_var_entry(name, *span, None, ty, false));
            }
        }
        Stmt::ActionDecl {
            name,
            params,
            return_ty,
            body,
            span,
            ..
        } => {
            let action_name = resolve(*name);
            current_scope.entry(action_name.clone()).or_insert_with(|| {
                make_local_action_entry(action_name, *span, params, return_ty, interner)
            });
            let nested_initial = params
                .iter()
                .map(|param| make_param_entry_from_ast(param, interner))
                .collect();
            collect_scope_entries(*span, nested_initial, body, arena, typed, interner, scopes);
        }
        Stmt::If {
            then_body,
            else_ifs,
            else_body,
            span,
            ..
        } => {
            collect_block_scope_entries(
                FxHashMap::default(),
                then_body,
                *span,
                arena,
                typed,
                interner,
                scopes,
            );
            for else_if in else_ifs {
                collect_block_scope_entries(
                    FxHashMap::default(),
                    &else_if.body,
                    else_if.span,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
            if let Some(else_body) = else_body {
                collect_block_scope_entries(
                    FxHashMap::default(),
                    else_body,
                    *span,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
        }
        Stmt::For {
            binding,
            iterable,
            body,
            span,
        }
        | Stmt::ParallelFor {
            binding,
            iterable,
            body,
            span,
        } => {
            let name = resolve(*binding);
            let mut nested_initial = FxHashMap::default();
            nested_initial.insert(
                name.clone(),
                make_loop_binding_entry(name, *span, *iterable, typed, interner),
            );
            collect_scope_entries(*span, nested_initial, body, arena, typed, interner, scopes);
        }
        Stmt::While { body, span, .. } => {
            collect_block_scope_entries(
                FxHashMap::default(),
                body,
                *span,
                arena,
                typed,
                interner,
                scopes,
            );
        }
        Stmt::Attempt {
            body,
            catches,
            otherwise,
            finally,
            span,
        } => {
            collect_block_scope_entries(
                FxHashMap::default(),
                body,
                *span,
                arena,
                typed,
                interner,
                scopes,
            );
            for catch in catches {
                let mut nested_initial = FxHashMap::default();
                if let Some(binding) = catch.binding {
                    let name = resolve(binding);
                    let rendered = catch
                        .ty
                        .as_ref()
                        .map(|ty| fmt_type_expr(ty, interner))
                        .unwrap_or_else(|| "dynamic".to_string());
                    nested_initial.insert(
                        name.clone(),
                        make_var_entry(name, catch.span, None, rendered, false),
                    );
                }
                collect_scope_entries(
                    catch.span,
                    nested_initial,
                    &catch.body,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
            if let Some(otherwise) = otherwise {
                collect_block_scope_entries(
                    FxHashMap::default(),
                    otherwise,
                    *span,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
            if let Some(finally) = finally {
                collect_block_scope_entries(
                    FxHashMap::default(),
                    finally,
                    *span,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
        }
        Stmt::Check { arms, .. } => {
            for arm in arms {
                collect_block_scope_entries(
                    FxHashMap::default(),
                    &arm.body,
                    arm.span,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
        }
        Stmt::ConcurrentBlock { tasks, .. } => {
            for task in tasks {
                collect_scope_entries(
                    task.span,
                    FxHashMap::default(),
                    &task.body,
                    arena,
                    typed,
                    interner,
                    scopes,
                );
            }
        }
        _ => {}
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Walk `module` and `typed` to produce a per-document [`SymbolTable`].
pub fn build(module: &Module, typed: &TypedModule, interner: &SymbolInterner) -> SymbolTable {
    let mut table = SymbolTable::default();
    let res = |sym: Symbol| interner.resolve(sym).to_string();

    // ── Actions ───────────────────────────────────────────────────────────────
    for (&sym, info) in &typed.actions {
        let name = res(sym);
        let detail = fmt_action_sig(&name, info, interner);
        let param_types: Vec<String> = info
            .params
            .iter()
            .map(|p| type_name(&p.ty, interner))
            .collect();
        table.put(
            name,
            SymbolEntry {
                kind: SymKind::Action,
                span: info.span,
                detail,
                ty_name: Some("action".to_string()),
                param_types,
                param_required: info.params.iter().map(|p| !p.optional).collect(),
                return_type: Some(type_name(&info.return_ty, interner)),
                param_names: vec![],
                is_param: false,
            },
        );
    }

    // ── Objects and their members ─────────────────────────────────────────────
    for (&sym, info) in &typed.objects {
        let name = res(sym);
        let detail = fmt_object(&name, info, interner);
        table.put(
            name.clone(),
            SymbolEntry {
                kind: SymKind::Object,
                span: info.span,
                detail,
                // Store the parent type name so the LSP can follow the
                // inheritance chain across documents (e.g. TRex → Dinosaur).
                ty_name: info.parent.map(&res),
                param_types: vec![],
                param_required: vec![],
                return_type: None,
                param_names: vec![],
                is_param: false,
            },
        );

        // Methods — stored under "ClassName.method_name" for potential
        // property-hover lookup.
        for (&msym, minfo) in &info.methods {
            let mname = res(msym);
            let sig = fmt_action_sig(&mname, minfo, interner);
            let param_types: Vec<String> = minfo
                .params
                .iter()
                .map(|p| type_name(&p.ty, interner))
                .collect();
            table.put(
                format!("{}.{}", name, mname),
                SymbolEntry {
                    kind: SymKind::Method,
                    span: minfo.span,
                    detail: sig,
                    ty_name: None,
                    param_types,
                    param_required: minfo.params.iter().map(|p| !p.optional).collect(),
                    return_type: Some(type_name(&minfo.return_ty, interner)),
                    param_names: vec![],
                    is_param: false,
                },
            );
        }

        // Fields — stored under "ClassName.field_name".
        // Use `info.span` as a temporary placeholder; corrected in the
        // AST pass below where the individual FieldDecl spans are available.
        for (&fsym, fty) in &info.fields {
            let fname = res(fsym);
            let ty_s = type_name(fty, interner);
            table.put(
                format!("{}.{}", name, fname),
                SymbolEntry {
                    kind: SymKind::Field,
                    span: info.span,
                    detail: format!("```fidan\n{}.{}: {}\n```", name, fname, ty_s),
                    ty_name: resolved_type_name(fty, interner),
                    param_types: vec![],
                    param_required: vec![],
                    return_type: None,
                    param_names: vec![],
                    is_param: false,
                },
            );
        }
    }

    // ── Enums and their variants ──────────────────────────────────────────────
    for (&sym, info) in &typed.enums {
        let name = res(sym);
        let variant_labels: Vec<String> = info
            .variants
            .iter()
            .map(|(variant_sym, payload_arity)| {
                let variant_name = res(*variant_sym);
                if *payload_arity == 0 {
                    variant_name
                } else {
                    format!(
                        "{}({})",
                        variant_name,
                        ", ".repeat(payload_arity.saturating_sub(1))
                            + if *payload_arity > 0 { "?" } else { "" }
                    )
                }
            })
            .collect();
        table.put(
            name.clone(),
            make_enum_entry(name.clone(), info.span, &variant_labels),
        );
        for (variant_sym, _) in &info.variants {
            let variant_name = res(*variant_sym);
            table.put(
                format!("{}.{}", name, variant_name),
                make_enum_variant_entry(&name, &variant_name, info.span, &[]),
            );
        }
    }

    // ── Top-level variable / const declarations ───────────────────────────────
    for &iid in &module.items {
        let item = module.arena.get_item(iid);
        match item {
            Item::VarDecl {
                name,
                ty,
                init,
                is_const,
                span,
            } => {
                let vname = res(*name);
                let (ty_name, ty_s) = inferred_var_type(ty.as_ref(), *init, typed, interner);
                table.put(
                    vname.clone(),
                    make_var_entry(vname, *span, ty_name, ty_s, *is_const),
                );
            }
            // Fix-up field declaration spans from the AST — ObjectInfo only stores
            // FidanType per field, not the source span, so the typed pass above used
            // the whole-object span.  Here we overwrite with the real FieldDecl span.
            Item::ObjectDecl {
                name,
                fields,
                methods,
                ..
            } => {
                let class_name = res(*name);
                for field in fields {
                    let fname = res(field.name);
                    let key = format!("{}.{}", class_name, fname);
                    if let Some(entry) = table.entries.get_mut(&key) {
                        entry.span = field.span;
                    }
                }
                // Patch method parameter spans from AST so named-arg goto-def works.
                // Also build scope-aware symbol maps for method completions.
                for &mid in methods {
                    if let Item::ActionDecl {
                        name: mname,
                        params,
                        body,
                        span: method_span,
                        ..
                    } = module.arena.get_item(mid)
                    {
                        let key = format!("{}.{}", class_name, res(*mname));
                        if let Some(entry) = table.entries.get_mut(&key) {
                            entry.param_names =
                                params.iter().map(|p| (res(p.name), p.span)).collect();
                        }
                        let method_info = typed
                            .objects
                            .get(name)
                            .and_then(|obj| obj.methods.get(mname));
                        if let Some(minfo) = method_info {
                            let initial_entries = params
                                .iter()
                                .zip(minfo.params.iter())
                                .map(|(ast_param, typed_param)| {
                                    make_param_entry_from_typed(ast_param, typed_param, interner)
                                })
                                .collect();
                            collect_scope_entries(
                                *method_span,
                                initial_entries,
                                body,
                                &module.arena,
                                typed,
                                interner,
                                &mut table.lexical_scopes,
                            );
                        }
                    }
                }
            }
            Item::ActionDecl {
                name,
                params,
                body,
                span,
                ..
            } => {
                let aname = res(*name);
                if let Some(entry) = table.entries.get_mut(&aname) {
                    entry.param_names = params.iter().map(|p| (res(p.name), p.span)).collect();
                }
                if let Some(info) = typed.actions.get(name) {
                    let initial_entries = params
                        .iter()
                        .zip(info.params.iter())
                        .map(|(ast_param, typed_param)| {
                            make_param_entry_from_typed(ast_param, typed_param, interner)
                        })
                        .collect();
                    collect_scope_entries(
                        *span,
                        initial_entries,
                        body,
                        &module.arena,
                        typed,
                        interner,
                        &mut table.lexical_scopes,
                    );
                }
            }
            Item::EnumDecl {
                name,
                variants,
                span,
            } => {
                let enum_name = res(*name);
                let variant_details: Vec<String> = variants
                    .iter()
                    .map(|variant| {
                        let variant_name = res(variant.name);
                        let payload_types: Vec<String> = variant
                            .payload_types
                            .iter()
                            .map(|payload| fmt_type_expr(payload, interner))
                            .collect();
                        let key = format!("{}.{}", enum_name, variant_name);
                        table.entries.insert(
                            key,
                            make_enum_variant_entry(
                                &enum_name,
                                &variant_name,
                                variant.span,
                                &payload_types,
                            ),
                        );
                        format_enum_variant_summary(&variant_name, &payload_types)
                    })
                    .collect();

                table.entries.insert(
                    enum_name.clone(),
                    make_enum_entry(enum_name, *span, &variant_details),
                );
            }
            Item::ExtensionAction {
                name,
                extends,
                params,
                ..
            } => {
                let key = format!("{}.{}", res(*extends), res(*name));
                if let Some(entry) = table.entries.get_mut(&key) {
                    entry.param_names = params.iter().map(|p| (res(p.name), p.span)).collect();
                }
            }
            Item::TestDecl { body, span, .. } => {
                collect_scope_entries(
                    *span,
                    FxHashMap::default(),
                    body,
                    &module.arena,
                    typed,
                    interner,
                    &mut table.lexical_scopes,
                );
            }
            _ => {}
        }
    }

    // ── Inherited members ─────────────────────────────────────────────────────
    // For each child object, walk the parent chain and add entries for fields
    // and methods it inherits (e.g. `"TRex.name"` inherited from `"Dinosaur"`).
    // We collect (child, parent) pairs first so the main `typed.objects` map
    // can still be borrowed immutably inside the loop.
    // ── Built-in `action` virtual members ─────────────────────────────────────
    // `action.name` is a read-only string property available on every value of
    // type `action`.  Adding a virtual entry lets the hover handler resolve
    // `fn.name` for any parameter typed as `action`.
    table.put(
        "action.name".to_string(),
        SymbolEntry {
            kind: SymKind::Field,
            span: Span::default(),
            detail: "```fidan\naction.name -> string\n```\n\nThe name of the action as declared in source.".to_string(),
            ty_name: None,
            param_types: vec![],
            param_required: vec![],
            return_type: Some("string".to_string()),
            param_names: vec![],
            is_param: false,
        },
    );

    build_builtin_receiver_member_entries(&mut table);

    let child_parent_pairs: Vec<(Symbol, Option<Symbol>)> = typed
        .objects
        .iter()
        .map(|(&s, info)| (s, info.parent))
        .collect();

    for (child_sym, _) in &child_parent_pairs {
        let child_name = res(*child_sym);
        let child_info = &typed.objects[child_sym];
        let mut cur = child_info.parent;
        while let Some(parent_sym) = cur {
            let parent_info = match typed.objects.get(&parent_sym) {
                Some(i) => i,
                None => break,
            };
            let parent_name = res(parent_sym);
            // Inherited fields — reuse the already-corrected field span.
            for (&fsym, fty) in &parent_info.fields {
                let fname = res(fsym);
                let ty_s = type_name(fty, interner);
                let key = format!("{}.{}", child_name, fname);
                let span = table
                    .entries
                    .get(&format!("{}.{}", parent_name, fname))
                    .map(|e| e.span)
                    .unwrap_or(parent_info.span);
                table.put(
                    key,
                    SymbolEntry {
                        kind: SymKind::Field,
                        span,
                        detail: format!(
                            "```fidan\n(inherited from {}) {}: {}\n```",
                            parent_name, fname, ty_s
                        ),
                        ty_name: resolved_type_name(fty, interner),
                        param_types: vec![],
                        param_required: vec![],
                        return_type: None,
                        param_names: vec![],
                        is_param: false,
                    },
                );
            }
            // Inherited methods (skip constructors).
            for (&msym, minfo) in &parent_info.methods {
                let mname = res(msym);
                if mname == "new" {
                    continue;
                }
                let sig = fmt_action_sig(&mname, minfo, interner);
                let param_types: Vec<String> = minfo
                    .params
                    .iter()
                    .map(|p| type_name(&p.ty, interner))
                    .collect();
                table.put(
                    format!("{}.{}", child_name, mname),
                    SymbolEntry {
                        kind: SymKind::Method,
                        span: minfo.span,
                        detail: sig,
                        ty_name: None,
                        param_types,
                        param_required: minfo.params.iter().map(|p| !p.optional).collect(),
                        return_type: Some(type_name(&minfo.return_ty, interner)),
                        param_names: vec![],
                        is_param: false,
                    },
                );
            }
            cur = parent_info.parent;
        }
    }

    table
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn fmt_action_sig(name: &str, info: &ActionInfo, interner: &SymbolInterner) -> String {
    let res = |sym: Symbol| interner.resolve(sym).to_string();
    let params: Vec<String> = info
        .params
        .iter()
        .map(|p| {
            let pname = res(p.name);
            let ty = type_name(&p.ty, interner);
            if p.certain {
                format!("certain {} -> {}", pname, ty)
            } else if p.optional {
                format!("optional {} -> {}", pname, ty)
            } else {
                format!("{} -> {}", pname, ty)
            }
        })
        .collect();
    let ret = type_name(&info.return_ty, interner);
    let params_str = if params.is_empty() {
        String::new()
    } else {
        format!(" with ({})", params.join(", "))
    };
    format!("```fidan\naction {}{} -> {}\n```", name, params_str, ret)
}

fn fmt_object(name: &str, info: &ObjectInfo, interner: &SymbolInterner) -> String {
    let res = |sym: Symbol| interner.resolve(sym).to_string();
    let header = match info.parent {
        Some(p) => format!("object {} extends {}", name, res(p)),
        None => format!("object {}", name),
    };
    let mut lines = vec![header];
    for (&fsym, fty) in &info.fields {
        lines.push(format!("  {}: {}", res(fsym), type_name(fty, interner)));
    }
    format!("```fidan\n{}\n```", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::Lexer;
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    fn build_symbols(src: &str) -> SymbolTable {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<symbols>", src);
        let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        assert!(lex_diags.is_empty(), "lexer diagnostics: {lex_diags:#?}");
        let (module, parse_diags) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        assert!(
            parse_diags.is_empty(),
            "parser diagnostics: {parse_diags:#?}"
        );
        let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        assert!(
            typed
                .diagnostics
                .iter()
                .all(|diag| diag.severity != fidan_diagnostics::Severity::Error),
            "type diagnostics: {:#?}",
            typed.diagnostics
        );
        build(&module, &typed, &interner)
    }

    #[test]
    fn hover_details_use_current_type_spellings() {
        let table = build_symbols(
            r#"action work with (optional name oftype dynamic = r"{guest}") returns dynamic {
    return name
}

var result = work()
"#,
        );

        let action = table.get("work").expect("action symbol");
        assert!(action.detail.contains("optional name -> dynamic"));
        assert!(action.detail.contains("action work"));

        let var = table.get("result").expect("var symbol");
        assert!(var.detail.contains("var result -> dynamic"));
        assert!(!var.detail.contains("flexible"));
    }

    #[test]
    fn scoped_lookup_finds_locals_without_polluting_top_level_symbols() {
        let src = r#"var top_level = 1

action outer {
    var local_count = 2
    action inner with (certain amount oftype integer) returns integer {
        return amount + local_count
    }
    print(local_count)
}
"#;
        let cursor = src.find("print(local_count)").expect("cursor marker") as u32;
        let table = build_symbols(src);

        assert!(table.get("local_count").is_none());
        assert!(table.lookup_visible(cursor, "local_count").is_some());
        assert!(table.lookup_visible(cursor, "inner").is_some());

        let visible: Vec<String> = table
            .visible_unqualified_at(cursor)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(visible.contains(&"local_count".to_string()));
        assert!(visible.contains(&"inner".to_string()));
        assert!(visible.contains(&"top_level".to_string()));
    }

    #[test]
    fn scoped_lookup_excludes_locals_before_declaration() {
        let src = r#"action outer {
    print("before")
    var local_count = 2
}
"#;
        let cursor = src.find("print").expect("cursor marker") as u32;
        let table = build_symbols(src);

        assert!(table.lookup_visible(cursor, "local_count").is_none());
        let visible: Vec<String> = table
            .visible_unqualified_at(cursor)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(!visible.contains(&"local_count".to_string()));
    }

    #[test]
    fn if_branch_scopes_do_not_leak_sibling_locals() {
        let src = r#"action outer with (certain flag oftype boolean) {
    if flag {
        var then_only = 1
        print(then_only)
    } otherwise {
        var else_only = 2
        print(else_only)
    }
}
"#;
        let table = build_symbols(src);
        let then_cursor = src.find("print(then_only)").expect("then cursor") as u32;
        let else_cursor = src.find("print(else_only)").expect("else cursor") as u32;

        assert!(table.lookup_visible(then_cursor, "then_only").is_some());
        assert!(table.lookup_visible(then_cursor, "else_only").is_none());
        assert!(table.lookup_visible(else_cursor, "else_only").is_some());
        assert!(table.lookup_visible(else_cursor, "then_only").is_none());
    }

    #[test]
    fn loop_bindings_are_visible_from_header_and_body() {
        let src = r#"use std.collections.{enumerate}

action outer {
    for task_info in enumerate(["a", "b"]) {
        print(task_info[0])
        print(task_info[1])
    }
}
"#;
        let table = build_symbols(src);
        let header_cursor = src.find("task_info in").expect("header cursor") as u32;
        let body_cursor = src.rfind("task_info[1]").expect("body cursor") as u32;

        let header_entry = table
            .lookup_visible(header_cursor, "task_info")
            .expect("loop binding should resolve in header");
        assert!(
            header_entry
                .detail
                .contains("for task_info -> (integer, string)")
        );

        let body_entry = table
            .lookup_visible(body_cursor, "task_info")
            .expect("loop binding should resolve in body");
        assert!(
            body_entry
                .detail
                .contains("for task_info -> (integer, string)")
        );
    }

    #[test]
    fn object_method_symbols_cover_enum_and_concurrency_adjacent_surface() {
        let table = build_symbols(
            r#"enum Result {
    Ok(string)
    Err(integer, dynamic)
}

action helper returns dynamic {
    return "ok"
}

object Worker {
    var name oftype string

    action run returns dynamic {
        var pending = spawn helper()
        return await pending
    }
}
"#,
        );

        let enum_symbol = table.get("Result").expect("enum symbol");
        assert!(enum_symbol.detail.contains("enum Result"));
        let ok_variant = table.get("Result.Ok").expect("enum variant symbol");
        assert!(ok_variant.detail.contains("Result.Ok(string)"));

        let object = table.get("Worker").expect("object symbol");
        assert!(object.detail.contains("object Worker"));
        let method = table.get("Worker.run").expect("method symbol");
        assert!(method.detail.contains("action run"));
        assert!(method.detail.contains("-> dynamic"));
    }
}
