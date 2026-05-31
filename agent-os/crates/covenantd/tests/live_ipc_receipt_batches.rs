//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::ReceiptBatches` over the raw IPC socket through its capability gate
//! — ungranted rejection, granted-but-empty, then one flushed batch.
//!
//! The verb is covered today over the CLI (`live_cli_receipt_batches_json.rs`)
//! and HTTP (`live_http_chain_receipt_batches.rs`), but both go through a remap
//! layer; the only raw-socket coverage is the ungranted denial in
//! `live_chain_delegated_denial.rs`, so the granted success frame is unpinned.
//! This pins that wire contract: `Response::ReceiptBatches { batches }`
//! (covenant-ipc/src/lib.rs:793) and `ReceiptBatchSummary`
//! (covenant-ipc/src/lib.rs:53), reached via the `chain.batches` gate
//! (`receipt_batches`, covenantd/src/lib.rs:5224).
//!
//! Seeded entirely over the socket via the public API: an intent mints one
//! operator-owned local receipt, then `FlushReceipts` assigns it a batch_id
//! (the read skips unbatched receipts). Hermetic — the batch is local, with no
//! on-chain signature or slot. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_receipt_batches -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::ReceiptBatches through its capability gate over the socket"]
async fn live_ipc_receipt_batches_gates_then_lists_flushed_batch() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Ungranted baseline: batch reads require the chain.batches capability.
    match req(&mut stream, Request::ReceiptBatches { limit: 10 }).await {
        Response::Error { message } => assert!(
            message.contains("chain.batches"),
            "ungranted batch read must name the missing capability: {message:?}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Seed capabilities: memory.write lets the intent settle a receipt,
    // chain.flush lets it be batched, chain.batches opens the read.
    for action in ["memory.write", "chain.flush", "chain.batches"] {
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

    // Granted but nothing flushed yet: the batch list is empty.
    match req(&mut stream, Request::ReceiptBatches { limit: 10 }).await {
        Response::ReceiptBatches { batches } => assert!(
            batches.is_empty(),
            "no receipt has been flushed into a batch yet: {batches:?}"
        ),
        other => panic!("expected empty Response::ReceiptBatches after grant, got {other:?}"),
    }

    // Mint one local receipt, then flush it into a batch.
    match req(
        &mut stream,
        Request::SubmitIntent {
            text: "socket receipt batch probe".into(),
            prefer_stream: None,
        },
    )
    .await
    {
        Response::IntentResult { .. } => {}
        other => panic!("expected Response::IntentResult, got {other:?}"),
    }

    let flushed_id = match req(&mut stream, Request::FlushReceipts { limit: 10 }).await {
        Response::ReceiptBatchFlushed {
            batch,
            receipts_updated,
        } => {
            assert!(
                !batch.batch_id.is_empty(),
                "a flushed batch must carry an id: {batch:?}"
            );
            assert_eq!(
                receipts_updated, 1,
                "exactly one minted receipt is flushed into the batch: {receipts_updated}"
            );
            batch.batch_id
        }
        other => panic!("expected Response::ReceiptBatchFlushed, got {other:?}"),
    };

    // The flushed batch now lists, keyed by its id and aggregating the receipt.
    match req(&mut stream, Request::ReceiptBatches { limit: 10 }).await {
        Response::ReceiptBatches { batches } => {
            assert_eq!(
                batches.len(),
                1,
                "the one flushed batch must be the only summary: {batches:?}"
            );
            let b = &batches[0];
            assert_eq!(
                b.batch_id, flushed_id,
                "the listed batch must be the flushed batch: {b:?}"
            );
            assert_eq!(
                b.receipt_count, 1,
                "the batch aggregates the one minted receipt: {b:?}"
            );
            assert!(
                !b.merkle_root.is_empty(),
                "a batch must carry a merkle root: {b:?}"
            );
            assert!(
                b.tx_sig.is_none(),
                "a local batch has no on-chain signature: {b:?}"
            );
            assert!(
                b.slot.is_none(),
                "a local batch has no on-chain slot: {b:?}"
            );
        }
        other => panic!("expected Response::ReceiptBatches with one row, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
