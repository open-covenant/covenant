//! Live HTTP delegated denial coverage for `/peers/revoke`,
//! `/peers/purge`, and `/peers/enroll`. All three verbs are operator
//! administration: a non-operator delegate must be refused before any
//! registry mutation runs. Revoke and purge sit behind the capability
//! gate (a delegate without the named grant is rejected); enrollment
//! is a pure operator-identity gate with no capability fallback, so a
//! Bearer delegate is refused outright and the registry must not gain
//! a row. The IPC revoke/purge variants are already pinned by
//! `live_peers_list_purge_delegated_denial.rs`; this test extends the
//! same pin to the HTTP gateway mutation surface.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_peers_lifecycle_delegated_denial -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

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

async fn read_operator_token(home: &std::path::Path) -> String {
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

fn displays_in(roster: &Value) -> Vec<String> {
    roster["peers"]
        .as_array()
        .expect("peers array")
        .iter()
        .filter_map(|peer| peer.pointer("/agent_id/display").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /peers/revoke and /peers/purge"]
async fn live_http_peers_revoke_and_purge_reject_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([81u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [82u8; 32];
    let delegate_display = "delegate-http-peers-mutator@local";
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

    // ── Phase 1: POST /peers/revoke with an arbitrary token prefix.
    //     The capability gate fires before the registry lookup, so
    //     the request is rejected without revealing whether the
    //     prefix matches any live peer entry.
    let revoke_response = client
        .post(format!("{base}/peers/revoke"))
        .json(&json!({ "token_prefix": "abcdef" }))
        .send()
        .await
        .expect("send /peers/revoke request");
    let revoke_json: Value = revoke_response
        .json()
        .await
        .expect("/peers/revoke response body");
    assert_eq!(
        revoke_json["kind"], "error",
        "delegate without peers.revoke must be rejected; got {revoke_json:?}",
    );
    let revoke_message = revoke_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        revoke_message.contains("peers.revoke") || revoke_message.contains("operator identity"),
        "HTTP /peers/revoke denial must name the missing capability or operator-identity gate; got {revoke_message:?}",
    );

    // ── Phase 2: POST /peers/purge with a non-zero before_ms.
    //     Same gate semantics — registry retention is operator-only
    //     and the delegate hits the capability layer first.
    let purge_response = client
        .post(format!("{base}/peers/purge"))
        .json(&json!({ "before_ms": 1u64 }))
        .send()
        .await
        .expect("send /peers/purge request");
    let purge_json: Value = purge_response
        .json()
        .await
        .expect("/peers/purge response body");
    assert_eq!(
        purge_json["kind"], "error",
        "delegate without peers.purge must be rejected; got {purge_json:?}",
    );
    let purge_message = purge_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        purge_message.contains("peers.purge") || purge_message.contains("operator identity"),
        "HTTP /peers/purge denial must name the missing capability or operator-identity gate; got {purge_message:?}",
    );

    // ── Phase 3: the delegate's session survives both denials —
    //     `/health` still succeeds so the gate is scope, not auth.
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

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial and operator allow for /peers/enroll"]
async fn live_http_peers_enroll_rejects_bearer_delegate_without_operator_identity() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([83u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [84u8; 32];
    let delegate_display = "delegate-http-peers-enroller@local";
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

    // A well-formed enrollment: the display already carries the
    // `name@host` shape and the action passes scope validation, so the
    // only thing between the delegate and a registry write is the
    // operator-identity gate (which `enroll_peer` checks before any
    // payload validation).
    let enroll_body = json!({
        "display": "intruder-partner@peer",
        "actions": ["tool.call.echo"],
    });

    // ── Phase 1: a Bearer delegate POSTs the enrollment and must be
    //     refused by the identity gate — the exact message, not a
    //     validation error, proves which branch fired.
    let delegate = delegate_client(&delegate_token_b58);
    let denied: Value = delegate
        .post(format!("{base}/peers/enroll"))
        .json(&enroll_body)
        .send()
        .await
        .expect("send delegated /peers/enroll request")
        .json()
        .await
        .expect("/peers/enroll denial body");
    assert_eq!(
        denied["kind"], "error",
        "delegate must not be able to enroll peers; got {denied:?}",
    );
    assert_eq!(
        denied["message"], "enrolling a peer requires the operator identity",
        "denial must come from the operator-identity gate, not payload validation; got {denied:?}",
    );

    // ── Phase 2: the daemon survives the denial — `/health` still
    //     answers, so the refusal did not wedge the gateway. (Auth
    //     isolation is proven by Phase 1 itself: only a request that
    //     cleared the Bearer middleware can reach the identity gate.)
    let health = delegate
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send /health request");
    assert!(
        health.status().is_success(),
        "daemon must stay healthy after the enroll denial; got {}",
        health.status(),
    );

    // ── Phase 3: no-mutation readback. The operator's roster holds
    //     exactly the operator row and the seeded delegate — the denied
    //     enrollment minted nothing.
    let operator_token_b58 = read_operator_token(home.path()).await;
    let operator = delegate_client(&operator_token_b58);
    let roster: Value = operator
        .get(format!("{base}/peers/list?limit=20"))
        .send()
        .await
        .expect("send operator /peers/list readback")
        .json()
        .await
        .expect("/peers/list readback body");
    let displays = displays_in(&roster);
    assert_eq!(
        displays.len(),
        2,
        "denied enrollment must not grow the registry beyond operator + seeded delegate; got {displays:?}",
    );
    assert!(
        displays.iter().any(|d| d == delegate_display),
        "readback must still show the seeded delegate; got {displays:?}",
    );
    assert!(
        !displays.iter().any(|d| d == "intruder-partner@peer"),
        "denied enrollment must not register the intruder; got {displays:?}",
    );

    // ── Phase 4: the identical payload clears the gate under the
    //     operator identity, so Phase 1's refusal was the identity
    //     gate and not a defect in the enrollment path.
    let enrolled: Value = operator
        .post(format!("{base}/peers/enroll"))
        .json(&enroll_body)
        .send()
        .await
        .expect("send operator /peers/enroll request")
        .json()
        .await
        .expect("/peers/enroll operator body");
    assert_eq!(
        enrolled["kind"], "peer_enrolled",
        "operator enrollment with the identical payload must succeed; got {enrolled:?}",
    );
    assert_eq!(
        enrolled["display"], "intruder-partner@peer",
        "enrollment must echo the validated display; got {enrolled:?}",
    );
    assert_eq!(
        enrolled["granted"],
        json!(["tool.call.echo"]),
        "enrollment must grant exactly the requested actions; got {enrolled:?}",
    );
    assert!(
        enrolled["token_b58"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "operator enrollment must mint a scoped bearer token; got {enrolled:?}",
    );

    let after: Value = operator
        .get(format!("{base}/peers/list?limit=20"))
        .send()
        .await
        .expect("send operator /peers/list post-enroll readback")
        .json()
        .await
        .expect("/peers/list post-enroll body");
    let after_displays = displays_in(&after);
    assert_eq!(
        after_displays.len(),
        3,
        "operator enrollment must add exactly one registry row; got {after_displays:?}",
    );
    assert!(
        after_displays.iter().any(|d| d == "intruder-partner@peer"),
        "post-enroll readback must show the newly enrolled peer; got {after_displays:?}",
    );

    let _ = child.kill().await;
}
