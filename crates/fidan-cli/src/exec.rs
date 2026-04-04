use anyhow::{Context, Result, bail};
use fidan_driver::{ResolvedToolchain, is_valid_exec_namespace, resolve_fidan_home};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

#[derive(Clone)]
struct RegisteredExecCommand {
    namespace: String,
    description: Option<String>,
    toolchain: ResolvedToolchain,
}

pub(crate) fn run(namespace: Option<String>, args: Vec<String>) -> Result<()> {
    let home = resolve_fidan_home()?;
    let toolchains = fidan_driver::install::installed_toolchains(&home, None)?;
    let commands = collect_registered_exec_commands(toolchains)?;

    let Some(namespace) = namespace else {
        return list_commands(&commands);
    };

    let namespace = namespace.trim().to_string();
    if !is_valid_exec_namespace(&namespace) {
        bail!(
            "invalid exec namespace `{namespace}` — expected lowercase ASCII letters, digits, or `-`, starting with a letter"
        );
    }

    let registered = commands.get(&namespace).with_context(|| {
        if commands.is_empty() {
            "no external exec namespaces are registered — install a toolchain that exports one first".to_string()
        } else {
            let available = available_namespace_list(&commands);
            format!(
                "external exec namespace `{namespace}` is not registered — available namespaces: {available}"
            )
        }
    })?;

    if !registered.toolchain.helper_path.is_file() {
        bail!(
            "registered helper for `fidan exec {}` is missing at `{}` — reinstall toolchain `{}` version `{}`",
            namespace,
            registered.toolchain.helper_path.display(),
            registered.toolchain.metadata.kind,
            registered.toolchain.metadata.toolchain_version
        );
    }

    let status = Command::new(&registered.toolchain.helper_path)
        .arg("exec")
        .arg(&registered.namespace)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| {
            format!(
                "failed to launch exec helper `{}`",
                registered.toolchain.helper_path.display()
            )
        })?;

    if status.success() {
        return Ok(());
    }

    let code = status.code().unwrap_or(1);
    bail!("external exec command `{namespace}` exited with status {code}")
}

fn available_namespace_list(commands: &BTreeMap<String, RegisteredExecCommand>) -> String {
    commands
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn list_commands(commands: &BTreeMap<String, RegisteredExecCommand>) -> Result<()> {
    if commands.is_empty() {
        println!("No external exec namespaces are installed.");
        return Ok(());
    }

    println!("Available external exec namespaces:");
    for command in commands.values() {
        match command.description.as_deref() {
            Some(description) => println!("- {}: {}", command.namespace, description),
            None => println!("- {}", command.namespace),
        }
    }
    Ok(())
}

fn collect_registered_exec_commands(
    toolchains: Vec<ResolvedToolchain>,
) -> Result<BTreeMap<String, RegisteredExecCommand>> {
    let mut commands: BTreeMap<String, RegisteredExecCommand> = BTreeMap::new();

    for toolchain in toolchains {
        for registration in &toolchain.metadata.exec_commands {
            let namespace = registration.namespace.trim();
            if !is_valid_exec_namespace(namespace) {
                continue;
            }

            match commands.get(namespace) {
                Some(existing) if existing.toolchain.metadata.kind == toolchain.metadata.kind => {
                    continue;
                }
                Some(existing) => {
                    bail!(
                        "exec namespace `{}` is exported by both toolchains `{}` and `{}` — remove one of them",
                        namespace,
                        existing.toolchain.metadata.kind,
                        toolchain.metadata.kind
                    );
                }
                None => {
                    commands.insert(
                        namespace.to_string(),
                        RegisteredExecCommand {
                            namespace: namespace.to_string(),
                            description: registration.description.clone(),
                            toolchain: toolchain.clone(),
                        },
                    );
                }
            }
        }
    }

    Ok(commands)
}
