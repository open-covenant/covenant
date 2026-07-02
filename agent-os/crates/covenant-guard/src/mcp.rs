//! A minimal Model Context Protocol server over stdio, so an agent (or a
//! Claude/Codex plugin) can read the guard's state through a standard interface.
//!
//! The tools are deliberately read-only: check the last run, list receipts,
//! verify one. Running an agent is the CLI's job, not something to trigger from
//! inside another agent's tool call. Transport is newline-delimited JSON-RPC
//! 2.0, which is the MCP stdio framing.

use crate::chain::Entry;
use crate::receipt::{self, Receipt};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL: &str = "2025-06-18";

fn receipts_dir() -> std::path::PathBuf {
    crate::home_dir().join("receipts")
}

fn resolve_id(which: &str) -> Option<String> {
    if which == "last" {
        std::fs::read_to_string(receipts_dir().join("last"))
            .ok()
            .map(|s| s.trim().to_string())
    } else {
        Some(which.to_string())
    }
}

fn load_receipt(id: &str) -> Option<(Receipt, std::path::PathBuf)> {
    let dir = receipts_dir().join(id);
    let bytes = std::fs::read(dir.join("receipt.json")).ok()?;
    let r: Receipt = serde_json::from_slice(&bytes).ok()?;
    Some((r, dir))
}

fn load_events(dir: &std::path::Path) -> Result<Option<Vec<Entry>>, String> {
    let text = match std::fs::read_to_string(dir.join("events.jsonl")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read events: {e}")),
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(line) {
            Ok(entry) => out.push(entry),
            Err(e) => return Err(format!("events.jsonl line {} is malformed: {e}", i + 1)),
        }
    }
    Ok(Some(out))
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "guard_status",
            "description": "Summarize the most recent guarded run: outcome, spend against the cap, turns, files changed.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "guard_receipts",
            "description": "List recent guarded runs with their outcome and spend.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "guard_verify",
            "description": "Verify a guarded run's receipt: signature and event chain. Pass a run id, or 'last'.",
            "inputSchema": {
                "type": "object",
                "properties": { "run": { "type": "string", "description": "Run id or 'last'" } },
                "required": ["run"],
                "additionalProperties": false
            }
        }
    ])
}

fn text_result(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn summarize(r: &Receipt) -> String {
    let c = &r.core;
    format!(
        "{}, {}\nspend ${:.2} of ${:.2} cap{}\nturns {} · files {} · {:.0}s\nrun {}",
        c.tool,
        c.outcome,
        c.spent_usd,
        c.budget_usd,
        if c.spend_estimated { " (est.)" } else { "" },
        c.calls,
        c.files_changed.len(),
        c.duration_s,
        c.run_id
    )
}

fn call_tool(name: &str, args: &Value) -> Value {
    match name {
        "guard_status" => match resolve_id("last").and_then(|id| load_receipt(&id)) {
            Some((r, _)) => text_result(summarize(&r)),
            None => text_result("no guarded runs yet".into()),
        },
        "guard_receipts" => {
            let mut rows: Vec<(u64, String)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(receipts_dir()) {
                for e in entries.flatten() {
                    if let Some((r, _)) = load_receipt(&e.file_name().to_string_lossy()) {
                        rows.push((r.core.started_ms, summarize(&r)));
                    }
                }
            }
            rows.sort_by_key(|(t, _)| *t);
            let text = if rows.is_empty() {
                "no guarded runs yet".to_string()
            } else {
                rows.into_iter()
                    .map(|(_, s)| s)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            };
            text_result(text)
        }
        "guard_verify" => {
            let which = args.get("run").and_then(|v| v.as_str()).unwrap_or("last");
            match resolve_id(which).and_then(|id| load_receipt(&id)) {
                Some((r, dir)) => {
                    let verdict = match load_events(&dir) {
                        Ok(Some(events)) => match receipt::verify_with_events(&r, &events) {
                            Ok(()) => format!("PASS: signature valid, {} events chain to the signed root", events.len()),
                            Err(e) => format!("FAIL: {e}"),
                        },
                        Ok(None) => match receipt::verify_signature(&r) {
                            Ok(()) => "PASS: signature valid (no event log alongside the receipt; chain not re-checked)".to_string(),
                            Err(e) => format!("FAIL: {e}"),
                        },
                        Err(e) => format!("FAIL: {e}"),
                    };
                    text_result(format!("{verdict}\n{}", summarize(&r)))
                }
                None => text_result(format!("no receipt for '{which}'")),
            }
        }
        _ => {
            json!({ "content": [{ "type": "text", "text": format!("unknown tool: {name}") }], "isError": true })
        }
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Handle one request. Returns `None` for notifications (no id / no reply).
fn handle(req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // Notifications carry no id and get no response.
    let id = req.get("id").cloned()?;
    match method {
        "initialize" => {
            let ver = req
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL)
                .to_string();
            Some(ok(
                id,
                json!({
                    "protocolVersion": ver,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "covenant-guard", "version": env!("CARGO_PKG_VERSION") }
                }),
            ))
        }
        "tools/list" => Some(ok(id, json!({ "tools": tool_defs() }))),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Some(ok(id, call_tool(name, &args)))
        }
        "ping" => Some(ok(id, json!({}))),
        other => Some(err(id, -32601, &format!("method not found: {other}"))),
    }
}

/// Serve MCP over stdio until the input closes. One JSON message per line.
pub fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        // A single undecodable byte must not tear down the session; reply with
        // a parse error and keep going, matching the malformed-JSON path.
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                writeln!(stdout, "{}", err(Value::Null, -32700, "parse error"))?;
                stdout.flush()?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                writeln!(stdout, "{}", err(Value::Null, -32700, "parse error"))?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(reply) = handle(&req) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_reports_tools_capability() {
        let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
        let reply = handle(&req).unwrap();
        assert_eq!(reply["result"]["serverInfo"]["name"], "covenant-guard");
        assert!(reply["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_has_the_three_tools() {
        let req = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let reply = handle(&req).unwrap();
        let names: Vec<String> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"guard_status".to_string()));
        assert!(names.contains(&"guard_verify".to_string()));
    }

    #[test]
    fn notification_gets_no_reply() {
        let req = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle(&req).is_none());
    }

    #[test]
    fn unknown_method_is_an_error() {
        let req = json!({"jsonrpc":"2.0","id":3,"method":"frobnicate"});
        let reply = handle(&req).unwrap();
        assert_eq!(reply["error"]["code"], -32601);
    }
}
