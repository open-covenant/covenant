//! Live HTTP coverage for `GET /intents/:id/events` — the id-scoped SSE
//! replay route a reconnecting client uses to follow a specific
//! in-flight intent's live trace.
//!
//! Distinct from `live_http_intent_sse.rs`, which covers `POST /intent`
//! with `Accept: text/event-stream` (submit-and-stream). This route
//! instead subscribes to the daemon's live-trace broadcast
//! (`live_traces_tx`, fed only when `COVENANT_LIVE_TRACE=1`), filters
//! frames by the path intent id, and emits one SSE `data:` frame per
//! matching `StreamedTrace` with a keep-alive heartbeat on idle.
//!
//! Hermetic trace source. The only runner that emits `StreamedTrace`s is
//! the `HermesRunner`, so this test stands up an in-process wiremock
//! gateway in place of Hermes, registers a `runtime = hermes` agent card,
//! points `HERMES_API_BASE_URL` at the gateway, and drives a
//! `tool.started` SSE event through the gateway's `/runs/:id/events`
//! endpoint. A hermes-routed intent dispatches asynchronously
//! (`allow_async`): the verb returns the intent id immediately with
//! `status: "running"` and the run streams in a background task. That is
//! what lets the test subscribe to `/intents/:id/events` *before* the
//! gateway — deliberately delayed — publishes the trace, closing the
//! broadcast subscribe-vs-publish race (a broadcast channel does not
//! replay to late subscribers).
//!
//! One `#[ignore]`'d tokio test asserts:
//! - the stream scoped to the driven intent id delivers a `data:` frame
//!   carrying the public `tool_call` AgentEvent, and
//! - a concurrently-open stream scoped to a *different* intent id
//!   receives no `data:` frame from the same broadcast — pinning the
//!   per-intent filter (a handler that ignored the path id would leak the
//!   frame onto both streams).
//!
//! Both reads are tokio-timeout-bounded so the 15s idle heartbeat path
//! cannot hang the test.
//!
//! Hermetic — no external services (the gateway is in-process wiremock).
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_intent_events_sse -- --ignored live_`.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RUN_ID: &str = "live-events-run";

/// The gateway holds the event-stream response this long before delivering
/// the trace frame, so both SSE subscriptions are established before the
/// broadcast fires. Comfortably under the run's cpu budget and well over
/// the millisecond two local subscriptions take to open.
const EVENT_PUBLISH_DELAY: Duration = Duration::from_millis(1500);

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

/// In-process stand-in for the Hermes gateway. Advertises the
/// runner-required capabilities, accepts a run submission, holds the event
/// stream briefly then delivers one `tool.started` frame, and keeps the
/// run "running" so the daemon's background dispatch stays in-flight (and
/// the event subscriber stays live) across the test read window.
async fn spawn_hermes_gateway() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "features": {
                "run_submission": true,
                "run_events_sse": true,
                "run_stop": true,
                "run_approval_response": true,
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "run_id": RUN_ID })))
        .mount(&server)
        .await;

    // The gateway's wire form: `event: tool.started` parses to
    // RuntimeTrace::HermesToolInvoked, which the daemon projects to the
    // public `tool_call` AgentEvent before it hits the SSE wire.
    let frame = format!(
        "data: {}\n\n",
        json!({
            "event": "tool.started",
            "run_id": RUN_ID,
            "tool": "terminal",
            "preview": "ls -la",
        })
    );
    Mock::given(method("GET"))
        .and(path(format!("/runs/{RUN_ID}/events")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(frame)
                .set_delay(EVENT_PUBLISH_DELAY),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/runs/{RUN_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "running" })))
        .mount(&server)
        .await;

    server
}

struct Daemon {
    child: tokio::process::Child,
    base: String,
    token: String,
    _home: tempfile::TempDir,
}

async fn spawn_daemon_with_hermes(hermes_base_url: &str) -> Daemon {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("secrets.toml"),
        b"[embed]\nprovider = \"mock\"\n",
    )
    .expect("write secrets.toml");

    // A hermes-runtime agent the router will pick for a "build ..." intent.
    // `tool.code` sits in `optional` so the router's keyword bridge matches
    // the intent text while the dispatch capability gate (which reads
    // `capabilities.required`) stays empty — no extra operator grant needed.
    let agent_dir = home.path().join("agents").join("coder");
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    std::fs::write(
        agent_dir.join("agent.toml"),
        br#"[agent]
id = "coder"
name = "Coder"
version = "0.0.1"
runtime = "hermes"
entry = "./noop"

[capabilities]
optional = ["tool.code"]

[resources]
cpu_ms_per_task = 30000
network = "off"
"#,
    )
    .expect("write agent.toml");

    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");

    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .env("HERMES_API_BASE_URL", hermes_base_url)
        .env("COVENANT_LIVE_TRACE", "1")
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

impl Daemon {
    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

async fn grant_dispatch_capabilities(daemon: &Daemon) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    // The dispatch chain walks memory.write → embed → settlement before it
    // reaches the runner; grant the same four the buffered-dispatch live
    // test uses so the run reaches the hermes runner and streams a trace.
    for action in ["memory.write", "memory.read", "chain.receipts", "tool.web_search"] {
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
            "operator self-grant of {action} must succeed before driving the intent: {grant:?}",
        );
    }
}

/// Read SSE `data:` frames from a streaming response until one arrives or
/// `within` elapses. Comment heartbeats (`: keepalive`) carry no `data:`
/// line and are skipped. Returns the first `data:` payload, or `None` on
/// timeout / stream close — the bound is what keeps the idle-heartbeat
/// path from hanging the test.
async fn read_data_frame(resp: &mut reqwest::Response, within: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + within;
    let mut buf = String::new();
    loop {
        // Only parse a frame once its terminating blank line has arrived, so a
        // `data:` payload split across two chunks is never read half-formed.
        while let Some(boundary) = buf.find("\n\n") {
            let frame: String = buf.drain(..boundary + 2).collect();
            if let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) {
                return Some(data.to_string());
            }
            // A comment heartbeat (`: keepalive`) carries no data line; keep scanning.
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, resp.chunk()).await {
            Err(_) => return None,
            Ok(Ok(Some(bytes))) => buf.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Ok(None)) => return None,
            Ok(Err(_)) => return None,
        }
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + in-process wiremock hermes gateway; asserts GET /intents/:id/events streams that intent's tool_call frame over HTTP SSE and withholds it from a different intent id"]
async fn live_http_intent_events_streams_scoped_trace_and_filters_other_intents() {
    let gateway = spawn_hermes_gateway().await;
    let daemon = spawn_daemon_with_hermes(&gateway.uri()).await;
    grant_dispatch_capabilities(&daemon).await;

    // Drive a hermes-routed intent. allow_async returns the id immediately
    // with status "running" while the run streams in a background task — so
    // the subscriptions below are established before the (delayed) gateway
    // publishes the trace.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let submit: Value = client
        .post(format!("{}/intent", daemon.base))
        .bearer_auth(&daemon.token)
        .json(&json!({ "text": "build a website with a frontend page" }))
        .send()
        .await
        .expect("submit intent")
        .json()
        .await
        .expect("intent submit json");
    assert_eq!(
        submit["kind"], "intent_result",
        "submit must return an intent_result envelope: {submit:?}",
    );
    assert_eq!(
        submit["status"], "running",
        "a hermes-routed intent must dispatch asynchronously (status=running) so the id is \
         available before the run streams traces; got {submit:?}. A non-running status means the \
         router did not pick the hermes agent — check the agent card and keyword match",
    );
    let intent_id = submit["intent_id"]
        .as_str()
        .expect("intent_result must carry a string intent_id")
        .to_string();

    // A decoy id the driven intent will never use. Subscribed alongside the
    // real stream so the same broadcast is checked against both: the
    // per-intent filter must pass the trace to the scoped stream and
    // withhold it from the decoy.
    let decoy_id = "11111111-1111-4111-8111-111111111111";
    assert_ne!(
        intent_id, decoy_id,
        "decoy id must differ from the driven intent id",
    );

    // No total request timeout — these stay open for streaming; the reads
    // below are tokio-bounded instead.
    let sse_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest sse client");

    let mut scoped = sse_client
        .get(format!("{}/intents/{}/events", daemon.base, intent_id))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .expect("open scoped /intents/:id/events stream");
    assert!(
        scoped.status().is_success(),
        "scoped SSE subscribe must respond 2xx; got {}",
        scoped.status(),
    );

    // Pin the SSE header trio over the gateway — the route is otherwise
    // only unit-tested in-process.
    let headers = scoped.headers().clone();
    assert_eq!(
        headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
        "exact bare media type — strict EventSource clients reject a charset suffix",
    );
    assert_eq!(
        headers
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache"),
        "Cache-Control: no-cache keeps intermediate caches from holding frames back",
    );
    assert_eq!(
        headers
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no"),
        "X-Accel-Buffering: no defeats nginx response buffering on the stream",
    );

    let mut decoy = sse_client
        .get(format!("{}/intents/{}/events", daemon.base, decoy_id))
        .bearer_auth(&daemon.token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .expect("open decoy /intents/:id/events stream");
    assert!(
        decoy.status().is_success(),
        "decoy SSE subscribe must respond 2xx; got {}",
        decoy.status(),
    );

    // The scoped stream must deliver the broadcast trace as a public
    // tool_call AgentEvent. None here means the matching trace never
    // arrived (filter dropped it, the gateway never published, or the
    // subscribed id did not match the driven intent) — and the timeout is
    // what prevents the heartbeat-only path from hanging instead of failing.
    let frame = read_data_frame(&mut scoped, Duration::from_secs(12))
        .await
        .expect(
            "scoped /intents/:id/events must deliver a data frame for the driven intent before \
             the read budget elapses",
        );
    let event: Value = serde_json::from_str(&frame).expect("SSE data payload must be JSON");
    assert_eq!(
        event["type"], "tool_call",
        "the gateway's tool.started event must project to the public tool_call AgentEvent on the \
         wire (not the internal hermes_tool_invoked shape); got {event:?}",
    );
    assert_eq!(
        event["tool"], "terminal",
        "the AgentEvent must carry the tool name from the broadcast trace: {event:?}",
    );

    // The decoy stream must NOT receive that frame. The broadcast has
    // already fired (the scoped read above proved it), so a correct
    // per-intent filter leaves the decoy silent; a handler that ignored the
    // path id would have leaked the same frame onto this stream.
    let leaked = read_data_frame(&mut decoy, Duration::from_secs(2)).await;
    assert!(
        leaked.is_none(),
        "a stream scoped to a different intent id must receive no data frame from another \
         intent's trace; got leaked={leaked:?} — the per-intent filter is not pinning the path id",
    );

    daemon.shutdown().await;
    drop(gateway);
}
