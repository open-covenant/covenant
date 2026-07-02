//! The metering proxy — where spend is counted and the cap is enforced.
//!
//! The agent is pointed at this server on loopback (`ANTHROPIC_BASE_URL`). Every
//! model call is forwarded upstream and the response stream is teed: bytes go
//! back to the agent while the usage blocks are parsed and committed to the
//! ledger. When a commit crosses the budget the ledger trips, the proxy stops
//! forwarding the current response, and the run orchestrator kills the agent's
//! process group. Because metering happens as the stream flows, overshoot is
//! bounded to the one call in flight.
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

const UPSTREAM: &str = "https://api.anthropic.com";
const BODY_LIMIT: usize = 64 * 1024 * 1024;

/// Shared proxy state. Cloned (via `Arc`) into every request handler.
pub struct Proxy {
    client: reqwest::Client,
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
    pub fn new(ledger: Arc<Ledger>, chain: Arc<Chain>, inject: Option<String>) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .build()
            .expect("reqwest client with default (rustls) config");
        Arc::new(Self {
            client,
            upstream: UPSTREAM.to_string(),
            upstream_host: "api.anthropic.com".to_string(),
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
/// is intentionally empty — the agent reads its hook *command's* stdout, not
/// this body, so recording here never gates the agent.
async fn hook_sink(State(proxy): State<Arc<Proxy>>, body: Bytes) -> Response {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
        let event = v.get("hook_event_name").and_then(|x| x.as_str()).unwrap_or("hook").to_string();
        let tool = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
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
    let meter = path_q.starts_with("/v1/messages");

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

    proxy.network.lock().unwrap().insert(proxy.upstream_host.clone());

    let status = upstream.status();
    let ctype = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

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
                    let _ = tx.send(Err(std::io::Error::new(std::io::ErrorKind::Other, e))).await;
                    break;
                }
            }
        }
    });

    let mut builder = Response::builder().status(status);
    if let Some(ct) = ctype {
        builder = builder.header("content-type", ct);
    }
    builder.body(Body::from_stream(rx)).unwrap_or_else(|_| err_response(500, "failed to build response"))
}

fn err_response(code: u16, msg: &str) -> Response {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({ "error": { "type": "covguard_error", "message": msg } }).to_string()))
        .unwrap()
}

/// Incremental parser over the Anthropic SSE stream. Emits one `(model, Usage)`
/// per completed message. Tolerant of chunk boundaries and of non-SSE bodies
/// (which simply produce nothing).
struct SseUsage {
    buf: Vec<u8>,
    model: String,
    usage: Usage,
    open: bool,
}

impl SseUsage {
    fn new() -> Self {
        Self { buf: Vec::new(), model: String::new(), usage: Usage::default(), open: false }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<(String, Usage)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) else { continue };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("message_start") => {
                    self.open = true;
                    self.usage = Usage::default();
                    if let Some(m) = v.get("message") {
                        self.model = m.get("model").and_then(|x| x.as_str()).unwrap_or("unknown").to_string();
                        if let Some(u) = m.get("usage") {
                            self.usage.input = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                            self.usage.cache_read = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                            self.usage.cache_creation = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                            self.usage.output = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        }
                    }
                }
                Some("message_delta") => {
                    if let Some(o) = v.get("usage").and_then(|u| u.get("output_tokens")).and_then(|x| x.as_u64()) {
                        self.usage.output = o;
                    }
                }
                Some("message_stop") => {
                    if self.open {
                        out.push((self.model.clone(), self.usage));
                        self.open = false;
                    }
                }
                _ => {}
            }
        }
        out
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
    fn two_messages_two_emissions() {
        let mut p = SseUsage::new();
        let s = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\ndata: {\"type\":\"message_stop\"}\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":2}}}\ndata: {\"type\":\"message_stop\"}\n";
        let out = p.push(s.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].1.input, 2);
    }
}
