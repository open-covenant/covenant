//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::PurgePeers` over the raw IPC socket, asserting the daemon answers
//! `Response::PeersPurged { purged }` — the revoked-peer retention sweep an
//! operator runs to drop tombstoned rows from the JSONL registry.
//!
//! The verb is covered today over the CLI (`live_cli_peers_purge_json.rs`) and
//! HTTP (`live_http_peers_purge.rs`), and the delegated-denial path is pinned
//! over the socket (`live_peers_list_purge_delegated_denial.rs:164`), but the
//! `Response::PeersPurged` success frame those analogs are built on is never
//! exercised over the raw Unix socket. This pins that wire contract
//! (covenant-ipc/src/lib.rs:894) at the boundary the in-process unit test
//! cannot reach.
//!
//! Hermetic — a local JSONL registry seeded with one revoked guest, no network,
//! Solana, or model. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_peers_purge -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
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
#[ignore = "live: spawns covenantd + drives Request::PurgePeers over the socket"]
async fn live_ipc_peers_purge_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    // Seed one revoked guest before boot so the daemon replays a tombstone the
    // sweep can drop. `revoke` stamps `revoked_at` with the current epoch_ms(),
    // far above the `before_ms: 1` baseline below.
    let guest_token = PeerToken::from_bytes([31u8; 32]);
    {
        let registry = JsonlPeerRegistry::open(home.path().join("peers").join("registry.jsonl"))
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: guest_token,
                agent_id: AgentId::new("guest-purge-ipc@local", [42u8; 32]),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed guest peer");
        assert!(
            registry.revoke(&guest_token).await.expect("revoke guest"),
            "seeded guest should revoke"
        );
    }

    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    match req(
        &mut stream,
        Request::GrantCapability {
            action: "peers.purge".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant failed: {other:?}"),
    }

    // Baseline: the JSONL filter is strict `revoked_at < before_ms`, and the
    // guest was revoked at epoch_ms() (>> 1), so a cutoff of 1 drops nothing.
    match req(&mut stream, Request::PurgePeers { before_ms: 1 }).await {
        Response::PeersPurged { purged } => assert_eq!(
            purged, 0,
            "a before_ms below the seeded revoked_at must drop nothing"
        ),
        other => panic!("expected PeersPurged, got {other:?}"),
    }

    // A far-future cutoff sweeps the lone revoked guest tombstone (and its
    // matching Registered row); the operator's own live row has no Revoked
    // event, so it is never at risk.
    match req(
        &mut stream,
        Request::PurgePeers {
            before_ms: 9_999_999_999_999,
        },
    )
    .await
    {
        Response::PeersPurged { purged } => {
            assert_eq!(
                purged, 1,
                "the lone revoked guest tombstone must be dropped"
            )
        }
        other => panic!("expected PeersPurged, got {other:?}"),
    }

    // Non-draining: a second purge finds nothing left, proving the first sweep
    // was the only destructive match.
    match req(
        &mut stream,
        Request::PurgePeers {
            before_ms: 9_999_999_999_999,
        },
    )
    .await
    {
        Response::PeersPurged { purged } => {
            assert_eq!(purged, 0, "a second purge finds nothing left")
        }
        other => panic!("expected PeersPurged, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
