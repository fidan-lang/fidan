use anyhow::{Context, Result, bail};
use fidan_ast::{Expr, Item, Stmt};
use fidan_driver::{
    AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisCommand, AiAnalysisRequest, AiAnalysisResponse,
    AiAnalysisResult, AiDependency, AiDeterministicExplainLine, AiDiagnosticSummary,
    AiExplainContext, AiModuleOutline, AiOutlineItem, AiProjectSummary, AiSymbolInfo, AiSymbolRef,
    collect_file_import_paths,
};
use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::SourceMap;
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn handle_internal_request_from_stdio() -> Result<()> {
    let mut request_bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut request_bytes)
        .context("failed to read ai-analysis request from stdin")?;
    let request: AiAnalysisRequest =
        serde_json::from_slice(&request_bytes).context("failed to parse ai-analysis request")?;

    let response = if request.protocol_version != AI_ANALYSIS_PROTOCOL_VERSION {
        AiAnalysisResponse {
            protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
            success: false,
            result: None,
            error: Some(format!(
                "ai-analysis protocol mismatch (request={}, cli={})",
                request.protocol_version, AI_ANALYSIS_PROTOCOL_VERSION
            )),
        }
    } else {
        match handle_request(request.command) {
            Ok(result) => AiAnalysisResponse {
                protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
                success: true,
                result: Some(result),
                error: None,
            },
            Err(error) => AiAnalysisResponse {
                protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
                success: false,
                result: None,
                error: Some(format!("{error:#}")),
            },
        }
    };

    let response_bytes =
        serde_json::to_vec(&response).context("failed to serialize ai-analysis response")?;
    std::io::stdout()
        .write_all(&response_bytes)
        .context("failed to write ai-analysis response to stdout")?;
    Ok(())
}

pub(crate) fn handle_request(command: AiAnalysisCommand) -> Result<AiAnalysisResult> {
    match command {
        AiAnalysisCommand::ExplainContext {
            file,
            line_start,
            line_end,
        } => Ok(AiAnalysisResult::ExplainContext(analyze_explain_context(
            &file, line_start, line_end,
        )?)),
        AiAnalysisCommand::ModuleOutline { file } => Ok(AiAnalysisResult::ModuleOutline(
            analyze_module_outline(&file)?,
        )),
        AiAnalysisCommand::ProjectSummary { entry } => Ok(AiAnalysisResult::ProjectSummary(
            analyze_project_summary(&entry)?,
        )),
        AiAnalysisCommand::SymbolInfo { file, symbol } => Ok(AiAnalysisResult::SymbolInfo(
            analyze_symbol_info(&file, &symbol)?,
        )),
    }
}

struct ParsedFile {
    file: PathBuf,
    src: String,
    lines: Vec<String>,
    interner: Arc<SymbolInterner>,
    module: fidan_ast::Module,
    typed: fidan_typeck::TypedModule,
}

fn parse_file(file: &Path) -> Result<ParsedFile> {
    let src = std::fs::read_to_string(file).with_context(|| format!("cannot read {:?}", file))?;
    let interner = Arc::new(SymbolInterner::new());
    let source_map = Arc::new(SourceMap::new());
    let source_name = file.display().to_string();
    let f = source_map.add_file(&*source_name, &*src);
    let (tokens, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
    if !lex_diags.is_empty() {
        bail!("lex errors prevent ai-analysis");
    }
    let (module, parse_diags) = fidan_parser::parse(&tokens, f.id, Arc::clone(&interner));
    if !parse_diags.is_empty() {
        bail!("parse errors prevent ai-analysis");
    }
    let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    Ok(ParsedFile {
        file: file.to_path_buf(),
        src: src.clone(),
        lines: src.lines().map(str::to_string).collect(),
        interner,
        module,
        typed,
    })
}

pub(crate) fn analyze_explain_context(
    file: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<AiExplainContext> {
    let parsed = parse_file(file)?;
    let total_lines = parsed.lines.len().max(1);
    let line_start = line_start.unwrap_or(1).max(1).min(total_lines);
    let line_end = line_end.unwrap_or_else(|| {
        if line_start == 1 && line_end.is_none() {
            total_lines
        } else {
            line_start
        }
    });
    let line_end = line_end.max(line_start).min(total_lines);

    let selected_source = parsed.lines[line_start.saturating_sub(1)..line_end]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n");

    let deterministic_lines = collect_explain_lines(&parsed, line_start, line_end);
    let module_outline = build_outline(&parsed);
    let dependencies = build_dependencies(&parsed.module, &parsed.interner);
    let diagnostics = parsed
        .typed
        .diagnostics
        .iter()
        .filter_map(|diag| {
            let line = offset_line(&parsed.src, diag.span.start as usize);
            (line >= line_start && line <= line_end).then(|| AiDiagnosticSummary {
                severity: diag.severity.to_string(),
                code: diag.code.clone(),
                message: diag.message.clone(),
                line,
            })
        })
        .collect::<Vec<_>>();

    let mut symbol_names = BTreeSet::new();
    for line in &deterministic_lines {
        for read in &line.reads {
            symbol_names.insert(read.split(' ').next().unwrap_or(read).to_string());
        }
        for write in &line.writes {
            symbol_names.insert(write.clone());
        }
    }
    let related_symbols = build_outline(&parsed)
        .into_iter()
        .filter(|item| symbol_names.contains(&item.name))
        .map(|item| AiSymbolRef {
            name: item.name,
            kind: item.kind,
            file: parsed.file.clone(),
            line: item.line,
            snippet: parsed
                .lines
                .get(item.line.saturating_sub(1))
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            detail: item.detail,
        })
        .collect();

    Ok(AiExplainContext {
        file: file.to_path_buf(),
        line_start,
        line_end,
        total_lines,
        selected_source,
        deterministic_lines,
        module_outline,
        dependencies,
        related_symbols,
        diagnostics,
    })
}

pub(crate) fn analyze_module_outline(file: &Path) -> Result<AiModuleOutline> {
    let parsed = parse_file(file)?;
    Ok(AiModuleOutline {
        file: file.to_path_buf(),
        items: build_outline(&parsed),
        dependencies: build_dependencies(&parsed.module, &parsed.interner),
    })
}

pub(crate) fn analyze_project_summary(entry: &Path) -> Result<AiProjectSummary> {
    let parsed = parse_file(entry)?;
    let files = collect_project_files(entry)?;
    Ok(AiProjectSummary {
        entry: entry.to_path_buf(),
        file_count: files.len(),
        files,
        top_level_items: build_outline(&parsed),
        dependencies: build_dependencies(&parsed.module, &parsed.interner),
    })
}

pub(crate) fn analyze_symbol_info(file: &Path, symbol: &str) -> Result<AiSymbolInfo> {
    let parsed = parse_file(file)?;
    let matches = build_outline(&parsed)
        .into_iter()
        .filter(|item| item.name == symbol || item.name.contains(symbol))
        .map(|item| AiSymbolRef {
            name: item.name,
            kind: item.kind,
            file: file.to_path_buf(),
            line: item.line,
            snippet: parsed
                .lines
                .get(item.line.saturating_sub(1))
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            detail: item.detail,
        })
        .collect();
    Ok(AiSymbolInfo {
        file: file.to_path_buf(),
        symbol: symbol.to_string(),
        matches,
    })
}

fn build_outline(parsed: &ParsedFile) -> Vec<AiOutlineItem> {
    let mut items = Vec::new();
    for &iid in &parsed.module.items {
        match parsed.module.arena.get_item(iid) {
            Item::ActionDecl {
                name,
                params,
                body,
                span,
                ..
            } => items.push(AiOutlineItem {
                kind: "action".to_string(),
                name: parsed.interner.resolve(*name).to_string(),
                line: offset_line(&parsed.src, span.start as usize),
                detail: Some(summarize_action_like_body(
                    &format!("{} parameter(s)", params.len()),
                    body,
                    &parsed.module,
                    &parsed.interner,
                )),
            }),
            Item::ExtensionAction {
                name,
                extends,
                params,
                body,
                span,
                ..
            } => items.push(AiOutlineItem {
                kind: "extension_action".to_string(),
                name: parsed.interner.resolve(*name).to_string(),
                line: offset_line(&parsed.src, span.start as usize),
                detail: Some(summarize_action_like_body(
                    &format!(
                        "extends {} with {} parameter(s)",
                        parsed.interner.resolve(*extends),
                        params.len()
                    ),
                    body,
                    &parsed.module,
                    &parsed.interner,
                )),
            }),
            Item::ObjectDecl {
                name,
                fields,
                methods,
                span,
                ..
            } => items.push(AiOutlineItem {
                kind: "object".to_string(),
                name: parsed.interner.resolve(*name).to_string(),
                line: offset_line(&parsed.src, span.start as usize),
                detail: Some(format!(
                    "{} field(s), {} method(s)",
                    fields.len(),
                    methods.len()
                )),
            }),
            Item::VarDecl {
                name,
                is_const,
                span,
                ..
            } => items.push(AiOutlineItem {
                kind: if *is_const { "const" } else { "var" }.to_string(),
                name: parsed.interner.resolve(*name).to_string(),
                line: offset_line(&parsed.src, span.start as usize),
                detail: None,
            }),
            Item::Use {
                path,
                alias,
                re_export,
                span,
                ..
            } => items.push(AiOutlineItem {
                kind: if *re_export {
                    "re_export".to_string()
                } else {
                    "import".to_string()
                },
                name: path
                    .iter()
                    .map(|sym| parsed.interner.resolve(*sym).to_string())
                    .collect::<Vec<_>>()
                    .join("."),
                line: offset_line(&parsed.src, span.start as usize),
                detail: alias.map(|sym| format!("alias {}", parsed.interner.resolve(sym))),
            }),
            Item::TestDecl {
                name, body, span, ..
            } => items.push(AiOutlineItem {
                kind: "test".to_string(),
                name: name.clone(),
                line: offset_line(&parsed.src, span.start as usize),
                detail: Some(summarize_action_like_body(
                    "test block",
                    body,
                    &parsed.module,
                    &parsed.interner,
                )),
            }),
            Item::EnumDecl {
                name,
                variants,
                span,
                ..
            } => items.push(AiOutlineItem {
                kind: "enum".to_string(),
                name: parsed.interner.resolve(*name).to_string(),
                line: offset_line(&parsed.src, span.start as usize),
                detail: Some(format!("{} variant(s)", variants.len())),
            }),
            _ => {}
        }
    }
    items
}

fn summarize_action_like_body(
    prefix: &str,
    body: &[fidan_ast::StmtId],
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> String {
    let mut facts = BTreeSet::new();
    let mut queue = VecDeque::from(body.to_vec());

    while let Some(stmt_id) = queue.pop_front() {
        let stmt = module.arena.get_stmt(stmt_id);
        match stmt {
            Stmt::Expr { expr, .. } => {
                collect_expr_outline_facts(*expr, module, interner, &mut facts)
            }
            Stmt::Assign { value, .. } => {
                collect_expr_outline_facts(*value, module, interner, &mut facts);
                facts.insert("performs assignment".to_string());
            }
            Stmt::Return { value, .. } => {
                facts.insert("returns from the action".to_string());
                if let Some(value) = value {
                    collect_expr_outline_facts(*value, module, interner, &mut facts);
                }
            }
            Stmt::If { condition, .. } => {
                facts.insert("branches conditionally".to_string());
                collect_expr_outline_facts(*condition, module, interner, &mut facts);
            }
            Stmt::For { iterable, .. } => {
                facts.insert("iterates sequentially".to_string());
                collect_expr_outline_facts(*iterable, module, interner, &mut facts);
            }
            Stmt::ParallelFor { iterable, .. } => {
                facts.insert("iterates in parallel".to_string());
                collect_expr_outline_facts(*iterable, module, interner, &mut facts);
            }
            Stmt::While { condition, .. } => {
                facts.insert("loops while a condition holds".to_string());
                collect_expr_outline_facts(*condition, module, interner, &mut facts);
            }
            Stmt::Attempt { .. } => {
                facts.insert("handles errors with attempt/catch".to_string());
            }
            Stmt::ConcurrentBlock {
                is_parallel, tasks, ..
            } => {
                facts.insert(if *is_parallel {
                    format!("starts a parallel block with {} task(s)", tasks.len())
                } else {
                    format!("starts a concurrent block with {} task(s)", tasks.len())
                });
            }
            Stmt::Panic { value, .. } => {
                facts.insert("can panic".to_string());
                collect_expr_outline_facts(*value, module, interner, &mut facts);
            }
            Stmt::Check {
                scrutinee, arms, ..
            } => {
                facts.insert(format!("pattern-matches with {} arm(s)", arms.len()));
                collect_expr_outline_facts(*scrutinee, module, interner, &mut facts);
            }
            Stmt::Destructure { value, .. } => {
                facts.insert("destructures values".to_string());
                collect_expr_outline_facts(*value, module, interner, &mut facts);
            }
            Stmt::VarDecl { init, .. } => {
                if let Some(init) = init {
                    collect_expr_outline_facts(*init, module, interner, &mut facts);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Error { .. } => {}
        }

        queue.extend(child_stmt_ids(stmt));
    }

    if facts.is_empty() {
        prefix.to_string()
    } else {
        format!(
            "{prefix}; {}",
            facts.into_iter().collect::<Vec<_>>().join("; ")
        )
    }
}

fn collect_expr_outline_facts(
    eid: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    facts: &mut BTreeSet<String>,
) {
    match module.arena.get_expr(eid) {
        Expr::Call { callee, args, .. } => {
            facts.insert(format!(
                "calls {}",
                summarize_call_target(*callee, module, interner)
            ));
            collect_expr_outline_facts(*callee, module, interner, facts);
            for arg in args {
                collect_expr_outline_facts(arg.value, module, interner, facts);
            }
        }
        Expr::Spawn { expr, .. } => {
            facts.insert("spawns asynchronous work".to_string());
            collect_expr_outline_facts(*expr, module, interner, facts);
        }
        Expr::Await { expr, .. } => {
            facts.insert("awaits an asynchronous result".to_string());
            collect_expr_outline_facts(*expr, module, interner, facts);
        }
        Expr::StringInterp { .. } => {
            facts.insert("builds interpolated strings".to_string());
        }
        Expr::Index { object, index, .. } => {
            facts.insert("indexes into a collection".to_string());
            collect_expr_outline_facts(*object, module, interner, facts);
            collect_expr_outline_facts(*index, module, interner, facts);
        }
        Expr::Binary { lhs, rhs, .. } | Expr::NullCoalesce { lhs, rhs, .. } => {
            collect_expr_outline_facts(*lhs, module, interner, facts);
            collect_expr_outline_facts(*rhs, module, interner, facts);
        }
        Expr::Unary { operand, .. } => {
            collect_expr_outline_facts(*operand, module, interner, facts)
        }
        Expr::Field { object, .. } => collect_expr_outline_facts(*object, module, interner, facts),
        Expr::Assign { value, .. } | Expr::CompoundAssign { value, .. } => {
            collect_expr_outline_facts(*value, module, interner, facts);
        }
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            collect_expr_outline_facts(*condition, module, interner, facts);
            collect_expr_outline_facts(*then_val, module, interner, facts);
            collect_expr_outline_facts(*else_val, module, interner, facts);
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            for eid in elements {
                collect_expr_outline_facts(*eid, module, interner, facts);
            }
        }
        Expr::Dict { entries, .. } => {
            for (key, value) in entries {
                collect_expr_outline_facts(*key, module, interner, facts);
                collect_expr_outline_facts(*value, module, interner, facts);
            }
        }
        Expr::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            collect_expr_outline_facts(*target, module, interner, facts);
            if let Some(start) = start {
                collect_expr_outline_facts(*start, module, interner, facts);
            }
            if let Some(end) = end {
                collect_expr_outline_facts(*end, module, interner, facts);
            }
            if let Some(step) = step {
                collect_expr_outline_facts(*step, module, interner, facts);
            }
        }
        _ => {}
    }
}

fn summarize_call_target(
    callee: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> String {
    match module.arena.get_expr(callee) {
        Expr::Ident { name, .. } => format!("`{}`", interner.resolve(*name)),
        Expr::Field { field, .. } => format!("method `{}`", interner.resolve(*field)),
        Expr::Parent { .. } => "the parent constructor".to_string(),
        _ => "another callable expression".to_string(),
    }
}

fn build_dependencies(module: &fidan_ast::Module, interner: &SymbolInterner) -> Vec<AiDependency> {
    let mut deps = Vec::new();
    for &iid in &module.items {
        if let Item::Use {
            path,
            alias,
            re_export,
            ..
        } = module.arena.get_item(iid)
        {
            deps.push(AiDependency {
                path: path
                    .iter()
                    .map(|sym| interner.resolve(*sym).to_string())
                    .collect::<Vec<_>>()
                    .join("."),
                alias: alias.map(|sym| interner.resolve(sym).to_string()),
                is_re_export: *re_export,
            });
        }
    }
    deps
}

fn collect_project_files(entry: &Path) -> Result<Vec<PathBuf>> {
    let interner = Arc::new(SymbolInterner::new());
    let mut files = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::from([entry.to_path_buf()]);
    while let Some(path) = queue.pop_front() {
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !visited.insert(canon.clone()) {
            continue;
        }
        files.push(canon.clone());
        let src = std::fs::read_to_string(&canon)
            .with_context(|| format!("cannot read `{}`", canon.display()))?;
        let source_map = Arc::new(SourceMap::new());
        let display_name = canon.display().to_string();
        let file = source_map.add_file(&*display_name, &*src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, parse_diags) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
        if !parse_diags.is_empty() {
            continue;
        }
        let base_dir = canon.parent().unwrap_or_else(|| Path::new("."));
        let (imports, _) = collect_file_import_paths(&module, &interner, base_dir);
        for (import_path, _, _) in imports {
            queue.push_back(import_path);
        }
    }
    files.sort();
    Ok(files)
}

fn collect_explain_lines(
    parsed: &ParsedFile,
    line_start: usize,
    line_end: usize,
) -> Vec<AiDeterministicExplainLine> {
    let mut lines = Vec::new();
    let mut covered = BTreeSet::new();

    for &iid in &parsed.module.items {
        match parsed.module.arena.get_item(iid) {
            Item::ActionDecl {
                name,
                params,
                body,
                span,
                ..
            } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: format!(
                            "declares action `{}` with {} parameter(s)",
                            parsed.interner.resolve(*name),
                            params.len()
                        ),
                        inferred_type: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
                for sid in body {
                    collect_stmt_line(
                        parsed.module.arena.get_stmt(*sid),
                        parsed,
                        line_start,
                        line_end,
                        &mut covered,
                        &mut lines,
                    );
                }
            }
            Item::ExtensionAction {
                name,
                extends,
                body,
                span,
                ..
            } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: format!(
                            "declares extension action `{}` for `{}`",
                            parsed.interner.resolve(*name),
                            parsed.interner.resolve(*extends)
                        ),
                        inferred_type: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
                for sid in body {
                    collect_stmt_line(
                        parsed.module.arena.get_stmt(*sid),
                        parsed,
                        line_start,
                        line_end,
                        &mut covered,
                        &mut lines,
                    );
                }
            }
            Item::Use { path, span, .. } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: format!(
                            "imports namespace `{}`",
                            path.iter()
                                .map(|sym| parsed.interner.resolve(*sym).to_string())
                                .collect::<Vec<_>>()
                                .join(".")
                        ),
                        inferred_type: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
            }
            Item::VarDecl {
                name, init, span, ..
            } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    let reads = init
                        .map(|eid| collect_ident_names(eid, &parsed.module, &parsed.interner))
                        .unwrap_or_default();
                    let risks = init
                        .map(|eid| collect_risks(eid, &parsed.module))
                        .unwrap_or_default();
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: format!(
                            "declares module-level variable `{}`",
                            parsed.interner.resolve(*name)
                        ),
                        inferred_type: init
                            .and_then(|eid| parsed.typed.expr_types.get(&eid))
                            .map(type_name),
                        reads,
                        writes: vec![parsed.interner.resolve(*name).to_string()],
                        risks,
                    });
                }
            }
            Item::ObjectDecl {
                name,
                methods,
                span,
                ..
            } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: format!(
                            "declares object type `{}`",
                            parsed.interner.resolve(*name)
                        ),
                        inferred_type: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
                for method in methods {
                    match parsed.module.arena.get_item(*method) {
                        Item::ActionDecl {
                            name,
                            params,
                            body,
                            span,
                            ..
                        } => {
                            let line = offset_line(&parsed.src, span.start as usize);
                            if line >= line_start && line <= line_end && covered.insert(line) {
                                lines.push(AiDeterministicExplainLine {
                                    line,
                                    source: line_text(&parsed.lines, line),
                                    what_it_does: format!(
                                        "declares method `{}` with {} parameter(s)",
                                        parsed.interner.resolve(*name),
                                        params.len()
                                    ),
                                    inferred_type: None,
                                    reads: vec![],
                                    writes: vec![],
                                    risks: vec![],
                                });
                            }
                            for sid in body {
                                collect_stmt_line(
                                    parsed.module.arena.get_stmt(*sid),
                                    parsed,
                                    line_start,
                                    line_end,
                                    &mut covered,
                                    &mut lines,
                                );
                            }
                        }
                        Item::ExtensionAction {
                            name,
                            extends,
                            body,
                            span,
                            ..
                        } => {
                            let line = offset_line(&parsed.src, span.start as usize);
                            if line >= line_start && line <= line_end && covered.insert(line) {
                                lines.push(AiDeterministicExplainLine {
                                    line,
                                    source: line_text(&parsed.lines, line),
                                    what_it_does: format!(
                                        "declares extension method `{}` for `{}`",
                                        parsed.interner.resolve(*name),
                                        parsed.interner.resolve(*extends)
                                    ),
                                    inferred_type: None,
                                    reads: vec![],
                                    writes: vec![],
                                    risks: vec![],
                                });
                            }
                            for sid in body {
                                collect_stmt_line(
                                    parsed.module.arena.get_stmt(*sid),
                                    parsed,
                                    line_start,
                                    line_end,
                                    &mut covered,
                                    &mut lines,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            Item::Stmt(sid) => collect_stmt_line(
                parsed.module.arena.get_stmt(*sid),
                parsed,
                line_start,
                line_end,
                &mut covered,
                &mut lines,
            ),
            Item::ExprStmt(eid) => {
                let expr = parsed.module.arena.get_expr(*eid);
                let line = offset_line(&parsed.src, expr.span().start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: describe_expr(*eid, &parsed.module, &parsed.interner),
                        inferred_type: parsed.typed.expr_types.get(eid).map(type_name),
                        reads: collect_ident_names(*eid, &parsed.module, &parsed.interner),
                        writes: vec![],
                        risks: collect_risks(*eid, &parsed.module),
                    });
                }
            }
            Item::TestDecl { body, span, .. } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end && covered.insert(line) {
                    lines.push(AiDeterministicExplainLine {
                        line,
                        source: line_text(&parsed.lines, line),
                        what_it_does: "declares a test block".to_string(),
                        inferred_type: None,
                        reads: vec![],
                        writes: vec![],
                        risks: vec![],
                    });
                }
                for sid in body {
                    collect_stmt_line(
                        parsed.module.arena.get_stmt(*sid),
                        parsed,
                        line_start,
                        line_end,
                        &mut covered,
                        &mut lines,
                    );
                }
            }
            _ => {}
        }
    }

    lines.sort_by_key(|line| line.line);
    lines
}

fn collect_stmt_line(
    stmt: &Stmt,
    parsed: &ParsedFile,
    line_start: usize,
    line_end: usize,
    covered: &mut BTreeSet<usize>,
    lines: &mut Vec<AiDeterministicExplainLine>,
) {
    let span = stmt_span(stmt);
    let line = offset_line(&parsed.src, span.start as usize);
    if line >= line_start && line <= line_end && covered.insert(line) {
        let (what_it_does, inferred_type, reads, writes, risks) = match stmt {
            Stmt::VarDecl {
                name,
                init,
                is_const,
                ..
            } => (
                format!(
                    "declares {} `{}`",
                    if *is_const { "constant" } else { "variable" },
                    parsed.interner.resolve(*name)
                ),
                init.and_then(|eid| parsed.typed.expr_types.get(&eid))
                    .map(type_name),
                init.map(|eid| collect_ident_names(eid, &parsed.module, &parsed.interner))
                    .unwrap_or_default(),
                vec![parsed.interner.resolve(*name).to_string()],
                init.map(|eid| collect_risks(eid, &parsed.module))
                    .unwrap_or_default(),
            ),
            Stmt::Assign { target, value, .. } => (
                format!(
                    "sets `{}` to {}",
                    describe_expr(*target, &parsed.module, &parsed.interner),
                    describe_expr(*value, &parsed.module, &parsed.interner)
                ),
                parsed.typed.expr_types.get(value).map(type_name),
                collect_ident_names(*value, &parsed.module, &parsed.interner),
                collect_target_names(*target, &parsed.module, &parsed.interner),
                collect_risks(*value, &parsed.module),
            ),
            Stmt::Expr { expr, .. } => (
                describe_expr(*expr, &parsed.module, &parsed.interner),
                parsed.typed.expr_types.get(expr).map(type_name),
                collect_ident_names(*expr, &parsed.module, &parsed.interner),
                vec![],
                collect_risks(*expr, &parsed.module),
            ),
            Stmt::Return { value, .. } => (
                "returns from the current action".to_string(),
                value
                    .and_then(|eid| parsed.typed.expr_types.get(&eid))
                    .map(type_name),
                value
                    .map(|eid| collect_ident_names(eid, &parsed.module, &parsed.interner))
                    .unwrap_or_default(),
                vec![],
                value
                    .map(|eid| collect_risks(eid, &parsed.module))
                    .unwrap_or_default(),
            ),
            Stmt::If { condition, .. } => (
                format!(
                    "branches based on {}",
                    describe_expr(*condition, &parsed.module, &parsed.interner)
                ),
                None,
                collect_ident_names(*condition, &parsed.module, &parsed.interner),
                vec![],
                collect_risks(*condition, &parsed.module),
            ),
            Stmt::For {
                binding, iterable, ..
            } => (
                format!(
                    "iterates over {} and binds each element to `{}`",
                    describe_expr(*iterable, &parsed.module, &parsed.interner),
                    parsed.interner.resolve(*binding)
                ),
                None,
                collect_ident_names(*iterable, &parsed.module, &parsed.interner),
                vec![parsed.interner.resolve(*binding).to_string()],
                collect_risks(*iterable, &parsed.module),
            ),
            Stmt::ParallelFor {
                binding, iterable, ..
            } => (
                format!(
                    "parallel-iterates over {} and binds each element to `{}`",
                    describe_expr(*iterable, &parsed.module, &parsed.interner),
                    parsed.interner.resolve(*binding)
                ),
                None,
                collect_ident_names(*iterable, &parsed.module, &parsed.interner),
                vec![parsed.interner.resolve(*binding).to_string()],
                collect_risks(*iterable, &parsed.module),
            ),
            Stmt::While { condition, .. } => (
                format!(
                    "loops while {} is true",
                    describe_expr(*condition, &parsed.module, &parsed.interner)
                ),
                None,
                collect_ident_names(*condition, &parsed.module, &parsed.interner),
                vec![],
                collect_risks(*condition, &parsed.module),
            ),
            Stmt::Attempt { .. } => (
                "handles errors with an attempt/catch block".to_string(),
                None,
                vec![],
                vec![],
                vec![],
            ),
            Stmt::ConcurrentBlock {
                is_parallel, tasks, ..
            } => (
                format!(
                    "{} block with {} task(s)",
                    if *is_parallel {
                        "parallel"
                    } else {
                        "concurrent"
                    },
                    tasks.len()
                ),
                None,
                vec![],
                vec![],
                vec![],
            ),
            Stmt::Panic { value, .. } => (
                format!(
                    "panics with {}",
                    describe_expr(*value, &parsed.module, &parsed.interner)
                ),
                None,
                collect_ident_names(*value, &parsed.module, &parsed.interner),
                vec![],
                collect_risks(*value, &parsed.module),
            ),
            Stmt::Check {
                scrutinee, arms, ..
            } => (
                format!(
                    "pattern-matches on {} with {} arm(s)",
                    describe_expr(*scrutinee, &parsed.module, &parsed.interner),
                    arms.len()
                ),
                None,
                collect_ident_names(*scrutinee, &parsed.module, &parsed.interner),
                vec![],
                collect_risks(*scrutinee, &parsed.module),
            ),
            Stmt::Destructure {
                bindings, value, ..
            } => (
                "destructures a value into bindings".to_string(),
                None,
                collect_ident_names(*value, &parsed.module, &parsed.interner),
                bindings
                    .iter()
                    .map(|sym| parsed.interner.resolve(*sym).to_string())
                    .collect(),
                collect_risks(*value, &parsed.module),
            ),
            Stmt::Break { .. } => (
                "breaks out of the current loop".to_string(),
                None,
                vec![],
                vec![],
                vec![],
            ),
            Stmt::Continue { .. } => (
                "continues with the next loop iteration".to_string(),
                None,
                vec![],
                vec![],
                vec![],
            ),
            Stmt::Error { .. } => (
                "contains a parser recovery placeholder".to_string(),
                None,
                vec![],
                vec![],
                vec![],
            ),
        };
        lines.push(AiDeterministicExplainLine {
            line,
            source: line_text(&parsed.lines, line),
            what_it_does,
            inferred_type,
            reads,
            writes,
            risks,
        });
    }

    for sid in child_stmt_ids(stmt) {
        collect_stmt_line(
            parsed.module.arena.get_stmt(sid),
            parsed,
            line_start,
            line_end,
            covered,
            lines,
        );
    }
}

fn child_stmt_ids(stmt: &Stmt) -> Vec<fidan_ast::StmtId> {
    match stmt {
        Stmt::If {
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            let mut nested = then_body.clone();
            for branch in else_ifs {
                nested.extend_from_slice(&branch.body);
            }
            if let Some(body) = else_body {
                nested.extend_from_slice(body);
            }
            nested
        }
        Stmt::For { body, .. } | Stmt::ParallelFor { body, .. } | Stmt::While { body, .. } => {
            body.clone()
        }
        Stmt::Attempt {
            body,
            catches,
            otherwise,
            finally,
            ..
        } => {
            let mut nested = body.clone();
            for catch in catches {
                nested.extend_from_slice(&catch.body);
            }
            if let Some(body) = otherwise {
                nested.extend_from_slice(body);
            }
            if let Some(body) = finally {
                nested.extend_from_slice(body);
            }
            nested
        }
        Stmt::ConcurrentBlock { tasks, .. } => {
            tasks.iter().flat_map(|task| task.body.clone()).collect()
        }
        Stmt::Check { arms, .. } => arms.iter().flat_map(|arm| arm.body.clone()).collect(),
        _ => vec![],
    }
}

fn stmt_span(stmt: &Stmt) -> fidan_source::Span {
    match stmt {
        Stmt::VarDecl { span, .. }
        | Stmt::Destructure { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Expr { span, .. }
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

fn offset_line(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())]
        .chars()
        .filter(|&ch| ch == '\n')
        .count()
        + 1
}

fn line_text(lines: &[String], line: usize) -> String {
    lines
        .get(line.saturating_sub(1))
        .map(String::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn describe_expr(
    eid: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> String {
    match module.arena.get_expr(eid) {
        Expr::IntLit { value, .. } => format!("integer literal `{value}`"),
        Expr::FloatLit { value, .. } => format!("float literal `{value}`"),
        Expr::StrLit { value, .. } => format!("string literal `\"{value}\"`"),
        Expr::BoolLit { value, .. } => format!("boolean `{value}`"),
        Expr::Nothing { .. } => "`nothing`".to_string(),
        Expr::Ident { name, .. } => format!("`{}`", interner.resolve(*name)),
        Expr::Call { callee, args, .. } => describe_call(*callee, args, module, interner),
        Expr::Field { object, field, .. } => format!(
            "{}.{}",
            describe_expr(*object, module, interner),
            interner.resolve(*field)
        ),
        Expr::Index { object, index, .. } => format!(
            "{}[{}]",
            describe_expr(*object, module, interner),
            describe_expr(*index, module, interner)
        ),
        Expr::Binary { op, lhs, rhs, .. } => format!(
            "{} {} {}",
            describe_expr(*lhs, module, interner),
            match op {
                fidan_ast::BinOp::Add => "+",
                fidan_ast::BinOp::Sub => "-",
                fidan_ast::BinOp::Mul => "*",
                fidan_ast::BinOp::Div => "/",
                fidan_ast::BinOp::Rem => "%",
                fidan_ast::BinOp::Pow => "**",
                _ => "op",
            },
            describe_expr(*rhs, module, interner)
        ),
        Expr::Spawn { expr, .. } => format!("spawns {}", describe_expr(*expr, module, interner)),
        Expr::Await { expr, .. } => format!("awaits {}", describe_expr(*expr, module, interner)),
        Expr::StringInterp { .. } => "builds a string using interpolation".to_string(),
        Expr::List { elements, .. } => format!("list literal with {} element(s)", elements.len()),
        Expr::Dict { entries, .. } => format!("dict literal with {} entr(y/ies)", entries.len()),
        Expr::Tuple { elements, .. } => format!("tuple with {} element(s)", elements.len()),
        Expr::Ternary { .. } => "conditional expression".to_string(),
        Expr::This { .. } => "the current object (`this`)".to_string(),
        Expr::Parent { .. } => "the parent constructor (`parent`)".to_string(),
        _ => "(expression)".to_string(),
    }
}

fn describe_call(
    callee: fidan_ast::ExprId,
    args: &[fidan_ast::Arg],
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> String {
    let arg_phrase = if args.is_empty() {
        "with no arguments".to_string()
    } else if args.len() == 1 {
        format!("passing {}", describe_expr(args[0].value, module, interner))
    } else {
        format!("with {} arguments", args.len())
    };
    match module.arena.get_expr(callee) {
        Expr::Parent { .. } => format!("calls the parent constructor, {arg_phrase}"),
        Expr::Ident { name, .. } => {
            if args.is_empty() {
                format!("calls `{}`", interner.resolve(*name))
            } else {
                format!("calls `{}`, {arg_phrase}", interner.resolve(*name))
            }
        }
        Expr::Field { object, field, .. } => format!(
            "calls method `{}` on {}, {arg_phrase}",
            interner.resolve(*field),
            describe_expr(*object, module, interner)
        ),
        _ => format!(
            "calls {}, {arg_phrase}",
            describe_expr(callee, module, interner)
        ),
    }
}

fn collect_ident_names(
    eid: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> Vec<String> {
    let mut names = BTreeSet::new();
    collect_ident_names_inner(eid, module, interner, &mut names);
    names.into_iter().collect()
}

fn collect_ident_names_inner(
    eid: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    out: &mut BTreeSet<String>,
) {
    match module.arena.get_expr(eid) {
        Expr::Ident { name, .. } => {
            out.insert(interner.resolve(*name).to_string());
        }
        Expr::Binary { lhs, rhs, .. } | Expr::NullCoalesce { lhs, rhs, .. } => {
            collect_ident_names_inner(*lhs, module, interner, out);
            collect_ident_names_inner(*rhs, module, interner, out);
        }
        Expr::Unary { operand, .. } => collect_ident_names_inner(*operand, module, interner, out),
        Expr::Call { callee, args, .. } => {
            collect_ident_names_inner(*callee, module, interner, out);
            for arg in args {
                collect_ident_names_inner(arg.value, module, interner, out);
            }
        }
        Expr::Field { object, .. } => collect_ident_names_inner(*object, module, interner, out),
        Expr::Index { object, index, .. } => {
            collect_ident_names_inner(*object, module, interner, out);
            collect_ident_names_inner(*index, module, interner, out);
        }
        Expr::Assign { value, .. } | Expr::CompoundAssign { value, .. } => {
            collect_ident_names_inner(*value, module, interner, out);
        }
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            collect_ident_names_inner(*condition, module, interner, out);
            collect_ident_names_inner(*then_val, module, interner, out);
            collect_ident_names_inner(*else_val, module, interner, out);
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            for eid in elements {
                collect_ident_names_inner(*eid, module, interner, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (key, value) in entries {
                collect_ident_names_inner(*key, module, interner, out);
                collect_ident_names_inner(*value, module, interner, out);
            }
        }
        Expr::StringInterp { parts, .. } => {
            for part in parts {
                if let fidan_ast::InterpPart::Expr(eid) = part {
                    collect_ident_names_inner(*eid, module, interner, out);
                }
            }
        }
        Expr::Spawn { expr, .. } | Expr::Await { expr, .. } => {
            collect_ident_names_inner(*expr, module, interner, out);
        }
        _ => {}
    }
}

fn collect_target_names(
    eid: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> Vec<String> {
    let mut names = Vec::new();
    match module.arena.get_expr(eid) {
        Expr::Ident { name, .. } => names.push(interner.resolve(*name).to_string()),
        Expr::Field { object, .. } | Expr::Index { object, .. } => {
            names.extend(collect_target_names(*object, module, interner))
        }
        _ => {}
    }
    names
}

fn collect_risks(eid: fidan_ast::ExprId, module: &fidan_ast::Module) -> Vec<String> {
    let mut risks = BTreeSet::new();
    collect_risks_inner(eid, module, &mut risks);
    risks.into_iter().collect()
}

fn collect_risks_inner(
    eid: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    out: &mut BTreeSet<String>,
) {
    match module.arena.get_expr(eid) {
        Expr::Binary { op, lhs, rhs, .. } => {
            match op {
                fidan_ast::BinOp::Div | fidan_ast::BinOp::Rem => {
                    out.insert("division or modulo by zero".to_string());
                }
                fidan_ast::BinOp::Add
                | fidan_ast::BinOp::Sub
                | fidan_ast::BinOp::Mul
                | fidan_ast::BinOp::Pow => {
                    out.insert("integer overflow on very large values".to_string());
                }
                _ => {}
            }
            collect_risks_inner(*lhs, module, out);
            collect_risks_inner(*rhs, module, out);
        }
        Expr::Index { object, index, .. } => {
            out.insert("index out of bounds".to_string());
            collect_risks_inner(*object, module, out);
            collect_risks_inner(*index, module, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_risks_inner(*callee, module, out);
            for arg in args {
                collect_risks_inner(arg.value, module, out);
            }
        }
        Expr::Assign { value, .. } | Expr::CompoundAssign { value, .. } => {
            collect_risks_inner(*value, module, out);
        }
        _ => {}
    }
}

fn type_name(ty: &fidan_typeck::FidanType) -> String {
    ty.to_string()
}
