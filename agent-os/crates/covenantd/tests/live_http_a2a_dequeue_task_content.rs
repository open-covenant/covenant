//! Live HTTP coverage for `GET /a2a/tasks/next` (`try_recv_a2a_task`).
//!
//! `GET /a2a/tasks/next` leases the next queued task for the authenticated
//! peer and serializes the whole `A2ATask` into a
//! `Response::A2ATaskOpt { task: Option<A2ATask> }` envelope
//! (`covenant-ipc/src/lib.rs`, `kind: "a2_a_task_opt"`). Every other live
//! HTTP dequeue test asserts only the leased task's `id`
//! (`live_http_a2a_results.rs`, `live_http_a2a_queue_min_lease_age.rs`, …),
//! so the task *body* is unproven over HTTP: a daemon that round-tripped
//! the id but dropped or garbled `intent_text`, `task_kind`, or
//! `deadline_ms` through the JSON boundary passes every existing test.
//!
//! Using the HTTP gateway exclusively, this authenticates as the operator,
//! grants `a2a.send.<self>`, posts one self-addressed task with a
//! distinctive `intent_text` marker, `task_kind = "research"`, and a
//! concrete future `deadline_ms`, then:
//!
//! 1. Leases it with `GET /a2a/tasks/next` and asserts the envelope is
//!    `kind: "a2_a_task_opt"`, the distinctive marker survives in the raw
//!    serialized body, and the deserialized `A2ATask` equals the posted
//!    task field-for-field — `id`, `intent_text`, `task_kind ==
//!    Some("research")`, and `deadline_ms` all match the posted values,
//!    not merely a non-null default.
//! 2. Calls `GET /a2a/tasks/next` again and asserts it drains to a null
//!    task — proving single-lease semantics over the HTTP boundary (a
//!    fresh queue holds exactly the one task).
//!
//! The send is gated by `a2a.send.<recipient>`; the dequeue is
//! authenticated-only. Sender and recipient are the same operator peer, so
//! the cross-peer `a2a.recv` admission gate is a loopback no-op — only the
//! single `a2a.send` grant is required, mirroring `live_http_a2a_results.rs`.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_dequeue_task_content -- --ignored live_`.

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

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /a2a/tasks/next round-trips the full A2ATask body (intent_text, task_kind, deadline_ms) over HTTP and drains single-lease"]
async fn live_http_a2a_dequeue_returns_full_task_body_over_http() {
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

    let send_grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": format!("a2a.send.{}", peer.display) }))
        .send()
        .await
        .expect("send a2a.send grant")
        .json()
        .await
        .expect("send grant body json");
    assert_eq!(send_grant["kind"], "capability_granted");

    // Distinctive, fully-populated task: a daemon that defaulted or
    // dropped any of these fields through the HTTP JSON boundary cannot
    // reproduce the exact posted values below.
    const MARKER: &str = "http-a2a-dequeue-body-integrity-probe";
    const DEADLINE_MS: u64 = 1_900_000_000_123;
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: MARKER.to_string(),
        task_kind: Some("research".to_string()),
        parent: None,
        deadline_ms: Some(DEADLINE_MS),
        idempotency: None,
    };

    let queued: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task")
        .json()
        .await
        .expect("post task body");
    assert_eq!(
        queued["kind"], "a2_a_task_queued",
        "granted send must enqueue the task: {queued:?}",
    );
    assert_eq!(
        queued.pointer("/task_id").and_then(Value::as_str),
        Some(task.id.to_string().as_str()),
        "queue ack must echo the posted task id: {queued:?}",
    );

    let leased: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("lease task")
        .json()
        .await
        .expect("lease body");
    assert_eq!(
        leased["kind"], "a2_a_task_opt",
        "dequeue must be an a2a_task_opt envelope: {leased:?}",
    );
    let row = leased
        .get("task")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| panic!("a queued task must lease over HTTP: {leased:?}"));
    assert!(
        serde_json::to_string(&row).unwrap().contains(MARKER),
        "the distinctive intent_text marker must survive the HTTP round-trip: {row:?}",
    );

    let typed: A2ATask =
        serde_json::from_value(row).expect("leased row deserializes as A2ATask");
    assert_eq!(typed.id, task.id, "round-tripped task id must match the posted id");
    assert_eq!(
        typed.intent_text, MARKER,
        "round-tripped intent_text must equal the posted body, not a default",
    );
    assert_eq!(
        typed.task_kind.as_deref(),
        Some("research"),
        "round-tripped task_kind must equal the posted Some(\"research\")",
    );
    assert_eq!(
        typed.deadline_ms,
        Some(DEADLINE_MS),
        "round-tripped deadline_ms must equal the exact posted value",
    );
    assert_eq!(
        typed, task,
        "the full leased A2ATask must equal the posted task field-for-field",
    );

    let drained: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("drain tasks")
        .json()
        .await
        .expect("drain body");
    assert_eq!(
        drained["kind"], "a2_a_task_opt",
        "second dequeue must still be an a2a_task_opt envelope: {drained:?}",
    );
    assert!(
        drained["task"].is_null(),
        "single-lease: a fresh queue holds exactly one task, so the second dequeue must drain to null: {drained:?}",
    );

    let _ = child.kill().await;
}
