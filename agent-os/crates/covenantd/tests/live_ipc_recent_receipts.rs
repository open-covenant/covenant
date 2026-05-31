//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentReceipts` over the raw IPC socket through its full gate —
//! ungranted rejection, granted-but-empty, then a single settled row.
//!
//! The verb is covered today over the CLI (`live_cli_receipts_recent_json.rs`)
//! and HTTP (`live_http_receipts_recent_since_ms.rs`) but never over the raw
//! Unix socket both are built on. This pins that wire contract: the
//! `chain.receipts` capability gate (`recent_receipts`, covenantd/src/lib.rs:4898),
//! the `Response::Receipts { receipts }` variant (covenant-ipc/src/lib.rs:783),
//! and the payer attribution the handler filters on — `SettlementReceipt.payer`
//! is the authenticated peer, set in `dispatch_intent` (covenant-types/src/lib.rs:392).
//!
//! Receipts are seeded through the public API: one `SubmitIntent` persists a
//! memory record and settles credits for it, producing exactly one local
//! receipt (`tx_sig == None`, no on-chain RPC). A frozen gate would answer
//! `Response::Error` on the granted read; a dropped receipt would leave the
//! final list empty.
//!
//! Hermetic — no external services, network, or on-chain settlement. The
//! receipt is local and deterministic. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_recent_receipts -- --ignored live_`.

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

/// Authenticates as the daemon's operator and returns the live stream plus the
/// resolved operator display. `Response::Authenticated` carries only `display`,
/// which is also `SettlementReceipt.payer.display` for receipts this operator
/// settles, so it is the observable handle on payer attribution at the socket.
async fn authenticated_stream(home: &Path) -> (UnixStream, String) {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    let operator = match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { display } => display,
        other => panic!("authenticate failed: {other:?}"),
    };
    (stream, operator)
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::RecentReceipts through its capability gate over the socket"]
async fn live_ipc_recent_receipts_gates_then_lists_settled_row() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let (mut stream, operator) = authenticated_stream(home.path()).await;

    // Ungranted baseline: receipt reads require the chain.receipts capability,
    // so the daemon must reject the read before any grant.
    match req(
        &mut stream,
        Request::RecentReceipts {
            limit: 10,
            since_ms: None,
        },
    )
    .await
    {
        Response::Error { message } => assert!(
            message.contains("chain.receipts"),
            "ungranted receipt read must name the missing capability: {message:?}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Seed capabilities through the public API: memory.write lets the intent
    // persist (and settle) a memory record; chain.receipts opens the read.
    for action in ["memory.write", "chain.receipts"] {
        match req(
            &mut stream,
            Request::GrantCapability {
                action: action.into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("expected Response::CapabilityGranted for {action}, got {other:?}"),
        }
    }

    // Granted but nothing settled yet: the list is empty.
    match req(
        &mut stream,
        Request::RecentReceipts {
            limit: 10,
            since_ms: None,
        },
    )
    .await
    {
        Response::Receipts { receipts } => assert!(
            receipts.is_empty(),
            "no intent has settled yet, so the receipt list must be empty: {receipts:?}"
        ),
        other => panic!("expected empty Response::Receipts after grant, got {other:?}"),
    }

    // Settle one receipt: an intent persists a memory record and burns credits
    // for it. tx_sig stays None — settlement is local, no on-chain RPC.
    match req(
        &mut stream,
        Request::SubmitIntent {
            text: "socket receipt probe".into(),
            prefer_stream: None,
        },
    )
    .await
    {
        Response::IntentResult { .. } => {}
        other => panic!("expected Response::IntentResult, got {other:?}"),
    }

    // The settled row now surfaces, attributed to the operator that paid for it.
    match req(
        &mut stream,
        Request::RecentReceipts {
            limit: 10,
            since_ms: None,
        },
    )
    .await
    {
        Response::Receipts { receipts } => {
            assert_eq!(
                receipts.len(),
                1,
                "exactly one intent settled, so one receipt must surface: {receipts:?}"
            );
            let receipt = &receipts[0];
            assert_eq!(
                receipt.payer.display, operator,
                "the receipt must be attributed to the authenticated operator: {receipt:?}"
            );
            assert!(
                receipt.credits_consumed > 0,
                "settling a memory write must consume credits: {receipt:?}"
            );
            assert!(
                receipt.memory_record_id.is_some(),
                "the intent persisted a memory record, so the receipt must reference it: {receipt:?}"
            );
            assert!(
                receipt.tx_sig.is_none(),
                "a local settlement carries no on-chain signature: {receipt:?}"
            );
        }
        other => panic!("expected Response::Receipts with one row, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
