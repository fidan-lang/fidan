mod config;
mod fidan_client;
mod mcp;
mod provider;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fidan_driver::{
    AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AiAnalysisHelperCommand, AiAnalysisHelperRequest,
    AiAnalysisHelperResponse, AiAnalysisHelperResult, AiStructuredExplanation,
};
use std::io::{Read, Write};
use std::path::PathBuf;

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
    println!("- doctor");
    println!("- login --api-key <token> [--keyring-account <account>]");
    println!("- logout [--keyring-account <account>]");
    println!("- mcp");
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
    use super::{render_ai_doctor_report, select_keyring_account};
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
}
