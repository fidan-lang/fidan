//! Shared import graph loading for CLI, embedding and editor analysis.
//! Parse once, check dependencies before their consumers, and retain private HIR
//! for execution without leaking private symbols into importing scopes.
use crate::frontend::{
    ImportFilter, ResolvedImport, bind_hir_file_imports, collect_file_import_paths,
    collect_file_import_paths_with_root, pre_register_hir_into_tc,
};
use fidan_ast::Module;
use fidan_diagnostics::{Diagnostic, Severity, diag_code};
use fidan_hir::HirModule;
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::{SourceMap, Span};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct LoadedImports {
    pub modules: Vec<(HirModule, ImportFilter, bool)>,
    pub diagnostics: Vec<Diagnostic>,
}

struct ParsedImport {
    module: Module,
    imports: Vec<ResolvedImport>,
    valid: bool,
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

fn normalize(imports: impl IntoIterator<Item = ResolvedImport>) -> Vec<ResolvedImport> {
    imports
        .into_iter()
        .map(|(path, export, names)| (canonical(&path), export, names))
        .collect()
}

/// An empty set means all exports; None means no flat exports. Intersect masks
/// before following re-exports so a selective import never exposes extra names.
fn visible_modules(
    roots: &[ResolvedImport],
    graph: &HashMap<PathBuf, ParsedImport>,
) -> HashMap<PathBuf, HashSet<String>> {
    let mut visible = HashMap::<PathBuf, HashSet<String>>::new();
    let mut pending = roots
        .iter()
        .map(|(path, _, names)| (path.clone(), names.clone()))
        .collect::<VecDeque<_>>();
    while let Some((path, names)) = pending.pop_front() {
        let Some(names) = names else {
            continue;
        };
        let changed = match visible.entry(path.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(names.clone());
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().is_empty() {
                    false
                } else if names.is_empty() {
                    entry.get_mut().clear();
                    true
                } else {
                    let before = entry.get().len();
                    entry.get_mut().extend(names);
                    before != entry.get().len()
                }
            }
        };
        if !changed {
            continue;
        }
        let mask = &visible[&path];
        if let Some(module) = graph.get(&path) {
            for (child, exported, names) in &module.imports {
                if !exported {
                    continue;
                }
                if let Some(names) = names {
                    let selected = if mask.is_empty() {
                        names.clone()
                    } else if names.is_empty() {
                        mask.clone()
                    } else {
                        mask.intersection(names).cloned().collect()
                    };
                    if !selected.is_empty() || mask.is_empty() && names.is_empty() {
                        pending.push_back((child.clone(), Some(selected)));
                    }
                }
            }
        }
    }
    visible
}

/// Compiler-owned import contracts. The reader permits unsaved editor overlays;
/// no imported user code is executed while inspecting signatures.
pub fn load_imported_hirs(
    module: &Module,
    interner: &Arc<SymbolInterner>,
    base_dir: &Path,
    source_map: &Arc<SourceMap>,
    root_path: Option<&Path>,
    read_source: &impl Fn(&Path) -> std::io::Result<String>,
) -> LoadedImports {
    let mut diagnostics = Vec::new();
    let (roots, unresolved) = collect_file_import_paths(module, interner, base_dir);
    let roots = normalize(roots);
    for (name, span) in unresolved {
        diagnostics.push(Diagnostic::error(
            diag_code!("E0106"),
            format!("module `{name}` not found"),
            span,
        ));
    }
    let root_path = root_path.map(canonical);
    let mut queue = roots
        .iter()
        .map(|(path, _, _)| path.clone())
        .collect::<VecDeque<_>>();
    let mut seen = HashSet::new();
    let mut graph = HashMap::new();
    while let Some(path) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if root_path.as_ref() == Some(&path) {
            diagnostics.push(Diagnostic::error(
                diag_code!("E0106"),
                "import cycle reaches the entry module",
                Span::point(module.file, 0),
            ));
            continue;
        }
        let text = match read_source(&path) {
            Ok(text) => text,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    diag_code!("E0106"),
                    format!("cannot load import `{}`: {error}", path.display()),
                    Span::point(module.file, 0),
                ));
                continue;
            }
        };
        let file = source_map.add_file(path.display().to_string(), text);
        let (tokens, lex) = Lexer::new(&file, Arc::clone(interner)).tokenise();
        let (parsed, syntax) = fidan_parser::parse(&tokens, file.id, Arc::clone(interner));
        let valid = !lex
            .iter()
            .chain(syntax.iter())
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        diagnostics.extend(lex);
        diagnostics.extend(syntax);
        let (imports, unresolved) = collect_file_import_paths_with_root(
            &parsed,
            interner,
            path.parent().unwrap_or(base_dir),
            base_dir,
        );
        for (name, span) in unresolved {
            diagnostics.push(Diagnostic::error(
                diag_code!("E0106"),
                format!("module `{name}` not found"),
                span,
            ));
        }
        let imports = normalize(imports);
        queue.extend(imports.iter().map(|(path, _, _)| path.clone()));
        graph.insert(
            path,
            ParsedImport {
                module: parsed,
                imports,
                valid,
            },
        );
    }

    // Iterative DFS avoids recursion on long import chains. Declaration-order
    // traversal gives dependency-first, once-only module initialization.
    let mut stack = roots
        .iter()
        .rev()
        .map(|(path, _, _)| (path.clone(), false))
        .collect::<Vec<_>>();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut order = Vec::new();
    while let Some((path, leaving)) = stack.pop() {
        if visited.contains(&path) {
            continue;
        }
        let Some(node) = graph.get(&path) else {
            continue;
        };
        if leaving {
            visiting.remove(&path);
            visited.insert(path.clone());
            order.push(path);
        } else if visiting.insert(path.clone()) {
            stack.push((path, true));
            stack.extend(
                node.imports
                    .iter()
                    .rev()
                    .map(|(path, _, _)| (path.clone(), false)),
            );
        } else {
            diagnostics.push(Diagnostic::error(
                diag_code!("E0106"),
                "import cycle detected",
                Span::point(node.module.file, 0),
            ));
        }
    }

    let mut checked = HashMap::<PathBuf, HirModule>::new();
    for path in &order {
        let node = &graph[path];
        if !node.valid {
            continue;
        }
        let mut checker = fidan_typeck::TypeChecker::new(Arc::clone(interner), node.module.file);
        let mut contracts = visible_modules(&node.imports, &graph)
            .into_iter()
            .collect::<Vec<_>>();
        contracts.sort_by(|left, right| left.0.cmp(&right.0));
        for (dependency, names) in &contracts {
            if let Some(hir) = checked.get(dependency) {
                pre_register_hir_into_tc(&mut checker, hir, Some(names), interner);
            }
        }
        checker.check_module(&node.module);
        let typed = checker.finish_typed();
        let valid = !typed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        diagnostics.extend(typed.diagnostics.iter().cloned());
        if valid {
            let mut hir = fidan_hir::lower_module(&node.module, &typed, interner);
            bind_hir_file_imports(
                &mut hir,
                &node.module,
                interner,
                source_map,
                path.parent().unwrap_or(base_dir),
                base_dir,
            );
            checked.insert(path.clone(), hir);
        }
    }
    let mut visible = visible_modules(&roots, &graph);
    let modules = order
        .into_iter()
        .rev()
        .filter_map(|path| {
            let hir = checked.remove(&path)?;
            let names = visible.remove(&path);
            let expose = names.is_some();
            Some((hir, names, expose))
        })
        .collect();
    LoadedImports {
        modules,
        diagnostics,
    }
}
