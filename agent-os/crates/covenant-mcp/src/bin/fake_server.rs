//! Hermetic MCP server stand-in. Reads JSON-RPC requests from stdin
//! (line-delimited), writes responses to stdout. Implements just enough of
//! the spec for the `live_` integration test in `tests/live_stdio.rs`:
//! `initialize`, `notifications/initialized`, `tools/list`, `tools/call`.
//!
//! Default tool: `ping` — returns the `text` argument prefixed with
//! "pong: ". `--multi-tool` also exposes `sum` and `fail` so live tests
//! can exercise JSON content and tool-level error results.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

fn write_response(out: &mut impl Write, response: &Value, split_response: bool) -> io::Result<()> {
    if split_response {
        write!(out, "{response}")?;
        out.flush()?;
        thread::sleep(Duration::from_millis(10));
        writeln!(out)?;
    } else {
        writeln!(out, "{response}")?;
    }
    out.flush()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_after_initialize = args.iter().any(|arg| arg == "--exit-after-initialize");
    let string_ids = args.iter().any(|arg| arg == "--string-ids");
    let stderr_noise = args.iter().any(|arg| arg == "--stderr-noise");
    let split_response = args.iter().any(|arg| arg == "--split-response");
    let multi_tool = args.iter().any(|arg| arg == "--multi-tool");
    let oversized_response = args.iter().any(|arg| arg == "--oversized-response");
    let missing_result_error = args.iter().any(|arg| arg == "--missing-result-error");

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if stderr_noise {
        eprintln!("fake-mcp: stderr noise enabled");
    }

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(s) => s,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        if stderr_noise {
            eprintln!("fake-mcp: received line");
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = req.get("id").cloned();
        let response_id = if string_ids {
            match id.as_ref() {
                Some(Value::Number(n)) => Some(Value::String(n.to_string())),
                Some(Value::String(s)) => Some(Value::String(s.clone())),
                other => other.cloned(),
            }
        } else {
            id.clone()
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // Notifications carry no `id` and want no reply.
        if id.is_none() {
            continue;
        }

        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            }),
            "tools/list" => {
                let mut tools = vec![json!({
                    "name": "ping",
                    "description": "echoes its `text` argument back with a pong prefix",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "text": { "type": "string" } },
                        "required": ["text"]
                    }
                })];
                if multi_tool {
                    tools.push(json!({
                        "name": "sum",
                        "description": "adds two integer arguments and returns JSON content",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "a": { "type": "integer" },
                                "b": { "type": "integer" }
                            },
                            "required": ["a", "b"]
                        }
                    }));
                    tools.push(json!({
                        "name": "fail",
                        "description": "returns a tool-level error result",
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }
                    }));
                }
                json!({ "tools": tools })
            }
            "tools/call" => {
                let name = req
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("ping");
                let args = req
                    .get("params")
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null);
                match name {
                    "ping" => {
                        let text = args
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        json!({
                            "content": [{ "type": "text", "text": format!("pong: {text}") }],
                            "isError": false
                        })
                    }
                    "sum" if multi_tool => {
                        let a = args.get("a").and_then(Value::as_i64).unwrap_or(0);
                        let b = args.get("b").and_then(Value::as_i64).unwrap_or(0);
                        json!({
                            "content": [{ "type": "json", "value": { "sum": a + b } }],
                            "isError": false
                        })
                    }
                    "fail" if multi_tool => json!({
                        "content": [{ "type": "text", "text": "forced tool failure" }],
                        "isError": true
                    }),
                    "flood" if oversized_response => {
                        // Emit a single stdout line larger than the transport's
                        // 16 MiB MAX_LINE_BYTES read cap, with no trailing
                        // newline, so read_capped_line trips InvalidData and the
                        // client closes the transport instead of buffering an
                        // unbounded line. We then loop back to read stdin and
                        // stay alive, so the close is attributable to the cap
                        // rather than process exit.
                        let blob = "x".repeat(17 * 1024 * 1024);
                        let _ = out.write_all(blob.as_bytes());
                        let _ = out.flush();
                        continue;
                    }
                    "void" if missing_result_error => {
                        // A protocol-violating response carrying the matching id
                        // but neither `result` nor `error`. deliver_response's
                        // (None, None) arm synthesizes a -32603 error client-side
                        // rather than hanging the request or treating it as a
                        // success. The id is required so the response routes to
                        // the pending request instead of being dropped.
                        let bare = json!({ "jsonrpc": "2.0", "id": response_id });
                        let _ = write_response(&mut out, &bare, split_response);
                        continue;
                    }
                    _ => {
                        let err = json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "error": { "code": -32602, "message": format!("unknown tool: {name}") }
                        });
                        let _ = write_response(&mut out, &err, split_response);
                        continue;
                    }
                }
            }
            _ => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": response_id,
                    "error": { "code": -32601, "message": format!("method not found: {method}") }
                });
                let _ = write_response(&mut out, &err, split_response);
                continue;
            }
        };

        let resp = json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": result
        });
        if write_response(&mut out, &resp, split_response).is_err() {
            break;
        }

        if exit_after_initialize && method == "initialize" {
            return;
        }
    }
}
