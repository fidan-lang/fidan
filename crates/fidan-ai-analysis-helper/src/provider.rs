use crate::config::{Config, resolve_api_key};
use anyhow::{Context, Result, bail};
use fidan_driver::{
    AiCallNode, AiDiagnosticSummary, AiExplainContext, AiFixHunk, AiFixMode, AiFixResult,
    AiRuntimeTrace, AiTypedBinding,
};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RenderedExplanation {
    pub provider: String,
    pub model: Option<String>,
    pub summary: String,
    pub input_output_behavior: String,
    pub dependencies: String,
    pub possible_edge_cases: String,
    pub why_pattern_is_used: String,
    pub related_symbols: String,
    pub underlying_behaviour: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ExplanationPayload {
    summary: String,
    input_output_behavior: String,
    dependencies: String,
    possible_edge_cases: String,
    why_pattern_is_used: String,
    related_symbols: String,
    underlying_behaviour: String,
}

pub fn run_explain(
    config: &Config,
    context: &AiExplainContext,
    prompt: Option<&str>,
) -> Result<RenderedExplanation> {
    let api_key = resolve_api_key(config)?;
    let system_prompt = build_system_prompt(config);
    let user_prompt = build_user_prompt(context, prompt);
    let payload = request_validated_explain_payload(
        config,
        api_key.as_deref(),
        &system_prompt,
        &user_prompt,
    )?;
    Ok(RenderedExplanation {
        provider: config.provider.clone(),
        model: Some(config.model.clone()),
        summary: payload.summary,
        input_output_behavior: payload.input_output_behavior,
        dependencies: payload.dependencies,
        possible_edge_cases: payload.possible_edge_cases,
        why_pattern_is_used: payload.why_pattern_is_used,
        related_symbols: payload.related_symbols,
        underlying_behaviour: payload.underlying_behaviour,
    })
}

pub fn run_fix(
    config: &Config,
    file: &Path,
    source: &str,
    diagnostics: &[AiDiagnosticSummary],
    explain_context: Option<&AiExplainContext>,
    mode: AiFixMode,
    prompt: Option<&str>,
) -> Result<AiFixResult> {
    let api_key = resolve_api_key(config)?;
    let system_prompt = build_fix_system_prompt(config, mode);
    let user_prompt =
        build_fix_user_prompt(file, source, diagnostics, explain_context, mode, prompt);
    let payload = request_validated_fix_payload(
        config,
        api_key.as_deref(),
        &system_prompt,
        &user_prompt,
        source,
        diagnostics,
    )?;
    Ok(AiFixResult {
        summary: payload.summary,
        hunks: payload.hunks,
        model: Some(config.model.clone()),
        provider: Some(config.provider.clone()),
    })
}

fn request_validated_explain_payload(
    config: &Config,
    api_key: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<ExplanationPayload> {
    let raw = call_ai_provider(config, api_key, system_prompt, user_prompt)?;
    let first_attempt = parse_json_payload(&raw)
        .context("model response was not valid ai-analysis JSON")
        .and_then(|payload| {
            validate_explanation_payload(&payload)?;
            Ok(payload)
        });
    match first_attempt {
        Ok(payload) => Ok(payload),
        Err(first_error) => {
            let retry_prompt =
                build_explain_retry_user_prompt(user_prompt, &first_error.to_string());
            let retry_raw = call_ai_provider(config, api_key, system_prompt, &retry_prompt)?;
            let retry_attempt = parse_json_payload(&retry_raw)
                .context("model retry response was not valid ai-analysis JSON")
                .and_then(|payload| {
                    validate_explanation_payload(&payload)?;
                    Ok(payload)
                });
            if let Err(retry_error) = retry_attempt {
                let preview: String = retry_raw.chars().take(600).collect();
                bail!(
                    "model returned invalid ai-analysis payload even after retry: {}\nRetry response preview: {}",
                    retry_error,
                    preview
                );
            }
            retry_attempt
        }
    }
}

fn request_validated_fix_payload(
    config: &Config,
    api_key: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
    source: &str,
    diagnostics: &[AiDiagnosticSummary],
) -> Result<FixPayload> {
    let raw = call_ai_provider(config, api_key, system_prompt, user_prompt)?;
    let first_attempt = parse_fix_payload(&raw)
        .context("model response was not valid fix JSON")
        .and_then(|payload| {
            validate_fix_payload(source, diagnostics, &payload)?;
            Ok(payload)
        });
    match first_attempt {
        Ok(payload) => Ok(payload),
        Err(first_error) => {
            let retry_prompt = build_fix_retry_user_prompt(user_prompt, &first_error.to_string());
            let retry_raw = call_ai_provider(config, api_key, system_prompt, &retry_prompt)?;
            let retry_attempt = parse_fix_payload(&retry_raw)
                .context("model retry response was not valid fix JSON")
                .and_then(|payload| {
                    validate_fix_payload(source, diagnostics, &payload)?;
                    Ok(payload)
                });
            if let Err(retry_error) = retry_attempt {
                let preview: String = retry_raw.chars().take(600).collect();
                bail!(
                    "model returned invalid fix payload even after retry: {}\nRetry response preview: {}",
                    retry_error,
                    preview
                );
            }
            retry_attempt
        }
    }
}

fn call_ai_provider(
    config: &Config,
    api_key: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String> {
    match config.provider.trim().to_ascii_lowercase().as_str() {
        "openai-compatible" | "openai" => {
            call_openai_compatible(config, api_key, system_prompt, user_prompt)
        }
        "anthropic" => call_anthropic(config, api_key, system_prompt, user_prompt),
        other => bail!("unsupported ai-analysis provider `{other}`"),
    }
}

fn build_fix_system_prompt(config: &Config, mode: AiFixMode) -> String {
    let custom = config
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let base = match mode {
        AiFixMode::Diagnostics => include_str!("../prompts/fix_system.txt"),
        AiFixMode::Improve => include_str!("../prompts/improve_system.txt"),
    };
    if config.replace_system_prompt {
        return custom
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| base.to_string());
    }
    match custom {
        Some(extra) => format!("{base}\n\nAdditional instructions:\n{extra}"),
        None => base.to_string(),
    }
}

fn build_fix_user_prompt(
    file: &Path,
    source: &str,
    diagnostics: &[AiDiagnosticSummary],
    explain_context: Option<&AiExplainContext>,
    mode: AiFixMode,
    prompt: Option<&str>,
) -> String {
    let rendered_context = explain_context.map(render_explain_context_sections);
    if matches!(mode, AiFixMode::Improve) {
        return render_prompt_template(
            include_str!("../prompts/improve_user.txt"),
            &[
                ("{{FILE}}", file.display().to_string()),
                ("{{SOURCE}}", source.to_string()),
                (
                    "{{DIAGNOSTICS}}",
                    if diagnostics.is_empty() {
                        "(none)".to_string()
                    } else {
                        diagnostics
                            .iter()
                            .map(|d| {
                                format!(
                                    "  line {}: {} {} — {}",
                                    d.line, d.severity, d.code, d.message
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                ),
                (
                    "{{DETERMINISTIC}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.deterministic.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{MODULE_OUTLINE}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.module_outline.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{DEPENDENCIES}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.dependencies.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{RELATED_SYMBOLS}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.related_symbols.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{CALL_GRAPH}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.call_graph.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{TYPE_MAP}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.type_map.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{RUNTIME_TRACE}}",
                    rendered_context
                        .as_ref()
                        .map(|ctx| ctx.runtime_trace.clone())
                        .unwrap_or_else(|| {
                            "(unavailable for the current source state)".to_string()
                        }),
                ),
                (
                    "{{ADDITIONAL_GUIDANCE}}",
                    prompt
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(|s| format!("\nRequested improvement focus: {s}\n\n"))
                        .unwrap_or_default(),
                ),
            ],
        );
    }

    let diag_list = if diagnostics.is_empty() {
        "(none)".to_string()
    } else {
        diagnostics
            .iter()
            .map(|d| {
                format!(
                    "  line {}: {} {} — {}",
                    d.line, d.severity, d.code, d.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let steering = prompt.map(str::trim).filter(|v| !v.is_empty());
    render_prompt_template(
        include_str!("../prompts/fix_user.txt"),
        &[
            ("{{FILE}}", file.display().to_string()),
            ("{{DIAGNOSTICS}}", diag_list),
            (
                "{{DETERMINISTIC}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.deterministic.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            (
                "{{MODULE_OUTLINE}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.module_outline.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            (
                "{{DEPENDENCIES}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.dependencies.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            (
                "{{RELATED_SYMBOLS}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.related_symbols.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            (
                "{{CALL_GRAPH}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.call_graph.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            (
                "{{TYPE_MAP}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.type_map.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            (
                "{{RUNTIME_TRACE}}",
                rendered_context
                    .as_ref()
                    .map(|ctx| ctx.runtime_trace.clone())
                    .unwrap_or_else(|| "(unavailable for the current source state)".to_string()),
            ),
            ("{{SOURCE}}", source.to_string()),
            (
                "{{ADDITIONAL_GUIDANCE}}",
                steering
                    .map(|s| format!("\nAdditional guidance from user: {s}\n\n"))
                    .unwrap_or_default(),
            ),
        ],
    )
}

fn build_explain_retry_user_prompt(original_prompt: &str, error: &str) -> String {
    render_prompt_template(
        include_str!("../prompts/explain_retry_user.txt"),
        &[
            ("{{ORIGINAL_PROMPT}}", original_prompt.to_string()),
            ("{{VALIDATION_ERROR}}", error.to_string()),
        ],
    )
}

fn build_fix_retry_user_prompt(original_prompt: &str, error: &str) -> String {
    render_prompt_template(
        include_str!("../prompts/fix_retry_user.txt"),
        &[
            ("{{ORIGINAL_PROMPT}}", original_prompt.to_string()),
            ("{{VALIDATION_ERROR}}", error.to_string()),
        ],
    )
}

#[derive(Debug, Clone, Deserialize)]
struct FixPayload {
    summary: String,
    hunks: Vec<AiFixHunk>,
}

fn validate_explanation_payload(payload: &ExplanationPayload) -> Result<()> {
    let fields = [
        ("summary", payload.summary.as_str()),
        (
            "input_output_behavior",
            payload.input_output_behavior.as_str(),
        ),
        ("dependencies", payload.dependencies.as_str()),
        ("possible_edge_cases", payload.possible_edge_cases.as_str()),
        ("why_pattern_is_used", payload.why_pattern_is_used.as_str()),
        ("related_symbols", payload.related_symbols.as_str()),
        (
            "underlying_behaviour",
            payload.underlying_behaviour.as_str(),
        ),
    ];
    let errors = fields
        .iter()
        .filter_map(|(name, value)| {
            if value.trim().is_empty() {
                Some(format!("field `{name}` must be a non-empty JSON string"))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

fn validate_fix_payload(
    source: &str,
    diagnostics: &[AiDiagnosticSummary],
    payload: &FixPayload,
) -> Result<()> {
    let mut errors = Vec::new();
    let mut real_hunk_count = 0usize;

    if diagnostics
        .iter()
        .any(|diag| diag.severity.eq_ignore_ascii_case("error"))
        && payload.hunks.is_empty()
    {
        errors.push(
            "payload contains no fix hunks even though compiler errors still need to be resolved"
                .to_string(),
        );
    }

    for (index, hunk) in payload.hunks.iter().enumerate() {
        let line_count = hunk.old_text.trim_end_matches('\n').lines().count();
        if line_count == 0 {
            let max_insert_line = source.lines().count() + 1;
            if hunk.new_text.trim().is_empty() {
                errors.push(format!(
                    "hunk {} has empty old_text and empty new_text",
                    index + 1
                ));
            } else if hunk.line_start == 0 || hunk.line_start > max_insert_line {
                errors.push(format!(
                    "hunk {} insertion line_start {} is out of range (expected 1..={})",
                    index + 1,
                    hunk.line_start,
                    max_insert_line
                ));
            } else {
                real_hunk_count += 1;
            }
            continue;
        }

        if hunk.old_text.trim() == hunk.new_text.trim() {
            errors.push(format!(
                "hunk {} is a no-op: old_text and new_text are identical",
                index + 1
            ));
            continue;
        }

        let old_text_found_exactly = source.contains(&hunk.old_text);
        let old_text_found_trimmed = consecutive_trimmed_block_matches(source, &hunk.old_text);
        if !old_text_found_exactly && !old_text_found_trimmed {
            errors.push(format!(
                "hunk {} old_text does not appear in the provided source",
                index + 1
            ));
            continue;
        }

        real_hunk_count += 1;
    }

    let summary_lower = payload.summary.to_ascii_lowercase();
    let summary_claims_change = [
        "add", "added", "insert", "inserted", "change", "changed", "update", "updated", "replace",
        "replaced", "fix", "fixed", "resolve", "resolved", "remove", "removed", "delete",
        "deleted",
    ]
    .iter()
    .any(|token| summary_lower.contains(token));

    if real_hunk_count == 0 && !payload.hunks.is_empty() {
        errors.push(
            "payload contains hunks but none of them make an actual source change".to_string(),
        );
    }
    if real_hunk_count == 0 && !diagnostics.is_empty() && summary_claims_change {
        errors.push(
            "summary claims a source change but the payload contains no effective edit".to_string(),
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

fn parse_fix_payload(raw: &str) -> Result<FixPayload> {
    if let Ok(payload) = serde_json::from_str::<FixPayload>(raw) {
        return Ok(normalize_fix_payload(payload));
    }
    if let Some(body) = extract_json_fence_body(raw) {
        let payload: FixPayload = serde_json::from_str(body.trim())
            .context("failed to parse fenced JSON fix payload from model response")?;
        return Ok(normalize_fix_payload(payload));
    }
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}'))
        && let Ok(payload) = serde_json::from_str::<FixPayload>(&raw[start..=end])
    {
        return Ok(normalize_fix_payload(payload));
    }
    let preview: String = raw.chars().take(300).collect();
    let hint = if is_prose_response(raw) {
        "model returned prose instead of JSON — use a model that follows JSON output instructions (e.g. GPT-4, Claude, Llama-3-8B+)"
    } else {
        "model returned malformed JSON — check the model or adjust your system_prompt"
    };
    bail!("{hint}.\nRaw response preview: {preview}")
}

fn normalize_fix_payload(mut payload: FixPayload) -> FixPayload {
    payload
        .hunks
        .retain(|hunk| hunk.old_text.trim() != hunk.new_text.trim());
    for hunk in &mut payload.hunks {
        let line_count = hunk.old_text.trim_end_matches('\n').lines().count();
        if line_count > 0 {
            hunk.line_end = hunk.line_start + line_count - 1;
        }
    }
    payload
}

fn consecutive_trimmed_block_matches(source: &str, old_text: &str) -> bool {
    let expected_lines: Vec<&str> = old_text.trim_end_matches('\n').lines().collect();
    if expected_lines.is_empty() {
        return false;
    }

    let source_lines: Vec<&str> = source.lines().collect();
    source_lines.windows(expected_lines.len()).any(|window| {
        window
            .iter()
            .zip(expected_lines.iter())
            .all(|(actual, expected)| actual.trim() == expected.trim())
    })
}

fn extract_json_fence_body(raw: &str) -> Option<&str> {
    let mut offset = 0usize;
    while let Some(start_rel) = raw[offset..].find("```") {
        let start = offset + start_rel;
        let fence = &raw[start + 3..];
        let newline_rel = fence.find('\n')?;
        let info = fence[..newline_rel].trim();
        let body_start = start + 3 + newline_rel + 1;
        let end_rel = raw[body_start..].find("```")?;
        if info.is_empty() || info.eq_ignore_ascii_case("json") {
            return Some(raw[body_start..body_start + end_rel].trim());
        }
        offset = body_start + end_rel + 3;
    }
    None
}

fn call_openai_compatible(
    config: &Config,
    api_key: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String> {
    let url = config
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or("https://api.openai.com/v1/chat/completions");
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .build()
        .context("failed to build ai-analysis HTTP client")?;
    let body = json!({
        "model": config.model,
        "temperature": 0.1,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt }
        ]
    });
    let mut builder = client.post(url).json(&body);
    if let Some(key) = api_key {
        builder = builder.bearer_auth(key);
    }
    let response: serde_json::Value = builder
        .send()
        .with_context(|| format!("failed to call `{url}`"))?
        .error_for_status()
        .with_context(|| format!("ai-analysis request failed for `{url}`"))?
        .json()
        .context("failed to decode openai-compatible response")?;

    extract_openai_text(&response).context("openai-compatible response did not contain text")
}

fn call_anthropic(
    config: &Config,
    api_key: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String> {
    let url = config
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or("https://api.anthropic.com/v1/messages");
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .build()
        .context("failed to build ai-analysis HTTP client")?;
    let body = json!({
        "model": config.model,
        "max_tokens": 4096,
        "temperature": 0.1,
        "system": system_prompt,
        "messages": [
            { "role": "user", "content": user_prompt }
        ]
    });
    let mut builder = client
        .post(url)
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    let response: serde_json::Value = builder
        .send()
        .with_context(|| format!("failed to call `{url}`"))?
        .error_for_status()
        .with_context(|| format!("ai-analysis request failed for `{url}`"))?
        .json()
        .context("failed to decode anthropic response")?;

    extract_anthropic_text(&response).context("anthropic response did not contain text")
}

fn build_user_prompt(context: &AiExplainContext, prompt: Option<&str>) -> String {
    let prompt_text = prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(
            "Explain this code for a beginner while staying precise and technically grounded.",
        );
    let rendered_context = render_explain_context_sections(context);

    render_prompt_template(
        include_str!("../prompts/explain_user.txt"),
        &[
            ("{{PROMPT_TEXT}}", prompt_text.to_string()),
            ("{{TARGET_FILE}}", context.file.display().to_string()),
            ("{{LINE_START}}", context.line_start.to_string()),
            ("{{LINE_END}}", context.line_end.to_string()),
            ("{{TOTAL_LINES}}", context.total_lines.to_string()),
            ("{{SELECTED_SOURCE}}", context.selected_source.clone()),
            ("{{DETERMINISTIC}}", rendered_context.deterministic),
            ("{{MODULE_OUTLINE}}", rendered_context.module_outline),
            ("{{DEPENDENCIES}}", rendered_context.dependencies),
            ("{{RELATED_SYMBOLS}}", rendered_context.related_symbols),
            ("{{DIAGNOSTICS}}", rendered_context.diagnostics),
            ("{{CALL_GRAPH}}", rendered_context.call_graph),
            ("{{TYPE_MAP}}", rendered_context.type_map),
            ("{{RUNTIME_TRACE}}", rendered_context.runtime_trace),
        ],
    )
}

struct RenderedExplainContextSections {
    deterministic: String,
    module_outline: String,
    dependencies: String,
    related_symbols: String,
    diagnostics: String,
    call_graph: String,
    type_map: String,
    runtime_trace: String,
}

fn render_explain_context_sections(context: &AiExplainContext) -> RenderedExplainContextSections {
    let deterministic = context
        .deterministic_lines
        .iter()
        .map(|line| {
            format!(
                "line {}: {}\nwhat: {}\ninferred_type: {}\nreads: {}\nwrites: {}\nrisks: {}",
                line.line,
                line.source,
                line.what_it_does,
                line.inferred_type.as_deref().unwrap_or("-"),
                if line.reads.is_empty() {
                    "-".to_string()
                } else {
                    line.reads.join(", ")
                },
                if line.writes.is_empty() {
                    "-".to_string()
                } else {
                    line.writes.join(", ")
                },
                if line.risks.is_empty() {
                    "-".to_string()
                } else {
                    line.risks.join(", ")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let module_outline = context
        .module_outline
        .iter()
        .map(|item| {
            format!(
                "{} {} at line {}{}",
                item.kind,
                item.name,
                item.line,
                item.detail
                    .as_deref()
                    .map(|detail| format!(" — {detail}"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let related_symbols = context
        .related_symbols
        .iter()
        .map(|symbol| {
            format!(
                "{} {}:{} — {}{}",
                symbol.kind,
                symbol.file.display(),
                symbol.line,
                symbol.snippet,
                symbol
                    .detail
                    .as_deref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let dependencies = context
        .dependencies
        .iter()
        .map(|dep| {
            let mut rendered = dep.path.clone();
            if let Some(alias) = dep.alias.as_deref() {
                rendered.push_str(&format!(" as {alias}"));
            }
            if dep.is_re_export {
                rendered.push_str(" [re-export]");
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join("\n");
    let diagnostics = context
        .diagnostics
        .iter()
        .map(|diag| {
            format!(
                "{} {} at line {}: {}",
                diag.severity, diag.code, diag.line, diag.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    RenderedExplainContextSections {
        deterministic: if deterministic.is_empty() {
            "(none)".to_string()
        } else {
            deterministic
        },
        module_outline: if module_outline.is_empty() {
            "(none)".to_string()
        } else {
            module_outline
        },
        dependencies: if dependencies.is_empty() {
            "(none)".to_string()
        } else {
            dependencies
        },
        related_symbols: if related_symbols.is_empty() {
            "(none)".to_string()
        } else {
            related_symbols
        },
        diagnostics: if diagnostics.is_empty() {
            "(none)".to_string()
        } else {
            diagnostics
        },
        call_graph: render_call_graph(&context.call_graph),
        type_map: render_type_map(&context.type_map),
        runtime_trace: context
            .runtime_trace
            .as_ref()
            .map(render_runtime_trace)
            .unwrap_or_else(|| "(none)".to_string()),
    }
}

fn render_prompt_template(template: &str, replacements: &[(&str, String)]) -> String {
    let mut rendered = template.to_string();
    for (placeholder, value) in replacements {
        rendered = rendered.replace(placeholder, value);
    }
    rendered
}

fn normalize_explanation_payload(value: Value) -> Result<ExplanationPayload> {
    let object = value
        .as_object()
        .context("model response did not contain a JSON object")?;
    Ok(ExplanationPayload {
        summary: explanation_field_to_string(object, "summary")?,
        input_output_behavior: explanation_field_to_string(object, "input_output_behavior")?,
        dependencies: explanation_field_to_string(object, "dependencies")?,
        possible_edge_cases: explanation_field_to_string(object, "possible_edge_cases")?,
        why_pattern_is_used: explanation_field_to_string(object, "why_pattern_is_used")?,
        related_symbols: explanation_field_to_string(object, "related_symbols")?,
        underlying_behaviour: explanation_field_to_string(object, "underlying_behaviour")?,
    })
}

fn explanation_field_to_string(object: &Map<String, Value>, key: &str) -> Result<String> {
    let value = object
        .get(key)
        .with_context(|| format!("missing required field `{key}`"))?;
    stringify_explanation_value(value, key)
}

fn stringify_explanation_value(value: &Value, key: &str) -> Result<String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(boolean) => Ok(boolean.to_string()),
        Value::Null => bail!("field `{key}` was null"),
        Value::Array(values) => {
            let parts = values
                .iter()
                .map(explanation_value_fragment)
                .filter(|fragment| !fragment.trim().is_empty())
                .collect::<Vec<_>>();
            if parts.is_empty() {
                bail!("field `{key}` was an empty array")
            }
            Ok(parts.join("\n"))
        }
        Value::Object(_) => serde_json::to_string_pretty(value)
            .with_context(|| format!("failed to stringify field `{key}`")),
    }
}

fn explanation_value_fragment(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Null => String::new(),
        Value::Array(values) => values
            .iter()
            .map(explanation_value_fragment)
            .filter(|fragment| !fragment.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn render_call_graph(nodes: &[AiCallNode]) -> String {
    if nodes.is_empty() {
        return "(none)".to_string();
    }
    nodes
        .iter()
        .map(|node| {
            let callees = if node.callees.is_empty() {
                "(no calls)".to_string()
            } else {
                node.callees.join(", ")
            };
            let recursive_marker = if node.is_recursive {
                " [recursive]"
            } else {
                ""
            };
            format!(
                "line {}: {} → {}{}",
                node.line, node.caller, callees, recursive_marker
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_type_map(bindings: &[AiTypedBinding]) -> String {
    if bindings.is_empty() {
        return "(none)".to_string();
    }
    bindings
        .iter()
        .map(|b| {
            if b.line > 0 {
                format!(
                    "line {}: {} {} : {}",
                    b.line, b.kind, b.name, b.inferred_type
                )
            } else {
                format!("{} {} : {}", b.kind, b.name, b.inferred_type)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_runtime_trace(trace: &AiRuntimeTrace) -> String {
    if trace.steps.is_empty() {
        return "(no steps in selected range)".to_string();
    }
    let mut lines: Vec<String> = trace
        .steps
        .iter()
        .map(|step| {
            let line_part = step.line.map(|l| format!("line {l}: ")).unwrap_or_default();
            let value_part = step
                .value
                .as_deref()
                .map(|v| format!(" = {v}"))
                .unwrap_or_default();
            format!(
                "[{}] {}{}{}",
                step.kind, line_part, step.description, value_part
            )
        })
        .collect();
    if trace.truncated {
        lines.push("(trace truncated at 250 steps)".to_string());
    }
    lines.join("\n")
}

fn default_system_prompt() -> &'static str {
    include_str!("../prompts/explain_system.txt")
}

fn build_system_prompt(config: &Config) -> String {
    let custom = config
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if config.replace_system_prompt {
        return custom
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| default_system_prompt().to_string());
    }

    match custom {
        Some(custom) => format!(
            "{}\n\nAdditional instructions:\n{}",
            default_system_prompt(),
            custom
        ),
        None => default_system_prompt().to_string(),
    }
}

fn is_prose_response(raw: &str) -> bool {
    // If the trimmed content doesn't start with `{` or `[`, it's almost certainly prose.
    let trimmed = raw.trim();
    !trimmed.starts_with('{') && !trimmed.starts_with('[')
}

fn parse_json_payload(raw: &str) -> Result<ExplanationPayload> {
    if let Ok(payload) = serde_json::from_str::<ExplanationPayload>(raw) {
        return Ok(payload);
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return normalize_explanation_payload(value);
    }

    if let Some(body) = extract_json_fence_body(raw) {
        if let Ok(payload) = serde_json::from_str::<ExplanationPayload>(body.trim()) {
            return Ok(payload);
        }
        let value = serde_json::from_str::<Value>(body.trim())
            .context("failed to parse fenced JSON payload from model response")?;
        return normalize_explanation_payload(value)
            .context("failed to normalize fenced JSON payload from model response");
    }

    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}'))
        && let Ok(payload) = serde_json::from_str::<ExplanationPayload>(&raw[start..=end])
    {
        return Ok(payload);
    }
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}'))
        && let Ok(value) = serde_json::from_str::<Value>(&raw[start..=end])
    {
        return normalize_explanation_payload(value);
    }

    // Model returned prose or otherwise non-JSON content.
    let preview: String = raw.chars().take(300).collect();
    let hint = if is_prose_response(raw) {
        "model returned prose instead of JSON — use a model that follows JSON output instructions (e.g. GPT-4, Claude, Llama-3-8B+)"
    } else {
        "model returned malformed JSON — check the model or adjust your system_prompt"
    };
    bail!("{hint}.\nRaw response preview: {preview}")
}

fn extract_openai_text(value: &serde_json::Value) -> Option<String> {
    let message = value
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?;
    if let Some(text) = message.as_str() {
        return Some(text.to_string());
    }
    let parts = message.as_array()?;
    let joined = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

fn extract_anthropic_text(value: &serde_json::Value) -> Option<String> {
    let parts = value.get("content")?.as_array()?;
    let joined = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::{
        FixPayload, build_fix_system_prompt, build_fix_user_prompt, build_system_prompt,
        build_user_prompt, normalize_fix_payload, parse_fix_payload, parse_json_payload,
        validate_explanation_payload, validate_fix_payload,
    };
    use crate::config::Config;
    use fidan_driver::{
        AiCallNode, AiDependency, AiDeterministicExplainLine, AiDiagnosticSummary,
        AiExplainContext, AiFixHunk, AiFixMode, AiOutlineItem, AiRuntimeTrace, AiSymbolRef,
        AiTraceStep, AiTypedBinding,
    };
    use std::path::Path;
    use std::path::PathBuf;

    fn raw_payload() -> &'static str {
        r#"{
  "summary": "Summary",
  "input_output_behavior": "IO",
  "dependencies": "Deps",
  "possible_edge_cases": "Edges",
  "why_pattern_is_used": "Why",
  "related_symbols": "Symbols",
  "underlying_behaviour": "Behaviour"
}"#
    }

    #[test]
    fn parse_json_payload_accepts_raw_json() {
        let payload = parse_json_payload(raw_payload()).expect("parse raw payload");
        assert_eq!(payload.summary, "Summary");
        assert_eq!(payload.underlying_behaviour, "Behaviour");
    }

    #[test]
    fn parse_json_payload_accepts_fenced_json() {
        let text = format!("Here is the result:\n```json\n{}\n```", raw_payload());
        let payload = parse_json_payload(&text).expect("parse fenced payload");
        assert_eq!(payload.dependencies, "Deps");
        assert_eq!(payload.related_symbols, "Symbols");
    }

    #[test]
    fn parse_json_payload_accepts_uppercase_json_fence() {
        let text = format!("Here is the result:\n```JSON\n{}\n```", raw_payload());
        let payload = parse_json_payload(&text).expect("parse uppercase fenced payload");
        assert_eq!(payload.summary, "Summary");
    }

    #[test]
    fn parse_json_payload_accepts_generic_fence() {
        let text = format!("Here is the result:\n```\n{}\n```", raw_payload());
        let payload = parse_json_payload(&text).expect("parse generic fenced payload");
        assert_eq!(payload.dependencies, "Deps");
    }

    #[test]
    fn parse_json_payload_extracts_embedded_object() {
        let text = format!("The explanation is {}\nThanks.", raw_payload());
        let payload = parse_json_payload(&text).expect("parse embedded payload");
        assert_eq!(payload.input_output_behavior, "IO");
        assert_eq!(payload.possible_edge_cases, "Edges");
    }

    #[test]
    fn parse_json_payload_normalizes_array_fields_to_strings() {
        let payload = parse_json_payload(
            r#"{
  "summary": ["First line", "Second line"],
  "input_output_behavior": "Reads input and prints output.",
  "dependencies": ["std.io.print", "std.math"],
  "possible_edge_cases": ["Empty input"],
  "why_pattern_is_used": "Keeps the entry point explicit.",
  "related_symbols": ["main", "helper"],
  "underlying_behaviour": ["Calls helper()", "Prints the result"]
}"#,
        )
        .expect("normalize array-valued explanation fields");

        assert_eq!(payload.summary, "First line\nSecond line");
        assert_eq!(payload.dependencies, "std.io.print\nstd.math");
        assert_eq!(payload.related_symbols, "main\nhelper");
    }

    #[test]
    fn validate_explanation_payload_rejects_empty_fields() {
        let err = validate_explanation_payload(&super::ExplanationPayload {
            summary: "Summary".to_string(),
            input_output_behavior: String::new(),
            dependencies: "Deps".to_string(),
            possible_edge_cases: "Edges".to_string(),
            why_pattern_is_used: "Why".to_string(),
            related_symbols: "Symbols".to_string(),
            underlying_behaviour: "Behaviour".to_string(),
        })
        .expect_err("empty explanation fields should be rejected");

        assert!(err.to_string().contains("input_output_behavior"));
    }

    fn config_with_prompt(system_prompt: Option<&str>, replace_system_prompt: bool) -> Config {
        Config {
            schema_version: 1,
            provider: "openai-compatible".to_string(),
            model: "mock-model".to_string(),
            base_url: None,
            api_key_env: None,
            keyring_account: None,
            timeout_secs: 60,
            system_prompt: system_prompt.map(ToOwned::to_owned),
            replace_system_prompt,
        }
    }

    #[test]
    fn build_system_prompt_appends_custom_instructions_by_default() {
        let prompt = build_system_prompt(&config_with_prompt(Some("Focus on pedagogy."), false));
        assert!(prompt.contains("You are the Fidan AI analysis assistant."));
        assert!(prompt.contains("Additional instructions:"));
        assert!(prompt.contains("Focus on pedagogy."));
    }

    #[test]
    fn build_system_prompt_can_replace_default_prompt() {
        let prompt = build_system_prompt(&config_with_prompt(Some("Only do X."), true));
        assert_eq!(prompt, "Only do X.");
    }

    #[test]
    fn build_system_prompt_hardens_schema_requirements() {
        let prompt = build_system_prompt(&config_with_prompt(None, false));
        assert!(prompt.contains("Return exactly one JSON object."));
        assert!(prompt.contains("Every field value must be a single JSON string."));
        assert!(prompt.contains("inferred types"));
        assert!(prompt.contains("diagnostics"));
    }

    #[test]
    fn build_fix_system_prompt_allows_minimal_structural_supporting_edits() {
        let prompt =
            build_fix_system_prompt(&config_with_prompt(None, false), AiFixMode::Diagnostics);
        assert!(prompt.contains("You may change lines that do not themselves carry a diagnostic"));
        assert!(prompt.contains("Structural fixes are allowed when necessary"));
        assert!(prompt.contains("keep the edit set minimal"));
    }

    #[test]
    fn build_user_prompt_includes_compiler_backed_context() {
        let prompt = build_user_prompt(
            &AiExplainContext {
                file: PathBuf::from("sample.fdn"),
                line_start: 2,
                line_end: 4,
                total_lines: 8,
                selected_source: "let total = values[0] + values[1]".to_string(),
                deterministic_lines: vec![AiDeterministicExplainLine {
                    line: 2,
                    source: "let total = values[0] + values[1]".to_string(),
                    what_it_does: "adds two indexed values".to_string(),
                    inferred_type: Some("integer".to_string()),
                    reads: vec!["values".to_string()],
                    writes: vec!["total".to_string()],
                    risks: vec!["index out of bounds".to_string()],
                }],
                module_outline: vec![AiOutlineItem {
                    kind: "action".to_string(),
                    name: "main".to_string(),
                    line: 1,
                    detail: Some("0 parameter(s)".to_string()),
                }],
                dependencies: vec![AiDependency {
                    path: "std.io.print".to_string(),
                    alias: Some("print".to_string()),
                    is_re_export: false,
                }],
                related_symbols: vec![AiSymbolRef {
                    name: "values".to_string(),
                    kind: "var".to_string(),
                    file: PathBuf::from("sample.fdn"),
                    line: 1,
                    snippet: "let values = [1, 2]".to_string(),
                    detail: Some("list literal".to_string()),
                }],
                diagnostics: vec![AiDiagnosticSummary {
                    severity: "warning".to_string(),
                    code: "W1001".to_string(),
                    message: "possible bounds issue".to_string(),
                    line: 2,
                }],
                call_graph: vec![],
                type_map: vec![],
                runtime_trace: None,
            },
            Some("Explain for debugging."),
        );

        assert!(prompt.contains("Explain for debugging."));
        assert!(prompt.contains("inferred_type: integer"));
        assert!(prompt.contains("Module outline:"));
        assert!(prompt.contains("action main at line 1"));
        assert!(prompt.contains("std.io.print as print"));
        assert!(prompt.contains("warning W1001 at line 2: possible bounds issue"));
        assert!(prompt.contains("list literal"));
        assert!(prompt.contains("distinguish between behaviour guaranteed by the static code"));
    }

    #[test]
    fn build_fix_user_prompt_includes_compiler_backed_context() {
        let prompt = build_fix_user_prompt(
            Path::new("sample.fdn"),
            "action main {\n    var total set values[0] + values[1]\n}\n",
            &[AiDiagnosticSummary {
                severity: "warning".to_string(),
                code: "W1001".to_string(),
                message: "possible bounds issue".to_string(),
                line: 2,
            }],
            Some(&AiExplainContext {
                file: PathBuf::from("sample.fdn"),
                line_start: 1,
                line_end: 3,
                total_lines: 3,
                selected_source: "action main {\n    var total set values[0] + values[1]\n}"
                    .to_string(),
                deterministic_lines: vec![AiDeterministicExplainLine {
                    line: 2,
                    source: "var total set values[0] + values[1]".to_string(),
                    what_it_does: "adds two indexed values".to_string(),
                    inferred_type: Some("integer".to_string()),
                    reads: vec!["values".to_string()],
                    writes: vec!["total".to_string()],
                    risks: vec!["index out of bounds".to_string()],
                }],
                module_outline: vec![AiOutlineItem {
                    kind: "action".to_string(),
                    name: "main".to_string(),
                    line: 1,
                    detail: Some("0 parameter(s)".to_string()),
                }],
                dependencies: vec![AiDependency {
                    path: "std.io.print".to_string(),
                    alias: Some("print".to_string()),
                    is_re_export: false,
                }],
                related_symbols: vec![AiSymbolRef {
                    name: "values".to_string(),
                    kind: "var".to_string(),
                    file: PathBuf::from("sample.fdn"),
                    line: 1,
                    snippet: "var values set [1, 2]".to_string(),
                    detail: Some("list literal".to_string()),
                }],
                diagnostics: vec![AiDiagnosticSummary {
                    severity: "warning".to_string(),
                    code: "W1001".to_string(),
                    message: "possible bounds issue".to_string(),
                    line: 2,
                }],
                call_graph: vec![AiCallNode {
                    caller: "main".to_string(),
                    callees: vec!["print".to_string()],
                    line: 1,
                    is_recursive: false,
                }],
                type_map: vec![AiTypedBinding {
                    name: "total".to_string(),
                    inferred_type: "integer".to_string(),
                    line: 2,
                    kind: "var".to_string(),
                }],
                runtime_trace: Some(AiRuntimeTrace {
                    steps: vec![AiTraceStep {
                        kind: "assign".to_string(),
                        description: "compute total".to_string(),
                        line: Some(2),
                        value: None,
                    }],
                    truncated: false,
                }),
            }),
            AiFixMode::Diagnostics,
            Some("Prefer the narrowest safe fix."),
        );

        assert!(prompt.contains("Prefer the narrowest safe fix."));
        assert!(prompt.contains("Deterministic line analysis:"));
        assert!(prompt.contains("inferred_type: integer"));
        assert!(prompt.contains("Static call graph:"));
        assert!(prompt.contains("line 1: main → print"));
        assert!(prompt.contains("Inferred type map:"));
        assert!(prompt.contains("line 2: var total : integer"));
        assert!(prompt.contains("Static execution trace:"));
        assert!(prompt.contains("[assign] line 2: compute total"));
    }

    #[test]
    fn validate_fix_payload_rejects_noop_hunks() {
        let payload = FixPayload {
            summary: "Added a compute action to resolve the undefined name error.".to_string(),
            hunks: vec![AiFixHunk {
                line_start: 3,
                line_end: 3,
                old_text: "    print(compute(result))".to_string(),
                new_text: "    print(compute(result))".to_string(),
                reason: "No change".to_string(),
            }],
        };

        let err = validate_fix_payload(
            "action main {\n    var result = greet(\"World\")\n    print(compute(result))\n}\n",
            &[AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `compute`".to_string(),
                line: 3,
            }],
            &payload,
        )
        .expect_err("no-op payload should be rejected");

        let message = err.to_string();
        assert!(message.contains("no-op"));
        assert!(message.contains("summary claims a source change"));
    }

    #[test]
    fn validate_fix_payload_rejects_empty_hunks_when_errors_remain() {
        let payload = FixPayload {
            summary: "No fixes needed.".to_string(),
            hunks: vec![],
        };

        let err = validate_fix_payload(
            "action main {\n    print(compute(result))\n}\n",
            &[AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `compute`".to_string(),
                line: 2,
            }],
            &payload,
        )
        .expect_err("empty payload should be rejected while errors remain");

        assert!(err.to_string().contains("contains no fix hunks"));
    }

    #[test]
    fn validate_fix_payload_accepts_real_insert_style_edit() {
        let payload = FixPayload {
            summary: "Inserted a new line between two existing lines.".to_string(),
            hunks: vec![AiFixHunk {
                line_start: 2,
                line_end: 3,
                old_text: "    first()\n    second()".to_string(),
                new_text: "    first()\n    inserted()\n    second()".to_string(),
                reason: "E0101".to_string(),
            }],
        };

        validate_fix_payload(
            "action main {\n    first()\n    second()\n}\n",
            &[AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `inserted`".to_string(),
                line: 2,
            }],
            &payload,
        )
        .expect("real edit payload should be accepted");
    }

    #[test]
    fn validate_fix_payload_accepts_insertion_only_hunk() {
        let payload = FixPayload {
            summary: "Inserted a helper action before main.".to_string(),
            hunks: vec![AiFixHunk {
                line_start: 4,
                line_end: 4,
                old_text: String::new(),
                new_text: "action helper returns integer {\n    return 1\n}".to_string(),
                reason: "E0101".to_string(),
            }],
        };

        validate_fix_payload(
            "action main {\n    print(1)\n}\nmain()\n",
            &[AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `helper`".to_string(),
                line: 4,
            }],
            &payload,
        )
        .expect("insertion-only payload should be accepted");
    }

    #[test]
    fn validate_fix_payload_rejects_non_consecutive_trimmed_match() {
        let payload = FixPayload {
            summary: "Replaced a broken consecutive block.".to_string(),
            hunks: vec![AiFixHunk {
                line_start: 2,
                line_end: 3,
                old_text: "    alpha()\n    gamma()".to_string(),
                new_text: "    alpha()\n    delta()".to_string(),
                reason: "E0101".to_string(),
            }],
        };

        let err = validate_fix_payload(
            "action main {\n    alpha()\n    beta()\n    gamma()\n}\n",
            &[AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `delta`".to_string(),
                line: 3,
            }],
            &payload,
        )
        .expect_err("non-consecutive trim-only matches should be rejected");

        assert!(err.to_string().contains("old_text does not appear"));
    }

    #[test]
    fn normalize_fix_payload_derives_line_end_from_old_text() {
        let payload = normalize_fix_payload(FixPayload {
            summary: "Added a helper action.".to_string(),
            hunks: vec![AiFixHunk {
                line_start: 13,
                line_end: 13,
                old_text: "    first()\n    second()\n    third()".to_string(),
                new_text: "    first()\n    inserted()\n    second()\n    third()".to_string(),
                reason: "E0101".to_string(),
            }],
        });

        assert_eq!(payload.hunks[0].line_end, 15);
    }

    #[test]
    fn normalize_fix_payload_drops_noop_hunks() {
        let payload = normalize_fix_payload(FixPayload {
            summary: "Mixed payload.".to_string(),
            hunks: vec![
                AiFixHunk {
                    line_start: 10,
                    line_end: 10,
                    old_text: "    unchanged()".to_string(),
                    new_text: "    unchanged()".to_string(),
                    reason: "No change".to_string(),
                },
                AiFixHunk {
                    line_start: 11,
                    line_end: 11,
                    old_text: "    old()".to_string(),
                    new_text: "    new()".to_string(),
                    reason: "E0101".to_string(),
                },
            ],
        });

        assert_eq!(payload.hunks.len(), 1);
        assert_eq!(payload.hunks[0].old_text, "    old()");
    }

    #[test]
    fn parse_fix_payload_accepts_uppercase_json_fence() {
        let text = r#"```JSON
{
  "summary": "Replace old call.",
  "hunks": [
    {
      "line_start": 2,
      "line_end": 2,
      "old_text": "    old()",
      "new_text": "    new()",
      "reason": "E0101"
    }
  ]
}
```"#;

        let payload = parse_fix_payload(text).expect("parse uppercase fenced fix payload");
        assert_eq!(payload.summary, "Replace old call.");
        assert_eq!(payload.hunks.len(), 1);
        assert_eq!(payload.hunks[0].new_text, "    new()");
    }

    #[test]
    fn parse_fix_payload_accepts_generic_fence() {
        let text = r#"```
{
  "summary": "Insert helper.",
  "hunks": [
    {
      "line_start": 4,
      "line_end": 4,
      "old_text": "",
      "new_text": "action helper {}",
      "reason": "E0101"
    }
  ]
}
```"#;

        let payload = parse_fix_payload(text).expect("parse generic fenced fix payload");
        assert_eq!(payload.summary, "Insert helper.");
        assert_eq!(payload.hunks.len(), 1);
        assert_eq!(payload.hunks[0].line_start, 4);
    }
}
