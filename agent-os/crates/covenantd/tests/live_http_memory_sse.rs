//! Live HTTP coverage for the dual-mode `GET /memory/recent` handler
//! wired in ADR 0010 slice 6.h.
//!
//! Three `#[ignore]`'d tests, each spawning a fresh covenantd against
//! a temp `COVENANT_HOME` and granting `memory.read` over HTTP before
//! issuing the request under test:
//!
//! 1. `Accept: text/event-stream` selects the SSE branch. The
//!    response carries the pinned header trio (`Content-Type:
//!    text/event-stream`, `Cache-Control: no-cache`,
//!    `X-Accel-Buffering: no`); the body is exactly two SSE event
//!    blocks (begin + end) for an empty memory store, delimited by
//!    `\n\n`. The blocks decode as a `stream_begin` with
//!    `response_kind: "memories"` and a `stream_end` whose `summary`
//!    key is omitted from the wire.
//! 2. `Accept: application/json` selects the buffered JSON branch.
//!    The wire shape is byte-identical with the v1 contract:
//!    `Content-Type: application/json` and body
//!    `{"kind":"memories","records":[]}`.
//! 3. No Accept header at all selects the buffered branch as well —
//!    the SSE classifier is opt-in (no `*/*` fallback), so the
//!    absent-Accept case must behave identically to (2).
//!
//! Assertions key off the SSE-frame structure (split body on `\n\n`,
//! parse `event:` and `data:` lines) and exact-string Content-Type
//! comparisons rather than substring matches, so a regression that
//! flips one block's framing or appends `charset=utf-8` to the
//! Content-Type fails the test instead of passing vacuously.
//!
//! The empty-page case is the documented test scope. Seeding memory
//! records would require a CLI verb that does not exist yet; the
//! begin+end pair already pins the SSE wire shape and the streaming
//! response_kind, which is what the slice's test-expansion gate
//! requires.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_memory_sse -- --ignored live_`.

use serde_json::Value;
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

async fn spawn_daemon_with_memory_read_grant() -> Daemon {
    let home = tempfile::tempdir().expect("tempdir");
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    let grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "action": "memory.read" }))
        .send()
        .await
        .expect("grant memory.read request")
        .json()
        .await
        .expect("grant memory.read json");
    assert_eq!(
        grant["kind"], "capability_granted",
        "operator self-grant of memory.read must succeed before issuing GET /memory/recent: {grant:?}",
    );

    Daemon {
        child,
        base,
        token,
        _home: home,
    }
}

impl Daemon {
    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /memory/recent with Accept: text/event-stream returns SSE begin+end frames"]
async fn live_http_memory_recent_sse_accept_returns_streamed_begin_and_end() {
    let daemon = spawn_daemon_with_memory_read_grant().await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    let response = client
        .get(format!("{}/memory/recent", daemon.base))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .expect("send GET /memory/recent SSE request");

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
        3,
        "empty memory store yields exactly 2 SSE event blocks terminated by \\n\\n, which split into 2 non-empty parts plus 1 trailing empty part; got body={body:?}",
    );
    assert_eq!(
        parts[2], "",
        "trailing SSE terminator must produce an empty final split part; got {:?}",
        parts[2],
    );

    let begin_block_lines: Vec<&str> = parts[0].split('\n').collect();
    assert_eq!(
        begin_block_lines.len(),
        2,
        "first SSE block must be exactly an event line and a data line; got {begin_block_lines:?}",
    );
    assert_eq!(
        begin_block_lines[0], "event: stream_begin",
        "first block's event line must name stream_begin",
    );
    let begin_data = begin_block_lines[1]
        .strip_prefix("data: ")
        .expect("first block's data line must start with 'data: '");
    let begin: Value = serde_json::from_str(begin_data).expect("first block's data must be JSON");
    assert_eq!(
        begin["kind"], "stream_begin",
        "begin envelope kind discriminator must round-trip through SSE: {begin:?}",
    );
    assert_eq!(
        begin["response_kind"], "memories",
        "begin envelope must announce response_kind=memories for the recent-memory stream: {begin:?}",
    );

    let end_block_lines: Vec<&str> = parts[1].split('\n').collect();
    assert_eq!(
        end_block_lines.len(),
        2,
        "second SSE block must be exactly an event line and a data line; got {end_block_lines:?}",
    );
    assert_eq!(
        end_block_lines[0], "event: stream_end",
        "second block's event line must name stream_end",
    );
    let end_data = end_block_lines[1]
        .strip_prefix("data: ")
        .expect("second block's data line must start with 'data: '");
    let end: Value = serde_json::from_str(end_data).expect("second block's data must be JSON");
    assert_eq!(
        end["kind"], "stream_end",
        "end envelope kind discriminator must round-trip through SSE: {end:?}",
    );
    assert!(
        end.get("summary").is_none(),
        "stream_end with no summary must omit the summary key on the wire (skip_serializing_if); got {end:?}",
    );

    daemon.shutdown().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /memory/recent with Accept: application/json returns v1-shape buffered JSON"]
async fn live_http_memory_recent_buffered_when_accept_omits_event_stream() {
    let daemon = spawn_daemon_with_memory_read_grant().await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    let response = client
        .get(format!("{}/memory/recent", daemon.base))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .expect("send GET /memory/recent buffered request");

    assert!(
        response.status().is_success(),
        "buffered branch must respond 2xx; got {} body={:?}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    let headers = response.headers().clone();
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .expect("buffered response must carry Content-Type");
    assert_eq!(
        content_type.to_str().expect("ASCII Content-Type"),
        "application/json",
        "Accept: application/json must select the v1 buffered path with the JSON content type",
    );

    let body: Value = response
        .json()
        .await
        .expect("buffered branch body must parse as JSON");
    assert_eq!(
        body,
        serde_json::json!({ "kind": "memories", "records": [] }),
        "Accept: application/json must yield the v1 byte-identical empty-page shape; got {body:?}",
    );

    daemon.shutdown().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /memory/recent without Accept header returns v1-shape buffered JSON"]
async fn live_http_memory_recent_buffered_when_no_accept_header() {
    let daemon = spawn_daemon_with_memory_read_grant().await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    let response = client
        .get(format!("{}/memory/recent", daemon.base))
        .bearer_auth(&daemon.token)
        .send()
        .await
        .expect("send GET /memory/recent no-Accept request");

    assert!(
        response.status().is_success(),
        "no-Accept branch must respond 2xx; got {} body={:?}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    let headers = response.headers().clone();
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .expect("no-Accept response must carry Content-Type");
    assert_eq!(
        content_type.to_str().expect("ASCII Content-Type"),
        "application/json",
        "absent Accept must fall through to the buffered path, NOT to SSE (the classifier is opt-in)",
    );

    let body: Value = response
        .json()
        .await
        .expect("no-Accept branch body must parse as JSON");
    assert_eq!(
        body,
        serde_json::json!({ "kind": "memories", "records": [] }),
        "no Accept header must yield the v1 byte-identical empty-page shape; got {body:?}",
    );

    daemon.shutdown().await;
}
