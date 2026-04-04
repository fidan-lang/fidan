#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use fidan_diagnostics::{Severity, render_backtrace_to_stderr, render_message_to_stderr};
use fidan_driver::{
    CompileOptions, EmitKind, ExecutionMode, LtoMode, OptLevel, SandboxPolicy, StripMode, TraceMode,
};
use std::path::PathBuf;

mod ai_analysis;
mod dal;
mod distribution;
mod exec;
mod explain;
mod fix;
mod imports;
mod last_error;
mod pipeline;
mod prompts;
mod repl;
mod replay;
mod self_cmd;
mod toolchain;

// On Windows, PowerShell's default code page is CP1252 which corrupts the UTF-8
// box-drawing characters emitted by ariadne.  Switch both the input and output
// console code pages to UTF-8 (65001) as early as possible.
#[cfg(target_os = "windows")]
fn ensure_utf8_console() {
    unsafe extern "system" {
        fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
        fn SetConsoleCP(wCodePageID: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}
#[cfg(not(target_os = "windows"))]
fn ensure_utf8_console() {}

#[derive(Parser)]
#[command(
    name    = "fidan",
    version,
    about   = "The Fidan language compiler and toolchain",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a Fidan source file (pass `-` to read from stdin)
    Run {
        /// Path to the .fdn source file, or `-` to read from stdin
        file: PathBuf,
        /// Emit intermediate representation: tokens | ast | hir | mir
        #[arg(long, value_delimiter = ',')]
        emit: Vec<String>,
        /// Print the call stack on uncaught panics: none | short | full | compact
        #[arg(long, default_value = "none")]
        trace: String,
        /// Stop after this many errors (default: 1, 0 = no limit)
        #[arg(long, default_value = "1")]
        max_errors: usize,
        /// JIT compilation threshold: compile a function after this many calls (0 = off)
        #[arg(long, default_value = "500")]
        jit_threshold: u32,
        /// Treat select warnings (W1001–W1003, W2004–W2006) as errors
        #[arg(long)]
        strict: bool,
        /// Watch source files and re-run automatically on change
        #[arg(long)]
        reload: bool,
        /// Replay a previous run using a saved bundle ID (e.g. `a1b2c3d4`)
        /// or an explicit path to a `.bundle` file
        #[arg(long)]
        replay: Option<String>,
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
        /// Enable zero-config sandbox: deny file and environment access by default
        #[arg(long)]
        sandbox: bool,
        /// Allow file-system reads from path prefix (repeatable; `*` = allow all)
        #[arg(long, value_delimiter = ',')]
        allow_read: Vec<String>,
        /// Allow file-system writes to path prefix (repeatable; `*` = allow all)
        #[arg(long, value_delimiter = ',')]
        allow_write: Vec<String>,
        /// Allow environment variable access (`getEnv`, `setEnv`, `args`, `cwd`)
        #[arg(long)]
        allow_env: bool,
        /// Wall-time limit in seconds when `--sandbox` is active (0 = no limit)
        #[arg(long)]
        time_limit: Option<u64>,
        /// Memory limit in MB when `--sandbox` is active (0 = no limit)
        #[arg(long)]
        mem_limit: Option<u64>,
        /// Arguments passed through to the Fidan program itself. Use `--` before
        /// them when they could be mistaken for CLI flags.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compile a Fidan source file to a native binary
    Build {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Output binary path
        #[arg(short, long, default_value = "out")]
        output: PathBuf,
        /// Optimisation level: O0, O1, O2 (default), O3, Os, Oz
        #[arg(long, default_value = "O2")]
        opt: String,
        /// Full release profile: O3 + full LTO + strip all + host-tuned CPU
        /// unless explicitly overridden by the corresponding flags.
        #[arg(long)]
        release: bool,
        /// Link-time optimization mode for AOT builds: off | full
        #[arg(long, value_name = "off|full", default_value = "off")]
        lto: String,
        /// Strip the produced binary after linking: off | symbols | all
        #[arg(long, value_name = "off|symbols|all", default_value = "off")]
        strip: String,
        /// Emit intermediate representation: tokens | ast | hir | mir | obj
        /// (`obj` keeps the generated native .o/.obj file alongside the binary)
        #[arg(long, value_delimiter = ',')]
        emit: Vec<String>,
        /// Additional library search directories for the system linker (repeatable)
        #[arg(long)]
        lib_dir: Vec<PathBuf>,
        /// How to link the Fidan runtime into the compiled binary:
        /// `static` (default) — embed libfidan_runtime.a for a self-contained binary;
        /// `dynamic` — link libfidan_runtime.so/.dll (smaller binary, but the
        /// runtime shared library must be present at the Fidan install path at run time.
        #[arg(long, value_name = "static|dynamic", default_value = "static")]
        link_runtime: String,
        /// Override the system linker for this build.  Accepts a bare executable
        /// name (resolved via PATH) or an absolute path.  Equivalent to setting
        /// the FIDAN_LINKER environment variable, but applies to this invocation
        /// only.  Example: `--linker lld-link` or `--linker /usr/bin/clang`.
        #[arg(long)]
        linker: Option<String>,
        /// Treat select warnings (W1001–W1003, W2004–W2006) as errors
        #[arg(long)]
        strict: bool,
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
        /// AOT codegen backend: `auto` (prefer installed LLVM), `cranelift`, or `llvm`
        #[arg(long, default_value = "auto")]
        backend: String,
        /// Stop after this many errors (default: 1, 0 = no limit)
        #[arg(long, default_value = "1")]
        max_errors: usize,
        /// Target CPU for AOT codegen: `generic` (portable), `native` (host-tuned), or
        /// a backend-specific CPU name. LLVM fully supports this today; Cranelift
        /// currently treats `native`/omitted as host ISA and rejects other values.
        #[arg(long)]
        target_cpu: Option<String>,
    },
    /// Profile a Fidan source file: call counts, time per action, hot-path hints
    Profile {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Write profiling data to a file in JSON format
        #[arg(long)]
        profile_out: Option<PathBuf>,
        /// Stop after this many errors (default: 1, 0 = no limit)
        #[arg(long, default_value = "1")]
        max_errors: usize,
        /// Suppress specific diagnostic codes (comma-separated)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
    },
    /// Run `test { ... }` blocks in a Fidan source file
    Test {
        file: PathBuf,
        /// Stop after this many errors (default: 1, 0 = no limit)
        #[arg(long, default_value = "1")]
        max_errors: usize,
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
    },
    /// Start an interactive REPL (lex + parse + typecheck each line)
    Repl {
        /// Print the call stack on uncaught panics: none | short | full | compact
        #[arg(long, default_value = "short")]
        trace: String,
        /// Stop after this many errors for each submitted REPL input
        /// (default: 1, 0 = no limit)
        #[arg(long, default_value = "1")]
        max_errors_per_input: usize,
    },
    /// Start the language server (LSP)
    Lsp {
        /// Communicate over stdin/stdout.
        /// This flag is accepted for compatibility with LSP clients (e.g. VS Code's
        /// vscode-languageclient) that automatically append `--stdio` to the server
        /// process arguments when `TransportKind.stdio` is configured.  The Fidan
        /// LSP server always uses stdio, so the flag is a no-op.
        #[arg(long)]
        stdio: bool,
    },
    /// Format a Fidan source file
    Format {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Rewrite the file in place instead of printing to stdout
        #[arg(long)]
        in_place: bool,
        /// Exit 1 if the file is not already formatted (useful in CI)
        #[arg(long)]
        check: bool,
        /// Number of spaces per indent level (default: 4)
        #[arg(long)]
        indent_width: Option<usize>,
        /// Soft line-length limit (default: 100)
        #[arg(long)]
        max_line_len: Option<usize>,
    },
    /// Check a Fidan source file for errors without running it
    Check {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Stop after this many errors (0 = no limit)
        #[arg(long, default_value = "0")]
        max_errors: usize,
        /// Treat select warnings (W1001–W1003, W2004–W2006) as errors
        #[arg(long)]
        strict: bool,
        /// Suppress specific diagnostic codes (comma-separated, e.g. `W5003,W1004`)
        #[arg(long, value_delimiter = ',')]
        suppress: Vec<String>,
    },
    /// Apply high-confidence fix suggestions to a Fidan source file
    Fix {
        /// Path to the .fdn source file
        file: PathBuf,
        /// Print the proposed changes without writing to the file
        #[arg(long)]
        dry_run: bool,
    },
    /// Explain source lines, a diagnostic code, or the last recorded error
    #[command(name = "explain")]
    Explain {
        /// Path to the .fdn source file.
        /// You can also use `path/to/file.fdn:100` or `path/to/file.fdn:100-120`.
        target: Option<String>,
        /// First line to explain (1-based)
        #[arg(long)]
        line: Option<usize>,
        /// Last line to explain, inclusive (defaults to --line)
        #[arg(long)]
        end_line: Option<usize>,
        /// Explain a diagnostic code instead of source lines (for example `E0101`)
        #[arg(long)]
        diagnostic: Option<String>,
        /// Explain the last recorded diagnostic automatically
        #[arg(long)]
        last_error: bool,
        /// Use the installed AI analysis toolchain. Optionally pass extra steering text.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        ai: Option<String>,
    },
    /// Scaffold a new Fidan project in a new directory
    New {
        /// Name of the project (also the directory name)
        project_name: String,
        /// Output directory (default: current directory)
        #[arg(short, long)]
        dir: Option<PathBuf>,
        /// Scaffold a Dal package layout with dal.toml and src/init.fdn
        #[arg(long)]
        package: bool,
    },
    /// Manage installed Fidan versions
    #[command(name = "self")]
    SelfManage {
        #[command(subcommand)]
        command: self_cmd::SelfCommand,
    },
    /// Manage optional heavyweight toolchains like LLVM
    Toolchain {
        #[command(subcommand)]
        command: toolchain::ToolchainCommand,
    },
    /// Dal package registry commands
    Dal {
        #[command(subcommand)]
        command: dal::DalCommand,
    },
    /// Run a toolchain-provided external command namespace
    Exec {
        /// Registered external namespace, for example `ai`
        namespace: Option<String>,
        /// Arguments passed through to the external toolchain command
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(hide = true, name = "__ai-analysis")]
    AiAnalysisInternal,
}

fn parse_emit(raw: &[String]) -> Result<Vec<EmitKind>> {
    raw.iter()
        .map(|s| match s.trim().to_lowercase().as_str() {
            "tokens" => Ok(EmitKind::Tokens),
            "ast" => Ok(EmitKind::Ast),
            "hir" => Ok(EmitKind::Hir),
            "mir" => Ok(EmitKind::Mir),
            "obj" | "object" => Ok(EmitKind::Obj),
            other => bail!(
                "unknown --emit target {:?}  (valid: tokens, ast, hir, mir, obj)",
                other
            ),
        })
        .collect()
}

fn parse_trace(raw: &str) -> Result<TraceMode> {
    match raw.trim().to_lowercase().as_str() {
        "none" | "" => Ok(TraceMode::None),
        "short" => Ok(TraceMode::Short),
        "full" => Ok(TraceMode::Full),
        "compact" => Ok(TraceMode::Compact),
        other => bail!(
            "unknown --trace mode {:?}  (valid: none, short, full, compact)",
            other
        ),
    }
}

fn parse_opt_level(raw: &str) -> Result<fidan_driver::OptLevel> {
    use fidan_driver::OptLevel;
    match raw.trim() {
        "O0" | "o0" | "0" => Ok(OptLevel::O0),
        "O1" | "o1" | "1" => Ok(OptLevel::O1),
        "O2" | "o2" | "2" => Ok(OptLevel::O2),
        "O3" | "o3" | "3" => Ok(OptLevel::O3),
        "Os" | "os" | "s" => Ok(OptLevel::Os),
        "Oz" | "oz" | "z" => Ok(OptLevel::Oz),
        other => bail!(
            "unknown optimisation level {:?}  (valid: O0, O1, O2, O3, Os, Oz)",
            other
        ),
    }
}

fn parse_lto_mode(raw: &str) -> Result<fidan_driver::LtoMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "none" | "false" | "0" => Ok(LtoMode::Off),
        "full" | "true" | "1" => Ok(LtoMode::Full),
        other => bail!("unknown --lto mode {:?}  (valid: off, full)", other),
    }
}

fn parse_strip_mode(raw: &str) -> Result<fidan_driver::StripMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "none" | "false" | "0" => Ok(StripMode::Off),
        "symbols" | "sym" => Ok(StripMode::Symbols),
        "all" | "true" | "1" => Ok(StripMode::All),
        other => bail!(
            "unknown --strip mode {:?}  (valid: off, symbols, all)",
            other
        ),
    }
}

fn main() {
    ensure_utf8_console();

    // Catch all Rust panics and render them as Fidan-style boxed error messages
    // instead of the default Rust backtrace.
    std::panic::set_hook(Box::new(|info| {
        // Capture the backtrace immediately so all call frames are present.
        let bt = std::backtrace::Backtrace::force_capture();
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "unexpected internal error".to_string()
        };
        let loc = info
            .location()
            .map(|l| format!(" [{}:{}]", l.file(), l.line()))
            .unwrap_or_default();
        render_message_to_stderr(
            Severity::Error,
            "internal",
            &format!(
                "compiler crashed: {msg}{loc}\n  This is a bug — please report it at https://github.com/fidan-lang/fidan/issues"
            ),
        );
        // Render filtered Fidan-only stack frames below the crash box.
        render_backtrace_to_stderr(&bt);
    }));

    let exit_code = match run_cli() {
        Ok(()) => 0,
        Err(err) => {
            render_message_to_stderr(Severity::Error, "cli", &format_cli_error(&err));
            1
        }
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn run_cli() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            file,
            emit,
            trace,
            max_errors,
            jit_threshold,
            strict,
            reload,
            replay,
            suppress,
            sandbox,
            allow_read,
            allow_write,
            allow_env,
            time_limit,
            mem_limit,
            args,
        } => {
            let emit_kinds = parse_emit(&emit)?;
            let trace_mode = parse_trace(&trace)?;
            let replay_inputs = match replay {
                Some(ref id_or_path) => replay::load_replay_bundle(id_or_path)?,
                None => vec![],
            };
            let sandbox_policy = if sandbox {
                let mut policy = SandboxPolicy::default();
                let mut read_all = false;
                for p in &allow_read {
                    if p == "*" {
                        policy = policy.with_allow_read_all();
                        read_all = true;
                        break;
                    }
                }
                if !read_all {
                    for p in &allow_read {
                        policy = policy.with_allow_read_prefix(p);
                    }
                }
                let mut write_all = false;
                for p in &allow_write {
                    if p == "*" {
                        policy = policy.with_allow_write_all();
                        write_all = true;
                        break;
                    }
                }
                if !write_all {
                    for p in &allow_write {
                        policy = policy.with_allow_write_prefix(p);
                    }
                }
                if allow_env {
                    policy = policy.with_allow_env();
                }
                if let Some(t) = time_limit {
                    policy = policy.with_time_limit(t);
                }
                if let Some(m) = mem_limit {
                    policy = policy.with_mem_limit(m);
                }
                Some(policy)
            } else {
                None
            };
            let opts = CompileOptions {
                input: file,
                output: None,
                mode: ExecutionMode::Interpret,
                emit: emit_kinds,
                trace: trace_mode,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                jit_threshold,
                strict_mode: strict,
                replay_inputs,
                program_args: args,
                suppress,
                sandbox: sandbox_policy,
                opt_level: Default::default(),
                extra_lib_dirs: vec![],
                link_dynamic: false,
                ..Default::default()
            };
            if reload {
                pipeline::run_with_reload(opts)
            } else {
                pipeline::run_pipeline(opts)
            }
        }
        Command::Build {
            file,
            output,
            opt,
            release,
            lto,
            strip,
            emit,
            lib_dir,
            link_runtime,
            linker,
            strict,
            suppress,
            backend,
            max_errors,
            target_cpu,
        } => {
            // Apply --linker before the pipeline so FIDAN_LINKER is set for
            // the Cranelift codegen backend.  This takes priority over any
            // pre-existing FIDAN_LINKER env var.
            if let Some(ref l) = linker {
                // SAFETY: single-threaded at this point; no other threads
                // read env vars concurrently.
                unsafe { std::env::set_var("FIDAN_LINKER", l) };
            }
            let emit_kinds = parse_emit(&emit)?;
            let opt_is_default = opt.trim().eq_ignore_ascii_case("O2");
            let lto_is_default = lto.trim().eq_ignore_ascii_case("off");
            let strip_is_default = strip.trim().eq_ignore_ascii_case("off");
            let opt_level = if release && opt_is_default {
                OptLevel::O3
            } else {
                parse_opt_level(&opt)?
            };
            let lto = if release && lto_is_default {
                LtoMode::Full
            } else {
                parse_lto_mode(&lto)?
            };
            let strip = if release && strip_is_default {
                StripMode::All
            } else {
                parse_strip_mode(&strip)?
            };
            let link_dynamic = match link_runtime.trim().to_lowercase().as_str() {
                "static" | "s" => false,
                "dynamic" | "dyn" | "d" => true,
                other => bail!(
                    "unknown --link-runtime {:?}  (valid: static, dynamic)",
                    other
                ),
            };
            let backend = match backend.trim().to_lowercase().as_str() {
                "auto" | "" => fidan_driver::Backend::Auto,
                "cranelift" | "cl" => fidan_driver::Backend::Cranelift,
                "llvm" => fidan_driver::Backend::Llvm,
                other => bail!(
                    "unknown --backend {:?}  (valid: auto, cranelift, llvm)",
                    other
                ),
            };
            let effective_target_cpu = if release && target_cpu.is_none() {
                Some("native".to_string())
            } else {
                target_cpu
            };
            let opts = CompileOptions {
                input: file,
                output: Some(output),
                mode: ExecutionMode::Build,
                emit: emit_kinds,
                opt_level,
                lto,
                strip,
                extra_lib_dirs: lib_dir,
                link_dynamic,
                strict_mode: strict,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                suppress,
                backend,
                target_cpu: effective_target_cpu,
                ..Default::default()
            };
            pipeline::run_pipeline(opts)
        }
        Command::Profile {
            file,
            profile_out,
            max_errors,
            suppress,
        } => {
            let opts = CompileOptions {
                input: file,
                output: profile_out,
                mode: ExecutionMode::Profile,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                suppress,
                ..Default::default()
            };
            pipeline::run_pipeline(opts)
        }
        Command::Test {
            file,
            max_errors,
            suppress,
        } => {
            let opts = CompileOptions {
                input: file,
                mode: ExecutionMode::Test,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                suppress,
                ..Default::default()
            };
            pipeline::run_pipeline(opts)
        }
        Command::Check {
            file,
            max_errors,
            strict,
            suppress,
        } => {
            let opts = CompileOptions {
                input: file,
                mode: ExecutionMode::Check,
                max_errors: if max_errors == 0 {
                    None
                } else {
                    Some(max_errors)
                },
                strict_mode: strict,
                suppress,
                ..Default::default()
            };
            pipeline::run_pipeline(opts)
        }
        Command::Fix { file, dry_run } => fix::run_fix(file, dry_run),
        Command::Format {
            file,
            in_place,
            check,
            indent_width,
            max_line_len,
        } => pipeline::run_fmt(file, in_place, check, indent_width, max_line_len),
        Command::Explain {
            target,
            line,
            end_line,
            diagnostic,
            last_error,
            ai,
        } => explain::run_explain_command(explain::ExplainArgs {
            target,
            line,
            end_line,
            diagnostic,
            last_error,
            ai,
        }),
        Command::Repl {
            trace,
            max_errors_per_input,
        } => {
            let trace_mode = parse_trace(&trace)?;
            repl::run_repl(trace_mode, max_errors_per_input)
        }
        Command::Lsp { .. } => {
            fidan_lsp::run();
            Ok(())
        }
        Command::New {
            project_name,
            dir,
            package,
        } => pipeline::run_new(&project_name, dir.as_ref(), package),
        Command::SelfManage { command } => self_cmd::run(command),
        Command::Toolchain { command } => toolchain::run(command),
        Command::Dal { command } => dal::run(command),
        Command::Exec { namespace, args } => exec::run(namespace, args),
        Command::AiAnalysisInternal => ai_analysis::handle_internal_request_from_stdio(),
    }
}

fn format_cli_error(err: &anyhow::Error) -> String {
    let mut rendered = err.to_string();
    let mut chain = err.chain();
    let _ = chain.next();
    for cause in chain {
        let cause_text = cause.to_string();
        if cause_text != rendered {
            rendered.push_str(&format!("\n  cause: {cause_text}"));
        }
    }
    rendered
}
