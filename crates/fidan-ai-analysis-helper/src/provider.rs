use crate::config::{Config, resolve_api_key};
use anyhow::{Context, Result, bail};
use fidan_driver::AiExplainContext;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

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
    let raw = match config.provider.trim().to_ascii_lowercase().as_str() {
        "openai-compatible" | "openai" => {
            call_openai_compatible(config, api_key.as_deref(), &system_prompt, &user_prompt)?
        }
        "anthropic" => call_anthropic(config, api_key.as_deref(), &system_prompt, &user_prompt)?,
        other => bail!("unsupported ai-analysis provider `{other}`"),
    };
    let payload: ExplanationPayload =
        parse_json_payload(&raw).context("model response was not valid ai-analysis JSON")?;
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
        "max_tokens": 1200,
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

    format!(
        "{prompt_text}\n\nAnalysis contract:\n- Use the provided deterministic analysis, inferred types, reads/writes, risks, diagnostics, dependencies, and related symbols as primary evidence.\n- If you mention runtime behaviour, distinguish between behaviour guaranteed by the static code and behaviour that depends on called code or runtime values.\n- Do not claim that code executes, loops, returns, panics, or performs I/O unless the provided evidence supports that claim.\n- If a called symbol is not defined in the selected context, say that its downstream behaviour depends on that symbol.\n\nTarget file: {}\nTarget lines: {}-{}\nTotal file lines: {}\n\nSelected source:\n{}\n\nDeterministic analysis:\n{}\n\nModule outline:\n{}\n\nDependencies:\n{}\n\nRelated symbols:\n{}\n\nDiagnostics touching the selected range:\n{}\n",
        context.file.display(),
        context.line_start,
        context.line_end,
        context.total_lines,
        context.selected_source,
        if deterministic.is_empty() {
            "(none)"
        } else {
            deterministic.as_str()
        },
        if module_outline.is_empty() {
            "(none)"
        } else {
            module_outline.as_str()
        },
        if dependencies.is_empty() {
            "(none)"
        } else {
            dependencies.as_str()
        },
        if related_symbols.is_empty() {
            "(none)"
        } else {
            related_symbols.as_str()
        },
        if diagnostics.is_empty() {
            "(none)"
        } else {
            diagnostics.as_str()
        },
    )
}

fn default_system_prompt() -> String {
    r#"You are the Fidan AI analysis assistant.

Your occupation:
- You are a senior software engineer with deep experience in code comprehension and explanation, especially for Fidan code.
- You have a strong pedagogical instinct and are skilled at breaking down complex technical concepts into clear, precise, and beginner-friendly explanations.
- You have a deep understanding of Fidan, its patterns, and its ecosystem, and you are familiar with the common pitfalls and edge cases that arise in Fidan code.
- You are a helpful, patient, and thorough explainer, and you take care to ensure that your explanations are accurate, grounded in the provided context, and accessible to a wide range of readers.

Your job:
- Explain Fidan code clearly, accurately, and concretely.
- Stay grounded in the provided source, deterministic analysis, inferred types, diagnostics, module outline, dependencies, and related symbols.
- Treat the compiler-derived context as authoritative evidence.
- Do not invent runtime facts, dependencies, or behavior that are not supported by the context.
- Separate what is statically certain from what depends on other symbols, runtime inputs, or code not shown here.
- Prefer precise beginner-friendly wording over vague expert jargon unless prompted otherwise.

Output contract:
- Return only valid JSON.
- Do not wrap the JSON in Markdown fences.
- Return exactly one JSON object.
- Use these exact string fields and no others:
  - summary
  - input_output_behavior
  - dependencies
  - possible_edge_cases
  - why_pattern_is_used
  - related_symbols
  - underlying_behaviour
- Every field value must be a single JSON string.
- Do not use arrays, objects, numbers, booleans, or null for any field.

Quality bar:
- Explain what the code actually does, not just its syntax shape.
- Use inferred types, reads, writes, risks, diagnostics, and declarations when they help make the explanation more concrete.
- Prefer call flow, data flow, side effects, error conditions, and symbol relationships over surface-level syntax commentary.
- Mention uncertainty implicitly by staying within the evidence.
- Keep the response structured, readable, and technically grounded."#
        .to_string()
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
            .unwrap_or_else(default_system_prompt);
    }

    match custom {
        Some(custom) => format!(
            "{}\n\nAdditional instructions:\n{}",
            default_system_prompt(),
            custom
        ),
        None => default_system_prompt(),
    }
}

fn parse_json_payload(raw: &str) -> Result<ExplanationPayload> {
    if let Ok(payload) = serde_json::from_str::<ExplanationPayload>(raw) {
        return Ok(payload);
    }

    if let Some(start) = raw.find("```json")
        && let Some(end) = raw[start + 7..].find("```")
    {
        let body = &raw[start + 7..start + 7 + end];
        return serde_json::from_str(body.trim())
            .context("failed to parse fenced JSON payload from model response");
    }

    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
        return serde_json::from_str(&raw[start..=end])
            .context("failed to parse JSON object from model response");
    }

    bail!("response did not contain a parseable JSON object")
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
    use super::{build_system_prompt, build_user_prompt, parse_json_payload};
    use crate::config::Config;
    use fidan_driver::{
        AiDependency, AiDeterministicExplainLine, AiDiagnosticSummary, AiExplainContext,
        AiOutlineItem, AiSymbolRef,
    };
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
    fn parse_json_payload_extracts_embedded_object() {
        let text = format!("The explanation is {}\nThanks.", raw_payload());
        let payload = parse_json_payload(&text).expect("parse embedded payload");
        assert_eq!(payload.input_output_behavior, "IO");
        assert_eq!(payload.possible_edge_cases, "Edges");
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
}
