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
    ///
    /// Returns an error if reqwest fails to build its TLS-enabled
    /// client (typically a misconfigured system trust store). Boot
    /// callers should log the error and disable the Hermes runtime
    /// rather than panic.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            http,
        })
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

    /// Probe `GET /v1/capabilities` and return the advertised feature
    /// set. Used by the daemon at startup to warn loudly when the
    /// gateway is missing a feature this runner depends on (run
    /// submission, SSE event stream, stop). A failed probe (gateway
    /// down, auth missing) returns `None` — the daemon logs and
    /// continues, since transient outages should not block boot.
    pub async fn probe_capabilities(&self) -> Option<HermesCapabilities> {
        let url = format!("{}/capabilities", self.base_url);
        let resp = self.apply_auth(self.http.get(&url)).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let value: Value = resp.json().await.ok()?;
        let features = value.get("features")?.as_object()?;
        Some(HermesCapabilities {
            run_submission: feature_flag(features, "run_submission"),
            run_events_sse: feature_flag(features, "run_events_sse"),
            run_stop: feature_flag(features, "run_stop"),
            run_approval_response: feature_flag(features, "run_approval_response"),
        })
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
        // operator can audit a failed run end-to-end. A poisoned mutex
        // means the SSE parser panicked inside the lock — recover what
        // we can from the poison payload and warn so the operator knows
        // the trace may be incomplete.
        let drained = match events.lock() {
            Ok(mut v) => std::mem::take(&mut *v),
            Err(poisoned) => {
                warn!(%run_id, "hermes event lock poisoned — runtime trace may be incomplete");
                std::mem::take(&mut *poisoned.into_inner())
            }
        };

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
                        match sink.lock() {
                            Ok(mut v) => v.push(trace),
                            Err(poisoned) => {
                                // Already poisoned — push anyway. The
                                // main thread will surface the lock
                                // state via its own match arm above.
                                poisoned.into_inner().push(trace);
                            }
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
            resolved: value.get("resolved").and_then(|v| v.as_u64()).unwrap_or(0),
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

/// Subset of Hermes's advertised features that the Covenant runner
/// depends on. Anything we use is checked here so a misconfigured
/// gateway surfaces at startup rather than at first dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HermesCapabilities {
    pub run_submission: bool,
    pub run_events_sse: bool,
    pub run_stop: bool,
    pub run_approval_response: bool,
}

impl HermesCapabilities {
    /// `true` iff every feature this runner needs is advertised.
    pub fn covers_runner(&self) -> bool {
        self.run_submission && self.run_events_sse && self.run_stop
    }
}

fn feature_flag(features: &serde_json::Map<String, Value>, key: &str) -> bool {
    features.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
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
    fn parse_sse_frame_pins_strict_crlf_frame_parses_identically_to_lf() {
        // covenant_runtime::hermes::parse_sse_frame (line 359-383)
        // splits each frame on b'\n' and passes each line through
        // strip_cr (line 385-391) to remove a trailing b'\r' before
        // the 'data:' prefix match feeds serde_json. This is what
        // makes CRLF-formatted SSE streams (the strict SSE spec form)
        // decode identically to LF-formatted streams (what Hermes's
        // current aiohttp gateway emits).
        //
        // parse_sse_frame_handles_lf_lf_and_crlf NAMES the
        // CRLF case in its identifier but the body only exercises LF
        // — no test passes a 'data: {...}\r' or '...\r\n\r\n' frame
        // through parse_sse_frame. find_boundary_prefers_crlf_crlf_over_lf_lf
        // tests find_boundary, NOT the downstream line-level
        // CR strip. A refactor that dropped the strip_cr() call under
        // an 'aiohttp uses LF, we don't need this' rationale would
        // silently break parsing of any future strict-CRLF Hermes
        // deployment; the trailing \r would land inside the JSON body
        // and serde_json::from_str would reject it, dropping every
        // tool.started/tool.completed/approval.* trace with no
        // parse-time signal at the runner level.

        // Single-line CRLF frame: the trailing \r belongs to the SSE
        // line terminator and must be stripped before serde_json sees
        // the payload. If strip_cr is dropped, the \r lands in the
        // JSON suffix and parse fails silently.
        let single_line_crlf = b"data: {\"event\":\"tool.started\",\"run_id\":\"r-crlf\",\"tool\":\"terminal\",\"preview\":\"ls\"}\r";
        match parse_sse_frame(single_line_crlf).expect(
            "single-line CRLF SSE frame must parse — strip_cr must \
             remove the trailing \\r so the JSON payload reaches \
             serde_json clean. If this fires, parse_sse_frame is \
             handing \\r-terminated bytes to from_str and dropping \
             every event on a strict-CRLF Hermes deployment",
        ) {
            RuntimeTrace::HermesToolInvoked {
                run_id,
                tool,
                preview,
            } => {
                assert_eq!(
                    run_id, "r-crlf",
                    "run_id from CRLF frame must match the literal in \
                     the JSON; a mismatch here would indicate the \\r \
                     leaked into the payload and the value got coerced",
                );
                assert_eq!(
                    tool, "terminal",
                    "tool from CRLF frame must match the literal — \
                     pins that the strip happens BEFORE serde extracts \
                     the field, not as a post-decode cleanup",
                );
                assert_eq!(
                    preview, "ls",
                    "preview from CRLF frame must match the literal — \
                     anchors that string-valued fields are not \
                     trailing-\\r corrupted",
                );
            }
            other => {
                panic!("single-line CRLF frame must parse to HermesToolInvoked; got {other:?}")
            }
        }

        // Multi-line strict-CRLF frame matching the canonical SSE
        // termination shape ('\r\n\r\n' between frames). Anchors that
        // every line iterated by parse_sse_frame is stripped, not
        // just the first or last.
        let multi_line_crlf = b"data: {\"event\":\"approval.request\",\"run_id\":\"r2\",\"choices\":[\"once\",\"always\"]}\r\n\r\n";
        match parse_sse_frame(multi_line_crlf).expect(
            "multi-line CRLF SSE frame ending in \\r\\n\\r\\n must \
             parse — each line that the b'\\n' split yields carries a \
             trailing \\r that strip_cr must remove uniformly. A \
             refactor that stripped only the first or last line would \
             leave a \\r in the data: payload of any intermediate line",
        ) {
            RuntimeTrace::HermesApprovalRequested { run_id, choices } => {
                assert_eq!(run_id, "r2");
                assert_eq!(
                    choices,
                    vec!["once".to_string(), "always".to_string()],
                    "choices array from CRLF frame must round-trip the \
                     two literals in order — pins that array-typed \
                     fields decode through CRLF correctly, not just \
                     scalar string fields",
                );
            }
            other => {
                panic!("multi-line CRLF frame must parse to HermesApprovalRequested; got {other:?}")
            }
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
    fn strip_cr_pins_only_one_trailing_cr_and_preserves_middle_and_leading() {
        // hermes::strip_cr (line 385-391) is the SSE line-end
        // normalizer that runs on every line of an SSE frame before
        // parse_sse_frame slices the leading 'data:' prefix. The
        // function strips at most ONE trailing \r — the standard SSE
        // line ending under both LF and CRLF wire conventions, where
        // the framing layer already split on \n leaving an optional
        // \r at the end of each line.
        //
        // parse_sse_frame_pins_strict_crlf_frame_parses_identically_to_lf
        // (above) verifies the COMPOSITION of strip_cr inside
        // parse_sse_frame's lf-vs-crlf parity but never directly pins
        // strip_cr's arms. This pin closes:
        //   - only one trailing \r is stripped (not multiple)
        //   - input without trailing \r passes through untouched
        //   - middle \r is preserved (suffix-only strip)
        //   - empty input is safe (no panic, no underflow)
        //
        // A refactor that switched to .trim_end_matches(b'\r') would
        // strip multiple trailing \r and break SSE servers that emit
        // \r\r as a deliberate keepalive boundary. A refactor that
        // switched to .replace would strip middle \r from data lines
        // and silently mangle event payloads.
        assert_eq!(
            strip_cr(b"hello\r"),
            b"hello",
            "trailing \\r must be stripped — the canonical SSE line \
             ending under CRLF",
        );
        assert_eq!(
            strip_cr(b"hello"),
            b"hello",
            "no trailing \\r must be a no-op — the LF-only SSE line \
             ending must pass through unchanged",
        );
        assert_eq!(
            strip_cr(b"hello\r\r"),
            b"hello\r",
            "exactly one trailing \\r must be stripped, not multiple — \
             a refactor that switched to trim_end_matches(b'\\r') under \
             a 'be permissive of pathological multi-\\r servers' \
             rationale would silently strip both, masking double-CR-LF \
             keepalive boundaries that some SSE implementations emit \
             as inter-event separators",
        );
        assert_eq!(
            strip_cr(b"\r"),
            b"",
            "a bare \\r must strip to empty — pinning the underflow-\
             safety boundary on the smallest CR-bearing input",
        );
        assert_eq!(
            strip_cr(b""),
            b"",
            "empty input must return empty without panic — a refactor \
             that 'simplified' to &line[..line.len()-1] under an \
             'every SSE line has trailing \\r per spec' rationale would \
             underflow on usize subtraction and panic on the first \
             empty line of an SSE stream, crashing the daemon's \
             runtime-event ingest path",
        );
        assert_eq!(
            strip_cr(b"hel\rlo"),
            b"hel\rlo",
            "a \\r in the middle of the line must be preserved — a \
             refactor that switched to .replace(b'\\r', b'') under a \
             'normalize all CR everywhere' rationale would silently \
             strip \\r from operator-supplied event payload (e.g., a \
             tool response containing a Windows-style multi-line \
             string) and mangle every such payload before it reached \
             the JSON parser",
        );
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

    #[test]
    fn find_boundary_pins_crlf_priority_when_both_crlf_and_lf_lf_present_in_same_buffer() {
        // find_boundary (line 340-348) checks CRLF-CRLF FIRST via
        // find_subseq(buf, b"\r\n\r\n") and only falls back to LF-LF
        // when CRLF-CRLF is absent. The existing
        // find_boundary_prefers_crlf_crlf_over_lf_lf test exercises
        // CRLF-only, LF-only, and no-boundary buffers independently —
        // none of those arms covers the BOTH-PRESENT case where LF-LF
        // appears BEFORE CRLF-CRLF in the same buffer. Hermes's
        // aiohttp gateway emits CRLF-CRLF for spec-strict SSE clients,
        // but mixed line endings can survive intermediate proxies; a
        // buffer carrying 'pre\n\nbody\r\n\r\nrest' must surface the
        // CRLF-CRLF boundary at byte 9, NOT the LF-LF boundary at
        // byte 3 — otherwise the parser splits on the wrong byte and
        // parse_sse_frame discards the malformed leading 'frame'
        // silently.
        //
        // A refactor that swapped the if-else order to check LF-LF
        // first 'for speed' would silently regress strict-SSE
        // compatibility; a refactor that collapsed both checks into a
        // single .find() pattern would silently pick the
        // earliest-by-position match rather than the CRLF-CRLF
        // priority match.
        let mixed = b"pre\n\nbody\r\n\r\nrest";
        // LF-LF starts at byte 3 ("pre" is 3 bytes long).
        // CRLF-CRLF starts at byte 9 ("pre\n\nbody" is 9 bytes long).
        assert_eq!(
            find_subseq(mixed, b"\n\n"),
            Some(3),
            "fixture-sanity: LF-LF must be at byte 3 so the priority \
             assertion below tests the both-present case where LF-LF \
             appears strictly earlier than CRLF-CRLF in the buffer",
        );
        assert_eq!(
            find_subseq(mixed, b"\r\n\r\n"),
            Some(9),
            "fixture-sanity: CRLF-CRLF must be at byte 9 so the priority \
             assertion below tests the both-present case where CRLF-CRLF \
             appears strictly later than LF-LF in the buffer",
        );
        assert_eq!(
            find_boundary(mixed),
            Some(9),
            "find_boundary must return the CRLF-CRLF position (byte 9) \
             even when LF-LF appears earlier (byte 3) — strict-SSE \
             priority is the documented invariant. A refactor that \
             swapped the if-else order to check LF-LF first would \
             return Some(3) here and silently split parse_sse_frame on \
             the wrong byte, discarding the malformed leading 'frame' \
             and routing the actual frame's payload through a parser \
             call that sees only the trailing bytes",
        );

        // Cross-bind: confirm the CRLF-only and LF-only arms still
        // surface their respective positions. Anchors that the
        // priority-arm pin above is not accidentally specific to the
        // both-present buffer shape.
        let crlf_only = b"head\r\n\r\ntail";
        assert_eq!(
            find_boundary(crlf_only),
            Some(4),
            "CRLF-only buffer must surface the CRLF-CRLF position — \
             cross-bind for the priority pin so a refactor that broke \
             the CRLF-CRLF branch entirely surfaces here regardless of \
             whether LF-LF is also present",
        );
        let lf_only = b"head\n\ntail";
        assert_eq!(
            find_boundary(lf_only),
            Some(4),
            "LF-only buffer must surface the LF-LF position — cross-bind \
             for the priority pin so a refactor that removed the LF-LF \
             fallback under a 'strict CRLF-only mode' rationale surfaces \
             here and operators running through non-CRLF proxies still \
             see frame boundaries",
        );
    }

    #[test]
    fn hermes_runner_new_pins_greedy_trim_of_trailing_slashes_on_base_url() {
        // covenant_runtime::hermes::HermesRunner::new (line 63-75)
        // normalizes base_url on line 71 with:
        //
        //   base_url.into().trim_end_matches('/').to_string()
        //
        // trim_end_matches is GREEDY — it strips every trailing
        // slash, not just one. This is what makes subsequent
        // format!("{}/runs", self.base_url) produce a single-slash
        // join regardless of whether the operator's config has
        // 'http://x/v1', 'http://x/v1/', or 'http://x/v1///'.
        // Hermes's aiohttp gateway is strict about double-slash
        // paths (silent 404 on '/v1//runs' vs working '/v1/runs');
        // without the greedy trim, an operator's trailing-slash typo
        // would silently 404 every dispatch with no parse-time
        // signal.
        //
        // hermes_runner_rejects_non_hermes_runtime (lib.rs line
        // 1557) and composite_hermes_runtime_without_gateway_returns_hermes_unconfigured
        // construct HermesRunner with a no-trailing-slash base_url,
        // so the trim arm is never exercised. A refactor that swapped
        // trim_end_matches('/') for strip_suffix('/').unwrap_or(&s)
        // would silently leave multi-slash trailing inputs
        // un-normalized; pinning the trim against three explicit
        // input shapes catches the regression directly.

        let no_trailing = HermesRunner::new("http://x:8642/v1", None)
            .expect("client must build for the no-trailing-slash form");
        assert_eq!(
            no_trailing.base_url, "http://x:8642/v1",
            "no-trailing-slash input must be unchanged — anchors that \
             the trim is a no-op when nothing matches, NOT an \
             unconditional rewrite that could accidentally strip a \
             documented path segment",
        );

        let one_trailing = HermesRunner::new("http://x:8642/v1/", None)
            .expect("client must build for the single-trailing-slash form");
        assert_eq!(
            one_trailing.base_url, "http://x:8642/v1",
            "single trailing slash must be stripped — the canonical \
             operator typo (config 'http://x:8642/v1/' instead of \
             'http://x:8642/v1') must normalize to the no-slash form \
             so format!('{{}}/runs', base_url) produces '/v1/runs' \
             not '/v1//runs'",
        );

        let many_trailing = HermesRunner::new("http://x:8642/v1///", None)
            .expect("client must build for the multi-trailing-slash form");
        assert_eq!(
            many_trailing.base_url, "http://x:8642/v1",
            "multiple trailing slashes must ALL be stripped (greedy \
             trim) — a refactor that swapped trim_end_matches('/') \
             for strip_suffix('/').unwrap_or(&s) would strip only one \
             slash and leave 'http://x:8642/v1//' un-normalized, \
             producing '/v1///runs' on the next format!() join and \
             silently 404ing every dispatch on aiohttp 3.x",
        );

        let bare_host = HermesRunner::new("http://x:8642", None)
            .expect("client must build for a bare host base_url");
        assert_eq!(
            bare_host.base_url, "http://x:8642",
            "bare host with no path and no trailing slash must be \
             unchanged — anchors that the trim does not eat a \
             non-slash terminal character (e.g., would not strip the \
             port number)",
        );
    }

    #[test]
    fn runtime_name_pins_each_kind_to_documented_manifest_slug() {
        // covenant_runtime::hermes::runtime_name (line 455-462) maps
        // every RuntimeKind variant to the static slug the operator
        // sees in the 'got:' field of RunnerError::WrongRuntime when
        // they accidentally route a non-Hermes runtime through
        // HermesRunner. The slug must match the value the operator
        // typed in agent.toml's [agent].runtime field, so the slug
        // contract is documentation + runtime mapping.
        //
        // hermes_runner_rejects_non_hermes_runtime
        // (in covenant-runtime/src/lib.rs) only exercises the Python3
        // arm (its fixture is subprocess_manifest with runtime =
        // python3). The Node, RustBin, and Hermes arms of
        // runtime_name are unpinned by any test. A refactor that
        // renamed 'rust-bin' to 'rust_bin' (underscore normalization),
        // 'node' to 'nodejs' (official-project-name alignment), or
        // hoisted runtime_name into a Display impl derived from the
        // variant identifier (where 'RustBin' would silently become
        // 'rustbin' under naive to_lowercase rather than 'rust-bin'),
        // would make the WrongRuntime error display a slug that does
        // not match the operator's agent.toml value — debugging the
        // diverged slug adds a round-trip the operator should not
        // need to make.

        assert_eq!(
            runtime_name(RuntimeKind::Python3),
            "python3",
            "Python3 arm must surface as 'python3' — matches the \
             agent.toml schema validator's accepted value; cross-\
             bound by hermes_runner_rejects_non_hermes_runtime which \
             asserts got == \"python3\" on the WrongRuntime error",
        );
        assert_eq!(
            runtime_name(RuntimeKind::Node),
            "node",
            "Node arm must surface as 'node' — a refactor renaming \
             to 'nodejs' under an 'official Node.js project name' \
             rationale would silently make the WrongRuntime error \
             slug diverge from the agent.toml schema slug, and \
             operators copying the error back to their config would \
             trigger a manifest parse failure with no obvious \
             diagnostic link",
        );
        assert_eq!(
            runtime_name(RuntimeKind::RustBin),
            "rust-bin",
            "RustBin arm must surface as 'rust-bin' WITH the hyphen — \
             anchors against 'rustbin' (naive to_lowercase) or \
             'rust_bin' (underscore normalization). A refactor that \
             hoisted runtime_name into a Display impl derived from \
             the variant identifier would silently produce 'rustbin' \
             because to_lowercase has no insertion-character rule; \
             pinning the verbatim hyphenated form catches this \
             directly",
        );
        assert_eq!(
            runtime_name(RuntimeKind::Hermes),
            "hermes",
            "Hermes arm must surface as 'hermes' — anchors the \
             self-referential slug so a refactor that renamed the \
             Hermes runtime kind itself (e.g., to a vendor-namespaced \
             form) would have to update the slug in lockstep with \
             the agent.toml schema; without this pin, the slug could \
             silently shift while the WrongRuntime path is never \
             hit for hermes runtime cards in production",
        );
    }

    #[test]
    fn hermes_capabilities_covers_runner_pins_three_required_flags_excluding_approval_response() {
        // covenant_runtime::hermes::HermesCapabilities::covers_runner
        // (line 506-508) is the boot-time gate covenantd consults in
        // covenantd/src/main.rs to decide whether to emit
        // 'hermes gateway features confirmed' or
        // 'hermes gateway missing required features'.
        //
        // The contract: self.run_submission && self.run_events_sse
        // && self.run_stop — three required AND-folded flags.
        // run_approval_response is INTENTIONALLY EXCLUDED because
        // hermes-agent < v0.12 deployments do not advertise approval
        // support, and forcing it would silently disable Hermes for
        // those operators. The daemon logs run_approval_response
        // alongside the 'hermes gateway features confirmed' info span
        // in covenantd/src/main.rs but does not gate on it.
        //
        // No test pins covers_runner; the daemon-side consumer is
        // integration-only. A refactor that 'tightened' the AND to
        // also require run_approval_response would silently log
        // 'missing required features' against every < v0.12 gateway;
        // a refactor that 'relaxed' to OR-fold or to only require
        // run_submission would silently re-enable Hermes for
        // gateways missing the SSE event stream or stop endpoint —
        // dispatches would silently lose the audit trail or hang on
        // wall-clock budget overrun.

        let all_advertised = HermesCapabilities {
            run_submission: true,
            run_events_sse: true,
            run_stop: true,
            run_approval_response: true,
        };
        assert!(
            all_advertised.covers_runner(),
            "the happy path: every feature this runner needs is \
             advertised and the daemon must emit 'features \
             confirmed' at boot",
        );

        let missing_submission = HermesCapabilities {
            run_submission: false,
            ..all_advertised
        };
        assert!(
            !missing_submission.covers_runner(),
            "run_submission=false must drop covers_runner to false — \
             a gateway that cannot accept run submissions is unusable \
             and the daemon must surface 'missing required features' \
             at boot",
        );

        let missing_events_sse = HermesCapabilities {
            run_events_sse: false,
            ..all_advertised
        };
        assert!(
            !missing_events_sse.covers_runner(),
            "run_events_sse=false must drop covers_runner to false — \
             without the SSE event stream, every dispatch silently \
             loses the audit-chain Hermes step events. A refactor \
             that dropped this from the AND chain under a 'SSE is \
             optional, events are nice-to-have' rationale would \
             silently emit empty Hermes audit trails",
        );

        let missing_stop = HermesCapabilities {
            run_stop: false,
            ..all_advertised
        };
        assert!(
            !missing_stop.covers_runner(),
            "run_stop=false must drop covers_runner to false — \
             without the stop endpoint, the daemon cannot cancel a \
             wedged run on wall-clock budget overrun. A refactor \
             that dropped this from the AND chain would silently let \
             wedged Hermes runs hold connections indefinitely",
        );

        let missing_approval_only = HermesCapabilities {
            run_approval_response: false,
            ..all_advertised
        };
        assert!(
            missing_approval_only.covers_runner(),
            "run_approval_response=false (with the other three \
             advertised) must NOT drop covers_runner to false — \
             approval is an optional feature that hermes-agent \
             < v0.12 deployments do not advertise. A refactor that \
             added run_approval_response to the AND chain under a \
             'be strict about full feature coverage' rationale would \
             silently disable Hermes for every operator on the \
             documented compatibility floor; pinning this case \
             anchors the intentional exclusion",
        );
    }

    #[test]
    fn truncate_pins_ellipsis_character_and_inclusive_length_boundary() {
        // covenant_runtime::hermes::truncate (line 471-477) bounds the
        // Hermes remote-error text that feeds RunnerError::Remote
        // messages at line 115 and line 135 (both with max=512). Two
        // arms:
        //
        //   if s.len() <= max { s.to_string() }
        //   else { format!("{}…", &s[..max]) }
        //
        // The '…' is U+2026 (horizontal ellipsis, three UTF-8 bytes:
        // 0xE2 0x80 0xA6) — the documented single-codepoint truncation
        // marker operators grep dashboards for. A refactor that swapped
        // it for the ASCII three-dots '...' under a 'log-friendly
        // ASCII-only' rationale would silently change every truncated
        // Hermes error message; dashboards that scan for '…' would
        // lose every truncation signal. The boundary is INCLUSIVE
        // (<= max passes through verbatim); a refactor flipping the
        // comparison to strict '<' would silently truncate strings
        // whose length exactly equals max (e.g., a 512-byte response
        // would gain an ellipsis suffix indistinguishable from genuine
        // overflow). No existing test in this module exercises
        // truncate.

        assert_eq!(
            truncate("abc", 5),
            "abc",
            "len < max must pass through verbatim — the helper only \
             appends the ellipsis on the strictly-larger arm, and a \
             refactor that prepended a leading marker for 'short' \
             messages would silently change every passthrough output",
        );

        assert_eq!(
            truncate("abcde", 5),
            "abcde",
            "len == max must pass through verbatim — anchors the \
             inclusive '<= max' boundary against a refactor to strict \
             '<', which would silently truncate exactly-at-max strings \
             and append an ellipsis to messages that already fit (a \
             512-byte Hermes response that fits cleanly would gain a \
             '…' suffix indistinguishable from a 513+ byte truncation)",
        );

        assert_eq!(
            truncate("abcdef", 5),
            "abcde\u{2026}",
            "len > max must return the first max bytes followed by the \
             U+2026 horizontal-ellipsis character specifically — NOT \
             three ASCII dots '...', NOT the U+22EF midline-ellipsis \
             '⋯', NOT a bare cut. A refactor swapping '…' for '...' \
             under a 'log-friendly ASCII' rationale would silently \
             change every truncated Hermes error message and break \
             operator dashboards that grep for the single-codepoint \
             marker",
        );

        // Byte-level pin so a refactor that swapped the literal for a
        // visually similar but differently-encoded character (e.g.,
        // U+22EF midline ellipsis at 0xE2 0x8B 0xAF) would surface
        // even if the assert_eq! comparison was somehow defeated by a
        // future trait swap on the right-hand side.
        let out = truncate("abcdef", 5);
        let suffix = &out.as_bytes()[out.len() - 3..];
        assert_eq!(
            suffix,
            &[0xE2, 0x80, 0xA6],
            "the truncation suffix's last three bytes must be the \
             UTF-8 encoding of U+2026 (0xE2 0x80 0xA6); a refactor to \
             a similar-looking but differently-encoded glyph would \
             silently shift the byte layout while a string-equality \
             test could conceivably accept it under a future trait \
             swap",
        );
    }

    #[test]
    fn map_hermes_event_pins_missing_event_and_run_id_fields_return_none() {
        // covenant_runtime::hermes::map_hermes_event (line 393-453) is
        // the final step of the SSE pipeline that folds a parsed JSON
        // value into a typed RuntimeTrace. It opens with two
        // question-mark guards:
        //
        //   let event = value.get("event")?.as_str()?;
        //   let run_id = value.get("run_id")?.as_str()?.to_string();
        //
        // Each '?' silently drops the frame if the field is absent or
        // not a string. parse_sse_frame tests only feed well-formed
        // JSON through serde_json::from_str, so neither guard arm is
        // exercised directly today.
        //
        // The run_id guard is especially load-bearing: a refactor that
        // swapped the '?' for unwrap_or("") under a 'be permissive
        // about gateway payloads' rationale would silently fold every
        // malformed payload as a typed trace with an empty run_id,
        // breaking audit-row correlation with no parse-time signal.
        // A refactor that swapped the event '?' for unwrap_or with
        // any default that matches a real arm (e.g., "tool.started")
        // would silently populate the audit chain with stale defaults.

        let no_event = serde_json::json!({
            "run_id": "r-x",
            "tool": "terminal"
        });
        assert!(
            map_hermes_event(&no_event).is_none(),
            "Value with no 'event' field must yield None — the first \
             '?' fires here. A refactor that swapped it for \
             unwrap_or(\"tool.started\") under a 'default to the \
             most common event' rationale would silently route this \
             frame into the tool.started arm and emit a typed \
             HermesToolInvoked with stale defaults",
        );

        let event_not_str = serde_json::json!({
            "event": 1,
            "run_id": "r-x"
        });
        assert!(
            map_hermes_event(&event_not_str).is_none(),
            "Value where 'event' is a number must yield None — the \
             '.as_str()?' on line 394 fires here. A refactor that \
             loosened to to_string() would let a numeric 'event' \
             value stringify to '1' which falls through to the '_' \
             arm and returns None anyway; the more dangerous refactor \
             is one that special-cases numeric event codes (e.g., \
             '1 means tool.started') and starts emitting typed traces \
             from numeric inputs",
        );

        let no_run_id = serde_json::json!({
            "event": "tool.started",
            "tool": "terminal"
        });
        assert!(
            map_hermes_event(&no_run_id).is_none(),
            "Value with 'event' but no 'run_id' must yield None — the \
             '?' on line 395 fires here. A refactor that swapped this \
             for unwrap_or(\"\") would silently fold the frame as \
             HermesToolInvoked {{ run_id: \"\", tool: \"terminal\", ... }} \
             — empty-string run_ids break audit-row correlation \
             because every malformed payload would land in the same \
             phantom bucket. This is the highest-value pin in this \
             test",
        );

        let run_id_null = serde_json::json!({
            "event": "tool.started",
            "run_id": null,
            "tool": "terminal"
        });
        assert!(
            map_hermes_event(&run_id_null).is_none(),
            "Value where 'run_id' is explicit JSON null must yield \
             None — the '.as_str()?' on line 395 fires here. Distinct \
             from the missing-key case at the wire level but must \
             collapse to the same answer, so a refactor that \
             special-cased null (e.g., 'treat null run_id as anonymous \
             and emit an empty-string trace') surfaces here",
        );

        let happy = serde_json::json!({
            "event": "tool.started",
            "run_id": "r-happy",
            "tool": "terminal",
            "preview": "ls"
        });
        match map_hermes_event(&happy).expect(
            "the happy path: a well-formed tool.started Value must \
             fold to HermesToolInvoked. If this fires, both guard \
             arms have been over-tightened and the function rejects \
             valid frames",
        ) {
            RuntimeTrace::HermesToolInvoked {
                run_id,
                tool,
                preview,
            } => {
                assert_eq!(run_id, "r-happy");
                assert_eq!(tool, "terminal");
                assert_eq!(preview, "ls");
            }
            other => panic!("expected HermesToolInvoked, got {other:?}"),
        }
    }

    #[test]
    fn find_subseq_pins_empty_needle_and_short_haystack_return_none() {
        // covenant_runtime::hermes::find_subseq (line 350-357) is the
        // search primitive find_boundary (line 340-348) uses to locate
        // SSE frame terminators ('\r\n\r\n' or '\n\n') in the streaming
        // event buffer. Its body opens with two guards:
        //
        //   if needle.is_empty() || haystack.len() < needle.len() {
        //       return None;
        //   }
        //
        // The first guard is a correctness defense: slice::windows(0)
        // panics, so removing the guard would convert an empty-needle
        // call into a runtime panic mid-SSE-stream rather than a clean
        // None. The second guard is a perf optimization that documents
        // the early-out shape for any future swap to a memmem-style
        // search.
        //
        // find_boundary_prefers_crlf_crlf_over_lf_lf only
        // exercises non-empty four-byte and two-byte needles against
        // larger haystacks; neither guard arm is hit. A refactor that
        // dropped the is_empty() guard under a 'this branch can't be
        // reached' rationale would turn an unreachable-in-practice
        // call into a panic; a refactor that swapped windows()+position()
        // for a memmem helper without preserving the empty-needle
        // semantics would silently start returning Some(0) for an
        // empty needle (every byte position matches).

        assert_eq!(
            find_subseq(b"data: x\n\n", b""),
            None,
            "empty needle on a non-empty haystack must return None — \
             this is the correctness guard: slice::windows(0) panics, \
             so removing the is_empty() check would convert the call \
             into a runtime panic. A refactor that swapped \
             windows()+position() for a memmem-style helper without \
             preserving this semantics would silently return Some(0) \
             (every byte position matches an empty needle) and break \
             find_boundary's framing if it were ever passed an empty \
             terminator",
        );

        assert_eq!(
            find_subseq(b"", b""),
            None,
            "empty needle on an empty haystack must return None — pins \
             that the is_empty() guard fires before the length \
             comparison, not after. A refactor that reordered the \
             guards (e.g., 'check length first, then emptiness') would \
             still return None here via the length comparison, but \
             the explicit empty-needle-first pin makes the intent \
             visible to anyone reading the test",
        );

        assert_eq!(
            find_subseq(b"abc", b"abcd"),
            None,
            "needle one byte longer than haystack must return None — \
             pins the haystack.len() < needle.len() perf guard. \
             slice::windows(needle.len()) on a haystack shorter than \
             needle.len() returns an empty iterator (no panic), so \
             this assertion would still hold without the explicit \
             guard, but pinning it makes the early-return part of the \
             documented surface so a future refactor that drops it \
             surfaces here",
        );

        assert_eq!(
            find_subseq(b"abcd", b"abcd"),
            Some(0),
            "needle equal to haystack must return Some(0) — pins the \
             boundary case at the upper end of the length comparison, \
             not just below it. A refactor that wrote the guard as \
             haystack.len() <= needle.len() (off-by-one) would \
             silently return None here and break framing whenever an \
             SSE chunk arrived exactly the size of the terminator",
        );

        assert_eq!(
            find_subseq(b"data: x\r\n\r\n", b"\r\n\r\n"),
            Some(7),
            "needle found at the end of the haystack must return the \
             index of the match — pins the positive arm with the \
             canonical SSE-CRLF terminator at the documented offset. \
             find_boundary_prefers_crlf_crlf_over_lf_lf exercises this \
             same input through find_boundary; pinning it directly at \
             find_subseq anchors the contract independent of the \
             caller",
        );
    }

    #[test]
    fn feature_flag_pins_missing_key_and_non_bool_value_default_to_false() {
        // covenant_runtime::hermes::feature_flag (line 511-513) is the
        // only function probe_capabilities (line 147-161) consults to
        // fold a hermes gateway's /capabilities features map into the
        // typed HermesCapabilities struct. Its body is:
        //
        //   features.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
        //
        // The contract is intentionally strict: only a JSON `true`
        // counts as advertised. A missing key defaults to false (which
        // is correct because hermes-agent < v0.12 deployments omit
        // `run_approval_response` entirely, and the daemon must be
        // able to distinguish "not advertised" from "advertised false"
        // without that wrecking the covers_runner gate). A non-bool
        // value (string, number, null, array, object) also defaults
        // to false so a gateway that returns `{"run_submission": "true"}`
        // as a string cannot silently bypass the typed gate.
        //
        // No test pins these arms today. A refactor that switched the
        // `unwrap_or(false)` to `unwrap_or(true)` under an "optimistic
        // defaults" rationale would silently pass covers_runner against
        // any gateway that omits a flag entirely. A refactor that
        // dropped `as_bool()` in favor of a truthy-coercion helper
        // (e.g., `to_string().parse::<bool>()`) would let the same
        // gateway advertise capabilities it cannot honor.

        let mut features = serde_json::Map::new();
        assert!(
            !feature_flag(&features, "run_submission"),
            "missing key must default to false — hermes-agent < v0.12 \
             does not advertise run_approval_response at all, and the \
             daemon must treat that as 'not advertised' rather than \
             coerced-true. A refactor that switched unwrap_or(false) \
             to unwrap_or(true) under an optimistic-defaults rationale \
             would silently pass covers_runner against any gateway \
             that omits a flag",
        );

        features.insert("run_submission".into(), Value::Bool(true));
        assert!(
            feature_flag(&features, "run_submission"),
            "explicit JSON `true` is the only value that must yield \
             true — pins the happy path so a refactor that inverted \
             the as_bool result surfaces here",
        );

        features.insert("run_submission".into(), Value::Bool(false));
        assert!(
            !feature_flag(&features, "run_submission"),
            "explicit JSON `false` must yield false — pins the \
             explicit-negative case distinct from the missing-key \
             case so a refactor that conflated the two (e.g., 'treat \
             false as missing and default') surfaces here",
        );

        features.insert("run_submission".into(), Value::String("true".into()));
        assert!(
            !feature_flag(&features, "run_submission"),
            "string \"true\" must NOT coerce to bool true — a refactor \
             that swapped as_bool() for a truthy-coercion helper would \
             let a malformed gateway advertise capabilities it cannot \
             honor. The capability schema contract is strictly JSON \
             bool, not stringly-typed",
        );

        features.insert(
            "run_submission".into(),
            Value::Number(serde_json::Number::from(1)),
        );
        assert!(
            !feature_flag(&features, "run_submission"),
            "numeric 1 must NOT coerce to bool true — pins that a \
             gateway returning {{\"run_submission\": 1}} cannot bypass \
             the typed gate under a 'non-zero is truthy' refactor",
        );

        features.insert("run_submission".into(), Value::Null);
        assert!(
            !feature_flag(&features, "run_submission"),
            "explicit JSON null must yield false — distinct from the \
             missing-key case at the wire level but must collapse to \
             the same answer so a refactor that special-cased null \
             (e.g., 'treat null as opt-in') surfaces here",
        );

        features.insert(
            "run_submission".into(),
            Value::Array(vec![Value::Bool(true)]),
        );
        assert!(
            !feature_flag(&features, "run_submission"),
            "array value must yield false even when it contains a \
             true element — pins that as_bool inspects the outer \
             value, not nested ones; a refactor that recursed into \
             container values under a 'be permissive' rationale would \
             surface here",
        );

        features.insert(
            "run_submission".into(),
            Value::Object(serde_json::Map::new()),
        );
        assert!(
            !feature_flag(&features, "run_submission"),
            "object value must yield false — pins that the contract \
             is a flat bool, not a nested capability descriptor; a \
             refactor that switched to a structured feature schema \
             without updating this helper would surface here",
        );
    }
}
