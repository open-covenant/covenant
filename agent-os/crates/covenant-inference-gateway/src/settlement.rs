//! Folds served-inference receipts into the on-chain provenance root.
//!
//! Each successful turn produces a `receipt_hash`. With `--settlement-url` set, that
//! hash is handed to this folder, which forwards it to the settlement bridge's
//! `POST /fold`. The bridge meters it with a gasless `consume_credits` on a
//! MagicBlock ER, folding `provenance_root = sha256(provenance_root || receipt_hash)`
//! on-chain, and returns the ER signature, the new root, and the fold's position in
//! the chain. That outcome is written back onto the stored receipt.
//!
//! It is deliberately fire-and-forget from the request's point of view: the enqueue
//! is a non-blocking `try_send`, so a slow or dead bridge never delays or fails an
//! inference response. The chain is order-sensitive, so a single worker drains the
//! queue in enqueue order and folds one receipt at a time.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use axum::body::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::receipts::{ReceiptStore, Settlement};

#[derive(Serialize)]
struct FoldRequest<'a> {
    receipt_hash_hex: &'a str,
    amount: u64,
}

#[derive(Deserialize)]
struct FoldResponse {
    er_sig: String,
    provenance_root_hex: String,
    position: u64,
}

/// Hands receipt hashes to a background worker that folds them via the bridge. Drop
/// this to close the queue; the worker drains what is left and exits.
pub struct SettlementFolder {
    tx: mpsc::Sender<String>,
}

impl SettlementFolder {
    /// Spawns the fold worker against `bridge_url` (e.g. `http://127.0.0.1:8799`).
    /// `amount` is the credit debited per fold (>=1); `capacity` bounds the queue so
    /// a backed-up bridge sheds load instead of growing memory without bound.
    pub fn spawn(
        bridge_url: String,
        amount: u64,
        store: Arc<ReceiptStore>,
        capacity: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<String>(capacity.max(1));
        let base = Arc::new(bridge_url);
        tokio::spawn(async move {
            while let Some(receipt_hash) = rx.recv().await {
                let settlement = match fold(&base, &receipt_hash, amount).await {
                    Ok(response) => {
                        tracing::info!(
                            receipt = %short(&receipt_hash),
                            position = response.position,
                            er_sig = %short(&response.er_sig),
                            "folded receipt into provenance root"
                        );
                        Settlement {
                            er_sig: Some(response.er_sig),
                            provenance_root: Some(response.provenance_root_hex),
                            position: Some(response.position),
                            error: None,
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            receipt = %short(&receipt_hash),
                            error = %format_args!("{error:#}"),
                            "fold receipt"
                        );
                        Settlement {
                            error: Some(format!("{error:#}")),
                            ..Settlement::default()
                        }
                    }
                };
                store.record_settlement(&receipt_hash, settlement);
            }
        });
        Self { tx }
    }

    /// Non-blocking enqueue. A full queue drops the receipt (logged) rather than
    /// applying back-pressure to the inference path.
    pub fn enqueue(&self, receipt_hash: String) {
        if let Err(error) = self.tx.try_send(receipt_hash) {
            tracing::warn!(%error, "settlement queue full; receipt not folded");
        }
    }
}

async fn fold(base_url: &str, receipt_hash: &str, amount: u64) -> anyhow::Result<FoldResponse> {
    let body = serde_json::to_vec(&FoldRequest {
        receipt_hash_hex: receipt_hash,
        amount,
    })
    .expect("fold request serializes");
    let raw = post_json(base_url, "/fold", body).await?;
    serde_json::from_slice(&raw).context("decode fold response")
}

/// Minimal HTTP/1.1 JSON POST over a fresh TCP connection. The bridge is a local
/// process, so this speaks plain `http://host:port` and buffers the reply; it exists
/// to avoid pulling a second HTTP client into the gateway's pinned transport tree.
async fn post_json(base_url: &str, path: &str, body: Vec<u8>) -> anyhow::Result<Bytes> {
    let authority = base_url
        .trim_end_matches('/')
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let addr = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };

    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr))
        .await
        .context("connect settlement bridge timed out")?
        .with_context(|| format!("connect settlement bridge {addr}"))?;
    stream.set_nodelay(true).ok();

    let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .context("http/1 handshake with settlement bridge")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(%error, "settlement bridge connection closed");
        }
    });

    let request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(HOST, authority)
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_LENGTH, body.len())
        .body(Full::new(Bytes::from(body)))
        .context("build fold request")?;

    // One deadline covers both sending the request and reading the body: a bridge
    // that returns headers then stalls the body would otherwise wedge the single
    // fold worker forever and silently stop all further folds.
    let (status, bytes) = tokio::time::timeout(Duration::from_secs(30), async {
        let response = sender
            .send_request(request)
            .await
            .context("send fold request")?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .context("read fold response")?
            .to_bytes();
        anyhow::Ok((status, bytes))
    })
    .await
    .context("settlement bridge fold timed out")??;
    if !status.is_success() {
        return Err(anyhow!(
            "settlement bridge {status}: {}",
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(160)
                .collect::<String>()
        ));
    }
    Ok(bytes)
}

fn short(s: &str) -> &str {
    // Truncate on a char boundary: `er_sig` is bridge-supplied, and a byte-index
    // slice would panic (killing the worker) if a multibyte char fell in range.
    match s.char_indices().nth(12) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
