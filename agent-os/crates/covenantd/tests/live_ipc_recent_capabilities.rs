//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentCapabilities` over the raw IPC socket, asserting the daemon
//! answers `Response::Capabilities` and that a freshly granted capability shows
//! up on the operator's own feed.
//!
//! The verb is covered today over the CLI (`live_cli_capabilities_recent.rs`,
//! `live_cli_capabilities_recent_json.rs`) and HTTP
//! (`live_http_capabilities_recent.rs`) but never over the raw Unix socket they
//! are built on. This pins that wire contract — the `Response::Capabilities`
//! variant (covenant-ipc/src/lib.rs) and the full `SignedCapability` shape
//! (`capability` + base58 `signature`; covenant-permissions/src/lib.rs).
//!
//! A fresh daemon lists nothing; granting one capability through the public API
//! must surface exactly that signed row, matched by its `signature_b58` and
//! action — proving the dispatch route and the keep-own side of the
//! subject/granted_by filter. The view is non-draining.
//!
//! Hermetic — capabilities are signed and stored locally. `#[ignore]`'d. Run
//! with `cargo test -p covenantd --test live_ipc_recent_capabilities -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::RecentCapabilities over the socket as a grant is added"]
async fn live_ipc_recent_capabilities_lists_granted_capability() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    match req(&mut stream, Request::RecentCapabilities { limit: 100 }).await {
        Response::Capabilities { capabilities } => assert!(
            capabilities.is_empty(),
            "a fresh daemon must list no capabilities: {capabilities:?}"
        ),
        other => panic!("expected Response::Capabilities, got {other:?}"),
    }

    let signature_b58 = match req(
        &mut stream,
        Request::GrantCapability {
            action: "memory.read".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { signature_b58, .. } => signature_b58,
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    };

    match req(&mut stream, Request::RecentCapabilities { limit: 100 }).await {
        Response::Capabilities { capabilities } => {
            assert_eq!(
                capabilities.len(),
                1,
                "the operator's own grant must surface exactly one row: {capabilities:?}"
            );
            assert_eq!(
                bs58::encode(capabilities[0].signature).into_string(),
                signature_b58,
                "the listed row must be the capability just granted"
            );
            assert_eq!(
                capabilities[0].capability.action, "memory.read",
                "the listed capability must carry the granted action"
            );
        }
        other => panic!("expected Response::Capabilities, got {other:?}"),
    }

    // A read must not drain the store.
    match req(&mut stream, Request::RecentCapabilities { limit: 100 }).await {
        Response::Capabilities { capabilities } => assert_eq!(
            capabilities.len(),
            1,
            "listing capabilities must not consume them: {capabilities:?}"
        ),
        other => panic!("expected Response::Capabilities, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
