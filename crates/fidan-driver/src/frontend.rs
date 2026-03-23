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

pub fn collect_file_import_paths(
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    base_dir: &Path,
) -> (VecDeque<ResolvedImport>, Vec<UnresolvedImport>) {
    enum Mode {
        Wildcard,
        Namespace,
        Flat(String),
    }

    let mut path_map: Vec<ResolvedImport> = Vec::new();
    let mut unresolved: Vec<UnresolvedImport> = Vec::new();

    let mut add = |resolved: PathBuf, re_export: bool, mode: Mode| {
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
                add(base_dir.join(&*first), *re_export, mode);
                continue;
            }

            if STDLIB_MODULES.contains(&&*first) {
                continue;
            }

            let segments: Vec<String> = path
                .iter()
                .map(|&sym| interner.resolve(sym).to_string())
                .collect();

            if *grouped {
                if segments.len() >= 2 {
                    let prefix = &segments[..segments.len() - 1];
                    let specific_name = segments.last().cloned().unwrap_or_default();
                    if let Some(resolved) = find_relative(base_dir, prefix) {
                        add(resolved, *re_export, Mode::Flat(specific_name));
                    } else if let Some(resolved) = find_relative(base_dir, &segments) {
                        add(resolved, *re_export, Mode::Namespace);
                    } else {
                        unresolved.push((segments.join("."), *span));
                    }
                } else if let Some(resolved) = find_relative(base_dir, &segments) {
                    add(resolved, *re_export, Mode::Namespace);
                } else {
                    unresolved.push((segments.join("."), *span));
                }
            } else if let Some(resolved) = find_relative(base_dir, &segments) {
                add(resolved, *re_export, Mode::Namespace);
            } else {
                unresolved.push((segments.join("."), *span));
            }
        }
    }

    (path_map.into_iter().collect(), unresolved)
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
                    tc.pre_register_namespace(name);
                }
            } else {
                let alias = decl
                    .alias
                    .as_deref()
                    .unwrap_or(decl.module_path[1].as_str());
                tc.pre_register_namespace(alias);
            }
        } else if decl.module_path.len() == 1 && decl.specific_names.is_none() {
            let alias = decl
                .alias
                .as_deref()
                .unwrap_or(decl.module_path[0].as_str());
            tc.pre_register_namespace(alias);
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

fn load_imported_hirs(
    module: &fidan_ast::Module,
    interner: &Arc<SymbolInterner>,
    base_dir: &Path,
    source_map: &Arc<SourceMap>,
    root_path: Option<&Path>,
    errors: &mut Vec<String>,
) -> Vec<(fidan_hir::HirModule, ImportFilter, bool)> {
    type QueueItem = (PathBuf, bool, ImportFilter);

    let mut hirs: Vec<(fidan_hir::HirModule, ImportFilter, bool)> = Vec::new();
    let (main_paths, main_unresolved) = collect_file_import_paths(module, interner, base_dir);
    for (name, _) in main_unresolved {
        errors.push(format!(
            "{}: module `{name}` not found",
            diag_code!("E0106")
        ));
    }

    let mut queue: VecDeque<QueueItem> = main_paths
        .into_iter()
        .map(|(path, _, filter)| (path, true, filter))
        .collect();

    let mut loaded: HashSet<PathBuf> = HashSet::new();
    if let Some(path) = root_path
        && let Ok(canon) = path.canonicalize()
    {
        loaded.insert(canon);
    }

    while let Some((import_path, expose, filter)) = queue.pop_front() {
        let canon = import_path
            .canonicalize()
            .unwrap_or_else(|_| import_path.clone());
        if !loaded.insert(canon) {
            continue;
        }

        match std::fs::read_to_string(&import_path) {
            Ok(import_src) => {
                let import_name = import_path.display().to_string();
                let import_file = source_map.add_file(&*import_name, &*import_src);
                let (import_tokens, import_lex_diags) =
                    Lexer::new(&import_file, Arc::clone(interner)).tokenise();
                let lex_errs = collect_error_messages(&import_lex_diags, errors);

                let (import_module, import_parse_diags) =
                    fidan_parser::parse(&import_tokens, import_file.id, Arc::clone(interner));
                let parse_errs = collect_error_messages(&import_parse_diags, errors);

                let import_base = import_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                let (sub_paths, sub_unresolved) =
                    collect_file_import_paths(&import_module, interner, &import_base);
                for (name, _) in sub_unresolved {
                    errors.push(format!(
                        "{}: module `{name}` not found",
                        diag_code!("E0106")
                    ));
                }
                for (sub_path, sub_re_export, sub_filter) in sub_paths {
                    queue.push_back((sub_path, expose && sub_re_export, sub_filter));
                }

                if lex_errs == 0 && parse_errs == 0 {
                    let import_tm =
                        fidan_typeck::typecheck_full(&import_module, Arc::clone(interner));
                    let type_errs = collect_error_messages(&import_tm.diagnostics, errors);
                    if type_errs == 0 {
                        let import_hir =
                            fidan_hir::lower_module(&import_module, &import_tm, interner);
                        hirs.push((import_hir, filter, expose));
                    }
                }
            }
            Err(err) => errors.push(format!(
                "{}: cannot load import `{}`: {err}",
                diag_code!("R0001"),
                import_path.display()
            )),
        }
    }

    hirs
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

    let (tokens, lex_diags) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));

    let mut errors = Vec::new();
    let mut error_count = 0usize;
    error_count += collect_error_messages(&lex_diags, &mut errors);
    error_count += collect_error_messages(&parse_diags, &mut errors);

    let imported_hirs = if error_count == 0 {
        load_imported_hirs(&module, &interner, base_dir, &source_map, None, &mut errors)
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
        tc.check_module(&module);
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
            let base = fidan_hir::lower_module(&module, &typed, &interner);
            imported_hirs
                .into_iter()
                .fold(base, |acc, (import_hir, filter, _)| {
                    let filtered = if let Some(names) = filter.as_ref().filter(|f| !f.is_empty()) {
                        filter_hir_module(import_hir, names, &interner)
                    } else {
                        import_hir
                    };
                    fidan_hir::merge_module(acc, filtered)
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

    fidan_passes::run_all(&mut mir);

    Ok(FrontendOutput {
        interner,
        source_map,
        mir,
    })
}

pub fn compile_file_to_mir(path: &Path) -> Result<FrontendOutput> {
    let src = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read `{}`", path.display()))?;
    let base_dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    compile_source_to_mir(&path.display().to_string(), &src, &base_dir)
}
