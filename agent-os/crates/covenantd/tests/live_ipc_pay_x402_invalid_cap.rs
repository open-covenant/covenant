//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::PayX402` over the raw IPC socket, pinning the fail-closed
//! input-validation guard on the daemon's sole outbound-spend trigger.
//!
//! The handler gates in strict order — the `x402.outbound.pay` capability,
//! then a configured + enabled x402 dispatch, then a valid HTTP method, and
//! only then `per_call_cap.parse::<u128>()` (covenantd/src/lib.rs:5153). A
//! malformed cap returns `Response::Error { "invalid per_call_cap ..." }`
//! *before* the subprocess signer is constructed or any network call is made.
//! Every other `Request` variant is exercised over the socket; PayX402's only
//! coverage is in-crate unit tests (capability/config branches) and a borsh
//! round-trip, none of which pin the cap-parse contract over the wire.
//!
//! Mirrors the two-step gate proof in `live_ipc_flush_receipts.rs`: an
//! ungranted send must be rejected by the capability gate first, and only
//! after the grant does the cap-parse become the rejecting gate.
//!
//! Hermetic — x402 dispatch is enabled from env alone (`COVENANT_X402_ENABLED`
//! + a `/bin/true` signer binary so `x402_dispatch_config_from_env` resolves
//! `Some`); the malformed-cap path short-circuits before the signer is even
//! constructed, so no real signer, Solana RPC, or x402 endpoint is touched.
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
#[ignore = "live: spawns covenantd with x402 enabled + drives Request::PayX402 over the socket, pinning the capability gate then the per_call_cap u128 parse rejection"]
async fn live_ipc_pay_x402_rejects_malformed_per_call_cap() {
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

    // Step 3 — capability, config, and method gates now all clear, so the
    // per_call_cap parse is the rejecting gate. It must fail closed (no signer
    // spawn, no network) with the cap-specific message echoing the bad value.
    match req(&mut stream, malformed_pay("not_a_number")).await {
        Response::Error { message } => {
            assert!(
                message.contains("invalid per_call_cap (must be decimal u128)"),
                "a malformed cap must be rejected by the u128 parse gate: {message:?}"
            );
            assert!(
                message.contains("not_a_number"),
                "the error must echo the offending cap value: {message:?}"
            );
        }
        other => panic!("expected Response::Error for a malformed cap, got {other:?}"),
    }

    // A value past u128::MAX proves the gate is a real numeric parse, not an
    // is-empty / is-ascii-digit check that a 40-digit string would slip past.
    let overflow = "9".repeat(40);
    match req(&mut stream, malformed_pay(&overflow)).await {
        Response::Error { message } => assert!(
            message.contains("invalid per_call_cap (must be decimal u128)"),
            "a cap past u128::MAX must fail the same parse gate: {message:?}"
        ),
        other => panic!("expected Response::Error for an overflow cap, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
