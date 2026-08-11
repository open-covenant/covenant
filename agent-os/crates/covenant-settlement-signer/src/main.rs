//! Standalone Covenant settlement batch-anchor signer (sidecar to covenantd).
//!
//! One-shot, stdin -> stdout. The daemon spawns this process per flush,
//! pipes an [`AnchorRequest`] as JSON to stdin, and reads an
//! [`AnchorResponse`] from stdout. The anchoring key never enters the
//! daemon's address space, and the anchor-lang 0.31 / solana-sdk 2 dep
//! tree never enters the daemon's build.
//!
//! The one write action is `anchor_receipt_batch` against the deployed
//! Covenant settlement program: it inits the `["receipt_batch", batch_id]`
//! PDA with the batch's merkle root and receipt count in a single confirmed
//! transaction, signed by the program's `config.authority`.
//!
//! Idempotency: the anchor instruction is `init`, so a second submission of
//! an already-anchored `batch_id` fails on-chain. This sidecar preflights
//! `getAccountInfo(receipt_batch_pda)` and, when the batch already exists,
//! returns `already_anchored: true` with the original signature instead of
//! resubmitting — so a re-flush after a confirm-timeout is a safe no-op.
//!
//! Devnet only. `assert_devnet` refuses to run unless the cluster is
//! `devnet`, the RPC URL looks like a devnet endpoint, and the configured
//! program id is the one this binary is linked against. Mainnet promotion
//! is a separate, human-approved step and is not reachable from here.
//!
//! Protocol:
//! - stdin:  one JSON `AnchorRequest`.
//! - stdout: one JSON `AnchorResponse` on success.
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! Configuration (env, set by the daemon, read after `env_clear`):
//! - `COVENANT_SETTLEMENT_KEYPAIR`: anchoring keypair JSON path. Required.
//! - `COVENANT_SETTLEMENT_RPC_URL`: devnet Solana RPC. Required.
//! - `COVENANT_SETTLEMENT_CLUSTER`: must be `devnet`. Default `devnet`.
//! - `COVENANT_SETTLEMENT_PROGRAM_ID`: settlement program id. Required; must
//!   equal the linked program id.

use std::process::ExitCode;
use std::str::FromStr;

use anchor_lang::{AccountDeserialize, InstructionData};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use covenant_settlement_program::{
    instruction as ix, AnchorReceiptBatchArgs, ReceiptBatch, ID as PROGRAM_ID,
};
use serde::{Deserialize, Serialize};
use solana_sdk::hash::Hash;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One flush request from the daemon. `batch_id` and `merkle_root` are the
/// 64-char lowercase-hex strings [`covenant_settlement::build_receipt_batch`]
/// produces; `receipt_count` is the number of receipts in the batch.
#[derive(Debug, Clone, Deserialize)]
struct AnchorRequest {
    batch_id: String,
    merkle_root: String,
    receipt_count: u32,
}

/// The signer's reply. JSON on stdout. `confirmed` is always true on a
/// successful exit (the process fails otherwise); it is explicit so the
/// daemon reads the same field on every path. `already_anchored` is true
/// when the batch existed on-chain before this call and no transaction was
/// submitted.
#[derive(Debug, Clone, Serialize)]
struct AnchorResponse {
    confirmed: bool,
    signature: String,
    slot: u64,
    confirmed_at: u64,
    already_anchored: bool,
    cluster: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    match run().await {
        Ok(resp) => {
            let line = match serde_json::to_string(&resp) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("covenant-settlement-signer: encode response: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let mut stdout = tokio::io::stdout();
            if stdout.write_all(line.as_bytes()).await.is_err() {
                return ExitCode::FAILURE;
            }
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("covenant-settlement-signer: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<AnchorResponse> {
    let keypair_path = std::env::var("COVENANT_SETTLEMENT_KEYPAIR")
        .map_err(|_| anyhow!("COVENANT_SETTLEMENT_KEYPAIR is not set"))?;
    let rpc_url = std::env::var("COVENANT_SETTLEMENT_RPC_URL")
        .map_err(|_| anyhow!("COVENANT_SETTLEMENT_RPC_URL is not set"))?;
    let cluster =
        std::env::var("COVENANT_SETTLEMENT_CLUSTER").unwrap_or_else(|_| "devnet".to_string());
    let program_id = std::env::var("COVENANT_SETTLEMENT_PROGRAM_ID")
        .map_err(|_| anyhow!("COVENANT_SETTLEMENT_PROGRAM_ID is not set"))?;
    let program_id =
        Pubkey::from_str(&program_id).map_err(|e| anyhow!("invalid program id: {e}"))?;

    // The mainnet gate, first half. Refuse before loading the key or touching
    // the network if anything about the configured target is not the intended
    // devnet program. Mainnet promotion is a separate, human-approved task.
    assert_devnet(&cluster, &rpc_url, &program_id)?;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // The mainnet gate, second half. The RPC URL is a hint, not proof: a
    // devnet-substring endpoint that actually fronts mainnet clears
    // assert_devnet. getGenesisHash is the cluster's authoritative identity,
    // so verify it against devnet's before loading the key or sending a
    // transaction. Fail closed.
    assert_devnet_genesis(&fetch_genesis_hash(&http, &rpc_url).await?)?;

    let payer = load_keypair(&keypair_path)?;

    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let request: AnchorRequest = serde_json::from_str(input.trim())
        .map_err(|e| anyhow!("decode AnchorRequest from stdin: {e}"))?;

    if request.receipt_count == 0 {
        bail!("receipt_count must be > 0");
    }
    let batch_id = decode_hex32(&request.batch_id).context("batch_id")?;
    let merkle_root = decode_hex32(&request.merkle_root).context("merkle_root")?;

    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &PROGRAM_ID);
    let (batch_pda, _) = Pubkey::find_program_address(&[b"receipt_batch", &batch_id], &PROGRAM_ID);

    // Idempotency preflight: the anchor instruction inits the batch PDA, so
    // re-anchoring the same batch_id fails on-chain. If the batch already
    // exists, confirm the stored root matches and report the original
    // signature instead of resubmitting — the safe re-flush path.
    if let Some(existing) = get_batch_account(&http, &rpc_url, &batch_pda).await? {
        if existing.merkle_root != merkle_root {
            bail!(
                "batch {} already anchored with a different merkle root; refusing to reconcile",
                request.batch_id
            );
        }
        let (signature, slot) = original_anchor_signature(&http, &rpc_url, &batch_pda).await?;
        return Ok(AnchorResponse {
            confirmed: true,
            signature,
            slot,
            confirmed_at: confirmed_at_from_created(existing.created_at)?,
            already_anchored: true,
            cluster,
        });
    }

    let data = ix::AnchorReceiptBatch {
        args: AnchorReceiptBatchArgs {
            batch_id,
            merkle_root,
            receipt_count: request.receipt_count,
        },
    }
    .data();
    let anchor_ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(batch_pda, false),
            AccountMeta::new(payer.pubkey(), true),
            // System Program is the all-zero address; referenced directly to
            // avoid the deprecated solana-sdk system_program facade.
            AccountMeta::new_readonly(Pubkey::default(), false),
        ],
        data,
    };

    let recent = latest_blockhash(&http, &rpc_url).await?;
    let tx =
        Transaction::new_signed_with_payer(&[anchor_ix], Some(&payer.pubkey()), &[&payer], recent);

    let signature = match send_transaction(&http, &rpc_url, &tx).await {
        Ok(sig) => sig,
        Err(e) => {
            // A confirm-timeout on a prior flush can leave the batch anchored
            // while the daemon still holds the receipts unsettled. If the PDA
            // now exists, the prior submission won the race: report it as
            // already-anchored rather than surfacing a spurious error.
            if let Some(existing) = get_batch_account(&http, &rpc_url, &batch_pda).await? {
                if existing.merkle_root != merkle_root {
                    bail!(
                        "batch {} already anchored with a different merkle root; refusing to reconcile",
                        request.batch_id
                    );
                }
                let (signature, slot) =
                    original_anchor_signature(&http, &rpc_url, &batch_pda).await?;
                return Ok(AnchorResponse {
                    confirmed: true,
                    signature,
                    slot,
                    confirmed_at: confirmed_at_from_created(existing.created_at)?,
                    already_anchored: true,
                    cluster,
                });
            }
            return Err(e);
        }
    };

    let slot = confirm_signature(&http, &rpc_url, &signature).await?;

    // Read the anchored batch back for its on-chain timestamp. `created_at`
    // is the cluster's block time at anchor — authoritative and never zero,
    // which the daemon's audit invariants require of a confirmed receipt.
    let anchored = get_batch_account(&http, &rpc_url, &batch_pda)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "anchored batch {} not found after confirmation",
                request.batch_id
            )
        })?;
    if anchored.merkle_root != merkle_root {
        bail!(
            "anchored batch {} merkle root does not match the request",
            request.batch_id
        );
    }

    Ok(AnchorResponse {
        confirmed: true,
        signature,
        slot,
        confirmed_at: confirmed_at_from_created(anchored.created_at)?,
        already_anchored: false,
        cluster,
    })
}

/// The devnet gate. Every condition must hold or the process refuses to run
/// before loading the key or opening a socket.
fn assert_devnet(cluster: &str, rpc_url: &str, program_id: &Pubkey) -> Result<()> {
    if cluster != "devnet" {
        bail!("refusing to run: cluster is {cluster:?}, only devnet is permitted by this binary");
    }
    if !rpc_url.contains("devnet") {
        bail!("refusing to run: RPC url {rpc_url:?} is not a recognizable devnet endpoint");
    }
    // Two-sided: a URL may carry the devnet substring and still name mainnet
    // (a proxy, or a path with both). Fail closed on any mainnet mention.
    if rpc_url.contains("mainnet") {
        bail!("refusing to run: RPC url {rpc_url:?} names mainnet; this binary anchors on devnet only");
    }
    if *program_id != PROGRAM_ID {
        bail!(
            "refusing to run: configured program id {program_id} is not the linked settlement program {PROGRAM_ID}"
        );
    }
    Ok(())
}

/// Devnet's genesis hash — the cluster's authoritative identity, independent
/// of the RPC URL string. Sourced from `getGenesisHash` on
/// `https://api.devnet.solana.com`.
const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

/// Refuse unless the cluster's live genesis hash is devnet's. This is the
/// authoritative half of the mainnet gate: [`assert_devnet`] screens the
/// configured strings, this proves the endpoint the process actually reached
/// is a devnet cluster.
fn assert_devnet_genesis(genesis_hash: &str) -> Result<()> {
    if genesis_hash != DEVNET_GENESIS_HASH {
        bail!(
            "refusing to run: cluster genesis hash {genesis_hash} is not devnet's \
             ({DEVNET_GENESIS_HASH}); the RPC endpoint is not a devnet cluster"
        );
    }
    Ok(())
}

fn confirmed_at_from_created(created_at: i64) -> Result<u64> {
    u64::try_from(created_at)
        .map_err(|_| anyhow!("on-chain created_at {created_at} is not a valid unix timestamp"))
        .and_then(|t| {
            if t == 0 {
                Err(anyhow!("on-chain created_at is zero"))
            } else {
                Ok(t)
            }
        })
}

fn decode_hex32(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 {
        bail!("expected 64 hex characters, got {}", s.len());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow!("invalid hex at byte {i}: {e}"))?;
    }
    Ok(out)
}

fn load_keypair(path: &str) -> Result<Keypair> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read keypair {path}"))?;
    let bytes: Vec<u8> =
        serde_json::from_str(raw.trim()).context("keypair file must be a JSON byte array")?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow!("load keypair: {e}"))
}

async fn rpc(
    http: &reqwest::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "covenant-settlement-signer",
        "method": method,
        "params": params,
    });
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("rpc {method}"))?;
    let value: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("rpc {method} decode"))?;
    if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
        bail!("rpc {method} error: {err}");
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("rpc {method}: no result"))
}

/// The cluster's genesis hash via `getGenesisHash` — its authoritative
/// identity, which [`assert_devnet_genesis`] checks against devnet's.
async fn fetch_genesis_hash(http: &reqwest::Client, url: &str) -> Result<String> {
    let result = rpc(http, url, "getGenesisHash", serde_json::json!([])).await?;
    result
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("getGenesisHash: result was not a string"))
}

/// Fetch and deserialize the on-chain `ReceiptBatch` at `pda`, or `None`
/// when the account does not exist.
async fn get_batch_account(
    http: &reqwest::Client,
    url: &str,
    pda: &Pubkey,
) -> Result<Option<ReceiptBatch>> {
    let result = rpc(
        http,
        url,
        "getAccountInfo",
        serde_json::json!([pda.to_string(), { "encoding": "base64", "commitment": "confirmed" }]),
    )
    .await?;
    let Some(value) = result.get("value").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let encoded = value
        .get("data")
        .and_then(|d| d.get(0))
        .and_then(|d| d.as_str())
        .ok_or_else(|| anyhow!("getAccountInfo: missing account data"))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decode account data")?;
    let batch = ReceiptBatch::try_deserialize(&mut raw.as_slice())
        .map_err(|e| anyhow!("deserialize ReceiptBatch: {e}"))?;
    Ok(Some(batch))
}

/// Recover the signature and slot of the transaction that created `pda`.
/// The batch PDA is init-only, so exactly one transaction ever touches it;
/// `getSignaturesForAddress` with `limit: 1` returns that sole anchor.
async fn original_anchor_signature(
    http: &reqwest::Client,
    url: &str,
    pda: &Pubkey,
) -> Result<(String, u64)> {
    let result = rpc(
        http,
        url,
        "getSignaturesForAddress",
        serde_json::json!([pda.to_string(), { "limit": 1, "commitment": "confirmed" }]),
    )
    .await?;
    let entry = result
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("no confirmed signature found for anchored batch {pda}"))?;
    let signature = entry
        .get("signature")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("getSignaturesForAddress: missing signature"))?
        .to_string();
    let slot = entry.get("slot").and_then(|s| s.as_u64()).unwrap_or(0);
    Ok((signature, slot))
}

async fn latest_blockhash(http: &reqwest::Client, url: &str) -> Result<Hash> {
    let result = rpc(
        http,
        url,
        "getLatestBlockhash",
        serde_json::json!([{ "commitment": "confirmed" }]),
    )
    .await?;
    let bh = result
        .get("value")
        .and_then(|v| v.get("blockhash"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("getLatestBlockhash: missing blockhash"))?;
    Hash::from_str(bh).map_err(|e| anyhow!("parse blockhash: {e}"))
}

async fn send_transaction(http: &reqwest::Client, url: &str, tx: &Transaction) -> Result<String> {
    let raw = bincode::serialize(tx).context("serialize transaction")?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    let result = rpc(
        http,
        url,
        "sendTransaction",
        serde_json::json!([encoded, { "encoding": "base64", "preflightCommitment": "confirmed" }]),
    )
    .await?;
    result
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("sendTransaction: result was not a signature string"))
}

/// Poll `getSignatureStatuses` until the tx is confirmed/finalized or the
/// window elapses, returning the slot it landed in. An on-chain error
/// returns `Err`; a timeout returns `Err` naming the signature so the batch
/// is left retryable. Recent blockhash validity (~60-90s) bounds the wait.
async fn confirm_signature(http: &reqwest::Client, url: &str, signature: &str) -> Result<u64> {
    const MAX_POLLS: u32 = 40;
    for _ in 0..MAX_POLLS {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        let result = rpc(
            http,
            url,
            "getSignatureStatuses",
            serde_json::json!([[signature], { "searchTransactionHistory": true }]),
        )
        .await?;
        let status = result.get("value").and_then(|v| v.get(0));
        let Some(status) = status.filter(|s| !s.is_null()) else {
            continue; // not yet seen by the cluster
        };
        if let Some(err) = status.get("err").filter(|e| !e.is_null()) {
            bail!("transaction {signature} failed on-chain: {err}");
        }
        let confirmed = status
            .get("confirmationStatus")
            .and_then(|c| c.as_str())
            .map(|c| c == "confirmed" || c == "finalized")
            .unwrap_or(false);
        if confirmed {
            let slot = status.get("slot").and_then(|s| s.as_u64()).unwrap_or(0);
            return Ok(slot);
        }
    }
    bail!("transaction {signature} not confirmed within timeout; check it on-chain")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_devnet_accepts_devnet_target() {
        assert!(assert_devnet("devnet", "https://api.devnet.solana.com", &PROGRAM_ID).is_ok());
    }

    #[test]
    fn assert_devnet_refuses_non_devnet_cluster() {
        let err = assert_devnet("mainnet-beta", "https://api.devnet.solana.com", &PROGRAM_ID)
            .unwrap_err()
            .to_string();
        assert!(err.contains("only devnet"), "unexpected: {err}");
    }

    #[test]
    fn assert_devnet_refuses_non_devnet_rpc() {
        let err = assert_devnet("devnet", "https://api.mainnet-beta.solana.com", &PROGRAM_ID)
            .unwrap_err()
            .to_string();
        assert!(err.contains("devnet endpoint"), "unexpected: {err}");
    }

    #[test]
    fn assert_devnet_refuses_rpc_naming_mainnet_even_with_devnet_substring() {
        let err = assert_devnet(
            "devnet",
            "https://devnet.mainnet-beta.example.com",
            &PROGRAM_ID,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("names mainnet"), "unexpected: {err}");
    }

    #[test]
    fn assert_devnet_refuses_foreign_program_id() {
        let foreign = Pubkey::new_unique();
        let err = assert_devnet("devnet", "https://api.devnet.solana.com", &foreign)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not the linked settlement program"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn assert_devnet_genesis_accepts_devnet_hash() {
        assert!(assert_devnet_genesis(DEVNET_GENESIS_HASH).is_ok());
    }

    #[test]
    fn assert_devnet_genesis_refuses_mainnet_hash() {
        // Solana mainnet-beta's genesis hash: the exact endpoint a
        // misleading devnet-substring URL could front, and the case this
        // gate exists to refuse.
        let err = assert_devnet_genesis("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d")
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not devnet"), "unexpected: {err}");
    }

    #[test]
    fn decode_hex32_round_trips() {
        let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let bytes = decode_hex32(hex).unwrap();
        assert_eq!(bytes[0], 0x00);
        assert_eq!(bytes[1], 0x11);
        assert_eq!(bytes[31], 0xff);
    }

    #[test]
    fn decode_hex32_rejects_wrong_length() {
        assert!(decode_hex32("00").is_err());
    }

    #[test]
    fn confirmed_at_rejects_zero_and_negative() {
        assert!(confirmed_at_from_created(0).is_err());
        assert!(confirmed_at_from_created(-1).is_err());
        assert_eq!(
            confirmed_at_from_created(1_700_000_000).unwrap(),
            1_700_000_000
        );
    }
}
