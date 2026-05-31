//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RevokeCapability` over the raw IPC socket after a grant —
//! confirming the active row, revoking it, the idempotent second revoke, and
//! the drained ledger.
//!
//! The verb is covered today over the CLI (`live_cli_capabilities_revoke.rs`,
//! `live_cli_capabilities_revoke_json.rs`) but never over the raw Unix socket
//! both are built on. This pins that wire contract: `Response::CapabilityRevoked
//! { signature_b58, removed }` (covenant-ipc/src/lib.rs:835) and the
//! already-revoked branch that reports `removed = false` on a repeat
//! (covenantd/src/lib.rs:6498).
//!
//! Seeded over the socket via `GrantCapability`; assertions key off the granted
//! `signature_b58` only, so no identity derivation is needed (the operator
//! stream's peer is the granting subject). Hermetic — no external services.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_revoke_capability -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::RevokeCapability over the socket after a grant"]
async fn live_ipc_revoke_capability_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Seed one capability and capture its signature.
    let signature_b58 = match req(
        &mut stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { signature_b58, .. } => signature_b58,
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    };

    // Baseline: the grant is live in the ledger.
    match req(&mut stream, Request::RecentCapabilities { limit: 100 }).await {
        Response::Capabilities { capabilities } => {
            assert_eq!(
                capabilities.len(),
                1,
                "the seeded grant must surface one row: {capabilities:?}"
            );
            assert_eq!(
                bs58::encode(capabilities[0].signature).into_string(),
                signature_b58,
                "the listed capability must be the one granted"
            );
        }
        other => panic!("expected Response::Capabilities, got {other:?}"),
    }

    // First revoke removes the operator's own capability.
    match req(
        &mut stream,
        Request::RevokeCapability {
            signature_b58: signature_b58.clone(),
        },
    )
    .await
    {
        Response::CapabilityRevoked {
            signature_b58: echoed,
            removed,
        } => {
            assert_eq!(
                echoed, signature_b58,
                "the response must echo the revoked signature"
            );
            assert!(
                removed,
                "the operator's own capability must be removed on first revoke"
            );
        }
        other => panic!("expected Response::CapabilityRevoked, got {other:?}"),
    }

    // The idempotent second revoke finds nothing to remove.
    match req(
        &mut stream,
        Request::RevokeCapability {
            signature_b58: signature_b58.clone(),
        },
    )
    .await
    {
        Response::CapabilityRevoked {
            signature_b58: echoed,
            removed,
        } => {
            assert_eq!(echoed, signature_b58);
            assert!(
                !removed,
                "a second revoke of the same signature must report removed=false"
            );
        }
        other => panic!("expected Response::CapabilityRevoked, got {other:?}"),
    }

    // The revoked capability no longer lists as active.
    match req(&mut stream, Request::RecentCapabilities { limit: 100 }).await {
        Response::Capabilities { capabilities } => assert!(
            capabilities
                .iter()
                .all(|c| bs58::encode(c.signature).into_string() != signature_b58),
            "the revoked capability must not remain active: {capabilities:?}"
        ),
        other => panic!("expected Response::Capabilities, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
