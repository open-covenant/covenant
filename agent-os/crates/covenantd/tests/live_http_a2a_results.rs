//! Live HTTP coverage for `POST /a2a/results` (`post_a2a_result`).
//!
//! `POST /a2a/results` is the A2A task-result submission edge. It ships
//! and is IPC-tested by `live_a2a.rs`, but no live test drives it over
//! the HTTP gateway, leaving the `Json<A2ATaskResult>` deserialization,
//! bearer-auth peer extraction, the `a2a.respond` capability gate, the
//! unknown-task guard, and mailbox result delivery unexercised on the
//! HTTP path. This mirrors the IPC result round-trip over the gateway.
//!
//! Using the HTTP gateway exclusively, it authenticates as the operator,
//! grants `a2a.send.<self>`, sends and leases one task, then exercises
//! `POST /a2a/results`:
//!
//! 1. Posting a result before `a2a.respond.<sender>` is granted is
//!    rejected — the body is `kind: "error"` naming `a2a.respond`, and
//!    `GET /a2a/results/next` still returns `null`, proving the rejected
//!    result never enqueued.
//! 2. Posting a result for a task id this daemon never dispatched is
//!    rejected by the unknown-task guard (checked before the capability
//!    gate), independent of any grant.
//! 3. After `a2a.respond.<sender>` is granted, the post returns
//!    `kind: "a2_a_result_posted"` with the matching task id.
//! 4. `GET /a2a/results/next` returns the posted result with its
//!    `task_id`, `status`, and content body intact, then drains to
//!    `null` — proving single delivery over the HTTP boundary.
//!
//! Assertions compare the task UUID and deserialize the returned row into
//! a typed `A2ATaskResult` so a wrong-task or wrong-status row fails the
//! test instead of passing vacuously.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_results -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult, A2ATaskStatus};
use covenant_mcp::Content;
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

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /a2a/results gates on a2a.respond, rejects undispatched tasks, and round-trips a posted result over HTTP"]
async fn live_http_a2a_results_post_gates_and_round_trips_over_http() {
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

    let task = task_for(&peer, "http result probe");
    let _: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task")
        .json()
        .await
        .expect("task body");
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
        Some(task.id.to_string().as_str()),
        "lease must return the dispatched task id: {leased:?}",
    );

    const MARKER: &str = "http-a2a-result-integrity-probe";
    let result = A2ATaskResult::ok(task.id, vec![Content::text(MARKER)]);

    let denied: Value = client
        .post(format!("{base}/a2a/results"))
        .json(&result)
        .send()
        .await
        .expect("post result pre-grant")
        .json()
        .await
        .expect("pre-grant body");
    assert_eq!(
        denied["kind"], "error",
        "posting before a2a.respond is granted must be rejected: {denied:?}",
    );
    assert!(
        denied["message"]
            .as_str()
            .is_some_and(|m| m.contains("a2a.respond")),
        "rejection must name the missing a2a.respond capability: {denied:?}",
    );

    let drained_after_deny: Value = client
        .get(format!("{base}/a2a/results/next"))
        .send()
        .await
        .expect("read results after denial")
        .json()
        .await
        .expect("results body");
    assert_eq!(
        drained_after_deny["kind"], "a2_a_result_opt",
        "result read must be an a2a_result_opt envelope: {drained_after_deny:?}",
    );
    assert!(
        drained_after_deny["result"].is_null(),
        "a rejected result must not enqueue: {drained_after_deny:?}",
    );

    let stray = A2ATaskResult::ok(Uuid::new_v4(), vec![Content::text("stray")]);
    let stray_rejected: Value = client
        .post(format!("{base}/a2a/results"))
        .json(&stray)
        .send()
        .await
        .expect("post stray result")
        .json()
        .await
        .expect("stray body");
    assert_eq!(
        stray_rejected["kind"], "error",
        "a result for an undispatched task id must be rejected: {stray_rejected:?}",
    );
    assert!(
        stray_rejected["message"]
            .as_str()
            .is_some_and(|m| m.contains("never dispatched")),
        "rejection must cite the unknown-task guard: {stray_rejected:?}",
    );

    let respond_grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": format!("a2a.respond.{}", peer.display) }))
        .send()
        .await
        .expect("send a2a.respond grant")
        .json()
        .await
        .expect("respond grant body json");
    assert_eq!(respond_grant["kind"], "capability_granted");

    let posted: Value = client
        .post(format!("{base}/a2a/results"))
        .json(&result)
        .send()
        .await
        .expect("post result post-grant")
        .json()
        .await
        .expect("post-grant body");
    assert_eq!(
        posted["kind"], "a2_a_result_posted",
        "granted post must be accepted: {posted:?}",
    );
    assert_eq!(
        posted.pointer("/task_id").and_then(Value::as_str),
        Some(task.id.to_string().as_str()),
        "accepted post must echo the task id: {posted:?}",
    );

    let received: Value = client
        .get(format!("{base}/a2a/results/next"))
        .send()
        .await
        .expect("read posted result")
        .json()
        .await
        .expect("received body");
    assert_eq!(received["kind"], "a2_a_result_opt");
    let row = received
        .get("result")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| panic!("posted result must be readable over HTTP: {received:?}"));
    assert!(
        serde_json::to_string(&row).unwrap().contains(MARKER),
        "result content must survive the HTTP round-trip intact: {row:?}",
    );
    let typed: A2ATaskResult =
        serde_json::from_value(row).expect("returned row deserializes as A2ATaskResult");
    assert_eq!(typed.task_id, task.id, "round-tripped task id must match");
    assert_eq!(
        typed.status,
        A2ATaskStatus::Ok,
        "round-tripped status must match the posted Ok",
    );

    let drained: Value = client
        .get(format!("{base}/a2a/results/next"))
        .send()
        .await
        .expect("drain results")
        .json()
        .await
        .expect("drain body");
    assert!(
        drained["result"].is_null(),
        "a delivered result must drain to null on the next read: {drained:?}",
    );

    let _ = child.kill().await;
}
