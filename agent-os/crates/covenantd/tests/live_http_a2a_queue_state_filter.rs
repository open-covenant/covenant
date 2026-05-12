//! Live HTTP coverage for `GET /a2a/queue?state_filter=queued|in_flight`.
//!
//! Authenticates as the operator, grants `a2a.send.<self>`, then uses
//! the HTTP gateway exclusively to send one task and lease it (flipping
//! its state to `in_flight`) and to send a second task that stays
//! queued. The four GETs that follow exercise:
//!
//! 1. No filter — both task ids visible by pubkey-keyed identity.
//! 2. `state_filter=queued` — only the queued task id visible.
//! 3. `state_filter=in_flight` — only the leased task id visible.
//! 4. `state_filter=Queued` (mixed case) — `LimitParams::state_filter`
//!    is a typed enum so axum's `Query` extractor rejects the request
//!    with `400 Bad Request` rather than degrading to no filter. This
//!    distinguishes the typed-enum query field from the untyped string
//!    `status` field on `/peers/list`, which intentionally degrades.
//!
//! Assertions compare task UUIDs (not counts) so a wrong-state row
//! leaking across filters fails the test instead of passing
//! vacuously.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_queue_state_filter -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

fn read_peer_pubkey(home: &Path) -> [u8; 32] {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity");
    id.pubkey_bytes()
}

fn task_for(peer: &AgentId, intent: &str) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: intent.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    }
}

fn task_ids_in(value: &Value) -> Vec<String> {
    value["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t.pointer("/task/id").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /a2a/queue?state_filter narrows by task id and rejects mixed-case variant"]
async fn live_http_a2a_queue_state_filter_narrows_each_state_and_rejects_mixed_case() {
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
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client");

    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);

    let grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": format!("a2a.send.{}", peer.display) }))
        .send()
        .await
        .expect("send capability grant")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(grant["kind"], "capability_granted");

    let in_flight = task_for(&peer, "in-flight http probe");
    let _: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&in_flight)
        .send()
        .await
        .expect("post first task")
        .json()
        .await
        .expect("first task body");
    let leased: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("lease task")
        .json()
        .await
        .expect("lease body");
    assert_eq!(
        leased.pointer("/task/id").and_then(Value::as_str),
        Some(in_flight.id.to_string().as_str()),
        "lease must return the only queued task id: {leased:?}",
    );

    let queued = task_for(&peer, "queued http probe");
    let _: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&queued)
        .send()
        .await
        .expect("post second task")
        .json()
        .await
        .expect("second task body");

    let unfiltered: Value = client
        .get(format!("{base}/a2a/queue?limit=20"))
        .send()
        .await
        .expect("queue unfiltered")
        .json()
        .await
        .expect("queue body");
    let unfiltered_ids = task_ids_in(&unfiltered);
    assert!(
        unfiltered_ids.iter().any(|id| id == &queued.id.to_string())
            && unfiltered_ids
                .iter()
                .any(|id| id == &in_flight.id.to_string()),
        "unfiltered /a2a/queue must surface both task ids: {unfiltered_ids:?}",
    );

    let queued_only: Value = client
        .get(format!("{base}/a2a/queue?limit=20&state_filter=queued"))
        .send()
        .await
        .expect("queue queued")
        .json()
        .await
        .expect("queue body");
    let queued_only_ids = task_ids_in(&queued_only);
    assert!(
        queued_only_ids
            .iter()
            .any(|id| id == &queued.id.to_string()),
        "state_filter=queued must keep the queued task id: {queued_only_ids:?}",
    );
    assert!(
        !queued_only_ids
            .iter()
            .any(|id| id == &in_flight.id.to_string()),
        "state_filter=queued must drop the in-flight task id: {queued_only_ids:?}",
    );

    let in_flight_only: Value = client
        .get(format!("{base}/a2a/queue?limit=20&state_filter=in_flight"))
        .send()
        .await
        .expect("queue in_flight")
        .json()
        .await
        .expect("queue body");
    let in_flight_only_ids = task_ids_in(&in_flight_only);
    assert!(
        in_flight_only_ids
            .iter()
            .any(|id| id == &in_flight.id.to_string()),
        "state_filter=in_flight must keep the leased task id: {in_flight_only_ids:?}",
    );
    assert!(
        !in_flight_only_ids
            .iter()
            .any(|id| id == &queued.id.to_string()),
        "state_filter=in_flight must drop the queued task id: {in_flight_only_ids:?}",
    );

    let mixed_case = client
        .get(format!("{base}/a2a/queue?limit=20&state_filter=Queued"))
        .send()
        .await
        .expect("queue mixed-case");
    assert!(
        mixed_case.status().is_client_error(),
        "mixed-case state_filter must fail at the typed-enum query layer (4xx) rather than degrade to no filter; got {}",
        mixed_case.status(),
    );

    let _ = child.kill().await;
}
