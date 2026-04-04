use anyhow::{Context, Result};
use fidan_diagnostics::Diagnostic;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use std::path::PathBuf;

pub(crate) fn run_fix(file: PathBuf, dry_run: bool) -> Result<()> {
    use fidan_diagnostics::{Confidence, render_to_stderr};
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;
    let source_name = file.display().to_string();
    let source_map = Arc::new(SourceMap::new());
    let interner = Arc::new(SymbolInterner::new());
    let f = source_map.add_file(source_name.as_str(), src.as_str());
    let (tokens, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
    for d in &lex_diags {
        render_to_stderr(d, &source_map);
    }
    let (module, parse_diags) = fidan_parser::parse(&tokens, f.id, Arc::clone(&interner));
    for d in &parse_diags {
        render_to_stderr(d, &source_map);
    }
    let type_diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));
    for d in &type_diags {
        render_to_stderr(d, &source_map);
    }

    // Collect all High-confidence machine-applicable edits.
    let mut edits: Vec<(u32, u32, String)> = vec![]; // (lo, hi, replacement)
    for diag in type_diags
        .iter()
        .chain(parse_diags.iter())
        .chain(lex_diags.iter())
    {
        for sug in &diag.suggestions {
            if sug.confidence == Confidence::High
                && let Some(edit) = &sug.edit
            {
                edits.push((edit.span.start, edit.span.end, edit.replacement.clone()));
            }
        }
    }
    edits.extend(synthesize_grouped_import_edits(&type_diags, &src));

    if edits.is_empty() {
        render_message_to_stderr(Severity::Note, "", "no high-confidence fixes available");
        return Ok(());
    }

    // Sort descending by byte offset — apply back-to-front to preserve earlier offsets.
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    edits.dedup_by_key(|e| e.0);

    let src_bytes = src.as_bytes();
    let mut patched = src.clone();
    for (lo, hi, replacement) in &edits {
        let lo = *lo as usize;
        let hi = (*hi as usize).min(patched.len());
        if dry_run {
            let line_start = src_bytes[..lo]
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let line_end = src_bytes[hi..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| hi + p)
                .unwrap_or(src.len());
            println!("\x1b[31m- {}\x1b[0m", &src[line_start..line_end]);
            let new_line = format!(
                "{}{}{}",
                &src[line_start..lo],
                replacement,
                &src[hi..line_end]
            );
            println!("\x1b[32m+ {}\x1b[0m", new_line);
        } else {
            patched.replace_range(lo..hi, replacement);
        }
    }

    if !dry_run {
        std::fs::write(&file, &patched).with_context(|| format!("cannot write {:?}", file))?;
        render_message_to_stderr(
            Severity::Note,
            "",
            &format!("applied {} fix(es) to {source_name}", edits.len()),
        );
    }
    Ok(())
}

fn synthesize_grouped_import_edits(diags: &[Diagnostic], src: &str) -> Vec<(u32, u32, String)> {
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct GroupedImportPlan {
        remove_unused: HashSet<String>,
        duplicate_removals: HashMap<String, usize>,
    }

    let mut plans: HashMap<(u32, u32), GroupedImportPlan> = HashMap::new();
    for diag in diags {
        if !matches!(diag.code.as_str(), "W1005" | "W1007") {
            continue;
        }
        if diag.suggestions.iter().any(|s| s.edit.is_some()) {
            continue;
        }
        let Some(import_name) = extract_backticked_name(&diag.message) else {
            continue;
        };
        let key = (diag.span.start, diag.span.end);
        let plan = plans.entry(key).or_default();
        match diag.code.as_str() {
            "W1005" => {
                plan.remove_unused.insert(import_name.to_string());
            }
            "W1007" => {
                *plan
                    .duplicate_removals
                    .entry(import_name.to_string())
                    .or_insert(0) += 1;
            }
            _ => {}
        }
    }

    let mut edits = Vec::new();
    for ((span_lo, span_hi), mut plan) in plans {
        let lo = span_lo as usize;
        let hi = span_hi as usize;
        let Some(stmt) = src.get(lo..hi) else {
            continue;
        };
        let Some(open) = stmt.find('{') else {
            continue;
        };
        let Some(close) = stmt.rfind('}') else {
            continue;
        };
        if close <= open {
            continue;
        }

        let prefix = &stmt[..open];
        let suffix = &stmt[close + 1..];
        let inner = &stmt[open + 1..close];
        let members = parse_grouped_import_members(inner);
        if members.is_empty() {
            continue;
        }

        let mut seen_counts: HashMap<&str, usize> = HashMap::new();
        let mut remaining = Vec::new();
        for member in members {
            if plan.remove_unused.contains(member) {
                continue;
            }
            let seen = seen_counts.entry(member).or_insert(0);
            *seen += 1;
            if *seen > 1
                && let Some(removals_left) = plan.duplicate_removals.get_mut(member)
                && *removals_left > 0
            {
                *removals_left -= 1;
                continue;
            }
            remaining.push(member);
        }

        if remaining.is_empty() {
            let (line_lo, line_hi) = expand_statement_to_trailing_newline(src, lo, hi);
            edits.push((line_lo as u32, line_hi as u32, String::new()));
            continue;
        }

        let replacement = format!("{}{{{}}}{}", prefix, remaining.join(", "), suffix);
        edits.push((span_lo, span_hi, replacement));
    }

    edits
}

fn extract_backticked_name(message: &str) -> Option<&str> {
    let start = message.find('`')?;
    let rest = &message[start + 1..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

fn parse_grouped_import_members(inner: &str) -> Vec<&str> {
    inner
        .split(',')
        .map(str::trim)
        .filter(|member| !member.is_empty())
        .collect()
}

fn expand_statement_to_trailing_newline(src: &str, lo: usize, hi: usize) -> (usize, usize) {
    let bytes = src.as_bytes();
    let mut end = hi.min(bytes.len());
    if end < bytes.len() {
        if bytes[end] == b'\r' && end + 1 < bytes.len() && bytes[end + 1] == b'\n' {
            end += 2;
        } else if matches!(bytes[end], b'\n' | b'\r') {
            end += 1;
        }
    }
    (lo, end)
}
