//! Live HTTP coverage for `POST /a2a/tasks` (`send_a2a_task`).
//!
//! `POST /a2a/tasks` is the A2A enqueue edge: it gates on
//! `a2a.send.<recipient>`, enqueues the task, and acknowledges with a
//! `Response::A2ATaskQueued { task_id }` envelope (`kind:
//! "a2_a_task_queued"`) carrying the client-supplied id the handler
//! preserves (`send_a2a_task` binds `let task_id = task.id`,
//! `covenantd/src/lib.rs`). Every other live HTTP test treats the post as
//! setup and discards the response (`let _: Value = ...`), so two things
//! were unproven over the gateway: the enqueue acknowledgement envelope,
//! and the `a2a.send` capability rejection (the delegated-denial test
//! covers only `a2a.repair`/`a2a.compact`).
//!
//! Using the HTTP gateway exclusively, this authenticates as the operator
//! and drives both:
//!
//! 1. Posting before `a2a.send.<self>` is granted is rejected — the body
//!    is `kind: "error"` naming `a2a.send`, and `GET /a2a/tasks/next`
//!    returns `null`, proving the rejected task never enqueued.
//! 2. After the grant, the post returns `kind: "a2_a_task_queued"` whose
//!    `task_id` equals the client-supplied id (the daemon preserves it,
//!    it does not allocate a fresh one), and `GET /a2a/tasks/next` then
//!    leases a task whose `/task/id` equals that acknowledged `task_id` —
//!    binding the enqueue acknowledgement to the task a consumer actually
//!    leases, so a fabricated or stale id naming nothing in the queue
//!    fails the test.
//!
//! Sender and recipient are the same operator peer, so the cross-peer
//! `a2a.recv` admission gate is a loopback no-op — only the single
//! `a2a.send` grant gates the enqueue. Idempotency replay is out of scope:
//! A2A dedup is result-cache-based (populated on completion), so an
//! immediate re-post would enqueue a second task, not dedup.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_enqueue_envelope -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + asserts POST /a2a/tasks gates on a2a.send, then acknowledges with an a2_a_task_queued envelope whose task_id binds to the task GET /a2a/tasks/next leases"]
async fn live_http_a2a_enqueue_acknowledges_and_gates_over_http() {
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

    // Explicit client-supplied id — the handler preserves it rather than
    // allocating a fresh server id, so the acknowledged task_id below must
    // equal this exact value.
    const MARKER: &str = "http-a2a-enqueue-ack-probe";
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: MARKER.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    let denied: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task pre-grant")
        .json()
        .await
        .expect("pre-grant body");
    assert_eq!(
        denied["kind"], "error",
        "posting before a2a.send is granted must be rejected: {denied:?}",
    );
    assert!(
        denied["message"]
            .as_str()
            .is_some_and(|m| m.contains("a2a.send")),
        "rejection must name the missing a2a.send capability: {denied:?}",
    );

    let drained_after_deny: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("read tasks after denial")
        .json()
        .await
        .expect("tasks body");
    assert_eq!(
        drained_after_deny["kind"], "a2_a_task_opt",
        "task read must be an a2a_task_opt envelope: {drained_after_deny:?}",
    );
    assert!(
        drained_after_deny["task"].is_null(),
        "a rejected enqueue must not leave a task in the queue: {drained_after_deny:?}",
    );

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

    let queued: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task post-grant")
        .json()
        .await
        .expect("post-grant body");
    assert_eq!(
        queued["kind"], "a2_a_task_queued",
        "granted enqueue must acknowledge with an a2a_task_queued envelope: {queued:?}",
    );
    let acked_id = queued
        .pointer("/task_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("queue ack must carry a task_id: {queued:?}"));
    assert_eq!(
        acked_id,
        task.id.to_string(),
        "the acknowledged task_id must equal the client-supplied id (the daemon preserves it): {queued:?}",
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
        leased.pointer("/task/id").and_then(Value::as_str),
        Some(acked_id),
        "the acknowledged task_id must name the task a consumer actually leases: {leased:?}",
    );
    assert_eq!(
        leased.pointer("/task/intent_text").and_then(Value::as_str),
        Some(MARKER),
        "the leased task must be the one posted, not a different queue entry: {leased:?}",
    );

    let _ = child.kill().await;
}
