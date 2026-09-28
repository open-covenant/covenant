//! Minimal live buyer: sign one job envelope with a throwaway identity,
//! submit it to a running coordinator, poll the receipt, and re-verify
//! every commitment locally (receipt signature, job-hash pin, output
//! hash) — exactly what `covenantd`'s `compute.infer` path does, as a
//! hand-runnable smoke tool against real coordinator + node binaries.
//!
//! Run from `agent-os/` with a coordinator (and at least one node)
//! already up:
//!
//! `cargo run -p covenant-compute-coordinator --example buyer_smoke`
//!
//! - `COVENANT_COMPUTE_COORDINATOR_URL` — default `http://127.0.0.1:8720`
//! - `COVENANT_COMPUTE_SMOKE_PROMPT` — job input; with the node's
//!   default subprocess executor this is a shell command
//!   (default `echo hello-compute`)
//! - `COVENANT_COMPUTE_SMOKE_KIND` — `batch_job` (default) or
//!   `inference_call` (for an ollama node)
//! - `COVENANT_COMPUTE_SMOKE_MODEL` — model id to require (optional)
//! - `COVENANT_COMPUTE_SMOKE_DEADLINE_MS` — default 30000; raise for
//!   a first-generation model load
//! - `COVENANT_COMPUTE_SMOKE_PRICE_MICRO_USDC` — default 1000
//! - `COVENANT_COMPUTE_SMOKE_STREAM=1` — sign the stream flag and
//!   print the live output feed while the job runs, then check the
//!   assembled feed against the receipt-verified output (a node whose
//!   executor cannot stream just yields no chunks — advisory by
//!   protocol design, the verification still runs)
//! - `COVENANT_COMPUTE_RPC_URL` — your own Solana RPC endpoint; set it
//!   and the final verify_payout step proves the payout transfer
//!   on-chain instead of stopping at the off-chain record
//!
//! Exits 0 only on a verified, completed job.

use std::time::Duration;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_compute_protocol::{
    output_hash_hex, CapabilityRequirement, JobEnvelopePayload, JobKind, SignedJobEnvelope,
    SignedWorkReceipt,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::var("COVENANT_COMPUTE_COORDINATOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8720".into());
    let prompt = std::env::var("COVENANT_COMPUTE_SMOKE_PROMPT")
        .unwrap_or_else(|_| "echo hello-compute".into());
    let price: u64 = std::env::var("COVENANT_COMPUTE_SMOKE_PRICE_MICRO_USDC")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000);
    let kind = match std::env::var("COVENANT_COMPUTE_SMOKE_KIND").as_deref() {
        Ok("inference_call") => JobKind::InferenceCall,
        _ => JobKind::BatchJob,
    };
    let model_id = std::env::var("COVENANT_COMPUTE_SMOKE_MODEL").ok();
    let deadline_ms: u64 = std::env::var("COVENANT_COMPUTE_SMOKE_DEADLINE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);
    let stream = std::env::var("COVENANT_COMPUTE_SMOKE_STREAM").as_deref() == Ok("1");

    let buyer = LocalIdentity::generate("buyer@smoke");
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id,
            kind,
            max_duration_secs: (deadline_ms / 1_000).max(1) as u32,
            min_reputation_bps: None,
        },
        input: vec![Content::text(prompt.clone())],
        price_micro_usdc: price,
        deadline_ms,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer)?;

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?;

    println!("submitting job {job_id} ({prompt:?}, {price} micro-USDC) to {base}");
    let resp = http
        .post(format!("{base}/federation/jobs"))
        .json(&envelope)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("submit failed: {} {}", resp.status(), resp.text().await?);
    }

    let deadline_at = epoch_ms() + deadline_ms + 5_000;

    // The live feed first, when asked for: drain chunks by signed
    // cursor reads until the job leaves its running phases, printing
    // each delta as it lands. The receipt loop below then completes on
    // its first poll and the assembled feed is held against the
    // verified output.
    let mut assembled = String::new();
    if stream {
        use std::io::Write as _;
        let stream_path = format!("/federation/jobs/{job_id}/stream");
        let mut since = 0u64;
        let mut printed_any = false;
        loop {
            let signed_at_ms = epoch_ms();
            let signature =
                covenant_compute_protocol::sign_read(&buyer, &stream_path, signed_at_ms)?;
            let view: serde_json::Value = http
                .get(format!("{base}{stream_path}?since={since}"))
                .header(
                    covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                    signed_at_ms.to_string(),
                )
                .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
                .send()
                .await?
                .json()
                .await?;
            for chunk in view["chunks"]
                .as_array()
                .map(|c| c.as_slice())
                .unwrap_or(&[])
            {
                let text = chunk["text"].as_str().unwrap_or_default();
                if !printed_any {
                    print!("stream: ");
                    printed_any = true;
                }
                print!("{text}");
                std::io::stdout().flush().ok();
                assembled.push_str(text);
            }
            since = view["next_seq"].as_u64().unwrap_or(since);
            let status = view["status"].as_str().unwrap_or_default();
            let done = view["done"].as_bool().unwrap_or(false);
            if matches!(status, "completed" | "failed" | "refunded" | "rejected")
                || done
                || epoch_ms() > deadline_at
            {
                if printed_any {
                    println!();
                }
                if view["truncated"].as_bool().unwrap_or(false) {
                    println!("stream: truncated by the relay cap; the receipt output is whole");
                    assembled.clear();
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    let receipt_path = format!("/federation/jobs/{job_id}/receipt");
    loop {
        // The receipt carries the output; the coordinator only serves
        // it to a signed read by the buyer key that submitted the job.
        let signed_at_ms = epoch_ms();
        let signature = covenant_compute_protocol::sign_read(&buyer, &receipt_path, signed_at_ms)?;
        let status: serde_json::Value = http
            .get(format!("{base}{receipt_path}"))
            .header(
                covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                signed_at_ms.to_string(),
            )
            .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
            .send()
            .await?
            .json()
            .await?;
        match status["status"].as_str().unwrap_or_default() {
            "completed" => {
                let receipt: SignedWorkReceipt = serde_json::from_value(status["receipt"].clone())?;
                let output: Vec<Content> = serde_json::from_value(status["output"].clone())?;
                receipt.verify()?;
                anyhow::ensure!(
                    receipt.receipt.job_id == job_id,
                    "receipt names another job"
                );
                anyhow::ensure!(
                    receipt.receipt.job_hash_hex == sha256_hex(envelope.payload_json.as_bytes()),
                    "receipt is not bound to the envelope we signed"
                );
                anyhow::ensure!(
                    output_hash_hex(&output) == receipt.receipt.result_hash_hex,
                    "output does not hash to the signed receipt"
                );
                println!(
                    "verified: operator {} served the job in {} ms",
                    receipt.signer_pubkey_b58, receipt.receipt.meter.wall_ms
                );
                for block in &output {
                    match block {
                        Content::Text { text } => println!("output: {text}"),
                        Content::Json { value } => println!("output: {value}"),
                    }
                }
                if stream {
                    let final_text: String = output
                        .iter()
                        .filter_map(|c| match c {
                            Content::Text { text } => Some(text.as_str()),
                            Content::Json { .. } => None,
                        })
                        .collect();
                    if assembled.is_empty() {
                        println!(
                            "stream: no chunks arrived (node executor doesn't stream); \
                             output verified all the same"
                        );
                    } else {
                        anyhow::ensure!(
                            assembled == final_text,
                            "assembled stream diverges from the receipt-verified output"
                        );
                        println!("stream: assembled feed equals the verified output");
                    }
                }
                // Where the money went, when the payout push had
                // already landed. The memo is the receipt's own
                // derivation — check it on-chain via the tx signature.
                if status["payout"].is_object() {
                    let expected = receipt.payout_memo();
                    let echoed = status["payout"]["memo"].as_str().unwrap_or_default();
                    anyhow::ensure!(
                        echoed == expected,
                        "coordinator echoed a memo this receipt does not derive"
                    );
                    println!(
                        "payout: {} micro-USDC, tx {}, memo {}",
                        status["payout"]["amount_micro_usdc"],
                        status["payout"]["tx_signature"]
                            .as_str()
                            .unwrap_or("(none yet — mock backend or still confirming)"),
                        expected
                    );
                } else {
                    println!("payout: not pushed yet (retry sweep will; re-poll for the tx)");
                }

                // The composed close: one verify_payout call — exactly
                // what `compute.verify` serves an agent — re-reads the
                // history row, re-verifies the receipt, and (with an
                // RPC endpoint configured) holds the chain to the
                // books. A contradiction fails the smoke run.
                let rpc_url = std::env::var("COVENANT_COMPUTE_RPC_URL")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                let no_rpc = rpc_url.is_none();
                let config = covenant_compute_buyer::BuyerConfig {
                    coordinator_url: base.clone(),
                    poll_interval: Duration::from_millis(500),
                    referral_code: None,
                    rpc_url,
                };
                match covenant_compute_buyer::verify_payout(&http, &config, &buyer, job_id).await {
                    Ok(v) => {
                        println!("verify_payout: {} — {}", v.verdict, v.detail);
                        if let Some(proof) = v.proof {
                            println!(
                                "chain record: {} base units of {} to {}",
                                proof.amount_micro_usdc, proof.mint_b58, proof.recipient_owner_b58
                            );
                        }
                    }
                    // Without an RPC endpoint an already-pushed payout
                    // can't be read back — a config gap, not a
                    // contradiction.
                    Err(e) if no_rpc => {
                        println!("verify_payout: {e}");
                        println!("set COVENANT_COMPUTE_RPC_URL to prove the transfer on-chain");
                    }
                    Err(e) => anyhow::bail!("verify_payout contradicted the books: {e}"),
                }
                return Ok(());
            }
            "refunded" | "rejected" | "failed" => {
                anyhow::bail!("job was {}", status["status"]);
            }
            other => {
                if epoch_ms() > deadline_at {
                    anyhow::bail!("no receipt before deadline (last status: {other})");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}
