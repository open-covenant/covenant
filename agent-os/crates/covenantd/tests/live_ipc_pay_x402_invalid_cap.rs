//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::PayX402` over the raw IPC socket, pinning the fail-closed parked
//! boundary on the daemon's legacy outbound-spend trigger.
//!
//! The handler gates in strict order — the `x402.outbound.pay` capability,
//! then the unconditional parked boundary. A granted call returns the stable
//! parked reason before config parsing, signer construction, or network I/O.
//! Every other `Request` variant is exercised over the socket; PayX402's only
//! coverage is in-crate unit tests (capability/config branches) and a borsh
//! round-trip, none of which pin the cap-parse contract over the wire.
//!
//! Mirrors the two-step gate proof in `live_ipc_flush_receipts.rs`: an
//! ungranted send must be rejected by the capability gate first, and only
//! after the grant does the parked boundary become the rejecting gate.
//!
//! Hermetic — the old environment opt-in is set but ignored.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_pay_x402_invalid_cap -- --ignored live_`.

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

/// Spawn covenantd against `home`, clearing inherited x402 env so the dispatch
/// config is host-independent, then applying `env` overrides.
async fn spawn_daemon(home: &Path, env: &[(&str, &str)]) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(exe);
    cmd.env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .env_remove("COVENANT_X402_ENABLED")
        .env_remove("COVENANT_X402_SIGNER_BINARY")
        .env_remove("COVENANT_X402_FUNDING_KEYPAIR")
        .env_remove("COVENANT_X402_RPC_URL");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let child = cmd
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

/// A PayX402 whose only invalid field is `per_call_cap`: a valid method and
/// well-formed strings everywhere else, so the cap-parse is the only gate that
/// can reject it once the capability + config gates pass.
fn malformed_pay(per_call_cap: &str) -> Request {
    Request::PayX402 {
        provider: "x".into(),
        endpoint: "https://example.test/e".into(),
        method: "POST".into(),
        body: None,
        network: "solana:mainnet".into(),
        asset: "usdc-sol".into(),
        per_call_cap: per_call_cap.into(),
        credits: 8,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd with the legacy x402 env + proves IPC PayX402 remains parked"]
async fn live_ipc_pay_x402_rejects_ungranted_then_parked() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(
        home.path(),
        &[
            ("COVENANT_X402_ENABLED", "1"),
            ("COVENANT_X402_SIGNER_BINARY", "/bin/true"),
        ],
    )
    .await;
    let mut stream = authenticated_stream(home.path()).await;

    // Step 1 — ungranted baseline: the operator clears the identity gate, but
    // spending still requires x402.outbound.pay, so the daemon must reject on
    // the capability gate before it ever parses the cap.
    match req(&mut stream, malformed_pay("not_a_number")).await {
        Response::Error { message } => assert!(
            message.contains("x402.outbound.pay"),
            "ungranted PayX402 must name the missing capability, not parse the cap: {message:?}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Step 2 — grant the spend capability to the operator peer.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "x402.outbound.pay".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }

    // Step 3 — after the grant, even a malformed payload stops at the parked
    // boundary before the cap is parsed.
    match req(&mut stream, malformed_pay("not_a_number")).await {
        Response::Error { message } => {
            assert_eq!(message, covenantd::x402::LEGACY_OUTBOUND_PARKED)
        }
        other => panic!("expected parked Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
