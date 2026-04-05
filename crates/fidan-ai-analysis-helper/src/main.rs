mod config;
mod fidan_client;
mod mcp;
mod provider;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fidan_driver::{
    AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AiAnalysisHelperCommand, AiAnalysisHelperRequest,
    AiAnalysisHelperResponse, AiAnalysisHelperResult, AiFixResult, AiStructuredExplanation,
};
use reqwest::blocking::Client;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "fidan-ai-analysis-helper",
    about = "AI analysis helper process for Fidan toolchains"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one-shot AI analysis for `fidan explain --ai`
    Analyze {
        #[arg(long)]
        request: Option<PathBuf>,
        #[arg(long)]
        response: Option<PathBuf>,
    },
    /// Expose Fidan analysis over MCP stdio
    Mcp,
    /// Execute a registered external namespace command
    Exec {
        namespace: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fidan-ai-analysis-helper: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Analyze { request, response } => {
            handle_analyze(request.as_ref(), response.as_ref())
        }
        Command::Mcp => mcp::serve(),
        Command::Exec { namespace, args } => handle_exec(&namespace, &args),
    }
}

fn handle_analyze(request_path: Option<&PathBuf>, response_path: Option<&PathBuf>) -> Result<()> {
    match (request_path, response_path) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => bail!(
            "`analyze` expects either both --request/--response paths or neither (stdin/stdout mode)"
        ),
    }

    let request_bytes = match request_path {
        Some(path) => {
            std::fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?
        }
        None => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut bytes)
                .context("failed to read ai-analysis helper request from stdin")?;
            bytes
        }
    };
    let request: AiAnalysisHelperRequest = serde_json::from_slice(&request_bytes)
        .context("failed to parse ai-analysis helper request")?;

    let response = if request.protocol_version != AI_ANALYSIS_HELPER_PROTOCOL_VERSION {
        AiAnalysisHelperResponse {
            protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
            success: false,
            result: None,
            error: Some(format!(
                "ai-analysis helper protocol mismatch (request={}, helper={})",
                request.protocol_version, AI_ANALYSIS_HELPER_PROTOCOL_VERSION
            )),
        }
    } else {
        match handle_helper_request(request.command) {
            Ok(result) => AiAnalysisHelperResponse {
                protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
                success: true,
                result: Some(result),
                error: None,
            },
            Err(error) => AiAnalysisHelperResponse {
                protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
                success: false,
                result: None,
                error: Some(format!("{error:#}")),
            },
        }
    };

    let response_bytes =
        serde_json::to_vec(&response).context("failed to serialize ai-analysis helper response")?;
    match response_path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create `{}`", parent.display()))?;
            }
            std::fs::write(path, response_bytes)
                .with_context(|| format!("failed to write `{}`", path.display()))?;
        }
        None => {
            std::io::stdout()
                .write_all(&response_bytes)
                .context("failed to write ai-analysis helper response to stdout")?;
        }
    }
    Ok(())
}

fn handle_helper_request(command: AiAnalysisHelperCommand) -> Result<AiAnalysisHelperResult> {
    match command {
        AiAnalysisHelperCommand::Explain {
            file,
            line_start,
            line_end,
            prompt,
            fidan_path,
        } => {
            let explain_context = fidan_client::request_explain_context(
                fidan_path.as_deref(),
                &file,
                line_start,
                line_end,
            )?;
            let config = config::load()?;
            let rendered = provider::run_explain(&config, &explain_context, prompt.as_deref())?;
            Ok(AiAnalysisHelperResult::Explain(AiStructuredExplanation {
                model: rendered.model.clone(),
                provider: Some(rendered.provider.clone()),
                summary: rendered.summary,
                input_output_behavior: rendered.input_output_behavior,
                dependencies: rendered.dependencies,
                possible_edge_cases: rendered.possible_edge_cases,
                why_pattern_is_used: rendered.why_pattern_is_used,
                related_symbols: rendered.related_symbols,
                underlying_behaviour: rendered.underlying_behaviour,
            }))
        }
        AiAnalysisHelperCommand::Fix {
            file,
            source,
            diagnostics,
            mode,
            prompt,
        } => {
            let config = config::load()?;
            let fix_result = provider::run_fix(
                &config,
                &file,
                &source,
                &diagnostics,
                mode,
                prompt.as_deref(),
            )?;
            Ok(AiAnalysisHelperResult::Fix(AiFixResult {
                summary: fix_result.summary,
                hunks: fix_result.hunks,
                model: fix_result.model,
                provider: fix_result.provider,
            }))
        }
    }
}

fn handle_exec(namespace: &str, args: &[String]) -> Result<()> {
    match namespace {
        "ai" => handle_ai_exec(args),
        other => bail!("unsupported exec namespace `{other}` for ai-analysis helper"),
    }
}

fn handle_ai_exec(args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_ai_exec_usage();
        return Ok(());
    }

    match args[0].as_str() {
        "mcp" => mcp::serve(),
        "doctor" => run_ai_doctor(),
        "login" => run_ai_login(&args[1..]),
        "logout" => run_ai_logout(&args[1..]),
        "configure" => run_ai_configure(&args[1..]),
        "setup" => run_ai_setup(&args[1..]),
        "help" | "--help" | "-h" => {
            print_ai_exec_usage();
            Ok(())
        }
        other => bail!("unknown `fidan exec ai` subcommand `{other}` — run `fidan exec ai help`"),
    }
}

fn run_ai_doctor() -> Result<()> {
    let path = config::resolve_config_path()?;
    let config = config::load_if_present()?;
    let api_key_present = match config.as_ref() {
        Some(config) => config::resolve_api_key(config)?.is_some(),
        None => config::resolve_api_key_with(None, None)?.is_some(),
    };
    print!(
        "{}",
        render_ai_doctor_report(&path, config.as_ref(), api_key_present)
    );
    Ok(())
}

fn run_ai_login(args: &[String]) -> Result<()> {
    let mut api_key = None;
    let mut keyring_account_override = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--api-key" => {
                index += 1;
                let value = args
                    .get(index)
                    .context("`fidan exec ai login` requires a value after `--api-key`")?;
                api_key = Some(value.clone());
            }
            "--keyring-account" => {
                index += 1;
                let value = args
                    .get(index)
                    .context("`fidan exec ai login` requires a value after `--keyring-account`")?;
                keyring_account_override = Some(value.clone());
            }
            other => bail!(
                "unknown `fidan exec ai login` option `{other}` — supported: --api-key, --keyring-account"
            ),
        }
        index += 1;
    }

    let api_key = api_key.context("`fidan exec ai login` requires `--api-key <token>` for now")?;
    let loaded_config = config::load_if_present()?;
    let keyring_account =
        select_keyring_account(loaded_config.as_ref(), keyring_account_override.as_deref());
    config::store_api_key(&api_key, Some(&keyring_account))?;
    println!(
        "Stored ai-analysis API key in the OS keychain under account `{}`.",
        keyring_account
    );
    Ok(())
}

/// Core logic of `configure --set key=value`, separated from file I/O for testability.
///
/// Mutates `table` in-place and returns the human-readable list of updated/removed key names.
/// Guarantees that `schema_version` is present in `table` after the call.
fn process_configure_sets(
    table: &mut toml::Table,
    sets: &[(String, String)],
) -> Result<Vec<String>> {
    let mut updated_keys: Vec<String> = Vec::new();
    for (key, value) in sets {
        let key = key.as_str();
        let value = value.as_str();

        let should_remove = matches!(
            key,
            "base_url" | "api_key_env" | "keyring_account" | "system_prompt"
        ) && (value.is_empty() || value.eq_ignore_ascii_case("none"));

        if should_remove {
            table.remove(key);
            updated_keys.push(format!("{key} (removed)"));
            continue;
        }

        let toml_value = match key {
            "provider" | "model" => {
                if value.is_empty() {
                    bail!(
                        "`{key}` must not be empty — provide a value like `openai-compatible` or your model name"
                    );
                }
                toml::Value::String(value.to_string())
            }
            "base_url" | "api_key_env" | "keyring_account" | "system_prompt" => {
                toml::Value::String(value.to_string())
            }
            "timeout_secs" => {
                let secs: u64 = value.parse().with_context(|| {
                    format!("`timeout_secs` must be a positive integer, got `{value}`")
                })?;
                if secs == 0 {
                    bail!("`timeout_secs` must be greater than 0");
                }
                toml::Value::Integer(secs as i64)
            }
            "replace_system_prompt" => {
                let b = match value.to_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    _ => bail!("`replace_system_prompt` must be `true` or `false`, got `{value}`"),
                };
                toml::Value::Boolean(b)
            }
            _ => bail!(
                "unknown configuration key `{key}`\n\nValid keys: provider, model, base_url, api_key_env, keyring_account, timeout_secs, system_prompt, replace_system_prompt"
            ),
        };
        table.insert(key.to_string(), toml_value);
        updated_keys.push(key.to_string());
    }

    // Ensure schema_version is always present
    if !table.contains_key("schema_version") {
        table.insert("schema_version".to_string(), toml::Value::Integer(1));
    }

    Ok(updated_keys)
}

fn run_ai_configure(args: &[String]) -> Result<()> {
    let mut sets: Vec<(String, String)> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--set" => {
                index += 1;
                let pair = args
                    .get(index)
                    .context("`fidan exec ai configure` requires a value after `--set`")?;
                let eq = pair.find('=').with_context(|| {
                    format!("`--set` argument must be in `key=value` form, got `{pair}`")
                })?;
                sets.push((
                    pair[..eq].trim().to_string(),
                    pair[eq + 1..].trim().to_string(),
                ));
            }
            other => bail!(
                "unknown `fidan exec ai configure` option `{other}`\n\nUsage: fidan exec ai configure --set key=value"
            ),
        }
        index += 1;
    }

    let path = config::resolve_config_path()?;

    if sets.is_empty() {
        // Show current config
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read `{}`", path.display()))?;
            println!("# ai-analysis config: {}", path.display());
            println!();
            print!("{text}");
        } else {
            println!("No ai-analysis configuration found.");
            println!("Config path: {}", path.display());
            println!();
            println!("To configure, run:");
            println!(
                "  fidan exec ai configure --set provider=openai-compatible --set model=MODEL"
            );
            println!(
                "  fidan exec ai configure --set base_url=http://localhost:11434/v1/chat/completions"
            );
            println!("  fidan exec ai login --api-key YOUR_API_KEY");
        }
        println!();
        println!("Valid keys:");
        println!("  provider             AI provider: \"openai-compatible\" or \"anthropic\"");
        println!("  model                Model name (e.g., \"gpt-4.1-mini\", \"llama3.2\")");
        println!("  base_url             Override endpoint URL (\"none\" or empty to remove)");
        println!(
            "  api_key_env          Environment variable to read API key from (\"none\" to remove)"
        );
        println!(
            "  keyring_account      OS keychain account name for the API key (\"none\" to remove)"
        );
        println!("  timeout_secs         HTTP request timeout in seconds (default: 60)");
        println!(
            "  system_prompt        Extra instructions for the AI (\"none\" or empty to remove)"
        );
        println!("  replace_system_prompt  Replace built-in system prompt entirely (true/false)");
        return Ok(());
    }

    // Load existing table or start with a minimal one
    let mut table: toml::Table = if path.is_file() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse existing config at `{}`", path.display()))?
    } else {
        let mut t = toml::Table::new();
        t.insert("schema_version".to_string(), toml::Value::Integer(1));
        t
    };

    let updated_keys = process_configure_sets(&mut table, &sets)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory `{}`", parent.display()))?;
    }
    let text =
        toml::to_string_pretty(&table).context("failed to serialize ai-analysis configuration")?;
    std::fs::write(&path, &text)
        .with_context(|| format!("failed to write config to `{}`", path.display()))?;

    println!(
        "Updated {} in `{}`.",
        updated_keys.join(", "),
        path.display()
    );
    Ok(())
}

fn run_ai_logout(args: &[String]) -> Result<()> {
    let mut keyring_account_override = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--keyring-account" => {
                index += 1;
                let value = args
                    .get(index)
                    .context("`fidan exec ai logout` requires a value after `--keyring-account`")?;
                keyring_account_override = Some(value.clone());
            }
            other => bail!(
                "unknown `fidan exec ai logout` option `{other}` — supported: --keyring-account"
            ),
        }
        index += 1;
    }

    let loaded_config = config::load_if_present()?;
    let keyring_account =
        select_keyring_account(loaded_config.as_ref(), keyring_account_override.as_deref());
    config::clear_api_key(Some(&keyring_account))?;
    println!("Removed ai-analysis API key from the OS keychain for account `{keyring_account}`.");
    Ok(())
}

fn run_ai_setup(args: &[String]) -> Result<()> {
    if !args.is_empty() {
        bail!("`fidan exec ai setup` does not accept extra arguments");
    }

    let choice = prompt_menu(
        "Choose ai-analysis setup",
        &[
            "OpenAI-compatible cloud API",
            "Anthropic API",
            "Auto-detect local Ollama or LM Studio",
            "Custom OpenAI-compatible endpoint",
        ],
        3,
    )?;

    match choice {
        1 => configure_openai_cloud(),
        2 => configure_anthropic(),
        3 => configure_local_auto_detect(),
        4 => configure_custom_endpoint(),
        _ => bail!("invalid setup option `{choice}`"),
    }
}

fn configure_openai_cloud() -> Result<()> {
    let model = prompt_required("OpenAI-compatible model name:")?;
    let api_key = prompt_required("API key:")?;
    run_ai_configure_sets(&[
        ("provider", "openai-compatible"),
        ("model", &model),
        ("base_url", "none"),
        ("api_key_env", "none"),
        ("keyring_account", "none"),
    ])?;
    run_ai_login(&["--api-key".to_string(), api_key])
}

fn configure_anthropic() -> Result<()> {
    let model = prompt_required("Anthropic model name:")?;
    let api_key = prompt_required("Anthropic API key:")?;
    run_ai_configure_sets(&[
        ("provider", "anthropic"),
        ("model", &model),
        ("base_url", "none"),
        ("api_key_env", "none"),
        ("keyring_account", "none"),
    ])?;
    run_ai_login(&["--api-key".to_string(), api_key])
}

fn configure_local_auto_detect() -> Result<()> {
    let detections = [
        (
            "Ollama",
            "http://localhost:11434",
            "http://localhost:11434/v1/chat/completions",
            Some("llama3.2"),
        ),
        (
            "LM Studio",
            "http://localhost:1234",
            "http://localhost:1234/v1/chat/completions",
            None,
        ),
    ];

    let mut selected = None;
    for (name, probe_url, base_url, default_model) in detections {
        if endpoint_reachable(probe_url)?
            && prompt_yes_no(&format!("Detected {name} at {probe_url} — use it?"), true)?
        {
            selected = Some((base_url, default_model));
            break;
        }
    }

    let Some((base_url, default_model)) = selected else {
        bail!(
            "No supported local AI endpoint was selected — start Ollama or LM Studio, or choose the custom endpoint setup"
        );
    };

    let model = prompt_required_with_default("Local model name:", default_model)?;
    run_ai_configure_sets(&[
        ("provider", "openai-compatible"),
        ("model", &model),
        ("base_url", base_url),
        ("api_key_env", "FIDAN_AI_ANALYSIS_NO_API_KEY"),
        ("keyring_account", "none"),
    ])
}

fn configure_custom_endpoint() -> Result<()> {
    let base_url = prompt_required("Custom base URL:")?;
    let model = prompt_required("Model name:")?;
    let api_key = prompt_required("API key:")?;
    run_ai_configure_sets(&[
        ("provider", "openai-compatible"),
        ("model", &model),
        ("base_url", &base_url),
        ("api_key_env", "none"),
        ("keyring_account", "none"),
    ])?;
    run_ai_login(&["--api-key".to_string(), api_key])
}

fn run_ai_configure_sets(pairs: &[(&str, &str)]) -> Result<()> {
    let sets = pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    let path = config::resolve_config_path()?;
    let mut table: toml::Table = if path.is_file() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse existing config at `{}`", path.display()))?
    } else {
        let mut t = toml::Table::new();
        t.insert("schema_version".to_string(), toml::Value::Integer(1));
        t
    };
    let updated_keys = process_configure_sets(&mut table, &sets)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory `{}`", parent.display()))?;
    }
    let text =
        toml::to_string_pretty(&table).context("failed to serialize ai-analysis configuration")?;
    std::fs::write(&path, &text)
        .with_context(|| format!("failed to write config to `{}`", path.display()))?;
    println!(
        "Updated {} in `{}`.",
        updated_keys.join(", "),
        path.display()
    );
    Ok(())
}

fn select_keyring_account(config: Option<&config::Config>, explicit: Option<&str>) -> String {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            config
                .map(config::resolved_keyring_account)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| config::default_keyring_account().to_string())
}

fn print_ai_exec_usage() {
    println!("fidan exec ai <subcommand>");
    println!();
    println!("Available subcommands:");
    println!("- configure [--set key=value ...]  View or update the ai-analysis configuration");
    println!("- doctor");
    println!("- login --api-key <token> [--keyring-account <account>]");
    println!("- logout [--keyring-account <account>]");
    println!("- setup");
    println!("- mcp");
}

fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    eprint!("\x1b[1;33mconfirm\x1b[0m ");
    print!("\x1b[1m{prompt}\x1b[0m \x1b[36m{suffix}\x1b[0m ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read response")?;
    let trimmed = line.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

fn prompt_text(prompt: &str, default: Option<&str>) -> Result<String> {
    let suffix = default
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" [default: {value}]"))
        .unwrap_or_default();
    print!("\x1b[1;36minput\x1b[0m \x1b[1m{prompt}\x1b[0m{suffix} ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read input")?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(default.unwrap_or_default().to_string());
    }
    Ok(trimmed.to_string())
}

fn prompt_menu(prompt: &str, options: &[&str], default: usize) -> Result<usize> {
    eprintln!("\x1b[1;33mselect\x1b[0m {prompt}");
    for (index, option) in options.iter().enumerate() {
        eprintln!("  {}. {}", index + 1, option);
    }

    loop {
        let answer = prompt_text("Choose an option by number:", Some(&default.to_string()))?;
        let Ok(choice) = answer.parse::<usize>() else {
            eprintln!("Please enter a number between 1 and {}.", options.len());
            continue;
        };
        if (1..=options.len()).contains(&choice) {
            return Ok(choice);
        }
        eprintln!("Please enter a number between 1 and {}.", options.len());
    }
}

fn prompt_required(prompt: &str) -> Result<String> {
    prompt_required_with_default(prompt, None)
}

fn prompt_required_with_default(prompt: &str, default: Option<&str>) -> Result<String> {
    loop {
        let value = prompt_text(prompt, default)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        eprintln!("Value must not be empty.");
    }
}

fn endpoint_reachable(base_url: &str) -> Result<bool> {
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("failed to build local AI endpoint probe client")?;
    for candidate in [
        base_url.to_string(),
        format!("{}/v1/models", base_url.trim_end_matches('/')),
        format!("{}/api/tags", base_url.trim_end_matches('/')),
    ] {
        if client
            .get(&candidate)
            .send()
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn render_ai_doctor_report(
    path: &std::path::Path,
    config: Option<&config::Config>,
    api_key_present: bool,
) -> String {
    let config_status = if path.is_file() { "present" } else { "missing" };
    let provider = config
        .map(|config| config.provider.as_str())
        .unwrap_or("(not configured)");
    let model = config
        .map(|config| config.model.as_str())
        .unwrap_or("(not configured)");
    let base_url = config
        .and_then(|config| config.base_url.as_deref())
        .unwrap_or("(default)");
    let api_key_env = config
        .and_then(|config| config.api_key_env.as_deref())
        .unwrap_or("(none)");
    let keyring_account = config
        .map(config::resolved_keyring_account)
        .unwrap_or(config::default_keyring_account());
    let system_prompt = if config.is_some_and(|config| {
        config
            .system_prompt
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    }) {
        "custom"
    } else {
        "default"
    };
    let replace_system_prompt = config.is_some_and(|config| config.replace_system_prompt);

    format!(
        "ai-analysis doctor\n\
config: {} ({config_status})\n\
provider: {provider}\n\
model: {model}\n\
base_url: {base_url}\n\
api_key_env: {api_key_env}\n\
keyring_account: {keyring_account}\n\
api_key: {}\n\
system_prompt: {system_prompt}\n\
replace_system_prompt: {replace_system_prompt}\n",
        path.display(),
        if api_key_present {
            "present"
        } else {
            "missing"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{process_configure_sets, render_ai_doctor_report, select_keyring_account};
    use crate::config::Config;
    use std::path::Path;

    fn sample_config() -> Config {
        Config {
            schema_version: 1,
            provider: "openai-compatible".to_string(),
            model: "gpt-4.1-mini".to_string(),
            base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            api_key_env: Some("FIDAN_AI_ANALYSIS_API_KEY".to_string()),
            keyring_account: Some("custom-ai-account".to_string()),
            timeout_secs: 60,
            system_prompt: Some("Extra steering".to_string()),
            replace_system_prompt: true,
        }
    }

    #[test]
    fn doctor_report_handles_missing_config() {
        let report = render_ai_doctor_report(Path::new("missing.toml"), None, false);
        assert!(report.contains("config: missing.toml (missing)"));
        assert!(report.contains("provider: (not configured)"));
        assert!(report.contains("model: (not configured)"));
        assert!(report.contains("keyring_account: ai_analysis_api_key"));
        assert!(report.contains("api_key: missing"));
        assert!(report.contains("system_prompt: default"));
        assert!(report.contains("replace_system_prompt: false"));
    }

    #[test]
    fn doctor_report_uses_loaded_config_values() {
        let report =
            render_ai_doctor_report(Path::new("config.toml"), Some(&sample_config()), true);
        assert!(report.contains("config: config.toml (missing)"));
        assert!(report.contains("provider: openai-compatible"));
        assert!(report.contains("model: gpt-4.1-mini"));
        assert!(report.contains("base_url: http://127.0.0.1:11434/v1/chat/completions"));
        assert!(report.contains("api_key_env: FIDAN_AI_ANALYSIS_API_KEY"));
        assert!(report.contains("keyring_account: custom-ai-account"));
        assert!(report.contains("api_key: present"));
        assert!(report.contains("system_prompt: custom"));
        assert!(report.contains("replace_system_prompt: true"));
    }

    #[test]
    fn select_keyring_account_prefers_explicit_override() {
        assert_eq!(
            select_keyring_account(Some(&sample_config()), Some("override-account")),
            "override-account"
        );
    }

    #[test]
    fn select_keyring_account_uses_config_value_when_present() {
        assert_eq!(
            select_keyring_account(Some(&sample_config()), None),
            "custom-ai-account"
        );
    }

    #[test]
    fn select_keyring_account_falls_back_to_default() {
        assert_eq!(
            select_keyring_account(None, Some("   ")),
            "ai_analysis_api_key"
        );
    }

    // --- process_configure_sets ---

    fn empty_table() -> toml::Table {
        toml::Table::new()
    }

    fn sets(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn configure_sets_string_keys() {
        let mut t = empty_table();
        let updated = process_configure_sets(
            &mut t,
            &sets(&[("provider", "openai-compatible"), ("model", "llama3.2")]),
        )
        .unwrap();
        assert_eq!(
            t["provider"],
            toml::Value::String("openai-compatible".into())
        );
        assert_eq!(t["model"], toml::Value::String("llama3.2".into()));
        assert!(updated.contains(&"provider".to_string()));
        assert!(updated.contains(&"model".to_string()));
    }

    #[test]
    fn configure_sets_optional_string() {
        let mut t = empty_table();
        process_configure_sets(&mut t, &sets(&[("base_url", "http://localhost:11434")])).unwrap();
        assert_eq!(
            t["base_url"],
            toml::Value::String("http://localhost:11434".into())
        );
    }

    #[test]
    fn configure_removes_optional_key_with_none_literal() {
        let mut t = empty_table();
        t.insert("base_url".to_string(), toml::Value::String("old".into()));
        let updated = process_configure_sets(&mut t, &sets(&[("base_url", "none")])).unwrap();
        assert!(!t.contains_key("base_url"));
        assert!(updated.iter().any(|k| k.contains("removed")));
    }

    #[test]
    fn configure_removes_optional_key_with_empty_value() {
        let mut t = empty_table();
        t.insert(
            "system_prompt".to_string(),
            toml::Value::String("old".into()),
        );
        process_configure_sets(&mut t, &sets(&[("system_prompt", "")])).unwrap();
        assert!(!t.contains_key("system_prompt"));
    }

    #[test]
    fn configure_parses_timeout_secs() {
        let mut t = empty_table();
        process_configure_sets(&mut t, &sets(&[("timeout_secs", "120")])).unwrap();
        assert_eq!(t["timeout_secs"], toml::Value::Integer(120));
    }

    #[test]
    fn configure_rejects_zero_timeout() {
        let mut t = empty_table();
        let err = process_configure_sets(&mut t, &sets(&[("timeout_secs", "0")])).unwrap_err();
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn configure_rejects_non_integer_timeout() {
        let mut t = empty_table();
        let err = process_configure_sets(&mut t, &sets(&[("timeout_secs", "fast")])).unwrap_err();
        assert!(err.to_string().contains("positive integer"));
    }

    #[test]
    fn configure_parses_boolean_replace_system_prompt() {
        for (input, expected) in [
            ("true", true),
            ("false", false),
            ("1", true),
            ("off", false),
            ("yes", true),
        ] {
            let mut t = empty_table();
            process_configure_sets(&mut t, &sets(&[("replace_system_prompt", input)])).unwrap();
            assert_eq!(
                t["replace_system_prompt"],
                toml::Value::Boolean(expected),
                "input={input}"
            );
        }
    }

    #[test]
    fn configure_rejects_invalid_boolean() {
        let mut t = empty_table();
        let err = process_configure_sets(&mut t, &sets(&[("replace_system_prompt", "maybe")]))
            .unwrap_err();
        assert!(err.to_string().contains("true` or `false"));
    }

    #[test]
    fn configure_rejects_empty_provider() {
        let mut t = empty_table();
        let err = process_configure_sets(&mut t, &sets(&[("provider", "")])).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn configure_rejects_unknown_key() {
        let mut t = empty_table();
        let err = process_configure_sets(&mut t, &sets(&[("typo_key", "val")])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown configuration key"));
        assert!(msg.contains("Valid keys:"));
    }

    #[test]
    fn configure_adds_schema_version_when_missing() {
        let mut t = empty_table();
        process_configure_sets(&mut t, &sets(&[("provider", "openai-compatible")])).unwrap();
        assert_eq!(t["schema_version"], toml::Value::Integer(1));
    }

    #[test]
    fn configure_does_not_overwrite_existing_schema_version() {
        let mut t = empty_table();
        t.insert("schema_version".to_string(), toml::Value::Integer(1));
        t.insert(
            "provider".to_string(),
            toml::Value::String("existing".into()),
        );
        process_configure_sets(&mut t, &sets(&[("model", "gpt-4.1-mini")])).unwrap();
        assert_eq!(t["schema_version"], toml::Value::Integer(1));
        assert_eq!(t["provider"], toml::Value::String("existing".into()));
    }

    #[test]
    fn configure_preserves_unrelated_keys() {
        let mut t = empty_table();
        t.insert(
            "provider".to_string(),
            toml::Value::String("anthropic".into()),
        );
        t.insert(
            "model".to_string(),
            toml::Value::String("claude-3-5-sonnet".into()),
        );
        process_configure_sets(&mut t, &sets(&[("timeout_secs", "30")])).unwrap();
        assert_eq!(t["provider"], toml::Value::String("anthropic".into()));
        assert_eq!(t["model"], toml::Value::String("claude-3-5-sonnet".into()));
        assert_eq!(t["timeout_secs"], toml::Value::Integer(30));
    }
}
