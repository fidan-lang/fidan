#![allow(unused_crate_dependencies)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixture() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/verbosity-contract")
        .join(format!(
            "{} {}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
    fs::create_dir_all(&root).unwrap();
    let file = root.join("output.fdn");
    fs::write(&file, "print(\"program-output\")\n").unwrap();
    file
}
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fidan"));
    let home = std::env::var_os("FIDAN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/verbosity-contract/isolated-home")
        });
    command.env("FIDAN_HOME", home).env_remove("RUST_LOG");
    command
}

fn cli(args: &[&str]) -> Output {
    command()
        .env_remove("RUST_LOG")
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn verbosity_is_progressive_and_preserves_stdout() {
    let file = fixture();
    let file = file.to_str().unwrap();
    for (flags, expected, excluded) in [
        (vec![], "", "INFO"),
        (vec!["-v"], "INFO", "DEBUG"),
        (vec!["--verbose"], "INFO", "DEBUG"),
        (vec!["-vv"], "DEBUG", "TRACE"),
        (vec!["-v", "-v"], "DEBUG", "TRACE"),
        (vec!["-vvv"], "TRACE", ""),
        (vec!["-v", "-v", "-v"], "TRACE", ""),
        (vec!["--verbose", "--verbose", "--verbose"], "TRACE", ""),
    ] {
        let mut args = flags;
        args.extend(["run", file, "--jit-threshold", "0"]);
        let out = cli(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"program-output\n");
        let logs = String::from_utf8_lossy(&out.stderr);
        if !expected.is_empty() {
            assert!(logs.contains(expected), "{logs}");
            assert!(logs.contains("executing program"), "{logs}");
            if matches!(expected, "DEBUG" | "TRACE") {
                assert!(logs.contains("lexed_items="), "{logs}");
            }
            if expected == "TRACE" {
                assert!(logs.contains("elapsed_us="), "{logs}");
            }
        }
        if !excluded.is_empty() {
            assert!(!logs.contains(excluded), "{logs}");
        }
        assert!(!logs.contains('\u{1b}'));
    }
}
#[test]
fn excessive_verbosity_is_an_actionable_usage_error() {
    let out = cli(&["-vvvv", "toolchain", "list"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("three"));
    let out = cli(&["-vv", "toolchain", "list", "-vv"]);
    assert_eq!(out.status.code(), Some(2));
}
#[test]
fn help_and_version_remain_independent() {
    for flag in ["-V", "--version"] {
        let out = cli(&[flag]);
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("fidan "));
        assert!(out.stderr.is_empty());
        let verbose = cli(&["-vvv", flag]);
        assert!(verbose.status.success());
        assert_eq!(verbose.stdout, out.stdout);
        assert!(verbose.stderr.is_empty());
    }
    let out = cli(&["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--verbose"));
    assert!(help.contains("-V"));
}
#[test]
fn nested_commands_accept_global_flags() {
    let file = fixture();
    let out = cli(&["run", "-vv", file.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("lexer completed"));
    for args in [
        vec!["-vvv", "toolchain", "list"],
        vec!["toolchain", "-vvv", "list"],
        vec!["toolchain", "list", "-vvv"],
    ] {
        let out = cli(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("toolchain list"));
    }
}

#[test]
fn program_arguments_are_not_cli_verbosity() {
    let file = fixture().with_file_name("arguments.fdn");
    fs::write(
        &file,
        "use std.env\nvar values = env.args()\nfor i in 0..len(values) { print(values[i]) }\n",
    )
    .unwrap();
    for tail in [
        vec!["--", "--verbose", "-v", "-V"],
        vec!["positional", "-v", "--verbose", "-V"],
        vec!["-v", "--verbose", "-V"],
        vec!["--verbose=true", "-vvvv", "-V"],
    ] {
        let mut args = vec!["-vv", "run", file.to_str().unwrap()];
        args.extend(tail.clone());
        let out = cli(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let values = tail
            .into_iter()
            .filter(|a| *a != "--")
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert_eq!(String::from_utf8_lossy(&out.stdout), values);
    }
}
#[test]
fn environment_filter_has_documented_precedence() {
    for (flag, environment, want) in [
        ("", "debug", "DEBUG"),
        ("", "fidan_diagnostics::logging=trace", "TRACE"),
        ("-v", "trace", "INFO"),
    ] {
        let file = fixture();
        let mut command = command();
        command.env("RUST_LOG", environment);
        if !flag.is_empty() {
            command.arg(flag);
        }
        let out = command
            .args(["check", file.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let logs = String::from_utf8_lossy(&out.stderr);
        assert!(logs.contains(want), "{logs}");
        if flag == "-v" {
            assert!(!logs.contains("TRACE"));
            assert!(!logs.contains("DEBUG"));
        }
    }
    let out = command()
        .env("RUST_LOG", "hyper=trace")
        .args(["toolchain", "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only Fidan targets"));
}
#[test]
fn errors_and_warnings_are_independent_of_verbosity() {
    let file = fixture().with_file_name("broken.fdn");
    fs::write(&file, "var value = absent_identifier\n").unwrap();
    for flag in ["", "-v", "-vv", "-vvv"] {
        let mut args = Vec::new();
        if !flag.is_empty() {
            args.push(flag)
        }
        args.extend(["check", file.to_str().unwrap()]);
        let out = cli(&args);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("E0101"));
    }
}

#[test]
fn all_command_help_inherits_verbosity_without_execution() {
    for command in [
        "run",
        "build",
        "profile",
        "test",
        "repl",
        "lsp",
        "format",
        "check",
        "fix",
        "explain",
        "new",
        "self",
        "toolchain",
        "dal",
        "exec",
    ] {
        let out = cli(&[command, "--help"]);
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("--verbose"),
            "{command}"
        );
    }
    for (command, leaves) in [
        (
            "dal",
            vec![
                "login", "logout", "whoami", "search", "info", "add", "remove", "package",
                "publish", "yank", "unyank",
            ],
        ),
        ("self", vec!["list", "current", "install", "use", "remove"]),
        ("toolchain", vec!["available", "list", "add", "remove"]),
    ] {
        for leaf in leaves {
            let out = cli(&[command, leaf, "--help"]);
            assert!(out.status.success(), "{command} {leaf}");
            assert!(String::from_utf8_lossy(&out.stdout).contains("--verbose"));
        }
    }
    let out = cli(&["--unknown-option"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
#[test]
fn source_tools_keep_pipe_outputs_and_repl_semantics() {
    let file = fixture();
    let file = file.to_str().unwrap();
    for args in [
        vec!["format", file],
        vec!["check", file],
        vec!["fix", file],
        vec!["explain", "--diagnostic", "R2003"],
        vec!["explain", file],
    ] {
        let normal = cli(&args);
        let mut verbose = vec!["-vvv"];
        verbose.extend(args);
        let traced = cli(&verbose);
        assert_eq!(normal.status.code(), traced.status.code());
        assert_eq!(normal.stdout, traced.stdout);
        assert!(String::from_utf8_lossy(&traced.stderr).contains("operation completed"));
    }
    use std::io::Write;
    let run = |flags: &[&str]| {
        let mut child = command()
            .env_remove("RUST_LOG")
            .args(flags)
            .arg("repl")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"print(42)\n:quit\n")
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let normal = run(&[]);
    let traced = run(&["-vvv"]);
    assert_eq!(normal.status.code(), traced.status.code());
    assert_eq!(normal.stdout, traced.stdout);
    assert!(String::from_utf8_lossy(&traced.stderr).contains("repl_typecheck"));
}
#[test]
fn untrusted_registry_errors_do_not_leak_credentials_at_any_level() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    for flag in ["", "-v", "-vv", "-vvv"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut request = [0; 8192];
            let received = socket.read(&mut request).unwrap();
            assert!(received > 0);
            let body = "Authorization: Bearer synthetic-bearer\nCookie: session=synthetic-cookie\npassword=synthetic-password\nhttps://u:p@host/private?X-Amz-Signature=synthetic-signature\nenv-credential-verbosity-test";
            write!(
                socket,
                "HTTP/1.1 500 Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut command = command();
        command
            .env_remove("RUST_LOG")
            .env("FIDAN_TEST_SECRET", "env-credential-verbosity-test");
        if !flag.is_empty() {
            command.arg(flag);
        }
        let out = command
            .args(["dal", "search", "fixture", "--registry", &origin])
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        for secret in [
            "synthetic-bearer",
            "synthetic-cookie",
            "synthetic-password",
            "synthetic-signature",
            "env-credential-verbosity-test",
            "u:p@",
        ] {
            assert!(!stderr.contains(secret), "{stderr}");
        }
        assert!(stderr.contains("HTTP 500"));
    }
}

#[test]
fn interpreter_and_jit_keep_results_and_report_actual_compilation() {
    let file = fixture().with_file_name("jit.fdn");
    fs::write(&file,"action add with (certain x oftype integer) returns integer { if x == 0 { return 0 }; return x + 1 }\nprint(add(41))\n").unwrap();
    for threshold in ["0", "1"] {
        for flag in ["", "-v", "-vv", "-vvv"] {
            let mut args = Vec::new();
            if !flag.is_empty() {
                args.push(flag)
            }
            args.extend(["run", file.to_str().unwrap(), "--jit-threshold", threshold]);
            let out = cli(&args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(out.stdout, b"42\n");
            if threshold == "1" && matches!(flag, "-vv" | "-vvv") {
                assert!(
                    String::from_utf8_lossy(&out.stderr)
                        .contains("selective JIT compilation completed")
                );
                assert!(String::from_utf8_lossy(&out.stderr).contains("native=true"));
            }
        }
    }
}

#[test]
fn native_verbosity_preserves_object_bytes_and_runtime_output() {
    let file = fixture();
    let directory = file.parent().unwrap();
    let llvm = fidan_driver::resolve_fidan_home()
        .ok()
        .and_then(|home| fidan_driver::install::installed_llvm_toolchains(&home).ok())
        .is_some_and(|found| !found.is_empty());
    assert!(
        std::env::var_os("FIDAN_REQUIRE_LLVM").is_none() || llvm,
        "LLVM required for this validation but no compatible helper is installed"
    );
    for backend in ["cranelift", "llvm"] {
        if backend == "llvm" && !llvm {
            eprintln!("LLVM verbosity matrix not executed: no compatible local helper");
            continue;
        }
        let output = directory.join(if cfg!(windows) {
            "native.exe"
        } else {
            "native"
        });
        let mut reference = None;
        for flag in ["", "-v", "-vv", "-vvv"] {
            let mut args = Vec::new();
            if !flag.is_empty() {
                args.push(flag)
            }
            args.extend([
                "build",
                file.to_str().unwrap(),
                "--backend",
                backend,
                "--output",
                output.to_str().unwrap(),
                "--emit",
                "obj",
            ]);
            if backend == "llvm" {
                args.extend(["--lto", "full"]);
            }
            let built = cli(&args);
            assert!(
                built.status.success(),
                "{}",
                String::from_utf8_lossy(&built.stderr)
            );
            assert!(built.stdout.is_empty());
            let logs = String::from_utf8_lossy(&built.stderr);
            if !flag.is_empty() {
                assert!(logs.contains("AOT backend selected"));
            }
            if backend == "llvm" && flag == "-vvv" {
                assert!(logs.contains("LLVM backend transition"));
                assert!(logs.contains("LLVM optimization policy selected"));
            }
            let object = output.with_extension(if cfg!(windows) { "obj" } else { "o" });
            let bytes = fs::read(object).unwrap();
            if let Some(reference) = &reference {
                assert_eq!(
                    &bytes, reference,
                    "verbosity changed object bytes: {backend}"
                );
            } else {
                reference = Some(bytes);
            }
            let run = Command::new(&output).output().unwrap();
            assert!(run.status.success());
            assert_eq!(run.stdout, b"program-output\n");
        }
    }
}

#[test]
fn native_subprocess_errors_are_redacted_without_logging_arguments() {
    let file = fixture();
    for flag in ["", "-v", "-vv", "-vvv"] {
        let secret = "subprocess-credential-verbosity-test";
        let linker = file.parent().unwrap().join(secret);
        let output = file.parent().unwrap().join("failed-native");
        let mut command = command();
        command
            .env_remove("RUST_LOG")
            .env("FIDAN_TEST_SECRET", secret);
        if !flag.is_empty() {
            command.arg(flag);
        }
        let out = command
            .args([
                "build",
                file.to_str().unwrap(),
                "--backend",
                "cranelift",
                "--linker",
                linker.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains(secret), "{stderr}");
        assert!(stderr.contains("linker"));
    }
}

#[test]
fn internal_machine_json_and_profile_files_remain_parseable() {
    use std::io::Write;
    let file = fixture();
    let request = fidan_driver::AiAnalysisRequest {
        protocol_version: fidan_driver::AI_ANALYSIS_PROTOCOL_VERSION,
        command: fidan_driver::AiAnalysisCommand::ExplainContext {
            file: file.clone(),
            line_start: Some(1),
            line_end: Some(1),
        },
    };
    let mut child = command()
        .env("RUST_LOG", "trace")
        .arg("__ai-analysis")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let response: fidan_driver::AiAnalysisResponse = serde_json::from_slice(&out.stdout).unwrap();
    assert!(response.success);
    let profile = file.parent().unwrap().join("profile.json");
    let out = cli(&[
        "-vvv",
        "profile",
        file.to_str().unwrap(),
        "--profile-out",
        profile.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let _: serde_json::Value = serde_json::from_slice(&fs::read(profile).unwrap()).unwrap();
}

#[test]
fn authentication_echoes_and_existing_warnings_are_safe_at_every_level() {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };
    let warning = fixture().with_file_name("warning.fdn");
    fs::write(&warning, "use std.io\nprint(42)\n").unwrap();
    for flag in ["", "-v", "-vv", "-vvv"] {
        let mut args = Vec::new();
        if !flag.is_empty() {
            args.push(flag)
        }
        args.extend(["check", warning.to_str().unwrap()]);
        let checked = cli(&args);
        assert!(checked.status.success());
        assert!(String::from_utf8_lossy(&checked.stderr).contains("unused import"));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut headers = String::new();
            let mut reader = BufReader::new(&socket);
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(
                headers
                    .to_lowercase()
                    .contains("authorization: bearer explicit-auth-verbosity-secret")
            );
            let body = r#"{"message":"explicit-auth-verbosity-secret","access_token":"json-auth-verbosity-secret"}"#;
            write!(socket,"HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let mut args = Vec::new();
        if !flag.is_empty() {
            args.push(flag)
        }
        args.extend([
            "dal",
            "login",
            "--registry",
            &origin,
            "--token",
            "explicit-auth-verbosity-secret",
        ]);
        let out = cli(&args);
        server.join().unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("explicit-auth-verbosity-secret"),
            "{stderr}"
        );
        assert!(!stderr.contains("json-auth-verbosity-secret"), "{stderr}");
        assert!(stderr.contains("HTTP 401"));
    }
}

#[cfg(any(unix, windows))]
#[test]
fn invalid_unicode_filter_fails_safely_and_explicit_flags_override_it() {
    #[cfg(unix)]
    let value = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xff])
    };
    #[cfg(windows)]
    let value = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xd800])
    };
    let invalid = command()
        .env("RUST_LOG", &value)
        .args(["toolchain", "list"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("RUST_LOG must be valid Unicode"));
    let explicit = command()
        .env("RUST_LOG", &value)
        .args(["-v", "toolchain", "list"])
        .output()
        .unwrap();
    assert!(explicit.status.success());
    assert!(String::from_utf8_lossy(&explicit.stderr).contains("operation completed"));
}

#[test]
fn application_runtime_messages_keep_their_existing_contract() {
    let file = fixture().with_file_name("application-message.fdn");
    fs::write(&file, "panic(\"token=application-defined-message\")\n").unwrap();
    for flag in ["", "-v", "-vv", "-vvv"] {
        let mut args = Vec::new();
        if !flag.is_empty() {
            args.push(flag)
        }
        args.extend(["run", file.to_str().unwrap()]);
        let out = cli(&args);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("token=application-defined-message"));
    }
}
