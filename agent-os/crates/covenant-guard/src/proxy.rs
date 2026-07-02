//! The metering proxy: where spend is counted and the cap is enforced.
//!
//! The agent is pointed at this server on loopback (`ANTHROPIC_BASE_URL`). Every
//! model call is forwarded upstream and the response stream is teed: bytes go
//! back to the agent while the usage blocks are parsed and committed to the
//! ledger. When a commit crosses the budget the ledger trips, the proxy refuses
//! further metered calls and cuts the in-flight ones, and the run orchestrator
//! kills the agent's process group. Overshoot is bounded to the calls already
//! streaming when the cap trips (at least one model call), not the whole run.
//!
//! In pass-through mode the agent presents its own credential and the proxy
//! forwards it verbatim (this is what lets a subscription/OAuth session work).
//! In injection mode the proxy holds the credential and stamps it on every
//! outbound request, so the real key never enters the sandboxed environment.

use crate::chain::Chain;
use crate::ledger::Ledger;
use crate::pricing::Usage;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::Response;
use axum::routing::{any, get, post};
use axum::Router;
use futures::{SinkExt, StreamExt};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BODY_LIMIT: usize = 64 * 1024 * 1024;

/// Which provider the proxy forwards to. Selected per run from the agent being
/// guarded: Claude Code talks to Anthropic, Codex to OpenAI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Anthropic,
    OpenAI,
}

impl Host {
    pub fn base(&self) -> &'static str {
        match self {
            Host::Anthropic => "https://api.anthropic.com",
            Host::OpenAI => "https://api.openai.com",
        }
    }
    pub fn host(&self) -> &'static str {
        match self {
            Host::Anthropic => "api.anthropic.com",
            Host::OpenAI => "api.openai.com",
        }
    }
    /// The request paths whose responses carry billable usage. Everything the
    /// provider charges for must be here, or an agent could spend on an
    /// unmetered endpoint.
    fn meters(&self, path: &str) -> bool {
        match self {
            Host::Anthropic => path.starts_with("/v1/messages"),
            Host::OpenAI => {
                path.starts_with("/v1/responses")
                    || path.starts_with("/v1/chat/completions")
                    || path.starts_with("/v1/embeddings")
                    || path.starts_with("/v1/images")
                    || path.starts_with("/v1/audio")
            }
        }
    }
}

/// Shared proxy state. Cloned (via `Arc`) into every request handler.
pub struct Proxy {
    client: reqwest::Client,
    host: Host,
    upstream: String,
    upstream_host: String,
    /// `Some(header_value)` injects this `Authorization` on every request and
    /// strips any credential the agent sent; `None` forwards the agent's own.
    inject: Option<String>,
    pub ledger: Arc<Ledger>,
    pub chain: Arc<Chain>,
    network: Mutex<BTreeSet<String>>,
}

impl Proxy {
    pub fn new(
        ledger: Arc<Ledger>,
        chain: Arc<Chain>,
        inject: Option<String>,
        host: Host,
    ) -> Arc<Self> {
        // Bound the connect, not the request: SSE responses are long-lived by
        // design, so a total timeout would abort legitimate streams. A hung
        // connect is covered here; a mid-stream stall is covered by the run's
        // wall clock.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client with default (rustls) config");
        Arc::new(Self {
            client,
            host,
            upstream: host.base().to_string(),
            upstream_host: host.host().to_string(),
            inject,
            ledger,
            chain,
            network: Mutex::new(BTreeSet::new()),
        })
    }

    /// Hosts the proxy actually contacted upstream, for the receipt.
    pub fn network(&self) -> Vec<String> {
        self.network.lock().unwrap().iter().cloned().collect()
    }

    /// Commit one parsed usage block: price it, debit the ledger, record it on
    /// the chain. Synchronous so no lock is held across an await.
    fn record(&self, model: &str, usage: Usage) {
        let cumulative = self.ledger.commit(model, &usage);
        let cost = crate::pricing::cost_usd(model, &usage);
        self.chain.append(
            crate::now_ms(),
            "api_call",
            serde_json::json!({
                "model": model,
                "input_tokens": usage.input,
                "output_tokens": usage.output,
                "cache_read_tokens": usage.cache_read,
                "cache_creation_tokens": usage.cache_creation,
                "cache_creation_1h_tokens": usage.cache_creation_1h,
                "cost_usd": cost,
                "cumulative_usd": cumulative,
            }),
        );
        if self.ledger.is_killed() {
            self.chain.append(
                crate::now_ms(),
                "budget_exceeded",
                serde_json::json!({
                    "cumulative_usd": cumulative,
                    "budget_usd": self.ledger.budget(),
                }),
            );
        }
    }
}

/// Bind loopback on an ephemeral port and start serving. Returns the port and
/// the serving task's handle.
pub async fn start(proxy: Arc<Proxy>) -> anyhow::Result<(u16, tokio::task::JoinHandle<()>)> {
    let app = Router::new()
        .route("/", get(root_ok).head(root_ok))
        .route("/__guard/hook", post(hook_sink))
        .fallback(any(forward))
        .with_state(proxy);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((port, handle))
}

/// Answers the agent's `HEAD /` (and `GET /`) base-URL health probe. Without
/// this the agent stalls before its first call.
async fn root_ok() -> Response {
    Response::builder()
        .status(200)
        .header("content-length", "0")
        .body(Body::empty())
        .unwrap()
}

/// Records a hook callback from the agent as an event on the chain. The reply
/// is intentionally empty: the agent reads its hook *command's* stdout, not
/// this body, so recording here never gates the agent.
async fn hook_sink(State(proxy): State<Arc<Proxy>>, body: Bytes) -> Response {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
        let event = v
            .get("hook_event_name")
            .and_then(|x| x.as_str())
            .unwrap_or("hook")
            .to_string();
        let tool = v
            .get("tool_name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let command = v
            .get("tool_input")
            .and_then(|ti| ti.get("command"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        proxy.chain.append(
            crate::now_ms(),
            "hook",
            serde_json::json!({ "event": event, "tool": tool, "command": command }),
        );
    }
    Response::builder().status(200).body(Body::empty()).unwrap()
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "connection"
            | "accept-encoding"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-connection"
            | "upgrade"
    )
}

/// Forward one request upstream and tee the response back to the agent while
/// metering it.
async fn forward(State(proxy): State<Arc<Proxy>>, req: axum::extract::Request) -> Response {
    let (parts, body) = req.into_parts();
    let path_q = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    let meter = proxy.host.meters(&path_q);

    // Once the cap has tripped, refuse metered calls at the meter itself. The
    // kill signal tears the agent down, but a call that slips through the
    // teardown window (or a process that escaped the group via setsid) must not
    // reach the provider. This is what makes the cap hold regardless of the kill.
    if meter && proxy.ledger.is_killed() {
        return err_response(402, "spend cap reached; covguard is stopping this run");
    }

    let body_bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return err_response(400, "request body too large"),
    };

    // Build the outbound request: same method, upstream URL, forwarded headers
    // minus hop-by-hop ones (and minus accept-encoding, so the response arrives
    // as identity SSE we can parse).
    let url = format!("{}{}", proxy.upstream, path_q);
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in parts.headers.iter() {
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        if proxy.inject.is_some() && matches!(name.as_str(), "authorization" | "x-api-key") {
            continue; // the proxy supplies the credential in injection mode
        }
        headers.insert(name.clone(), value.clone());
    }
    if let Some(cred) = &proxy.inject {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(cred) {
            headers.insert(reqwest::header::AUTHORIZATION, v);
        }
    }

    let upstream = match proxy
        .client
        .request(parts.method, &url)
        .headers(headers)
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err_response(502, &format!("upstream request failed: {e}")),
    };

    proxy
        .network
        .lock()
        .unwrap()
        .insert(proxy.upstream_host.clone());

    let status = upstream.status();
    let ctype = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let is_sse = ctype
        .as_deref()
        .map(|c| c.contains("event-stream"))
        .unwrap_or(false);

    // A non-streaming metered response carries all its usage in one JSON body.
    // Buffer, meter, and return it whole; otherwise an agent could evade the
    // cap simply by not streaming.
    if meter && !is_sse {
        let bytes = upstream.bytes().await.unwrap_or_default();
        if let Some((model, usage)) = parse_message_usage(&bytes, proxy.host) {
            proxy.record(&model, usage);
        }
        let mut builder = Response::builder().status(status);
        if let Some(ct) = &ctype {
            builder = builder.header("content-type", ct);
        }
        return builder
            .body(Body::from(bytes))
            .unwrap_or_else(|_| err_response(500, "failed to build response"));
    }

    // Tee: forward chunks to the agent, parse usage as they pass.
    let (mut tx, rx) = futures::channel::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let task_proxy = proxy.clone();
    tokio::spawn(async move {
        let mut stream = upstream.bytes_stream();
        let mut parser = SseUsage::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    if meter {
                        for (model, usage) in parser.push(&chunk) {
                            task_proxy.record(&model, usage);
                        }
                    }
                    if tx.send(Ok(chunk)).await.is_err() {
                        break; // the agent hung up
                    }
                    if task_proxy.ledger.is_killed() {
                        // Cap crossed: stop feeding the runaway. The orchestrator
                        // is already tearing the process group down.
                        let _ = tx.close().await;
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e))).await;
                    break;
                }
            }
        }
        // A stream cut before its terminal event (upstream reset, the agent
        // hanging up, or our own kill) still spent real tokens. Count what we
        // saw; under-counting is the wrong direction for a cap.
        if meter {
            for (model, usage) in parser.finish() {
                task_proxy.record(&model, usage);
            }
        }
    });

    let mut builder = Response::builder().status(status);
    if let Some(ct) = ctype {
        builder = builder.header("content-type", ct);
    }
    builder
        .body(Body::from_stream(rx))
        .unwrap_or_else(|_| err_response(500, "failed to build response"))
}

fn err_response(code: u16, msg: &str) -> Response {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "error": { "type": "covguard_error", "message": msg } })
                .to_string(),
        ))
        .unwrap()
}

fn u64at(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Anthropic usage block → billing tiers. `input_tokens` already excludes cached
/// reads. Cache creation splits into 5-minute and 1-hour writes when the API
/// reports the breakdown; otherwise the aggregate is treated as 5-minute.
fn anthropic_usage(u: &serde_json::Value) -> Usage {
    let (c5, c1h) = match u.get("cache_creation") {
        Some(cc) => (
            u64at(cc, "ephemeral_5m_input_tokens"),
            u64at(cc, "ephemeral_1h_input_tokens"),
        ),
        None => (u64at(u, "cache_creation_input_tokens"), 0),
    };
    Usage {
        input: u64at(u, "input_tokens"),
        output: u64at(u, "output_tokens"),
        cache_read: u64at(u, "cache_read_input_tokens"),
        cache_creation: c5,
        cache_creation_1h: c1h,
    }
}

/// OpenAI Responses usage. Its `input_tokens` includes cached, so split cached
/// into its own tier.
fn openai_responses_usage(u: &serde_json::Value) -> Usage {
    let cached = u
        .get("input_tokens_details")
        .map(|d| u64at(d, "cached_tokens"))
        .unwrap_or(0);
    Usage {
        input: u64at(u, "input_tokens").saturating_sub(cached),
        output: u64at(u, "output_tokens"),
        cache_read: cached,
        ..Usage::default()
    }
}

/// OpenAI Chat Completions usage.
fn openai_chat_usage(u: &serde_json::Value) -> Usage {
    let cached = u
        .get("prompt_tokens_details")
        .map(|d| u64at(d, "cached_tokens"))
        .unwrap_or(0);
    Usage {
        input: u64at(u, "prompt_tokens").saturating_sub(cached),
        output: u64at(u, "completion_tokens"),
        cache_read: cached,
        ..Usage::default()
    }
}

/// Extract usage from a single non-streamed response body (the `stream:false`
/// path). None if the body isn't a recognizable metered response.
fn parse_message_usage(bytes: &[u8], host: Host) -> Option<(String, Usage)> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let u = v.get("usage")?;
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    let usage = match host {
        Host::Anthropic => anthropic_usage(u),
        Host::OpenAI if u.get("prompt_tokens").is_some() => openai_chat_usage(u),
        Host::OpenAI => openai_responses_usage(u),
    };
    Some((model, usage))
}

/// Incremental parser over the model provider's SSE stream. Emits one
/// `(model, Usage)` per completed message. Tolerant of chunk boundaries and of
/// non-SSE bodies (which simply produce nothing).
struct SseUsage {
    buf: Vec<u8>,
    model: String,
    usage: Usage,
    open: bool,
}

impl SseUsage {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            model: String::new(),
            usage: Usage::default(),
            open: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<(String, Usage)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
                continue;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                // Anthropic: usage accrues across message_start (input/cache) and
                // message_delta (output), sealed at message_stop.
                Some("message_start") => {
                    self.open = true;
                    self.model = v
                        .get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    self.usage = v
                        .get("message")
                        .and_then(|m| m.get("usage"))
                        .map(anthropic_usage)
                        .unwrap_or_default();
                }
                Some("message_delta") => {
                    if let Some(o) = v
                        .get("usage")
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(|x| x.as_u64())
                    {
                        self.usage.output = o;
                    }
                }
                Some("message_stop") => {
                    if self.open {
                        out.push((std::mem::take(&mut self.model), self.usage));
                        self.usage = Usage::default();
                        self.open = false;
                    }
                }
                // OpenAI Responses: one terminal event carries the whole turn's
                // usage. completed / incomplete / failed all bill.
                Some("response.completed")
                | Some("response.incomplete")
                | Some("response.failed") => {
                    if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                        let model = v
                            .get("response")
                            .and_then(|r| r.get("model"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        out.push((model, openai_responses_usage(u)));
                    }
                }
                _ => {
                    // OpenAI Chat Completions: the final chunk carries usage only
                    // when the client set stream_options.include_usage.
                    if v.get("object").and_then(|o| o.as_str()) == Some("chat.completion.chunk") {
                        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                            let model = v
                                .get("model")
                                .and_then(|x| x.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            out.push((model, openai_chat_usage(u)));
                        }
                    }
                }
            }
        }
        out
    }

    /// Flush a partially-seen Anthropic message when the stream ends without
    /// message_stop. OpenAI carries no incremental state, so nothing to flush.
    fn finish(&mut self) -> Vec<(String, Usage)> {
        if self.open {
            self.open = false;
            vec![(std::mem::take(&mut self.model), self.usage)]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_across_chunk_boundaries() {
        let mut p = SseUsage::new();
        // A message split mid-line to exercise buffering.
        let a = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":10}}}\n\nevent: message_delta\ndata: {\"type\":\"messa";
        let b = "ge_delta\",\"usage\":{\"output_tokens\":42}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert!(p.push(a.as_bytes()).is_empty());
        let out = p.push(b.as_bytes());
        assert_eq!(out.len(), 1);
        let (model, usage) = &out[0];
        assert_eq!(model, "claude-opus-4-8");
        assert_eq!(usage.input, 100);
        assert_eq!(usage.cache_read, 10);
        assert_eq!(usage.output, 42);
    }

    #[test]
    fn non_sse_body_yields_nothing() {
        let mut p = SseUsage::new();
        assert!(p.push(b"{\"error\":\"not a stream\"}\n").is_empty());
    }

    #[test]
    fn parses_openai_responses_usage() {
        let mut p = SseUsage::new();
        let s = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-5.5\",\"usage\":{\"input_tokens\":500,\"output_tokens\":120,\"input_tokens_details\":{\"cached_tokens\":200}}}}\n\n";
        let out = p.push(s.as_bytes());
        assert_eq!(out.len(), 1);
        let (model, u) = &out[0];
        assert_eq!(model, "gpt-5.5");
        assert_eq!(u.input, 300); // 500 total − 200 cached
        assert_eq!(u.cache_read, 200);
        assert_eq!(u.output, 120);
    }

    #[test]
    fn parses_openai_chat_completions_final_chunk() {
        let mut p = SseUsage::new();
        let s = "data: {\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.5\",\"usage\":{\"prompt_tokens\":80,\"completion_tokens\":20}}\n";
        let out = p.push(s.as_bytes());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.input, 80);
        assert_eq!(out[0].1.output, 20);
    }

    #[test]
    fn host_meter_paths() {
        assert!(Host::Anthropic.meters("/v1/messages?beta=true"));
        assert!(!Host::Anthropic.meters("/v1/models"));
        assert!(Host::OpenAI.meters("/v1/responses"));
        assert!(Host::OpenAI.meters("/v1/chat/completions"));
        assert!(Host::OpenAI.meters("/v1/embeddings"));
        assert!(!Host::OpenAI.meters("/v1/models"));
    }

    #[test]
    fn one_hour_cache_split_is_read() {
        let mut p = SseUsage::new();
        let s = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":10,\"cache_creation\":{\"ephemeral_5m_input_tokens\":100,\"ephemeral_1h_input_tokens\":200}}}}\ndata: {\"type\":\"message_stop\"}\n";
        let out = p.push(s.as_bytes());
        assert_eq!(out[0].1.cache_creation, 100);
        assert_eq!(out[0].1.cache_creation_1h, 200);
    }

    #[test]
    fn finish_flushes_a_cut_stream() {
        // message_start seen, no message_stop (stream cut at the cap).
        let mut p = SseUsage::new();
        let s = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":50}}}\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n";
        assert!(p.push(s.as_bytes()).is_empty());
        let flushed = p.finish();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].1.input, 50);
        assert_eq!(flushed[0].1.output, 7);
        assert!(p.finish().is_empty()); // idempotent
    }

    #[test]
    fn non_stream_body_metered() {
        let body = br#"{"type":"message","model":"claude-opus-4-8","usage":{"input_tokens":123,"output_tokens":45}}"#;
        let (model, u) = parse_message_usage(body, Host::Anthropic).unwrap();
        assert_eq!(model, "claude-opus-4-8");
        assert_eq!(u.input, 123);
        assert_eq!(u.output, 45);
    }

    #[test]
    fn two_messages_two_emissions() {
        let mut p = SseUsage::new();
        let s = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\ndata: {\"type\":\"message_stop\"}\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":2}}}\ndata: {\"type\":\"message_stop\"}\n";
        let out = p.push(s.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].1.input, 2);
    }
}
