use fidan_driver::{
    AI_ANALYSIS_PROTOCOL_VERSION, AiAnalysisResponse, AiAnalysisResult, AiCallGraph, AiCallNode,
    AiDiagnosticSummary, AiExplainContext, AiRuntimeTrace, AiTraceStep, AiTypeMap, AiTypedBinding,
};
use serde_json::{Value, json};
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
        "fidan_ai_helper_{name}_{}_{}",
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
        "fidan_ai_helper_{name}_{}_{}.fdn",
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

fn run_mcp_request(envs: &[(&str, String)], request: Value) -> Value {
    let request_bytes = serde_json::to_vec(&request).expect("serialize MCP request");
    let mut packet = format!("Content-Length: {}\r\n\r\n", request_bytes.len()).into_bytes();
    packet.extend_from_slice(&request_bytes);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fidan-ai-analysis-helper"));
    cmd.arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().expect("spawn helper MCP server");
    {
        let stdin = child.stdin.as_mut().expect("helper stdin");
        stdin.write_all(&packet).expect("write MCP packet");
    }

    let output = child
        .wait_with_output()
        .expect("wait for helper MCP output");
    assert!(
        output.status.success(),
        "expected MCP request to succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    parse_single_mcp_response(&output.stdout)
}

fn parse_single_mcp_response(stdout: &[u8]) -> Value {
    let output = String::from_utf8(stdout.to_vec()).expect("stdout should be utf-8");
    let Some((header, body)) = output.split_once("\r\n\r\n") else {
        panic!("missing MCP header separator in output: {output}");
    };
    let content_length = header
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length:"))
        .map(str::trim)
        .and_then(|value| value.parse::<usize>().ok())
        .expect("parse Content-Length header");
    let body_bytes = body.as_bytes();
    let payload = &body_bytes[..content_length.min(body_bytes.len())];
    serde_json::from_slice(payload).expect("parse MCP response payload")
}

fn explain_context_response(file: &Path, diagnostics: Vec<AiDiagnosticSummary>) -> String {
    let response = AiAnalysisResponse {
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
            diagnostics,
            call_graph: vec![],
            type_map: vec![],
            runtime_trace: None,
        })),
        error: None,
    };
    serde_json::to_string(&response).expect("serialize explain context response")
}

fn analysis_response(result: AiAnalysisResult) -> String {
    serde_json::to_string(&AiAnalysisResponse {
        protocol_version: AI_ANALYSIS_PROTOCOL_VERSION,
        success: true,
        result: Some(result),
        error: None,
    })
    .expect("serialize ai-analysis response")
}

fn spawn_openai_stub(content: String) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP stub");
    let addr = listener.local_addr().expect("read local HTTP stub address");
    let handle = thread::spawn(move || {
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
    });
    (format!("http://{addr}/v1/chat/completions"), handle)
}

fn write_config(path: &Path, base_url: &str) {
    let config = format!(
        "schema_version = 1\nprovider = \"openai-compatible\"\nmodel = \"test-model\"\nbase_url = \"{base_url}\"\ntimeout_secs = 10\n"
    );
    std::fs::write(path, config).expect("write helper config");
}

#[test]
fn mcp_tools_list_includes_phase_d_tools() {
    let response = run_mcp_request(
        &[],
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools/list result array");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();

    for expected in [
        "get_diagnostics",
        "get_call_chain",
        "get_type_info",
        "get_runtime_trace",
        "suggest_fix",
        "apply_fix_preview",
    ] {
        assert!(
            names.contains(&expected),
            "expected tools/list to include `{expected}`, got {names:?}"
        );
    }
}

#[test]
fn mcp_get_diagnostics_returns_structured_diagnostics() {
    let root = make_temp_dir("mcp_diag_root");
    let capture = root.join("capture.json");
    let file = make_temp_program(
        "mcp_diag",
        "action main {\n    print(completely_unknown_xyz)\n}\nmain()\n",
    );
    let fake_fidan = make_fake_fidan(
        &root,
        &explain_context_response(
            &file,
            vec![AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `completely_unknown_xyz`".to_string(),
                line: 2,
            }],
        ),
        &capture,
    );

    let response = run_mcp_request(
        &[("FIDAN_EXE", fake_fidan.display().to_string())],
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "get_diagnostics",
                "arguments": { "file": file }
            }
        }),
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    let diagnostics = response["result"]["structuredContent"]
        .as_array()
        .expect("structured diagnostics array");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "E0101");
}

#[test]
fn mcp_get_call_chain_returns_graph() {
    let root = make_temp_dir("mcp_call_graph_root");
    let capture = root.join("capture.json");
    let file = make_temp_program("mcp_call_graph", "action main {\n    greet()\n}\nmain()\n");
    let fake_fidan = make_fake_fidan(
        &root,
        &analysis_response(AiAnalysisResult::CallGraph(AiCallGraph {
            nodes: vec![AiCallNode {
                caller: "main".to_string(),
                callees: vec!["greet".to_string()],
                line: 1,
                is_recursive: false,
            }],
        })),
        &capture,
    );

    let response = run_mcp_request(
        &[("FIDAN_EXE", fake_fidan.display().to_string())],
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_call_chain",
                "arguments": { "file": file }
            }
        }),
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    let nodes = response["result"]["structuredContent"]["nodes"]
        .as_array()
        .expect("call graph nodes");
    assert_eq!(nodes[0]["caller"], "main");
    assert_eq!(nodes[0]["callees"][0], "greet");
}

#[test]
fn mcp_get_type_info_returns_bindings() {
    let root = make_temp_dir("mcp_type_map_root");
    let capture = root.join("capture.json");
    let file = make_temp_program("mcp_type_map", "action main {\n    var x = 1\n}\nmain()\n");
    let fake_fidan = make_fake_fidan(
        &root,
        &analysis_response(AiAnalysisResult::TypeMap(AiTypeMap {
            bindings: vec![AiTypedBinding {
                name: "x".to_string(),
                inferred_type: "integer".to_string(),
                line: 2,
                kind: "var".to_string(),
            }],
        })),
        &capture,
    );

    let response = run_mcp_request(
        &[("FIDAN_EXE", fake_fidan.display().to_string())],
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "get_type_info",
                "arguments": { "file": file }
            }
        }),
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    let bindings = response["result"]["structuredContent"]["bindings"]
        .as_array()
        .expect("type map bindings");
    assert_eq!(bindings[0]["name"], "x");
    assert_eq!(bindings[0]["inferred_type"], "integer");
}

#[test]
fn mcp_get_runtime_trace_returns_steps() {
    let root = make_temp_dir("mcp_runtime_trace_root");
    let capture = root.join("capture.json");
    let file = make_temp_program(
        "mcp_runtime_trace",
        "action main {\n    print(1)\n}\nmain()\n",
    );
    let fake_fidan = make_fake_fidan(
        &root,
        &analysis_response(AiAnalysisResult::RuntimeTrace(AiRuntimeTrace {
            steps: vec![AiTraceStep {
                kind: "call".to_string(),
                description: "enter action `main`".to_string(),
                line: Some(1),
                value: None,
            }],
            truncated: false,
        })),
        &capture,
    );

    let response = run_mcp_request(
        &[("FIDAN_EXE", fake_fidan.display().to_string())],
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "get_runtime_trace",
                "arguments": { "file": file }
            }
        }),
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    let steps = response["result"]["structuredContent"]["steps"]
        .as_array()
        .expect("runtime trace steps");
    assert_eq!(steps[0]["kind"], "call");
    assert_eq!(steps[0]["description"], "enter action `main`");
}

#[test]
fn mcp_suggest_fix_returns_validated_hunks() {
    let root = make_temp_dir("mcp_suggest_fix_root");
    let capture = root.join("capture.json");
    let file = make_temp_program(
        "mcp_suggest_fix",
        "action main {\n    print(completely_unknown_xyz)\n}\nmain()\n",
    );
    let fake_fidan = make_fake_fidan(
        &root,
        &explain_context_response(
            &file,
            vec![AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `completely_unknown_xyz`".to_string(),
                line: 2,
            }],
        ),
        &capture,
    );
    let (base_url, server) = spawn_openai_stub(
        json!({
            "summary": "Replace the undefined symbol with a string literal.",
            "hunks": [{
                "line_start": 2,
                "line_end": 2,
                "old_text": "    print(completely_unknown_xyz)",
                "new_text": "    print(\"hello\")",
                "reason": "E0101 undefined name"
            }]
        })
        .to_string(),
    );
    let config_path = root.join("ai-analysis.toml");
    write_config(&config_path, &base_url);

    let response = run_mcp_request(
        &[
            ("FIDAN_EXE", fake_fidan.display().to_string()),
            (
                "FIDAN_AI_ANALYSIS_CONFIG",
                config_path.display().to_string(),
            ),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "suggest_fix",
                "arguments": { "file": file }
            }
        }),
    );

    server.join().expect("join OpenAI stub server");
    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    let hunks = response["result"]["structuredContent"]["hunks"]
        .as_array()
        .expect("suggest_fix hunks");
    assert_eq!(
        response["result"]["structuredContent"]["summary"],
        "Replace the undefined symbol with a string literal."
    );
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0]["new_text"], "    print(\"hello\")");
}

#[test]
fn mcp_apply_fix_preview_returns_unified_diff() {
    let root = make_temp_dir("mcp_fix_preview_root");
    let capture = root.join("capture.json");
    let file = make_temp_program(
        "mcp_fix_preview",
        "action main {\n    print(completely_unknown_xyz)\n}\nmain()\n",
    );
    let fake_fidan = make_fake_fidan(
        &root,
        &explain_context_response(
            &file,
            vec![AiDiagnosticSummary {
                severity: "error".to_string(),
                code: "E0101".to_string(),
                message: "undefined name `completely_unknown_xyz`".to_string(),
                line: 2,
            }],
        ),
        &capture,
    );
    let (base_url, server) = spawn_openai_stub(
        json!({
            "summary": "Replace the undefined symbol with a string literal.",
            "hunks": [{
                "line_start": 2,
                "line_end": 2,
                "old_text": "    print(completely_unknown_xyz)",
                "new_text": "    print(\"hello\")",
                "reason": "E0101 undefined name"
            }]
        })
        .to_string(),
    );
    let config_path = root.join("ai-analysis.toml");
    write_config(&config_path, &base_url);

    let response = run_mcp_request(
        &[
            ("FIDAN_EXE", fake_fidan.display().to_string()),
            (
                "FIDAN_AI_ANALYSIS_CONFIG",
                config_path.display().to_string(),
            ),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "apply_fix_preview",
                "arguments": { "file": file }
            }
        }),
    );

    server.join().expect("join OpenAI stub server");
    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&root).ok();

    let diff = response["result"]["structuredContent"]["diff"]
        .as_str()
        .expect("apply_fix_preview diff string");
    assert!(diff.contains("--- "));
    assert!(diff.contains("+++ "));
    assert!(diff.contains("-    print(completely_unknown_xyz)"));
    assert!(diff.contains("+    print(\"hello\")"));
}
