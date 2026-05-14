//! Live HTTP delegated denial coverage for `/a2a/repair` and
//! `/a2a/compact`. Both verbs mutate the A2A mailbox: repair re-
//! places or force-errors a leased task, compact drops resolved
//! task event rows. The daemon-side dispatch gates each on
//! `a2a.repair.<action>` and `a2a.compact` respectively. The IPC
//! variants are pinned by existing live tests; this test extends the
//! same pin to the HTTP gateway mutation surface.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_recovery_delegated_denial -- --ignored live_`.

use covenant_a2a::{A2ADuplicateRisk, A2ARepairCommand, A2ARepairRequest};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
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

fn delegate_client(token_b58: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token_b58}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /a2a/repair and /a2a/compact"]
async fn live_http_a2a_repair_and_compact_reject_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([71u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [72u8; 32];
    let delegate_display = "delegate-http-a2a-mutator@local";
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new(delegate_display, delegate_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed delegate");
    }

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

    let client = delegate_client(&delegate_token_b58);

    // ── Phase 1: POST /a2a/repair with a Requeue command targeting
    //     a random task_id lands at the capability gate before the
    //     task-visibility check, so the rejection names
    //     `a2a.repair.requeue` even though the task does not exist.
    let repair_body = A2ARepairRequest {
        task_id: Uuid::new_v4(),
        command: A2ARepairCommand::Requeue {
            lease_id: None,
            duplicate_risk: A2ADuplicateRisk::Idempotent,
        },
        reason: "http delegated a2a.repair denial probe".into(),
    };
    let repair_response = client
        .post(format!("{base}/a2a/repair"))
        .json(&repair_body)
        .send()
        .await
        .expect("send /a2a/repair request");
    let repair_json: Value = repair_response
        .json()
        .await
        .expect("/a2a/repair response body");
    assert_eq!(
        repair_json["kind"], "error",
        "delegate without a2a.repair.requeue must be rejected; got {repair_json:?}",
    );
    let repair_message = repair_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        repair_message.contains("a2a.repair.requeue"),
        "HTTP /a2a/repair denial must name the missing capability; got {repair_message:?}",
    );

    // ── Phase 2: POST /a2a/compact with no body — `CompactA2A` has
    //     no payload. The capability gate fires before any compaction
    //     work runs.
    let compact_response = client
        .post(format!("{base}/a2a/compact"))
        .send()
        .await
        .expect("send /a2a/compact request");
    let compact_json: Value = compact_response
        .json()
        .await
        .expect("/a2a/compact response body");
    assert_eq!(
        compact_json["kind"], "error",
        "delegate without a2a.compact must be rejected; got {compact_json:?}",
    );
    let compact_message = compact_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        compact_message.contains("a2a.compact"),
        "HTTP /a2a/compact denial must name the missing capability; got {compact_message:?}",
    );

    // ── Phase 3: the delegate's session survives both denials.
    let health = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send /health request");
    assert!(
        health.status().is_success(),
        "delegate must remain authenticated after capability denial; got {}",
        health.status(),
    );

    let _ = child.kill().await;
}
