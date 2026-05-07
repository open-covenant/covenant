//! Hermetic MCP server stand-in. Reads JSON-RPC requests from stdin
//! (line-delimited), writes responses to stdout. Implements just enough of
//! the spec for the `live_` integration test in `tests/live_stdio.rs`:
//! `initialize`, `notifications/initialized`, `tools/list`, `tools/call`.
//!
//! Single tool: `ping` — returns the `text` argument prefixed with "pong: ".

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(s) => s,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = req.get("id").cloned();
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
            "tools/list" => json!({
                "tools": [{
                    "name": "ping",
                    "description": "echoes its `text` argument back with a pong prefix",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "text": { "type": "string" } },
                        "required": ["text"]
                    }
                }]
            }),
            "tools/call" => {
                let args = req
                    .get("params")
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null);
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
            _ => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {method}") }
                });
                writeln!(out, "{err}").ok();
                out.flush().ok();
                continue;
            }
        };

        let resp = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        });
        if writeln!(out, "{resp}").is_err() {
            break;
        }
        if out.flush().is_err() {
            break;
        }
    }
}
