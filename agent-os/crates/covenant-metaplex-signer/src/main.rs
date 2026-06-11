//! Standalone Metaplex minting-key signer (sidecar to covenantd).
//!
//! One-shot, stdin -> stdout. The daemon spawns this process per write,
//! pipes a [`covenant_metaplex::SignerRequest`] as JSON to stdin, and
//! reads a [`covenant_metaplex::SignerResponse`] from stdout. The minting
//! key never enters the daemon's address space, and the solana-sdk 3.x +
//! mpl-core / mpl-agent dep tree never enters the daemon's build.
//!
//! Two write actions, both in a single confirmed transaction:
//!
//! - attest-audit-root: mint a fresh MPL Core asset and write the
//!   Covenant attestation into its AppData external plugin (JSON schema,
//!   minting key as data authority). DAS indexes the JSON automatically.
//! - register-identity: mint a fresh MPL Core asset and bind a Covenant
//!   agent identity to it through the MPL Agent identity registry
//!   (register_identity_v1), which CPIs into MPL Core to anchor the
//!   tamper-evident PDA<->asset binding. This puts the agent in
//!   Metaplex's on-chain agent registry.
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
use covenant_metaplex::config::{
    ALLOWED_PROGRAM_IDS, MPL_AGENT_IDENTITY_PROGRAM_ID, MPL_CORE_PROGRAM_ID,
};
use covenant_metaplex::{SignerRequest, SignerResponse};
use mpl_agent_identity::instructions::RegisterIdentityV1Builder;
use mpl_core::instructions::{CreateV2Builder, WriteExternalPluginAdapterDataV1Builder};
use mpl_core::types::{
    AppDataInitInfo, DataState, ExternalPluginAdapterInitInfo, ExternalPluginAdapterKey,
    ExternalPluginAdapterSchema, PluginAuthority,
};
use solana_program::instruction::Instruction;
use solana_program::pubkey::Pubkey;
use solana_sdk::hash::Hash;
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default per-action ceiling (~0.02 SOL) when none is configured. A
/// single Core asset write is well under this; the cap exists to refuse a
/// pathologically large payload draining the key.
const DEFAULT_CAP_LAMPORTS: u64 = 20_000_000;
/// Coarse cost estimate parts (lamports): asset account rent + the Core
/// protocol fee, plus rent for stored bytes (~2 years of byte-rent), plus
/// a fixed allowance for the identity registry PDA.
const ASSET_BASE_LAMPORTS: u64 = 2_900_000;
const CORE_PROTOCOL_FEE_LAMPORTS: u64 = 1_500_000;
const LAMPORTS_PER_DATA_BYTE: u64 = 6_960;
const IDENTITY_PDA_LAMPORTS: u64 = 1_500_000;
/// Seed prefix for the MPL Agent identity PDA: ["agent_identity", asset].
const AGENT_IDENTITY_SEED: &[u8] = b"agent_identity";
/// On-chain MPL Core asset name cap we stay under.
const NAME_MAX: usize = 32;

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
    // Program-id pins: refuse to run if a linked program id is not one
    // Covenant trusts. Closes the only path by which a dependency swap
    // could redirect a signed instruction.
    assert_pinned("mpl-core", &mpl_core::ID.to_string(), MPL_CORE_PROGRAM_ID)?;
    assert_pinned(
        "mpl-agent-identity",
        &mpl_agent_identity::ID.to_string(),
        MPL_AGENT_IDENTITY_PROGRAM_ID,
    )?;

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

    let built = build(&request, &payer)?;

    // Per-action cost guard before we touch the network.
    if built.est_lamports > cap {
        bail!(
            "estimated cost {} lamports exceeds the per-action cap {cap}; \
             raise COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS or shrink the payload",
            built.est_lamports
        );
    }

    let http = reqwest::Client::new();
    let recent = latest_blockhash(&http, &rpc_url).await?;
    let tx = Transaction::new_signed_with_payer(
        &built.instructions,
        Some(&payer.pubkey()),
        &[&payer, &built.asset],
        recent,
    );

    let signature = send_transaction(&http, &rpc_url, &tx).await?;
    // Confirm before reporting success: sendTransaction returns as soon
    // as the tx is submitted, so an optimistic return could claim a write
    // that never landed. The daemon only sees success once it is confirmed.
    confirm_signature(&http, &rpc_url, &signature).await?;

    Ok(SignerResponse {
        signature,
        asset: built.asset.pubkey().to_string(),
        cluster,
    })
}

/// A ready-to-sign transaction: the instructions, the fresh asset keypair
/// that must co-sign the create, and the estimated lamport cost.
struct BuiltTx {
    instructions: Vec<Instruction>,
    asset: Keypair,
    est_lamports: u64,
}

fn build(request: &SignerRequest, payer: &Keypair) -> Result<BuiltTx> {
    match request {
        SignerRequest::AttestAuditRoot {
            payload,
            collection: req_collection,
            asset,
        } => {
            if asset.is_some() {
                bail!("append-to-existing-asset is not supported yet; omit `asset`");
            }
            covenant_metaplex::validate_root_hash_hex(&payload.root_hash_hex)
                .map_err(|e| anyhow!("{e}"))?;
            let collection = match req_collection {
                Some(c) => Some(parse_pubkey(c).context("request collection")?),
                None => collection_from_env()?,
            };
            let data = serde_json::to_vec(payload).context("encode attestation payload")?;
            let asset = Keypair::new();
            let create_ix = CreateV2Builder::new()
                .asset(asset.pubkey())
                .payer(payer.pubkey())
                .collection(collection)
                .data_state(DataState::AccountState)
                .name(truncate(&format!(
                    "Covenant root {}",
                    payload.release_target
                )))
                .uri(String::new())
                .external_plugin_adapters(vec![ExternalPluginAdapterInitInfo::AppData(
                    AppDataInitInfo {
                        data_authority: PluginAuthority::Address {
                            address: payer.pubkey(),
                        },
                        init_plugin_authority: Some(PluginAuthority::Address {
                            address: payer.pubkey(),
                        }),
                        schema: Some(ExternalPluginAdapterSchema::Json),
                    },
                )])
                .instruction();
            let write_ix = WriteExternalPluginAdapterDataV1Builder::new()
                .asset(asset.pubkey())
                // The asset lives in this collection (when one is pinned);
                // Core requires the collection account on every instruction
                // touching such an asset, or it fails with MissingCollection.
                .collection(collection)
                .payer(payer.pubkey())
                .authority(Some(payer.pubkey()))
                .key(ExternalPluginAdapterKey::AppData(
                    PluginAuthority::Address {
                        address: payer.pubkey(),
                    },
                ))
                .data(data.clone())
                .instruction();
            let est = ASSET_BASE_LAMPORTS
                + CORE_PROTOCOL_FEE_LAMPORTS
                + LAMPORTS_PER_DATA_BYTE * data.len() as u64;
            Ok(BuiltTx {
                instructions: vec![create_ix, write_ix],
                asset,
                est_lamports: est,
            })
        }
        SignerRequest::RegisterIdentity {
            agent_label,
            agent_pubkey,
            asset,
            registration_uri,
        } => {
            if asset.is_some() {
                bail!("binding an existing asset is not supported yet; omit `asset`");
            }
            let collection = collection_from_env()?;
            let asset = Keypair::new();
            // Prefer a fetchable ERC-8004 registration document; fall back
            // to the namespaced identifier form pointing at the agent's
            // Covenant pubkey identity.
            let uri = match registration_uri {
                Some(u) => {
                    covenant_metaplex::validate_registration_uri(u).map_err(|e| anyhow!("{e}"))?;
                    u.clone()
                }
                None => format!("covenant://agent/{agent_pubkey}"),
            };
            let create_ix = CreateV2Builder::new()
                .asset(asset.pubkey())
                .payer(payer.pubkey())
                .collection(collection)
                .data_state(DataState::AccountState)
                .name(truncate(&format!("Covenant agent {agent_label}")))
                .uri(uri.clone())
                .instruction();
            // register_identity_v1 CPIs into MPL Core to anchor the binding;
            // the agent_identity PDA is ["agent_identity", asset].
            let (agent_identity, _bump) = Pubkey::find_program_address(
                &[AGENT_IDENTITY_SEED, asset.pubkey().as_ref()],
                &mpl_agent_identity::ID,
            );
            let register_ix = RegisterIdentityV1Builder::new()
                .agent_identity(agent_identity)
                .asset(asset.pubkey())
                .collection(collection)
                .payer(payer.pubkey())
                .agent_registration_uri(uri.clone())
                .instruction();
            let est = ASSET_BASE_LAMPORTS
                + CORE_PROTOCOL_FEE_LAMPORTS
                + IDENTITY_PDA_LAMPORTS
                + LAMPORTS_PER_DATA_BYTE * uri.len() as u64;
            Ok(BuiltTx {
                instructions: vec![create_ix, register_ix],
                asset,
                est_lamports: est,
            })
        }
    }
}

fn assert_pinned(name: &str, linked: &str, pinned: &str) -> Result<()> {
    if linked != pinned || !ALLOWED_PROGRAM_IDS.contains(&linked) {
        bail!("{name} program id {linked} is not the pinned {pinned}");
    }
    Ok(())
}

fn collection_from_env() -> Result<Option<Pubkey>> {
    parse_collection(std::env::var("COVENANT_METAPLEX_COLLECTION").ok())
}

/// Resolve the optional collection pin from its raw env value. Split from the
/// env read so the parse, empty-as-unset, and error arms are unit-testable
/// without mutating process-global state.
fn parse_collection(raw: Option<String>) -> Result<Option<Pubkey>> {
    match raw {
        Some(c) if !c.is_empty() => Ok(Some(
            parse_pubkey(&c).context("COVENANT_METAPLEX_COLLECTION")?,
        )),
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

async fn rpc(
    http: &reqwest::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
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
/// window elapses. An on-chain error returns `Err` with the program
/// error; a timeout returns `Err` naming the signature so the operator
/// can check it. Recent blockhash validity (~60-90s) bounds the wait.
async fn confirm_signature(http: &reqwest::Client, url: &str, signature: &str) -> Result<()> {
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
            return Ok(());
        }
    }
    bail!("transaction {signature} not confirmed within timeout; check it on-chain")
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_metaplex::AttestationPayload;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "covenant-metaplex-signer-{}-{tag}.json",
            std::process::id()
        ))
    }

    #[test]
    fn assert_pinned_accepts_a_pinned_allowlisted_id() {
        assert_pinned("mpl-core", MPL_CORE_PROGRAM_ID, MPL_CORE_PROGRAM_ID).unwrap();
    }

    #[test]
    fn assert_pinned_rejects_a_mismatched_id() {
        let err = assert_pinned(
            "mpl-core",
            &Pubkey::new_unique().to_string(),
            MPL_CORE_PROGRAM_ID,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("is not the pinned"), "{err}");
    }

    #[test]
    fn assert_pinned_rejects_a_matching_but_unallowlisted_id() {
        // linked == pinned, so the equality check passes; an id absent from
        // ALLOWED_PROGRAM_IDS must still be refused.
        let rogue = Pubkey::new_unique().to_string();
        let err = assert_pinned("mpl-core", &rogue, &rogue)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not the pinned"), "{err}");
    }

    #[test]
    fn parse_pubkey_round_trips_a_valid_key() {
        let key = Pubkey::new_unique();
        assert_eq!(parse_pubkey(&key.to_string()).unwrap(), key);
    }

    #[test]
    fn parse_pubkey_rejects_garbage() {
        let err = parse_pubkey("not-base58!!").unwrap_err().to_string();
        assert!(err.contains("invalid pubkey"), "{err}");
    }

    #[test]
    fn truncate_caps_at_name_max_on_a_char_boundary() {
        assert_eq!(truncate(&"a".repeat(40)).chars().count(), NAME_MAX);
        assert_eq!(truncate("short"), "short");
        // multibyte chars must cap at NAME_MAX chars without panicking on a byte split.
        assert_eq!(truncate(&"é".repeat(40)).chars().count(), NAME_MAX);
    }

    #[test]
    fn load_keypair_round_trips_a_written_key() {
        let key = Keypair::new();
        let path = temp_path("valid");
        std::fs::write(
            &path,
            serde_json::to_string(&key.to_bytes().to_vec()).unwrap(),
        )
        .unwrap();
        let loaded = load_keypair(path.to_str().unwrap()).unwrap();
        assert_eq!(loaded.pubkey(), key.pubkey());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_keypair_reports_a_missing_file() {
        let path = temp_path("absent");
        let _ = std::fs::remove_file(&path);
        let err = load_keypair(path.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("read keypair"), "{err}");
    }

    #[test]
    fn load_keypair_rejects_non_array_json() {
        let path = temp_path("badjson");
        std::fs::write(&path, "not json").unwrap();
        let err = load_keypair(path.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("keypair file must be a JSON byte array"),
            "{err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_keypair_rejects_a_wrong_length_byte_array() {
        let path = temp_path("short");
        std::fs::write(&path, "[1,2,3,4,5,6,7,8,9,10]").unwrap();
        let err = load_keypair(path.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("load keypair"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    fn valid_payload() -> AttestationPayload {
        AttestationPayload::new("a".repeat(64), "v0.1.0", "covenant", "audit", 1_700_000_000)
    }

    // BuiltTx is intentionally not Debug (it carries a Keypair), so .unwrap_err()
    // is unavailable; surface the error message without printing the Ok value.
    fn build_err(req: &SignerRequest) -> String {
        match build(req, &Keypair::new()) {
            Ok(_) => panic!("expected build() to reject the request"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn build_rejects_attest_on_an_existing_asset() {
        let req = SignerRequest::AttestAuditRoot {
            payload: valid_payload(),
            collection: None,
            asset: Some(Pubkey::new_unique().to_string()),
        };
        let err = build_err(&req);
        assert!(err.contains("not supported"), "{err}");
    }

    #[test]
    fn build_rejects_register_on_an_existing_asset() {
        let req = SignerRequest::RegisterIdentity {
            agent_label: "agent".into(),
            agent_pubkey: Pubkey::new_unique().to_string(),
            asset: Some(Pubkey::new_unique().to_string()),
            registration_uri: None,
        };
        let err = build_err(&req);
        assert!(err.contains("not supported"), "{err}");
    }

    #[test]
    fn build_rejects_attest_with_a_malformed_root_hash() {
        let req = SignerRequest::AttestAuditRoot {
            payload: AttestationPayload::new("zz".repeat(32), "v0.1.0", "covenant", "audit", 1),
            collection: None,
            asset: None,
        };
        let err = build_err(&req);
        assert!(err.contains("hex"), "{err}");
    }

    #[test]
    fn build_attest_emits_two_instructions_and_a_payload_sized_cost() {
        let payload = valid_payload();
        let req = SignerRequest::AttestAuditRoot {
            payload: payload.clone(),
            collection: None,
            asset: None,
        };
        let built = build(&req, &Keypair::new()).unwrap();
        assert_eq!(built.instructions.len(), 2);
        let data_len = serde_json::to_vec(&payload).unwrap().len() as u64;
        assert_eq!(
            built.est_lamports,
            ASSET_BASE_LAMPORTS + CORE_PROTOCOL_FEE_LAMPORTS + LAMPORTS_PER_DATA_BYTE * data_len
        );
    }

    #[test]
    fn build_attest_rejects_a_malformed_request_collection() {
        // The request-collection arm parses the caller-supplied pin before the
        // env fallback; a bad value is a hard error tagged "request collection"
        // so it is distinguishable from a bad COVENANT_METAPLEX_COLLECTION.
        let req = SignerRequest::AttestAuditRoot {
            payload: valid_payload(),
            collection: Some("not-base58!!".into()),
            asset: None,
        };
        let err = build_err(&req);
        assert!(err.contains("request collection"), "{err}");
    }

    #[test]
    fn build_attest_accepts_a_valid_request_collection() {
        // A valid caller-supplied collection parses and builds the same two
        // instructions at the same payload-sized cost as the no-collection
        // case: a collection account does not perturb the cost estimate.
        let payload = valid_payload();
        let req = SignerRequest::AttestAuditRoot {
            payload: payload.clone(),
            collection: Some(Pubkey::new_unique().to_string()),
            asset: None,
        };
        let built = build(&req, &Keypair::new()).unwrap();
        assert_eq!(built.instructions.len(), 2);
        let data_len = serde_json::to_vec(&payload).unwrap().len() as u64;
        assert_eq!(
            built.est_lamports,
            ASSET_BASE_LAMPORTS + CORE_PROTOCOL_FEE_LAMPORTS + LAMPORTS_PER_DATA_BYTE * data_len
        );
    }

    #[test]
    fn build_register_falls_back_to_the_covenant_uri_and_prices_it() {
        let agent_pubkey = Pubkey::new_unique().to_string();
        let req = SignerRequest::RegisterIdentity {
            agent_label: "agent".into(),
            agent_pubkey: agent_pubkey.clone(),
            asset: None,
            registration_uri: None,
        };
        let built = build(&req, &Keypair::new()).unwrap();
        assert_eq!(built.instructions.len(), 2);
        let uri_len = format!("covenant://agent/{agent_pubkey}").len() as u64;
        assert_eq!(
            built.est_lamports,
            ASSET_BASE_LAMPORTS
                + CORE_PROTOCOL_FEE_LAMPORTS
                + IDENTITY_PDA_LAMPORTS
                + LAMPORTS_PER_DATA_BYTE * uri_len
        );
    }

    #[test]
    fn build_register_rejects_a_plain_http_uri() {
        let req = SignerRequest::RegisterIdentity {
            agent_label: "agent".into(),
            agent_pubkey: Pubkey::new_unique().to_string(),
            asset: None,
            registration_uri: Some("http://example.org/a.json".into()),
        };
        let err = build_err(&req);
        assert!(err.contains("registrationUri"), "{err}");
    }

    #[test]
    fn build_register_accepts_an_https_uri_and_prices_it() {
        let uri = "https://example.org/agent.json";
        let req = SignerRequest::RegisterIdentity {
            agent_label: "agent".into(),
            agent_pubkey: Pubkey::new_unique().to_string(),
            asset: None,
            registration_uri: Some(uri.into()),
        };
        let built = build(&req, &Keypair::new()).unwrap();
        assert_eq!(built.instructions.len(), 2);
        assert_eq!(
            built.est_lamports,
            ASSET_BASE_LAMPORTS
                + CORE_PROTOCOL_FEE_LAMPORTS
                + IDENTITY_PDA_LAMPORTS
                + LAMPORTS_PER_DATA_BYTE * uri.len() as u64
        );
    }

    #[test]
    fn build_register_binds_the_agent_identity_pda_to_the_created_asset() {
        // The register instruction must target the PDA ["agent_identity", asset]
        // under the MPL Agent Identity program, and that asset must be the one
        // the create instruction mints. The seed is hardcoded here so a change
        // to AGENT_IDENTITY_SEED is caught, not mirrored.
        let req = SignerRequest::RegisterIdentity {
            agent_label: "agent".into(),
            agent_pubkey: Pubkey::new_unique().to_string(),
            asset: None,
            registration_uri: None,
        };
        let built = build(&req, &Keypair::new()).unwrap();
        let asset = built.asset.pubkey();
        let (expected_pda, _) = Pubkey::find_program_address(
            &[b"agent_identity", asset.as_ref()],
            &mpl_agent_identity::ID,
        );
        let register_ix = &built.instructions[1];
        assert!(
            register_ix
                .accounts
                .iter()
                .any(|a| a.pubkey == expected_pda),
            "register instruction must reference the derived agent_identity PDA"
        );
        let create_ix = &built.instructions[0];
        assert!(
            create_ix.accounts.iter().any(|a| a.pubkey == asset),
            "create and register must reference the same minted asset"
        );
    }

    #[test]
    fn parse_collection_parses_a_pubkey_and_treats_unset_or_empty_as_none() {
        assert_eq!(parse_collection(None).unwrap(), None, "unset -> no pin");
        assert_eq!(
            parse_collection(Some(String::new())).unwrap(),
            None,
            "empty -> treated as unset, not an error"
        );
        let pin = Pubkey::new_unique();
        assert_eq!(parse_collection(Some(pin.to_string())).unwrap(), Some(pin));
        let err = parse_collection(Some("not-base58!!".into()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("COVENANT_METAPLEX_COLLECTION"),
            "a bad pubkey is a hard error tagged with the env var: {err}"
        );
    }
}
