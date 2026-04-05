use fidan_driver::{
    AI_ANALYSIS_HELPER_PROTOCOL_VERSION, AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisHelperCommand,
    AiAnalysisHelperRequest, AiAnalysisHelperResponse, AiAnalysisHelperResult, AiAnalysisResponse,
    AiAnalysisResult, AiExplainContext,
};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "fidan_ai_helper_analyze_{name}_{}_{}",
        std::process::id(),
        nonce
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn make_temp_program(name: &str, source: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "fidan_ai_helper_analyze_{name}_{}_{}.fdn",
        std::process::id(),
        nonce
    ));
    std::fs::write(&path, source).expect("write temp program");
    path
}

fn make_fake_fidan(root: &Path, response_json: &str, capture_path: &Path) -> PathBuf {
    if cfg!(windows) {
        let fake = root.join("fidan.cmd");
        let response_path = root.join("response.json");
        std::fs::write(&response_path, response_json).expect("write fake fidan response");
        let escaped_capture = capture_path.display().to_string().replace('\'', "''");
        let escaped_response = response_path.display().to_string().replace('\'', "''");
        let script = format!(
            "@echo off\r\npowershell -NoProfile -Command \"$inputJson = [Console]::In.ReadToEnd(); Set-Content -LiteralPath '{escaped_capture}' -Value $inputJson -NoNewline; [Console]::Out.Write((Get-Content -LiteralPath '{escaped_response}' -Raw))\"\r\n"
        );
        std::fs::write(&fake, script).expect("write fake fidan cmd");
        fake
    } else {
        let fake = root.join("fidan.sh");
        let response_path = root.join("response.json");
        std::fs::write(&response_path, response_json).expect("write fake fidan response");
        let script = format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s' '{}'\n",
            capture_path.display(),
            response_json.replace('\'', "'\"'\"'")
        );
        std::fs::write(&fake, script).expect("write fake fidan sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake)
                .expect("fake metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake, perms).expect("set fake fidan perms");
        }
        fake
    }
}

fn explain_context_response(file: &Path) -> String {
    serde_json::to_string(&AiAnalysisResponse {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        success: true,
        result: Some(AiAnalysisResult::ExplainContext(AiExplainContext {
            file: file.to_path_buf(),
            line_start: 1,
            line_end: 4,
            total_lines: 4,
            selected_source: std::fs::read_to_string(file).unwrap_or_default(),
            deterministic_lines: vec![],
            module_outline: vec![],
            dependencies: vec![],
            related_symbols: vec![],
            diagnostics: vec![],
            call_graph: vec![],
            type_map: vec![],
            runtime_trace: None,
        })),
        error: None,
    })
    .expect("serialize explain context response")
}

fn write_config(path: &Path, base_url: &str) {
    let config = format!(
        "schema_version = 1\nprovider = \"openai-compatible\"\nmodel = \"test-model\"\nbase_url = \"{base_url}\"\napi_key_env = \"FIDAN_TEST_FAKE_API_KEY_SHOULD_NOT_EXIST\"\ntimeout_secs = 10\n"
    );
    std::fs::write(path, config).expect("write helper config");
}

fn spawn_openai_sequence_stub(contents: Vec<String>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP stub");
    let addr = listener.local_addr().expect("read local HTTP stub address");
    let handle = thread::spawn(move || {
        for content in contents {
            let (mut stream, _) = listener.accept().expect("accept HTTP connection");
            let mut buffer = vec![0u8; 16 * 1024];
            let _ = stream.read(&mut buffer).expect("read HTTP request");
            let body = json!({
                "choices": [{
                    "message": {
                        "content": content
                    }
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write HTTP response");
        }
    });
    (format!("http://{addr}/v1/chat/completions"), handle)
}

fn run_analyze_request(
    envs: &[(&str, String)],
    request: AiAnalysisHelperRequest,
) -> AiAnalysisHelperResponse {
    let request_bytes = serde_json::to_vec(&request).expect("serialize analyze request");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fidan-ai-analysis-helper"));
    cmd.arg("analyze")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn helper analyze");
    child
        .stdin
        .as_mut()
        .expect("helper stdin")
        .write_all(&request_bytes)
        .expect("write analyze request");
    let output = child
        .wait_with_output()
        .expect("wait for helper analyze output");
    assert!(
        output.status.success(),
        "expected helper analyze request to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse helper analyze response")
}

#[test]
fn analyze_explain_normalizes_array_fields() {
    let root = make_temp_dir("normalize_arrays");
    let capture = root.join("capture.json");
    let file = make_temp_program(
        "normalize_arrays",
        "action main {\n    print(1)\n}\nmain()\n",
    );
    let fake_fidan = make_fake_fidan(&root, &explain_context_response(&file), &capture);
    let (base_url, server) = spawn_openai_sequence_stub(vec![
        json!({
            "summary": ["Summary line 1", "Summary line 2"],
            "input_output_behavior": "Prints a value.",
            "dependencies": ["std.io.print"],
            "possible_edge_cases": ["None"],
            "why_pattern_is_used": "Entry point is explicit.",
            "related_symbols": ["main"],
            "underlying_behaviour": ["Calls print"]
        })
        .to_string(),
    ]);
    let config_path = root.join("ai-analysis.toml");
    write_config(&config_path, &base_url);

    let response = run_analyze_request(
        &[
            ("FIDAN_EXE", fake_fidan.display().to_string()),
            (
                "FIDAN_AI_ANALYSIS_CONFIG",
                config_path.display().to_string(),
            ),
        ],
        AiAnalysisHelperRequest {
            protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
            command: AiAnalysisHelperCommand::Explain {
                file: file.clone(),
                line_start: None,
                line_end: None,
                prompt: None,
                fidan_path: None,
            },
        },
    );

    server.join().expect("join OpenAI stub server");
    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    assert!(
        response.success,
        "expected normalized explain response to succeed"
    );
    match response.result.expect("explain result") {
        AiAnalysisHelperResult::Explain(explanation) => {
            assert_eq!(explanation.summary, "Summary line 1\nSummary line 2");
            assert_eq!(explanation.dependencies, "std.io.print");
            assert_eq!(explanation.related_symbols, "main");
        }
        AiAnalysisHelperResult::Fix(_) => panic!("unexpected fix result"),
    }
}

#[test]
fn analyze_explain_retries_after_invalid_payload() {
    let root = make_temp_dir("retry_invalid");
    let capture = root.join("capture.json");
    let file = make_temp_program("retry_invalid", "action main {\n    print(1)\n}\nmain()\n");
    let fake_fidan = make_fake_fidan(&root, &explain_context_response(&file), &capture);
    let (base_url, server) = spawn_openai_sequence_stub(vec![
        json!({
            "summary": "Summary",
            "input_output_behavior": "Prints a value.",
            "dependencies": "std.io.print",
            "possible_edge_cases": "None",
            "why_pattern_is_used": "Entry point is explicit.",
            "related_symbols": "main"
        })
        .to_string(),
        json!({
            "summary": "Recovered summary",
            "input_output_behavior": "Prints a value.",
            "dependencies": "std.io.print",
            "possible_edge_cases": "None",
            "why_pattern_is_used": "Entry point is explicit.",
            "related_symbols": "main",
            "underlying_behaviour": "Calls print"
        })
        .to_string(),
    ]);
    let config_path = root.join("ai-analysis.toml");
    write_config(&config_path, &base_url);

    let response = run_analyze_request(
        &[
            ("FIDAN_EXE", fake_fidan.display().to_string()),
            (
                "FIDAN_AI_ANALYSIS_CONFIG",
                config_path.display().to_string(),
            ),
        ],
        AiAnalysisHelperRequest {
            protocol_version: AI_ANALYSIS_HELPER_PROTOCOL_VERSION,
            command: AiAnalysisHelperCommand::Explain {
                file: file.clone(),
                line_start: None,
                line_end: None,
                prompt: None,
                fidan_path: None,
            },
        },
    );

    server.join().expect("join OpenAI stub server");
    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    assert!(
        response.success,
        "expected retrying explain response to succeed"
    );
    match response.result.expect("explain result") {
        AiAnalysisHelperResult::Explain(explanation) => {
            assert_eq!(explanation.summary, "Recovered summary");
            assert_eq!(explanation.underlying_behaviour, "Calls print");
        }
        AiAnalysisHelperResult::Fix(_) => panic!("unexpected fix result"),
    }
}
