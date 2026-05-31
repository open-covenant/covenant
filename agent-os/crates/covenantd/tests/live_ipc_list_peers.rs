//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::ListPeers` over the raw IPC socket, asserting the daemon answers
//! `Response::PeerList` carrying exactly the operator bootstrap peer.
//!
//! The verb is covered today over the CLI (`live_cli_peers_list.rs`,
//! `live_cli_peers_list_status_filter.rs`) and HTTP
//! (`live_http_peers_list_status_filter.rs`) but never over the raw Unix socket
//! both are built on. This pins that wire contract — the `Response::PeerList`
//! variant (`peers`, `operator_pubkey_b58`, `truncated`;
//! covenant-ipc/src/lib.rs:930) and the redacted `PeerSummary`
//! (covenant-peer-auth/src/lib.rs:140) the operator reads to triage peers.
//!
//! A fresh daemon registers exactly one peer at boot — the operator itself, via
//! `bootstrap_operator_token` (covenantd/src/main.rs) — so the response is
//! deterministic: one live row whose pubkey matches `operator_pubkey_b58`, with
//! the token redacted to its 6-char prefix.
//!
//! Hermetic — the registry is local and offline. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_list_peers -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

#[tokio::test]
#[ignore = "live: spawns covenantd + queries Request::ListPeers over the socket"]
async fn live_ipc_list_peers_returns_operator_bootstrap_peer() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    match req(
        &mut stream,
        Request::ListPeers {
            limit: 10,
            pubkey_prefix: None,
            status_filter: None,
        },
    )
    .await
    {
        Response::PeerList {
            peers,
            operator_pubkey_b58,
            truncated,
        } => {
            assert_eq!(
                peers.len(),
                1,
                "a fresh daemon registers exactly the operator bootstrap peer: {peers:?}"
            );
            assert!(
                !operator_pubkey_b58.is_empty(),
                "the report must name the operator pubkey"
            );
            assert!(
                !truncated,
                "one row under a limit of 10 must not be truncated"
            );

            let peer = &peers[0];
            assert!(
                peer.revoked_at.is_none(),
                "the operator bootstrap peer must be live, not revoked: {peer:?}"
            );
            assert_eq!(
                bs58::encode(peer.agent_id.pubkey).into_string(),
                operator_pubkey_b58,
                "the lone row must be the operator's own peer"
            );
            assert_eq!(
                peer.token_prefix.chars().count(),
                6,
                "the wire must carry only the 6-char token prefix, never the full token: {:?}",
                peer.token_prefix
            );
        }
        other => panic!("expected Response::PeerList, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
