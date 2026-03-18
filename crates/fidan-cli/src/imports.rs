// ── File-import helpers ──────────────────────────────────────────────────────────

/// The single stdlib root prefix.  Every stdlib import starts with `std`
/// (`use std.io`, `use std.math`, …), so only that one token needs to be
/// excluded from file-based resolution.  If a user writes bare `use math`,
/// `find_relative` will simply find nothing — no magic silent swallow.
const STDLIB_MODULES: &[&str] = &["std"];
type ImportFilter = Option<std::collections::HashSet<String>>;
type ResolvedImport = (std::path::PathBuf, bool, ImportFilter);
type UnresolvedImport = (String, fidan_source::Span);

/// Resolve a dot-path user import relative to `base_dir` (the directory of
/// the importing file), mirroring Python's package layout:
///
/// ```text
/// use mymod            →  {base_dir}/mymod.fdn
///                      OR {base_dir}/mymod/init.fdn
///
/// use mymod.utils      →  {base_dir}/mymod/utils.fdn
///                      OR {base_dir}/mymod/utils/init.fdn
/// ```
///
/// The user chooses the folder name — no magic directory is required.
fn find_relative(base_dir: &std::path::Path, segments: &[String]) -> Option<std::path::PathBuf> {
    // Build the directory prefix from all but the last segment.
    // e.g. ["mymod", "utils"] → prefix = base_dir/mymod, leaf = "utils"
    // e.g. ["mymod"]          → prefix = base_dir,        leaf = "mymod"
    let (dir_parts, leaf) = segments.split_at(segments.len().saturating_sub(1));

    let mut dir = base_dir.to_path_buf();
    for part in dir_parts {
        dir.push(part);
    }

    let leaf = leaf.first().map(|s| s.as_str()).unwrap_or("");

    // Try `{dir}/{leaf}.fdn`
    let flat = dir.join(format!("{leaf}.fdn"));
    if flat.exists() {
        return Some(flat);
    }
    // Try `{dir}/{leaf}/init.fdn`
    let init = dir.join(leaf).join("init.fdn");
    if init.exists() {
        return Some(init);
    }
    None
}

/// Returns `(resolved_path, re_export)` pairs for every file-import in `module`.
///
/// - `use "./path"` / `use "../path"` / `use "/abs/path"` — explicit file path
/// - `use mymod` / `use mymod.sub` — resolved relative to the importing file's
///   directory (Python-style): `mymod.fdn` or `mymod/init.fdn`
///
/// The `re_export` flag mirrors the `export use` keyword: when `true` the
/// imported file's symbols should be re-exposed to the grandparent importer.
///
/// Stdlib names (`io`, `math`, etc.) are skipped — the MIR lowerer handles those.
///
/// Returns `(resolved, unresolved)` where `unresolved` holds `(dotted_name, span)`
/// for every user import whose file could not be found on disk.
pub(crate) fn collect_file_import_paths(
    module: &fidan_ast::Module,
    interner: &fidan_lexer::SymbolInterner,
    base_dir: &std::path::Path,
) -> (
    std::collections::VecDeque<ResolvedImport>,
    Vec<UnresolvedImport>,
) {
    // Three import modes, encoded in `Option<HashSet<String>>`:
    //
    //   None               = Namespace  (`use mod` / `use mod.sub`): HirModule is merged into
    //                         MIR for dispatch, but nothing is registered flat in typeck.
    //                         Call as `mod.fn()` only.
    //
    //   Some(empty set)    = Wildcard   (file-path imports: `use "./utils.fdn"`): everything
    //                         from the module is registered flat in typeck.  Backward-compat.
    //
    //   Some(non-empty)    = Flat       (`use mod.{name}`): only the listed names are registered
    //                         flat in typeck; HIR is filtered before merging into MIR.
    //
    // Priority when the same path is imported multiple times: Wildcard > Namespace > Flat.

    // Enum used only inside this function to compute the filter before writing to path_map.
    enum Mode {
        Wildcard,
        Namespace,
        Flat(String),
    }

    let mut path_map: Vec<ResolvedImport> = Vec::new();
    let mut unresolved: Vec<UnresolvedImport> = Vec::new();

    let mut add = |resolved: std::path::PathBuf, re_export: bool, mode: Mode| {
        if let Some(entry) = path_map.iter_mut().find(|(p, _, _)| *p == resolved) {
            entry.1 |= re_export;
            // If already a wildcard (Some(empty)), it can never be downgraded.
            if entry.2.as_ref().is_some_and(|s| s.is_empty()) {
                return;
            }
            match mode {
                // Upgrade to wildcard.
                Mode::Wildcard => entry.2 = Some(std::collections::HashSet::new()),
                // Namespace wins over existing flat (Some(names) → None).
                Mode::Namespace => entry.2 = None,
                // Accumulate flat name — but only if currently flat (Some).
                // If currently namespace (None), do nothing (namespace wins).
                Mode::Flat(name) => {
                    if let Some(ref mut set) = entry.2 {
                        set.insert(name);
                    }
                }
            }
        } else {
            let filter = match mode {
                Mode::Wildcard => Some(std::collections::HashSet::new()),
                Mode::Namespace => None,
                Mode::Flat(name) => {
                    let mut s = std::collections::HashSet::new();
                    s.insert(name);
                    Some(s)
                }
            };
            path_map.push((resolved, re_export, filter));
        }
    };

    for &item_id in &module.items {
        let item = module.arena.get_item(item_id);
        if let fidan_ast::Item::Use {
            path,
            alias: item_alias,
            re_export,
            grouped,
            span,
            ..
        } = item
        {
            if path.is_empty() {
                continue;
            }
            let first = interner.resolve(path[0]);

            // ── Explicit file-path import (string with ./ ../ / or .fdn) ───
            // Wildcard when no alias (all symbols exposed flat).
            // Namespace when alias given: `use "./f.fdn" as ns` → `ns.fn()` only.
            if path.len() == 1
                && (first.starts_with("./")
                    || first.starts_with("../")
                    || first.starts_with('/')
                    || first.ends_with(".fdn"))
            {
                let mode = if item_alias.is_some() {
                    Mode::Namespace
                } else {
                    Mode::Wildcard
                };
                add(base_dir.join(&*first), *re_export, mode);
                continue;
            }

            // ── Stdlib import — handled by MIR lowerer, skip ───────────────
            if STDLIB_MODULES.contains(&&*first) {
                continue;
            }

            // ── User package import — resolve relative to base_dir ─────────
            let segments: Vec<String> = path
                .iter()
                .map(|&s| interner.resolve(s).to_string())
                .collect();

            if *grouped {
                // Flat import: `use mod.{name}` — the last path segment is a
                // specific name to import flat.  Resolve the prefix as the file.
                if segments.len() >= 2 {
                    let prefix = &segments[..segments.len() - 1];
                    let specific_name = segments.last().unwrap().clone();
                    if let Some(resolved) = find_relative(base_dir, prefix) {
                        add(resolved, *re_export, Mode::Flat(specific_name));
                    } else if let Some(resolved) = find_relative(base_dir, &segments) {
                        // Edge: the full path happens to be a file — namespace.
                        add(resolved, *re_export, Mode::Namespace);
                    } else {
                        unresolved.push((segments.join("."), *span));
                    }
                } else {
                    // Single-segment grouped edge case — treat as namespace.
                    if let Some(resolved) = find_relative(base_dir, &segments) {
                        add(resolved, *re_export, Mode::Namespace);
                    } else {
                        unresolved.push((segments.join("."), *span));
                    }
                }
            } else {
                // Namespace import: `use mod` / `use mod.submod` — resolve the
                // full path; the last segment becomes the namespace alias.
                if let Some(resolved) = find_relative(base_dir, &segments) {
                    add(resolved, *re_export, Mode::Namespace);
                } else {
                    unresolved.push((segments.join("."), *span));
                }
            }
        }
    }

    (path_map.into_iter().collect(), unresolved)
}

/// Pre-register functions, objects, and globals from `hir` into `tc` so the
/// main file's type-checker sees imported symbols as known bindings.
///
/// `filter` — when `Some`, only names in the set are registered (flat/grouped
/// import, e.g. `use mod.{name}`).  When `None` (namespace import, e.g.
/// `use mod`), nothing is registered flat — the namespace variable itself is
/// already bound by `check_item` so calls like `mod.fn()` type-check correctly
/// via dynamic dispatch on `FidanType::Dynamic`.
pub(crate) fn pre_register_hir_into_tc(
    tc: &mut fidan_typeck::TypeChecker,
    hir: &fidan_hir::HirModule,
    filter: Option<&std::collections::HashSet<String>>,
    interner: &fidan_lexer::SymbolInterner,
) {
    use fidan_typeck::{ActionInfo, ParamInfo};

    let visible = |sym: fidan_lexer::Symbol| -> bool {
        // None              → namespace import: nothing registered flat.
        // Some(empty set)   → wildcard (file-path): everything registered flat.
        // Some(non-empty)   → flat import: only listed names registered.
        match filter {
            None => false,
            Some(f) if f.is_empty() => true,
            Some(f) => f.contains(interner.resolve(sym).as_ref()),
        }
    };

    for func in &hir.functions {
        if !visible(func.name) {
            continue;
        }
        let info = ActionInfo {
            params: func
                .params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name,
                    ty: p.ty.clone(),
                    certain: p.certain,
                    optional: p.optional,
                    has_default: p.default.is_some(),
                })
                .collect(),
            return_ty: func.return_ty.clone(),
            span: func.span,
        };
        tc.pre_register_action(func.name, info);
    }

    for obj in &hir.objects {
        if !visible(obj.name) {
            continue;
        }
        tc.pre_register_object_data(
            obj.name,
            obj.parent,
            obj.span,
            obj.fields.iter().map(|f| (f.name, f.ty.clone())),
            obj.methods.iter().map(|m| {
                let ai = ActionInfo {
                    params: m
                        .params
                        .iter()
                        .map(|p| ParamInfo {
                            name: p.name,
                            ty: p.ty.clone(),
                            certain: p.certain,
                            optional: p.optional,
                            has_default: p.default.is_some(),
                        })
                        .collect(),
                    return_ty: m.return_ty.clone(),
                    span: m.span,
                };
                (m.name, ai)
            }),
        );
    }

    for glob in &hir.globals {
        if !visible(glob.name) {
            continue;
        }
        tc.pre_register_global(glob.name, glob.ty.clone(), glob.is_const, glob.span);
    }

    // Top-level variable declarations live in init_stmts (HirGlobal is unused by the
    // current HIR lowerer — all top-level vars, including `const var`, become VarDecl
    // init statements).  Scan the first level to pre-register any such declarations.
    for stmt in &hir.init_stmts {
        if let fidan_hir::HirStmt::VarDecl {
            name,
            ty,
            is_const,
            span,
            ..
        } = stmt
            && visible(*name)
        {
            tc.pre_register_global(*name, ty.clone(), *is_const, *span);
        }
    }

    // Re-exported stdlib namespaces: if the imported file declared `export use
    // std.X`, expose the binding in the caller's type-checker so accesses like
    // `X.fn()` don't produce false E0101 errors.
    for decl in &hir.use_decls {
        if !decl.re_export {
            continue;
        }
        if decl.module_path.len() >= 2 {
            if let Some(names) = &decl.specific_names {
                // `export use std.io.readFile` — register the free-function name.
                for name in names {
                    tc.pre_register_namespace(name);
                }
            } else {
                // `export use std.io` — register the namespace alias.
                let alias = decl
                    .alias
                    .as_deref()
                    .unwrap_or(decl.module_path[1].as_str());
                tc.pre_register_namespace(alias);
            }
        } else if decl.module_path.len() == 1 && decl.specific_names.is_none() {
            // `export use mymod` — user-module re-export.  Register the namespace
            // alias so the importer's typechecker allows `mymod.fn()` calls.
            let alias = decl
                .alias
                .as_deref()
                .unwrap_or(decl.module_path[0].as_str());
            tc.pre_register_namespace(alias);
        }
    }
}

/// Filter a HIR module to only the named functions, objects, and globals.
///
/// Used for flat/grouped imports (`use mod.{name}`) so that only the requested
/// symbols end up in the merged MIR — preventing unnamed symbols from being
/// callable without a namespace prefix.
/// Top-level init statements (side-effects) and use_decls are kept intact.
pub(crate) fn filter_hir_module(
    mut hir: fidan_hir::HirModule,
    names: &std::collections::HashSet<String>,
    interner: &fidan_lexer::SymbolInterner,
) -> fidan_hir::HirModule {
    hir.functions
        .retain(|f| names.contains(interner.resolve(f.name).as_ref()));
    hir.objects
        .retain(|o| names.contains(interner.resolve(o.name).as_ref()));
    hir.globals
        .retain(|g| names.contains(interner.resolve(g.name).as_ref()));
    // Keep init_stmts as-is: top-level side-effects (e.g. print("IMPORTED"))
    // should execute even for selective imports, matching Python semantics.
    hir
}
