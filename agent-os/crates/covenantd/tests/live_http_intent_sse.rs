//! Live HTTP coverage for the dual-mode `POST /intent` handler wired
//! in ADR 0010 slice 6.j.
//!
//! Mirror of `live_http_memory_sse.rs` and `live_http_audit_sse.rs`
//! for the intent dispatch route, plus the slice's signature Err-arm
//! contract pin: when `submit_intent_envelopes` returns `Err(Response)`
//! (capability failure, ignore-rule match, budget exhaustion) the
//! daemon renders that response as a buffered JSON document with
//! `Content-Type: application/json` regardless of the client's Accept
//! header. The buffered shape is the daemon's "streaming refused"
//! signal; an SSE `stream_error` frame is reserved for streams that
//! opened then failed mid-flight.
//!
//! The dispatch path is hermetic: with an empty `agents/` directory,
//! no agent card matches the intent text and the daemon falls through
//! to its phase-0 echo branch (`phase 0 echo (no agent matched):
//! <text>`). The mock embedder is pinned via `secrets.toml` so the
//! daemon does not call out to a host Ollama. Same hermetic posture
//! as `live_cli_intent_dispatch.rs` and
//! `live_cli_memory_search_min_relevance.rs`.
//!
//! Four `#[ignore]`'d tokio tests:
//!
//! 1. SSE Accept + grants returns the 3-event SSE stream (begin
//!    response_kind=intent_result + chunk(AgentResult shape) + end
//!    with summary carrying intent_id/status/settlement).
//! 2. `Accept: application/json` + grants returns the v1 buffered
//!    IntentResult shape.
//! 3. No Accept + grants returns the same buffered IntentResult.
//! 4. SSE Accept + NO grants returns `Content-Type: application/json`
//!    with a buffered error envelope naming the missing capability —
//!    NOT a faked SSE stream wrapping the error.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_intent_sse -- --ignored live_`.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let token = text.trim();
            if !token.is_empty() {
                return token.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn wait_for_http(base: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    for _ in 0..80 {
        match client.get(format!("{base}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("http gateway never became healthy at {base}/health");
}

struct Daemon {
    child: tokio::process::Child,
    base: String,
    token: String,
    _home: tempfile::TempDir,
}

async fn spawn_daemon() -> Daemon {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("secrets.toml"),
        b"[embed]\nprovider = \"mock\"\n",
    )
    .expect("write secrets.toml");

    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");

    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_http(&base).await;

    let token = read_operator_token(home.path()).await;

    Daemon {
        child,
        base,
        token,
        _home: home,
    }
}

async fn spawn_daemon_with_intent_dispatch_grants() -> Daemon {
    let daemon = spawn_daemon().await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    // dispatch_intent's hermetic echo branch still walks the
    // memory.write → embed → settlement chain, so the test grants the
    // same four actions the http_gateway::intent_round_trip_after_grant
    // mock test uses. Sequential to keep the audit chain serial.
    for action in [
        "memory.write",
        "memory.read",
        "chain.receipts",
        "tool.web_search",
    ] {
        let grant: Value = client
            .post(format!("{}/capabilities/grant", daemon.base))
            .bearer_auth(&daemon.token)
            .json(&json!({ "action": action }))
            .send()
            .await
            .expect("grant request")
            .json()
            .await
            .expect("grant json");
        assert_eq!(
            grant["kind"], "capability_granted",
            "operator self-grant of {action} must succeed before issuing POST /intent: {grant:?}",
        );
    }

    daemon
}

impl Daemon {
    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

const ECHO_MARKER: &str = "phase 0 echo (no agent matched):";

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /intent with Accept: text/event-stream returns SSE begin+chunk+end frames"]
async fn live_http_intent_sse_accept_returns_streamed_begin_chunk_end() {
    let daemon = spawn_daemon_with_intent_dispatch_grants().await;

    let intent_text = "hello from sse";
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let response = client
        .post(format!("{}/intent", daemon.base))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&json!({ "text": intent_text }))
        .send()
        .await
        .expect("send POST /intent SSE request");

    assert!(
        response.status().is_success(),
        "SSE branch must respond 2xx; got {} body={:?}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    let headers = response.headers().clone();
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .expect("SSE response must carry Content-Type");
    assert_eq!(
        content_type.to_str().expect("ASCII Content-Type"),
        "text/event-stream",
        "exact bare media type — strict EventSource implementations reject a charset suffix",
    );
    let cache_control = headers
        .get(reqwest::header::CACHE_CONTROL)
        .expect("SSE response must carry Cache-Control");
    assert_eq!(
        cache_control.to_str().expect("ASCII Cache-Control"),
        "no-cache",
        "Cache-Control: no-cache is the pinned SSE shape so intermediate caches forward every chunk",
    );
    let x_accel = headers
        .get("x-accel-buffering")
        .expect("SSE response must carry X-Accel-Buffering");
    assert_eq!(
        x_accel.to_str().expect("ASCII X-Accel-Buffering"),
        "no",
        "X-Accel-Buffering: no defeats nginx response buffering on the streaming path",
    );

    let body = response.text().await.expect("read SSE body");
    let parts: Vec<&str> = body.split("\n\n").collect();
    assert_eq!(
        parts.len(),
        4,
        "intent dispatch yields exactly 3 SSE event blocks (begin + chunk + end) terminated by \\n\\n, which split into 3 non-empty parts plus 1 trailing empty part; got body={body:?}",
    );
    assert_eq!(
        parts[3], "",
        "trailing SSE terminator must produce an empty final split part; got {:?}",
        parts[3],
    );

    // ── begin envelope
    let begin_lines: Vec<&str> = parts[0].split('\n').collect();
    assert_eq!(
        begin_lines.len(),
        2,
        "first SSE block must be exactly an event line and a data line; got {begin_lines:?}",
    );
    assert_eq!(
        begin_lines[0], "event: stream_begin",
        "first block's event line must name stream_begin",
    );
    let begin: Value = serde_json::from_str(
        begin_lines[1]
            .strip_prefix("data: ")
            .expect("first block's data line must start with 'data: '"),
    )
    .expect("first block's data must be JSON");
    assert_eq!(begin["kind"], "stream_begin");
    assert_eq!(
        begin["response_kind"], "intent_result",
        "begin envelope must announce response_kind=intent_result for the submit-intent stream: {begin:?}",
    );

    // ── chunk envelope
    let chunk_lines: Vec<&str> = parts[1].split('\n').collect();
    assert_eq!(
        chunk_lines.len(),
        2,
        "second SSE block must be exactly an event line and a data line; embedded JSON newlines must be escaped, not literal; got {chunk_lines:?}",
    );
    assert_eq!(
        chunk_lines[0], "event: stream_chunk",
        "second block's event line must name stream_chunk",
    );
    let chunk: Value = serde_json::from_str(
        chunk_lines[1]
            .strip_prefix("data: ")
            .expect("second block's data line must start with 'data: '"),
    )
    .expect("second block's data must be JSON");
    assert_eq!(chunk["kind"], "stream_chunk");
    assert_eq!(
        chunk["sequence"], 0,
        "intent dispatch emits exactly one chunk with sequence=0: {chunk:?}",
    );
    let inner = &chunk["chunk"];
    assert!(
        inner.is_object(),
        "stream_chunk.chunk must be the AgentResult JSON object: {chunk:?}",
    );
    assert_eq!(
        inner["runtime_events"],
        json!([]),
        "AgentResult.runtime_events must be empty on the wire — dispatch_intent already folded them into the audit chain: {inner:?}",
    );
    let text = inner["text"]
        .as_str()
        .expect("AgentResult.text must be a string");
    assert!(
        text.contains(ECHO_MARKER) && text.contains(intent_text),
        "AgentResult.text must carry the echo-fallback marker and the operator's intent text; got {text:?}",
    );
    assert!(
        inner["sources"].is_array(),
        "AgentResult.sources must be an array (empty in the echo branch): {inner:?}",
    );

    // ── end envelope
    let end_lines: Vec<&str> = parts[2].split('\n').collect();
    assert_eq!(
        end_lines.len(),
        2,
        "third SSE block must be exactly an event line and a data line; got {end_lines:?}",
    );
    assert_eq!(
        end_lines[0], "event: stream_end",
        "third block's event line must name stream_end",
    );
    let end: Value = serde_json::from_str(
        end_lines[1]
            .strip_prefix("data: ")
            .expect("third block's data line must start with 'data: '"),
    )
    .expect("third block's data must be JSON");
    assert_eq!(end["kind"], "stream_end");
    let summary = end
        .get("summary")
        .expect("intent stream_end must carry a summary (IntentResult-only bookkeeping)");
    assert!(
        summary.get("intent_id").and_then(Value::as_str).is_some(),
        "summary.intent_id must be a string: {summary:?}",
    );
    assert!(
        summary.get("status").is_some(),
        "summary.status must be present: {summary:?}",
    );
    assert!(
        summary.get("settlement").is_some(),
        "summary.settlement must be present (Optional<SettlementReceipt>; null is fine, key must exist): {summary:?}",
    );

    daemon.shutdown().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /intent with Accept: application/json returns v1-shape buffered IntentResult"]
async fn live_http_intent_buffered_when_accept_omits_event_stream() {
    let daemon = spawn_daemon_with_intent_dispatch_grants().await;

    let intent_text = "hello from buffered json";
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let response = client
        .post(format!("{}/intent", daemon.base))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&json!({ "text": intent_text }))
        .send()
        .await
        .expect("send POST /intent buffered request");

    assert!(
        response.status().is_success(),
        "buffered branch must respond 2xx; got {} body={:?}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .expect("buffered response must carry Content-Type")
        .to_str()
        .expect("ASCII Content-Type")
        .to_string();
    assert_eq!(
        content_type, "application/json",
        "Accept: application/json must select the v1 buffered path with the JSON content type",
    );

    let body: Value = response
        .json()
        .await
        .expect("buffered branch body must parse as JSON");
    assert_eq!(
        body["kind"], "intent_result",
        "Accept: application/json must yield the v1 IntentResult shape; got {body:?}",
    );
    let text = body["text"]
        .as_str()
        .expect("IntentResult.text must be a string");
    assert!(
        text.contains(ECHO_MARKER) && text.contains(intent_text),
        "IntentResult.text must carry the echo-fallback marker and the operator's text; got {text:?}",
    );

    daemon.shutdown().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /intent without Accept header returns v1-shape buffered IntentResult"]
async fn live_http_intent_buffered_when_no_accept_header() {
    let daemon = spawn_daemon_with_intent_dispatch_grants().await;

    let intent_text = "hello without accept";
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let response = client
        .post(format!("{}/intent", daemon.base))
        .bearer_auth(&daemon.token)
        .json(&json!({ "text": intent_text }))
        .send()
        .await
        .expect("send POST /intent no-Accept request");

    assert!(
        response.status().is_success(),
        "no-Accept branch must respond 2xx; got {} body={:?}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .expect("no-Accept response must carry Content-Type")
        .to_str()
        .expect("ASCII Content-Type")
        .to_string();
    assert_eq!(
        content_type, "application/json",
        "absent Accept must fall through to the buffered path, NOT to SSE (the classifier is opt-in)",
    );

    let body: Value = response
        .json()
        .await
        .expect("no-Accept branch body must parse as JSON");
    assert_eq!(body["kind"], "intent_result");
    let text = body["text"]
        .as_str()
        .expect("IntentResult.text must be a string");
    assert!(
        text.contains(ECHO_MARKER) && text.contains(intent_text),
        "IntentResult.text must carry the echo-fallback marker and the operator's text; got {text:?}",
    );

    daemon.shutdown().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts SSE-Accept POST /intent without memory.write grant returns BUFFERED JSON error, not a faked SSE error stream"]
async fn live_http_intent_sse_accept_renders_capability_failure_as_buffered_json() {
    let daemon = spawn_daemon().await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    let response = client
        .post(format!("{}/intent", daemon.base))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&json!({ "text": "hello with no grants" }))
        .send()
        .await
        .expect("send POST /intent SSE-accept without grant");

    assert!(
        response.status().is_success(),
        "Err-arm fallback must still respond 2xx (the daemon returns the error envelope as a buffered JSON body, not as an HTTP-level failure): got {} body={:?}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .expect("Err-arm fallback response must carry Content-Type")
        .to_str()
        .expect("ASCII Content-Type")
        .to_string();
    assert_eq!(
        content_type, "application/json",
        "Err-arm fallback must render as buffered JSON regardless of Accept; rendering as text/event-stream here would force v2 consumers to disambiguate 'streaming refused' from 'streaming failed mid-flight' on the wire",
    );

    let body: Value = response
        .json()
        .await
        .expect("Err-arm fallback body must parse as JSON");
    assert_eq!(
        body["kind"], "error",
        "missing memory.write grant must surface as Response::Error: got {body:?}",
    );
    let message = body["message"]
        .as_str()
        .expect("error envelope must carry a message string");
    assert!(
        message.contains("memory.write"),
        "Err-arm message must name the missing capability so the operator can grant it; got {message:?}",
    );

    daemon.shutdown().await;
}
