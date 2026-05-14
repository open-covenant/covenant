//! Live HTTP delegated denial coverage for `/memory/repair` and
//! `/memory/compact`. The capability matrix marks both
//! `memory.repair.*` and `memory.compact.*` as delegated-denial-only:
//! a non-operator delegate without the named grant must hit the
//! capability gate before any retention state mutates. The IPC path
//! is already pinned by `live_memory_repair_compact_delegated_denial`;
//! this test extends the same pin to the HTTP gateway mutation surface.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_memory_repair_delegated_denial -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::{
    AgentId, MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRepairCommand,
    MemoryRepairMode, MemoryRepairRequest,
};
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
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /memory/repair and /memory/compact"]
async fn live_http_memory_repair_and_compact_reject_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([91u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [92u8; 32];
    let delegate_display = "delegate-http-memory-mutator@local";
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

    // ── Phase 1: POST /memory/repair with a DryRun DeleteRecord
    //     targeting a random Uuid lands at the capability gate before
    //     the record-visibility check, so the rejection names
    //     `memory.repair.dry_run` even though the record does not
    //     exist.
    let repair_body = MemoryRepairRequest {
        mode: MemoryRepairMode::DryRun,
        command: MemoryRepairCommand::DeleteRecord { id: Uuid::new_v4() },
        reason: "http delegated memory.repair denial probe".into(),
    };
    let repair_response = client
        .post(format!("{base}/memory/repair"))
        .json(&repair_body)
        .send()
        .await
        .expect("send /memory/repair request");
    let repair_json: Value = repair_response
        .json()
        .await
        .expect("/memory/repair response body");
    assert_eq!(
        repair_json["kind"], "error",
        "delegate without memory.repair.dry_run must be rejected; got {repair_json:?}",
    );
    let repair_message = repair_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        repair_message.contains("memory.repair.dry_run"),
        "HTTP /memory/repair denial message must name the missing capability; got {repair_message:?}",
    );

    // ── Phase 2: POST /memory/compact with an empty dry-run policy
    //     hits the same capability gate before any retention state
    //     can change.
    let compact_body = MemoryCompactionRequest {
        mode: MemoryRepairMode::DryRun,
        policy: MemoryCompactionPolicy::default(),
        reason: "http delegated memory.compact denial probe".into(),
    };
    let compact_response = client
        .post(format!("{base}/memory/compact"))
        .json(&compact_body)
        .send()
        .await
        .expect("send /memory/compact request");
    let compact_json: Value = compact_response
        .json()
        .await
        .expect("/memory/compact response body");
    assert_eq!(
        compact_json["kind"], "error",
        "delegate without memory.compact.dry_run must be rejected; got {compact_json:?}",
    );
    let compact_message = compact_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        compact_message.contains("memory.compact.dry_run"),
        "HTTP /memory/compact denial message must name the missing capability; got {compact_message:?}",
    );

    // ── Phase 3: the delegate's session is not bricked by the
    //     denials — `/health` keeps reporting healthy (capability
    //     scope, not auth, was the gate).
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
