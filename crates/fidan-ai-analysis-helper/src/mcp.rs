use crate::fidan_client;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout().lock();

    loop {
        let Some(message) = read_message(&mut reader)? else {
            return Ok(());
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

        match method {
            "initialize" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": "2024-11-05",
                                "serverInfo": {
                                    "name": "fidan-ai-analysis-helper",
                                    "version": env!("CARGO_PKG_VERSION")
                                },
                                "capabilities": {
                                    "tools": {}
                                }
                            }
                        }),
                    )?;
                }
            }
            "notifications/initialized" => {}
            "tools/list" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": [
                                    tool("explain_target_context", "Return grounded explain context for a file/range."),
                                    tool("module_outline", "Return the top-level outline for a Fidan source file."),
                                    tool("project_summary", "Return the entry file plus imported project files and top-level items."),
                                    tool("symbol_info", "Return matching top-level symbol definitions for a file and symbol name.")
                                ]
                            }
                        }),
                    )?;
                }
            }
            "tools/call" => {
                if let Some(id) = id {
                    let result = handle_tool_call(&params);
                    match result {
                        Ok(value) => write_message(
                            &mut stdout,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                                        }
                                    ],
                                    "structuredContent": value,
                                    "isError": false
                                }
                            }),
                        )?,
                        Err(error) => write_message(
                            &mut stdout,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": format!("{error:#}")
                                        }
                                    ],
                                    "isError": true
                                }
                            }),
                        )?,
                    }
                }
            }
            "ping" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    )?;
                }
            }
            _ => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": format!("unsupported MCP method `{method}`")
                            }
                        }),
                    )?;
                }
            }
        }
    }
}

fn handle_tool_call(params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("tool call was missing `name`")?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    match name {
        "explain_target_context" => {
            let file = required_path(&args, "file")?;
            let line_start = optional_usize(&args, "line_start");
            let line_end = optional_usize(&args, "line_end");
            Ok(serde_json::to_value(
                fidan_client::request_explain_context(None, &file, line_start, line_end)?,
            )?)
        }
        "module_outline" => {
            let file = required_path(&args, "file")?;
            fidan_client::request_module_outline(None, &file)
        }
        "project_summary" => {
            let entry = required_path(&args, "entry")?;
            fidan_client::request_project_summary(None, &entry)
        }
        "symbol_info" => {
            let file = required_path(&args, "file")?;
            let symbol = args
                .get("symbol")
                .and_then(Value::as_str)
                .context("symbol_info requires `symbol`")?;
            fidan_client::request_symbol_info(None, &file, symbol)
        }
        _ => anyhow::bail!("unsupported MCP tool `{name}`"),
    }
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .context("failed to read MCP header line")?;
        if bytes == 0 {
            return Ok(None);
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid MCP Content-Length header")?,
            );
        }
    }

    let length = content_length.context("missing MCP Content-Length header")?;
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .context("failed to read MCP body")?;
    let value = serde_json::from_slice(&body).context("failed to parse MCP JSON body")?;
    Ok(Some(value))
}

fn write_message(writer: &mut impl Write, value: Value) -> Result<()> {
    let payload = serde_json::to_vec(&value).context("failed to serialize MCP message")?;
    write!(writer, "Content-Length: {}\r\n\r\n", payload.len())
        .context("failed to write MCP header")?;
    writer
        .write_all(&payload)
        .context("failed to write MCP body")?;
    writer.flush().context("failed to flush MCP output")
}

fn tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object"
        }
    })
}

fn required_path(args: &serde_json::Map<String, Value>, key: &str) -> Result<PathBuf> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("tool argument `{key}` is required"))?;
    Ok(PathBuf::from(value))
}

fn optional_usize(args: &serde_json::Map<String, Value>, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}
