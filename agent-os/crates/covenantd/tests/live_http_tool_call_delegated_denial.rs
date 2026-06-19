//! Live HTTP delegated denial coverage for `POST /tools/call` (`call_tool`).
//!
//! The capability matrix marks `tool.call.<name>` as delegated-denial-only:
//! tools can touch the filesystem, network, and subprocesses, so a delegated
//! allowance for arbitrary tool execution requires a human release review. The
//! IPC path is pinned by `live_tool_call_delegated_denial.rs`; the operator
//! denial is exercised over the gateway by `live_http_tools_call.rs`, but two
//! things were unproven over the HTTP boundary for a real non-operator
//! delegate: that the `tool.call.<name>` gate rejects it as itself, and — the
//! security-relevant part — that the capability gate fires *before* the tool
//! registry lookup, so a delegate cannot enumerate the registry by probing the
//! error path.
//!
//! Using the HTTP gateway exclusively, a Bearer-authed non-operator delegate:
//!
//! 1. Calling the built-in `echo` tool without `tool.call.echo` is rejected —
//!    `kind: "error"` naming `tool.call.echo`.
//! 2. Calling a tool name that does not exist, still without a grant, is
//!    rejected with the same `requires capability` shape naming
//!    `tool.call.<that-name>` rather than a "tool not found" error. A handler
//!    that ran the registry lookup before the capability gate would leak which
//!    names exist; this assertion pins the ordering over HTTP.
//!
//! Phase 3 confirms the denials are scoped to the privileged action: the
//! delegate session stays healthy (`/health` still succeeds).
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_tool_call_delegated_denial -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

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

fn delegate_client(token_b58: &str) -> reqwest::Client {
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

async fn call_tool(client: &reqwest::Client, base: &str, name: &str) -> Value {
    client
        .post(format!("{base}/tools/call"))
        .json(&json!({ "name": name, "arguments": { "message": "delegated tool.call denial probe" } }))
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST /tools/call ({name}) failed: {e}"))
        .json()
        .await
        .unwrap_or_else(|e| panic!("/tools/call ({name}) body: {e}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP POST /tools/call rejects a non-operator delegate at the tool.call.<name> gate, including for an unknown tool name (capability gate fires before the registry lookup)"]
async fn live_http_tool_call_rejects_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([143u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [144u8; 32];
    let delegate_display = "delegate-http-tool-caller@local";
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

    // ── Phase 1: calling the built-in `echo` tool without `tool.call.echo`
    //     is rejected at the capability gate, which names the missing grant.
    let echo_json = call_tool(&client, &base, "echo").await;
    assert_eq!(
        echo_json["kind"], "error",
        "delegate without tool.call.echo must be rejected; got {echo_json:?}",
    );
    let echo_message = echo_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        echo_message.contains("tool.call.echo"),
        "HTTP /tools/call denial must name the missing tool.call.echo capability; got {echo_message:?}",
    );

    // ── Phase 2: a tool name that does not exist is rejected with the same
    //     `requires capability` shape (naming tool.call.<that-name>), not a
    //     "tool not found" error — proving the capability gate fires before
    //     the registry lookup over HTTP, so a delegate cannot enumerate the
    //     registry from the error path.
    let ghost_json = call_tool(&client, &base, "tool-that-does-not-exist").await;
    assert_eq!(
        ghost_json["kind"], "error",
        "delegate calling an unknown tool must be rejected by the capability gate; got {ghost_json:?}",
    );
    let ghost_message = ghost_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        ghost_message.contains("tool.call.tool-that-does-not-exist"),
        "the capability gate must fire before the registry lookup, naming tool.call.<name> rather than a tool-not-found error; got {ghost_message:?}",
    );

    // ── Phase 3: the delegate session is not bricked — `/health` still
    //     reports healthy, so the rejections are scoped to the privileged
    //     action rather than the authenticated connection.
    let health = client
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
