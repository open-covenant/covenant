//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::FlushReceipts` over the raw IPC socket through its full gate —
//! ungranted rejection, an empty-store flush error, then one settled receipt
//! batched into a confirmed local merkle batch.
//!
//! The verb is covered today over the CLI (`live_cli_chain_flush_receipts_json.rs`)
//! and HTTP (`live_http_chain_flush_receipts.rs`), and `live_chain_delegated_denial.rs`
//! pins the non-operator denial, but the success path is never driven over the
//! raw Unix socket all three are built on. This pins that wire contract: the
//! operator clears the identity gate, the `chain.flush` capability gate
//! (`flush_receipts`, covenantd/src/lib.rs:4986), and the
//! `Response::ReceiptBatchFlushed { batch, receipts_updated }` variant
//! (covenant-ipc/src/lib.rs:789) round-trips back through `write_frame`/`read_frame`.
//!
//! Receipts are seeded through the public API: one `SubmitIntent` persists a
//! memory record and settles credits for it, producing exactly one local
//! receipt (`tx_sig == None`, no on-chain RPC). Flushing it yields a one-row
//! batch carrying a local merkle root but no on-chain signature or slot. A
//! frozen capability gate would reject with `chain.flush` instead of the
//! empty-store no-op; a dropped receipt would leave the final batch empty.
//!
//! Hermetic — no external services, network, or on-chain settlement. The batch
//! is local and deterministic. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_flush_receipts -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::FlushReceipts through its operator+capability gate over the socket"]
async fn live_ipc_flush_receipts_gates_then_flushes_one_local_batch() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Ungranted baseline: the operator clears the identity gate, but flushing
    // still requires the chain.flush capability, so the daemon must reject the
    // flush before any grant.
    match req(&mut stream, Request::FlushReceipts { limit: 10 }).await {
        Response::Error { message } => assert!(
            message.contains("chain.flush"),
            "ungranted flush must name the missing capability: {message:?}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Seed capabilities through the public API: memory.write lets the intent
    // persist (and settle) a memory record; chain.flush opens the flush.
    for action in ["memory.write", "chain.flush"] {
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

    // Granted but nothing settled yet: flushing an empty store is a no-op error,
    // not an empty batch. The message is distinct from the capability rejection,
    // so reaching it also proves the chain.flush gate opened.
    match req(&mut stream, Request::FlushReceipts { limit: 10 }).await {
        Response::Error { message } => {
            assert!(
                message.contains("no unsettled receipts"),
                "an empty flush must report nothing to settle, not a capability error: {message:?}"
            );
            assert!(
                !message.contains("chain.flush"),
                "the operator holds chain.flush, so the empty flush must clear the gate: {message:?}"
            );
        }
        other => panic!("expected Response::Error for an empty flush after grant, got {other:?}"),
    }

    // Settle one receipt: an intent persists a memory record and burns credits
    // for it. tx_sig stays None — settlement is local, no on-chain RPC.
    match req(
        &mut stream,
        Request::SubmitIntent {
            text: "socket flush probe".into(),
            prefer_stream: None,
        },
    )
    .await
    {
        Response::IntentResult { .. } => {}
        other => panic!("expected Response::IntentResult, got {other:?}"),
    }

    // The settled receipt now flushes into a one-row batch carrying local merkle
    // evidence but no on-chain signature or slot.
    match req(&mut stream, Request::FlushReceipts { limit: 10 }).await {
        Response::ReceiptBatchFlushed {
            batch,
            receipts_updated,
        } => {
            assert_eq!(
                receipts_updated, 1,
                "the one settled receipt must be re-correlated to the batch"
            );
            assert_eq!(
                batch.receipt_count, 1,
                "exactly one receipt settled, so the batch counts one: {batch:?}"
            );
            assert!(
                !batch.batch_id.is_empty(),
                "a flushed batch must carry an id: {batch:?}"
            );
            assert!(
                !batch.merkle_root.is_empty(),
                "a flushed batch must carry a merkle root: {batch:?}"
            );
            assert!(
                batch.tx_sig.is_none(),
                "a local flush carries no on-chain signature: {batch:?}"
            );
            assert!(
                batch.slot.is_none(),
                "a local flush carries no on-chain slot: {batch:?}"
            );
        }
        other => panic!("expected Response::ReceiptBatchFlushed with one row, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
