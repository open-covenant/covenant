//! Standalone Metaplex minting-key signer (sidecar to covenantd).
//!
//! One-shot, stdin -> stdout. The daemon spawns this process per write,
//! pipes a [`covenant_metaplex::SignerRequest`] as JSON to stdin, and
//! reads a [`covenant_metaplex::SignerResponse`] from stdout. The minting
//! key never enters the daemon's address space, and the solana-sdk 3.x +
//! mpl-core dep tree never enters the daemon's build.
//!
//! v1 scope: every write goes through MPL Core's AppData external plugin
//! — a fresh Core asset whose AppData carries the Covenant attestation /
//! identity JSON, with the minting key as the data authority. DAS indexes
//! JSON-schema AppData automatically, so the record is verifiable through
//! a plain DAS query with no Covenant infrastructure. Compressed-NFT
//! receipts (Bubblegum v2) and the native mpl-agent-identity register
//! instruction are later phases; both reuse this same sidecar.
//!
//! Protocol:
//! - stdin:  one JSON `SignerRequest`.
//! - stdout: one JSON `SignerResponse` on success.
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! Configuration (env, set by the daemon, read after `env_clear`):
//! - `COVENANT_METAPLEX_KEYPAIR`  — minting keypair JSON path. Required.
//! - `COVENANT_METAPLEX_RPC_URL`  — Solana RPC. Required.
//! - `COVENANT_METAPLEX_CLUSTER`  — `devnet` | `mainnet-beta`. Default devnet.
//! - `COVENANT_METAPLEX_COLLECTION` — MPL Core collection (optional).
//! - `COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS` — refuse a write whose
//!   estimated cost exceeds this. `0`/unset uses the built-in cap.

use std::process::ExitCode;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use covenant_metaplex::config::{ALLOWED_PROGRAM_IDS, MPL_CORE_PROGRAM_ID};
use covenant_metaplex::{SignerRequest, SignerResponse};
use mpl_core::instructions::{CreateV2Builder, WriteExternalPluginAdapterDataV1Builder};
use mpl_core::types::{
    AppDataInitInfo, DataState, ExternalPluginAdapterInitInfo, ExternalPluginAdapterKey,
    ExternalPluginAdapterSchema, PluginAuthority,
};
use solana_program::pubkey::Pubkey;
use solana_sdk::hash::Hash;
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default per-action ceiling (~0.02 SOL) when none is configured. A
/// single Core asset + AppData write is well under this; the cap exists
/// to refuse a pathologically large AppData payload draining the key.
const DEFAULT_CAP_LAMPORTS: u64 = 20_000_000;
/// Coarse cost estimate parts (lamports). Asset account rent + the Core
/// protocol fee, plus rent for the AppData bytes (~2 years of byte-rent).
const ASSET_BASE_LAMPORTS: u64 = 2_900_000;
const CORE_PROTOCOL_FEE_LAMPORTS: u64 = 1_500_000;
const LAMPORTS_PER_DATA_BYTE: u64 = 6_960;
/// On-chain MPL Core asset name cap we stay under.
const NAME_MAX: usize = 32;

const IDENTITY_SCHEMA: &str = "covenant.identity.appdata.v1";

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
                    eprintln!("covenant-metaplex-signer: encode response: {e}");
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
            eprintln!("covenant-metaplex-signer: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<SignerResponse> {
    // Program-id pin: refuse to run if the linked mpl-core program id is
    // not the one Covenant trusts. Closes the only path by which a
    // dependency swap could redirect a signed instruction.
    let core_id = mpl_core::ID.to_string();
    if core_id != MPL_CORE_PROGRAM_ID || !ALLOWED_PROGRAM_IDS.contains(&core_id.as_str()) {
        bail!("mpl-core program id {core_id} is not the pinned {MPL_CORE_PROGRAM_ID}");
    }

    let keypair_path = std::env::var("COVENANT_METAPLEX_KEYPAIR")
        .map_err(|_| anyhow!("COVENANT_METAPLEX_KEYPAIR is not set"))?;
    let rpc_url = std::env::var("COVENANT_METAPLEX_RPC_URL")
        .map_err(|_| anyhow!("COVENANT_METAPLEX_RPC_URL is not set"))?;
    let cluster =
        std::env::var("COVENANT_METAPLEX_CLUSTER").unwrap_or_else(|_| "devnet".to_string());
    let cap = std::env::var("COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|c| *c > 0)
        .unwrap_or(DEFAULT_CAP_LAMPORTS);

    let payer = load_keypair(&keypair_path)?;

    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let request: SignerRequest = serde_json::from_str(input.trim())
        .map_err(|e| anyhow!("decode SignerRequest from stdin: {e}"))?;

    let plan = plan_write(&request, &payer.pubkey())?;

    // Per-action cost guard before we touch the key or the network.
    let estimate = ASSET_BASE_LAMPORTS
        + CORE_PROTOCOL_FEE_LAMPORTS
        + LAMPORTS_PER_DATA_BYTE * plan.data.len() as u64;
    if estimate > cap {
        bail!(
            "estimated cost {estimate} lamports exceeds the per-action cap {cap}; \
             raise COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS or shrink the payload"
        );
    }

    let http = reqwest::Client::new();
    let recent = latest_blockhash(&http, &rpc_url).await?;
    let asset = Keypair::new();

    let create_ix = CreateV2Builder::new()
        .asset(asset.pubkey())
        .payer(payer.pubkey())
        .collection(plan.collection)
        .data_state(DataState::AccountState)
        .name(plan.name.clone())
        .uri(plan.uri.clone())
        .external_plugin_adapters(vec![ExternalPluginAdapterInitInfo::AppData(AppDataInitInfo {
            data_authority: PluginAuthority::Address {
                address: payer.pubkey(),
            },
            init_plugin_authority: Some(PluginAuthority::Address {
                address: payer.pubkey(),
            }),
            schema: Some(ExternalPluginAdapterSchema::Json),
        })])
        .instruction();

    let write_ix = WriteExternalPluginAdapterDataV1Builder::new()
        .asset(asset.pubkey())
        .payer(payer.pubkey())
        .authority(Some(payer.pubkey()))
        .key(ExternalPluginAdapterKey::AppData(PluginAuthority::Address {
            address: payer.pubkey(),
        }))
        .data(plan.data)
        .instruction();

    let tx = Transaction::new_signed_with_payer(
        &[create_ix, write_ix],
        Some(&payer.pubkey()),
        &[&payer, &asset],
        recent,
    );

    let signature = send_transaction(&http, &rpc_url, &tx).await?;

    Ok(SignerResponse {
        signature,
        asset: asset.pubkey().to_string(),
        cluster,
    })
}

/// What we are about to write: the AppData bytes plus the asset's
/// human-facing name/uri and optional collection.
struct WritePlan {
    data: Vec<u8>,
    name: String,
    uri: String,
    collection: Option<Pubkey>,
}

fn plan_write(request: &SignerRequest, _authority: &Pubkey) -> Result<WritePlan> {
    let collection = collection_from_env()?;
    match request {
        SignerRequest::AttestAuditRoot {
            payload,
            collection: req_collection,
            ..
        } => {
            let collection = match req_collection {
                Some(c) => Some(parse_pubkey(c).context("request collection")?),
                None => collection,
            };
            let data = serde_json::to_vec(payload).context("encode attestation payload")?;
            Ok(WritePlan {
                data,
                name: truncate(&format!("Covenant root {}", payload.release_target)),
                uri: String::new(),
                collection,
            })
        }
        SignerRequest::RegisterIdentity {
            agent_label,
            agent_pubkey,
            ..
        } => {
            let data = serde_json::to_vec(&serde_json::json!({
                "schema": IDENTITY_SCHEMA,
                "agentLabel": agent_label,
                "agentPubkey": agent_pubkey,
            }))
            .context("encode identity payload")?;
            Ok(WritePlan {
                data,
                name: truncate(&format!("Covenant agent {agent_label}")),
                uri: String::new(),
                collection,
            })
        }
    }
}

fn collection_from_env() -> Result<Option<Pubkey>> {
    match std::env::var("COVENANT_METAPLEX_COLLECTION") {
        Ok(c) if !c.is_empty() => Ok(Some(parse_pubkey(&c).context("COVENANT_METAPLEX_COLLECTION")?)),
        _ => Ok(None),
    }
}

fn parse_pubkey(s: &str) -> Result<Pubkey> {
    Pubkey::from_str(s).map_err(|e| anyhow!("invalid pubkey {s}: {e}"))
}

fn truncate(s: &str) -> String {
    s.chars().take(NAME_MAX).collect()
}

fn load_keypair(path: &str) -> Result<Keypair> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read keypair {path}"))?;
    let bytes: Vec<u8> =
        serde_json::from_str(raw.trim()).context("keypair file must be a JSON byte array")?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow!("load keypair: {e}"))
}

async fn rpc(http: &reqwest::Client, url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "covenant-metaplex-signer",
        "method": method,
        "params": params,
    });
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("rpc {method}"))?;
    let value: serde_json::Value = resp.json().await.with_context(|| format!("rpc {method} decode"))?;
    if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
        bail!("rpc {method} error: {err}");
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("rpc {method}: no result"))
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
