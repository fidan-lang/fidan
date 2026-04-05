use anyhow::{Context, Result, bail};
use fidan_diagnostics::Diagnostic;
use fidan_diagnostics::{Severity, render_message_to_stderr};
use fidan_driver::AiFixMode;
use std::io::IsTerminal as _;
use std::path::PathBuf;

pub(crate) fn run_fix(
    file: PathBuf,
    in_place: bool,
    ai_prompt: Option<String>,
    improve_prompt: Option<String>,
) -> Result<()> {
    use fidan_diagnostics::{Confidence, render_to_stderr};
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let ai_requested = ai_prompt.is_some() || improve_prompt.is_some();
    let improve_requested = improve_prompt.is_some();
    let ai_prompt = ai_prompt.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    let improve_prompt = improve_prompt.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    let combined_prompt = combine_ai_prompts(ai_prompt.as_deref(), improve_prompt.as_deref());

    let src = std::fs::read_to_string(&file).with_context(|| format!("cannot read {:?}", file))?;
    let source_name = file.display().to_string();
    let source_map = Arc::new(SourceMap::new());
    let interner = Arc::new(SymbolInterner::new());
    let f = source_map.add_file(source_name.as_str(), src.as_str());
    let (tokens, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
    let (module, parse_diags) = fidan_parser::parse(&tokens, f.id, Arc::clone(&interner));
    let type_diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));

    // In AI mode the assistant will handle remaining diagnostics; only print
    // them when running the deterministic-only path so the output isn't noisy.
    if !ai_requested {
        for d in &lex_diags {
            render_to_stderr(d, &source_map);
        }
        for d in &parse_diags {
            render_to_stderr(d, &source_map);
        }
        for d in &type_diags {
            render_to_stderr(d, &source_map);
        }
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

    // Sort descending by byte offset — apply back-to-front to preserve earlier offsets.
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    edits.dedup_by_key(|e| e.0);

    let src_bytes = src.as_bytes();

    if !ai_requested {
        // Deterministic-only path.
        if edits.is_empty() {
            render_message_to_stderr(Severity::Note, "", "no high-confidence fixes available");
            return Ok(());
        }

        if in_place {
            let mut patched = src.clone();
            for (lo, hi, replacement) in &edits {
                let lo = *lo as usize;
                let hi = (*hi as usize).min(patched.len());
                patched.replace_range(lo..hi, replacement);
            }
            std::fs::write(&file, &patched).with_context(|| format!("cannot write {:?}", file))?;
            render_message_to_stderr(
                Severity::Note,
                "",
                &format!("applied {} fix(es) to {source_name}", edits.len()),
            );
        } else {
            for (lo, hi, replacement) in &edits {
                print_diff_hunk(src_bytes, &src, *lo as usize, *hi as usize, replacement);
            }
        }
        return Ok(());
    }

    // ── AI path ──────────────────────────────────────────────────────────────
    // 1. Apply high-confidence fixes to get the patched source.
    let mut patched_src = src.clone();
    for (lo, hi, replacement) in &edits {
        let lo = *lo as usize;
        let hi = (*hi as usize).min(patched_src.len());
        patched_src.replace_range(lo..hi, replacement);
    }

    // 2. Re-run compiler on patched source to discover remaining diagnostics.
    let remaining_diags = collect_remaining_diagnostics(&patched_src, &source_name)?;

    if remaining_diags.is_empty() && edits.is_empty() && !improve_requested {
        render_message_to_stderr(Severity::Note, "", "no fixes needed");
        return Ok(());
    }

    // 3. If there are remaining diagnostics, call the AI helper.
    let ai_result = if remaining_diags.is_empty() && !improve_requested {
        fidan_driver::AiFixResult {
            summary: String::new(),
            hunks: vec![],
            model: None,
            provider: None,
        }
    } else {
        let mode = if improve_requested {
            AiFixMode::Improve
        } else {
            AiFixMode::Diagnostics
        };
        invoke_ai_fix_helper(
            &file,
            &patched_src,
            &remaining_diags,
            combined_prompt.as_deref(),
            mode,
        )?
    };

    // 4. Apply AI hunks to the patched source to get the final source.
    // Always warn about mismatches regardless of --in-place, so the user knows a hunk was skipped.
    let (final_src, applied_ai_hunks) =
        apply_ai_hunks(patched_src.clone(), &ai_result.hunks, true)?;
    let ai_applied = applied_ai_hunks.len();

    // 5. Output the result.
    if in_place {
        if !edits.is_empty() || ai_applied > 0 {
            std::fs::write(&file, &final_src)
                .with_context(|| format!("cannot write {:?}", file))?;
            render_message_to_stderr(
                Severity::Note,
                "",
                &format!(
                    "applied {} deterministic fix(es) + {} AI fix hunk(s) to {source_name}",
                    edits.len(),
                    ai_applied
                ),
            );
        } else {
            let message = if improve_requested {
                "AI returned no applicable improvements"
            } else if remaining_diags.is_empty() {
                "no fixes needed"
            } else {
                "AI returned no applicable changes"
            };
            render_message_to_stderr(Severity::Note, "", message);
        }
    } else {
        // Show diff of deterministic edits against original source.
        for (lo, hi, replacement) in &edits {
            print_diff_hunk(src_bytes, &src, *lo as usize, *hi as usize, replacement);
        }
        if !ai_result.hunks.is_empty() {
            render_message_to_stderr(
                Severity::Note,
                "",
                &format!("AI: {} applicable fix hunk(s)", ai_applied),
            );
        }
        // Show diff of AI hunks that were actually applicable to the patched source.
        for hunk in &applied_ai_hunks {
            print_ai_hunk_diff(hunk);
        }
        if edits.is_empty() && applied_ai_hunks.is_empty() {
            let message = if improve_requested {
                "AI returned no applicable improvements"
            } else if remaining_diags.is_empty() {
                "no fixes needed"
            } else {
                "AI returned no applicable changes"
            };
            render_message_to_stderr(Severity::Note, "", message);
        }
    }

    Ok(())
}

fn combine_ai_prompts(ai_prompt: Option<&str>, improve_prompt: Option<&str>) -> Option<String> {
    match (ai_prompt, improve_prompt) {
        (Some(ai), Some(improve)) => Some(format!(
            "Additional diagnostics-mode guidance: {ai}\n\nRequested improvements: {improve}"
        )),
        (Some(ai), None) => Some(ai.to_string()),
        (None, Some(improve)) => Some(improve.to_string()),
        (None, None) => None,
    }
}

fn print_diff_hunk(src_bytes: &[u8], src: &str, lo: usize, hi: usize, replacement: &str) {
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
}

fn print_ai_hunk_diff(hunk: &fidan_driver::AiFixHunk) {
    let old: Vec<&str> = hunk.old_text.trim_end_matches('\n').lines().collect();
    let new: Vec<&str> = hunk.new_text.trim_end_matches('\n').lines().collect();

    // LCS-based diff so unchanged lines show as context (no colour).
    enum Op<'a> {
        Context(&'a str),
        Remove(&'a str),
        Add(&'a str),
    }

    let m = old.len();
    let n = new.len();
    // dp[i][j] = LCS length for old[i..] vs new[j..]
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if old[i].trim() == new[j].trim() {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops: Vec<Op<'_>> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < m || j < n {
        if i < m && j < n && old[i].trim() == new[j].trim() {
            ops.push(Op::Context(new[j])); // use new[j] to preserve any restored indent
            i += 1;
            j += 1;
        } else if j < n && (i >= m || dp[i][j + 1] >= dp[i + 1][j]) {
            ops.push(Op::Add(new[j]));
            j += 1;
        } else {
            ops.push(Op::Remove(old[i]));
            i += 1;
        }
    }

    for op in &ops {
        match op {
            Op::Context(l) => println!("  {l}"),
            Op::Remove(l) => println!("\x1b[31m- {l}\x1b[0m"),
            Op::Add(l) => println!("\x1b[32m+ {l}\x1b[0m"),
        }
    }
    if !hunk.reason.is_empty() {
        println!("\x1b[33m  # {}\x1b[0m", hunk.reason);
    }
}

fn collect_remaining_diagnostics(
    source: &str,
    source_name: &str,
) -> Result<Vec<fidan_driver::AiDiagnosticSummary>> {
    use fidan_lexer::{Lexer, SymbolInterner};
    use fidan_source::SourceMap;
    use std::sync::Arc;

    let source_map = Arc::new(SourceMap::new());
    let interner = Arc::new(SymbolInterner::new());
    let f = source_map.add_file(source_name, source);
    let (tokens, lex_diags) = Lexer::new(&f, Arc::clone(&interner)).tokenise();
    let (module, parse_diags) = fidan_parser::parse(&tokens, f.id, Arc::clone(&interner));
    let type_diags = fidan_typeck::typecheck(&module, Arc::clone(&interner));

    let all_diags = type_diags
        .iter()
        .chain(parse_diags.iter())
        .chain(lex_diags.iter());

    let mut summaries = Vec::new();
    for d in all_diags {
        let line = source[..d.span.start.min(source.len() as u32) as usize]
            .chars()
            .filter(|&ch| ch == '\n')
            .count()
            + 1;
        summaries.push(fidan_driver::AiDiagnosticSummary {
            severity: format!("{:?}", d.severity).to_lowercase(),
            code: d.code.clone(),
            message: d.message.clone(),
            line,
        });
    }
    Ok(summaries)
}

fn invoke_ai_fix_helper(
    file: &std::path::Path,
    source: &str,
    diagnostics: &[fidan_driver::AiDiagnosticSummary],
    prompt: Option<&str>,
    mode: AiFixMode,
) -> Result<fidan_driver::AiFixResult> {
    use fidan_driver::{
        AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AiAnalysisHelperCommand, AiAnalysisHelperRequest,
        AiAnalysisHelperResponse, AiAnalysisHelperResult,
    };
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let toolchain = crate::toolchain::ensure_ai_toolchain_installed()?;

    if !toolchain.helper_path.is_file() {
        bail!(
            "installed ai-analysis helper is missing at `{}` — reinstall with `fidan toolchain add ai-analysis --version {}`",
            toolchain.helper_path.display(),
            toolchain.metadata.toolchain_version
        );
    }

    let request = AiAnalysisHelperRequest {
        protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
        command: AiAnalysisHelperCommand::Fix {
            file: file.to_path_buf(),
            source: source.to_string(),
            diagnostics: diagnostics.to_vec(),
            mode,
            prompt: prompt.map(ToOwned::to_owned),
        },
    };
    let request_bytes =
        serde_json::to_vec(&request).context("failed to serialize ai-fix helper request")?;

    let mut child = Command::new(&toolchain.helper_path)
        .arg("analyze")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch ai-analysis helper `{}`",
                toolchain.helper_path.display()
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&request_bytes)
            .context("failed to send ai-fix helper request")?;
    }

    let spinner = if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(
            indicatif::ProgressStyle::with_template("  {spinner:.cyan}  {msg}")
                .expect("valid spinner template")
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(70));
        pb.set_message(match mode {
            AiFixMode::Diagnostics => "Generating AI fixes…",
            AiFixMode::Improve => "Generating AI improvements…",
        });
        pb
    } else {
        indicatif::ProgressBar::hidden()
    };

    let output = child
        .wait_with_output()
        .context("failed while waiting for ai-fix helper to finish")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "ai-analysis helper exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let response: AiAnalysisHelperResponse =
        serde_json::from_slice(&output.stdout).context("failed to parse ai-fix helper response")?;
    if response.protocol_version != AI_ANALYSIS_HELPER_PROTOCOL_VERSION {
        bail!(
            "ai-analysis helper protocol mismatch (helper={}, cli={})",
            response.protocol_version,
            AI_ANALYSIS_HELPER_PROTOCOL_VERSION
        );
    }
    if !response.success {
        bail!(
            "ai-fix helper failed{}",
            response
                .error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        );
    }

    match response
        .result
        .context("ai-fix helper returned no result")?
    {
        AiAnalysisHelperResult::Fix(fix_result) => Ok(fix_result),
        _ => bail!("ai-analysis helper returned an unexpected result kind for a fix request"),
    }
}

/// How many lines the hunk position may drift from the model's reported
/// `line_start` before we give up. Models frequently miscount by several lines,
/// especially when a multi-line `old_text` is involved and the model anchors
/// `line_start` on the last (diagnostic) line instead of the first.
const HUNK_POSITION_TOLERANCE: usize = 5;

/// Apply AI-proposed hunks to `source` (line-based).
/// Returns the modified source and the number of hunks successfully applied.
///
/// Primary key is always the model-reported `line_start`; `old_text` is used
/// to *verify* the match (trim-based, so stripped indentation is tolerated).
/// We search within ±HUNK_POSITION_TOLERANCE lines of the hint to absorb
/// common off-by-one model errors, but we never scan the whole file — that
/// would silently apply a fix to the wrong copy of identical code.
fn apply_ai_hunks(
    source: String,
    hunks: &[fidan_driver::AiFixHunk],
    warn_on_mismatch: bool,
) -> Result<(String, Vec<fidan_driver::AiFixHunk>)> {
    if hunks.is_empty() {
        return Ok((source, vec![]));
    }

    let lines_snapshot: Vec<String> = source.lines().map(|l| l.to_string()).collect();
    let had_trailing_newline = source.ends_with('\n');

    struct Resolved {
        actual_start: usize,
        actual_end: usize,
        replacement_lines: Vec<String>,
        display_hunk: fidan_driver::AiFixHunk,
    }

    let mut resolved: Vec<Resolved> = Vec::new();

    for hunk in hunks {
        // Skip no-op hunks at the earliest possible point, before any processing.
        // Trim both sides so indentation differences don't cause false positives.
        if hunk.old_text.trim() == hunk.new_text.trim() {
            continue;
        }

        let hint = hunk.line_start.saturating_sub(1); // convert model 1-based to 0-based
        let old_lines: Vec<&str> = hunk.old_text.trim_end_matches('\n').lines().collect();
        if old_lines.is_empty() {
            let insert_at = hint.min(lines_snapshot.len());
            let replacement_lines: Vec<String> = hunk
                .new_text
                .trim_end_matches('\n')
                .lines()
                .map(|l| l.to_string())
                .collect();
            if replacement_lines.is_empty() {
                continue;
            }

            resolved.push(Resolved {
                actual_start: insert_at,
                actual_end: insert_at,
                replacement_lines: replacement_lines.clone(),
                display_hunk: fidan_driver::AiFixHunk {
                    line_start: insert_at + 1,
                    line_end: insert_at + 1,
                    old_text: String::new(),
                    new_text: replacement_lines.join("\n"),
                    reason: hunk.reason.clone(),
                },
            });
            continue;
        }
        let n = old_lines.len();

        // Check the hint and then narrow offsets until we find a trim-match.
        let matches_at = |start: usize| -> bool {
            if start + n > lines_snapshot.len() {
                return false;
            }
            lines_snapshot[start..start + n]
                .iter()
                .zip(old_lines.iter())
                .all(|(actual, expected)| actual.trim() == expected.trim())
        };

        let actual_start = std::iter::once(hint)
            .chain((1..=HUNK_POSITION_TOLERANCE).flat_map(|d| {
                let lo = hint.saturating_sub(d);
                let hi = hint + d;
                // yield lo then hi, dedup identical values
                if lo == hi { vec![lo] } else { vec![lo, hi] }
            }))
            .find(|&s| matches_at(s));

        match actual_start {
            None => {
                if warn_on_mismatch {
                    let actual_at_hint = lines_snapshot
                        .get(hint)
                        .map(String::as_str)
                        .unwrap_or("<out of range>");
                    let expected_preview: String = hunk
                        .old_text
                        .trim_end_matches('\n')
                        .chars()
                        .take(120)
                        .collect();
                    let actual_preview: String = actual_at_hint.chars().take(120).collect();
                    render_message_to_stderr(
                        Severity::Note,
                        "",
                        &format!(
                            "AI fix hunk for lines {}-{} skipped — old_text did not match source\n  \
                             expected: {expected_preview:?}\n  \
                             actual:   {actual_preview:?}",
                            hunk.line_start, hunk.line_end
                        ),
                    );
                }
            }
            Some(actual_start) => {
                // If the model stripped base indentation, restore it on new_text lines.
                let file_indent: String = lines_snapshot[actual_start]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                let model_indent_len = old_lines
                    .first()
                    .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
                    .unwrap_or(0);
                let restore_indent = file_indent.len() > model_indent_len;

                let replacement_lines: Vec<String> = if hunk.new_text.is_empty() {
                    vec![]
                } else {
                    hunk.new_text
                        .trim_end_matches('\n')
                        .lines()
                        .map(|l| {
                            if restore_indent {
                                format!("{file_indent}{l}")
                            } else {
                                l.to_string()
                            }
                        })
                        .collect()
                };

                let existing_lines = &lines_snapshot[actual_start..actual_start + n];
                let is_noop = existing_lines.len() == replacement_lines.len()
                    && existing_lines
                        .iter()
                        .zip(replacement_lines.iter())
                        .all(|(actual, replacement)| actual.trim() == replacement.trim());
                if is_noop {
                    continue;
                }

                resolved.push(Resolved {
                    actual_start,
                    actual_end: actual_start + n,
                    replacement_lines: replacement_lines.clone(),
                    display_hunk: fidan_driver::AiFixHunk {
                        line_start: actual_start + 1,
                        line_end: actual_start + n,
                        old_text: existing_lines.join("\n"),
                        new_text: replacement_lines.join("\n"),
                        reason: hunk.reason.clone(),
                    },
                });
            }
        }
    }

    // Apply back-to-front so earlier indices remain valid.
    resolved.sort_by(|a, b| b.actual_start.cmp(&a.actual_start));

    let mut lines = lines_snapshot;
    for r in &resolved {
        lines.splice(
            r.actual_start..r.actual_end,
            r.replacement_lines.iter().cloned(),
        );
    }

    let mut applied_hunks: Vec<fidan_driver::AiFixHunk> =
        resolved.iter().map(|r| r.display_hunk.clone()).collect();
    applied_hunks.sort_by_key(|h| h.line_start);

    let mut result = lines.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok((result, applied_hunks))
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
