//! Reference reader for the public settlement proof feed. Reads
//! `/proof/receipts` from a running coordinator and checks each entry the
//! way any third party would: the operator's receipt signature and the
//! release arithmetic offline, then — given a Solana RPC — the cited
//! payout transaction on-chain. It then fetches the batch root from
//! `/proof/batch` and proves each job is a member of it, the way a monitor
//! confirms one settlement against the commitment without downloading the
//! rest. Nothing here trusts the coordinator; the coordinator only points at
//! evidence, and this reproduces every check against it with the same
//! `covenant-compute-protocol` code the payout path stamps.
//!
//! Run from `agent-os/` against a coordinator serving the feed
//! (`COVENANT_COMPUTE_PUBLIC_PROOF_FEED=1`):
//!
//! `cargo run -p covenant-compute-coordinator --example verify_proof_feed`
//!
//! - `COVENANT_COMPUTE_COORDINATOR_URL` — default `http://127.0.0.1:8720`
//! - `COVENANT_COMPUTE_RPC_URL` — your own Solana RPC; set it and each
//!   proof is confirmed on-chain, otherwise the reader stops at the
//!   offline half and prints the request you would run
//! - `COVENANT_COMPUTE_PROOF_LIMIT` — how many recent proofs to pull
//!   (default 20)
//!
//! Exits 0 only when every proof it pulled verified as far as it could.

use covenant_compute_protocol::{BatchInclusionProof, SettlementBatch, SettlementProof};
use serde_json::Value;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::var("COVENANT_COMPUTE_COORDINATOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8720".into());
    let rpc_url = std::env::var("COVENANT_COMPUTE_RPC_URL").ok();
    let limit: usize = std::env::var("COVENANT_COMPUTE_PROOF_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let http = reqwest::Client::new();
    let feed_url = format!("{base}/proof/receipts?limit={limit}");
    let resp = http.get(&feed_url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("GET {feed_url} -> {status}: {body}");
    }
    let proofs: Vec<SettlementProof> = resp.json().await?;
    if proofs.is_empty() {
        println!("the feed is empty — no settled on-chain payouts to verify yet");
        return Ok(());
    }
    println!("verifying {} proof(s) from {base}\n", proofs.len());

    let mut failures = 0usize;
    for proof in &proofs {
        print!("job {} ", proof.job_id);
        if let Err(e) = proof.verify_offline() {
            println!("FAILED offline: {e}");
            failures += 1;
            continue;
        }
        let Some(rpc_url) = &rpc_url else {
            println!(
                "offline OK (net {} in mint {} to {}); set COVENANT_COMPUTE_RPC_URL to confirm on-chain",
                proof.net_micro_usdc, proof.mint_b58, proof.payout_address_b58
            );
            continue;
        };
        let tx = match fetch_transaction(&http, rpc_url, proof).await {
            Ok(tx) => tx,
            Err(e) => {
                println!("FAILED to fetch payout {}: {e}", proof.tx_signature);
                failures += 1;
                continue;
            }
        };
        match proof.verify(&tx) {
            Ok(paid) => println!(
                "VERIFIED: paid {} micro-USDC to {} in tx {}",
                paid.amount_micro_usdc, paid.recipient_owner_b58, proof.tx_signature
            ),
            Err(e) => {
                println!("FAILED on-chain: {e}");
                failures += 1;
            }
        }
    }

    // The committed feed: one binding root over every settlement, then prove
    // each job we pulled is a member. Keep a root and a proof and the
    // coordinator can't later deny a settlement it committed.
    let batch: SettlementBatch = http
        .get(format!("{base}/proof/batch"))
        .send()
        .await?
        .json()
        .await?;
    println!(
        "\nbatch root {} over {} settlement(s)",
        batch.root_hex, batch.tree_size
    );
    for proof in &proofs {
        let inclusion: BatchInclusionProof = http
            .get(format!("{base}/proof/batch/{}", proof.job_id))
            .send()
            .await?
            .json()
            .await?;
        match inclusion.verify_offline(&batch.root_hex) {
            Ok(()) => println!(
                "  job {} included at leaf {}",
                proof.job_id, inclusion.leaf_index
            ),
            Err(e) => {
                println!("  job {} FAILED inclusion: {e}", proof.job_id);
                failures += 1;
            }
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} check(s) did not verify");
    }
    println!(
        "\nall {} proof(s) verified, all under batch root",
        proofs.len()
    );
    Ok(())
}

/// Fetches the cited payout with the exact request the proof names, and
/// returns the transaction the verifier reads (`null` if the chain has no
/// record of it, which `verify` then rejects).
async fn fetch_transaction(
    http: &reqwest::Client,
    rpc_url: &str,
    proof: &SettlementProof,
) -> anyhow::Result<Value> {
    let response: Value = http
        .post(rpc_url)
        .json(&proof.payout_rpc_request())
        .send()
        .await?
        .json()
        .await?;
    if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
        anyhow::bail!("rpc error: {error}");
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}
