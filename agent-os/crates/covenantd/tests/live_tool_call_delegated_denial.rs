//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, has the delegate
//! authenticate, and verifies that `Request::CallTool` is rejected
//! by the capability gate without any prior `tool.call.<name>`
//! grant.
//!
//! Tools can have side effects on local filesystem, network, and
//! subprocesses (gVisor or otherwise), so a delegated allowance for
//! arbitrary tool execution requires a human release review. Per
//! the matrix policy, automation may only add denial-only coverage
//! here. The capability check fires before any tool registry
//! lookup, so the rejection lands regardless of whether the named
//! tool exists in the daemon's registry — that ordering is exactly
//! what the test is pinning down.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_tool_call_delegated_denial -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
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

async fn read_operator_token(home: &std::path::Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    let request = Request::Authenticate {
        token_b58: token_b58.to_string(),
    };
    match req(&mut stream, request).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies delegated denial for tool.call.<name>"]
async fn live_covenantd_tool_call_rejects_non_operator_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([141u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [142u8; 32];
    let delegate_display = "delegate-tool-caller@local";
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

    let operator_token = read_operator_token(home.path()).await;
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ"
    );

    // ── Phase 1: a non-operator delegate calling the built-in
    //     `echo` tool without `tool.call.echo` is rejected at the
    //     capability gate. The dispatch returns Response::Error
    //     whose message names the required capability so a CLI
    //     caller knows the exact grant to request.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::CallTool {
                name: "echo".into(),
                arguments: json!({ "message": "delegated tool.call denial probe" }),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("tool.call.echo"),
                "missing-grant tool.call.echo must reject by capability gate, got {message:?}"
            ),
            other => {
                panic!("non-operator delegate must not call tools without a grant, got {other:?}")
            }
        }
    }

    // ── Phase 2: the capability gate fires before any tool
    //     registry lookup. A non-existent tool name produces the
    //     same `requires capability` rejection rather than a
    //     `tool not found` error, which closes a side channel
    //     where a delegate could enumerate the registry from the
    //     error path.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::CallTool {
                name: "tool-that-does-not-exist".into(),
                arguments: json!({}),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("tool.call.tool-that-does-not-exist"),
                "capability gate must fire before the registry lookup, got {message:?}"
            ),
            other => {
                panic!("non-operator delegate must not enumerate the tool registry, got {other:?}")
            }
        }
    }

    // ── Phase 3: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the rejections above are
    //     scoped to the privileged action, not the entire session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
