//! Live HTTP coverage for `GET /a2a/queue?min_lease_age_ms`.
//!
//! The lease-age filter is live-covered only over the CLI/socket path
//! (`live_cli_a2a_status_min_lease_age_restart.rs`).
//! `live_http_a2a_queue_state_filter.rs` covers only `state_filter` over the
//! gateway, so the `min_lease_age_ms` query param — deserialized into
//! `LimitParams` and forwarded to `Request::A2AQueue` in covenantd's HTTP
//! layer — is never parsed-and-applied through HTTP. This mirrors the CLI
//! lease-age coverage on the HTTP boundary.
//!
//! `a2a_entry_matches_min_lease_age` keeps every non-in-flight entry
//! unconditionally and keeps an in-flight entry only when
//! `now - leased_at_ms >= threshold`, so the leased task is the discriminating
//! row. Send one task and lease it (in_flight, accruing age) and send one task
//! that stays queued; under a small threshold both are visible, and under a
//! far-future threshold the leased task drops out while the queued task — exempt
//! from the lease-age filter — remains. The leased task flipping present→absent
//! across the two thresholds is what rules out a gateway that ignores the param.
//! Assertions compare task UUIDs, not counts, so a wrong-state row leaking
//! across the filter fails instead of passing vacuously.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_queue_min_lease_age -- --ignored live_`.

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

async fn queue_ids(client: &reqwest::Client, base: &str, min_lease_age_ms: u64) -> Vec<String> {
    let body: Value = client
        .get(format!(
            "{base}/a2a/queue?limit=20&min_lease_age_ms={min_lease_age_ms}"
        ))
        .send()
        .await
        .expect("queue request")
        .json()
        .await
        .expect("queue body");
    task_ids_in(&body)
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /a2a/queue?min_lease_age_ms drops the leased task above its age and keeps the queued task"]
async fn live_http_a2a_queue_min_lease_age_drops_young_lease_keeps_queued() {
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

    // Send one task and lease it: leasing stamps leased_at_ms and flips the
    // entry to in_flight, the only state the lease-age filter acts on.
    let leased = task_for(&peer, "leased http lease-age probe");
    let _: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&leased)
        .send()
        .await
        .expect("post leased task")
        .json()
        .await
        .expect("leased task body");
    let lease: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("lease task")
        .json()
        .await
        .expect("lease body");
    assert_eq!(
        lease.pointer("/task/id").and_then(Value::as_str),
        Some(leased.id.to_string().as_str()),
        "lease must return the only queued task id: {lease:?}",
    );

    // Second task stays queued (never leased), so it is exempt from the
    // lease-age filter regardless of threshold.
    let queued = task_for(&peer, "queued http lease-age probe");
    let _: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&queued)
        .send()
        .await
        .expect("post queued task")
        .json()
        .await
        .expect("queued task body");

    // Let the lease accrue age well past the small threshold below.
    sleep(Duration::from_millis(300)).await;

    let leased_id = leased.id.to_string();
    let queued_id = queued.id.to_string();

    // Small threshold: the leased task's age (>= ~300ms) clears 25ms, and the
    // queued task is exempt, so both are present.
    let small = queue_ids(&client, &base, 25).await;
    assert!(
        small.contains(&leased_id),
        "min_lease_age_ms=25 must keep the aged in-flight task: {small:?}",
    );
    assert!(
        small.contains(&queued_id),
        "min_lease_age_ms=25 must keep the queued task (non-in-flight is exempt): {small:?}",
    );

    // Far-future threshold: the leased task is younger than an hour so it drops
    // out, while the queued task — exempt — remains. The leased task flipping
    // present→absent across thresholds proves the param is parsed and applied.
    let huge = queue_ids(&client, &base, 3_600_000).await;
    assert!(
        !huge.contains(&leased_id),
        "min_lease_age_ms=3_600_000 must drop the young in-flight task: {huge:?}",
    );
    assert!(
        huge.contains(&queued_id),
        "min_lease_age_ms=3_600_000 must still keep the queued task: {huge:?}",
    );

    let _ = child.kill().await;
}
