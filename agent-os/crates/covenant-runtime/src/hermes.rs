//! Hermes runtime backend.
//!
//! Drives a remote Hermes-agent gateway (https://github.com/NousResearch/hermes-agent)
//! over its native `/v1/runs/*` HTTP surface. The covenant daemon dispatches
//! the intent, Hermes does the agent stepping, and we poll the run to
//! terminal state. Wall-clock budget comes from the agent manifest's
//! `resources.cpu_ms_per_task`; we issue `POST /v1/runs/{id}/stop` if
//! exceeded.
//!
//! The capability gate is enforced one level up: the daemon's
//! `dispatch_intent` runs `capability_check` before handing off to the
//! runner. This module's job is the wire protocol, not policy.
//!
//! While the dispatch is in flight, an SSE subscriber on
//! `/v1/runs/{id}/events` accumulates Hermes step events into a
//! `RuntimeTrace` buffer. The buffer is handed back on `AgentResult`,
//! and the daemon folds each entry into the hash-chained audit log as a
//! Hermes-prefixed `AuditKind` row.

use async_trait::async_trait;
use covenant_manifest::Runtime as RuntimeKind;
use covenant_router::AgentCard;
use covenant_types::Intent;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::{AgentResult, Runner, RunnerError, RuntimeTrace};

/// Default polling cadence while waiting for a run to reach a terminal
/// state. Hermes does not push us over the wire on the status endpoint,
/// so we sleep this long between GETs. Kept short so wall-clock budgets
/// are honored within reasonable precision.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Cap on a single HTTP request so a wedged Hermes gateway can't pin the
/// runner forever. Independent from the per-intent wall-clock budget.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Driver for a Hermes gateway. One instance is shared across dispatches
/// — the underlying `reqwest::Client` pools connections.
pub struct HermesRunner {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl HermesRunner {
    /// `base_url` is the gateway's API root including the version prefix,
    /// e.g. `http://127.0.0.1:8642/v1`. `api_key` is sent as a Bearer
    /// token; pass `None` only when the gateway is bound to loopback with
    /// no `API_SERVER_KEY` configured (Hermes permits unauthenticated
    /// loopback in that case).
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("build reqwest client");
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            http,
        }
    }

    fn ensure_allowed(&self, card: &AgentCard) -> Result<(), RunnerError> {
        if !matches!(card.manifest.agent.runtime, RuntimeKind::Hermes) {
            return Err(RunnerError::WrongRuntime {
                agent: card.id.clone(),
                expected: "hermes",
                got: runtime_name(card.manifest.agent.runtime),
            });
        }
        Ok(())
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    }

    async fn submit(&self, intent: &Intent) -> Result<String, RunnerError> {
        let url = format!("{}/runs", self.base_url);
        let body = json!({
            "input": intent.text,
            "session_id": intent.id.to_string(),
        });
        let resp = self
            .apply_auth(self.http.post(&url).json(&body))
            // Hermes dedupes identical submissions in a five-minute
            // window when an Idempotency-Key is present, which lets a
            // covenantd restart mid-dispatch reattach to the same run.
            .header("Idempotency-Key", intent.id.to_string())
            .send()
            .await
            .map_err(remote_err)?;
        let status = resp.status();
        let text = resp.text().await.map_err(remote_err)?;
        if !status.is_success() {
            return Err(RunnerError::Remote {
                status: status.as_u16(),
                message: truncate(&text, 512),
            });
        }
        let body: HermesRunCreated = serde_json::from_str(&text)
            .map_err(|source| RunnerError::MalformedStdout { source })?;
        Ok(body.run_id)
    }

    async fn poll(&self, run_id: &str) -> Result<HermesRunStatus, RunnerError> {
        let url = format!("{}/runs/{}", self.base_url, run_id);
        let resp = self
            .apply_auth(self.http.get(&url))
            .send()
            .await
            .map_err(remote_err)?;
        let status = resp.status();
        let text = resp.text().await.map_err(remote_err)?;
        if !status.is_success() {
            return Err(RunnerError::Remote {
                status: status.as_u16(),
                message: truncate(&text, 512),
            });
        }
        serde_json::from_str(&text).map_err(|source| RunnerError::MalformedStdout { source })
    }

    async fn cancel(&self, run_id: &str) {
        let url = format!("{}/runs/{}/stop", self.base_url, run_id);
        // Best-effort: we're already in an error/timeout path; log and
        // move on if the cancel itself fails.
        match self.apply_auth(self.http.post(&url)).send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!(%run_id, "hermes run stop accepted");
            }
            Ok(resp) => {
                warn!(%run_id, status = %resp.status(), "hermes run stop returned non-success");
            }
            Err(e) => {
                warn!(%run_id, error = %e, "hermes run stop send failed");
            }
        }
    }
}

#[async_trait]
impl Runner for HermesRunner {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError> {
        self.ensure_allowed(card)?;

        let budget = Duration::from_millis(card.manifest.resources.cpu_ms_per_task);
        let deadline = Instant::now() + budget;

        let run_id = self.submit(intent).await?;
        debug!(%run_id, agent = %card.id, "hermes run submitted");

        // Spawn the SSE subscriber in parallel with status polling. The
        // subscriber accumulates RuntimeTrace events into a shared vec
        // we drain into the AgentResult on completion. A failed SSE
        // subscription is non-fatal — status polling carries the
        // primary signal — but events that arrived before the failure
        // are preserved.
        let events = Arc::new(Mutex::new(Vec::<RuntimeTrace>::new()));
        let sse_handle = self.spawn_event_stream(run_id.clone(), Arc::clone(&events));

        let outcome = self.poll_until_terminal(&run_id, deadline, budget).await;

        // Stop the SSE task; if it's still streaming when the run finishes
        // we don't need any further events.
        sse_handle.abort();
        // Drain accumulated events regardless of success or failure so an
        // operator can audit a failed run end-to-end.
        let drained = events
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default();

        match outcome {
            Ok(RunOutcome::Completed { output }) => Ok(AgentResult {
                text: output,
                sources: Vec::new(),
                runtime_events: drained,
            }),
            Ok(RunOutcome::Failed { message }) => Err(RunnerError::Remote { status: 0, message }),
            Err(e) => Err(e),
        }
    }
}

enum RunOutcome {
    Completed { output: String },
    Failed { message: String },
}

impl HermesRunner {
    async fn poll_until_terminal(
        &self,
        run_id: &str,
        deadline: Instant,
        budget: Duration,
    ) -> Result<RunOutcome, RunnerError> {
        loop {
            if Instant::now() >= deadline {
                warn!(%run_id, ?budget, "hermes run timed out — stopping");
                self.cancel(run_id).await;
                return Err(RunnerError::Timeout(budget));
            }
            let status = self.poll(run_id).await?;
            match status.status.as_str() {
                "completed" => {
                    return Ok(RunOutcome::Completed {
                        output: status.output.unwrap_or_default(),
                    });
                }
                "failed" => {
                    return Ok(RunOutcome::Failed {
                        message: status
                            .error
                            .unwrap_or_else(|| "hermes run failed".to_string()),
                    });
                }
                "cancelled" => {
                    return Ok(RunOutcome::Failed {
                        message: "hermes run cancelled".to_string(),
                    });
                }
                // "queued" | "running" | "waiting_for_approval" | "stopping" → keep polling
                _ => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    }

    fn spawn_event_stream(
        &self,
        run_id: String,
        sink: Arc<Mutex<Vec<RuntimeTrace>>>,
    ) -> JoinHandle<()> {
        let url = format!("{}/runs/{}/events", self.base_url, run_id);
        let api_key = self.api_key.clone();
        let http = self.http.clone();
        tokio::spawn(async move {
            let mut req = http.get(&url);
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(%run_id, error = %e, "hermes event stream connect failed");
                    return;
                }
            };
            if !resp.status().is_success() {
                warn!(%run_id, status = %resp.status(), "hermes event stream returned non-success");
                return;
            }
            let mut stream = resp.bytes_stream();
            // SSE messages are terminated by a blank line. Buffer until
            // we see `\n\n`, then parse one message at a time. Comment
            // lines (`: …\n`) are keepalives we ignore.
            let mut buffer = Vec::<u8>::new();
            while let Some(chunk) = stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        debug!(%run_id, error = %e, "hermes event stream chunk error");
                        return;
                    }
                };
                buffer.extend_from_slice(&bytes);
                while let Some(idx) = find_boundary(&buffer) {
                    let frame = buffer[..idx].to_vec();
                    // Drop the boundary itself (`\n\n` or `\r\n\r\n`).
                    let advance = if buffer[idx..].starts_with(b"\r\n\r\n") {
                        idx + 4
                    } else {
                        idx + 2
                    };
                    buffer.drain(..advance);
                    if let Some(trace) = parse_sse_frame(&frame) {
                        if let Ok(mut v) = sink.lock() {
                            v.push(trace);
                        }
                    }
                }
            }
        })
    }
}

fn find_boundary(buf: &[u8]) -> Option<usize> {
    // CRLF-CRLF preferred for strict SSE; fall back to LF-LF since
    // Hermes's aiohttp gateway emits unix-style line endings.
    if let Some(i) = find_subseq(buf, b"\r\n\r\n") {
        Some(i)
    } else {
        find_subseq(buf, b"\n\n")
    }
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_sse_frame(frame: &[u8]) -> Option<RuntimeTrace> {
    // One frame can carry multiple lines; we only care about `data:` lines.
    // Multiple data lines in one frame concatenate with `\n`.
    let mut data = String::new();
    for line in frame.split(|b| *b == b'\n') {
        let line = strip_cr(line);
        if line.is_empty() || line.starts_with(b":") {
            continue;
        }
        if let Some(payload) = line.strip_prefix(b"data:") {
            // The colon-prefix may or may not have a leading space.
            let payload = payload.strip_prefix(b" ").unwrap_or(payload);
            if !data.is_empty() {
                data.push('\n');
            }
            // Lossy is safe — Hermes emits utf8 over the wire.
            data.push_str(&String::from_utf8_lossy(payload));
        }
    }
    if data.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&data).ok()?;
    map_hermes_event(&value)
}

fn strip_cr(line: &[u8]) -> &[u8] {
    if let Some(stripped) = line.strip_suffix(b"\r") {
        stripped
    } else {
        line
    }
}

fn map_hermes_event(value: &Value) -> Option<RuntimeTrace> {
    let event = value.get("event")?.as_str()?;
    let run_id = value.get("run_id")?.as_str()?.to_string();
    match event {
        "tool.started" => Some(RuntimeTrace::HermesToolInvoked {
            run_id,
            tool: value
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            preview: value
                .get("preview")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "tool.completed" => Some(RuntimeTrace::HermesToolCompleted {
            run_id,
            tool: value
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            duration_ms: value
                .get("duration")
                .and_then(|v| v.as_f64())
                .map(|s| (s * 1000.0) as u64)
                .unwrap_or(0),
            error: value
                .get("error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }),
        "approval.request" => Some(RuntimeTrace::HermesApprovalRequested {
            run_id,
            choices: value
                .get("choices")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "approval.responded" => Some(RuntimeTrace::HermesApprovalResponded {
            run_id,
            choice: value
                .get("choice")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            resolved: value.get("resolved").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        }),
        // message.delta / reasoning.available / run.completed / run.failed
        // are observed elsewhere (status poll for terminal states; deltas
        // are too high-volume to audit).
        _ => None,
    }
}

fn runtime_name(r: RuntimeKind) -> &'static str {
    match r {
        RuntimeKind::Python3 => "python3",
        RuntimeKind::Node => "node",
        RuntimeKind::RustBin => "rust-bin",
        RuntimeKind::Hermes => "hermes",
    }
}

fn remote_err(e: reqwest::Error) -> RunnerError {
    RunnerError::Remote {
        status: e.status().map(|s| s.as_u16()).unwrap_or(0),
        message: e.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[derive(Debug, Deserialize)]
struct HermesRunCreated {
    run_id: String,
}

#[derive(Debug, Deserialize)]
struct HermesRunStatus {
    status: String,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_frame_handles_lf_lf_and_crlf() {
        let lf = b"data: {\"event\":\"tool.started\",\"run_id\":\"r1\",\"tool\":\"terminal\",\"preview\":\"ls\"}";
        let trace = parse_sse_frame(lf).expect("tool.started must parse");
        match trace {
            RuntimeTrace::HermesToolInvoked {
                run_id,
                tool,
                preview,
            } => {
                assert_eq!(run_id, "r1");
                assert_eq!(tool, "terminal");
                assert_eq!(preview, "ls");
            }
            other => panic!("expected HermesToolInvoked, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_frame_ignores_comments_and_empty_lines() {
        // Hermes sends `: keepalive\n` comments every 30s. They must
        // not be parsed as data, and a frame that contains only
        // comments must yield no trace rather than panicking on an
        // empty JSON body.
        let comment_only = b": keepalive 1710000000\n: keepalive 1710000030";
        assert!(parse_sse_frame(comment_only).is_none());

        let mixed =
            b": keepalive\ndata: {\"event\":\"approval.responded\",\"run_id\":\"r2\",\"choice\":\"once\",\"resolved\":1}";
        match parse_sse_frame(mixed).expect("approval.responded must parse past comments") {
            RuntimeTrace::HermesApprovalResponded {
                run_id,
                choice,
                resolved,
            } => {
                assert_eq!(run_id, "r2");
                assert_eq!(choice, "once");
                assert_eq!(resolved, 1);
            }
            other => panic!("expected HermesApprovalResponded, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_frame_maps_tool_completed_with_seconds_to_milliseconds() {
        // Hermes emits `duration` as seconds (f64); we persist as
        // milliseconds in the audit row so a refactor of the unit
        // conversion would silently report sub-second tool calls as
        // zero duration. Pin the conversion at the runner boundary.
        let frame = b"data: {\"event\":\"tool.completed\",\"run_id\":\"r3\",\"tool\":\"read_file\",\"duration\":0.250,\"error\":false}";
        match parse_sse_frame(frame).unwrap() {
            RuntimeTrace::HermesToolCompleted {
                run_id,
                tool,
                duration_ms,
                error,
            } => {
                assert_eq!(run_id, "r3");
                assert_eq!(tool, "read_file");
                assert_eq!(duration_ms, 250);
                assert!(!error);
            }
            other => panic!("expected HermesToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_frame_drops_unhandled_event_kinds() {
        // message.delta is intentionally not folded (too high volume);
        // a refactor that started accepting it without adding an audit
        // mapping would silently 0-fill the runtime_events buffer.
        let frame = b"data: {\"event\":\"message.delta\",\"run_id\":\"r4\",\"delta\":\"hello\"}";
        assert!(parse_sse_frame(frame).is_none());
    }

    #[test]
    fn find_boundary_prefers_crlf_crlf_over_lf_lf() {
        let crlf = b"data: x\r\n\r\ndata: y\r\n";
        assert_eq!(find_boundary(crlf), Some(7));

        let lf = b"data: x\n\ndata: y\n";
        assert_eq!(find_boundary(lf), Some(7));

        // No boundary yet — buffer still mid-frame.
        assert!(find_boundary(b"data: x").is_none());
    }
}
