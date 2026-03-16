use anyhow::{Context, Result};
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
