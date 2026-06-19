//! Live HTTP delegated denial coverage for `POST /a2a/tasks` (`send_a2a_task`).
//!
//! The handler forwards `Request::SendA2ATask { task }` to the shared
//! `Server::respond` gate keyed on the bearer-resolved peer. The dispatch
//! applies two denial gates in order: an anti-spoof check
//! (`task.sender == peer`) and then the `a2a.send.<recipient>` capability
//! gate. The IPC path is pinned by `live_a2a_send_delegated_denial.rs`, and
//! the capability denial is incidentally exercised over the gateway by
//! `live_http_a2a_enqueue_envelope.rs` — but that test authenticates as the
//! operator (a loopback self-send) and never POSTs a forged sender, so two
//! things were unproven over the HTTP boundary: that a real non-operator
//! delegate is gated as itself, and that the body-supplied `sender` is
//! validated against the bearer-resolved peer at all (the anti-spoof gate has
//! no behavioral coverage at any boundary).
//!
//! Using the HTTP gateway exclusively, a Bearer-authed non-operator delegate
//! drives both denial paths:
//!
//! 1. Posting a task whose `sender` is the operator (not the delegate) is
//!    rejected by the anti-spoof gate — `kind: "error"` naming "does not match
//!    authenticated peer". A handler that compared the forged body sender
//!    against itself, or forwarded the body sender as the acting identity,
//!    would let a delegate forge a task as another agent; this assertion pins
//!    the gate to the bearer-resolved peer.
//! 2. Posting a task where the delegate is the sender (spoof check passes) and
//!    the operator is the recipient, without an `a2a.send.<operator>` grant, is
//!    rejected by the capability gate — `kind: "error"` naming `a2a.send`. This
//!    is the same capability the IPC denial test pins, so a wrong-gate or
//!    wrong-route regression that denies for the wrong reason is still caught.
//!
//! Phase 3 confirms neither rejected send enqueued (the operator drains an
//! empty mailbox) and that the delegate session is not bricked (`/health` still
//! succeeds), so the denials are scoped to the privileged action rather than
//! the connection.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_send_delegated_denial -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::Value;
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

fn read_operator_pubkey(home: &Path) -> [u8; 32] {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity");
    id.pubkey_bytes()
}

fn bearer_client(token_b58: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token_b58}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP POST /a2a/tasks rejects a delegate's forged sender (anti-spoof gate) and an ungranted cross-peer send (a2a.send capability gate) before enqueue"]
async fn live_http_a2a_send_rejects_delegate_spoof_and_missing_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([113u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [114u8; 32];
    let delegate_display = "delegate-http-a2a-sender@local";
    let delegate = AgentId::new(delegate_display, delegate_pubkey);
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: delegate.clone(),
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

    let operator_token = read_operator_token(home.path()).await;
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ"
    );
    let operator = AgentId::new("user@local", read_operator_pubkey(home.path()));
    assert_ne!(
        operator.pubkey, delegate.pubkey,
        "operator and delegate pubkeys must differ"
    );

    let delegate_client = bearer_client(&delegate_token_b58);

    // ── Phase 1: the delegate POSTs a task forging the operator as the
    //     sender. `task.sender` (operator) != the bearer-resolved peer
    //     (delegate), so the anti-spoof gate fires before the capability
    //     gate, returning an error naming the sender/peer mismatch. This is
    //     the first behavioral coverage of the spoof gate at any boundary;
    //     it proves the body-supplied sender is checked against the
    //     authenticated bearer rather than trusted or compared to itself.
    let spoofed = A2ATask {
        id: Uuid::new_v4(),
        sender: operator.clone(),
        recipient: operator.clone(),
        intent_text: "http delegated a2a sender-spoof probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    let spoof_json: Value = delegate_client
        .post(format!("{base}/a2a/tasks"))
        .json(&spoofed)
        .send()
        .await
        .expect("send forged-sender /a2a/tasks request")
        .json()
        .await
        .expect("forged-sender response body");
    assert_eq!(
        spoof_json["kind"], "error",
        "a delegate forging another sender must be rejected; got {spoof_json:?}",
    );
    let spoof_message = spoof_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        spoof_message.contains("does not match authenticated peer"),
        "HTTP /a2a/tasks forged-sender denial must name the sender/peer mismatch, not a capability; got {spoof_message:?}",
    );

    // ── Phase 2: the delegate POSTs a task it legitimately sends (sender ==
    //     delegate, so the spoof gate passes) to the operator, without an
    //     `a2a.send.<operator>` grant. The capability gate rejects it. The
    //     message names `a2a.send` — the same capability the IPC denial test
    //     pins — so a wrong-gate regression that denies for another reason is
    //     caught.
    let ungranted = A2ATask {
        id: Uuid::new_v4(),
        sender: delegate.clone(),
        recipient: operator.clone(),
        intent_text: "http delegated a2a.send denial probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    let denied_json: Value = delegate_client
        .post(format!("{base}/a2a/tasks"))
        .json(&ungranted)
        .send()
        .await
        .expect("send ungranted /a2a/tasks request")
        .json()
        .await
        .expect("ungranted response body");
    assert_eq!(
        denied_json["kind"], "error",
        "a delegate without a2a.send.<operator> must be rejected by the capability gate; got {denied_json:?}",
    );
    let denied_message = denied_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        denied_message.contains("a2a.send"),
        "HTTP /a2a/tasks capability denial must name the missing a2a.send capability; got {denied_message:?}",
    );

    // ── Phase 3: neither rejected send enqueued. The operator drains its own
    //     mailbox and finds it empty — if either gate had failed open, the
    //     rejected task would surface here. The operator reads its own queue
    //     without a recv grant (loopback admission is a no-op).
    let operator_client = bearer_client(&operator_token);
    let drained: Value = operator_client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("operator drains mailbox")
        .json()
        .await
        .expect("drain body");
    assert_eq!(
        drained["kind"], "a2_a_task_opt",
        "operator mailbox read must be an a2a_task_opt envelope; got {drained:?}",
    );
    assert!(
        drained["task"].is_null(),
        "neither the spoofed nor the ungranted send may enqueue; operator mailbox must be empty: {drained:?}",
    );

    // The delegate session is not bricked by the denials — `/health` still
    // reports healthy, so the rejections are scoped to the privileged action,
    // not the authenticated connection.
    let health = delegate_client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("delegate /health after denials");
    assert!(
        health.status().is_success(),
        "delegate must remain authenticated after the denials; got {}",
        health.status(),
    );

    let _ = child.kill().await;
}
