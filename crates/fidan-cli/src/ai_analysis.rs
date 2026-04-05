use anyhow::{Context, Result, bail};
use fidan_ast::{Expr, ExprId, Item, Stmt};
use fidan_driver::{
    AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisCommand, AiAnalysisRequest, AiAnalysisResponse,
    AiAnalysisResult, AiCallGraph, AiCallNode, AiDependency, AiDeterministicExplainLine,
    AiDiagnosticSummary, AiExplainContext, AiModuleOutline, AiOutlineItem, AiProjectSummary,
    AiRuntimeTrace, AiSymbolInfo, AiSymbolRef, AiTraceStep, AiTypeMap, AiTypedBinding,
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
        AiAnalysisCommand::CallGraph { file } => {
            Ok(AiAnalysisResult::CallGraph(analyze_call_graph(&file)?))
        }
        AiAnalysisCommand::TypeMap { file } => {
            Ok(AiAnalysisResult::TypeMap(analyze_type_map(&file)?))
        }
        AiAnalysisCommand::RuntimeTrace {
            file,
            line_start,
            line_end,
        } => Ok(AiAnalysisResult::RuntimeTrace(analyze_runtime_trace(
            &file, line_start, line_end,
        )?)),
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
    parse_source(file, src)
}

fn parse_source(file: &Path, src: String) -> Result<ParsedFile> {
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
    Ok(build_explain_context(parsed, file, line_start, line_end))
}

pub(crate) fn analyze_explain_context_from_source(
    file: &Path,
    source: &str,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<AiExplainContext> {
    let parsed = parse_source(file, source.to_string())?;
    Ok(build_explain_context(parsed, file, line_start, line_end))
}

fn build_explain_context(
    parsed: ParsedFile,
    file: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> AiExplainContext {
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
    let related_symbols = module_outline
        .iter()
        .filter(|&item| symbol_names.contains(&item.name))
        .cloned()
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

    AiExplainContext {
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
        call_graph: build_call_graph(&parsed),
        type_map: build_type_map(&parsed),
        runtime_trace: Some(build_static_trace(&parsed, line_start, line_end)),
    }
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

pub(crate) fn analyze_call_graph(file: &Path) -> Result<AiCallGraph> {
    let parsed = parse_file(file)?;
    Ok(AiCallGraph {
        nodes: build_call_graph(&parsed),
    })
}

pub(crate) fn analyze_type_map(file: &Path) -> Result<AiTypeMap> {
    let parsed = parse_file(file)?;
    Ok(AiTypeMap {
        bindings: build_type_map(&parsed),
    })
}

pub(crate) fn analyze_runtime_trace(
    file: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<AiRuntimeTrace> {
    let parsed = parse_file(file)?;
    let total_lines = parsed.lines.len().max(1);
    let line_start = line_start.unwrap_or(1).max(1).min(total_lines);
    let line_end = line_end
        .unwrap_or(total_lines)
        .max(line_start)
        .min(total_lines);
    Ok(build_static_trace(&parsed, line_start, line_end))
}

// ── Call graph ────────────────────────────────────────────────────────────────

/// Walk every action/method in the module and record which callees each one
/// invokes.  The caller name is `"ObjectName::method"` for methods and just
/// `"name"` for top-level actions.
fn build_call_graph(parsed: &ParsedFile) -> Vec<AiCallNode> {
    let mut nodes = Vec::new();
    for &iid in &parsed.module.items {
        match parsed.module.arena.get_item(iid) {
            Item::ActionDecl {
                name,
                params,
                body,
                span,
                ..
            } => {
                let caller = parsed.interner.resolve(*name).to_string();
                let callees = collect_callees_from_stmts(body, &parsed.module, &parsed.interner);
                let is_recursive = callees.contains(&caller);
                nodes.push(AiCallNode {
                    caller,
                    callees,
                    line: offset_line(&parsed.src, span.start as usize),
                    is_recursive,
                });
                // Also emit params count for context but skip — not needed.
                let _ = params;
            }
            Item::ExtensionAction {
                name,
                extends,
                params,
                body,
                span,
                ..
            } => {
                let caller = format!(
                    "{}::{}",
                    parsed.interner.resolve(*extends),
                    parsed.interner.resolve(*name)
                );
                let callees = collect_callees_from_stmts(body, &parsed.module, &parsed.interner);
                let is_recursive = callees.contains(&caller)
                    || callees.contains(&parsed.interner.resolve(*name).to_string());
                nodes.push(AiCallNode {
                    caller,
                    callees,
                    line: offset_line(&parsed.src, span.start as usize),
                    is_recursive,
                });
                let _ = params;
            }
            Item::ObjectDecl { methods, .. } => {
                for &mid in methods {
                    match parsed.module.arena.get_item(mid) {
                        Item::ActionDecl {
                            name,
                            params,
                            body,
                            span,
                            ..
                        } => {
                            let caller = parsed.interner.resolve(*name).to_string();
                            let callees =
                                collect_callees_from_stmts(body, &parsed.module, &parsed.interner);
                            let is_recursive = callees.contains(&caller);
                            nodes.push(AiCallNode {
                                caller,
                                callees,
                                line: offset_line(&parsed.src, span.start as usize),
                                is_recursive,
                            });
                            let _ = params;
                        }
                        Item::ExtensionAction {
                            name,
                            extends,
                            params,
                            body,
                            span,
                            ..
                        } => {
                            let caller = format!(
                                "{}::{}",
                                parsed.interner.resolve(*extends),
                                parsed.interner.resolve(*name)
                            );
                            let callees =
                                collect_callees_from_stmts(body, &parsed.module, &parsed.interner);
                            let is_recursive = callees.contains(&caller)
                                || callees.contains(&parsed.interner.resolve(*name).to_string());
                            nodes.push(AiCallNode {
                                caller,
                                callees,
                                line: offset_line(&parsed.src, span.start as usize),
                                is_recursive,
                            });
                            let _ = params;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    nodes
}

/// Recursively collect all call target names from a list of statement ids.
fn collect_callees_from_stmts(
    stmts: &[fidan_ast::StmtId],
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from(stmts.to_vec());
    while let Some(sid) = queue.pop_front() {
        let stmt = module.arena.get_stmt(sid);
        collect_callees_from_stmts_inner(stmt, module, interner, &mut seen, &mut queue);
    }
    seen.into_iter().collect()
}

fn collect_callees_from_stmts_inner(
    stmt: &Stmt,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    seen: &mut BTreeSet<String>,
    queue: &mut VecDeque<fidan_ast::StmtId>,
) {
    match stmt {
        Stmt::Expr { expr, .. }
        | Stmt::Return {
            value: Some(expr), ..
        } => {
            collect_callees_from_expr(*expr, module, interner, seen);
        }
        Stmt::Assign { value, .. } => {
            collect_callees_from_expr(*value, module, interner, seen);
        }
        Stmt::VarDecl {
            init: Some(init), ..
        } => {
            collect_callees_from_expr(*init, module, interner, seen);
        }
        _ => {}
    }
    queue.extend(child_stmt_ids(stmt));
}

fn collect_callees_from_expr(
    eid: ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
    seen: &mut BTreeSet<String>,
) {
    match module.arena.get_expr(eid) {
        Expr::Call { callee, args, .. } => {
            seen.insert(raw_callee_name(*callee, module, interner));
            for arg in args {
                collect_callees_from_expr(arg.value, module, interner, seen);
            }
            collect_callees_from_expr(*callee, module, interner, seen);
        }
        Expr::Field { object, .. } => collect_callees_from_expr(*object, module, interner, seen),
        Expr::Index { object, index, .. } => {
            collect_callees_from_expr(*object, module, interner, seen);
            collect_callees_from_expr(*index, module, interner, seen);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_callees_from_expr(*lhs, module, interner, seen);
            collect_callees_from_expr(*rhs, module, interner, seen);
        }
        Expr::Unary { operand, .. } => {
            collect_callees_from_expr(*operand, module, interner, seen);
        }
        Expr::NullCoalesce { lhs, rhs, .. } => {
            collect_callees_from_expr(*lhs, module, interner, seen);
            collect_callees_from_expr(*rhs, module, interner, seen);
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            for &el in elements {
                collect_callees_from_expr(el, module, interner, seen);
            }
        }
        Expr::Assign { value, .. } | Expr::CompoundAssign { value, .. } => {
            collect_callees_from_expr(*value, module, interner, seen);
        }
        Expr::Ternary {
            condition,
            then_val,
            else_val,
            ..
        } => {
            collect_callees_from_expr(*condition, module, interner, seen);
            collect_callees_from_expr(*then_val, module, interner, seen);
            collect_callees_from_expr(*else_val, module, interner, seen);
        }
        Expr::StringInterp { parts, .. } => {
            for part in parts {
                if let fidan_ast::InterpPart::Expr(eid) = part {
                    collect_callees_from_expr(*eid, module, interner, seen);
                }
            }
        }
        Expr::Spawn { expr, .. } | Expr::Await { expr, .. } => {
            collect_callees_from_expr(*expr, module, interner, seen);
        }
        Expr::Dict { entries, .. } => {
            for (key, value) in entries {
                collect_callees_from_expr(*key, module, interner, seen);
                collect_callees_from_expr(*value, module, interner, seen);
            }
        }
        Expr::Slice {
            target,
            start,
            end,
            step,
            ..
        } => {
            collect_callees_from_expr(*target, module, interner, seen);
            if let Some(s) = start {
                collect_callees_from_expr(*s, module, interner, seen);
            }
            if let Some(e) = end {
                collect_callees_from_expr(*e, module, interner, seen);
            }
            if let Some(st) = step {
                collect_callees_from_expr(*st, module, interner, seen);
            }
        }
        Expr::ListComp {
            element,
            iterable,
            filter,
            ..
        } => {
            collect_callees_from_expr(*element, module, interner, seen);
            collect_callees_from_expr(*iterable, module, interner, seen);
            if let Some(f) = filter {
                collect_callees_from_expr(*f, module, interner, seen);
            }
        }
        Expr::DictComp {
            key,
            value,
            iterable,
            filter,
            ..
        } => {
            collect_callees_from_expr(*key, module, interner, seen);
            collect_callees_from_expr(*value, module, interner, seen);
            collect_callees_from_expr(*iterable, module, interner, seen);
            if let Some(f) = filter {
                collect_callees_from_expr(*f, module, interner, seen);
            }
        }
        Expr::Lambda { body, .. } => {
            seen.extend(collect_callees_from_stmts(body, module, interner));
        }
        _ => {}
    }
}

// ── Type map ──────────────────────────────────────────────────────────────────

/// Walk every variable / parameter declaration in every action and collect
/// inferred types from the type-checker output.
fn build_type_map(parsed: &ParsedFile) -> Vec<AiTypedBinding> {
    let mut bindings = Vec::new();
    for &iid in &parsed.module.items {
        collect_type_map_from_item(iid, parsed, &mut bindings);
    }
    bindings
}

fn collect_type_map_from_item(
    iid: fidan_ast::ItemId,
    parsed: &ParsedFile,
    bindings: &mut Vec<AiTypedBinding>,
) {
    match parsed.module.arena.get_item(iid) {
        Item::VarDecl {
            name,
            init,
            is_const,
            span,
            ..
        } => {
            let inferred = init
                .and_then(|eid| parsed.typed.expr_types.get(&eid))
                .map(type_name)
                .unwrap_or_else(|| "unknown".to_string());
            bindings.push(AiTypedBinding {
                name: parsed.interner.resolve(*name).to_string(),
                inferred_type: inferred,
                line: offset_line(&parsed.src, span.start as usize),
                kind: if *is_const { "const" } else { "var" }.to_string(),
            });
        }
        Item::ActionDecl { params, body, .. } => {
            // Parameters
            for param in params {
                bindings.push(AiTypedBinding {
                    name: parsed.interner.resolve(param.name).to_string(),
                    inferred_type: type_expr_name(&param.ty, &parsed.interner),
                    line: 0, // params don't have their own span in the AST
                    kind: "param".to_string(),
                });
            }
            collect_type_map_from_stmts(body, parsed, bindings);
        }
        Item::ExtensionAction { params, body, .. } => {
            for param in params {
                bindings.push(AiTypedBinding {
                    name: parsed.interner.resolve(param.name).to_string(),
                    inferred_type: type_expr_name(&param.ty, &parsed.interner),
                    line: 0,
                    kind: "param".to_string(),
                });
            }
            collect_type_map_from_stmts(body, parsed, bindings);
        }
        Item::ObjectDecl { methods, .. } => {
            for &mid in methods {
                collect_type_map_from_item(mid, parsed, bindings);
            }
        }
        _ => {}
    }
}

fn collect_type_map_from_stmts(
    stmts: &[fidan_ast::StmtId],
    parsed: &ParsedFile,
    bindings: &mut Vec<AiTypedBinding>,
) {
    let mut queue = VecDeque::from(stmts.to_vec());
    while let Some(sid) = queue.pop_front() {
        let stmt = parsed.module.arena.get_stmt(sid);
        if let Stmt::VarDecl {
            name,
            init,
            is_const,
            span,
            ..
        } = stmt
        {
            let inferred = init
                .and_then(|eid| parsed.typed.expr_types.get(&eid))
                .map(type_name)
                .unwrap_or_else(|| "unknown".to_string());
            bindings.push(AiTypedBinding {
                name: parsed.interner.resolve(*name).to_string(),
                inferred_type: inferred,
                line: offset_line(&parsed.src, span.start as usize),
                kind: if *is_const { "const" } else { "var" }.to_string(),
            });
        }
        queue.extend(child_stmt_ids(stmt));
    }
}

/// Convert a `TypeExpr` AST node to a display string.
fn type_expr_name(ty: &fidan_ast::TypeExpr, interner: &SymbolInterner) -> String {
    match ty {
        fidan_ast::TypeExpr::Named { name, .. } => interner.resolve(*name).to_string(),
        fidan_ast::TypeExpr::Oftype { base, param, .. } => format!(
            "{} of {}",
            type_expr_name(base, interner),
            type_expr_name(param, interner)
        ),
        fidan_ast::TypeExpr::Tuple { elements, .. } => {
            if elements.is_empty() {
                "tuple".to_string()
            } else {
                format!(
                    "({})",
                    elements
                        .iter()
                        .map(|e| type_expr_name(e, interner))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        fidan_ast::TypeExpr::Dynamic { .. } => "dynamic".to_string(),
        fidan_ast::TypeExpr::Nothing { .. } => "nothing".to_string(),
    }
}

// ── Static execution trace ────────────────────────────────────────────────────

/// Build a static execution trace for the selected source range.  This walks
/// statements in order and emits one `AiTraceStep` per significant statement,
/// capping at 250 steps.
fn build_static_trace(parsed: &ParsedFile, line_start: usize, line_end: usize) -> AiRuntimeTrace {
    const MAX_STEPS: usize = 250;
    let mut steps: Vec<AiTraceStep> = Vec::new();
    let mut truncated = false;

    for &iid in &parsed.module.items {
        if truncated {
            break;
        }
        match parsed.module.arena.get_item(iid) {
            Item::ActionDecl {
                name, body, span, ..
            } => {
                let action_line = offset_line(&parsed.src, span.start as usize);
                if action_line >= line_start && action_line <= line_end {
                    steps.push(AiTraceStep {
                        kind: "call".to_string(),
                        description: format!("enter action `{}`", parsed.interner.resolve(*name)),
                        line: Some(action_line),
                        value: None,
                    });
                    if steps.len() >= MAX_STEPS {
                        truncated = true;
                        break;
                    }
                }
                collect_trace_from_stmts(
                    body,
                    parsed,
                    line_start,
                    line_end,
                    &mut steps,
                    &mut truncated,
                    MAX_STEPS,
                );
            }
            Item::ExtensionAction {
                name,
                extends,
                body,
                span,
                ..
            } => {
                let action_line = offset_line(&parsed.src, span.start as usize);
                if action_line >= line_start && action_line <= line_end {
                    steps.push(AiTraceStep {
                        kind: "call".to_string(),
                        description: format!(
                            "enter extension action `{}` for `{}`",
                            parsed.interner.resolve(*name),
                            parsed.interner.resolve(*extends)
                        ),
                        line: Some(action_line),
                        value: None,
                    });
                    if steps.len() >= MAX_STEPS {
                        truncated = true;
                        break;
                    }
                }
                collect_trace_from_stmts(
                    body,
                    parsed,
                    line_start,
                    line_end,
                    &mut steps,
                    &mut truncated,
                    MAX_STEPS,
                );
            }
            Item::VarDecl {
                name, init, span, ..
            } => {
                let line = offset_line(&parsed.src, span.start as usize);
                if line >= line_start && line <= line_end {
                    let value_hint = init
                        .and_then(|eid| literal_value_hint(eid, &parsed.module))
                        .or_else(|| {
                            init.and_then(|eid| parsed.typed.expr_types.get(&eid))
                                .map(type_name)
                        });
                    steps.push(AiTraceStep {
                        kind: "assign".to_string(),
                        description: format!(
                            "initialize module variable `{}`",
                            parsed.interner.resolve(*name)
                        ),
                        line: Some(line),
                        value: value_hint,
                    });
                    if steps.len() >= MAX_STEPS {
                        truncated = true;
                    }
                }
            }
            Item::Stmt(sid) => {
                collect_trace_from_stmts(
                    &[*sid],
                    parsed,
                    line_start,
                    line_end,
                    &mut steps,
                    &mut truncated,
                    MAX_STEPS,
                );
            }
            _ => {}
        }
    }

    AiRuntimeTrace { steps, truncated }
}

fn collect_trace_from_stmts(
    stmts: &[fidan_ast::StmtId],
    parsed: &ParsedFile,
    line_start: usize,
    line_end: usize,
    steps: &mut Vec<AiTraceStep>,
    truncated: &mut bool,
    max_steps: usize,
) {
    for &sid in stmts {
        if *truncated {
            return;
        }
        collect_trace_from_stmt(
            sid, parsed, line_start, line_end, steps, truncated, max_steps,
        );
    }
}

fn collect_trace_from_stmt(
    sid: fidan_ast::StmtId,
    parsed: &ParsedFile,
    line_start: usize,
    line_end: usize,
    steps: &mut Vec<AiTraceStep>,
    truncated: &mut bool,
    max_steps: usize,
) {
    let stmt = parsed.module.arena.get_stmt(sid);
    let span = stmt_span(stmt);
    let line = offset_line(&parsed.src, span.start as usize);
    if line < line_start || line > line_end {
        // Recurse into children regardless — they might be in range.
        for child in child_stmt_ids(stmt) {
            if *truncated {
                return;
            }
            collect_trace_from_stmt(
                child, parsed, line_start, line_end, steps, truncated, max_steps,
            );
        }
        return;
    }

    let step = match stmt {
        Stmt::VarDecl {
            name,
            init,
            is_const,
            ..
        } => {
            let value_hint = init
                .and_then(|eid| literal_value_hint(eid, &parsed.module))
                .or_else(|| {
                    init.and_then(|eid| parsed.typed.expr_types.get(&eid))
                        .map(type_name)
                });
            AiTraceStep {
                kind: "assign".to_string(),
                description: format!(
                    "{} `{}` = {}",
                    if *is_const { "const" } else { "var" },
                    parsed.interner.resolve(*name),
                    init.map(|eid| describe_expr(eid, &parsed.module, &parsed.interner))
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                line: Some(line),
                value: value_hint,
            }
        }
        Stmt::Assign { target, value, .. } => AiTraceStep {
            kind: "assign".to_string(),
            description: format!(
                "set `{}` = {}",
                describe_expr(*target, &parsed.module, &parsed.interner),
                describe_expr(*value, &parsed.module, &parsed.interner)
            ),
            line: Some(line),
            value: literal_value_hint(*value, &parsed.module),
        },
        Stmt::Return { value, .. } => AiTraceStep {
            kind: "return".to_string(),
            description: match value {
                Some(eid) => format!(
                    "return {}",
                    describe_expr(*eid, &parsed.module, &parsed.interner)
                ),
                None => "return (no value)".to_string(),
            },
            line: Some(line),
            value: value.and_then(|eid| literal_value_hint(eid, &parsed.module)),
        },
        Stmt::Expr { expr, .. } => AiTraceStep {
            kind: if matches!(parsed.module.arena.get_expr(*expr), Expr::Call { .. }) {
                "call".to_string()
            } else {
                "other".to_string()
            },
            description: describe_expr(*expr, &parsed.module, &parsed.interner),
            line: Some(line),
            value: None,
        },
        Stmt::If { condition, .. } => AiTraceStep {
            kind: "branch".to_string(),
            description: format!(
                "if {}",
                describe_expr(*condition, &parsed.module, &parsed.interner)
            ),
            line: Some(line),
            value: None,
        },
        Stmt::For {
            binding, iterable, ..
        } => AiTraceStep {
            kind: "loop".to_string(),
            description: format!(
                "for `{}` in {}",
                parsed.interner.resolve(*binding),
                describe_expr(*iterable, &parsed.module, &parsed.interner)
            ),
            line: Some(line),
            value: None,
        },
        Stmt::ParallelFor {
            binding, iterable, ..
        } => AiTraceStep {
            kind: "loop".to_string(),
            description: format!(
                "parallel for `{}` in {}",
                parsed.interner.resolve(*binding),
                describe_expr(*iterable, &parsed.module, &parsed.interner)
            ),
            line: Some(line),
            value: None,
        },
        Stmt::While { condition, .. } => AiTraceStep {
            kind: "loop".to_string(),
            description: format!(
                "while {}",
                describe_expr(*condition, &parsed.module, &parsed.interner)
            ),
            line: Some(line),
            value: None,
        },
        Stmt::ConcurrentBlock {
            is_parallel, tasks, ..
        } => AiTraceStep {
            kind: "concurrent".to_string(),
            description: format!(
                "{} block with {} task(s)",
                if *is_parallel {
                    "parallel"
                } else {
                    "concurrent"
                },
                tasks.len()
            ),
            line: Some(line),
            value: None,
        },
        Stmt::Panic { value, .. } => AiTraceStep {
            kind: "panic".to_string(),
            description: format!(
                "panic: {}",
                describe_expr(*value, &parsed.module, &parsed.interner)
            ),
            line: Some(line),
            value: literal_value_hint(*value, &parsed.module),
        },
        Stmt::Attempt { .. } => AiTraceStep {
            kind: "other".to_string(),
            description: "attempt/catch error-handling block".to_string(),
            line: Some(line),
            value: None,
        },
        Stmt::Check {
            scrutinee, arms, ..
        } => AiTraceStep {
            kind: "branch".to_string(),
            description: format!(
                "check {} ({} arm(s))",
                describe_expr(*scrutinee, &parsed.module, &parsed.interner),
                arms.len()
            ),
            line: Some(line),
            value: None,
        },
        Stmt::Destructure {
            bindings, value, ..
        } => AiTraceStep {
            kind: "assign".to_string(),
            description: format!(
                "destructure {} into ({})",
                describe_expr(*value, &parsed.module, &parsed.interner),
                bindings
                    .iter()
                    .map(|sym| parsed.interner.resolve(*sym).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            line: Some(line),
            value: None,
        },
        Stmt::Break { .. } => AiTraceStep {
            kind: "other".to_string(),
            description: "break out of loop".to_string(),
            line: Some(line),
            value: None,
        },
        Stmt::Continue { .. } => AiTraceStep {
            kind: "other".to_string(),
            description: "continue to next iteration".to_string(),
            line: Some(line),
            value: None,
        },
        Stmt::Error { .. } => AiTraceStep {
            kind: "other".to_string(),
            description: "parser recovery placeholder".to_string(),
            line: Some(line),
            value: None,
        },
    };
    steps.push(step);
    if steps.len() >= max_steps {
        *truncated = true;
        return;
    }

    for child in child_stmt_ids(stmt) {
        if *truncated {
            return;
        }
        collect_trace_from_stmt(
            child, parsed, line_start, line_end, steps, truncated, max_steps,
        );
    }
}

/// Return a short string hint for a literal expression value, if it is a
/// simple literal that can be expressed concisely.
fn literal_value_hint(eid: ExprId, module: &fidan_ast::Module) -> Option<String> {
    match module.arena.get_expr(eid) {
        Expr::IntLit { value, .. } => Some(value.to_string()),
        Expr::FloatLit { value, .. } => Some(value.to_string()),
        Expr::BoolLit { value, .. } => Some(value.to_string()),
        Expr::StrLit { value, .. } => {
            if value.len() <= 40 {
                Some(format!("\"{value}\""))
            } else {
                Some(format!("\"{}…\"", &value[..37]))
            }
        }
        Expr::Nothing { .. } => Some("nothing".to_string()),
        _ => None,
    }
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

/// Return just the raw name of the call target (no formatting), used by the
/// call-graph builder so that recursion checks (`callees.contains(&caller)`)
/// work correctly with plain string equality.
fn raw_callee_name(
    callee: fidan_ast::ExprId,
    module: &fidan_ast::Module,
    interner: &SymbolInterner,
) -> String {
    match module.arena.get_expr(callee) {
        Expr::Ident { name, .. } => interner.resolve(*name).to_string(),
        Expr::Field { field, .. } => interner.resolve(*field).to_string(),
        Expr::Parent { .. } => "parent".to_string(),
        _ => "(callable)".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Write `content` to a unique temp file and return the path.
    fn write_temp(content: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("fidan_ai_test_{n}.fdn"));
        std::fs::write(&path, content).expect("failed to write temp fidan file");
        path
    }

    // A small but representative Fidan source covering:
    //   top-level vars, actions with params/locals/calls/if/return,
    //   a recursive action, an object with a method, an enum.
    const SAMPLE: &str = "
var global_x = 42

action add with (certain x oftype integer, certain y oftype integer) returns integer {
    return x + y
}

action double with (certain n oftype integer) returns integer {
    var result = add(n, n)
    return result
}

action recurse with (certain n oftype integer) returns integer {
    if n == 0 {
        return 0
    }
    return recurse(n - 1)
}

object Counter {
    var count oftype integer = 0

    new with (certain start oftype integer) {
        this.count = start
    }

    action increment {
        this.count += 1
    }
}

enum Status {
    Active
    Inactive
    Unknown
}
";

    #[test]
    fn call_graph_detects_callees() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let graph = build_call_graph(&parsed);

        let double_node = graph.iter().find(|n| n.caller == "double");
        assert!(double_node.is_some(), "double should appear in call graph");
        let callees = &double_node.unwrap().callees;
        assert!(
            callees.iter().any(|c| c.contains("add")),
            "double should list `add` as a callee; got: {callees:?}"
        );
    }

    #[test]
    fn call_graph_non_recursive_not_flagged() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let graph = build_call_graph(&parsed);

        let double_node = graph.iter().find(|n| n.caller == "double");
        assert!(
            !double_node.unwrap().is_recursive,
            "double is not recursive"
        );
    }

    #[test]
    fn call_graph_detects_recursion() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let graph = build_call_graph(&parsed);

        let recurse_node = graph.iter().find(|n| n.caller == "recurse");
        assert!(
            recurse_node.is_some(),
            "recurse should appear in call graph"
        );
        assert!(
            recurse_node.unwrap().is_recursive,
            "recurse should be marked is_recursive"
        );
    }

    #[test]
    fn type_map_collects_params() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let bindings = build_type_map(&parsed);

        let x_param = bindings.iter().find(|b| b.name == "x" && b.kind == "param");
        assert!(x_param.is_some(), "param 'x' should appear in type map");
        assert_eq!(
            x_param.unwrap().inferred_type,
            "integer",
            "param x should be typed integer"
        );
    }

    #[test]
    fn type_map_collects_local_vars() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let bindings = build_type_map(&parsed);

        let result_var = bindings
            .iter()
            .find(|b| b.name == "result" && b.kind == "var");
        assert!(
            result_var.is_some(),
            "local var 'result' inside 'double' should appear in type map"
        );
    }

    #[test]
    fn type_map_collects_module_vars() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let bindings = build_type_map(&parsed);

        // top-level `var global_x set 42` — should be in Item::VarDecl branch
        let global = bindings
            .iter()
            .find(|b| b.name == "global_x" && b.kind == "var");
        assert!(
            global.is_some(),
            "top-level module variable 'global_x' should appear in type map"
        );
    }

    #[test]
    fn static_trace_full_file_non_empty() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let total = parsed.lines.len();
        let trace = build_static_trace(&parsed, 1, total);

        assert!(
            !trace.steps.is_empty(),
            "static trace over the full file should produce at least one step"
        );
        assert!(
            trace.steps.iter().all(|s| s.line.is_some()),
            "all trace steps should have a line number"
        );
        // Small file — should never hit the 250-step truncation limit.
        assert!(
            !trace.truncated,
            "small sample file should not be truncated"
        );
    }

    #[test]
    fn static_trace_respects_line_range() {
        let path = write_temp(SAMPLE);
        let parsed = parse_file(&path).unwrap();
        let trace = build_static_trace(&parsed, 1, 1);

        // Every step emitted must fall within [1, 1].
        for step in &trace.steps {
            if let Some(line) = step.line {
                assert!(
                    line == 1,
                    "step at line {line} is outside requested range [1, 1]"
                );
            }
        }
    }

    #[test]
    fn static_trace_truncation_flag() {
        // Build a Fidan source with more than 250 statements to trigger truncation.
        let mut src = String::from("action many_stmts {\n");
        for i in 0..300_usize {
            src.push_str(&format!("    var v{i} = {i}\n"));
        }
        src.push_str("}\n");

        let path = write_temp(&src);
        let parsed = parse_file(&path).unwrap();
        let total = parsed.lines.len();
        let trace = build_static_trace(&parsed, 1, total);

        assert!(trace.truncated, "trace over 300 stmts should be truncated");
        assert_eq!(
            trace.steps.len(),
            250,
            "truncated trace should have exactly 250 steps"
        );
    }

    #[test]
    fn explain_context_populates_phase_a_fields() {
        let path = write_temp(SAMPLE);
        let ctx = analyze_explain_context(&path, None, None).unwrap();

        assert!(
            !ctx.call_graph.is_empty(),
            "call_graph should be populated by analyze_explain_context"
        );
        assert!(
            !ctx.type_map.is_empty(),
            "type_map should be populated by analyze_explain_context"
        );
        let trace = ctx
            .runtime_trace
            .as_ref()
            .expect("runtime_trace should be Some");
        assert!(
            !trace.steps.is_empty(),
            "runtime_trace should have at least one step"
        );
    }

    #[test]
    fn explain_context_no_duplicate_outline() {
        let path = write_temp(SAMPLE);
        let ctx = analyze_explain_context(&path, None, None).unwrap();

        // Verify the double-build_outline bug is fixed: names should not repeat.
        let names: Vec<_> = ctx.module_outline.iter().map(|i| &i.name).collect();
        let unique_count = names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert_eq!(
            names.len(),
            unique_count,
            "module_outline must not contain duplicate entries (double build_outline bug)"
        );
    }
}
