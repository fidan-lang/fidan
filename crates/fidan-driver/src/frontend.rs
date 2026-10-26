use crate::dal::{
    global_package_store, local_package_store, lock_entry_for_module, package_install_dir,
    project_root_from, read_lock_from_path,
};
use crate::install::resolve_fidan_home;
use anyhow::{Context, Result, bail};
use fidan_diagnostics::{Diagnostic, Severity, diag_code};
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_mir::MirProgram;
use fidan_source::{SourceMap, Span};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const STDLIB_MODULES: &[&str] = &["std"];

pub type ImportFilter = Option<HashSet<String>>;
pub type ResolvedImport = (PathBuf, bool, ImportFilter);
pub type UnresolvedImport = (String, Span);

pub struct FrontendOutput {
    pub interner: Arc<SymbolInterner>,
    pub source_map: Arc<SourceMap>,
    pub mir: MirProgram,
}

pub fn detect_import_cycles(entry_path: &Path) -> Vec<String> {
    fn visit(
        path: &Path,
        interner: &Arc<SymbolInterner>,
        visited: &mut HashSet<PathBuf>,
        stack: &mut Vec<PathBuf>,
        errors: &mut Vec<String>,
    ) {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(pos) = stack.iter().position(|entry| entry == &canon) {
            let mut cycle = stack[pos..]
                .iter()
                .map(|entry| entry.display().to_string())
                .collect::<Vec<_>>();
            cycle.push(canon.display().to_string());
            errors.push(format!(
                "{}: import cycle detected: {}",
                diag_code!("E0106"),
                cycle.join(" -> ")
            ));
            return;
        }
        fidan_diagnostics::tracing::trace!(path = %canon.display(), cached = visited.contains(&canon), "import graph visit");
        if !visited.insert(canon.clone()) {
            return;
        }

        let src = match std::fs::read_to_string(&canon) {
            Ok(src) => src,
            Err(_) => return,
        };
        let name = canon.display().to_string();
        let source_map = Arc::new(SourceMap::new());
        let file = source_map.add_file(name.as_str(), src.as_str());
        let (tokens, _) = Lexer::new(&file, Arc::clone(interner)).tokenise();
        let (module, _) = fidan_parser::parse(&tokens, file.id, Arc::clone(interner));
        let base_dir = canon
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        stack.push(canon);
        let (imports, _) = collect_file_import_paths(&module, interner, &base_dir);
        for (import_path, _, _) in imports {
            visit(&import_path, interner, visited, stack, errors);
        }
        stack.pop();
    }

    let interner = Arc::new(SymbolInterner::new());
    let mut visited = HashSet::new();
    let mut stack = Vec::new();
    let mut errors = Vec::new();
    visit(entry_path, &interner, &mut visited, &mut stack, &mut errors);
    errors.sort();
    errors.dedup();
    errors
}

fn find_relative(base_dir: &Path, segments: &[String]) -> Option<PathBuf> {
    let (dir_parts, leaf) = segments.split_at(segments.len().saturating_sub(1));

    let mut dir = base_dir.to_path_buf();
    for part in dir_parts {
        dir.push(part);
    }

    let leaf = leaf.first().map(|s| s.as_str()).unwrap_or("");

    let flat = dir.join(format!("{leaf}.fdn"));
    if flat.exists() {
        return Some(flat);
    }

    let init = dir.join(leaf).join("init.fdn");
    if init.exists() {
        return Some(init);
    }

    None
}

fn find_in_package_root(package_root: &Path, segments: &[String]) -> Option<PathBuf> {
    if segments.is_empty() {
        let init = package_root.join("init.fdn");
        if init.exists() {
            return Some(init);
        }
        return None;
    }

    find_relative(package_root, segments)
}

fn find_in_ancestor_local_package_stores(
    start_root: &Path,
    package: &str,
    version: &str,
    segments: &[String],
) -> Option<PathBuf> {
    let mut current = start_root.to_path_buf();
    loop {
        let package_root = package_install_dir(&local_package_store(&current), package, version);
        if let Some(path) = find_in_package_root(&package_root, segments) {
            return Some(path);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn package_resolution_scope(base_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    for ancestor in base_dir.ancestors() {
        if ancestor.file_name().is_some_and(|v| v == "packages") {
            let parent = ancestor.parent()?;
            if parent.file_name().is_some_and(|v| v == ".dal") {
                let root = parent.parent()?.to_path_buf();
                return Some((root.clone(), crate::dal::lock_path(&root)));
            }
            if parent.file_name().is_some_and(|v| v == "dal")
                && parent.join("global.lock").is_file()
            {
                return Some((parent.to_path_buf(), parent.join("global.lock")));
            }
        }
    }
    let root = project_root_from(base_dir)?;
    Some((root.clone(), crate::dal::lock_path(&root)))
}

fn find_locked_package_import(scope_dir: &Path, segments: &[String]) -> Result<Option<PathBuf>> {
    let Some(module) = segments.first() else {
        return Ok(None);
    };
    let Some((project_root, lock_file)) = package_resolution_scope(scope_dir) else {
        return Ok(None);
    };
    if !lock_file.is_file() {
        return Ok(None);
    }
    let lock = read_lock_from_path(&lock_file)?;
    let Some(locked) = lock_entry_for_module(&lock, module) else {
        return Ok(None);
    };
    let local = find_in_ancestor_local_package_stores(
        &project_root,
        &locked.name,
        &locked.version,
        &segments[1..],
    );
    let path = if local.is_some() {
        local
    } else {
        let home = resolve_fidan_home()?;
        let global_root =
            package_install_dir(&global_package_store(&home), &locked.name, &locked.version);
        find_in_package_root(&global_root, &segments[1..])
    };
    if let Some(path) = &path {
        let package_root = project_root_from(path).context("installed package has no manifest")?;
        let manifest = crate::dal::read_manifest(&package_root)?;
        if manifest.package.name != locked.name || manifest.package.version != locked.version {
            bail!("installed package identity differs from `dal.lock`");
        }
        crate::dal::ensure_compiler_compatible(&manifest)?;
    }
    Ok(path)
}

pub fn collect_file_import_paths(
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    base_dir: &Path,
) -> (VecDeque<ResolvedImport>, Vec<UnresolvedImport>) {
    collect_file_import_paths_with_root(module, interner, base_dir, base_dir)
}

/// Transitive package imports use the consumer's resolved graph, not a publisher's
/// embedded dal.lock. Relative source paths still resolve from the importing file.
pub fn collect_file_import_paths_with_root(
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    base_dir: &Path,
    scope_dir: &Path,
) -> (VecDeque<ResolvedImport>, Vec<UnresolvedImport>) {
    let (paths, unresolved, _) = resolve_file_imports(module, interner, base_dir, scope_dir);
    (paths, unresolved)
}

fn resolve_file_imports(
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    base_dir: &Path,
    scope_dir: &Path,
) -> (
    VecDeque<ResolvedImport>,
    Vec<UnresolvedImport>,
    Vec<(Span, PathBuf)>,
) {
    enum Mode {
        Wildcard,
        Namespace,
        Flat(String),
    }

    let mut path_map: Vec<ResolvedImport> = Vec::new();
    let mut unresolved: Vec<UnresolvedImport> = Vec::new();

    let mut bindings = Vec::new();
    let mut add = |resolved: PathBuf, span: Span, re_export: bool, mode: Mode| {
        bindings.push((
            span,
            resolved.canonicalize().unwrap_or_else(|_| resolved.clone()),
        ));
        if let Some(entry) = path_map.iter_mut().find(|(p, _, _)| *p == resolved) {
            entry.1 |= re_export;
            if entry.2.as_ref().is_some_and(|s| s.is_empty()) {
                return;
            }
            match mode {
                Mode::Wildcard => entry.2 = Some(HashSet::new()),
                Mode::Namespace => entry.2 = None,
                Mode::Flat(name) => {
                    if let Some(ref mut set) = entry.2 {
                        set.insert(name);
                    }
                }
            }
        } else {
            let filter = match mode {
                Mode::Wildcard => Some(HashSet::new()),
                Mode::Namespace => None,
                Mode::Flat(name) => {
                    let mut set = HashSet::new();
                    set.insert(name);
                    Some(set)
                }
            };
            path_map.push((resolved, re_export, filter));
        }
    };

    for &item_id in &module.items {
        let item = module.arena.get_item(item_id);
        if let fidan_ast::Item::Use {
            path,
            alias,
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

            if path.len() == 1
                && (first.starts_with("./")
                    || first.starts_with("../")
                    || first.starts_with('/')
                    || first.ends_with(".fdn"))
            {
                let mode = if alias.is_some() {
                    Mode::Namespace
                } else {
                    Mode::Wildcard
                };
                add(base_dir.join(&*first), *span, *re_export, mode);
                continue;
            }

            if STDLIB_MODULES.contains(&&*first) {
                continue;
            }

            let segments: Vec<String> = path
                .iter()
                .map(|&sym| interner.resolve(sym).to_string())
                .collect();

            // Preserve errors from corrupt locks and incompatible installed manifests.
            let locked_path = match find_locked_package_import(scope_dir, &segments) {
                Ok(path) => path,
                Err(error) => {
                    unresolved.push((error.to_string(), *span));
                    continue;
                }
            };
            let locked_prefix = if *grouped && segments.len() >= 2 {
                match find_locked_package_import(scope_dir, &segments[..segments.len() - 1]) {
                    Ok(path) => path,
                    Err(error) => {
                        unresolved.push((error.to_string(), *span));
                        continue;
                    }
                }
            } else {
                None
            };
            if *grouped {
                if segments.len() >= 2 {
                    let prefix = &segments[..segments.len() - 1];
                    let specific_name = segments.last().cloned().unwrap_or_default();
                    if let Some(resolved) = find_relative(base_dir, prefix) {
                        add(resolved, *span, *re_export, Mode::Flat(specific_name));
                    } else if let Some(resolved) = locked_prefix.clone() {
                        add(resolved, *span, *re_export, Mode::Flat(specific_name));
                    } else if let Some(resolved) = find_relative(base_dir, &segments) {
                        add(resolved, *span, *re_export, Mode::Namespace);
                    } else if let Some(resolved) = locked_path.clone() {
                        add(resolved, *span, *re_export, Mode::Namespace);
                    } else {
                        unresolved.push((segments.join("."), *span));
                    }
                } else if let Some(resolved) = find_relative(base_dir, &segments) {
                    add(resolved, *span, *re_export, Mode::Namespace);
                } else if let Some(resolved) = locked_path.clone() {
                    add(resolved, *span, *re_export, Mode::Namespace);
                } else {
                    unresolved.push((segments.join("."), *span));
                }
            } else if let Some(resolved) = find_relative(base_dir, &segments) {
                add(resolved, *span, *re_export, Mode::Namespace);
            } else if let Some(resolved) = locked_path.clone() {
                add(resolved, *span, *re_export, Mode::Namespace);
            } else {
                unresolved.push((segments.join("."), *span));
            }
        }
    }

    (path_map.into_iter().collect(), unresolved, bindings)
}

/// Filesystem import diagnostics for editor documents, using the compiler's
/// local/locked-package resolution and original source spans.
pub fn import_resolution_diagnostics(
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    base_dir: &Path,
) -> Vec<Diagnostic> {
    let (_, unresolved, bindings) = resolve_file_imports(module, interner, base_dir, base_dir);
    unresolved
        .into_iter()
        .map(|(name, span)| {
            Diagnostic::error(
                diag_code!("E0106"),
                format!("module `{name}` not found"),
                span,
            )
        })
        .chain(
            bindings
                .into_iter()
                .filter(|(_, path)| !path.is_file())
                .map(|(span, path)| {
                    Diagnostic::error(
                        diag_code!("E0106"),
                        format!("module file `{}` not found", path.display()),
                        span,
                    )
                }),
        )
        .collect()
}

/// Preserve each import's file identity for MIR namespace/function resolution.
pub fn bind_hir_file_imports(
    hir: &mut fidan_hir::HirModule,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    source_map: &SourceMap,
    base_dir: &Path,
    scope_dir: &Path,
) {
    let (_, _, bindings) = resolve_file_imports(module, interner, base_dir, scope_dir);
    for decl in &mut hir.use_decls {
        decl.target_path = bindings
            .iter()
            .find(|(span, _)| *span == decl.span)
            .map(|(_, path)| path.clone());
    }
    let source = source_map.get(module.file);
    let path = PathBuf::from(source.name.as_ref());
    hir.file_paths
        .push((module.file, path.canonicalize().unwrap_or(path)));
}

pub fn pre_register_hir_into_tc(
    tc: &mut fidan_typeck::TypeChecker,
    hir: &fidan_hir::HirModule,
    filter: Option<&HashSet<String>>,
    interner: &SymbolInterner,
) {
    use fidan_typeck::{ActionInfo, ParamInfo};

    let visible = |sym: fidan_lexer::Symbol| -> bool {
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

    for declaration in &hir.enums {
        if visible(declaration.name) {
            tc.pre_register_enum(
                declaration.name,
                fidan_typeck::EnumInfo {
                    variants: declaration.variants.clone(),
                    span: declaration.span,
                },
            );
        }
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
                let info = ActionInfo {
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
                (m.name, info)
            }),
        );
    }

    for glob in &hir.globals {
        if !visible(glob.name) {
            continue;
        }
        tc.pre_register_global(glob.name, glob.ty.clone(), glob.is_const, glob.span);
    }

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

    for decl in &hir.use_decls {
        if !decl.re_export {
            continue;
        }
        if decl.module_path.len() >= 2 {
            if let Some(names) = &decl.specific_names {
                for name in names {
                    if visible(interner.intern(name)) {
                        tc.pre_register_namespace(name);
                    }
                }
            } else {
                let alias = decl
                    .alias
                    .as_deref()
                    .unwrap_or(decl.module_path[1].as_str());
                if visible(interner.intern(alias)) {
                    tc.pre_register_namespace(alias);
                }
            }
        } else if decl.module_path.len() == 1 && decl.specific_names.is_none() {
            let alias = decl
                .alias
                .as_deref()
                .unwrap_or(decl.module_path[0].as_str());
            if visible(interner.intern(alias)) {
                tc.pre_register_namespace(alias);
            }
        }
    }
}

pub fn filter_hir_module(
    mut hir: fidan_hir::HirModule,
    names: &HashSet<String>,
    interner: &SymbolInterner,
) -> fidan_hir::HirModule {
    hir.functions
        .retain(|f| names.contains(interner.resolve(f.name).as_ref()));
    hir.objects
        .retain(|o| names.contains(interner.resolve(o.name).as_ref()));
    hir.globals
        .retain(|g| names.contains(interner.resolve(g.name).as_ref()));
    hir
}

fn diag_message(diag: &Diagnostic) -> String {
    format!("{}: {}", diag.code.as_str(), diag.message)
}

fn collect_error_messages(diags: &[Diagnostic], errors: &mut Vec<String>) -> usize {
    let mut count = 0usize;
    for diag in diags {
        if diag.severity == Severity::Error {
            errors.push(diag_message(diag));
            count += 1;
        }
    }
    count
}

fn join_errors(errors: Vec<String>) -> String {
    let mut deduped: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for error in errors {
        if seen.insert(error.clone()) {
            deduped.push(error);
        }
    }
    deduped.join("\n")
}

pub fn compile_source_to_mir(
    source_name: &str,
    src: &str,
    base_dir: &Path,
) -> Result<FrontendOutput> {
    let source_map = Arc::new(SourceMap::new());
    let file = source_map.add_file(source_name, src);
    let interner = Arc::new(SymbolInterner::new());

    let (tokens, lex_diags) = fidan_diagnostics::logging::phase("frontend_lex", || {
        Lexer::new(&file, Arc::clone(&interner)).tokenise()
    });
    let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));

    let mut errors = Vec::new();
    let mut error_count = 0usize;
    error_count += collect_error_messages(&lex_diags, &mut errors);
    error_count += collect_error_messages(&parse_diags, &mut errors);

    let imported_hirs = if error_count == 0 {
        let loaded =
            crate::load_imported_hirs(&module, &interner, base_dir, &source_map, None, &|path| {
                std::fs::read_to_string(path)
            });
        collect_error_messages(&loaded.diagnostics, &mut errors);
        loaded.modules
    } else {
        Vec::new()
    };
    error_count = errors.len();

    let typed_module = if error_count == 0 {
        let mut tc = fidan_typeck::TypeChecker::new(Arc::clone(&interner), file.id);
        for (hir, filter, expose_to_typeck) in &imported_hirs {
            if *expose_to_typeck {
                pre_register_hir_into_tc(&mut tc, hir, filter.as_ref(), &interner);
            }
        }
        fidan_diagnostics::logging::phase("frontend_typecheck", || tc.check_module(&module));
        let typed = tc.finish_typed();
        let type_errs = collect_error_messages(&typed.diagnostics, &mut errors);
        if type_errs == 0 { Some(typed) } else { None }
    } else {
        None
    };

    if !errors.is_empty() {
        bail!(join_errors(errors));
    }

    let merged_hir = typed_module
        .map(|typed| {
            let mut base = fidan_hir::lower_module(&module, &typed, &interner);
            bind_hir_file_imports(
                &mut base,
                &module,
                &interner,
                &source_map,
                base_dir,
                base_dir,
            );
            imported_hirs
                .into_iter()
                .fold(base, |acc, (import_hir, _filter, _)| {
                    // Import filters control visibility, not which internal helpers exist.
                    fidan_hir::merge_module(acc, import_hir)
                })
        })
        .context("frontend pipeline did not produce HIR")?;

    let mut mir = fidan_mir::lower_program(&merged_hir, &interner, &[]);
    for diag in fidan_passes::check_parallel_races(&mir, &interner) {
        errors.push(format!(
            "{}: data race on `{}`: {}",
            diag_code!("E0401"),
            diag.var_name,
            diag.context
        ));
    }
    if !errors.is_empty() {
        bail!(join_errors(errors));
    }

    fidan_diagnostics::logging::phase("frontend_optimize", || fidan_passes::run_all(&mut mir));

    Ok(FrontendOutput {
        interner,
        source_map,
        mir,
    })
}

pub fn compile_file_to_mir(path: &Path) -> Result<FrontendOutput> {
    let cycle_errors = detect_import_cycles(path);
    if !cycle_errors.is_empty() {
        bail!(join_errors(cycle_errors));
    }
    let src = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read `{}`", path.display()))?;
    let base_dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    compile_source_to_mir(&path.display().to_string(), &src, &base_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detect_import_cycles_reports_file_cycle() {
        let sandbox = make_temp_dir("fidan_import_cycle_test");
        let a = sandbox.join("a.fdn");
        let b = sandbox.join("b.fdn");
        fs::write(&a, "use \"./b.fdn\"\naction main {}\n").expect("write a");
        fs::write(&b, "use \"./a.fdn\"\naction helper {}\n").expect("write b");

        let errors = detect_import_cycles(&a);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("import cycle detected"));
        assert!(errors[0].contains("a.fdn"));
        assert!(errors[0].contains("b.fdn"));

        fs::remove_dir_all(&sandbox).ok();
    }

    fn make_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nonce));
        fs::create_dir_all(&dir).expect("failed to create temp test dir");
        dir
    }
}
