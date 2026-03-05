//! Document-level symbol table — maps identifier names to their declaration
//! location and a human-readable signature string.
//!
//! Consumed by hover, go-to-definition and completion handlers.

use fidan_ast::{Item, Module, TypeExpr};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_source::Span;
use fidan_typeck::{ActionInfo, FidanType, ObjectInfo, TypedModule};
use rustc_hash::FxHashMap;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymKind {
    Action,
    Object,
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
    /// For Method/Action entries: parameter types in declaration order.
    /// Used by the LSP to validate cross-module call argument types.
    pub param_types: Vec<String>,
    /// For Method/Action entries: whether each parameter is required (`!optional`).
    /// Used by the LSP to emit E0301 when a required arg is not provided.
    pub param_required: Vec<bool>,
    /// For Method/Action entries: the declared return type name (e.g. `"string"`).
    /// Used by the server to patch `var x: dynamic` → `var x: string`.
    pub return_type: Option<String>,
}

/// Per-document symbol registry built after every analysis pass.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    pub entries: FxHashMap<String, SymbolEntry>,
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

    fn put(&mut self, name: String, entry: SymbolEntry) {
        // First declaration wins — avoids overwriting with re-declarations.
        self.entries.entry(name).or_insert(entry);
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
                ty_name: None,
                param_types,
                param_required: info.params.iter().map(|p| !p.optional).collect(),
                return_type: Some(type_name(&info.return_ty, interner)),
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
                ty_name: info.parent.map(|p| res(p)),
                param_types: vec![],
                param_required: vec![],
                return_type: None,
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
                    ty_name: None,
                    param_types: vec![],
                    param_required: vec![],
                    return_type: None,
                },
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
                let kw = if *is_const { "const var" } else { "var" };
                // Infer the concrete type name so member accesses like
                // `rex.name` can be resolved via `TRex.name` in any doc.
                let ty_name: Option<String> = if let Some(t) = ty.as_ref() {
                    if let TypeExpr::Named { name: tname, .. } = t {
                        Some(res(*tname))
                    } else {
                        None
                    }
                } else if let Some(init_eid) = *init {
                    if let Some(FidanType::Object(sym)) = typed.expr_types.get(&init_eid) {
                        Some(res(*sym))
                    } else {
                        None
                    }
                } else {
                    None
                };
                // Use the inferred type name in the hover detail when there is
                // no explicit annotation (avoids showing `?` for `rex = TRex(...)`).
                let ty_s = ty
                    .as_ref()
                    .map(|t| fmt_type_expr(t, interner))
                    .or_else(|| ty_name.clone())
                    .or_else(|| {
                        // Show the inferred type even for non-Object init expressions
                        // (e.g. `var x = rex.roar()` where roar returns `nothing`).
                        if let Some(init_eid) = *init {
                            match typed.expr_types.get(&init_eid) {
                                Some(t)
                                    if !matches!(
                                        t,
                                        FidanType::Unknown
                                            | FidanType::Error
                                            | FidanType::Object(_)
                                    ) =>
                                {
                                    Some(type_name(t, interner))
                                }
                                _ => None,
                            }
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "?".into());
                table.put(
                    vname.clone(),
                    SymbolEntry {
                        kind: SymKind::Variable {
                            is_const: *is_const,
                        },
                        span: *span,
                        detail: format!("```fidan\n{} {}: {}\n```", kw, vname, ty_s),
                        ty_name,
                        param_types: vec![],
                        param_required: vec![],
                        return_type: None,
                    },
                );
            }
            // Fix-up field declaration spans from the AST — ObjectInfo only stores
            // FidanType per field, not the source span, so the typed pass above used
            // the whole-object span.  Here we overwrite with the real FieldDecl span.
            Item::ObjectDecl { name, fields, .. } => {
                let class_name = res(*name);
                for field in fields {
                    let fname = res(field.name);
                    let key = format!("{}.{}", class_name, fname);
                    if let Some(entry) = table.entries.get_mut(&key) {
                        entry.span = field.span;
                    }
                }
            }
            _ => {}
        }
    }

    // ── Inherited members ─────────────────────────────────────────────────────
    // For each child object, walk the parent chain and add entries for fields
    // and methods it inherits (e.g. `"TRex.name"` inherited from `"Dinosaur"`).
    // We collect (child, parent) pairs first so the main `typed.objects` map
    // can still be borrowed immutably inside the loop.
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
                        ty_name: None,
                        param_types: vec![],
                        param_required: vec![],
                        return_type: None,
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

fn type_name(ty: &FidanType, interner: &SymbolInterner) -> String {
    ty.display_name(&|sym| interner.resolve(sym).to_string())
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
