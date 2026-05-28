//! Covenant command-line client for the local daemon.
//!
//! ```text
//!   covenant ping [--json]
//!   covenant intent [--json] [--stream] <text>
//!   covenant memory recent [--tier <working|episodic|longterm>] [--limit N] [--json] [--stream]
//!   covenant memory search <query> [--tier <working|episodic|longterm>] [--limit N] [--min-relevance F] [--json]
//!   covenant memory purge [--tier <T>] (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant memory compact --reason <text> [--apply] [--detach-stale-parents] [--delete-working-before-ms <M>|--delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M>|--delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M>|--mark-longterm-stale-older-than-ms <D>] [--json]
//!   covenant memory plan-compaction --reason <text> [--detach-stale-parents] [--delete-working-before-ms <M>|--delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M>|--delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M>|--mark-longterm-stale-older-than-ms <D>] [--json]
//!   covenant memory plan-receipt-backfill [--limit N] [--json]
//!   covenant memory backfill-receipt-correlation [--dry-run] [--json]   (--scope-pubkey reserved, not yet supported)
//!   covenant memory repair detach-parent <id> --reason <text> [--expected-parent <uuid>] [--apply]
//!   covenant memory repair delete <id> --reason <text> [--apply]
//!   covenant memory repair backfill-provenance <id> --reason <text> --provenance <json> [--apply]
//!   covenant capabilities recent [--limit N] [--json]
//!   covenant capabilities grant <action> [--scope <json>] [--expires-at <ms>] [--json]
//!   covenant capabilities revoke <signature-b58> [--json]
//!   covenant capabilities purge (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant receipts recent [--limit N] [--since-ms <epoch_ms>] [--json]
//!   covenant chain status [--json]
//!   covenant chain flush-receipts [--limit N] [--json]
//!   covenant chain receipt-batches [--limit N] [--json]
//!   covenant chain register-agent --program-id <BASE58> --agent-key <BASE58> --metadata-hash <HEX64> --capability-hash <HEX64> [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]
//!   covenant chain stake --program-id <BASE58> --agent-key <BASE58> --owner-covnt <BASE58> --stake-vault <BASE58> --amount <U64> --lock-until <U64> [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]
//!   covenant chain buy-credits --program-id <BASE58> --owner-covnt <BASE58> --treasury <BASE58> --amount-covnt <U64> [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]
//!   covenant chain initialize --program-id <BASE58> --covnt-mint <BASE58> --treasury <BASE58> --slash-authority <BASE58> --credits-per-covnt <U64> [--min-stake-lock <U64>] [COMMON]
//!   covenant chain open-credit-account --program-id <BASE58> [COMMON]
//!   covenant chain unstake --program-id <BASE58> --agent-key <BASE58> --stake-vault <BASE58> --owner-covnt <BASE58> [COMMON]
//!   covenant chain close-position --program-id <BASE58> --agent-key <BASE58> [COMMON]
//!   covenant chain migrate-config --program-id <BASE58> --min-stake-lock <U64> [COMMON]
//!   covenant chain set-min-stake-lock --program-id <BASE58> --value <U64> [COMMON]
//!   covenant chain set-credits-per-covnt --program-id <BASE58> --value <U64> [COMMON]
//!   covenant chain update-authority --program-id <BASE58> --new <BASE58> [COMMON]
//!   covenant chain update-slash-authority --program-id <BASE58> --new <BASE58> [COMMON]
//!   covenant chain update-treasury --program-id <BASE58> --treasury <BASE58> [COMMON]
//!     COMMON = [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]
//!   covenant settlement backfill-receipts [--dry-run] [--json]   (--scope-pubkey reserved, not yet supported)
//!   covenant verify [--window N] [--json]
//!   covenant ignore check [--json] <text>
//!   covenant tools list [--json]
//!   covenant tools call <name> [--args <json>] [--json]
//!   covenant audit recent [--limit N] [--since-ms <epoch_ms>] [--json] [--stream]
//!   covenant audit verify [--json]
//!   covenant audit purge (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant a2a status [--limit N] [--min-lease-age-ms N] [--deadline-within-ms N] [--state queued|in_flight] [--json]
//!   covenant a2a requeue <task-id> --reason <text> --duplicate-risk <idempotent|operator-accepted> [--lease-id <uuid>]
//!   covenant a2a force-error <task-id> --reason <text> --message <text> [--lease-id <uuid>]
//!   covenant a2a retry-stale [--enable] [--min-lease-age-ms N] [--max-attempts N] [--max-requeues N] [--scan-limit N] [--json]
//!   covenant a2a compact [--json]
//!   covenant peers purge (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant peers rotate [--json]
//!   covenant peers list [--limit N] [--prefix <pubkey-b58-prefix>] [--json]
//!   covenant peers revoke <token-prefix> [--force] [--limit-matches <N>] [--json]
//!   covenant intents resume <intent-id> [--json]
//!   covenant intents resume latest [--json]
//! ```

#![deny(unsafe_code)]

use anyhow::{bail, Context, Result};
use covenant_a2a::{
    A2AAutoRetryPolicy, A2AAutoRetryReport, A2ADuplicateRisk, A2ARepairCommand, A2ARepairRequest,
    A2ATaskQueueEntry, A2ATaskQueueState, A2ATaskResult,
};
use covenant_audit::{AuditEvent, AuditIntegrityReport, AuditKind};
use covenant_ipc::{
    read_frame, read_response_or_stream, write_frame, ChainStatus, ReceiptBatchSummary, Request,
    Response, ResponseOrStream, VerifyCheck, VerifyDrift,
};
use covenant_mcp::ToolSpec;
use covenant_memory::memory_receipt_backfill_plan_json;
use covenant_peer_auth::{PeerStatusFilter, PeerSummary, RevokeOutcome};
use covenant_permissions::SignedCapability;
use covenant_types::{
    MemoryCompactionOutcome, MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRecord,
    MemoryRepairCommand, MemoryRepairMode, MemoryRepairRequest, MemoryTier, ResourceKind,
    SettlementReceipt,
};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

fn covenant_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant"))
}

#[derive(Debug, thiserror::Error)]
enum KeypairLoadError {
    #[error("cannot resolve default operator keypair path: HOME is not set (pass --keypair PATH or export HOME)")]
    HomeUnresolved,
    #[error("operator keypair file not found at {path}")]
    MissingFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("operator keypair file at {path} cannot be read: permission denied")]
    PermissionDenied {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("operator keypair file at {path} cannot be read")]
    NotReadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("operator keypair file at {path} is not a JSON array of bytes")]
    MalformedJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("operator keypair file at {path} has {actual} bytes, expected exactly 64 (Solana keypair convention)")]
    WrongByteCount { path: PathBuf, actual: usize },
    #[error("operator keypair file at {path} is not a valid Solana ed25519 keypair: {reason}")]
    InvalidKeyMaterial { path: PathBuf, reason: String },
}

fn compute_default_keypair_path(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join(".config").join("solana").join("id.json")
}

fn resolve_operator_keypair_path(provided: Option<PathBuf>) -> Result<PathBuf, KeypairLoadError> {
    if let Some(p) = provided {
        return Ok(p);
    }
    let home = std::env::var("HOME").map_err(|_| KeypairLoadError::HomeUnresolved)?;
    Ok(compute_default_keypair_path(home))
}

fn classify_keypair_read_error(path: PathBuf, source: std::io::Error) -> KeypairLoadError {
    match source.kind() {
        std::io::ErrorKind::NotFound => KeypairLoadError::MissingFile { path, source },
        std::io::ErrorKind::PermissionDenied => KeypairLoadError::PermissionDenied { path, source },
        _ => KeypairLoadError::NotReadable { path, source },
    }
}

#[derive(Debug, thiserror::Error)]
enum ClusterResolveError {
    #[error("unknown Solana cluster {name:?}; accepted values are devnet, localnet, mainnet, mainnet-beta")]
    UnknownCluster { name: String },
    #[error("--rpc-url was provided but the value is empty")]
    EmptyRpcUrl,
}

// Cluster -> default RPC URL mapping mirrors packages/config/networks.mjs
// so the CLI and the landing/UI route the same operator-supplied cluster
// names to the same endpoints. Default cluster is devnet to align with
// the devnet program ID pinned in docs/internal/status.md row "On-chain
// settlement".
fn resolve_solana_rpc_url(
    cluster: Option<&str>,
    rpc_url_override: Option<&str>,
) -> Result<String, ClusterResolveError> {
    if let Some(url) = rpc_url_override {
        if url.is_empty() {
            return Err(ClusterResolveError::EmptyRpcUrl);
        }
        return Ok(url.to_string());
    }
    let name = cluster.unwrap_or("devnet");
    let url = match name {
        "devnet" => "https://api.devnet.solana.com",
        "localnet" => "http://127.0.0.1:8899",
        "mainnet" | "mainnet-beta" => "https://api.mainnet-beta.solana.com",
        other => {
            return Err(ClusterResolveError::UnknownCluster {
                name: other.to_string(),
            })
        }
    };
    Ok(url.to_string())
}

// Settlement-program PDA seed bytes mirror agent-os/programs/settlement/
// src/lib.rs. Wrapping each find_program_address call lets the verb
// sub-slices use the canonical seeds without re-spelling the byte
// literals at every call site, where a one-character typo would derive a
// deterministic-but-wrong PDA and surface only as AccountNotInitialized
// from on-chain simulation.
fn settlement_config_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"config"], program_id)
}

fn settlement_agent_pda(program_id: &Pubkey, agent_key: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"agent", agent_key.as_ref()], program_id)
}

fn settlement_credits_pda(program_id: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"credits", owner.as_ref()], program_id)
}

// Seeds mirror agent-os/programs/settlement/src/lib.rs:547 exactly:
//   [b"stake", agent.agent_key.as_ref(), owner.key().as_ref()]
// A one-byte typo in either tag literal or a wrong slice (e.g. the
// agent PDA bytes instead of agent_key) derives a deterministic-but-
// wrong PDA. The on-chain dispatcher then rejects the transaction
// with ConstraintSeeds at submission time, which surfaces as an
// opaque RPC error rather than a local-test failure — so the helper
// is unit-pinned against the literal seed bytes.
fn settlement_stake_position_pda(
    program_id: &Pubkey,
    agent_key: &Pubkey,
    owner: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"stake", agent_key.as_ref(), owner.as_ref()], program_id)
}

// Field order MUST mirror agent-os/programs/settlement/src/lib.rs:877-882
// (agent_key, metadata_hash, capability_hash). Borsh serializes in
// struct declaration order; following the alphabetical data_keys order
// in packages/sdk/compatibility/instructions.v1.json (which is a sorted
// set used by validate-sdk-compatibility, not a serialization spec)
// would silently swap capability_hash and metadata_hash on the wire,
// and the on-chain account would deserialize with the operator's
// capability_hash parsed as metadata_hash and vice versa.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Clone, Debug, PartialEq, Eq)]
struct RegisterAgentArgs {
    agent_key: [u8; 32],
    metadata_hash: [u8; 32],
    capability_hash: [u8; 32],
}

fn serialize_register_agent_args(args: &RegisterAgentArgs) -> Vec<u8> {
    borsh::to_vec(args).expect("borsh serialization of fixed [u8;32] fields is infallible")
}

// Field order MUST mirror agent-os/programs/settlement/src/lib.rs:137
// (amount, lock_until). Borsh serializes in struct-declaration order;
// a swap would silently make the on-chain program lock for amount
// epochs and stake lock_until lamports — both numerically valid u64s,
// so neither side errors at the wire layer.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Clone, Debug, PartialEq, Eq)]
struct StakeArgs {
    amount: u64,
    lock_until: u64,
}

fn serialize_stake_args(args: &StakeArgs) -> Vec<u8> {
    borsh::to_vec(args).expect("borsh serialization of two u64 fields is infallible")
}

// Mirrors agent-os/programs/settlement/src/lib.rs:91
// `buy_credits(ctx, amount_covnt: u64)`. The on-chain handler
// reads a single u64 argument; a struct grow without an on-chain
// mirror would silently truncate at deserialize-time on chain.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Clone, Debug, PartialEq, Eq)]
struct BuyCreditsArgs {
    amount_covnt: u64,
}

fn serialize_buy_credits_args(args: &BuyCreditsArgs) -> Vec<u8> {
    borsh::to_vec(args).expect("borsh serialization of one u64 field is infallible")
}

// Account ordering and signer/writable flags mirror the on-chain
// RegisterAgent struct at agent-os/programs/settlement/src/lib.rs:429-448:
//   config           — PDA, read-only
//   agent            — PDA, writable, NOT signer (init-by-operator)
//   operator         — signer, writable (fee payer)
//   system_program   — read-only
// Anchor's dispatcher routes accounts positionally; any reorder silently
// remaps roles and the transaction would fail with a confusing
// ConstraintSeeds / AccountNotInitialized error.
fn build_register_agent_instruction(
    program_id: &Pubkey,
    operator_pubkey: &Pubkey,
    args: &RegisterAgentArgs,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};

    let (config_pda, _) = settlement_config_pda(program_id);
    let (agent_pda, _) = settlement_agent_pda(program_id, &Pubkey::new_from_array(args.agent_key));

    let mut data = Vec::with_capacity(8 + 96);
    data.extend_from_slice(&compute_anchor_global_discriminator("register_agent"));
    data.extend_from_slice(&serialize_register_agent_args(args));

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(agent_pda, false),
            AccountMeta::new(*operator_pubkey, true),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
        data,
    }
}

// The canonical SPL Token (legacy, v3) program ID. The on-chain
// Stake / BuyCredits instructions declare Program<Token> which only
// accepts this address; the Token-2022 program at
// TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb is a different
// program and a substitution would be rejected with InvalidProgramId.
const SPL_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Account ordering and signer/writable flags mirror the on-chain
// Stake struct at agent-os/programs/settlement/src/lib.rs:531-567:
//   config         — PDA, read-only
//   agent          — PDA, writable, !signer
//   position       — PDA, writable, !signer (#[account(init, ...)])
//   owner          — signer, writable (fee payer)
//   owner_covnt    — writable, !signer (source of the stake transfer)
//   stake_vault    — writable, !signer (destination of the stake transfer)
//   token_program  — read-only, legacy SPL Token
//   system_program — read-only
// Anchor's dispatcher reads accounts positionally, so a single-slot
// reorder silently remaps roles and either fails ConstraintSeeds
// (PDAs) or Unauthorized (token-owner checks).
fn build_stake_instruction(
    program_id: &Pubkey,
    operator: &Pubkey,
    agent_key: &Pubkey,
    owner_covnt: &Pubkey,
    stake_vault: &Pubkey,
    covnt_mint: &Pubkey,
    args: &StakeArgs,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};

    let (config_pda, _) = settlement_config_pda(program_id);
    let (agent_pda, _) = settlement_agent_pda(program_id, agent_key);
    let (position_pda, _) = settlement_stake_position_pda(program_id, agent_key, operator);

    let mut data = Vec::with_capacity(8 + 16);
    data.extend_from_slice(&compute_anchor_global_discriminator("stake"));
    data.extend_from_slice(&serialize_stake_args(args));

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(agent_pda, false),
            AccountMeta::new(position_pda, false),
            AccountMeta::new(*operator, true),
            AccountMeta::new(*owner_covnt, false),
            AccountMeta::new(*stake_vault, false),
            AccountMeta::new_readonly(*covnt_mint, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
        data,
    }
}

// Account ordering and signer/writable flags mirror the on-chain
// BuyCredits struct at agent-os/programs/settlement/src/lib.rs:483-503:
//   config        — PDA, read-only (has_one = treasury)
//   credits       — PDA, writable, !signer (has_one = owner)
//   owner         — signer, writable (fee payer + transfer authority)
//   owner_covnt   — writable, !signer (source of the COVNT transfer)
//   treasury      — writable, !signer (destination of the COVNT transfer;
//                   the operator must supply config.treasury verbatim or
//                   the has_one check fails)
//   token_program — read-only, legacy SPL Token
// No system_program is referenced because BuyCredits does not init
// any new account (credits PDA is initialized by initialize_credits).
fn build_buy_credits_instruction(
    program_id: &Pubkey,
    operator: &Pubkey,
    owner_covnt: &Pubkey,
    treasury: &Pubkey,
    covnt_mint: &Pubkey,
    args: &BuyCreditsArgs,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};

    let (config_pda, _) = settlement_config_pda(program_id);
    let (credits_pda, _) = settlement_credits_pda(program_id, operator);

    let mut data = Vec::with_capacity(8 + 8);
    data.extend_from_slice(&compute_anchor_global_discriminator("buy_credits"));
    data.extend_from_slice(&serialize_buy_credits_args(args));

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(credits_pda, false),
            AccountMeta::new(*operator, true),
            AccountMeta::new(*owner_covnt, false),
            AccountMeta::new(*treasury, false),
            AccountMeta::new_readonly(*covnt_mint, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data,
    }
}

// First 8 bytes of sha256("global:<method>") — Anchor's instruction
// discriminator scheme. The "global:" namespace is the only one Anchor's
// macro-generated dispatcher accepts for #[program] mod methods; dropping
// it would silently produce bytes that never route on chain.
fn compute_anchor_global_discriminator(method: &str) -> [u8; 8] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"global:");
    hasher.update(method.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

fn load_operator_keypair(provided: Option<PathBuf>) -> Result<Keypair, KeypairLoadError> {
    let path = resolve_operator_keypair_path(provided)?;
    let raw = std::fs::read(&path).map_err(|e| classify_keypair_read_error(path.clone(), e))?;
    let bytes: Vec<u8> =
        serde_json::from_slice(&raw).map_err(|source| KeypairLoadError::MalformedJson {
            path: path.clone(),
            source,
        })?;
    if bytes.len() != 64 {
        return Err(KeypairLoadError::WrongByteCount {
            path,
            actual: bytes.len(),
        });
    }
    Keypair::from_bytes(bytes.as_slice()).map_err(|e| KeypairLoadError::InvalidKeyMaterial {
        path,
        reason: e.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
enum KeypairModeError {
    #[error("operator keypair file at {path} cannot be inspected for permissions")]
    Stat {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "operator keypair file at {path} has overly-permissive mode {mode:#o}; group or world read \
         would let any local user steal the signing key. fix with: chmod 0600 {path}"
    )]
    GroupOrWorldReadable { path: PathBuf, mode: u32 },
}

// Operator keypairs are 64-byte ed25519 secret material. A mode that
// permits group or world read (any bit in mode & 0o077) lets a co-tenant
// or shared-CI scrape it without the operator's knowledge — the cluster
// would then mint settlement transactions signed by the operator's key
// for an attacker. Fail loudly before signing, with a chmod hint so the
// operator can fix it without guessing.
#[cfg(unix)]
fn check_keypair_mode(path: &Path) -> Result<(), KeypairModeError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|source| KeypairModeError::Stat {
        path: path.to_path_buf(),
        source,
    })?;
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(KeypairModeError::GroupOrWorldReadable {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_keypair_mode(_path: &Path) -> Result<(), KeypairModeError> {
    // Windows uses an ACL model rather than POSIX mode bits; the
    // 0o077 check would be a category error there. Operators on
    // non-Unix platforms must verify keypair ACLs out-of-band.
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum PubkeyArgError {
    #[error("--{flag} value is empty; expected a 32-byte base58-encoded Solana public key")]
    Empty { flag: &'static str },
    #[error("--{flag} {value:?} is not a valid 32-byte base58 Solana public key: {reason}")]
    Invalid {
        flag: &'static str,
        value: String,
        reason: String,
    },
}

fn parse_pubkey_arg(flag: &'static str, value: &str) -> Result<Pubkey, PubkeyArgError> {
    use std::str::FromStr;
    if value.is_empty() {
        return Err(PubkeyArgError::Empty { flag });
    }
    Pubkey::from_str(value).map_err(|e| PubkeyArgError::Invalid {
        flag,
        value: value.to_string(),
        reason: e.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
enum Hash32ArgError {
    #[error("--{flag} value is empty; expected exactly 64 hex characters (32 bytes)")]
    Empty { flag: &'static str },
    #[error("--{flag} expected exactly 64 hex characters (32 bytes); got {actual} characters")]
    WrongLength { flag: &'static str, actual: usize },
    #[error("--{flag} contains a non-hex character at position {position}: {ch:?}")]
    BadHexChar {
        flag: &'static str,
        position: usize,
        ch: char,
    },
}

fn parse_hash32_arg(flag: &'static str, value: &str) -> Result<[u8; 32], Hash32ArgError> {
    if value.is_empty() {
        return Err(Hash32ArgError::Empty { flag });
    }
    if value.len() != 64 {
        return Err(Hash32ArgError::WrongLength {
            flag,
            actual: value.len(),
        });
    }
    let mut out = [0u8; 32];
    let bytes = value.as_bytes();
    for i in 0..32 {
        let hi = hex_nibble(bytes[2 * i]).ok_or(Hash32ArgError::BadHexChar {
            flag,
            position: 2 * i,
            ch: bytes[2 * i] as char,
        })?;
        let lo = hex_nibble(bytes[2 * i + 1]).ok_or(Hash32ArgError::BadHexChar {
            flag,
            position: 2 * i + 1,
            ch: bytes[2 * i + 1] as char,
        })?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug)]
struct RegisterAgentCliArgs {
    keypair_path: Option<PathBuf>,
    cluster: String,
    rpc_url: Option<String>,
    program_id: Pubkey,
    agent_key: [u8; 32],
    metadata_hash: [u8; 32],
    capability_hash: [u8; 32],
    confirm_timeout_ms: u64,
    as_json: bool,
}

fn parse_register_agent_cli_args(args: &[String]) -> Result<RegisterAgentCliArgs> {
    let mut keypair_path: Option<PathBuf> = None;
    let mut cluster: String = "devnet".to_string();
    let mut rpc_url: Option<String> = None;
    let mut program_id: Option<Pubkey> = None;
    let mut agent_key_pubkey: Option<Pubkey> = None;
    let mut metadata_hash: Option<[u8; 32]> = None;
    let mut capability_hash: Option<[u8; 32]> = None;
    let mut confirm_timeout_ms: u64 = 60_000;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keypair" => {
                i += 1;
                let v = args.get(i).context("--keypair needs a value")?;
                keypair_path = Some(PathBuf::from(v));
            }
            "--cluster" => {
                i += 1;
                let v = args.get(i).context("--cluster needs a value")?;
                cluster = v.clone();
            }
            "--rpc-url" => {
                i += 1;
                let v = args.get(i).context("--rpc-url needs a value")?;
                rpc_url = Some(v.clone());
            }
            "--program-id" => {
                i += 1;
                let v = args.get(i).context("--program-id needs a value")?;
                program_id = Some(parse_pubkey_arg("program-id", v)?);
            }
            "--agent-key" => {
                i += 1;
                let v = args.get(i).context("--agent-key needs a value")?;
                agent_key_pubkey = Some(parse_pubkey_arg("agent-key", v)?);
            }
            "--metadata-hash" => {
                i += 1;
                let v = args.get(i).context("--metadata-hash needs a value")?;
                metadata_hash = Some(parse_hash32_arg("metadata-hash", v)?);
            }
            "--capability-hash" => {
                i += 1;
                let v = args.get(i).context("--capability-hash needs a value")?;
                capability_hash = Some(parse_hash32_arg("capability-hash", v)?);
            }
            "--confirm-timeout-ms" => {
                i += 1;
                let v = args.get(i).context("--confirm-timeout-ms needs a value")?;
                let parsed: u64 = v
                    .parse()
                    .context("--confirm-timeout-ms must be a non-negative integer")?;
                if parsed == 0 {
                    bail!("--confirm-timeout-ms must be greater than zero");
                }
                confirm_timeout_ms = parsed;
            }
            "--json" => as_json = true,
            other => bail!("unknown flag '{other}'"),
        }
        i += 1;
    }
    let program_id = program_id.context("--program-id is required")?;
    let agent_key_pubkey = agent_key_pubkey.context("--agent-key is required")?;
    let metadata_hash = metadata_hash.context("--metadata-hash is required")?;
    let capability_hash = capability_hash.context("--capability-hash is required")?;
    Ok(RegisterAgentCliArgs {
        keypair_path,
        cluster,
        rpc_url,
        program_id,
        agent_key: agent_key_pubkey.to_bytes(),
        metadata_hash,
        capability_hash,
        confirm_timeout_ms,
        as_json,
    })
}

// Splitting tx construction out of run_chain_register_agent lets unit
// tests pin the fee-payer + single-signer invariant without standing
// up a real RPC client. Anchor's dispatcher reads
// message.account_keys[0] as the fee payer, and a tx whose fee payer
// disagrees with the only signer would be rejected by the cluster
// with a SignatureFailure error that surfaces only at submission
// time.
fn sign_register_agent_tx(
    operator: &Keypair,
    program_id: &Pubkey,
    args: &RegisterAgentArgs,
    recent_blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let ix = build_register_agent_instruction(program_id, &operator.pubkey(), args);
    Transaction::new_signed_with_payer(
        &[ix],
        Some(&operator.pubkey()),
        &[operator],
        recent_blockhash,
    )
}

fn register_agent_confirmed_json(
    signature_b58: &str,
    rpc_url: &str,
    cluster: &str,
    agent_key_b58: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "covenant.chain.tx.v1",
        "verb": "register-agent",
        "signature": signature_b58,
        "rpc_url": rpc_url,
        "cluster": cluster,
        "agent_key": agent_key_b58,
        "status": "confirmed",
    })
}

fn register_agent_timeout_json(
    signature_b58: &str,
    rpc_url: &str,
    cluster: &str,
    agent_key_b58: &str,
    timeout_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "covenant.chain.tx.timeout.v1",
        "verb": "register-agent",
        "signature": signature_b58,
        "rpc_url": rpc_url,
        "cluster": cluster,
        "agent_key": agent_key_b58,
        "status": "submitted-not-confirmed",
        "timeout_ms": timeout_ms,
    })
}

#[derive(Debug)]
struct StakeCliArgs {
    keypair_path: Option<PathBuf>,
    cluster: String,
    rpc_url: Option<String>,
    program_id: Pubkey,
    agent_key: Pubkey,
    owner_covnt: Pubkey,
    stake_vault: Pubkey,
    covnt_mint: Pubkey,
    amount: u64,
    lock_until: u64,
    confirm_timeout_ms: u64,
    as_json: bool,
}

fn parse_u64_arg(flag: &'static str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("--{flag} must be a non-negative integer (got {value:?})"))
}

fn parse_stake_cli_args(args: &[String]) -> Result<StakeCliArgs> {
    let mut keypair_path: Option<PathBuf> = None;
    let mut cluster: String = "devnet".to_string();
    let mut rpc_url: Option<String> = None;
    let mut program_id: Option<Pubkey> = None;
    let mut agent_key: Option<Pubkey> = None;
    let mut owner_covnt: Option<Pubkey> = None;
    let mut stake_vault: Option<Pubkey> = None;
    let mut covnt_mint: Option<Pubkey> = None;
    let mut amount: Option<u64> = None;
    let mut lock_until: Option<u64> = None;
    let mut confirm_timeout_ms: u64 = 60_000;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keypair" => {
                i += 1;
                let v = args.get(i).context("--keypair needs a value")?;
                keypair_path = Some(PathBuf::from(v));
            }
            "--cluster" => {
                i += 1;
                let v = args.get(i).context("--cluster needs a value")?;
                cluster = v.clone();
            }
            "--rpc-url" => {
                i += 1;
                let v = args.get(i).context("--rpc-url needs a value")?;
                rpc_url = Some(v.clone());
            }
            "--program-id" => {
                i += 1;
                let v = args.get(i).context("--program-id needs a value")?;
                program_id = Some(parse_pubkey_arg("program-id", v)?);
            }
            "--agent-key" => {
                i += 1;
                let v = args.get(i).context("--agent-key needs a value")?;
                agent_key = Some(parse_pubkey_arg("agent-key", v)?);
            }
            "--owner-covnt" => {
                i += 1;
                let v = args.get(i).context("--owner-covnt needs a value")?;
                owner_covnt = Some(parse_pubkey_arg("owner-covnt", v)?);
            }
            "--stake-vault" => {
                i += 1;
                let v = args.get(i).context("--stake-vault needs a value")?;
                stake_vault = Some(parse_pubkey_arg("stake-vault", v)?);
            }
            "--covnt-mint" => {
                i += 1;
                let v = args.get(i).context("--covnt-mint needs a value")?;
                covnt_mint = Some(parse_pubkey_arg("covnt-mint", v)?);
            }
            "--amount" => {
                i += 1;
                let v = args.get(i).context("--amount needs a value")?;
                let parsed = parse_u64_arg("amount", v)?;
                if parsed == 0 {
                    // A zero stake transfers nothing, opens a
                    // 0-balance StakePosition the operator paid
                    // rent for, and still costs a tx fee — almost
                    // certainly a typo rather than intent.
                    bail!("--amount must be greater than zero");
                }
                amount = Some(parsed);
            }
            "--lock-until" => {
                i += 1;
                let v = args.get(i).context("--lock-until needs a value")?;
                lock_until = Some(parse_u64_arg("lock-until", v)?);
            }
            "--confirm-timeout-ms" => {
                i += 1;
                let v = args.get(i).context("--confirm-timeout-ms needs a value")?;
                let parsed = parse_u64_arg("confirm-timeout-ms", v)?;
                if parsed == 0 {
                    bail!("--confirm-timeout-ms must be greater than zero");
                }
                confirm_timeout_ms = parsed;
            }
            "--json" => as_json = true,
            other => bail!("unknown flag '{other}'"),
        }
        i += 1;
    }
    let program_id = program_id.context("--program-id is required")?;
    let agent_key = agent_key.context("--agent-key is required")?;
    let owner_covnt = owner_covnt.context("--owner-covnt is required")?;
    let stake_vault = stake_vault.context("--stake-vault is required")?;
    let covnt_mint = covnt_mint.context("--covnt-mint is required")?;
    let amount = amount.context("--amount is required")?;
    let lock_until = lock_until.context("--lock-until is required")?;
    Ok(StakeCliArgs {
        keypair_path,
        cluster,
        rpc_url,
        program_id,
        agent_key,
        owner_covnt,
        stake_vault,
        covnt_mint,
        amount,
        lock_until,
        confirm_timeout_ms,
        as_json,
    })
}

// 8 distinct on-chain accounts/keys are inherent to the stake instruction
// (operator signer, program, agent PDA, owner ATA, stake vault, mint, args,
// blockhash). Splitting would just bag them into a struct that has the same
// arity; the function stays narrow on purpose.
#[allow(clippy::too_many_arguments)]
fn sign_stake_tx(
    operator: &Keypair,
    program_id: &Pubkey,
    agent_key: &Pubkey,
    owner_covnt: &Pubkey,
    stake_vault: &Pubkey,
    covnt_mint: &Pubkey,
    args: &StakeArgs,
    recent_blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let ix = build_stake_instruction(
        program_id,
        &operator.pubkey(),
        agent_key,
        owner_covnt,
        stake_vault,
        covnt_mint,
        args,
    );
    Transaction::new_signed_with_payer(
        &[ix],
        Some(&operator.pubkey()),
        &[operator],
        recent_blockhash,
    )
}

fn stake_confirmed_json(
    signature_b58: &str,
    rpc_url: &str,
    cluster: &str,
    agent_key_b58: &str,
    amount: u64,
    lock_until: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "covenant.chain.tx.v1",
        "verb": "stake",
        "signature": signature_b58,
        "rpc_url": rpc_url,
        "cluster": cluster,
        "agent_key": agent_key_b58,
        "amount": amount,
        "lock_until": lock_until,
        "status": "confirmed",
    })
}

fn stake_timeout_json(
    signature_b58: &str,
    rpc_url: &str,
    cluster: &str,
    agent_key_b58: &str,
    amount: u64,
    lock_until: u64,
    timeout_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "covenant.chain.tx.timeout.v1",
        "verb": "stake",
        "signature": signature_b58,
        "rpc_url": rpc_url,
        "cluster": cluster,
        "agent_key": agent_key_b58,
        "amount": amount,
        "lock_until": lock_until,
        "status": "submitted-not-confirmed",
        "timeout_ms": timeout_ms,
    })
}

async fn run_chain_stake(args: &[String]) -> Result<()> {
    let parsed = parse_stake_cli_args(args)?;

    let resolved_keypair_path = resolve_operator_keypair_path(parsed.keypair_path.clone())?;
    check_keypair_mode(&resolved_keypair_path)?;
    let kp = load_operator_keypair(Some(resolved_keypair_path))?;

    let rpc_url = resolve_solana_rpc_url(Some(&parsed.cluster), parsed.rpc_url.as_deref())?;

    let on_chain_args = StakeArgs {
        amount: parsed.amount,
        lock_until: parsed.lock_until,
    };
    let program_id = parsed.program_id;
    let agent_key = parsed.agent_key;
    let owner_covnt = parsed.owner_covnt;
    let stake_vault = parsed.stake_vault;
    let covnt_mint = parsed.covnt_mint;
    let agent_key_b58 = agent_key.to_string();

    let url_for_prep = rpc_url.clone();
    let prep_args = on_chain_args.clone();
    let (tx, signature_b58) =
        tokio::task::spawn_blocking(move || -> Result<(Transaction, String)> {
            let client =
                RpcClient::new_with_commitment(url_for_prep, CommitmentConfig::confirmed());
            let blockhash = client
                .get_latest_blockhash()
                .context("get_latest_blockhash from Solana RPC")?;
            let tx = sign_stake_tx(
                &kp,
                &program_id,
                &agent_key,
                &owner_covnt,
                &stake_vault,
                &covnt_mint,
                &prep_args,
                blockhash,
            );
            let sig = tx.signatures[0].to_string();
            Ok((tx, sig))
        })
        .await
        .context("join blockhash worker")??;

    let confirm_timeout = Duration::from_millis(parsed.confirm_timeout_ms);
    let url_for_submit = rpc_url.clone();
    let tx_to_send = tx.clone();
    let submit_handle = tokio::task::spawn_blocking(
        move || -> std::result::Result<
            solana_sdk::signature::Signature,
            Box<solana_client::client_error::ClientError>,
        > {
            let client =
                RpcClient::new_with_commitment(url_for_submit, CommitmentConfig::confirmed());
            client
                .send_and_confirm_transaction_with_spinner_and_config(
                    &tx_to_send,
                    CommitmentConfig::confirmed(),
                    // Preflight defaults to `finalized`, which lags the
                    // `confirmed` blockhash and rejects with "Blockhash not
                    // found" before submit. Pin it to `confirmed` to match.
                    RpcSendTransactionConfig {
                        preflight_commitment: Some(CommitmentLevel::Confirmed),
                        ..Default::default()
                    },
                )
                .map_err(Box::new)
        },
    );

    match tokio::time::timeout(confirm_timeout, submit_handle).await {
        Err(_elapsed) => {
            let envelope = stake_timeout_json(
                &signature_b58,
                &rpc_url,
                &parsed.cluster,
                &agent_key_b58,
                parsed.amount,
                parsed.lock_until,
                parsed.confirm_timeout_ms,
            );
            if parsed.as_json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                println!("status: submitted-not-confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {}", parsed.cluster);
                println!("agent_key: {agent_key_b58}");
                println!("amount: {}", parsed.amount);
                println!("lock_until: {}", parsed.lock_until);
                println!(
                    "timeout_ms: {} (poll the cluster manually to confirm)",
                    parsed.confirm_timeout_ms,
                );
            }
            std::process::exit(1);
        }
        Ok(Err(join_err)) => bail!("submit worker panicked: {join_err}"),
        Ok(Ok(Err(client_err))) => {
            bail!("send_and_confirm_transaction failed: {client_err}")
        }
        Ok(Ok(Ok(_confirmed_sig))) => {
            let envelope = stake_confirmed_json(
                &signature_b58,
                &rpc_url,
                &parsed.cluster,
                &agent_key_b58,
                parsed.amount,
                parsed.lock_until,
            );
            if parsed.as_json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                println!("status: confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {}", parsed.cluster);
                println!("agent_key: {agent_key_b58}");
                println!("amount: {}", parsed.amount);
                println!("lock_until: {}", parsed.lock_until);
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
struct BuyCreditsCliArgs {
    keypair_path: Option<PathBuf>,
    cluster: String,
    rpc_url: Option<String>,
    program_id: Pubkey,
    owner_covnt: Pubkey,
    treasury: Pubkey,
    covnt_mint: Pubkey,
    amount_covnt: u64,
    confirm_timeout_ms: u64,
    as_json: bool,
}

fn parse_buy_credits_cli_args(args: &[String]) -> Result<BuyCreditsCliArgs> {
    let mut keypair_path: Option<PathBuf> = None;
    let mut cluster: String = "devnet".to_string();
    let mut rpc_url: Option<String> = None;
    let mut program_id: Option<Pubkey> = None;
    let mut owner_covnt: Option<Pubkey> = None;
    let mut treasury: Option<Pubkey> = None;
    let mut covnt_mint: Option<Pubkey> = None;
    let mut amount_covnt: Option<u64> = None;
    let mut confirm_timeout_ms: u64 = 60_000;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keypair" => {
                i += 1;
                let v = args.get(i).context("--keypair needs a value")?;
                keypair_path = Some(PathBuf::from(v));
            }
            "--cluster" => {
                i += 1;
                let v = args.get(i).context("--cluster needs a value")?;
                cluster = v.clone();
            }
            "--rpc-url" => {
                i += 1;
                let v = args.get(i).context("--rpc-url needs a value")?;
                rpc_url = Some(v.clone());
            }
            "--program-id" => {
                i += 1;
                let v = args.get(i).context("--program-id needs a value")?;
                program_id = Some(parse_pubkey_arg("program-id", v)?);
            }
            "--owner-covnt" => {
                i += 1;
                let v = args.get(i).context("--owner-covnt needs a value")?;
                owner_covnt = Some(parse_pubkey_arg("owner-covnt", v)?);
            }
            "--treasury" => {
                i += 1;
                let v = args.get(i).context("--treasury needs a value")?;
                treasury = Some(parse_pubkey_arg("treasury", v)?);
            }
            "--covnt-mint" => {
                i += 1;
                let v = args.get(i).context("--covnt-mint needs a value")?;
                covnt_mint = Some(parse_pubkey_arg("covnt-mint", v)?);
            }
            "--amount-covnt" => {
                i += 1;
                let v = args.get(i).context("--amount-covnt needs a value")?;
                let parsed = parse_u64_arg("amount-covnt", v)?;
                if parsed == 0 {
                    // A zero-amount buy_credits costs a tx fee
                    // for a no-op token transfer — almost
                    // certainly a typo rather than intent.
                    bail!("--amount-covnt must be greater than zero");
                }
                amount_covnt = Some(parsed);
            }
            "--confirm-timeout-ms" => {
                i += 1;
                let v = args.get(i).context("--confirm-timeout-ms needs a value")?;
                let parsed = parse_u64_arg("confirm-timeout-ms", v)?;
                if parsed == 0 {
                    bail!("--confirm-timeout-ms must be greater than zero");
                }
                confirm_timeout_ms = parsed;
            }
            "--json" => as_json = true,
            other => bail!("unknown flag '{other}'"),
        }
        i += 1;
    }
    let program_id = program_id.context("--program-id is required")?;
    let owner_covnt = owner_covnt.context("--owner-covnt is required")?;
    let treasury = treasury.context("--treasury is required")?;
    let covnt_mint = covnt_mint.context("--covnt-mint is required")?;
    let amount_covnt = amount_covnt.context("--amount-covnt is required")?;
    Ok(BuyCreditsCliArgs {
        keypair_path,
        cluster,
        rpc_url,
        program_id,
        owner_covnt,
        treasury,
        covnt_mint,
        amount_covnt,
        confirm_timeout_ms,
        as_json,
    })
}

fn sign_buy_credits_tx(
    operator: &Keypair,
    program_id: &Pubkey,
    owner_covnt: &Pubkey,
    treasury: &Pubkey,
    covnt_mint: &Pubkey,
    args: &BuyCreditsArgs,
    recent_blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let ix = build_buy_credits_instruction(
        program_id,
        &operator.pubkey(),
        owner_covnt,
        treasury,
        covnt_mint,
        args,
    );
    Transaction::new_signed_with_payer(
        &[ix],
        Some(&operator.pubkey()),
        &[operator],
        recent_blockhash,
    )
}

fn buy_credits_confirmed_json(
    signature_b58: &str,
    rpc_url: &str,
    cluster: &str,
    owner_b58: &str,
    amount_covnt: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "covenant.chain.tx.v1",
        "verb": "buy-credits",
        "signature": signature_b58,
        "rpc_url": rpc_url,
        "cluster": cluster,
        "owner": owner_b58,
        "amount_covnt": amount_covnt,
        "status": "confirmed",
    })
}

fn buy_credits_timeout_json(
    signature_b58: &str,
    rpc_url: &str,
    cluster: &str,
    owner_b58: &str,
    amount_covnt: u64,
    timeout_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "covenant.chain.tx.timeout.v1",
        "verb": "buy-credits",
        "signature": signature_b58,
        "rpc_url": rpc_url,
        "cluster": cluster,
        "owner": owner_b58,
        "amount_covnt": amount_covnt,
        "status": "submitted-not-confirmed",
        "timeout_ms": timeout_ms,
    })
}

async fn run_chain_buy_credits(args: &[String]) -> Result<()> {
    let parsed = parse_buy_credits_cli_args(args)?;

    let resolved_keypair_path = resolve_operator_keypair_path(parsed.keypair_path.clone())?;
    check_keypair_mode(&resolved_keypair_path)?;
    let kp = load_operator_keypair(Some(resolved_keypair_path))?;

    let rpc_url = resolve_solana_rpc_url(Some(&parsed.cluster), parsed.rpc_url.as_deref())?;

    let on_chain_args = BuyCreditsArgs {
        amount_covnt: parsed.amount_covnt,
    };
    let program_id = parsed.program_id;
    let owner_covnt = parsed.owner_covnt;
    let treasury = parsed.treasury;
    let covnt_mint = parsed.covnt_mint;
    let owner_b58 = kp.pubkey().to_string();

    let url_for_prep = rpc_url.clone();
    let prep_args = on_chain_args.clone();
    let (tx, signature_b58) =
        tokio::task::spawn_blocking(move || -> Result<(Transaction, String)> {
            let client =
                RpcClient::new_with_commitment(url_for_prep, CommitmentConfig::confirmed());
            let blockhash = client
                .get_latest_blockhash()
                .context("get_latest_blockhash from Solana RPC")?;
            let tx = sign_buy_credits_tx(
                &kp,
                &program_id,
                &owner_covnt,
                &treasury,
                &covnt_mint,
                &prep_args,
                blockhash,
            );
            let sig = tx.signatures[0].to_string();
            Ok((tx, sig))
        })
        .await
        .context("join blockhash worker")??;

    let confirm_timeout = Duration::from_millis(parsed.confirm_timeout_ms);
    let url_for_submit = rpc_url.clone();
    let tx_to_send = tx.clone();
    let submit_handle = tokio::task::spawn_blocking(
        move || -> std::result::Result<
            solana_sdk::signature::Signature,
            Box<solana_client::client_error::ClientError>,
        > {
            let client =
                RpcClient::new_with_commitment(url_for_submit, CommitmentConfig::confirmed());
            client
                .send_and_confirm_transaction_with_spinner_and_config(
                    &tx_to_send,
                    CommitmentConfig::confirmed(),
                    // Preflight defaults to `finalized`, which lags the
                    // `confirmed` blockhash and rejects with "Blockhash not
                    // found" before submit. Pin it to `confirmed` to match.
                    RpcSendTransactionConfig {
                        preflight_commitment: Some(CommitmentLevel::Confirmed),
                        ..Default::default()
                    },
                )
                .map_err(Box::new)
        },
    );

    match tokio::time::timeout(confirm_timeout, submit_handle).await {
        Err(_elapsed) => {
            let envelope = buy_credits_timeout_json(
                &signature_b58,
                &rpc_url,
                &parsed.cluster,
                &owner_b58,
                parsed.amount_covnt,
                parsed.confirm_timeout_ms,
            );
            if parsed.as_json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                println!("status: submitted-not-confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {}", parsed.cluster);
                println!("owner: {owner_b58}");
                println!("amount_covnt: {}", parsed.amount_covnt);
                println!(
                    "timeout_ms: {} (poll the cluster manually to confirm)",
                    parsed.confirm_timeout_ms,
                );
            }
            std::process::exit(1);
        }
        Ok(Err(join_err)) => bail!("submit worker panicked: {join_err}"),
        Ok(Ok(Err(client_err))) => {
            bail!(
                "send_and_confirm_transaction failed: {client_err} \
                 (if HasOneConstraintViolation: fetch config.treasury via `chain status` \
                 and pass it verbatim to --treasury)"
            )
        }
        Ok(Ok(Ok(_confirmed_sig))) => {
            let envelope = buy_credits_confirmed_json(
                &signature_b58,
                &rpc_url,
                &parsed.cluster,
                &owner_b58,
                parsed.amount_covnt,
            );
            if parsed.as_json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                println!("status: confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {}", parsed.cluster);
                println!("owner: {owner_b58}");
                println!("amount_covnt: {}", parsed.amount_covnt);
            }
        }
    }

    Ok(())
}

async fn run_chain_register_agent(args: &[String]) -> Result<()> {
    let parsed = parse_register_agent_cli_args(args)?;

    let resolved_keypair_path = resolve_operator_keypair_path(parsed.keypair_path.clone())?;
    check_keypair_mode(&resolved_keypair_path)?;
    let kp = load_operator_keypair(Some(resolved_keypair_path))?;

    let rpc_url = resolve_solana_rpc_url(Some(&parsed.cluster), parsed.rpc_url.as_deref())?;

    let on_chain_args = RegisterAgentArgs {
        agent_key: parsed.agent_key,
        metadata_hash: parsed.metadata_hash,
        capability_hash: parsed.capability_hash,
    };
    let program_id = parsed.program_id;
    let agent_key_b58 = Pubkey::new_from_array(parsed.agent_key).to_string();

    // Build + sign the tx on a dedicated blocking thread. The blocking
    // RpcClient builds its own current-thread tokio runtime in its
    // constructor; constructing it inside the outer #[tokio::main]
    // runtime would panic ("Cannot start a runtime from within a
    // runtime"), so the construction lives behind spawn_blocking.
    let url_for_prep = rpc_url.clone();
    let prep_args = on_chain_args.clone();
    let (tx, signature_b58) =
        tokio::task::spawn_blocking(move || -> Result<(Transaction, String)> {
            let client =
                RpcClient::new_with_commitment(url_for_prep, CommitmentConfig::confirmed());
            let blockhash = client
                .get_latest_blockhash()
                .context("get_latest_blockhash from Solana RPC")?;
            let tx = sign_register_agent_tx(&kp, &program_id, &prep_args, blockhash);
            // tx.signatures[0] is the canonical signature for the
            // tx; Transaction::new_signed_with_payer guarantees it is
            // filled when the operator keypair signs as fee payer.
            let sig = tx.signatures[0].to_string();
            Ok((tx, sig))
        })
        .await
        .context("join blockhash worker")??;

    let confirm_timeout = Duration::from_millis(parsed.confirm_timeout_ms);
    let url_for_submit = rpc_url.clone();
    let tx_to_send = tx.clone();
    // ClientError carries large detail fields; box it through the
    // join handle so clippy::result_large_err is satisfied and the
    // join payload stays a single pointer-sized smart pointer.
    let submit_handle = tokio::task::spawn_blocking(
        move || -> std::result::Result<
            solana_sdk::signature::Signature,
            Box<solana_client::client_error::ClientError>,
        > {
            let client =
                RpcClient::new_with_commitment(url_for_submit, CommitmentConfig::confirmed());
            client
                .send_and_confirm_transaction_with_spinner_and_config(
                    &tx_to_send,
                    CommitmentConfig::confirmed(),
                    // Preflight defaults to `finalized`, which lags the
                    // `confirmed` blockhash and rejects with "Blockhash not
                    // found" before submit. Pin it to `confirmed` to match.
                    RpcSendTransactionConfig {
                        preflight_commitment: Some(CommitmentLevel::Confirmed),
                        ..Default::default()
                    },
                )
                .map_err(Box::new)
        },
    );

    match tokio::time::timeout(confirm_timeout, submit_handle).await {
        Err(_elapsed) => {
            // The blocking submit task keeps running in the
            // background; we only stop waiting on confirmation. The
            // signature is already known from the locally-signed tx,
            // so the operator can poll the cluster manually.
            let envelope = register_agent_timeout_json(
                &signature_b58,
                &rpc_url,
                &parsed.cluster,
                &agent_key_b58,
                parsed.confirm_timeout_ms,
            );
            if parsed.as_json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                println!("status: submitted-not-confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {}", parsed.cluster);
                println!("agent_key: {agent_key_b58}");
                println!(
                    "timeout_ms: {} (poll the cluster manually to confirm)",
                    parsed.confirm_timeout_ms,
                );
            }
            std::process::exit(1);
        }
        Ok(Err(join_err)) => bail!("submit worker panicked: {join_err}"),
        Ok(Ok(Err(client_err))) => {
            bail!("send_and_confirm_transaction failed: {client_err}")
        }
        Ok(Ok(Ok(_confirmed_sig))) => {
            let envelope = register_agent_confirmed_json(
                &signature_b58,
                &rpc_url,
                &parsed.cluster,
                &agent_key_b58,
            );
            if parsed.as_json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                println!("status: confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {}", parsed.cluster);
                println!("agent_key: {agent_key_b58}");
            }
        }
    }

    Ok(())
}

// --- additional settlement instruction builders ---------------------------
// These mirror the on-chain account ordering in
// agent-os/programs/settlement/src/lib.rs. Anchor reads accounts positionally,
// so the order is load-bearing.

fn build_initialize_instruction(
    program_id: &Pubkey,
    authority: &Pubkey,
    covnt_mint: &Pubkey,
    treasury: &Pubkey,
    slash_authority: &Pubkey,
    credits_per_covnt: u64,
    min_stake_lock: u64,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (config, _) = settlement_config_pda(program_id);
    let mut data = compute_anchor_global_discriminator("initialize").to_vec();
    data.extend_from_slice(slash_authority.as_ref());
    data.extend_from_slice(&credits_per_covnt.to_le_bytes());
    data.extend_from_slice(&min_stake_lock.to_le_bytes());
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(config, false),
            AccountMeta::new(*authority, true),
            AccountMeta::new_readonly(*covnt_mint, false),
            AccountMeta::new_readonly(*treasury, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
        data,
    }
}

fn build_open_credit_account_instruction(
    program_id: &Pubkey,
    owner: &Pubkey,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (credits, _) = settlement_credits_pda(program_id, owner);
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(credits, false),
            AccountMeta::new(*owner, true),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
        data: compute_anchor_global_discriminator("open_credit_account").to_vec(),
    }
}

fn build_unstake_instruction(
    program_id: &Pubkey,
    owner: &Pubkey,
    agent_key: &Pubkey,
    stake_vault: &Pubkey,
    owner_covnt: &Pubkey,
    covnt_mint: &Pubkey,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (config, _) = settlement_config_pda(program_id);
    let (agent, _) = settlement_agent_pda(program_id, agent_key);
    let (position, _) = settlement_stake_position_pda(program_id, agent_key, owner);
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(config, false),
            AccountMeta::new(agent, false),
            AccountMeta::new(position, false),
            AccountMeta::new(*owner, true),
            AccountMeta::new(*stake_vault, false),
            AccountMeta::new(*owner_covnt, false),
            AccountMeta::new_readonly(*covnt_mint, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data: compute_anchor_global_discriminator("unstake").to_vec(),
    }
}

fn build_close_position_instruction(
    program_id: &Pubkey,
    owner: &Pubkey,
    agent_key: &Pubkey,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (position, _) = settlement_stake_position_pda(program_id, agent_key, owner);
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(position, false),
            AccountMeta::new(*owner, true),
        ],
        data: compute_anchor_global_discriminator("close_position").to_vec(),
    }
}

fn build_migrate_config_instruction(
    program_id: &Pubkey,
    authority: &Pubkey,
    min_stake_lock: u64,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (config, _) = settlement_config_pda(program_id);
    let mut data = compute_anchor_global_discriminator("migrate_config").to_vec();
    data.extend_from_slice(&min_stake_lock.to_le_bytes());
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(config, false),
            AccountMeta::new(*authority, true),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
        data,
    }
}

fn build_update_config_u64_instruction(
    program_id: &Pubkey,
    authority: &Pubkey,
    method: &str,
    value: u64,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (config, _) = settlement_config_pda(program_id);
    let mut data = compute_anchor_global_discriminator(method).to_vec();
    data.extend_from_slice(&value.to_le_bytes());
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(config, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

fn build_update_config_pubkey_instruction(
    program_id: &Pubkey,
    authority: &Pubkey,
    method: &str,
    new_value: &Pubkey,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (config, _) = settlement_config_pda(program_id);
    let mut data = compute_anchor_global_discriminator(method).to_vec();
    data.extend_from_slice(new_value.as_ref());
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(config, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

fn build_update_treasury_instruction(
    program_id: &Pubkey,
    authority: &Pubkey,
    treasury: &Pubkey,
) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let (config, _) = settlement_config_pda(program_id);
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(config, false),
            AccountMeta::new_readonly(*authority, true),
            AccountMeta::new_readonly(*treasury, false),
        ],
        data: compute_anchor_global_discriminator("update_treasury").to_vec(),
    }
}

// Shared flag bag for the direct-RPC chain verbs added on top of the original
// register-agent/stake/buy-credits trio. Every `--flag value` pair lands in
// `map`; `--json` is the only valueless flag.
struct ChainFlags {
    map: std::collections::HashMap<String, String>,
    json: bool,
}

fn parse_chain_flags(args: &[String]) -> Result<ChainFlags> {
    let mut map = std::collections::HashMap::new();
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--json" {
            json = true;
            i += 1;
            continue;
        }
        if a.starts_with("--") {
            let v = args
                .get(i + 1)
                .with_context(|| format!("{a} needs a value"))?;
            map.insert(a.clone(), v.clone());
            i += 2;
        } else {
            bail!("unexpected argument '{a}'");
        }
    }
    Ok(ChainFlags { map, json })
}

impl ChainFlags {
    fn pubkey(&self, flag: &'static str) -> Result<Pubkey> {
        let v = self
            .map
            .get(flag)
            .with_context(|| format!("{flag} is required"))?;
        Ok(parse_pubkey_arg(flag.trim_start_matches('-'), v)?)
    }

    fn u64(&self, flag: &'static str) -> Result<u64> {
        let v = self
            .map
            .get(flag)
            .with_context(|| format!("{flag} is required"))?;
        parse_u64_arg(flag.trim_start_matches('-'), v)
    }

    fn u64_or(&self, flag: &'static str, default: u64) -> Result<u64> {
        match self.map.get(flag) {
            Some(v) => parse_u64_arg(flag.trim_start_matches('-'), v),
            None => Ok(default),
        }
    }

    fn cluster(&self) -> String {
        self.map
            .get("--cluster")
            .cloned()
            .unwrap_or_else(|| "devnet".to_string())
    }

    fn timeout_ms(&self) -> u64 {
        self.map
            .get("--confirm-timeout-ms")
            .and_then(|v| v.parse().ok())
            .unwrap_or(60_000)
    }

    fn operator_and_rpc(&self) -> Result<(Keypair, String)> {
        let resolved = resolve_operator_keypair_path(self.map.get("--keypair").map(PathBuf::from))?;
        check_keypair_mode(&resolved)?;
        let keypair = load_operator_keypair(Some(resolved))?;
        let rpc_url = resolve_solana_rpc_url(
            Some(&self.cluster()),
            self.map.get("--rpc-url").map(|s| s.as_str()),
        )?;
        Ok((keypair, rpc_url))
    }
}

// Build + sign on a blocking thread (the blocking RpcClient builds its own
// runtime), submit with a confirmed-commitment preflight, and emit the same
// `covenant.chain.tx.v1` envelope shape the register-agent path uses.
#[allow(clippy::too_many_arguments)]
async fn submit_chain_tx(
    verb: &'static str,
    cluster: String,
    rpc_url: String,
    keypair: Keypair,
    confirm_timeout_ms: u64,
    as_json: bool,
    extra: serde_json::Map<String, serde_json::Value>,
    build_ix: impl FnOnce(&Pubkey) -> solana_sdk::instruction::Instruction + Send + 'static,
) -> Result<()> {
    let operator = keypair.pubkey();
    let url_for_prep = rpc_url.clone();
    let (tx, signature_b58) =
        tokio::task::spawn_blocking(move || -> Result<(Transaction, String)> {
            let client =
                RpcClient::new_with_commitment(url_for_prep, CommitmentConfig::confirmed());
            let blockhash = client
                .get_latest_blockhash()
                .context("get_latest_blockhash from Solana RPC")?;
            let ix = build_ix(&operator);
            let tx =
                Transaction::new_signed_with_payer(&[ix], Some(&operator), &[&keypair], blockhash);
            let sig = tx.signatures[0].to_string();
            Ok((tx, sig))
        })
        .await
        .context("join blockhash worker")??;

    let url_for_submit = rpc_url.clone();
    let tx_to_send = tx.clone();
    let submit_handle = tokio::task::spawn_blocking(
        move || -> std::result::Result<
            solana_sdk::signature::Signature,
            Box<solana_client::client_error::ClientError>,
        > {
            let client =
                RpcClient::new_with_commitment(url_for_submit, CommitmentConfig::confirmed());
            client
                .send_and_confirm_transaction_with_spinner_and_config(
                    &tx_to_send,
                    CommitmentConfig::confirmed(),
                    RpcSendTransactionConfig {
                        preflight_commitment: Some(CommitmentLevel::Confirmed),
                        ..Default::default()
                    },
                )
                .map_err(Box::new)
        },
    );

    let mut envelope = serde_json::Map::new();
    envelope.insert("verb".to_string(), verb.into());
    envelope.insert("signature".to_string(), signature_b58.clone().into());
    envelope.insert("rpc_url".to_string(), rpc_url.clone().into());
    envelope.insert("cluster".to_string(), cluster.clone().into());
    for (k, v) in extra {
        envelope.insert(k, v);
    }

    match tokio::time::timeout(Duration::from_millis(confirm_timeout_ms), submit_handle).await {
        Err(_elapsed) => {
            envelope.insert("kind".to_string(), "covenant.chain.tx.timeout.v1".into());
            envelope.insert("status".to_string(), "submitted-not-confirmed".into());
            envelope.insert("timeout_ms".to_string(), confirm_timeout_ms.into());
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::Value::Object(envelope))?
                );
            } else {
                println!("status: submitted-not-confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {cluster}");
            }
            std::process::exit(1);
        }
        Ok(Err(join_err)) => bail!("submit worker panicked: {join_err}"),
        Ok(Ok(Err(client_err))) => bail!("send_and_confirm_transaction failed: {client_err}"),
        Ok(Ok(Ok(_confirmed))) => {
            envelope.insert("kind".to_string(), "covenant.chain.tx.v1".into());
            envelope.insert("status".to_string(), "confirmed".into());
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::Value::Object(envelope))?
                );
            } else {
                println!("status: confirmed");
                println!("signature: {signature_b58}");
                println!("rpc_url: {rpc_url}");
                println!("cluster: {cluster}");
            }
            Ok(())
        }
    }
}

async fn run_chain_initialize(args: &[String]) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let covnt_mint = f.pubkey("--covnt-mint")?;
    let treasury = f.pubkey("--treasury")?;
    let slash_authority = f.pubkey("--slash-authority")?;
    let credits_per_covnt = f.u64("--credits-per-covnt")?;
    let min_stake_lock = f.u64_or("--min-stake-lock", 0)?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("covnt_mint".to_string(), covnt_mint.to_string().into());
    extra.insert("credits_per_covnt".to_string(), credits_per_covnt.into());
    extra.insert("min_stake_lock".to_string(), min_stake_lock.into());
    submit_chain_tx(
        "initialize",
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |authority| {
            build_initialize_instruction(
                &program_id,
                authority,
                &covnt_mint,
                &treasury,
                &slash_authority,
                credits_per_covnt,
                min_stake_lock,
            )
        },
    )
    .await
}

async fn run_chain_open_credit_account(args: &[String]) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    submit_chain_tx(
        "open-credit-account",
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        serde_json::Map::new(),
        move |owner| build_open_credit_account_instruction(&program_id, owner),
    )
    .await
}

async fn run_chain_unstake(args: &[String]) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let agent_key = f.pubkey("--agent-key")?;
    let stake_vault = f.pubkey("--stake-vault")?;
    let owner_covnt = f.pubkey("--owner-covnt")?;
    let covnt_mint = f.pubkey("--covnt-mint")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("agent_key".to_string(), agent_key.to_string().into());
    submit_chain_tx(
        "unstake",
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |owner| {
            build_unstake_instruction(
                &program_id,
                owner,
                &agent_key,
                &stake_vault,
                &owner_covnt,
                &covnt_mint,
            )
        },
    )
    .await
}

async fn run_chain_close_position(args: &[String]) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let agent_key = f.pubkey("--agent-key")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("agent_key".to_string(), agent_key.to_string().into());
    submit_chain_tx(
        "close-position",
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |owner| build_close_position_instruction(&program_id, owner, &agent_key),
    )
    .await
}

async fn run_chain_migrate_config(args: &[String]) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let min_stake_lock = f.u64("--min-stake-lock")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("min_stake_lock".to_string(), min_stake_lock.into());
    submit_chain_tx(
        "migrate-config",
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |authority| build_migrate_config_instruction(&program_id, authority, min_stake_lock),
    )
    .await
}

async fn run_chain_set_config_u64(
    args: &[String],
    verb: &'static str,
    method: &'static str,
) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let value = f.u64("--value")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("value".to_string(), value.into());
    submit_chain_tx(
        verb,
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |authority| build_update_config_u64_instruction(&program_id, authority, method, value),
    )
    .await
}

async fn run_chain_set_config_pubkey(
    args: &[String],
    verb: &'static str,
    method: &'static str,
) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let new_value = f.pubkey("--new")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("new".to_string(), new_value.to_string().into());
    submit_chain_tx(
        verb,
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |authority| {
            build_update_config_pubkey_instruction(&program_id, authority, method, &new_value)
        },
    )
    .await
}

async fn run_chain_update_treasury(args: &[String]) -> Result<()> {
    let f = parse_chain_flags(args)?;
    let program_id = f.pubkey("--program-id")?;
    let treasury = f.pubkey("--treasury")?;
    let (keypair, rpc_url) = f.operator_and_rpc()?;
    let mut extra = serde_json::Map::new();
    extra.insert("treasury".to_string(), treasury.to_string().into());
    submit_chain_tx(
        "update-treasury",
        f.cluster(),
        rpc_url,
        keypair,
        f.timeout_ms(),
        f.json,
        extra,
        move |authority| build_update_treasury_instruction(&program_id, authority, &treasury),
    )
    .await
}

async fn authenticate(stream: &mut UnixStream, home: &std::path::Path) -> Result<()> {
    let token_path = home.join("peers").join("operator.token");
    let token_b58 = std::fs::read_to_string(&token_path)
        .with_context(|| {
            format!(
                "read operator token at {} (start covenantd at least once to mint it)",
                token_path.display()
            )
        })?
        .trim()
        .to_string();
    write_frame(stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(stream).await? {
        Response::Authenticated { .. } => Ok(()),
        Response::AuthenticationFailed { reason } => bail!("authenticate failed: {reason}"),
        other => bail!("unexpected response to authenticate: {other:?}"),
    }
}

fn print_usage() {
    eprintln!("covenant — agent-native operating layer CLI");
    eprintln!();
    eprintln!("usage:");
    eprintln!(
        "  covenant bootstrap [--json]             grant the capabilities every loaded agent needs to run its first task"
    );
    eprintln!("  covenant intent [--json] [--stream] <text>  submit an intent and print the result (--stream opts into v2 streaming response framing)");
    eprintln!("  covenant ping [--json]                  check the daemon is responsive");
    eprintln!(
        "  covenant version                        print daemon protocol metadata as JSON (no token required)"
    );
    eprintln!(
        "  covenant memory recent [--tier T] [-n N] [--json] [--stream]      list recent memory records (--stream opts into v2 streaming response framing)"
    );
    eprintln!(
        "  covenant memory search <query> [--tier T] [-n N] [--min-relevance F] [--json]  semantic search via embeddings; --min-relevance F drops records whose cosine score is below F (range [0.0, 1.0]) before the limit is applied"
    );
    eprintln!(
        "  covenant memory purge [--tier T] (--before-ms M | --older-than-ms D) [--json]  delete records older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant memory compact --reason TEXT [--apply] [--detach-stale-parents] [--delete-working-before-ms M|--delete-working-older-than-ms D] [--delete-episodic-before-ms M|--delete-episodic-older-than-ms D] [--mark-longterm-stale-before-ms M|--mark-longterm-stale-older-than-ms D] [--json]"
    );
    eprintln!(
        "  covenant memory plan-compaction --reason TEXT [--detach-stale-parents] [--delete-working-before-ms M|--delete-working-older-than-ms D] [--delete-episodic-before-ms M|--delete-episodic-older-than-ms D] [--mark-longterm-stale-before-ms M|--mark-longterm-stale-older-than-ms D] [--json]"
    );
    eprintln!("  covenant memory plan-receipt-backfill [-n N] [--json]  dry-run legacy memory receipt correlation plan");
    eprintln!("  covenant memory backfill-receipt-correlation [--dry-run] [--json]  apply legacy memory receipt correlation (--scope-pubkey reserved, not yet supported)");
    eprintln!(
        "  covenant memory repair detach-parent <id> --reason TEXT [--expected-parent UUID] [--apply]"
    );
    eprintln!("  covenant memory repair delete <id> --reason TEXT [--apply]");
    eprintln!(
        "  covenant memory repair backfill-provenance <id> --reason TEXT --provenance JSON [--apply]"
    );
    eprintln!(
        "  covenant receipts recent [-n N] [--since-ms <epoch_ms>] [--json]  list recent settlement receipts"
    );
    eprintln!("  covenant chain status [--json]          show Solana protocol configuration");
    eprintln!(
        "  covenant chain flush-receipts [-n N] [--json]  batch local receipts into a Solana receipt root"
    );
    eprintln!("  covenant chain receipt-batches [-n N] [--json]  list local receipt batches");
    eprintln!(
        "  covenant chain register-agent --program-id BASE58 --agent-key BASE58 --metadata-hash HEX64 --capability-hash HEX64 [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]  sign and submit a settlement register_agent transaction with the operator keypair"
    );
    eprintln!(
        "  covenant chain stake --program-id BASE58 --agent-key BASE58 --owner-covnt BASE58 --stake-vault BASE58 --amount U64 --lock-until U64 [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]  sign and submit a settlement stake transaction with the operator keypair"
    );
    eprintln!(
        "  covenant chain buy-credits --program-id BASE58 --owner-covnt BASE58 --treasury BASE58 --amount-covnt U64 [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]  sign and submit a settlement buy_credits transaction with the operator keypair; --treasury MUST equal config.treasury (fetch via `chain status` if unknown)"
    );
    eprintln!(
        "  covenant chain initialize|open-credit-account|unstake|close-position|migrate-config|set-min-stake-lock|set-credits-per-covnt|update-authority|update-slash-authority|update-treasury  --program-id BASE58 [verb-specific flags] [--keypair PATH] [--cluster NAME] [--rpc-url URL] [--confirm-timeout-ms N] [--json]  sign and submit the matching settlement instruction with the operator keypair"
    );
    eprintln!(
        "  covenant settlement backfill-receipts [--dry-run] [--json]  repair legacy settlement-receipt rows (--scope-pubkey reserved, not yet supported)"
    );
    eprintln!("  covenant verify [-w N] [--json]      cross-check audit log vs other state");
    eprintln!("  covenant ignore check [--json] <text>   test text against .covenantignore rules");
    eprintln!("  covenant tools list [--json]            list registered tools");
    eprintln!("  covenant tools call <name> [--args <json>] [--json]   invoke a registered tool");
    eprintln!(
        "  covenant audit recent [-n N] [--since-ms <epoch_ms>] [--json] [--stream]   list recent audit events as JSONL or one JSON envelope; --since-ms drops events older than the given epoch ms before --limit is applied; --stream opts into v2 streaming response framing"
    );
    eprintln!("  covenant audit verify [--json]         verify local audit hash-chain sidecar");
    eprintln!(
        "  covenant audit purge (--before-ms M | --older-than-ms D) [--json]  drop audit events older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant capabilities recent [-n N] [--json]  list recent active capability tokens"
    );
    eprintln!("  covenant capabilities grant <action> [--scope JSON] [--expires-at M] [--json]");
    eprintln!("  covenant capabilities revoke <signature-b58> [--json]");
    eprintln!(
        "  covenant capabilities purge (--before-ms M | --older-than-ms D) [--json]  drop revoked caps older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant a2a status [-n N] [--min-lease-age-ms N] [--deadline-within-ms N] [--state queued|in_flight] [--json]  list queued tasks, in-flight leases, and pending results; --deadline-within-ms N keeps only tasks whose deadline_ms is set and within at most N ms from now; --state narrows to one queue state"
    );
    eprintln!(
        "  covenant a2a requeue <task-id> --reason TEXT --duplicate-risk idempotent|operator-accepted [--lease-id UUID]"
    );
    eprintln!(
        "  covenant a2a force-error <task-id> --reason TEXT --message TEXT [--lease-id UUID]"
    );
    eprintln!(
        "  covenant a2a retry-stale [--enable] [--min-lease-age-ms N] [--max-attempts N] [--max-requeues N] [--scan-limit N] [--json]"
    );
    eprintln!(
        "  covenant a2a compact [--json]          drop event-log lines for fully-resolved a2a tasks"
    );
    eprintln!(
        "  covenant peers purge (--before-ms M | --older-than-ms D) [--json]  drop revoked peer registrations older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant peers rotate [--json]          mint a fresh operator token and revoke the old one"
    );
    eprintln!(
        "  covenant peers list [--limit N] [--prefix B58] [--live-only | --revoked-only] [--json]  list registered peers (operator-only) — match audit `peer_pubkey_b58` via --prefix; add --json for stable machine output"
    );
    eprintln!(
        "  covenant peers revoke <TOKEN-PREFIX> [--force] [--limit-matches N] [--json]  revoke a single peer by its token prefix (operator-only); --json emits one stable machine-readable outcome"
    );
    eprintln!(
        "  covenant intents resume <intent-id> [--json]     re-dispatch a previously budget-rejected intent"
    );
    eprintln!(
        "  covenant intents resume latest [--json]          re-dispatch the most recent budget-rejected intent"
    );
    eprintln!(
        "  covenant sap status [--json]            resolved SAP bridge status (cluster, program, signer presence)"
    );
    eprintln!(
        "  covenant sap publish --manifest <file> [--json]  publish the daemon's agent through the SAP bridge"
    );
}

struct MemoryReadJsonArgs {
    mode: &'static str,
    tier: Option<MemoryTier>,
    limit: usize,
    query: Option<String>,
    min_relevance: Option<f32>,
}

async fn resolve_intents_resume_intent_id(
    stream: &mut UnixStream,
    want_latest: bool,
    explicit_id: Option<&str>,
) -> Result<uuid::Uuid> {
    if want_latest {
        if explicit_id.is_some() {
            bail!("covenant intents resume: pass either <intent-id> or latest, not both");
        }
        let limit = 200;
        write_frame(
            stream,
            &Request::RecentAudit {
                limit,
                since_ms: None,
                prefer_stream: None,
            },
        )
        .await?;
        let events = match read_frame::<_, Response>(stream).await? {
            Response::AuditEvents { events } => events,
            Response::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected response: {other:?}"),
        };
        let mut latest: Option<(u64, uuid::Uuid)> = None;
        for e in events {
            let id = match e.kind {
                AuditKind::BudgetExhausted { intent_id, .. } => intent_id,
                _ => continue,
            };
            match latest {
                Some((ts, _)) if ts >= e.timestamp_ms => {}
                _ => latest = Some((e.timestamp_ms, id)),
            }
        }
        return latest.map(|(_, id)| id).context(
            "no BudgetExhausted audit row found in recent audit feed (try `covenant audit recent`)",
        );
    }

    let intent_id_str =
        explicit_id.context("covenant intents resume: missing <intent-id> or latest")?;
    intent_id_str
        .parse()
        .with_context(|| format!("intent-id must be a uuid, got {intent_id_str:?}"))
}

async fn print_memory_response(
    stream: &mut UnixStream,
    json: Option<MemoryReadJsonArgs>,
) -> Result<()> {
    let response = match read_response_or_stream(stream).await? {
        ResponseOrStream::Terminal(r) => r,
        ResponseOrStream::Stream(collected) => {
            if collected.response_kind != "memories" {
                bail!(
                    "unexpected stream response_kind '{}' (expected 'memories')",
                    collected.response_kind
                );
            }
            let records = decode_memory_chunks(collected.chunks)?;
            Response::Memories { records }
        }
    };
    match response {
        Response::Memories { records } => {
            if let Some(args) = json {
                println!(
                    "{}",
                    serde_json::to_string(&memory_read_json(
                        args.mode,
                        args.tier,
                        args.limit,
                        args.query.as_deref(),
                        args.min_relevance,
                        &records
                    ))?
                );
                return Ok(());
            }
            if records.is_empty() {
                println!("(no records)");
            }
            for r in records {
                let tier = memory_tier_slug(r.tier);
                println!("[{}] {tier}: {}", r.created_at, r.text);
            }
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn decode_memory_chunks(chunks: Vec<serde_json::Value>) -> Result<Vec<MemoryRecord>> {
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            serde_json::from_value::<MemoryRecord>(chunk)
                .with_context(|| format!("decode memory stream chunk {i}"))
        })
        .collect()
}

fn decode_audit_chunks(chunks: Vec<serde_json::Value>) -> Result<Vec<AuditEvent>> {
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            serde_json::from_value::<AuditEvent>(chunk)
                .with_context(|| format!("decode audit stream chunk {i}"))
        })
        .collect()
}

fn decode_intent_stream(
    chunks: Vec<serde_json::Value>,
    summary: Option<serde_json::Value>,
) -> Result<Response> {
    // ADR 0010 / Server::stream_submit_intent emits exactly one
    // StreamChunk carrying an AgentResult plus a StreamEnd.summary that
    // names {intent_id, status, settlement}. The CLI extracts the two
    // load-bearing AgentResult fields (text + sources) by hand instead
    // of pulling in covenant-runtime as a CLI dep — the streamed
    // runtime_events vec is always empty per ADR.
    if chunks.len() != 1 {
        bail!(
            "intent stream expected exactly one AgentResult chunk, got {}",
            chunks.len()
        );
    }
    let chunk = chunks.into_iter().next().expect("len == 1 just checked");
    let chunk_obj = chunk
        .as_object()
        .context("intent stream chunk is not a JSON object")?;
    let text = chunk_obj
        .get("text")
        .and_then(|v| v.as_str())
        .context("intent stream chunk missing 'text'")?
        .to_string();
    let sources: Vec<String> = match chunk_obj.get("sources") {
        Some(v) => serde_json::from_value(v.clone())
            .context("decode 'sources' from intent stream chunk")?,
        None => Vec::new(),
    };
    let summary = summary.context("intent stream missing summary on StreamEnd")?;
    let intent_id: uuid::Uuid = serde_json::from_value(
        summary
            .get("intent_id")
            .cloned()
            .context("intent stream summary missing intent_id")?,
    )
    .context("decode intent_id from summary")?;
    let status: String = serde_json::from_value(
        summary
            .get("status")
            .cloned()
            .context("intent stream summary missing status")?,
    )
    .context("decode status from summary")?;
    let settlement: Option<SettlementReceipt> = match summary.get("settlement").cloned() {
        Some(v) if v.is_null() => None,
        Some(v) => Some(serde_json::from_value(v).context("decode settlement from summary")?),
        None => None,
    };
    Ok(Response::IntentResult {
        intent_id,
        status,
        text,
        sources,
        settlement,
    })
}

fn memory_tier_slug(tier: MemoryTier) -> &'static str {
    match tier {
        MemoryTier::Working => "working",
        MemoryTier::Episodic => "episodic",
        MemoryTier::LongTerm => "longterm",
    }
}

fn parse_tier(s: &str) -> Result<MemoryTier> {
    match s {
        "working" => Ok(MemoryTier::Working),
        "episodic" => Ok(MemoryTier::Episodic),
        "longterm" | "long-term" | "long_term" => Ok(MemoryTier::LongTerm),
        other => bail!("unknown tier '{other}' (expected working|episodic|longterm)"),
    }
}

fn parse_duplicate_risk(value: &str) -> Result<A2ADuplicateRisk> {
    match value {
        "idempotent" => Ok(A2ADuplicateRisk::Idempotent),
        "operator-accepted" | "operator_accepted" => Ok(A2ADuplicateRisk::OperatorAccepted),
        other => bail!("unknown duplicate risk '{other}' (expected idempotent|operator-accepted)"),
    }
}

fn parse_a2a_queue_state(value: &str) -> Result<A2ATaskQueueState> {
    match value {
        "queued" => Ok(A2ATaskQueueState::Queued),
        "in_flight" | "in-flight" => Ok(A2ATaskQueueState::InFlight),
        other => bail!("unknown a2a state '{other}' (expected queued|in_flight)"),
    }
}

fn parse_uuid(value: &str, name: &str) -> Result<uuid::Uuid> {
    value
        .parse()
        .with_context(|| format!("{name} must be a UUID"))
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn print_a2a_repair_response(response: Response) -> Result<()> {
    match response {
        Response::A2ARepaired { outcome } => {
            println!("{}", serde_json::to_string(&outcome)?);
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_memory_repair_response(response: Response) -> Result<()> {
    match response {
        Response::MemoryRepaired { outcome } => {
            println!("{}", serde_json::to_string(&outcome)?);
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_memory_compaction_response(response: Response, as_json: bool) -> Result<()> {
    match response {
        Response::MemoryCompacted { outcome } => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&memory_compaction_json(&outcome))?
                );
            } else {
                println!("{}", serde_json::to_string(&outcome)?);
            }
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_memory_compaction_plan_response(response: Response, as_json: bool) -> Result<()> {
    match response {
        Response::MemoryCompacted { outcome } => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&memory_compaction_plan_json(&outcome))?
                );
            } else {
                println!(
                    "would change: {} (delete {}, mark stale {}, detach parent {})",
                    outcome.would_change,
                    outcome.deleted.len(),
                    outcome.stale_marked.len(),
                    outcome.parents_detached.len()
                );
                println!("receipt backfill: none in dry-run plan");
            }
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(2);
    }

    let home = covenant_home()?;
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;

    // `version` is the operator's pre-auth protocol probe — same
    // payload as the HTTP `/version` route. Handle it before
    // `authenticate` so a fresh `COVENANT_HOME` with no operator
    // token still gets a usable response.
    if args[0] == "version" {
        write_frame(&mut stream, &Request::ProtocolInfo).await?;
        match read_frame::<_, Response>(&mut stream).await? {
            Response::ProtocolInfo { info } => {
                println!("{}", serde_json::to_string(&info)?);
                return Ok(());
            }
            Response::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    authenticate(&mut stream, &home).await?;

    match args[0].as_str() {
        "bootstrap" => {
            let mut as_json = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--json" => as_json = true,
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }

            // Union of every loaded agent's [capabilities] required, plus
            // memory.write — the daemon writes a working-memory record on
            // every successful dispatch, so without it the first intent
            // fails before any agent code runs.
            let agents_dir = home.join("agents");
            let mut required: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            required.insert("memory.write".to_string());
            if agents_dir.exists() {
                for entry in std::fs::read_dir(&agents_dir)
                    .with_context(|| format!("read agents dir {}", agents_dir.display()))?
                {
                    let entry = entry?;
                    let manifest_path = entry.path().join("agent.toml");
                    if !manifest_path.exists() {
                        continue;
                    }
                    let raw = std::fs::read_to_string(&manifest_path)
                        .with_context(|| format!("read {}", manifest_path.display()))?;
                    let manifest = covenant_manifest::Manifest::parse(&raw)
                        .with_context(|| format!("parse manifest {}", manifest_path.display()))?;
                    for action in manifest.capabilities.required.iter() {
                        required.insert(action.clone());
                    }
                }
            }

            // Skip what's already granted so re-running bootstrap is a no-op.
            write_frame(&mut stream, &Request::RecentCapabilities { limit: 512 }).await?;
            let existing: std::collections::HashSet<String> =
                match read_frame::<_, Response>(&mut stream).await? {
                    Response::Capabilities { capabilities } => capabilities
                        .iter()
                        .map(|c| c.capability.action.clone())
                        .collect(),
                    Response::Error { message } => bail!("daemon error: {message}"),
                    other => bail!("unexpected response: {other:?}"),
                };

            let mut granted: Vec<(String, String)> = Vec::new();
            let mut already: Vec<String> = Vec::new();
            for action in &required {
                if existing.contains(action) {
                    already.push(action.clone());
                    continue;
                }
                write_frame(
                    &mut stream,
                    &Request::GrantCapability {
                        action: action.clone(),
                        scope: None,
                        expires_at: None,
                    },
                )
                .await?;
                match read_frame::<_, Response>(&mut stream).await? {
                    Response::CapabilityGranted { signature_b58, .. } => {
                        granted.push((action.clone(), signature_b58));
                    }
                    Response::Error { message } => {
                        bail!("daemon error granting {action}: {message}")
                    }
                    other => bail!("unexpected response: {other:?}"),
                }
            }

            if as_json {
                let payload = bootstrap_result_json(&granted, &already);
                println!("{}", serde_json::to_string(&payload)?);
            } else if granted.is_empty() {
                println!(
                    "nothing to do — every required capability is already granted ({} total)",
                    already.len()
                );
            } else {
                println!(
                    "granted {} of {} capabilities to user@local:",
                    granted.len(),
                    granted.len() + already.len()
                );
                for (action, _) in &granted {
                    let label = covenant_permissions::friendly_action_title(action)
                        .map(|t| format!("{t} ({action})"))
                        .unwrap_or_else(|| action.clone());
                    println!("  + {label}");
                }
                if !already.is_empty() {
                    println!("{} already granted, skipped.", already.len());
                }
                println!();
                println!("ready. try: covenant intent \"say hello\"");
            }
        }
        "ping" => {
            let mut as_json = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--json" => as_json = true,
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::Ping).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Pong => {
                    if as_json {
                        println!("{}", serde_json::to_string(&ping_json())?);
                    } else {
                        println!("pong");
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "intent" => {
            if args.len() < 2 {
                eprintln!("covenant intent: missing intent text");
                std::process::exit(2);
            }
            // Strip `--json` and `--stream` only from leading positions.
            // Stop scanning for flags after the first non-flag token so
            // `covenant intent search --json command help` keeps the
            // literal text `search --json command help` instead of
            // silently dropping the `--json` in the middle.
            let mut as_json = false;
            let mut prefer_stream = false;
            let mut text_parts: Vec<String> = Vec::new();
            let mut consuming_flags = true;
            for arg in args.iter().skip(1) {
                if consuming_flags && arg == "--json" {
                    as_json = true;
                } else if consuming_flags && arg == "--stream" {
                    prefer_stream = true;
                } else {
                    consuming_flags = false;
                    text_parts.push(arg.clone());
                }
            }
            if text_parts.is_empty() {
                eprintln!("covenant intent: missing intent text");
                std::process::exit(2);
            }
            let request_text = text_parts.join(" ");
            write_frame(
                &mut stream,
                &Request::SubmitIntent {
                    text: request_text,
                    prefer_stream: prefer_stream.then_some(true),
                },
            )
            .await?;
            let response = match read_response_or_stream(&mut stream).await? {
                ResponseOrStream::Terminal(r) => r,
                ResponseOrStream::Stream(collected) => {
                    if collected.response_kind != "intent_result" {
                        bail!(
                            "unexpected stream response_kind '{}' (expected 'intent_result')",
                            collected.response_kind
                        );
                    }
                    decode_intent_stream(collected.chunks, collected.summary)?
                }
            };
            match response {
                Response::IntentResult {
                    intent_id,
                    status,
                    text,
                    sources,
                    settlement,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intent_result_json(
                                intent_id,
                                &status,
                                &text,
                                &sources,
                                settlement.as_ref(),
                            ))?
                        );
                    } else {
                        println!("{text}");
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "memory" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut tier: Option<MemoryTier> = None;
                    let mut limit: usize = 10;
                    let mut as_json = false;
                    let mut prefer_stream = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--tier" => {
                                i += 1;
                                let v = args.get(i).context("--tier needs a value")?;
                                tier = Some(parse_tier(v)?);
                            }
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            "--stream" => prefer_stream = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::RecentMemory {
                            tier,
                            limit,
                            prefer_stream: prefer_stream.then_some(true),
                        },
                    )
                    .await?;
                    print_memory_response(
                        &mut stream,
                        as_json.then_some(MemoryReadJsonArgs {
                            mode: "recent",
                            tier,
                            limit,
                            query: None,
                            min_relevance: None,
                        }),
                    )
                    .await?;
                }
                "purge" => {
                    let mut tier: Option<MemoryTier> = None;
                    let mut before_ms: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--tier" => {
                                i += 1;
                                let v = args.get(i).context("--tier needs a value")?;
                                tier = Some(parse_tier(v)?);
                            }
                            "--before-ms" => {
                                i += 1;
                                let v = args.get(i).context("--before-ms needs a value")?;
                                before_ms = Some(
                                    v.parse()
                                        .context("--before-ms must be an integer (epoch ms)")?,
                                );
                            }
                            "--older-than-ms" => {
                                i += 1;
                                let v = args.get(i).context("--older-than-ms needs a value")?;
                                let dur: u64 =
                                    v.parse().context("--older-than-ms must be an integer")?;
                                before_ms = Some(epoch_ms().saturating_sub(dur));
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeMemory { tier, before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::MemoryPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&memory_purge_json(
                                        tier, before_ms, purged
                                    ))?
                                );
                            } else {
                                println!("purged {purged} record(s)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "compact" | "plan-compaction" => {
                    let plan_only = args[1] == "plan-compaction";
                    let mut policy = MemoryCompactionPolicy::default();
                    let mut reason = None;
                    let mut apply = false;
                    let mut as_json = plan_only;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--apply" if plan_only => {
                                bail!("memory plan-compaction is read-only and does not accept --apply")
                            }
                            "--apply" => apply = true,
                            "--json" => as_json = true,
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--detach-stale-parents" => policy.detach_stale_parents = true,
                            "--delete-working-before-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-working-before-ms needs a value")?;
                                policy.delete_working_before_ms = Some(
                                    v.parse()
                                        .context("--delete-working-before-ms must be an integer")?,
                                );
                            }
                            "--delete-working-older-than-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-working-older-than-ms needs a value")?;
                                let dur: u64 = v
                                    .parse()
                                    .context("--delete-working-older-than-ms must be an integer")?;
                                policy.delete_working_before_ms =
                                    Some(epoch_ms().saturating_sub(dur));
                            }
                            "--delete-episodic-before-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-episodic-before-ms needs a value")?;
                                policy.delete_episodic_before_ms =
                                    Some(v.parse().context(
                                        "--delete-episodic-before-ms must be an integer",
                                    )?);
                            }
                            "--delete-episodic-older-than-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-episodic-older-than-ms needs a value")?;
                                let dur: u64 = v.parse().context(
                                    "--delete-episodic-older-than-ms must be an integer",
                                )?;
                                policy.delete_episodic_before_ms =
                                    Some(epoch_ms().saturating_sub(dur));
                            }
                            "--mark-longterm-stale-before-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--mark-longterm-stale-before-ms needs a value")?;
                                policy.mark_longterm_stale_before_ms = Some(v.parse().context(
                                    "--mark-longterm-stale-before-ms must be an integer",
                                )?);
                            }
                            "--mark-longterm-stale-older-than-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--mark-longterm-stale-older-than-ms needs a value")?;
                                let dur: u64 = v.parse().context(
                                    "--mark-longterm-stale-older-than-ms must be an integer",
                                )?;
                                policy.mark_longterm_stale_before_ms =
                                    Some(epoch_ms().saturating_sub(dur));
                            }
                            "--marked-at-ms" => {
                                i += 1;
                                let v = args.get(i).context("--marked-at-ms needs a value")?;
                                policy.marked_at_ms =
                                    Some(v.parse().context("--marked-at-ms must be an integer")?);
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let request = MemoryCompactionRequest {
                        mode: if apply {
                            MemoryRepairMode::Apply
                        } else {
                            MemoryRepairMode::DryRun
                        },
                        policy,
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::CompactMemory { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    if plan_only {
                        print_memory_compaction_plan_response(response, as_json)?;
                    } else {
                        print_memory_compaction_response(response, as_json)?;
                    }
                }
                "plan-receipt-backfill" => {
                    let mut limit: usize = 100;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--apply" => {
                                bail!(
                                    "memory plan-receipt-backfill is read-only and does not accept --apply"
                                )
                            }
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }

                    write_frame(
                        &mut stream,
                        &Request::RecentMemory {
                            tier: None,
                            limit,
                            prefer_stream: None,
                        },
                    )
                    .await?;
                    let memories = match read_frame::<_, Response>(&mut stream).await? {
                        Response::Memories { records } => records,
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    };

                    write_frame(
                        &mut stream,
                        &Request::RecentReceipts {
                            limit,
                            since_ms: None,
                        },
                    )
                    .await?;
                    let receipts = match read_frame::<_, Response>(&mut stream).await? {
                        Response::Receipts { receipts } => receipts,
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    };

                    let plan = memory_receipt_backfill_plan_json(limit, &memories, &receipts);
                    if as_json {
                        println!("{}", serde_json::to_string(&plan)?);
                    } else {
                        let records = plan["records"].as_array().map(Vec::len).unwrap_or(0);
                        let unmatched_receipts = plan["unmatched_legacy_receipts"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0);
                        let unmatched_memory = plan["unmatched_memory_records"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0);
                        println!(
                            "receipt backfill plan: {records} candidate(s), {unmatched_receipts} unmatched legacy receipt(s), {unmatched_memory} unmatched memory record(s)"
                        );
                        println!("mutation: unsupported; this command only emits a dry-run plan");
                    }
                }
                "backfill-receipt-correlation" => {
                    let mut dry_run = false;
                    let mut as_json = false;
                    let mut scope_pubkey = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--dry-run" => dry_run = true,
                            "--json" => as_json = true,
                            "--scope-pubkey" => {
                                i += 1;
                                scope_pubkey = Some(
                                    args.get(i).context("--scope-pubkey needs a value")?.clone(),
                                );
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::BackfillMemoryRecords {
                            dry_run,
                            scope_pubkey,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::MemoryRecordsBackfilled {
                            row_count,
                            savepoint_name,
                            dry_run,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&memory_backfill_json(
                                        row_count,
                                        &savepoint_name,
                                        dry_run
                                    ))?
                                );
                            } else {
                                println!("row_count: {row_count}");
                                println!("dry_run: {dry_run}");
                                println!("savepoint_name: {savepoint_name}");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "repair" => {
                    if args.len() < 4 {
                        bail!(
                            "covenant memory repair: expected detach-parent|delete|backfill-provenance <id>"
                        );
                    }
                    let id = parse_uuid(&args[3], "memory-id")?;
                    let mut reason = None;
                    let mut apply = false;
                    let mut expected_parent = None;
                    let mut provenance = None;
                    let mut i = 4;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--apply" => apply = true,
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--expected-parent" => {
                                i += 1;
                                let v = args.get(i).context("--expected-parent needs a value")?;
                                expected_parent = Some(parse_uuid(v, "--expected-parent")?);
                            }
                            "--provenance" => {
                                i += 1;
                                let v = args.get(i).context("--provenance needs a value")?;
                                provenance = Some(
                                    serde_json::from_str(v)
                                        .context("--provenance must be valid JSON")?,
                                );
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let command = match args[2].as_str() {
                        "detach-parent" => {
                            if provenance.is_some() {
                                bail!("detach-parent does not accept --provenance");
                            }
                            MemoryRepairCommand::DetachParent {
                                id,
                                expected_parent,
                            }
                        }
                        "delete" => {
                            if expected_parent.is_some() || provenance.is_some() {
                                bail!("delete accepts only --reason and --apply");
                            }
                            MemoryRepairCommand::DeleteRecord { id }
                        }
                        "backfill-provenance" => {
                            if expected_parent.is_some() {
                                bail!("backfill-provenance does not accept --expected-parent");
                            }
                            MemoryRepairCommand::BackfillProvenance {
                                id,
                                provenance: provenance.context("missing --provenance JSON")?,
                            }
                        }
                        other => bail!(
                            "unknown memory repair action '{other}' (expected detach-parent|delete|backfill-provenance)"
                        ),
                    };
                    let request = MemoryRepairRequest {
                        mode: if apply {
                            MemoryRepairMode::Apply
                        } else {
                            MemoryRepairMode::DryRun
                        },
                        command,
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::RepairMemory { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    print_memory_repair_response(response)?;
                }
                "search" => {
                    if args.len() < 3 {
                        eprintln!("covenant memory search: missing <query>");
                        std::process::exit(2);
                    }
                    let mut tier: Option<MemoryTier> = None;
                    let mut limit: usize = 10;
                    let mut min_relevance: Option<f32> = None;
                    let mut as_json = false;
                    let mut query_parts: Vec<String> = Vec::new();
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--tier" => {
                                i += 1;
                                let v = args.get(i).context("--tier needs a value")?;
                                tier = Some(parse_tier(v)?);
                            }
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--min-relevance" => {
                                i += 1;
                                let v = args.get(i).context("--min-relevance needs a value")?;
                                let parsed: f32 = v
                                    .parse()
                                    .context("--min-relevance must be a float in [0.0, 1.0]")?;
                                if !parsed.is_finite() || !(0.0..=1.0).contains(&parsed) {
                                    bail!(
                                        "--min-relevance must be a finite float in [0.0, 1.0]; got {v:?}"
                                    );
                                }
                                min_relevance = Some(parsed);
                            }
                            "--json" => as_json = true,
                            other => query_parts.push(other.to_string()),
                        }
                        i += 1;
                    }
                    let query = query_parts.join(" ");
                    if query.is_empty() {
                        bail!("query text is required");
                    }
                    write_frame(
                        &mut stream,
                        &Request::SearchMemory {
                            query: query.clone(),
                            tier,
                            limit,
                            min_relevance,
                        },
                    )
                    .await?;
                    print_memory_response(
                        &mut stream,
                        as_json.then_some(MemoryReadJsonArgs {
                            mode: "search",
                            tier,
                            limit,
                            query: Some(query),
                            min_relevance,
                        }),
                    )
                    .await?;
                }
                other => {
                    eprintln!("covenant memory: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "capabilities" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut limit: usize = 10;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RecentCapabilities { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::Capabilities { capabilities } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capability_list_json(
                                        limit,
                                        &capabilities
                                    ))?
                                );
                            } else if capabilities.is_empty() {
                                println!("(no capabilities granted)");
                            } else {
                                for c in capabilities {
                                    let exp = match c.capability.expires_at {
                                        Some(ms) => format!("expires {ms}"),
                                        None => "perpetual".into(),
                                    };
                                    let action_label =
                                        match covenant_permissions::friendly_action_title(
                                            &c.capability.action,
                                        ) {
                                            Some(title) => {
                                                format!("{title} ({})", c.capability.action)
                                            }
                                            None => c.capability.action.clone(),
                                        };
                                    println!(
                                        "{} → {} ({}) [{}]",
                                        c.capability.subject.display,
                                        action_label,
                                        c.capability.granted_by.display,
                                        exp
                                    );
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "grant" => {
                    if args.len() < 3 {
                        eprintln!("covenant capabilities grant: missing <action>");
                        std::process::exit(2);
                    }
                    let action = args[2].clone();
                    let mut scope: Option<serde_json::Value> = None;
                    let mut expires_at: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--scope" => {
                                i += 1;
                                let v = args.get(i).context("--scope needs a JSON value")?;
                                scope =
                                    Some(serde_json::from_str(v).context("--scope must be JSON")?);
                            }
                            "--expires-at" => {
                                i += 1;
                                let v = args.get(i).context("--expires-at needs a value")?;
                                expires_at = Some(
                                    v.parse()
                                        .context("--expires-at must be an integer (epoch ms)")?,
                                );
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let action = match peer_prefix_to_lookup(&action) {
                        Some(prefix) => {
                            write_frame(
                                &mut stream,
                                &Request::ListPeers {
                                    limit: PEER_LOOKUP_LIMIT,
                                    pubkey_prefix: Some(prefix.to_string()),
                                    status_filter: None,
                                },
                            )
                            .await?;
                            let peers = match read_frame::<_, Response>(&mut stream).await? {
                                Response::PeerList { peers, .. } => peers,
                                Response::Error { message } => {
                                    bail!("daemon error during peer lookup: {message}")
                                }
                                other => bail!(
                                    "unexpected response to ListPeers during grant expansion: {other:?}"
                                ),
                            };
                            match expand_a2a_action(&action, &peers) {
                                Ok(ExpandOutcome::Unchanged) => action,
                                Ok(ExpandOutcome::Rewritten { full, .. }) => {
                                    eprintln!("expanding {prefix} → {full}");
                                    full
                                }
                                Err(err) => {
                                    print_expand_error(&err);
                                    std::process::exit(1);
                                }
                            }
                        }
                        None => action,
                    };
                    write_frame(
                        &mut stream,
                        &Request::GrantCapability {
                            action: action.clone(),
                            scope: scope.clone(),
                            expires_at,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilityGranted {
                            signature_b58,
                            subject_display,
                            action,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capability_grant_json(
                                        &subject_display,
                                        &action,
                                        &signature_b58,
                                        scope.as_ref(),
                                        expires_at,
                                    ))?
                                );
                            } else {
                                let action_label =
                                    match covenant_permissions::friendly_action_title(&action) {
                                        Some(title) => format!("{title} ({action})"),
                                        None => action.clone(),
                                    };
                                println!("granted: {subject_display} → {action_label}");
                                println!("signature: {signature_b58}");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "revoke" => {
                    if args.len() < 3 {
                        eprintln!("covenant capabilities revoke: missing <signature-b58>");
                        std::process::exit(2);
                    }
                    let signature_b58 = args[2].clone();
                    let mut as_json = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::RevokeCapability {
                            signature_b58: signature_b58.clone(),
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilityRevoked {
                            signature_b58,
                            removed,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capability_revoke_json(
                                        &signature_b58,
                                        removed,
                                    ))?
                                );
                            } else if removed {
                                println!("revoked: {signature_b58}");
                            } else {
                                println!("(no live capability with that signature)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--before-ms" => {
                                i += 1;
                                let v = args.get(i).context("--before-ms needs a value")?;
                                before_ms = Some(
                                    v.parse()
                                        .context("--before-ms must be an integer (epoch ms)")?,
                                );
                            }
                            "--older-than-ms" => {
                                i += 1;
                                let v = args.get(i).context("--older-than-ms needs a value")?;
                                let dur: u64 =
                                    v.parse().context("--older-than-ms must be an integer")?;
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                before_ms = Some(now.saturating_sub(dur));
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeCapabilities { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilitiesPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capabilities_purge_json(
                                        before_ms, purged
                                    ))?
                                );
                            } else {
                                println!("purged {purged} revoked capability(ies)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant capabilities: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "receipts" => {
            if args.len() < 2 || args[1] != "recent" {
                print_usage();
                std::process::exit(2);
            }
            let mut limit: usize = 10;
            let mut as_json = false;
            let mut since_ms: Option<u64> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "-n" | "--limit" => {
                        i += 1;
                        let v = args.get(i).context("--limit needs a value")?;
                        limit = v.parse().context("--limit must be an integer")?;
                    }
                    "--since-ms" => {
                        i += 1;
                        let v = args.get(i).context("--since-ms needs a value")?;
                        since_ms = Some(v.parse().context("--since-ms must be an integer")?);
                    }
                    "--json" => as_json = true,
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::RecentReceipts { limit, since_ms }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Receipts { receipts } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&receipt_list_json(limit, since_ms, &receipts))?
                        );
                    } else if receipts.is_empty() {
                        println!("(no receipts)");
                    } else {
                        for r in receipts {
                            let resource = resource_name(r.resource);
                            let onchain = match r.tx_sig.as_ref().or(r.onchain_sig.as_ref()) {
                                Some(s) => s.as_str(),
                                None => "(local-only)",
                            };
                            println!(
                                "[{}] {resource}: {} credits — {onchain}",
                                r.settled_at, r.credits_consumed
                            );
                        }
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "chain" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "status" => {
                    let mut as_json = false;
                    for arg in &args[2..] {
                        match arg.as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                    }
                    write_frame(&mut stream, &Request::ChainStatus).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ChainStatus { status } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&chain_status_json(&status))?);
                            } else {
                                println!("chain: {}", status.chain);
                                println!("cluster: {}", status.cluster);
                                println!(
                                    "rpc_url: {}",
                                    status.rpc_url.as_deref().unwrap_or("(unset)")
                                );
                                println!(
                                    "ws_url: {}",
                                    status.ws_url.as_deref().unwrap_or("(unset)")
                                );
                                println!(
                                    "program_id: {}",
                                    status.program_id.as_deref().unwrap_or("(unset)")
                                );
                                println!(
                                    "covnt_mint: {}",
                                    status.covnt_mint.as_deref().unwrap_or("(unset)")
                                );
                                if status.ready {
                                    println!("ready: true");
                                } else {
                                    println!("ready: false ({})", status.missing.join(", "));
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "flush-receipts" => {
                    let mut limit = 10;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::FlushReceipts { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ReceiptBatchFlushed {
                            batch,
                            receipts_updated,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&flush_receipts_json(
                                        limit,
                                        &batch,
                                        receipts_updated
                                    ))?
                                );
                            } else {
                                println!("batch_id: {}", batch.batch_id);
                                println!("merkle_root: {}", batch.merkle_root);
                                println!("receipt_count: {}", batch.receipt_count);
                                println!("receipts_updated: {receipts_updated}");
                                println!(
                                    "tx_sig: {}",
                                    batch.tx_sig.as_deref().unwrap_or("(pending)")
                                );
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "receipt-batches" => {
                    let mut limit = 10;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::ReceiptBatches { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ReceiptBatches { batches } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&receipt_batch_list_json(
                                        limit, &batches
                                    ))?
                                );
                            } else if batches.is_empty() {
                                println!("(no receipt batches)");
                            } else {
                                for batch in batches {
                                    let tx_sig = batch.tx_sig.as_deref().unwrap_or("(pending)");
                                    let slot = batch
                                        .slot
                                        .map(|slot| slot.to_string())
                                        .unwrap_or_else(|| "(pending)".to_string());
                                    println!(
                                        "{} {} receipts root={} tx={} slot={}",
                                        batch.batch_id,
                                        batch.receipt_count,
                                        batch.merkle_root,
                                        tx_sig,
                                        slot
                                    );
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "register-agent" => {
                    run_chain_register_agent(&args[2..]).await?;
                }
                "stake" => {
                    run_chain_stake(&args[2..]).await?;
                }
                "buy-credits" => {
                    run_chain_buy_credits(&args[2..]).await?;
                }
                "initialize" => {
                    run_chain_initialize(&args[2..]).await?;
                }
                "open-credit-account" => {
                    run_chain_open_credit_account(&args[2..]).await?;
                }
                "unstake" => {
                    run_chain_unstake(&args[2..]).await?;
                }
                "close-position" => {
                    run_chain_close_position(&args[2..]).await?;
                }
                "migrate-config" => {
                    run_chain_migrate_config(&args[2..]).await?;
                }
                "set-min-stake-lock" => {
                    run_chain_set_config_u64(
                        &args[2..],
                        "set-min-stake-lock",
                        "set_min_stake_lock",
                    )
                    .await?;
                }
                "set-credits-per-covnt" => {
                    run_chain_set_config_u64(
                        &args[2..],
                        "set-credits-per-covnt",
                        "set_credits_per_covnt",
                    )
                    .await?;
                }
                "update-authority" => {
                    run_chain_set_config_pubkey(&args[2..], "update-authority", "update_authority")
                        .await?;
                }
                "update-slash-authority" => {
                    run_chain_set_config_pubkey(
                        &args[2..],
                        "update-slash-authority",
                        "update_slash_authority",
                    )
                    .await?;
                }
                "update-treasury" => {
                    run_chain_update_treasury(&args[2..]).await?;
                }
                other => bail!("unknown chain subcommand '{other}'"),
            }
        }
        "verify" => {
            let mut window: usize = 100;
            let mut as_json = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--window" | "-w" => {
                        i += 1;
                        let v = args.get(i).context("--window needs a value")?;
                        window = v.parse().context("--window must be an integer")?;
                    }
                    "--json" => as_json = true,
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::Verify { window }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::VerifyReport {
                    window,
                    checks,
                    drift,
                    orphans_total,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&verify_report_json(
                                window,
                                &checks,
                                &drift,
                                orphans_total
                            ))?
                        );
                    } else {
                        println!("verify (last {window} records):");
                        for c in &checks {
                            let mark = if c.passed { "✓" } else { "✗" };
                            println!("  {mark} {} — {}", c.name, c.message);
                        }
                        if !drift.is_empty() {
                            println!("drift:");
                            for item in &drift {
                                let id = item.id.as_deref().unwrap_or("-");
                                println!("  - {} [{}] — {}", item.kind, id, item.message);
                                println!("    repair: {}", item.repair);
                            }
                        }
                        println!("orphans total: {orphans_total}");
                    }
                    if orphans_total > 0 {
                        std::process::exit(1);
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "tools" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "list" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::ListTools).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ToolList { tools } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&tool_list_json(&tools))?);
                            } else if tools.is_empty() {
                                println!("(no tools registered)");
                            } else {
                                for t in tools {
                                    println!("{} — {}", t.name, t.description);
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "call" => {
                    if args.len() < 3 {
                        eprintln!("covenant tools call: missing <name>");
                        std::process::exit(2);
                    }
                    let name = args[2].clone();
                    let mut arguments = serde_json::Value::Null;
                    let mut as_json = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--args" => {
                                i += 1;
                                let v = args.get(i).context("--args needs a value")?;
                                arguments =
                                    serde_json::from_str(v).context("--args must be valid JSON")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::CallTool {
                            name: name.clone(),
                            arguments,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ToolResult { content, is_error } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&tool_result_json(
                                        &name, &content, is_error,
                                    ))?
                                );
                            } else {
                                for c in &content {
                                    match c {
                                        covenant_mcp::Content::Text { text } => println!("{text}"),
                                        covenant_mcp::Content::Json { value } => {
                                            println!("{}", serde_json::to_string_pretty(value)?);
                                        }
                                    }
                                }
                            }
                            if is_error {
                                std::process::exit(1);
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant tools: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "audit" => {
            if args.len() < 2 {
                eprintln!("covenant audit: expected `recent`, `verify`, or `purge`");
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut limit: usize = 50;
                    let mut since_ms: Option<u64> = None;
                    let mut as_json = false;
                    let mut prefer_stream = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--since-ms" => {
                                i += 1;
                                let v = args.get(i).context("--since-ms needs a value")?;
                                since_ms =
                                    Some(v.parse().context("--since-ms must be an integer")?);
                            }
                            "--json" => as_json = true,
                            "--stream" => prefer_stream = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::RecentAudit {
                            limit,
                            since_ms,
                            prefer_stream: prefer_stream.then_some(true),
                        },
                    )
                    .await?;
                    let response = match read_response_or_stream(&mut stream).await? {
                        ResponseOrStream::Terminal(r) => r,
                        ResponseOrStream::Stream(collected) => {
                            if collected.response_kind != "audit_events" {
                                bail!(
                                    "unexpected stream response_kind '{}' (expected 'audit_events')",
                                    collected.response_kind
                                );
                            }
                            let events = decode_audit_chunks(collected.chunks)?;
                            Response::AuditEvents { events }
                        }
                    };
                    match response {
                        Response::AuditEvents { events } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&audit_recent_json(
                                        limit, since_ms, &events
                                    ))?
                                );
                            } else {
                                // Default JSONL mirrors `audit/events.jsonl`, so tail/grep/jq
                                // users see the same row shape as the durable log.
                                if events.is_empty() {
                                    println!("(no audit events)");
                                }
                                for e in events {
                                    println!("{}", serde_json::to_string(&e)?);
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "verify" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::VerifyAuditIntegrity).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditIntegrity { report } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&audit_verify_json(&report))?);
                            } else {
                                println!("{}", serde_json::to_string(&report)?);
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--before-ms" => {
                                i += 1;
                                let v = args.get(i).context("--before-ms needs a value")?;
                                before_ms = Some(
                                    v.parse()
                                        .context("--before-ms must be an integer (epoch ms)")?,
                                );
                            }
                            "--older-than-ms" => {
                                i += 1;
                                let v = args.get(i).context("--older-than-ms needs a value")?;
                                let dur: u64 =
                                    v.parse().context("--older-than-ms must be an integer")?;
                                before_ms = Some(epoch_ms().saturating_sub(dur));
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeAudit { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&audit_purge_json(before_ms, purged))?
                                );
                            } else {
                                println!("purged {purged} event(s)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant audit: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "a2a" => {
            if args.len() < 2 {
                eprintln!(
                    "covenant a2a: expected `status`, `requeue`, `force-error`, `retry-stale`, or `compact`"
                );
                std::process::exit(2);
            }
            match args[1].as_str() {
                "status" => {
                    let mut limit: usize = 10;
                    let mut min_lease_age_ms: Option<u64> = None;
                    let mut deadline_within_ms: Option<u64> = None;
                    let mut state_filter: Option<A2ATaskQueueState> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--min-lease-age-ms" => {
                                i += 1;
                                let v = args.get(i).context("--min-lease-age-ms needs a value")?;
                                min_lease_age_ms = Some(
                                    v.parse().context("--min-lease-age-ms must be an integer")?,
                                );
                            }
                            "--deadline-within-ms" => {
                                i += 1;
                                let v =
                                    args.get(i).context("--deadline-within-ms needs a value")?;
                                deadline_within_ms = Some(
                                    v.parse()
                                        .context("--deadline-within-ms must be an integer")?,
                                );
                            }
                            "--state" => {
                                i += 1;
                                let v = args.get(i).context("--state needs a value")?;
                                state_filter = Some(parse_a2a_queue_state(v)?);
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::A2AQueue {
                            limit,
                            min_lease_age_ms,
                            deadline_within_ms,
                            state_filter,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2AQueue { tasks, results } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&a2a_status_json(
                                        limit,
                                        min_lease_age_ms,
                                        deadline_within_ms,
                                        state_filter,
                                        &tasks,
                                        &results
                                    ))?
                                );
                            } else {
                                if tasks.is_empty() && results.is_empty() {
                                    println!("(a2a queue empty)");
                                }
                                for entry in tasks {
                                    println!(
                                        "{}",
                                        serde_json::json!({ "type": "task", "entry": entry })
                                    );
                                }
                                for result in results {
                                    println!(
                                        "{}",
                                        serde_json::json!({ "type": "result", "result": result })
                                    );
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "requeue" => {
                    if args.len() < 3 {
                        bail!(
                            "covenant a2a requeue: missing <task-id> --reason TEXT --duplicate-risk idempotent|operator-accepted"
                        );
                    }
                    let task_id = parse_uuid(&args[2], "task-id")?;
                    let mut lease_id = None;
                    let mut reason = None;
                    let mut duplicate_risk = None;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--lease-id" => {
                                i += 1;
                                let v = args.get(i).context("--lease-id needs a value")?;
                                lease_id = Some(parse_uuid(v, "--lease-id")?);
                            }
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--duplicate-risk" => {
                                i += 1;
                                let v = args.get(i).context("--duplicate-risk needs a value")?;
                                duplicate_risk = Some(parse_duplicate_risk(v)?);
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let request = A2ARepairRequest {
                        task_id,
                        command: A2ARepairCommand::Requeue {
                            lease_id,
                            duplicate_risk: duplicate_risk
                                .context("missing --duplicate-risk idempotent|operator-accepted")?,
                        },
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::RepairA2ATask { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    print_a2a_repair_response(response)?;
                }
                "force-error" => {
                    if args.len() < 3 {
                        bail!(
                            "covenant a2a force-error: missing <task-id> --reason TEXT --message TEXT"
                        );
                    }
                    let task_id = parse_uuid(&args[2], "task-id")?;
                    let mut lease_id = None;
                    let mut reason = None;
                    let mut message = None;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--lease-id" => {
                                i += 1;
                                let v = args.get(i).context("--lease-id needs a value")?;
                                lease_id = Some(parse_uuid(v, "--lease-id")?);
                            }
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--message" => {
                                i += 1;
                                message =
                                    Some(args.get(i).context("--message needs a value")?.clone());
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let request = A2ARepairRequest {
                        task_id,
                        command: A2ARepairCommand::ForceError {
                            lease_id,
                            message: message.context("missing --message")?,
                        },
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::RepairA2ATask { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    print_a2a_repair_response(response)?;
                }
                "retry-stale" => {
                    let mut policy = A2AAutoRetryPolicy::default();
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--enable" => policy.enabled = true,
                            "--min-lease-age-ms" => {
                                i += 1;
                                let v = args.get(i).context("--min-lease-age-ms needs a value")?;
                                policy.min_lease_age_ms =
                                    v.parse().context("--min-lease-age-ms must be an integer")?;
                            }
                            "--max-attempts" => {
                                i += 1;
                                let v = args.get(i).context("--max-attempts needs a value")?;
                                policy.max_attempts =
                                    v.parse().context("--max-attempts must be an integer")?;
                            }
                            "--max-requeues" => {
                                i += 1;
                                let v = args.get(i).context("--max-requeues needs a value")?;
                                policy.max_requeues =
                                    v.parse().context("--max-requeues must be an integer")?;
                            }
                            "--scan-limit" => {
                                i += 1;
                                let v = args.get(i).context("--scan-limit needs a value")?;
                                policy.scan_limit =
                                    v.parse().context("--scan-limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RetryA2AStale { policy }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2AAutoRetried { report } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&a2a_retry_json(&report))?);
                            } else {
                                println!(
                                    "considered {} task(s), requeued {}, skipped {}",
                                    report.considered,
                                    report.requeued.len(),
                                    report.skipped.len()
                                );
                                if !report.policy.enabled {
                                    println!("automatic retry disabled; pass --enable to mutate");
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "compact" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::CompactA2A).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2ACompacted { dropped } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&a2a_compact_json(dropped))?);
                            } else {
                                println!("dropped {dropped} a2a event(s)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant a2a: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "peers" => {
            if args.len() < 2 {
                eprintln!("covenant peers: expected `purge`, `rotate`, `list`, or `revoke`");
                std::process::exit(2);
            }
            match args[1].as_str() {
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--before-ms" => {
                                i += 1;
                                let v = args.get(i).context("--before-ms needs a value")?;
                                before_ms = Some(
                                    v.parse()
                                        .context("--before-ms must be an integer (epoch ms)")?,
                                );
                            }
                            "--older-than-ms" => {
                                i += 1;
                                let v = args.get(i).context("--older-than-ms needs a value")?;
                                let dur: u64 =
                                    v.parse().context("--older-than-ms must be an integer")?;
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                before_ms = Some(now.saturating_sub(dur));
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgePeers { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeersPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&peers_purge_json(before_ms, purged))?
                                );
                            } else {
                                println!("purged {purged} revoked peer(s)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "rotate" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RotateOperatorToken).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::OperatorTokenRotated { token_b58 } => {
                            // The daemon already wrote the new token to
                            // `$COVENANT_HOME/peers/operator.token` (mode
                            // 0600); print it here so the operator can
                            // copy it into a web UI's
                            // `.env.development.local`. Any existing
                            // shells holding the old token need to
                            // re-read the file.
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&peers_rotate_json(&token_b58))?
                                );
                            } else {
                                println!(
                                    "rotated. new token (also written to peers/operator.token):"
                                );
                                println!("{token_b58}");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "list" => {
                    let mut limit: usize = 20;
                    let mut prefix: Option<String> = None;
                    let mut live_only = false;
                    let mut revoked_only = false;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--prefix" => {
                                i += 1;
                                let v = args.get(i).context("--prefix needs a value")?;
                                prefix = Some(v.clone());
                            }
                            "--live-only" => live_only = true,
                            "--revoked-only" => revoked_only = true,
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let status_filter = peers_list_status_filter(live_only, revoked_only)?;
                    write_frame(
                        &mut stream,
                        &Request::ListPeers {
                            limit,
                            pubkey_prefix: prefix.clone(),
                            status_filter,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeerList {
                            peers,
                            operator_pubkey_b58,
                            truncated,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&peer_list_json(
                                        limit,
                                        prefix.as_deref(),
                                        &peers,
                                        &operator_pubkey_b58,
                                        truncated,
                                    ))?
                                );
                            } else {
                                for line in peer_list_lines(&peers, &operator_pubkey_b58, truncated)
                                {
                                    println!("{line}");
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "revoke" => {
                    let force = args.iter().any(|a| a == "--force");
                    let mut match_limit: Option<usize> = None;
                    let mut token_prefix: Option<String> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--force" => {}
                            "--json" => as_json = true,
                            "--limit-matches" => {
                                i += 1;
                                let v = args.get(i).context("--limit-matches needs a value")?;
                                let n: usize = v
                                    .parse()
                                    .context("--limit-matches must be a positive integer")?;
                                if n == 0 {
                                    bail!("--limit-matches must be at least 1");
                                }
                                match_limit = Some(n);
                            }
                            other if !other.starts_with("--") && token_prefix.is_none() => {
                                token_prefix = Some(other.to_string());
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let token_prefix = token_prefix
                        .context("covenant peers revoke: missing TOKEN-PREFIX argument")?;
                    write_frame(
                        &mut stream,
                        &Request::RevokePeer {
                            token_prefix,
                            force,
                            match_limit,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeerRevoked { outcome } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&peer_revoke_json(&outcome))?);
                                if peer_revoke_is_failure(&outcome) {
                                    std::process::exit(1);
                                }
                                return Ok(());
                            }
                            match outcome {
                                RevokeOutcome::Revoked(s) => {
                                    println!(
                                        "revoked\t{display}\t{pubkey}\t{prefix}…\trevoked@{revoked}",
                                        display = s.agent_id.display,
                                        pubkey = s.agent_id.pubkey_base58(),
                                        prefix = s.token_prefix,
                                        revoked = s.revoked_at.unwrap_or(0),
                                    );
                                }
                                RevokeOutcome::AlreadyRevoked(s) => {
                                    println!(
                                        "already revoked at {revoked}: {display}\t{pubkey}\t{prefix}…",
                                        display = s.agent_id.display,
                                        pubkey = s.agent_id.pubkey_base58(),
                                        prefix = s.token_prefix,
                                        revoked = s.revoked_at.unwrap_or(0),
                                    );
                                }
                                RevokeOutcome::NotFound => {
                                    eprintln!("no peer matched the supplied prefix");
                                    std::process::exit(1);
                                }
                                RevokeOutcome::Ambiguous { matches, truncated } => {
                                    for line in peer_revoke_ambiguous_lines(&matches, truncated) {
                                        eprintln!("{line}");
                                    }
                                    std::process::exit(1);
                                }
                                RevokeOutcome::SelfRevokeForbidden(s) => {
                                    eprintln!(
                                        "refused to revoke the operator's own bootstrap token: {display}\t{pubkey}\t{prefix}…",
                                        display = s.agent_id.display,
                                        pubkey = s.agent_id.pubkey_base58(),
                                        prefix = s.token_prefix,
                                    );
                                    eprintln!(
                                        "  use `covenant peers rotate` to retire the current token without bricking auth,"
                                    );
                                    eprintln!(
                                        "  or pass --force to override (this WILL brick auth; recover by deleting"
                                    );
                                    eprintln!(
                                        "  $COVENANT_HOME/peers/operator.token and restarting the daemon)."
                                    );
                                    std::process::exit(1);
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => bail!("covenant peers: unknown subcommand '{other}'"),
            }
        }
        "intents" => {
            if args.len() < 2 || args[1] != "resume" {
                eprintln!("covenant intents: expected `resume [--json] <intent-id>|latest`");
                std::process::exit(2);
            }
            let mut explicit_id: Option<String> = None;
            let mut want_latest = false;
            let mut as_json = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--latest" | "latest" => want_latest = true,
                    "--json" => as_json = true,
                    other if !other.starts_with("--") && explicit_id.is_none() => {
                        explicit_id = Some(other.to_string());
                    }
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }

            let mode = if want_latest { "latest" } else { "explicit" };

            let intent_id = match resolve_intents_resume_intent_id(
                &mut stream,
                want_latest,
                explicit_id.as_deref(),
            )
            .await
            {
                Ok(intent_id) => intent_id,
                Err(err) => {
                    if as_json {
                        let message = err.to_string();
                        let code = intents_resume_error_code(&message);
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_error_json(
                                mode, None, code, &message
                            ))?
                        );
                        std::process::exit(1);
                    }
                    return Err(err);
                }
            };

            write_frame(&mut stream, &Request::ResumeIntent { intent_id }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IntentResult {
                    intent_id,
                    status,
                    text,
                    sources,
                    settlement,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_ok_json(
                                mode,
                                intent_id,
                                &status,
                                &text,
                                &sources,
                                &settlement
                            ))?
                        );
                        return Ok(());
                    }
                    println!("{text}");
                    if !sources.is_empty() {
                        println!();
                        println!("sources:");
                        for s in sources {
                            println!("  - {s}");
                        }
                    }
                }
                Response::Error { message } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_error_json(
                                mode,
                                Some(intent_id),
                                "daemon_error",
                                &message
                            ))?
                        );
                        std::process::exit(1);
                    }
                    bail!("daemon error: {message}")
                }
                other => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_error_json(
                                mode,
                                Some(intent_id),
                                "unexpected_response",
                                &format!("{other:?}")
                            ))?
                        );
                        std::process::exit(1);
                    }
                    bail!("unexpected response: {other:?}")
                }
            }
        }
        "ignore" => {
            if args.len() < 2 || args[1] != "check" {
                eprintln!("covenant ignore: expected `check <text>`");
                std::process::exit(2);
            }
            let mut as_json = false;
            let mut text_parts = Vec::new();
            for arg in args.iter().skip(2) {
                if arg == "--json" {
                    as_json = true;
                } else {
                    text_parts.push(arg.as_str());
                }
            }
            if text_parts.is_empty() {
                eprintln!("covenant ignore check: missing <text>");
                std::process::exit(2);
            }
            let text = text_parts.join(" ");
            write_frame(&mut stream, &Request::IgnoreCheck { text }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IgnoreReport {
                    ignored,
                    matched_pattern,
                    rules_loaded,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&ignore_report_json(
                                ignored,
                                matched_pattern.as_deref(),
                                rules_loaded
                            ))?
                        );
                    } else if ignored {
                        let pat = matched_pattern.as_deref().unwrap_or("(none)");
                        println!("ignored — matched rule: {pat}");
                    } else {
                        println!("not ignored ({rules_loaded} rule(s) loaded)");
                    }
                    if ignored {
                        std::process::exit(1);
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "settlement" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "backfill-receipts" => {
                    let mut dry_run = false;
                    let mut as_json = false;
                    let mut scope_pubkey = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--dry-run" => dry_run = true,
                            "--json" => as_json = true,
                            "--scope-pubkey" => {
                                i += 1;
                                scope_pubkey = Some(
                                    args.get(i).context("--scope-pubkey needs a value")?.clone(),
                                );
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::BackfillSettlementReceipts {
                            dry_run,
                            scope_pubkey,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::SettlementReceiptsBackfilled {
                            row_count,
                            rollback_path,
                            dry_run,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&settlement_backfill_json(
                                        row_count,
                                        rollback_path.as_deref(),
                                        dry_run
                                    ))?
                                );
                            } else {
                                println!("row_count: {row_count}");
                                println!("dry_run: {dry_run}");
                                println!(
                                    "rollback_path: {}",
                                    rollback_path.as_deref().unwrap_or("(none)")
                                );
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant settlement: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "sap" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "status" => {
                    let mut as_json = false;
                    for arg in &args[2..] {
                        match arg.as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                    }
                    write_frame(&mut stream, &Request::SapStatus).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::SapStatus {
                            enabled,
                            cluster,
                            program_id,
                            rpc_url,
                            explorer_url,
                            has_signer,
                        } => {
                            if as_json {
                                let value = serde_json::json!({
                                    "kind": "sap_status",
                                    "enabled": enabled,
                                    "cluster": cluster,
                                    "program_id": program_id,
                                    "rpc_url": rpc_url,
                                    "explorer_url": explorer_url,
                                    "has_signer": has_signer,
                                });
                                println!("{}", serde_json::to_string(&value)?);
                            } else {
                                println!("enabled: {enabled}");
                                println!("cluster: {cluster}");
                                println!("program_id: {program_id}");
                                println!("rpc_url: {rpc_url}");
                                println!("explorer_url: {explorer_url}");
                                println!("has_signer: {has_signer}");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "publish" => {
                    let mut manifest_path: Option<String> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--manifest" => {
                                i += 1;
                                manifest_path = Some(args.get(i).cloned().ok_or_else(|| {
                                    anyhow::anyhow!("--manifest needs a file path")
                                })?);
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let manifest_path = manifest_path.ok_or_else(|| {
                        anyhow::anyhow!("covenant sap publish requires --manifest <file>")
                    })?;
                    let manifest_json = std::fs::read_to_string(&manifest_path)
                        .with_context(|| format!("read manifest at {manifest_path}"))?;
                    // Parse-and-reserialize round-trip rejects malformed
                    // JSON up front so the daemon never sees garbage.
                    let _: serde_json::Value = serde_json::from_str(&manifest_json)
                        .with_context(|| format!("parse manifest at {manifest_path}"))?;
                    write_frame(&mut stream, &Request::SapPublishAgent { manifest_json }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::SapPublishedAgent {
                            agent_pda,
                            signature,
                        } => {
                            if as_json {
                                let value = serde_json::json!({
                                    "kind": "sap_published_agent",
                                    "agent_pda": agent_pda,
                                    "signature": signature,
                                });
                                println!("{}", serde_json::to_string(&value)?);
                            } else {
                                println!("agent_pda: {agent_pda}");
                                println!("signature: {signature}");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant sap: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        other => {
            eprintln!("covenant: unknown command '{other}'");
            print_usage();
            std::process::exit(2);
        }
    }
    Ok(())
}

const PEER_LOOKUP_LIMIT: usize = 16;
const PEER_SCOPED_PREFIXES: &[&str] = &["a2a.send.", "a2a.recv.", "a2a.respond."];

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpandOutcome {
    Unchanged,
    Rewritten {
        full: String,
        peer_pubkey_b58: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpandError {
    NoMatch {
        tail: String,
    },
    Ambiguous {
        tail: String,
        matches: Vec<PeerSummary>,
    },
    RevokedOnly {
        tail: String,
        matches: Vec<PeerSummary>,
    },
}

fn peer_prefix_to_lookup(action: &str) -> Option<&str> {
    for prefix in PEER_SCOPED_PREFIXES {
        if let Some(tail) = action.strip_prefix(prefix) {
            if tail.is_empty() || tail.contains('.') || tail.contains('@') {
                return None;
            }
            return Some(tail);
        }
    }
    None
}

fn expand_a2a_action(
    action: &str,
    peers: &[PeerSummary],
) -> std::result::Result<ExpandOutcome, ExpandError> {
    let (prefix, tail) = match PEER_SCOPED_PREFIXES
        .iter()
        .find_map(|p| action.strip_prefix(p).map(|t| (p.trim_end_matches('.'), t)))
    {
        Some(pair) => pair,
        None => return Ok(ExpandOutcome::Unchanged),
    };
    if tail.is_empty() || tail.contains('.') || tail.contains('@') {
        return Ok(ExpandOutcome::Unchanged);
    }

    let mut live: Vec<PeerSummary> = Vec::new();
    let mut revoked: Vec<PeerSummary> = Vec::new();
    for p in peers {
        if !p.agent_id.pubkey_base58().starts_with(tail) {
            continue;
        }
        if p.revoked_at.is_some() {
            revoked.push(p.clone());
        } else {
            live.push(p.clone());
        }
    }

    match live.len() {
        1 => {
            let peer = &live[0];
            let pubkey = peer.agent_id.pubkey_base58();
            let full = format!("{prefix}.{pubkey}");
            Ok(ExpandOutcome::Rewritten {
                full,
                peer_pubkey_b58: pubkey,
            })
        }
        0 if revoked.is_empty() => Err(ExpandError::NoMatch {
            tail: tail.to_string(),
        }),
        0 => Err(ExpandError::RevokedOnly {
            tail: tail.to_string(),
            matches: revoked,
        }),
        _ => Err(ExpandError::Ambiguous {
            tail: tail.to_string(),
            matches: live,
        }),
    }
}

/// Resolve the two `peers list` status flags into a single filter. The
/// pair is mutually exclusive — `--live-only && --revoked-only` would
/// silently empty the result, which is operationally a footgun. Reject
/// at parse time so the operator's mistake fails loudly with no daemon
/// round-trip.
fn peers_list_status_filter(
    live_only: bool,
    revoked_only: bool,
) -> Result<Option<PeerStatusFilter>> {
    match (live_only, revoked_only) {
        (true, true) => bail!("--live-only and --revoked-only are mutually exclusive"),
        (true, false) => Ok(Some(PeerStatusFilter::Live)),
        (false, true) => Ok(Some(PeerStatusFilter::Revoked)),
        (false, false) => Ok(None),
    }
}

fn peer_list_json(
    limit: usize,
    filter_pubkey_prefix: Option<&str>,
    peers: &[PeerSummary],
    operator_pubkey_b58: &str,
    truncated: bool,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "peer_list",
        "limit": limit,
        "filter_pubkey_prefix": filter_pubkey_prefix,
        "matched_count": peers.len(),
        "peers": peers,
        "operator_pubkey_b58": operator_pubkey_b58,
        "truncated": truncated,
    })
}

fn peer_list_lines(
    peers: &[PeerSummary],
    operator_pubkey_b58: &str,
    truncated: bool,
) -> Vec<String> {
    if peers.is_empty() {
        return vec!["(no matching peers)".into()];
    }
    let mut out: Vec<String> = peers
        .iter()
        .map(|p| {
            let pubkey = p.agent_id.pubkey_base58();
            let self_marker = if pubkey == operator_pubkey_b58 {
                " (self)"
            } else {
                ""
            };
            let status = match p.revoked_at {
                Some(ts) => format!("revoked@{ts}"),
                None => "live".into(),
            };
            format!(
                "{display}{self_marker}\t{pubkey}\t{prefix}…\tregistered@{registered}\t{status}",
                display = p.agent_id.display,
                prefix = p.token_prefix,
                registered = p.registered_at,
            )
        })
        .collect();
    if truncated {
        out.push(format!(
            "(truncated; {n} shown — narrow with --prefix or raise --limit)",
            n = peers.len()
        ));
    }
    out
}

fn peer_revoke_ambiguous_lines(matches: &[PeerSummary], truncated: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(matches.len() + 2);
    out.push(format!(
        "prefix matched {n} peers — narrow the prefix:",
        n = matches.len()
    ));
    for p in matches {
        let status = match p.revoked_at {
            Some(ts) => format!("revoked@{ts}"),
            None => "live".into(),
        };
        out.push(format!(
            "  {display}\t{pubkey}\t{prefix}…\tregistered@{registered}\t{status}",
            display = p.agent_id.display,
            pubkey = p.agent_id.pubkey_base58(),
            prefix = p.token_prefix,
            registered = p.registered_at,
        ));
    }
    if truncated {
        out.push(format!(
            "(truncated; {n} shown — re-run with a longer prefix or raise --limit-matches)",
            n = matches.len()
        ));
    }
    out
}

fn resource_name(resource: ResourceKind) -> &'static str {
    match resource {
        ResourceKind::Compute => "compute",
        ResourceKind::Memory => "memory",
        ResourceKind::Tool => "tool",
        ResourceKind::Message => "message",
        ResourceKind::Registration => "registration",
    }
}

fn receipt_list_json(
    limit: usize,
    since_ms: Option<u64>,
    receipts: &[SettlementReceipt],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "receipt_list",
        "limit": limit,
        "since_ms": since_ms,
        "receipts": receipts,
    })
}

fn intent_result_json(
    intent_id: uuid::Uuid,
    status: &str,
    text: &str,
    sources: &[String],
    settlement: Option<&SettlementReceipt>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "intent_result",
        "intent_id": intent_id,
        "status": status,
        "text": text,
        "sources": sources,
        "settlement": settlement,
    })
}

fn ping_json() -> serde_json::Value {
    serde_json::json!({
        "kind": "daemon_ping",
        "status": "ok",
    })
}

fn capability_list_json(limit: usize, capabilities: &[SignedCapability]) -> serde_json::Value {
    serde_json::json!({
        "kind": "capability_list",
        "limit": limit,
        "capabilities": capabilities,
    })
}

fn capability_grant_json(
    subject_display: &str,
    action: &str,
    signature_b58: &str,
    scope: Option<&serde_json::Value>,
    expires_at: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "capability_granted",
        "subject_display": subject_display,
        "action": action,
        "signature_b58": signature_b58,
        "scope": scope,
        "expires_at": expires_at,
    })
}

fn capability_revoke_json(signature_b58: &str, removed: bool) -> serde_json::Value {
    serde_json::json!({
        "kind": "capability_revoked",
        "signature_b58": signature_b58,
        "removed": removed,
    })
}

fn capabilities_purge_json(before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "capabilities_purged",
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn peers_purge_json(before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "peers_purged",
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn peers_rotate_json(token_b58: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "peer_token_rotated",
        "token_b58": token_b58,
    })
}

fn a2a_compact_json(dropped: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "a2a_compacted",
        "dropped": dropped,
    })
}

fn audit_purge_json(before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "audit_purged",
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn audit_recent_json(
    limit: usize,
    since_ms: Option<u64>,
    events: &[AuditEvent],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "audit_recent",
        "limit": limit,
        "since_ms": since_ms,
        "events": events,
    })
}

fn audit_verify_json(report: &AuditIntegrityReport) -> serde_json::Value {
    serde_json::json!({
        "kind": "audit_integrity",
        "report": report,
    })
}

fn memory_purge_json(tier: Option<MemoryTier>, before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_purged",
        "tier": tier.map(memory_tier_slug),
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn memory_compaction_json(outcome: &MemoryCompactionOutcome) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_compacted",
        "outcome": outcome,
    })
}

fn memory_compaction_plan_json(outcome: &MemoryCompactionOutcome) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_compaction_plan",
        "outcome": outcome,
        "expected_receipt_changes": {
            "mode": "none",
            "records": [],
            "reason": "dry-run compaction planning does not mutate memory or settlement receipts"
        }
    })
}

fn memory_read_json(
    mode: &str,
    tier: Option<MemoryTier>,
    limit: usize,
    query: Option<&str>,
    min_relevance: Option<f32>,
    records: &[MemoryRecord],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_read",
        "mode": mode,
        "tier": tier.map(memory_tier_slug),
        "limit": limit,
        "query": query,
        "min_relevance": min_relevance,
        "records": records,
    })
}

fn ignore_report_json(
    ignored: bool,
    matched_pattern: Option<&str>,
    rules_loaded: usize,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "ignore_report",
        "ignored": ignored,
        "matched_pattern": matched_pattern,
        "rules_loaded": rules_loaded,
    })
}

fn tool_list_json(tools: &[ToolSpec]) -> serde_json::Value {
    serde_json::json!({
        "kind": "tool_list",
        "tools": tools,
    })
}

fn tool_result_json(
    name: &str,
    content: &[covenant_mcp::Content],
    is_error: bool,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "tool_result",
        "name": name,
        "content": content,
        "is_error": is_error,
    })
}

fn receipt_batch_list_json(limit: usize, batches: &[ReceiptBatchSummary]) -> serde_json::Value {
    serde_json::json!({
        "kind": "receipt_batch_list",
        "limit": limit,
        "batches": batches,
    })
}

fn chain_status_json(status: &ChainStatus) -> serde_json::Value {
    serde_json::json!({
        "kind": "chain_status",
        "status": status,
    })
}

fn verify_report_json(
    window: usize,
    checks: &[VerifyCheck],
    drift: &[VerifyDrift],
    orphans_total: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "verify_report",
        "window": window,
        "checks": checks,
        "drift": drift,
        "orphans_total": orphans_total,
    })
}

fn flush_receipts_json(
    limit: usize,
    batch: &ReceiptBatchSummary,
    receipts_updated: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "receipt_batch_flushed",
        "limit": limit,
        "receipts_updated": receipts_updated,
        "batch": batch,
    })
}

fn settlement_backfill_json(
    row_count: u64,
    rollback_path: Option<&str>,
    dry_run: bool,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "covenant.settlement.backfill.v1",
        "row_count": row_count,
        "rollback_path": rollback_path,
        "dry_run": dry_run,
    })
}

fn memory_backfill_json(row_count: u64, savepoint_name: &str, dry_run: bool) -> serde_json::Value {
    serde_json::json!({
        "schema": "covenant.memory.backfill.v1",
        "row_count": row_count,
        "savepoint_name": savepoint_name,
        "dry_run": dry_run,
    })
}

fn bootstrap_result_json(
    granted: &[(String, String)],
    already_granted: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "bootstrap_result",
        "granted": granted
            .iter()
            .map(|(a, s)| serde_json::json!({ "action": a, "signature_b58": s }))
            .collect::<Vec<_>>(),
        "already_granted": already_granted,
    })
}

fn a2a_status_json(
    limit: usize,
    min_lease_age_ms: Option<u64>,
    deadline_within_ms: Option<u64>,
    state_filter: Option<A2ATaskQueueState>,
    tasks: &[A2ATaskQueueEntry],
    results: &[A2ATaskResult],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "a2a_status",
        "limit": limit,
        "min_lease_age_ms": min_lease_age_ms,
        "deadline_within_ms": deadline_within_ms,
        "state_filter": state_filter,
        "tasks": tasks,
        "results": results,
    })
}

fn a2a_retry_json(report: &A2AAutoRetryReport) -> serde_json::Value {
    serde_json::json!({
        "kind": "a2a_auto_retry",
        "report": report,
    })
}

fn peer_revoke_json(outcome: &RevokeOutcome) -> serde_json::Value {
    serde_json::json!({
        "kind": "peer_revoke",
        "outcome": outcome,
    })
}

fn intents_resume_error_code(message: &str) -> &'static str {
    if message.contains("no BudgetExhausted audit row found") {
        return "no_budget_exhausted_row";
    }
    if message.contains("missing <intent-id>") {
        return "missing_intent_id";
    }
    if message.contains("intent-id must be a uuid") || message.contains("intent-id must be a UUID")
    {
        return "invalid_intent_id";
    }
    if message.contains("pass either <intent-id> or latest") {
        return "conflicting_flags";
    }
    "error"
}

fn intents_resume_ok_json(
    mode: &str,
    intent_id: uuid::Uuid,
    status: &str,
    text: &str,
    sources: &[String],
    settlement: &Option<SettlementReceipt>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "intents_resume",
        "ok": true,
        "mode": mode,
        "intent_id": intent_id,
        "status": status,
        "text": text,
        "sources": sources,
        "settlement": settlement,
    })
}

fn intents_resume_error_json(
    mode: &str,
    intent_id: Option<uuid::Uuid>,
    code: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "intents_resume",
        "ok": false,
        "mode": mode,
        "intent_id": intent_id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn peer_revoke_is_failure(outcome: &RevokeOutcome) -> bool {
    matches!(
        outcome,
        RevokeOutcome::NotFound
            | RevokeOutcome::Ambiguous { .. }
            | RevokeOutcome::SelfRevokeForbidden(_)
    )
}

fn print_expand_error(err: &ExpandError) {
    match err {
        ExpandError::NoMatch { tail } => {
            eprintln!("no peer matched pubkey-prefix `{tail}`");
            eprintln!(
                "  use `covenant peers list --prefix <pubkey-b58-prefix>` to see registered peers"
            );
        }
        ExpandError::Ambiguous { tail, matches } => {
            eprintln!(
                "pubkey-prefix `{tail}` matched {n} live peers — narrow the prefix:",
                n = matches.len()
            );
            for p in matches {
                eprintln!(
                    "  {display}\t{pubkey}\tregistered@{registered}",
                    display = p.agent_id.display,
                    pubkey = p.agent_id.pubkey_base58(),
                    registered = p.registered_at,
                );
            }
        }
        ExpandError::RevokedOnly { tail, matches } => {
            eprintln!(
                "pubkey-prefix `{tail}` matched only revoked peers — granting against a revoked peer is meaningless:"
            );
            for p in matches {
                eprintln!(
                    "  {display}\t{pubkey}\trevoked@{revoked}",
                    display = p.agent_id.display,
                    pubkey = p.agent_id.pubkey_base58(),
                    revoked = p.revoked_at.unwrap_or(0),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ATask, A2ATaskQueueState};
    use covenant_types::{AgentId, Capability};

    fn make_peer(seed: u8, display: &str, revoked: bool) -> PeerSummary {
        let mut pk = [0u8; 32];
        pk[0] = seed;
        PeerSummary {
            agent_id: AgentId::new(display, pk),
            token_prefix: "tokenp".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: if revoked {
                Some(1_700_000_001_000)
            } else {
                None
            },
        }
    }

    #[test]
    fn peer_prefix_to_lookup_returns_none_for_non_a2a_actions() {
        assert_eq!(peer_prefix_to_lookup("tool.call.foo"), None);
        assert_eq!(peer_prefix_to_lookup("audit.purge"), None);
        assert_eq!(peer_prefix_to_lookup("a2a.compact"), None);
    }

    #[test]
    fn peer_prefix_to_lookup_returns_none_for_display_form() {
        assert_eq!(peer_prefix_to_lookup("a2a.send.research@local"), None);
        assert_eq!(peer_prefix_to_lookup("a2a.recv.orch@local"), None);
        assert_eq!(peer_prefix_to_lookup("a2a.respond.user@host"), None);
    }

    #[test]
    fn peer_prefix_to_lookup_returns_some_for_pubkey_form() {
        assert_eq!(peer_prefix_to_lookup("a2a.send.abc"), Some("abc"));
        assert_eq!(peer_prefix_to_lookup("a2a.recv.xyzPQ"), Some("xyzPQ"));
        assert_eq!(peer_prefix_to_lookup("a2a.respond.1"), Some("1"));
    }

    #[test]
    fn peer_prefix_to_lookup_returns_none_for_empty_tail() {
        assert_eq!(peer_prefix_to_lookup("a2a.send."), None);
        assert_eq!(peer_prefix_to_lookup("a2a.respond."), None);
    }

    #[test]
    fn expand_unchanged_when_no_a2a_prefix() {
        let peers = vec![make_peer(7, "x@y", false)];
        assert_eq!(
            expand_a2a_action("tool.call.foo", &peers),
            Ok(ExpandOutcome::Unchanged)
        );
    }

    #[test]
    fn expand_unchanged_when_tail_contains_at_sign() {
        let peers = vec![make_peer(7, "x@y", false)];
        assert_eq!(
            expand_a2a_action("a2a.send.research@local", &peers),
            Ok(ExpandOutcome::Unchanged)
        );
    }

    #[test]
    fn expand_unchanged_for_a2a_compact() {
        assert_eq!(
            expand_a2a_action("a2a.compact", &[]),
            Ok(ExpandOutcome::Unchanged)
        );
    }

    #[test]
    fn a2a_duplicate_risk_accepts_cli_spellings() {
        assert_eq!(
            parse_duplicate_risk("idempotent").unwrap(),
            A2ADuplicateRisk::Idempotent
        );
        assert_eq!(
            parse_duplicate_risk("operator-accepted").unwrap(),
            A2ADuplicateRisk::OperatorAccepted
        );
        assert_eq!(
            parse_duplicate_risk("operator_accepted").unwrap(),
            A2ADuplicateRisk::OperatorAccepted
        );
        assert!(parse_duplicate_risk("unsafe").is_err());
    }

    #[test]
    fn expand_rewrites_unique_live_match_for_send() {
        let peer = make_peer(7, "alice@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(3).collect();
        let action = format!("a2a.send.{prefix}");
        let outcome = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap();
        assert_eq!(
            outcome,
            ExpandOutcome::Rewritten {
                full: format!("a2a.send.{pubkey}"),
                peer_pubkey_b58: pubkey,
            }
        );
    }

    #[test]
    fn expand_rewrites_unique_live_match_for_recv() {
        let peer = make_peer(11, "bob@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(2).collect();
        let action = format!("a2a.recv.{prefix}");
        let ExpandOutcome::Rewritten { full, .. } =
            expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap()
        else {
            panic!("expected Rewritten");
        };
        assert_eq!(full, format!("a2a.recv.{pubkey}"));
    }

    #[test]
    fn expand_rewrites_unique_live_match_for_respond() {
        let peer = make_peer(13, "carol@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(4).collect();
        let action = format!("a2a.respond.{prefix}");
        let ExpandOutcome::Rewritten { full, .. } =
            expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap()
        else {
            panic!("expected Rewritten");
        };
        assert_eq!(full, format!("a2a.respond.{pubkey}"));
    }

    #[test]
    fn expand_errors_no_match_when_zero_peers() {
        let err = expand_a2a_action("a2a.send.abc", &[]).unwrap_err();
        assert_eq!(
            err,
            ExpandError::NoMatch {
                tail: "abc".to_string()
            }
        );
    }

    #[test]
    fn expand_errors_ambiguous_when_multiple_live_matches() {
        // Two peers with leading-zero-byte pubkeys differ only in the trailing
        // byte; bs58 maps each leading zero byte to '1', so both encode to
        // strings starting with many '1's. Tail "1" matches both → Ambiguous.
        let mut pk1 = [0u8; 32];
        pk1[31] = 1;
        let mut pk2 = [0u8; 32];
        pk2[31] = 2;
        let p1 = PeerSummary {
            agent_id: AgentId::new("alice@host", pk1),
            token_prefix: "tokenp".into(),
            registered_at: 0,
            revoked_at: None,
        };
        let p2 = PeerSummary {
            agent_id: AgentId::new("bob@host", pk2),
            token_prefix: "tokenp".into(),
            registered_at: 0,
            revoked_at: None,
        };
        assert!(p1.agent_id.pubkey_base58().starts_with('1'));
        assert!(p2.agent_id.pubkey_base58().starts_with('1'));
        let err = expand_a2a_action("a2a.send.1", &[p1, p2]).unwrap_err();
        match err {
            ExpandError::Ambiguous { matches, tail } => {
                assert_eq!(tail, "1");
                assert_eq!(matches.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn expand_errors_revoked_only_when_unique_match_is_revoked() {
        let peer = make_peer(17, "dave@host", true);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(3).collect();
        let action = format!("a2a.send.{prefix}");
        let err = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap_err();
        match err {
            ExpandError::RevokedOnly { matches, .. } => {
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0].agent_id.pubkey_base58(), pubkey);
            }
            other => panic!("expected RevokedOnly, got {other:?}"),
        }
    }

    #[test]
    fn expand_treats_full_44_char_b58_as_lookup_and_succeeds_when_peer_present() {
        let peer = make_peer(23, "eve@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let action = format!("a2a.send.{pubkey}");
        let outcome = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap();
        assert_eq!(
            outcome,
            ExpandOutcome::Rewritten {
                full: format!("a2a.send.{pubkey}"),
                peer_pubkey_b58: pubkey,
            }
        );
    }

    #[test]
    fn expand_full_length_b58_with_no_match_errors() {
        let registered = make_peer(29, "frank@host", false);
        let phantom = make_peer(31, "ghost@host", false);
        let phantom_pubkey = phantom.agent_id.pubkey_base58();
        let action = format!("a2a.send.{phantom_pubkey}");
        let err = expand_a2a_action(&action, &[registered]).unwrap_err();
        assert_eq!(
            err,
            ExpandError::NoMatch {
                tail: phantom_pubkey
            }
        );
    }

    #[test]
    fn expand_does_not_carry_token_prefix_in_outcome() {
        let peer = make_peer(37, "hank@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(3).collect();
        let action = format!("a2a.send.{prefix}");
        let outcome = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap();
        let dump = format!("{outcome:?}");
        assert!(
            !dump.contains(&peer.token_prefix),
            "Rewritten outcome must not carry token_prefix: {dump}"
        );
    }

    #[test]
    fn peer_list_lines_renders_empty_marker_when_no_peers() {
        let out = peer_list_lines(&[], "OPB58", false);
        assert_eq!(out, vec!["(no matching peers)"]);
    }

    #[test]
    fn peer_list_lines_marks_self_row_and_omits_truncation_hint_when_not_truncated() {
        let p = make_peer(7, "alice@host", false);
        let operator = p.agent_id.pubkey_base58();
        let out = peer_list_lines(std::slice::from_ref(&p), &operator, false);
        assert_eq!(out.len(), 1, "exactly one row, no trailing hint");
        assert!(
            out[0].starts_with("alice@host (self)\t"),
            "self marker missing: {}",
            out[0]
        );
        assert!(out[0].contains("\tlive"));
        assert!(!out.iter().any(|l| l.contains("truncated")));
    }

    #[test]
    fn peer_list_json_echoes_prefix_and_match_count() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", true);
        let value = peer_list_json(20, Some("ABcde"), &[p.clone(), q.clone()], "OPB58", false);
        assert_eq!(value["kind"], "peer_list");
        assert_eq!(value["limit"], 20);
        assert_eq!(value["filter_pubkey_prefix"], "ABcde");
        assert_eq!(value["matched_count"], 2);
        assert_eq!(value["operator_pubkey_b58"], "OPB58");
        assert_eq!(value["truncated"], false);
        assert_eq!(value["peers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn peer_list_json_omits_prefix_when_inactive() {
        let p = make_peer(7, "alice@host", false);
        let value = peer_list_json(20, None, &[p], "OPB58", false);
        assert!(
            value["filter_pubkey_prefix"].is_null(),
            "filter_pubkey_prefix must be null when --prefix is not supplied so machine consumers see a stable absent-filter shape: {value:?}",
        );
        assert_eq!(value["matched_count"], 1);
    }

    #[test]
    fn peer_list_json_reports_zero_match_count_for_empty_response() {
        let value = peer_list_json(20, Some("nomatch"), &[], "OPB58", false);
        assert_eq!(value["matched_count"], 0);
        assert_eq!(value["filter_pubkey_prefix"], "nomatch");
        assert_eq!(value["peers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn peer_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "filter_pubkey_prefix",
            "kind",
            "limit",
            "matched_count",
            "operator_pubkey_b58",
            "peers",
            "truncated",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("peer_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "peer_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("peer_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["filter_pubkey_prefix"].is_string()
                    || value["filter_pubkey_prefix"].is_null(),
                "filter_pubkey_prefix must be string or null (never integer / array): {value}",
            );
            assert!(
                value["matched_count"].is_u64(),
                "matched_count must serialize as a non-negative integer, not a string-of-integer: {value}",
            );
            assert!(value["peers"].is_array(), "peers must be an array: {value}",);
            assert!(
                value["operator_pubkey_b58"].is_string(),
                "operator_pubkey_b58 must be a string: {value}",
            );
            assert!(
                value["truncated"].is_boolean(),
                "truncated must be a boolean, not 0/1: {value}",
            );
        }

        let populated = peer_list_json(
            20,
            Some("ABcde"),
            &[
                make_peer(7, "alice@host", false),
                make_peer(8, "bob@host", true),
            ],
            "OPB58",
            true,
        );
        assert_shape(&populated);

        let empty = peer_list_json(20, None, &[], "OPB58", false);
        assert_shape(&empty);
    }

    #[test]
    fn peer_list_lines_appends_truncation_hint_when_truncated() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", false);
        let out = peer_list_lines(&[p, q], "different-pubkey", true);
        assert_eq!(out.len(), 3, "two rows + one hint line");
        let hint = out.last().unwrap();
        assert!(hint.starts_with("(truncated; 2 shown — "), "hint: {hint}");
        assert!(
            hint.contains("--prefix") && hint.contains("--limit"),
            "hint should suggest narrowing: {hint}"
        );
    }

    #[test]
    fn peer_revoke_ambiguous_lines_omits_hint_when_not_truncated() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", true);
        let out = peer_revoke_ambiguous_lines(&[p, q], false);
        assert_eq!(out.len(), 3, "header + two rows, no hint");
        assert!(out[0].starts_with("prefix matched 2 peers"));
        assert!(out[1].contains("alice@host"));
        assert!(out[1].contains("\tlive"));
        assert!(out[2].contains("bob@host"));
        assert!(out[2].contains("\trevoked@"));
        assert!(!out.iter().any(|l| l.contains("truncated")));
    }

    #[test]
    fn peer_revoke_ambiguous_lines_appends_truncation_hint_when_truncated() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", false);
        let out = peer_revoke_ambiguous_lines(&[p, q], true);
        let hint = out.last().unwrap();
        assert!(hint.starts_with("(truncated; 2 shown — "), "hint: {hint}");
        assert!(
            hint.contains("longer prefix") && hint.contains("--limit-matches"),
            "hint should suggest both narrowing options: {hint}"
        );
    }

    #[test]
    fn peer_revoke_json_renders_stable_ambiguous_shape() {
        let p = make_peer(7, "alice@host", false);
        let value = peer_revoke_json(&RevokeOutcome::Ambiguous {
            matches: vec![p.clone()],
            truncated: true,
        });
        assert_eq!(value["kind"], "peer_revoke");
        assert_eq!(value["outcome"]["type"], "ambiguous");
        assert_eq!(value["outcome"]["truncated"], true);
        assert_eq!(
            value["outcome"]["matches"][0]["token_prefix"],
            p.token_prefix
        );
        let text = serde_json::to_string(&value).unwrap();
        assert!(!text.contains("PeerToken"), "{text}");
    }

    #[test]
    fn peer_revoke_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "outcome"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("peer_revoke_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "peer_revoke_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("peer_revoke"));
            assert!(
                value["outcome"].is_object(),
                "outcome must be a tagged-enum object, not a string blob: {value}",
            );
        }

        let p = make_peer(7, "alice@host", false);
        assert_shape(&peer_revoke_json(&RevokeOutcome::Ambiguous {
            matches: vec![p],
            truncated: true,
        }));
        assert_shape(&peer_revoke_json(&RevokeOutcome::NotFound));
    }

    #[test]
    fn intents_resume_json_renders_stable_error_shape() {
        let intent_id = uuid::Uuid::nil();
        let value = intents_resume_error_json(
            "latest",
            Some(intent_id),
            "daemon_error",
            "budget exhausted; try again later",
        );
        assert_eq!(value["kind"], "intents_resume");
        assert_eq!(value["ok"], false);
        assert_eq!(value["mode"], "latest");
        assert_eq!(value["intent_id"], intent_id.to_string());
        assert_eq!(value["error"]["code"], "daemon_error");
        assert!(value["error"]["message"].as_str().is_some());
    }

    #[test]
    fn intents_resume_error_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["error", "intent_id", "kind", "mode", "ok"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("intents_resume_error_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "intents_resume_error_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("intents_resume"));
            assert!(
                value["ok"].is_boolean(),
                "ok must be a JSON bool, not 0/1 or a string: {value}",
            );
            assert_eq!(
                value["ok"].as_bool(),
                Some(false),
                "intents_resume_error_json must always report ok=false: {value}",
            );
            assert!(value["mode"].is_string(), "mode must be a string: {value}");
            assert!(
                value["intent_id"].is_string() || value["intent_id"].is_null(),
                "intent_id must be a string uuid when known and null when missing: {value}",
            );
            assert!(
                value["error"].is_object(),
                "error must be a structured object with code and message, not a string blob: {value}",
            );
        }

        let intent_id = uuid::Uuid::nil();
        assert_shape(&intents_resume_error_json(
            "latest",
            Some(intent_id),
            "daemon_error",
            "budget exhausted; try again later",
        ));
        assert_shape(&intents_resume_error_json(
            "explicit",
            None,
            "missing_intent_id",
            "missing <intent-id>",
        ));
    }

    #[test]
    fn intents_resume_error_json_pins_error_object_schema() {
        const EXPECTED_KEYS: &[&str] = &["code", "message"];

        fn assert_error_shape(value: &serde_json::Value) {
            let error = value["error"]
                .as_object()
                .expect("intents_resume_error_json error field must be an object");
            let mut keys: Vec<String> = error.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "intents_resume_error_json error object keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(
                value["error"]["code"].is_string(),
                "error.code must be a string, not a structured object: {value}",
            );
            assert!(
                value["error"]["message"].is_string(),
                "error.message must be a string, not a structured object: {value}",
            );
        }

        let intent_id = uuid::Uuid::nil();
        assert_error_shape(&intents_resume_error_json(
            "explicit",
            Some(intent_id),
            "invalid_intent_id",
            "intent-id must be a uuid",
        ));
        assert_error_shape(&intents_resume_error_json(
            "latest",
            None,
            "conflicting_flags",
            "pass either <intent-id> or latest, not both",
        ));
    }

    #[test]
    fn intents_resume_error_code_pins_documented_arms() {
        assert_eq!(
            intents_resume_error_code(
                "intents.resume failed: no BudgetExhausted audit row found before now"
            ),
            "no_budget_exhausted_row",
            "wrapping prose around the no-budget-exhausted phrase must still resolve to the typed slug",
        );
        assert_eq!(
            intents_resume_error_code("missing <intent-id>"),
            "missing_intent_id",
        );
        assert_eq!(
            intents_resume_error_code("intent-id must be a uuid"),
            "invalid_intent_id",
            "lower-case uuid spelling is the parser's wording and must map to invalid_intent_id",
        );
        assert_eq!(
            intents_resume_error_code("intent-id must be a UUID"),
            "invalid_intent_id",
            "upper-case UUID spelling is what callers paste from RFC 4122 docs and must map to invalid_intent_id",
        );
        assert_eq!(
            intents_resume_error_code("pass either <intent-id> or latest, not both"),
            "conflicting_flags",
        );
        assert_eq!(
            intents_resume_error_code("daemon connection refused"),
            "error",
            "an unrelated message must fall through to the catch-all slug, not a typed code",
        );
    }

    #[test]
    fn intents_resume_json_renders_stable_ok_shape() {
        let intent_id = uuid::Uuid::nil();
        let value = intents_resume_ok_json(
            "explicit",
            intent_id,
            "ok",
            "resumed intent text",
            &["a".into(), "b".into()],
            &None,
        );
        assert_eq!(value["kind"], "intents_resume");
        assert_eq!(value["ok"], true);
        assert_eq!(value["mode"], "explicit");
        assert_eq!(value["intent_id"], intent_id.to_string());
        assert_eq!(value["status"], "ok");
        assert_eq!(value["sources"][0], "a");
        assert!(value["settlement"].is_null());
    }

    #[test]
    fn intents_resume_ok_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "intent_id",
            "kind",
            "mode",
            "ok",
            "settlement",
            "sources",
            "status",
            "text",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("intents_resume_ok_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "intents_resume_ok_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("intents_resume"));
            assert!(
                value["ok"].is_boolean(),
                "ok must be a JSON bool, not 0/1 or a string: {value}",
            );
            assert!(value["mode"].is_string(), "mode must be a string: {value}");
            assert!(
                value["intent_id"].is_string(),
                "intent_id must be a string-serialized uuid: {value}",
            );
            assert!(
                value["status"].is_string(),
                "status must be a string: {value}",
            );
            assert!(value["text"].is_string(), "text must be a string: {value}");
            assert!(
                value["sources"].is_array(),
                "sources must be an array of strings: {value}",
            );
            assert!(
                value["settlement"].is_object() || value["settlement"].is_null(),
                "settlement must be a structured object or null: {value}",
            );
        }

        let intent_id = uuid::Uuid::nil();
        let payer = AgentId::new("payer@local", [3u8; 32]);
        let receipt = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer,
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 42,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        assert_shape(&intents_resume_ok_json(
            "explicit",
            intent_id,
            "ok",
            "resumed intent text",
            &["a".into(), "b".into()],
            &Some(receipt),
        ));
        assert_shape(&intents_resume_ok_json(
            "latest",
            intent_id,
            "ok",
            "",
            &[],
            &None,
        ));
    }

    #[test]
    fn receipt_list_json_renders_stable_shape() {
        let payer = AgentId::new("payer@local", [3u8; 32]);
        let receipt = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer: payer.clone(),
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 42,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let receipts = [receipt];
        let value = receipt_list_json(10, Some(1_699_999_999_000), &receipts);
        assert_eq!(value["kind"], "receipt_list");
        assert_eq!(value["limit"], 10);
        assert_eq!(value["since_ms"], 1_699_999_999_000u64);
        assert_eq!(value["receipts"][0]["payer"]["display"], "payer@local");
        assert_eq!(
            value["receipts"][0]["payer"]["pubkey"],
            payer.pubkey_base58()
        );
        assert_eq!(value["receipts"][0]["resource"], "memory");
        assert_eq!(
            value["receipts"][0]["memory_record_id"],
            uuid::Uuid::nil().to_string()
        );
        assert_eq!(value["receipts"][0]["credits_consumed"], 42);
        assert!(value["receipts"][0]["tx_sig"].is_null());

        let unfiltered = receipt_list_json(10, None, &receipts);
        assert!(unfiltered["since_ms"].is_null());
    }

    #[test]
    fn receipt_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "limit", "receipts", "since_ms"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("receipt_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "receipt_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("receipt_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["since_ms"].is_u64() || value["since_ms"].is_null(),
                "since_ms must be u64-or-null (never a string-of-integer or other type): {value}",
            );
            assert!(
                value["receipts"].is_array(),
                "receipts must be an array: {value}",
            );
        }

        let receipt = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer: AgentId::new("payer@local", [4u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 1,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let receipts = [receipt];

        assert_shape(&receipt_list_json(10, Some(1_699_999_999_000), &receipts));
        assert_shape(&receipt_list_json(10, None, &receipts));
        assert_shape(&receipt_list_json(10, None, &[]));
    }

    #[test]
    fn settlement_backfill_json_renders_stable_shape() {
        let dry_run = settlement_backfill_json(12, None, true);
        assert_eq!(dry_run["schema"], "covenant.settlement.backfill.v1");
        assert_eq!(dry_run["row_count"], 12);
        assert!(dry_run["rollback_path"].is_null());
        assert_eq!(dry_run["dry_run"], true);

        let mutation = settlement_backfill_json(
            12,
            Some("/tmp/settlement.backfill-rollback-001.jsonl"),
            false,
        );
        assert_eq!(mutation["schema"], "covenant.settlement.backfill.v1");
        assert_eq!(mutation["row_count"], 12);
        assert_eq!(
            mutation["rollback_path"],
            "/tmp/settlement.backfill-rollback-001.jsonl"
        );
        assert_eq!(mutation["dry_run"], false);
    }

    #[test]
    fn settlement_backfill_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["dry_run", "rollback_path", "row_count", "schema"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("settlement_backfill_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "settlement_backfill_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(
                value["schema"].is_string(),
                "schema must be a string: {value}"
            );
            assert_eq!(
                value["schema"].as_str(),
                Some("covenant.settlement.backfill.v1"),
                "schema literal must match the documented version slot exactly; renaming to a future v2 is a separate envelope, not a field rename",
            );
            assert!(
                value["row_count"].is_u64(),
                "row_count must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["rollback_path"].is_string() || value["rollback_path"].is_null(),
                "rollback_path must be string-or-null (never the literal \"(none)\"); JSON consumers branch on null to detect dry-run: {value}",
            );
            assert!(
                value["dry_run"].is_boolean(),
                "dry_run must serialize as a JSON boolean, never 0/1 or a string: {value}",
            );
        }

        assert_shape(&settlement_backfill_json(12, None, true));
        assert_shape(&settlement_backfill_json(
            12,
            Some("/tmp/settlement.backfill-rollback-001.jsonl"),
            false,
        ));
        assert_shape(&settlement_backfill_json(0, None, true));
    }

    #[test]
    fn memory_backfill_json_renders_stable_shape() {
        let dry_run = memory_backfill_json(0, "memory_backfill_sp_001", true);
        assert_eq!(dry_run["schema"], "covenant.memory.backfill.v1");
        assert_eq!(dry_run["row_count"], 0);
        assert_eq!(dry_run["savepoint_name"], "memory_backfill_sp_001");
        assert_eq!(dry_run["dry_run"], true);

        let mutation = memory_backfill_json(7, "memory_backfill_sp_002", false);
        assert_eq!(mutation["schema"], "covenant.memory.backfill.v1");
        assert_eq!(mutation["row_count"], 7);
        assert_eq!(mutation["savepoint_name"], "memory_backfill_sp_002");
        assert_eq!(mutation["dry_run"], false);
    }

    #[test]
    fn memory_backfill_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["dry_run", "row_count", "savepoint_name", "schema"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_backfill_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_backfill_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(
                value["schema"].is_string(),
                "schema must be a string: {value}"
            );
            assert_eq!(
                value["schema"].as_str(),
                Some("covenant.memory.backfill.v1"),
                "schema literal must match the documented version slot exactly; renaming to a future v2 is a separate envelope, not a field rename",
            );
            assert!(
                value["row_count"].is_u64(),
                "row_count must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["savepoint_name"].is_string(),
                "savepoint_name must always be a non-null string; the &str type at the emitter forbids null and the docs contract treats absence as a protocol violation: {value}",
            );
            assert!(
                !value["savepoint_name"].as_str().unwrap().is_empty(),
                "savepoint_name must be non-empty; the daemon allocates a real identifier even on dry-run so consumers can correlate planning runs against later mutation runs: {value}",
            );
            assert!(
                value["dry_run"].is_boolean(),
                "dry_run must serialize as a JSON boolean, never 0/1 or a string: {value}",
            );
        }

        assert_shape(&memory_backfill_json(0, "memory_backfill_sp_001", true));
        assert_shape(&memory_backfill_json(7, "memory_backfill_sp_002", false));
        assert_shape(&memory_backfill_json(0, "memory_backfill_sp_003", true));
    }

    #[test]
    fn bootstrap_result_json_renders_stable_shape() {
        let granted = vec![
            ("memory.read".to_string(), "sig_b58_a".to_string()),
            ("a2a.send".to_string(), "sig_b58_b".to_string()),
        ];
        let already = vec!["audit.read".to_string()];
        let populated = bootstrap_result_json(&granted, &already);
        assert_eq!(populated["kind"], "bootstrap_result");
        assert_eq!(populated["granted"][0]["action"], "memory.read");
        assert_eq!(populated["granted"][0]["signature_b58"], "sig_b58_a");
        assert_eq!(populated["granted"][1]["action"], "a2a.send");
        assert_eq!(populated["granted"][1]["signature_b58"], "sig_b58_b");
        assert_eq!(populated["already_granted"][0], "audit.read");

        let no_new_grants =
            bootstrap_result_json(&[], &["memory.read".to_string(), "audit.read".to_string()]);
        assert_eq!(no_new_grants["kind"], "bootstrap_result");
        assert!(no_new_grants["granted"].as_array().unwrap().is_empty());
        assert_eq!(no_new_grants["already_granted"][0], "memory.read");
        assert_eq!(no_new_grants["already_granted"][1], "audit.read");
    }

    #[test]
    fn bootstrap_result_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["already_granted", "granted", "kind"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("bootstrap_result_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "bootstrap_result_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("bootstrap_result"));
            assert!(
                value["granted"].is_array(),
                "granted must be an array: {value}",
            );
            assert!(
                value["already_granted"].is_array(),
                "already_granted must be an array: {value}",
            );
        }

        let granted = vec![
            ("memory.read".to_string(), "sig_b58_a".to_string()),
            ("a2a.send".to_string(), "sig_b58_b".to_string()),
        ];
        let already = vec!["audit.read".to_string()];
        let populated = bootstrap_result_json(&granted, &already);
        assert_shape(&populated);
        assert!(
            populated["granted"][0].is_object(),
            "granted entries must be {{action, signature_b58}} objects, never bare strings: {populated}",
        );
        assert!(
            populated["already_granted"][0].is_string(),
            "already_granted entries must be bare action strings, never objects — the asymmetry is documented: {populated}",
        );

        let no_new_grants =
            bootstrap_result_json(&[], &["memory.read".to_string(), "audit.read".to_string()]);
        assert_shape(&no_new_grants);
        assert!(
            no_new_grants["granted"].as_array().unwrap().is_empty(),
            "empty-granted case must serialize as [], not null or absent: {no_new_grants}",
        );
        assert!(
            no_new_grants["already_granted"][0].is_string(),
            "already_granted entries must be bare strings even when granted is empty: {no_new_grants}",
        );

        assert_shape(&bootstrap_result_json(&[], &[]));
    }

    #[test]
    fn intent_result_json_renders_stable_shape() {
        let value = intent_result_json(
            uuid::Uuid::nil(),
            "ok",
            "phase 0 echo",
            &["research".into()],
            None,
        );

        assert_eq!(value["kind"], "intent_result");
        assert_eq!(value["intent_id"], uuid::Uuid::nil().to_string());
        assert_eq!(value["status"], "ok");
        assert_eq!(value["text"], "phase 0 echo");
        assert_eq!(value["sources"][0], "research");
        assert!(value["settlement"].is_null());
    }

    #[test]
    fn intent_result_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "intent_id",
            "kind",
            "settlement",
            "sources",
            "status",
            "text",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("intent_result_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "intent_result_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("intent_result"));
            assert!(
                value["intent_id"].is_string(),
                "intent_id must be a string (uuid serialization), not bytes/array: {value}",
            );
            assert!(
                value["status"].is_string(),
                "status must be a string: {value}",
            );
            assert!(value["text"].is_string(), "text must be a string: {value}");
            assert!(
                value["sources"].is_array(),
                "sources must be an array: {value}",
            );
            assert!(
                value["settlement"].is_object() || value["settlement"].is_null(),
                "settlement must be object-or-null (never integer / array): {value}",
            );
        }

        let settlement = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer: AgentId::new("payer@local", [4u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 1,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };

        assert_shape(&intent_result_json(
            uuid::Uuid::nil(),
            "ok",
            "phase 0 echo",
            &["research".into()],
            Some(&settlement),
        ));
        assert_shape(&intent_result_json(uuid::Uuid::nil(), "ok", "", &[], None));
    }

    #[test]
    fn ping_json_renders_stable_shape() {
        let value = ping_json();
        assert_eq!(value["kind"], "daemon_ping");
        assert_eq!(value["status"], "ok");
    }

    #[test]
    fn ping_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "status"];

        let value = ping_json();
        let object = value.as_object().expect("ping_json must return an object");
        let mut keys: Vec<String> = object.keys().cloned().collect();
        keys.sort();
        let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
        assert_eq!(
            keys, expected,
            "ping_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
        );

        assert!(value["kind"].is_string(), "kind must be a string: {value}");
        assert_eq!(value["kind"].as_str(), Some("daemon_ping"));
        assert!(
            value["status"].is_string(),
            "status must be a string, not a non-string serialization: {value}",
        );
        assert_eq!(value["status"].as_str(), Some("ok"));
    }

    #[test]
    fn capability_list_json_renders_stable_shape() {
        let subject = AgentId::new("subject@local", [1u8; 32]);
        let granted_by = AgentId::new("issuer@local", [2u8; 32]);
        let signed = SignedCapability {
            capability: Capability {
                subject: subject.clone(),
                action: "tool.call.echo".into(),
                scope: serde_json::json!({"version": 1}),
                granted_by: granted_by.clone(),
                expires_at: None,
            },
            signature: [9u8; 64],
        };

        let value = capability_list_json(5, &[signed]);
        assert_eq!(value["kind"], "capability_list");
        assert_eq!(value["limit"], 5);
        assert_eq!(
            value["capabilities"][0]["capability"]["action"],
            "tool.call.echo"
        );
        assert_eq!(
            value["capabilities"][0]["capability"]["subject"]["pubkey"],
            subject.pubkey_base58()
        );
        assert_eq!(
            value["capabilities"][0]["capability"]["granted_by"]["display"],
            granted_by.display
        );
        assert_eq!(
            value["capabilities"][0]["capability"]["scope"]["version"],
            1
        );
        assert!(value["capabilities"][0]["capability"]["expires_at"].is_null());
        assert!(value["capabilities"][0]["signature"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn capability_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["capabilities", "kind", "limit"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capability_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capability_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capability_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["capabilities"].is_array(),
                "capabilities must be an array: {value}",
            );
        }

        let signed = SignedCapability {
            capability: Capability {
                subject: AgentId::new("subject@local", [1u8; 32]),
                action: "tool.call.echo".into(),
                scope: serde_json::json!({"version": 1}),
                granted_by: AgentId::new("issuer@local", [2u8; 32]),
                expires_at: None,
            },
            signature: [9u8; 64],
        };

        assert_shape(&capability_list_json(5, &[signed]));
        assert_shape(&capability_list_json(5, &[]));
    }

    #[test]
    fn capability_grant_json_renders_stable_shape() {
        let scope = serde_json::json!({"version": 1, "tools": ["echo"]});
        let value = capability_grant_json(
            "operator@local",
            "tool.call.echo",
            "sigb58",
            Some(&scope),
            Some(1_700_000_000_000),
        );

        assert_eq!(value["kind"], "capability_granted");
        assert_eq!(value["subject_display"], "operator@local");
        assert_eq!(value["action"], "tool.call.echo");
        assert_eq!(value["signature_b58"], "sigb58");
        assert_eq!(value["scope"]["version"], 1);
        assert_eq!(value["expires_at"], 1_700_000_000_000u64);

        let unscoped = capability_grant_json("operator@local", "memory.read", "sigb58", None, None);
        assert!(unscoped["scope"].is_null());
        assert!(unscoped["expires_at"].is_null());
    }

    #[test]
    fn capability_grant_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "action",
            "expires_at",
            "kind",
            "scope",
            "signature_b58",
            "subject_display",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capability_grant_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capability_grant_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capability_granted"));
            assert!(
                value["subject_display"].is_string(),
                "subject_display must be a string: {value}",
            );
            assert!(
                value["action"].is_string(),
                "action must be a string: {value}",
            );
            assert!(
                value["signature_b58"].is_string(),
                "signature_b58 must be a string: {value}",
            );
            assert!(
                value["scope"].is_object() || value["scope"].is_null(),
                "scope must be a structured object or null, never a string blob: {value}",
            );
            assert!(
                value["expires_at"].is_u64() || value["expires_at"].is_null(),
                "expires_at must be a non-negative integer or null, not a string: {value}",
            );
        }

        let scope = serde_json::json!({"version": 1, "tools": ["echo"]});
        assert_shape(&capability_grant_json(
            "operator@local",
            "tool.call.echo",
            "sigb58",
            Some(&scope),
            Some(1_700_000_000_000),
        ));
        assert_shape(&capability_grant_json(
            "operator@local",
            "memory.read",
            "sigb58",
            None,
            None,
        ));
    }

    #[test]
    fn capability_revoke_json_renders_stable_shape() {
        let removed = capability_revoke_json("sigb58", true);
        assert_eq!(removed["kind"], "capability_revoked");
        assert_eq!(removed["signature_b58"], "sigb58");
        assert_eq!(removed["removed"], true);

        let absent = capability_revoke_json("sigb58", false);
        assert_eq!(absent["kind"], "capability_revoked");
        assert_eq!(absent["signature_b58"], "sigb58");
        assert_eq!(absent["removed"], false);
    }

    #[test]
    fn capability_revoke_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "removed", "signature_b58"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capability_revoke_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capability_revoke_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capability_revoked"));
            assert!(
                value["signature_b58"].is_string(),
                "signature_b58 must be a string: {value}",
            );
            assert!(
                value["removed"].is_boolean(),
                "removed must be a JSON boolean, not 0/1 or string: {value}",
            );
        }

        assert_shape(&capability_revoke_json("sigb58", true));
        assert_shape(&capability_revoke_json("sigb58", false));
    }

    #[test]
    fn capabilities_purge_json_renders_stable_shape() {
        let value = capabilities_purge_json(1_700_000_000_000, 3);
        assert_eq!(value["kind"], "capabilities_purged");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);
    }

    #[test]
    fn capabilities_purge_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["before_ms", "kind", "purged"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capabilities_purge_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capabilities_purge_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capabilities_purged"));
            assert!(
                value["before_ms"].is_u64(),
                "before_ms must be a non-negative integer, not a string: {value}",
            );
            assert!(
                value["purged"].is_u64(),
                "purged must be a non-negative integer, not a string: {value}",
            );
        }

        assert_shape(&capabilities_purge_json(1_700_000_000_000, 3));
        assert_shape(&capabilities_purge_json(0, 0));
    }

    #[test]
    fn peers_purge_json_renders_stable_shape() {
        let value = peers_purge_json(1_700_000_000_000, 3);
        assert_eq!(value["kind"], "peers_purged");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);
    }

    #[test]
    fn peers_purge_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["before_ms", "kind", "purged"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("peers_purge_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "peers_purge_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("peers_purged"));
            assert!(
                value["before_ms"].is_u64(),
                "before_ms must be a non-negative integer, not a string: {value}",
            );
            assert!(
                value["purged"].is_u64(),
                "purged must be a non-negative integer, not a string: {value}",
            );
        }

        assert_shape(&peers_purge_json(1_700_000_000_000, 3));
        assert_shape(&peers_purge_json(0, 0));
    }

    #[test]
    fn peers_rotate_json_renders_stable_shape() {
        let value = peers_rotate_json("tokenb58");
        assert_eq!(value["kind"], "peer_token_rotated");
        assert_eq!(value["token_b58"], "tokenb58");
    }

    #[test]
    fn peers_rotate_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "token_b58"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("peers_rotate_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "peers_rotate_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("peer_token_rotated"));
            assert!(
                value["token_b58"].is_string(),
                "token_b58 must be a string, not bytes or a structured object: {value}",
            );
        }

        assert_shape(&peers_rotate_json("tokenb58"));
        assert_shape(&peers_rotate_json(""));
    }

    #[test]
    fn a2a_compact_json_renders_stable_shape() {
        let value = a2a_compact_json(3);
        assert_eq!(value["kind"], "a2a_compacted");
        assert_eq!(value["dropped"], 3);
    }

    #[test]
    fn a2a_compact_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["dropped", "kind"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("a2a_compact_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "a2a_compact_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("a2a_compacted"));
            assert!(
                value["dropped"].is_u64(),
                "dropped must be a non-negative integer, not a string: {value}",
            );
        }

        assert_shape(&a2a_compact_json(3));
        assert_shape(&a2a_compact_json(0));
    }

    #[test]
    fn a2a_retry_json_renders_stable_shape() {
        let mut report = A2AAutoRetryReport::new(A2AAutoRetryPolicy {
            enabled: true,
            min_lease_age_ms: 300_000,
            max_attempts: 3,
            max_requeues: 1,
            scan_limit: 100,
        });
        report.considered = 2;
        report.requeued.push(covenant_a2a::A2AAutoRetryRequeued {
            task_id: uuid::Uuid::nil(),
            lease_id: uuid::Uuid::nil(),
            attempt: 1,
            idempotency_key: "task:key".into(),
        });
        report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
            task_id: uuid::Uuid::nil(),
            reason: covenant_a2a::A2AAutoRetrySkipReason::UnsafeDuplicateSafety,
            attempt: 1,
            lease_age_ms: Some(300_000),
        });

        let value = a2a_retry_json(&report);
        assert_eq!(value["kind"], "a2a_auto_retry");
        assert_eq!(value["report"]["policy"]["enabled"], true);
        assert_eq!(value["report"]["considered"], 2);
        assert_eq!(
            value["report"]["requeued"][0]["idempotency_key"],
            "task:key"
        );
        assert_eq!(
            value["report"]["skipped"][0]["reason"],
            "unsafe_duplicate_safety"
        );
    }

    #[test]
    fn a2a_retry_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "report"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("a2a_retry_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "a2a_retry_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("a2a_auto_retry"));
            assert!(
                value["report"].is_object(),
                "report must be a structured object, not a string blob: {value}",
            );
        }

        let policy = A2AAutoRetryPolicy {
            enabled: true,
            min_lease_age_ms: 300_000,
            max_attempts: 3,
            max_requeues: 1,
            scan_limit: 100,
        };

        let mut populated = A2AAutoRetryReport::new(policy);
        populated.considered = 2;
        populated.requeued.push(covenant_a2a::A2AAutoRetryRequeued {
            task_id: uuid::Uuid::nil(),
            lease_id: uuid::Uuid::nil(),
            attempt: 1,
            idempotency_key: "task:key".into(),
        });
        populated.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
            task_id: uuid::Uuid::nil(),
            reason: covenant_a2a::A2AAutoRetrySkipReason::UnsafeDuplicateSafety,
            attempt: 1,
            lease_age_ms: Some(300_000),
        });
        assert_shape(&a2a_retry_json(&populated));

        let zero = A2AAutoRetryReport::new(policy);
        assert_shape(&a2a_retry_json(&zero));
    }

    #[test]
    fn audit_purge_json_renders_stable_shape() {
        let value = audit_purge_json(1_700_000_000_000, 3);
        assert_eq!(value["kind"], "audit_purged");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);
    }

    #[test]
    fn audit_purge_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["before_ms", "kind", "purged"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("audit_purge_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "audit_purge_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("audit_purged"));
            assert!(
                value["before_ms"].is_u64(),
                "before_ms must be a non-negative integer, not a string: {value}",
            );
            assert!(
                value["purged"].is_u64(),
                "purged must be a non-negative integer, not a string: {value}",
            );
        }

        assert_shape(&audit_purge_json(1_700_000_000_000, 3));
        assert_shape(&audit_purge_json(0, 0));
    }

    #[test]
    fn audit_recent_json_renders_stable_shape() {
        let event = AuditEvent {
            id: uuid::Uuid::nil(),
            timestamp_ms: 1_700_000_000_000,
            issuer: covenant_types::AgentId::new("operator@covenant", [7; 32]),
            kind: AuditKind::CapabilityGranted {
                subject_display: "operator@covenant".into(),
                action: "tool.call.echo".into(),
                granted_by_display: "operator@covenant".into(),
                signature_b58: "sigb58".into(),
            },
        };

        let value = audit_recent_json(5, Some(1_699_999_999_000), &[event]);
        assert_eq!(value["kind"], "audit_recent");
        assert_eq!(value["limit"], 5);
        assert_eq!(value["since_ms"], 1_699_999_999_000u64);
        assert_eq!(value["events"][0]["timestamp_ms"], 1_700_000_000_000u64);
        assert_eq!(value["events"][0]["kind"]["type"], "capability_granted");
        assert_eq!(value["events"][0]["kind"]["action"], "tool.call.echo");

        let empty = audit_recent_json(5, None, &[]);
        assert_eq!(empty["events"].as_array().unwrap().len(), 0);
        assert!(empty["since_ms"].is_null());
    }

    #[test]
    fn audit_recent_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["events", "kind", "limit", "since_ms"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("audit_recent_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "audit_recent_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("audit_recent"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["since_ms"].is_u64() || value["since_ms"].is_null(),
                "since_ms must be u64-or-null (never a string-of-integer or other type): {value}",
            );
            assert!(
                value["events"].is_array(),
                "events must be an array: {value}",
            );
        }

        let event = AuditEvent {
            id: uuid::Uuid::nil(),
            timestamp_ms: 1_700_000_000_000,
            issuer: covenant_types::AgentId::new("operator@covenant", [7; 32]),
            kind: AuditKind::CapabilityGranted {
                subject_display: "operator@covenant".into(),
                action: "tool.call.echo".into(),
                granted_by_display: "operator@covenant".into(),
                signature_b58: "sigb58".into(),
            },
        };
        let events = [event];

        assert_shape(&audit_recent_json(5, Some(1_699_999_999_000), &events));
        assert_shape(&audit_recent_json(5, Some(1_699_999_999_000), &[]));
        assert_shape(&audit_recent_json(5, None, &[]));
    }

    #[test]
    fn audit_verify_json_renders_stable_shape() {
        let report = AuditIntegrityReport {
            events: 2,
            anchors: 2,
            valid: true,
            root_hash_hex: "ab".repeat(32),
            failures: vec![],
        };

        let value = audit_verify_json(&report);
        assert_eq!(value["kind"], "audit_integrity");
        assert_eq!(value["report"]["events"], 2);
        assert_eq!(value["report"]["anchors"], 2);
        assert_eq!(value["report"]["valid"], true);
        assert_eq!(
            value["report"]["root_hash_hex"]
                .as_str()
                .unwrap_or_default()
                .len(),
            64
        );
        assert_eq!(
            value["report"]["failures"].as_array().map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn audit_verify_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "report"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("audit_verify_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "audit_verify_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("audit_integrity"));
            assert!(
                value["report"].is_object(),
                "report must be a structured object, not a string blob: {value}",
            );
        }

        let valid = AuditIntegrityReport {
            events: 2,
            anchors: 2,
            valid: true,
            root_hash_hex: "ab".repeat(32),
            failures: vec![],
        };
        let invalid = AuditIntegrityReport {
            events: 5,
            anchors: 4,
            valid: false,
            root_hash_hex: "cd".repeat(32),
            failures: vec!["chain hash mismatch at event 3".into()],
        };

        assert_shape(&audit_verify_json(&valid));
        assert_shape(&audit_verify_json(&invalid));
    }

    #[test]
    fn memory_purge_json_renders_stable_shape() {
        let value = memory_purge_json(Some(MemoryTier::Working), 1_700_000_000_000, 3);
        assert_eq!(value["kind"], "memory_purged");
        assert_eq!(value["tier"], "working");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);

        let all_tiers = memory_purge_json(None, 1_700_000_000_000, 0);
        assert!(all_tiers["tier"].is_null());
    }

    #[test]
    fn memory_purge_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["before_ms", "kind", "purged", "tier"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_purge_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_purge_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("memory_purged"));
            assert!(
                value["tier"].is_string() || value["tier"].is_null(),
                "tier must be a string slug or null when all tiers were purged: {value}",
            );
            assert!(
                value["before_ms"].is_u64(),
                "before_ms must be a non-negative integer, not a string: {value}",
            );
            assert!(
                value["purged"].is_u64(),
                "purged must be a non-negative integer, not a string: {value}",
            );
        }

        assert_shape(&memory_purge_json(
            Some(MemoryTier::Working),
            1_700_000_000_000,
            3,
        ));
        assert_shape(&memory_purge_json(None, 0, 0));
    }

    #[test]
    fn memory_compaction_json_renders_stable_shape() {
        let outcome = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            deleted: vec![],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        let value = memory_compaction_json(&outcome);
        assert_eq!(value["kind"], "memory_compacted");
        assert_eq!(value["outcome"]["mode"], "dry_run");
        assert_eq!(value["outcome"]["would_change"], true);
        assert_eq!(value["outcome"]["changed"], false);
        assert_eq!(
            value["outcome"]["deleted"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            value["outcome"]["parents_detached"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn memory_compaction_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "outcome"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_compaction_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_compaction_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("memory_compacted"));
            assert!(
                value["outcome"].is_object(),
                "outcome must be a structured object, not a string blob: {value}",
            );
        }

        let populated = MemoryCompactionOutcome {
            mode: MemoryRepairMode::Apply,
            would_change: true,
            changed: true,
            deleted: vec![uuid::Uuid::nil()],
            stale_marked: vec![uuid::Uuid::nil()],
            parents_detached: vec![uuid::Uuid::nil()],
        };
        let zero = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: false,
            changed: false,
            deleted: vec![],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        assert_shape(&memory_compaction_json(&populated));
        assert_shape(&memory_compaction_json(&zero));
    }

    #[test]
    fn memory_compaction_plan_json_renders_stable_shape() {
        let outcome = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            deleted: vec![uuid::Uuid::nil()],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        let value = memory_compaction_plan_json(&outcome);
        assert_eq!(value["kind"], "memory_compaction_plan");
        assert_eq!(value["outcome"]["mode"], "dry_run");
        assert_eq!(value["outcome"]["changed"], false);
        assert_eq!(value["expected_receipt_changes"]["mode"], "none");
        assert_eq!(
            value["expected_receipt_changes"]["records"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn memory_compaction_plan_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["expected_receipt_changes", "kind", "outcome"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_compaction_plan_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_compaction_plan_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("memory_compaction_plan"));
            assert!(
                value["outcome"].is_object(),
                "outcome must be a structured object, not a string blob: {value}",
            );
            assert!(
                value["expected_receipt_changes"].is_object(),
                "expected_receipt_changes must be a structured object, not a string blob: {value}",
            );
        }

        let populated = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            deleted: vec![uuid::Uuid::nil()],
            stale_marked: vec![uuid::Uuid::nil()],
            parents_detached: vec![uuid::Uuid::nil()],
        };
        let zero = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: false,
            changed: false,
            deleted: vec![],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        assert_shape(&memory_compaction_plan_json(&populated));
        assert_shape(&memory_compaction_plan_json(&zero));
    }

    #[test]
    fn memory_compaction_plan_json_pins_expected_receipt_changes_schema() {
        const EXPECTED_KEYS: &[&str] = &["mode", "reason", "records"];

        fn assert_expected_receipt_changes_shape(value: &serde_json::Value) {
            let block = value["expected_receipt_changes"].as_object().expect(
                "memory_compaction_plan_json expected_receipt_changes field must be an object",
            );
            let mut keys: Vec<String> = block.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_compaction_plan_json expected_receipt_changes keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(
                value["expected_receipt_changes"]["mode"].is_string(),
                "expected_receipt_changes.mode must be a string, not a structured object: {value}",
            );
            assert_eq!(
                value["expected_receipt_changes"]["mode"].as_str(),
                Some("none"),
                "dry-run compaction planning must report expected_receipt_changes.mode = none: {value}",
            );
            assert!(
                value["expected_receipt_changes"]["records"].is_array(),
                "expected_receipt_changes.records must be an array, not a string blob: {value}",
            );
            assert_eq!(
                value["expected_receipt_changes"]["records"]
                    .as_array()
                    .map(Vec::len),
                Some(0),
                "dry-run compaction planning must report empty expected_receipt_changes.records until receipt-aware compaction lands: {value}",
            );
            assert!(
                value["expected_receipt_changes"]["reason"].is_string(),
                "expected_receipt_changes.reason must be a string, not a structured object: {value}",
            );
        }

        let populated = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            deleted: vec![uuid::Uuid::nil()],
            stale_marked: vec![uuid::Uuid::nil()],
            parents_detached: vec![uuid::Uuid::nil()],
        };
        let zero = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: false,
            changed: false,
            deleted: vec![],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        assert_expected_receipt_changes_shape(&memory_compaction_plan_json(&populated));
        assert_expected_receipt_changes_shape(&memory_compaction_plan_json(&zero));
    }

    #[test]
    fn memory_read_json_renders_stable_shape() {
        let owner = AgentId::new("owner@local", [4u8; 32]);
        let record = MemoryRecord {
            id: uuid::Uuid::nil(),
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "memory read fixture".into(),
            embedding: vec![0.1, 0.2],
            metadata: serde_json::json!({"source": "test"}),
            created_at: 1_700_000_000_000,
            parent: None,
        };

        let value = memory_read_json(
            "search",
            Some(MemoryTier::Working),
            5,
            Some("memory read"),
            Some(0.6),
            &[record],
        );

        assert_eq!(value["kind"], "memory_read");
        assert_eq!(value["mode"], "search");
        assert_eq!(value["tier"], "working");
        assert_eq!(value["limit"], 5);
        assert_eq!(value["query"], "memory read");
        assert!(
            (value["min_relevance"].as_f64().unwrap() - 0.6).abs() < 1e-6,
            "min_relevance must round-trip the supplied threshold",
        );
        assert_eq!(value["records"][0]["text"], "memory read fixture");
        assert_eq!(value["records"][0]["tier"], "working");
        assert_eq!(value["records"][0]["owner"]["display"], owner.display);

        let recent = memory_read_json("recent", None, 3, None, None, &[]);
        assert!(recent["tier"].is_null());
        assert!(recent["query"].is_null());
        assert!(recent["min_relevance"].is_null());
        assert_eq!(recent["records"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn memory_read_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "kind",
            "limit",
            "min_relevance",
            "mode",
            "query",
            "records",
            "tier",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_read_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_read_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("memory_read"));
            assert!(value["mode"].is_string(), "mode must be a string: {value}");
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["tier"].is_string() || value["tier"].is_null(),
                "tier must be a string or null, never a structured object: {value}",
            );
            assert!(
                value["query"].is_string() || value["query"].is_null(),
                "query must be a string or null: {value}",
            );
            assert!(
                value["min_relevance"].is_f64() || value["min_relevance"].is_null(),
                "min_relevance must be a JSON number or null, never a string: {value}",
            );
            assert!(
                value["records"].is_array(),
                "records must be an array: {value}",
            );
        }

        let owner = AgentId::new("owner@local", [4u8; 32]);
        let record = MemoryRecord {
            id: uuid::Uuid::nil(),
            tier: MemoryTier::Working,
            owner,
            text: "memory read fixture".into(),
            embedding: vec![0.1, 0.2],
            metadata: serde_json::json!({"source": "test"}),
            created_at: 1_700_000_000_000,
            parent: None,
        };

        let search = memory_read_json(
            "search",
            Some(MemoryTier::Working),
            5,
            Some("memory read"),
            Some(0.6),
            &[record],
        );
        assert_shape(&search);

        let list = memory_read_json("recent", None, 3, None, None, &[]);
        assert_shape(&list);
    }

    #[test]
    fn ignore_report_json_renders_stable_shape() {
        let value = ignore_report_json(true, Some("*.pem"), 2);
        assert_eq!(value["kind"], "ignore_report");
        assert_eq!(value["ignored"], true);
        assert_eq!(value["matched_pattern"], "*.pem");
        assert_eq!(value["rules_loaded"], 2);

        let clear = ignore_report_json(false, None, 2);
        assert!(clear["matched_pattern"].is_null());
    }

    #[test]
    fn ignore_report_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["ignored", "kind", "matched_pattern", "rules_loaded"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("ignore_report_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "ignore_report_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("ignore_report"));
            assert!(
                value["ignored"].is_boolean(),
                "ignored must be a JSON bool, not 0/1 or a string: {value}",
            );
            assert!(
                value["matched_pattern"].is_string() || value["matched_pattern"].is_null(),
                "matched_pattern must be a string when matched and null when unmatched: {value}",
            );
            assert!(
                value["rules_loaded"].is_u64(),
                "rules_loaded must be a non-negative integer, not a string: {value}",
            );
        }

        assert_shape(&ignore_report_json(true, Some("*.pem"), 2));
        assert_shape(&ignore_report_json(false, None, 0));
    }

    #[test]
    fn tool_list_json_renders_stable_shape() {
        let spec = ToolSpec {
            name: "echo".into(),
            description: "Echo text".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
        };

        let value = tool_list_json(&[spec]);
        assert_eq!(value["kind"], "tool_list");
        assert_eq!(value["tools"][0]["name"], "echo");
        assert_eq!(value["tools"][0]["description"], "Echo text");
        assert_eq!(value["tools"][0]["inputSchema"]["type"], "object");
        assert_eq!(
            value["tools"][0]["inputSchema"]["properties"]["text"]["type"],
            "string"
        );
    }

    #[test]
    fn tool_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "tools"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("tool_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "tool_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("tool_list"));
            assert!(
                value["tools"].is_array(),
                "tools must be an array of tool specs, not a string blob: {value}",
            );
        }

        let spec = ToolSpec {
            name: "echo".into(),
            description: "Echo text".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
        };

        assert_shape(&tool_list_json(&[spec]));
        assert_shape(&tool_list_json(&[]));
    }

    #[test]
    fn tool_result_json_renders_stable_shape() {
        let content = vec![
            covenant_mcp::Content::Text {
                text: "hello".into(),
            },
            covenant_mcp::Content::Json {
                value: serde_json::json!({ "ok": true }),
            },
        ];

        let value = tool_result_json("echo", &content, false);
        assert_eq!(value["kind"], "tool_result");
        assert_eq!(value["name"], "echo");
        assert_eq!(value["is_error"], false);
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "hello");
        assert_eq!(value["content"][1]["type"], "json");
        assert_eq!(value["content"][1]["value"]["ok"], true);
    }

    #[test]
    fn tool_result_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["content", "is_error", "kind", "name"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("tool_result_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "tool_result_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("tool_result"));
            assert!(value["name"].is_string(), "name must be a string: {value}");
            assert!(
                value["content"].is_array(),
                "content must be an array: {value}",
            );
            assert!(
                value["is_error"].is_boolean(),
                "is_error must be a JSON boolean, not 0/1 or string: {value}",
            );
        }

        let content = vec![
            covenant_mcp::Content::Text {
                text: "hello".into(),
            },
            covenant_mcp::Content::Json {
                value: serde_json::json!({ "ok": true }),
            },
        ];

        assert_shape(&tool_result_json("echo", &content, true));
        assert_shape(&tool_result_json("echo", &[], false));
    }

    #[test]
    fn receipt_batch_list_json_renders_stable_shape() {
        let batch = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };
        let value = receipt_batch_list_json(10, &[batch]);
        assert_eq!(value["kind"], "receipt_batch_list");
        assert_eq!(value["limit"], 10);
        assert_eq!(value["batches"][0]["batch_id"], "batch-1");
        assert_eq!(value["batches"][0]["receipt_count"], 2);
        assert!(value["batches"][0]["tx_sig"].is_null());
        assert!(value["batches"][0]["slot"].is_null());
    }

    #[test]
    fn receipt_batch_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["batches", "kind", "limit"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("receipt_batch_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "receipt_batch_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("receipt_batch_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["batches"].is_array(),
                "batches must be an array: {value}",
            );
        }

        let batch = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };

        assert_shape(&receipt_batch_list_json(10, &[batch]));
        assert_shape(&receipt_batch_list_json(10, &[]));
    }

    #[test]
    fn chain_status_json_renders_stable_shape() {
        let status = ChainStatus {
            chain: "solana".into(),
            cluster: "localnet".into(),
            rpc_url: Some("http://127.0.0.1:8899".into()),
            ws_url: None,
            program_id: None,
            covnt_mint: Some("mint".into()),
            ready: false,
            missing: vec!["program_id".into()],
        };
        let value = chain_status_json(&status);
        assert_eq!(value["kind"], "chain_status");
        assert_eq!(value["status"]["chain"], "solana");
        assert_eq!(value["status"]["cluster"], "localnet");
        assert_eq!(value["status"]["rpc_url"], "http://127.0.0.1:8899");
        assert!(value["status"]["ws_url"].is_null());
        assert_eq!(value["status"]["ready"], false);
        assert_eq!(value["status"]["missing"][0], "program_id");
    }

    #[test]
    fn chain_status_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "status"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("chain_status_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "chain_status_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("chain_status"));
            assert!(
                value["status"].is_object(),
                "status must be a structured object, not a string blob: {value}",
            );
        }

        let ready = ChainStatus {
            chain: "solana".into(),
            cluster: "mainnet".into(),
            rpc_url: Some("https://api.mainnet-beta.solana.com".into()),
            ws_url: Some("wss://api.mainnet-beta.solana.com".into()),
            program_id: Some("11111111111111111111111111111111".into()),
            covnt_mint: Some("mint".into()),
            ready: true,
            missing: vec![],
        };
        let not_ready = ChainStatus {
            chain: "solana".into(),
            cluster: "localnet".into(),
            rpc_url: Some("http://127.0.0.1:8899".into()),
            ws_url: None,
            program_id: None,
            covnt_mint: Some("mint".into()),
            ready: false,
            missing: vec!["program_id".into()],
        };

        assert_shape(&chain_status_json(&ready));
        assert_shape(&chain_status_json(&not_ready));
    }

    #[test]
    fn verify_report_json_renders_stable_shape() {
        let checks = vec![VerifyCheck {
            name: "memory audit".into(),
            passed: false,
            message: "1 orphan".into(),
        }];
        let drift = vec![VerifyDrift {
            kind: "memory_without_audit".into(),
            id: Some("record-1".into()),
            message: "memory record has no matching audit row".into(),
            repair: "inspect before deleting".into(),
        }];
        let value = verify_report_json(100, &checks, &drift, 1);
        assert_eq!(value["kind"], "verify_report");
        assert_eq!(value["window"], 100);
        assert_eq!(value["orphans_total"], 1);
        assert_eq!(value["checks"][0]["name"], "memory audit");
        assert_eq!(value["checks"][0]["passed"], false);
        assert_eq!(value["drift"][0]["kind"], "memory_without_audit");
        assert_eq!(value["drift"][0]["id"], "record-1");
    }

    #[test]
    fn verify_report_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["checks", "drift", "kind", "orphans_total", "window"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("verify_report_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "verify_report_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("verify_report"));
            assert!(
                value["window"].is_u64(),
                "window must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["orphans_total"].is_u64(),
                "orphans_total must serialize as a non-negative integer, not a string-of-integer: {value}",
            );
            assert!(
                value["checks"].is_array(),
                "checks must be an array: {value}",
            );
            assert!(value["drift"].is_array(), "drift must be an array: {value}",);
        }

        let checks = vec![VerifyCheck {
            name: "memory audit".into(),
            passed: false,
            message: "1 orphan".into(),
        }];
        let drift = vec![VerifyDrift {
            kind: "memory_without_audit".into(),
            id: Some("record-1".into()),
            message: "memory record has no matching audit row".into(),
            repair: "inspect before deleting".into(),
        }];

        assert_shape(&verify_report_json(100, &checks, &drift, 1));
        assert_shape(&verify_report_json(100, &[], &[], 0));
    }

    #[test]
    fn flush_receipts_json_renders_stable_shape() {
        let batch = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };
        let value = flush_receipts_json(10, &batch, 7);
        assert_eq!(value["kind"], "receipt_batch_flushed");
        assert_eq!(value["limit"], 10);
        assert_eq!(value["receipts_updated"], 7);
        assert_eq!(value["batch"]["batch_id"], "batch-1");
        assert_eq!(value["batch"]["receipt_count"], 2);
        assert!(value["batch"]["tx_sig"].is_null());
        assert!(value["batch"]["slot"].is_null());
    }

    #[test]
    fn flush_receipts_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["batch", "kind", "limit", "receipts_updated"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("flush_receipts_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "flush_receipts_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("receipt_batch_flushed"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["receipts_updated"].is_u64(),
                "receipts_updated must serialize as a non-negative integer, not a string-of-integer: {value}",
            );
            assert!(
                value["batch"].is_object(),
                "batch must be a structured object, not a string blob: {value}",
            );
        }

        let unconfirmed = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };
        let confirmed = ReceiptBatchSummary {
            batch_id: "batch-2".into(),
            merkle_root: "cd".repeat(32),
            receipt_count: 5,
            tx_sig: Some("sigb58".into()),
            slot: Some(123_456),
        };

        assert_shape(&flush_receipts_json(10, &unconfirmed, 7));
        assert_shape(&flush_receipts_json(10, &confirmed, 5));
    }

    #[test]
    fn a2a_status_json_renders_stable_shape() {
        let sender = AgentId::new("sender@local", [1u8; 32]);
        let recipient = AgentId::new("recipient@local", [2u8; 32]);
        let task_id = uuid::Uuid::nil();
        let task = A2ATask {
            id: task_id,
            sender,
            recipient,
            intent_text: "status probe".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let entry = A2ATaskQueueEntry {
            state: A2ATaskQueueState::Queued,
            task,
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let result = A2ATaskResult::ok(task_id, vec![]);
        let value = a2a_status_json(
            5,
            Some(300_000),
            Some(60_000),
            Some(A2ATaskQueueState::InFlight),
            &[entry],
            &[result],
        );
        assert_eq!(value["kind"], "a2a_status");
        assert_eq!(value["limit"], 5);
        assert_eq!(value["min_lease_age_ms"], 300_000);
        assert_eq!(value["deadline_within_ms"], 60_000);
        assert_eq!(value["state_filter"], "in_flight");
        assert_eq!(value["tasks"][0]["state"], "queued");
        assert_eq!(value["tasks"][0]["task"]["intent_text"], "status probe");
        assert_eq!(value["results"][0]["status"], "ok");
    }

    #[test]
    fn a2a_status_json_omits_deadline_filter_when_inactive() {
        let value = a2a_status_json(5, None, None, None, &[], &[]);
        assert_eq!(value["kind"], "a2a_status");
        assert!(value["min_lease_age_ms"].is_null());
        assert!(value["deadline_within_ms"].is_null());
        assert!(value["state_filter"].is_null());
    }

    #[test]
    fn a2a_status_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "deadline_within_ms",
            "kind",
            "limit",
            "min_lease_age_ms",
            "results",
            "state_filter",
            "tasks",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("a2a_status_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "a2a_status_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("a2a_status"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["min_lease_age_ms"].is_u64() || value["min_lease_age_ms"].is_null(),
                "min_lease_age_ms must be u64-or-null (never a string-of-integer): {value}",
            );
            assert!(
                value["deadline_within_ms"].is_u64() || value["deadline_within_ms"].is_null(),
                "deadline_within_ms must be u64-or-null (never a string-of-integer): {value}",
            );
            assert!(
                value["state_filter"].is_string() || value["state_filter"].is_null(),
                "state_filter must be string-or-null (never integer / array): {value}",
            );
            assert!(value["tasks"].is_array(), "tasks must be an array: {value}",);
            assert!(
                value["results"].is_array(),
                "results must be an array: {value}",
            );
        }

        let sender = AgentId::new("sender@local", [1u8; 32]);
        let recipient = AgentId::new("recipient@local", [2u8; 32]);
        let task_id = uuid::Uuid::nil();
        let task = A2ATask {
            id: task_id,
            sender,
            recipient,
            intent_text: "status probe".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let entry = A2ATaskQueueEntry {
            state: A2ATaskQueueState::Queued,
            task,
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let result = A2ATaskResult::ok(task_id, vec![]);

        assert_shape(&a2a_status_json(
            5,
            Some(300_000),
            Some(60_000),
            Some(A2ATaskQueueState::InFlight),
            std::slice::from_ref(&entry),
            std::slice::from_ref(&result),
        ));
        assert_shape(&a2a_status_json(
            5,
            Some(300_000),
            Some(60_000),
            Some(A2ATaskQueueState::Queued),
            &[],
            &[],
        ));
        assert_shape(&a2a_status_json(5, None, None, None, &[entry], &[result]));
        assert_shape(&a2a_status_json(5, None, None, None, &[], &[]));
    }

    #[test]
    fn memory_tier_slug_pins_documented_slugs_and_round_trips_through_parse_tier() {
        assert_eq!(memory_tier_slug(MemoryTier::Working), "working");
        assert_eq!(memory_tier_slug(MemoryTier::Episodic), "episodic");
        assert_eq!(memory_tier_slug(MemoryTier::LongTerm), "longterm");

        for tier in [
            MemoryTier::Working,
            MemoryTier::Episodic,
            MemoryTier::LongTerm,
        ] {
            assert_eq!(
                parse_tier(memory_tier_slug(tier)).unwrap(),
                tier,
                "memory_tier_slug and parse_tier must round-trip; a slug rewording on one side without the other silently breaks JSON envelopes and CLI flags",
            );
        }
    }

    #[test]
    fn parse_tier_accepts_documented_spellings_and_rejects_unknown() {
        assert_eq!(parse_tier("working").unwrap(), MemoryTier::Working);
        assert_eq!(parse_tier("episodic").unwrap(), MemoryTier::Episodic);
        assert_eq!(parse_tier("longterm").unwrap(), MemoryTier::LongTerm);
        assert_eq!(parse_tier("long-term").unwrap(), MemoryTier::LongTerm);
        assert_eq!(parse_tier("long_term").unwrap(), MemoryTier::LongTerm);

        let err = parse_tier("workimg").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown tier"),
            "rejection must include the typed prefix so the CLI surface is recognisable: {err:?}",
        );
        assert!(
            msg.contains("workimg"),
            "rejection must echo the offending value so a typo is debuggable: {err:?}",
        );
    }

    #[test]
    fn parse_duplicate_risk_accepts_documented_spellings_and_rejects_unknown() {
        assert_eq!(
            parse_duplicate_risk("idempotent").unwrap(),
            A2ADuplicateRisk::Idempotent,
        );
        assert_eq!(
            parse_duplicate_risk("operator-accepted").unwrap(),
            A2ADuplicateRisk::OperatorAccepted,
        );
        assert_eq!(
            parse_duplicate_risk("operator_accepted").unwrap(),
            A2ADuplicateRisk::OperatorAccepted,
        );

        let err = parse_duplicate_risk("safe").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown duplicate risk"),
            "rejection must include the typed prefix so the CLI surface is recognisable: {err:?}",
        );
        assert!(
            msg.contains("safe"),
            "rejection must echo the offending value so a typo is debuggable: {err:?}",
        );
    }

    #[test]
    fn parse_a2a_queue_state_accepts_both_spellings() {
        assert_eq!(
            parse_a2a_queue_state("queued").unwrap(),
            A2ATaskQueueState::Queued,
        );
        assert_eq!(
            parse_a2a_queue_state("in_flight").unwrap(),
            A2ATaskQueueState::InFlight,
        );
        assert_eq!(
            parse_a2a_queue_state("in-flight").unwrap(),
            A2ATaskQueueState::InFlight,
        );
        let err = parse_a2a_queue_state("InFlight").unwrap_err();
        assert!(
            err.to_string().contains("unknown a2a state"),
            "case-sensitive parser must reject mixed case: {err:?}",
        );
        let err = parse_a2a_queue_state("queued ").unwrap_err();
        assert!(
            err.to_string().contains("unknown a2a state"),
            "trailing whitespace must be rejected so a typo does not silently disable the filter: {err:?}",
        );
    }

    #[test]
    fn resource_name_pins_documented_slugs_and_matches_serde_lowercase_form() {
        assert_eq!(resource_name(ResourceKind::Compute), "compute");
        assert_eq!(resource_name(ResourceKind::Memory), "memory");
        assert_eq!(resource_name(ResourceKind::Tool), "tool");
        assert_eq!(resource_name(ResourceKind::Message), "message");
        assert_eq!(resource_name(ResourceKind::Registration), "registration");

        for kind in [
            ResourceKind::Compute,
            ResourceKind::Memory,
            ResourceKind::Tool,
            ResourceKind::Message,
            ResourceKind::Registration,
        ] {
            let wire = serde_json::to_value(kind).unwrap();
            assert_eq!(
                wire.as_str(),
                Some(resource_name(kind)),
                "CLI slug and serde wire form must agree so logs and JSON envelopes can be cross-referenced without a translation table: {kind:?}",
            );
        }
    }

    #[test]
    fn parse_uuid_returns_value_on_documented_uuid_and_reports_field_name_on_invalid_input() {
        let literal = "550e8400-e29b-41d4-a716-446655440000";
        let expected = uuid::Uuid::parse_str(literal).unwrap();
        assert_eq!(
            parse_uuid(literal, "memory-id").unwrap(),
            expected,
            "parse_uuid must accept the same hyphenated UUID form as uuid::Uuid::parse_str so JSON envelopes and CLI flags agree on the canonical spelling",
        );

        let err = parse_uuid("not-a-uuid", "memory-id").unwrap_err();
        assert!(
            err.to_string().contains("memory-id"),
            "invalid UUID rejection must bind the caller-provided field name so the CLI can map a single parse failure back to the flag the operator typed: {err:?}",
        );

        let err = parse_uuid("", "task-id").unwrap_err();
        assert!(
            err.to_string().contains("task-id"),
            "empty input must still bind the field name so an unset flag does not look like a generic UUID error: {err:?}",
        );
    }

    #[test]
    fn peer_revoke_json_exit_classification_matches_human_cli() {
        let p = make_peer(7, "alice@host", false);
        assert!(!peer_revoke_is_failure(&RevokeOutcome::Revoked(p.clone())));
        assert!(!peer_revoke_is_failure(&RevokeOutcome::AlreadyRevoked(
            p.clone()
        )));
        assert!(peer_revoke_is_failure(&RevokeOutcome::NotFound));
        assert!(peer_revoke_is_failure(&RevokeOutcome::Ambiguous {
            matches: vec![p.clone()],
            truncated: false,
        }));
        assert!(peer_revoke_is_failure(&RevokeOutcome::SelfRevokeForbidden(
            p
        )));
    }

    #[test]
    fn decode_intent_stream_reassembles_response_from_chunk_and_summary() {
        // Daemon's stream_submit_intent emits one StreamChunk carrying
        // AgentResult { text, sources, runtime_events:[] } and a
        // StreamEnd.summary holding intent_id/status/settlement. The
        // CLI must reassemble those into Response::IntentResult so the
        // print branch is unchanged from v1.
        let intent_id = uuid::Uuid::from_u128(0xABCD_1234);
        let chunk = serde_json::json!({
            "text": "answer body",
            "sources": ["doc://a", "doc://b"],
            "runtime_events": []
        });
        let summary = serde_json::json!({
            "intent_id": intent_id,
            "status": "ok",
            "settlement": null,
        });
        let response =
            decode_intent_stream(vec![chunk], Some(summary)).expect("happy intent stream decodes");
        match response {
            Response::IntentResult {
                intent_id: id,
                status,
                text,
                sources,
                settlement,
            } => {
                assert_eq!(id, intent_id);
                assert_eq!(status, "ok");
                assert_eq!(text, "answer body");
                assert_eq!(sources, vec!["doc://a", "doc://b"]);
                assert!(settlement.is_none());
            }
            other => panic!("expected IntentResult, got {other:?}"),
        }
    }

    #[test]
    fn decode_intent_stream_rejects_wrong_chunk_count() {
        // The streamed intent path emits exactly one chunk per ADR.
        // Two chunks means the daemon changed its emit pattern; fail
        // loudly so the CLI doesn't silently drop or duplicate output.
        let summary = serde_json::json!({
            "intent_id": uuid::Uuid::nil(),
            "status": "ok",
            "settlement": null,
        });
        let err = decode_intent_stream(
            vec![
                serde_json::json!({"text": "a"}),
                serde_json::json!({"text": "b"}),
            ],
            Some(summary),
        )
        .expect_err("two-chunk intent stream must error");
        assert!(
            err.to_string().contains("exactly one"),
            "error must name the contract: {err}"
        );
    }

    #[test]
    fn decode_intent_stream_requires_summary() {
        // StreamEnd without a summary is a daemon protocol bug; without
        // it the CLI cannot reconstruct intent_id/status/settlement.
        let chunk = serde_json::json!({"text": "answer", "sources": []});
        let err = decode_intent_stream(vec![chunk], None).expect_err("missing summary must error");
        assert!(
            err.to_string().contains("missing summary"),
            "error names the missing field: {err}"
        );
    }

    #[test]
    fn peers_list_status_filter_resolves_three_branches_and_rejects_both() {
        // No flag → no filter; the wire default that surfaces both halves.
        assert_eq!(peers_list_status_filter(false, false).unwrap(), None);
        assert_eq!(
            peers_list_status_filter(true, false).unwrap(),
            Some(PeerStatusFilter::Live)
        );
        assert_eq!(
            peers_list_status_filter(false, true).unwrap(),
            Some(PeerStatusFilter::Revoked)
        );
        // Both flags set is operationally a footgun (silently empty
        // result against the registry); rejected at parse time.
        let err = peers_list_status_filter(true, true).unwrap_err();
        assert!(
            err.to_string().contains("mutually exclusive"),
            "error mentions mutual exclusion: {err}"
        );
    }

    mod keypair_loader {
        use super::super::{
            classify_keypair_read_error, compute_default_keypair_path, load_operator_keypair,
            resolve_operator_keypair_path, KeypairLoadError,
        };
        use solana_sdk::signer::{keypair::Keypair, Signer};
        use std::io::Write;
        use std::path::PathBuf;
        use tempfile::tempdir;

        fn write_bytes(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
            let path = dir.join(name);
            let mut f = std::fs::File::create(&path).expect("create fixture");
            f.write_all(bytes).expect("write fixture");
            path
        }

        #[test]
        fn happy_path_returns_keypair_matching_fixture() {
            // Generate a real Solana keypair, persist its 64 bytes in the
            // canonical JSON array form, and confirm the loader produces a
            // Keypair with the matching public key.
            let dir = tempdir().expect("tempdir");
            let kp = Keypair::new();
            let json = serde_json::to_vec(&kp.to_bytes().to_vec()).expect("serialize bytes");
            let path = write_bytes(dir.path(), "id.json", &json);
            let loaded = load_operator_keypair(Some(path)).expect("happy path");
            assert_eq!(loaded.pubkey(), kp.pubkey());
        }

        #[test]
        fn missing_file_returns_missing_variant() {
            // An explicit path to a never-created file must surface
            // MissingFile, never the generic NotReadable bucket, so the
            // operator sees "the file is absent" instead of an opaque IO
            // error.
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("absent.json");
            let err = load_operator_keypair(Some(path.clone())).expect_err("must error");
            match err {
                KeypairLoadError::MissingFile { path: p, .. } => assert_eq!(p, path),
                other => panic!("expected MissingFile, got {other:?}"),
            }
        }

        #[test]
        fn malformed_json_returns_malformed_variant() {
            // The file exists but its content is not a JSON byte array;
            // serde_json::from_slice rejects it and we must classify the
            // failure as MalformedJson, not WrongByteCount, so the
            // operator knows the file is corrupt rather than truncated.
            let dir = tempdir().expect("tempdir");
            let path = write_bytes(dir.path(), "id.json", b"not json at all");
            let err = load_operator_keypair(Some(path)).expect_err("must error");
            assert!(
                matches!(err, KeypairLoadError::MalformedJson { .. }),
                "expected MalformedJson, got {err:?}"
            );
        }

        #[test]
        fn wrong_byte_count_returns_explicit_count() {
            // A short JSON array (3 bytes) is the silent-wrong-pubkey
            // failure mode: solana_sdk::Keypair::from_bytes accepting a
            // truncated slice would derive a wrong identity. The loader
            // must reject any count != 64 with the actual count surfaced.
            let dir = tempdir().expect("tempdir");
            let path = write_bytes(dir.path(), "id.json", b"[1,2,3]");
            let err = load_operator_keypair(Some(path)).expect_err("must error");
            match err {
                KeypairLoadError::WrongByteCount { actual, .. } => assert_eq!(actual, 3),
                other => panic!("expected WrongByteCount, got {other:?}"),
            }
        }

        #[test]
        fn classify_read_error_distinguishes_missing_from_permission_denied() {
            // The two error classes must be distinct so an operator can
            // tell "the file is absent" from "the daemon process lacks
            // read permission on the file". Both map from std::io::Error
            // kinds, so we exercise the classifier directly without
            // depending on the host filesystem's permission model.
            let p = PathBuf::from("/cov-test/keypair.json");
            let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
            assert!(matches!(
                classify_keypair_read_error(p.clone(), not_found),
                KeypairLoadError::MissingFile { .. }
            ));
            let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
            assert!(matches!(
                classify_keypair_read_error(p.clone(), denied),
                KeypairLoadError::PermissionDenied { .. }
            ));
            let other = std::io::Error::from(std::io::ErrorKind::Interrupted);
            assert!(matches!(
                classify_keypair_read_error(p, other),
                KeypairLoadError::NotReadable { .. }
            ));
        }

        #[test]
        fn resolve_path_prefers_explicit_value_over_default() {
            let explicit = PathBuf::from("/tmp/cov-test/explicit.json");
            let resolved =
                resolve_operator_keypair_path(Some(explicit.clone())).expect("explicit wins");
            assert_eq!(resolved, explicit);
        }

        #[test]
        fn default_path_follows_solana_cli_convention() {
            // The canonical Solana CLI keypair lives at
            // $HOME/.config/solana/id.json. We test the pure helper so
            // the test does not race against other tests mutating HOME.
            assert_eq!(
                compute_default_keypair_path("/u/op"),
                PathBuf::from("/u/op/.config/solana/id.json")
            );
        }
    }

    mod anchor_discriminator {
        use super::super::compute_anchor_global_discriminator;

        // These three byte arrays are the canonical Anchor instruction
        // discriminators the on-chain settlement program (declared in
        // agent-os/programs/settlement/src/lib.rs) accepts for the three
        // operator-signed verbs. Independently reproducible with:
        //   python3 -c 'import hashlib;\
        //     print(list(hashlib.sha256(b"global:register_agent").digest()[:8]))'
        // A regression in compute_anchor_global_discriminator that drops
        // the "global:" prefix, hashes the wrong slice of the digest, or
        // accepts a non-snake-case method name would silently produce
        // bytes the dispatcher routes to InstructionFallbackNotFound; the
        // tests below pin the byte stream so that regression is loud.

        #[test]
        fn register_agent_matches_on_chain_discriminator() {
            assert_eq!(
                compute_anchor_global_discriminator("register_agent"),
                [135, 157, 66, 195, 2, 113, 175, 30]
            );
        }

        #[test]
        fn stake_matches_on_chain_discriminator() {
            assert_eq!(
                compute_anchor_global_discriminator("stake"),
                [206, 176, 202, 18, 200, 209, 179, 108]
            );
        }

        #[test]
        fn buy_credits_matches_on_chain_discriminator() {
            assert_eq!(
                compute_anchor_global_discriminator("buy_credits"),
                [14, 173, 58, 38, 248, 235, 115, 102]
            );
        }

        #[test]
        fn global_prefix_is_part_of_the_hash() {
            // A drop of the "global:" namespace would silently produce
            // a different byte stream. This test pins the difference so
            // a refactor that mistakenly hashes the bare method name
            // fails loudly instead of producing valid-looking but
            // unroutable bytes.
            use sha2::{Digest, Sha256};
            let bare = {
                let mut h = Sha256::new();
                h.update(b"register_agent");
                let d = h.finalize();
                let mut out = [0u8; 8];
                out.copy_from_slice(&d[..8]);
                out
            };
            assert_ne!(
                compute_anchor_global_discriminator("register_agent"),
                bare,
                "discriminator must include the 'global:' namespace prefix"
            );
        }

        #[test]
        fn snake_case_and_camel_case_produce_different_discriminators() {
            // Anchor's macro-generated dispatcher uses the snake_case
            // method identifier from the #[program] mod. A caller that
            // passes a wrong-case name will compute a digest the on-chain
            // dispatcher does not accept; this test pins the difference
            // so the failure is visible as a discriminator mismatch
            // rather than a silent on-chain rejection.
            assert_ne!(
                compute_anchor_global_discriminator("register_agent"),
                compute_anchor_global_discriminator("registerAgent")
            );
            assert_ne!(
                compute_anchor_global_discriminator("register_agent"),
                compute_anchor_global_discriminator("RegisterAgent")
            );
        }
    }

    mod build_register_agent_instruction {
        use super::super::{
            build_register_agent_instruction, compute_anchor_global_discriminator,
            serialize_register_agent_args, settlement_agent_pda, settlement_config_pda,
            RegisterAgentArgs,
        };
        use solana_sdk::pubkey::Pubkey;

        fn fixed_program() -> Pubkey {
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_operator() -> Pubkey {
            Pubkey::new_from_array([5u8; 32])
        }

        fn fixture_args() -> RegisterAgentArgs {
            RegisterAgentArgs {
                agent_key: [9u8; 32],
                metadata_hash: [10u8; 32],
                capability_hash: [11u8; 32],
            }
        }

        #[test]
        fn data_is_discriminator_followed_by_serialized_args() {
            // The on-chain Anchor dispatcher reads the first 8 bytes as
            // the instruction discriminator and the remainder as the
            // borsh-encoded args payload. Any other layout (args first,
            // discriminator omitted, padding) routes to
            // InstructionFallbackNotFound.
            let ix = build_register_agent_instruction(
                &fixed_program(),
                &fixed_operator(),
                &fixture_args(),
            );
            let disc = compute_anchor_global_discriminator("register_agent");
            assert_eq!(&ix.data[..8], &disc, "data prefix must be discriminator");
            let args_bytes = serialize_register_agent_args(&fixture_args());
            assert_eq!(
                &ix.data[8..],
                &args_bytes,
                "data tail must be serialized args"
            );
            assert_eq!(ix.data.len(), 8 + 96);
        }

        #[test]
        fn accounts_follow_on_chain_struct_order() {
            // agent-os/programs/settlement/src/lib.rs:429-448 declares
            // RegisterAgent { config, agent, operator, system_program }.
            // The Instruction.accounts vector must match positionally.
            let program = fixed_program();
            let operator = fixed_operator();
            let args = fixture_args();
            let ix = build_register_agent_instruction(&program, &operator, &args);
            assert_eq!(ix.accounts.len(), 4);
            assert_eq!(ix.accounts[0].pubkey, settlement_config_pda(&program).0);
            assert_eq!(
                ix.accounts[1].pubkey,
                settlement_agent_pda(&program, &Pubkey::new_from_array(args.agent_key)).0
            );
            assert_eq!(ix.accounts[2].pubkey, operator);
            assert_eq!(ix.accounts[3].pubkey, solana_sdk::system_program::id());
        }

        #[test]
        fn account_meta_flags_match_on_chain_struct_attributes() {
            // config        — read-only (no #[account(mut)])
            // agent         — writable (#[account(init, ...)]), not signer
            // operator      — signer + writable (#[account(mut)] + Signer)
            // system_program— read-only (Program<...>)
            let ix = build_register_agent_instruction(
                &fixed_program(),
                &fixed_operator(),
                &fixture_args(),
            );
            assert_eq!(
                (ix.accounts[0].is_signer, ix.accounts[0].is_writable),
                (false, false)
            );
            assert_eq!(
                (ix.accounts[1].is_signer, ix.accounts[1].is_writable),
                (false, true)
            );
            assert_eq!(
                (ix.accounts[2].is_signer, ix.accounts[2].is_writable),
                (true, true)
            );
            assert_eq!(
                (ix.accounts[3].is_signer, ix.accounts[3].is_writable),
                (false, false)
            );
        }

        #[test]
        fn program_id_is_propagated() {
            let program = fixed_program();
            let ix = build_register_agent_instruction(&program, &fixed_operator(), &fixture_args());
            assert_eq!(ix.program_id, program);
        }
    }

    mod build_stake_instruction {
        use super::super::{
            build_stake_instruction, compute_anchor_global_discriminator, serialize_stake_args,
            settlement_agent_pda, settlement_config_pda, settlement_stake_position_pda, StakeArgs,
            SPL_TOKEN_PROGRAM_ID,
        };
        use solana_sdk::pubkey::Pubkey;

        fn fixed_program() -> Pubkey {
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_operator() -> Pubkey {
            Pubkey::new_from_array([5u8; 32])
        }

        fn fixed_agent_key() -> Pubkey {
            Pubkey::new_from_array([7u8; 32])
        }

        fn fixed_owner_covnt() -> Pubkey {
            Pubkey::new_from_array([13u8; 32])
        }

        fn fixed_stake_vault() -> Pubkey {
            Pubkey::new_from_array([17u8; 32])
        }

        fn fixture_args() -> StakeArgs {
            StakeArgs {
                amount: 1_500_000,
                lock_until: 1_700_999_999,
            }
        }

        fn build_fixture_ix() -> solana_sdk::instruction::Instruction {
            build_stake_instruction(
                &fixed_program(),
                &fixed_operator(),
                &fixed_agent_key(),
                &fixed_owner_covnt(),
                &fixed_stake_vault(),
                &Pubkey::new_from_array([21u8; 32]),
                &fixture_args(),
            )
        }

        #[test]
        fn data_prefix_equals_stake_discriminator_literal() {
            // The literal stake discriminator is also pinned by the
            // existing anchor_discriminator mod. Repeating the byte
            // sequence here makes a regression in either source
            // surface as a duplicate failure rather than a silent
            // pass through one path.
            let ix = build_fixture_ix();
            assert_eq!(
                &ix.data[..8],
                &[206, 176, 202, 18, 200, 209, 179, 108],
                "data prefix must be the stake discriminator"
            );
            assert_eq!(&ix.data[..8], &compute_anchor_global_discriminator("stake"));
        }

        #[test]
        fn data_tail_equals_serialized_stake_args() {
            let ix = build_fixture_ix();
            assert_eq!(
                &ix.data[8..],
                &serialize_stake_args(&fixture_args()),
                "data tail must be the borsh-encoded StakeArgs"
            );
        }

        #[test]
        fn data_length_is_discriminator_plus_two_u64s() {
            // 8 (discriminator) + 16 (StakeArgs) = 24. A regression
            // that pads to alignment or drops the discriminator
            // would change this.
            let ix = build_fixture_ix();
            assert_eq!(ix.data.len(), 8 + 16);
        }

        #[test]
        fn accounts_follow_on_chain_struct_order_positionally() {
            // agent-os/programs/settlement/src/lib.rs:531-567 declares
            // Stake { config, agent, position, owner, owner_covnt,
            // stake_vault, token_program, system_program }.
            let program = fixed_program();
            let operator = fixed_operator();
            let agent_key = fixed_agent_key();
            let ix = build_fixture_ix();

            assert_eq!(ix.accounts.len(), 9);
            assert_eq!(ix.accounts[0].pubkey, settlement_config_pda(&program).0);
            assert_eq!(
                ix.accounts[1].pubkey,
                settlement_agent_pda(&program, &agent_key).0
            );
            assert_eq!(
                ix.accounts[2].pubkey,
                settlement_stake_position_pda(&program, &agent_key, &operator).0
            );
            assert_eq!(ix.accounts[3].pubkey, operator);
            assert_eq!(ix.accounts[4].pubkey, fixed_owner_covnt());
            assert_eq!(ix.accounts[5].pubkey, fixed_stake_vault());
            assert_eq!(ix.accounts[6].pubkey, Pubkey::new_from_array([21u8; 32]));
            assert_eq!(ix.accounts[7].pubkey, SPL_TOKEN_PROGRAM_ID);
            assert_eq!(ix.accounts[8].pubkey, solana_sdk::system_program::id());
        }

        #[test]
        fn account_meta_flags_match_on_chain_struct_attributes() {
            // config:         ro
            // agent:          w, !signer
            // position:       w, !signer (init by owner)
            // owner:          signer, w (fee payer + transfer authority)
            // owner_covnt:    w, !signer
            // stake_vault:    w, !signer
            // token_program:  ro
            // system_program: ro
            let ix = build_fixture_ix();
            let expected = [
                (false, false), // config
                (false, true),  // agent
                (false, true),  // position
                (true, true),   // owner
                (false, true),  // owner_covnt
                (false, true),  // stake_vault
                (false, false), // token_program
                (false, false), // system_program
            ];
            for (i, exp) in expected.iter().enumerate() {
                assert_eq!(
                    (ix.accounts[i].is_signer, ix.accounts[i].is_writable),
                    *exp,
                    "account[{i}] flag mismatch (expected (signer, writable) = {exp:?})"
                );
            }
        }

        #[test]
        fn token_program_id_is_legacy_spl_token_constant() {
            // Pin the canonical legacy-Token program ID inline. A
            // substitution with the Token-2022 program at
            // TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb would be
            // rejected on-chain with InvalidProgramId — this test
            // surfaces the substitution locally.
            assert_eq!(
                SPL_TOKEN_PROGRAM_ID.to_string(),
                "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
            );
        }

        #[test]
        fn program_id_is_propagated() {
            let program = fixed_program();
            let ix = build_stake_instruction(
                &program,
                &fixed_operator(),
                &fixed_agent_key(),
                &fixed_owner_covnt(),
                &fixed_stake_vault(),
                &Pubkey::new_from_array([21u8; 32]),
                &fixture_args(),
            );
            assert_eq!(ix.program_id, program);
        }
    }

    mod build_buy_credits_instruction {
        use super::super::{
            build_buy_credits_instruction, compute_anchor_global_discriminator,
            serialize_buy_credits_args, settlement_config_pda, settlement_credits_pda,
            BuyCreditsArgs, SPL_TOKEN_PROGRAM_ID,
        };
        use solana_sdk::pubkey::Pubkey;

        fn fixed_program() -> Pubkey {
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_operator() -> Pubkey {
            Pubkey::new_from_array([5u8; 32])
        }

        fn fixed_owner_covnt() -> Pubkey {
            Pubkey::new_from_array([19u8; 32])
        }

        fn fixed_treasury() -> Pubkey {
            Pubkey::new_from_array([23u8; 32])
        }

        fn fixture_args() -> BuyCreditsArgs {
            BuyCreditsArgs {
                amount_covnt: 7_777_777,
            }
        }

        fn build_fixture_ix() -> solana_sdk::instruction::Instruction {
            build_buy_credits_instruction(
                &fixed_program(),
                &fixed_operator(),
                &fixed_owner_covnt(),
                &fixed_treasury(),
                &Pubkey::new_from_array([22u8; 32]),
                &fixture_args(),
            )
        }

        #[test]
        fn data_prefix_equals_buy_credits_discriminator_literal() {
            // The literal buy_credits discriminator is also
            // pinned by the existing anchor_discriminator mod.
            // Repeating it here makes a regression in either
            // source surface as a duplicate failure rather than
            // a silent pass through one path.
            let ix = build_fixture_ix();
            assert_eq!(
                &ix.data[..8],
                &[14, 173, 58, 38, 248, 235, 115, 102],
                "data prefix must be the buy_credits discriminator"
            );
            assert_eq!(
                &ix.data[..8],
                &compute_anchor_global_discriminator("buy_credits")
            );
        }

        #[test]
        fn data_tail_equals_serialized_buy_credits_args() {
            let ix = build_fixture_ix();
            assert_eq!(
                &ix.data[8..],
                &serialize_buy_credits_args(&fixture_args()),
                "data tail must be the borsh-encoded BuyCreditsArgs"
            );
        }

        #[test]
        fn data_length_is_discriminator_plus_one_u64() {
            // 8 (discriminator) + 8 (BuyCreditsArgs) = 16. A
            // regression that pads to alignment or drops the
            // discriminator would change this.
            assert_eq!(build_fixture_ix().data.len(), 8 + 8);
        }

        #[test]
        fn accounts_follow_on_chain_struct_order_positionally() {
            // agent-os/programs/settlement/src/lib.rs:483-503 declares
            // BuyCredits { config, credits, owner, owner_covnt,
            // treasury, token_program }.
            let program = fixed_program();
            let operator = fixed_operator();
            let ix = build_fixture_ix();

            assert_eq!(ix.accounts.len(), 7);
            assert_eq!(ix.accounts[0].pubkey, settlement_config_pda(&program).0);
            assert_eq!(
                ix.accounts[1].pubkey,
                settlement_credits_pda(&program, &operator).0
            );
            assert_eq!(ix.accounts[2].pubkey, operator);
            assert_eq!(ix.accounts[3].pubkey, fixed_owner_covnt());
            assert_eq!(ix.accounts[4].pubkey, fixed_treasury());
            assert_eq!(ix.accounts[5].pubkey, Pubkey::new_from_array([22u8; 32]));
            assert_eq!(ix.accounts[6].pubkey, SPL_TOKEN_PROGRAM_ID);
        }

        #[test]
        fn account_meta_flags_match_on_chain_struct_attributes() {
            // config:        ro
            // credits:       w, !signer (#[account(mut)])
            // owner:         signer, w (fee payer + transfer authority)
            // owner_covnt:   w, !signer
            // treasury:      w, !signer
            // token_program: ro
            let ix = build_fixture_ix();
            let expected = [
                (false, false), // config
                (false, true),  // credits
                (true, true),   // owner
                (false, true),  // owner_covnt
                (false, true),  // treasury
                (false, false), // token_program
            ];
            for (i, exp) in expected.iter().enumerate() {
                assert_eq!(
                    (ix.accounts[i].is_signer, ix.accounts[i].is_writable),
                    *exp,
                    "account[{i}] flag mismatch (expected (signer, writable) = {exp:?})"
                );
            }
        }

        #[test]
        fn token_program_id_is_legacy_spl_token_constant() {
            assert_eq!(
                SPL_TOKEN_PROGRAM_ID.to_string(),
                "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
            );
        }

        #[test]
        fn program_id_is_propagated() {
            let program = fixed_program();
            let ix = build_buy_credits_instruction(
                &program,
                &fixed_operator(),
                &fixed_owner_covnt(),
                &fixed_treasury(),
                &Pubkey::new_from_array([22u8; 32]),
                &fixture_args(),
            );
            assert_eq!(ix.program_id, program);
        }
    }

    mod register_agent_args {
        use super::super::{serialize_register_agent_args, RegisterAgentArgs};
        use borsh::BorshDeserialize;

        fn fixture() -> RegisterAgentArgs {
            RegisterAgentArgs {
                agent_key: [1u8; 32],
                metadata_hash: [2u8; 32],
                capability_hash: [3u8; 32],
            }
        }

        #[test]
        fn encoded_bytes_match_struct_declaration_order() {
            // The on-chain RegisterAgentArgs struct declares fields in
            // order agent_key, metadata_hash, capability_hash. Borsh
            // serializes in struct order, so the wire bytes must be the
            // 96-byte concatenation in that exact order. The expected
            // bytes are spelled out inline so a refactor that switches
            // to data_keys order would break the assertion loudly.
            let mut expected = Vec::with_capacity(96);
            expected.extend_from_slice(&[1u8; 32]); // agent_key
            expected.extend_from_slice(&[2u8; 32]); // metadata_hash
            expected.extend_from_slice(&[3u8; 32]); // capability_hash
            assert_eq!(serialize_register_agent_args(&fixture()), expected);
            assert_eq!(serialize_register_agent_args(&fixture()).len(), 96);
        }

        #[test]
        fn round_trip_via_borsh_returns_same_struct() {
            // Anchor's on-chain dispatcher decodes the args via the same
            // borsh wire format we emit here. A round-trip with
            // BorshDeserialize confirms our encoded bytes can be parsed
            // back to the identical struct — if a field were skipped or
            // re-ordered, the deserialized struct would differ.
            let original = fixture();
            let bytes = serialize_register_agent_args(&original);
            let decoded = RegisterAgentArgs::try_from_slice(&bytes).expect("decodes");
            assert_eq!(decoded, original);
        }

        #[test]
        fn each_field_contributes_to_the_encoded_output() {
            // A regression that silently dropped any one field would
            // produce a shorter byte stream that still decodes if borsh
            // is permissive about trailing length. The on-chain program
            // is strict, so we pin per-field byte sensitivity: mutating
            // each field changes the output, proving no field is
            // skipped.
            let base = serialize_register_agent_args(&fixture());

            let mut a = fixture();
            a.agent_key[0] = 99;
            assert_ne!(serialize_register_agent_args(&a), base);

            let mut m = fixture();
            m.metadata_hash[0] = 99;
            assert_ne!(serialize_register_agent_args(&m), base);

            let mut c = fixture();
            c.capability_hash[0] = 99;
            assert_ne!(serialize_register_agent_args(&c), base);
        }
    }

    mod cluster_rpc_url {
        use super::super::{resolve_solana_rpc_url, ClusterResolveError};

        #[test]
        fn devnet_resolves_to_canonical_url() {
            assert_eq!(
                resolve_solana_rpc_url(Some("devnet"), None).unwrap(),
                "https://api.devnet.solana.com"
            );
        }

        #[test]
        fn localnet_resolves_to_loopback_rpc() {
            assert_eq!(
                resolve_solana_rpc_url(Some("localnet"), None).unwrap(),
                "http://127.0.0.1:8899"
            );
        }

        #[test]
        fn mainnet_and_mainnet_beta_are_aliases() {
            // Solana CLI accepts --url mainnet-beta. Operators following
            // that convention must not see UnknownCluster for it.
            let mb = resolve_solana_rpc_url(Some("mainnet-beta"), None).unwrap();
            let mn = resolve_solana_rpc_url(Some("mainnet"), None).unwrap();
            assert_eq!(mb, "https://api.mainnet-beta.solana.com");
            assert_eq!(mb, mn);
        }

        #[test]
        fn rpc_url_override_wins_over_cluster() {
            let url = "https://operator.private/solana-rpc";
            let resolved =
                resolve_solana_rpc_url(Some("mainnet"), Some(url)).expect("override wins");
            assert_eq!(resolved, url);
        }

        #[test]
        fn default_cluster_is_devnet() {
            assert_eq!(
                resolve_solana_rpc_url(None, None).unwrap(),
                "https://api.devnet.solana.com"
            );
        }

        #[test]
        fn unknown_cluster_errors_with_offending_name() {
            // A typo like "devnest" must not silently fall through to a
            // default URL — the operator could otherwise sign against
            // the wrong cluster.
            let err = resolve_solana_rpc_url(Some("devnest"), None).expect_err("must error");
            match err {
                ClusterResolveError::UnknownCluster { name } => assert_eq!(name, "devnest"),
                other => panic!("expected UnknownCluster, got {other:?}"),
            }
        }

        #[test]
        fn empty_override_is_rejected() {
            // Some(\"\") would route to a low-level connection error
            // downstream; reject it early with a clear cause.
            let err = resolve_solana_rpc_url(Some("devnet"), Some("")).expect_err("must error");
            assert!(matches!(err, ClusterResolveError::EmptyRpcUrl));
        }
    }

    mod settlement_pda {
        use super::super::{
            settlement_agent_pda, settlement_config_pda, settlement_credits_pda,
            settlement_stake_position_pda,
        };
        use solana_sdk::pubkey::Pubkey;

        fn fixed_program() -> Pubkey {
            // The devnet settlement program ID pinned in
            // docs/internal/status.md row "On-chain settlement".
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_agent_key() -> Pubkey {
            Pubkey::new_from_array([7u8; 32])
        }

        fn fixed_owner() -> Pubkey {
            Pubkey::new_from_array([11u8; 32])
        }

        #[test]
        fn config_pda_is_deterministic_for_fixed_program() {
            let p = fixed_program();
            assert_eq!(settlement_config_pda(&p), settlement_config_pda(&p));
        }

        #[test]
        fn config_pda_depends_on_program_id() {
            let a = settlement_config_pda(&fixed_program()).0;
            let b = settlement_config_pda(&Pubkey::new_from_array([1u8; 32])).0;
            assert_ne!(a, b, "config PDA must vary with program_id");
        }

        #[test]
        fn config_pda_matches_literal_seed_bytes() {
            // The literal seed bytes are spelled out a second time in
            // the test body so any drift between the helper's seeds and
            // the on-chain program's seeds = [b"config"] is caught by
            // this comparison instead of silently routing to the wrong
            // address.
            let p = fixed_program();
            let expected = Pubkey::find_program_address(&[b"config"], &p);
            assert_eq!(settlement_config_pda(&p), expected);
        }

        #[test]
        fn agent_pda_depends_on_agent_key() {
            let p = fixed_program();
            let a = settlement_agent_pda(&p, &fixed_agent_key()).0;
            let b = settlement_agent_pda(&p, &Pubkey::new_from_array([8u8; 32])).0;
            assert_ne!(a, b, "agent PDA must vary with agent_key");
        }

        #[test]
        fn agent_pda_matches_literal_seed_bytes() {
            let p = fixed_program();
            let key = fixed_agent_key();
            let expected = Pubkey::find_program_address(&[b"agent", key.as_ref()], &p);
            assert_eq!(settlement_agent_pda(&p, &key), expected);
        }

        #[test]
        fn credits_pda_depends_on_owner() {
            let p = fixed_program();
            let a = settlement_credits_pda(&p, &fixed_owner()).0;
            let b = settlement_credits_pda(&p, &Pubkey::new_from_array([12u8; 32])).0;
            assert_ne!(a, b, "credits PDA must vary with owner");
        }

        #[test]
        fn credits_pda_matches_literal_seed_bytes() {
            let p = fixed_program();
            let owner = fixed_owner();
            let expected = Pubkey::find_program_address(&[b"credits", owner.as_ref()], &p);
            assert_eq!(settlement_credits_pda(&p, &owner), expected);
        }

        #[test]
        fn stake_position_pda_depends_on_agent_key() {
            let p = fixed_program();
            let owner = fixed_owner();
            let a = settlement_stake_position_pda(&p, &fixed_agent_key(), &owner).0;
            let b =
                settlement_stake_position_pda(&p, &Pubkey::new_from_array([22u8; 32]), &owner).0;
            assert_ne!(a, b, "stake-position PDA must vary with agent_key");
        }

        #[test]
        fn stake_position_pda_depends_on_owner() {
            let p = fixed_program();
            let agent_key = fixed_agent_key();
            let a = settlement_stake_position_pda(&p, &agent_key, &fixed_owner()).0;
            let b =
                settlement_stake_position_pda(&p, &agent_key, &Pubkey::new_from_array([33u8; 32]))
                    .0;
            assert_ne!(a, b, "stake-position PDA must vary with owner");
        }

        #[test]
        fn stake_position_pda_matches_literal_seed_bytes() {
            // The literal seed list is spelled out again here so a
            // drift between the helper's seeds and the on-chain
            // program's seeds = [b"stake", agent.agent_key.as_ref(),
            // owner.key().as_ref()] surfaces locally instead of as a
            // ConstraintSeeds error from the cluster.
            let p = fixed_program();
            let agent_key = fixed_agent_key();
            let owner = fixed_owner();
            let expected =
                Pubkey::find_program_address(&[b"stake", agent_key.as_ref(), owner.as_ref()], &p);
            assert_eq!(
                settlement_stake_position_pda(&p, &agent_key, &owner),
                expected
            );
        }

        #[test]
        fn stake_position_pda_seeds_are_ordered() {
            // Swapping agent_key with owner in the seeds would
            // produce a different PDA even though both are 32-byte
            // pubkeys. Pin the ordering so a future refactor that
            // accidentally swaps the two arguments fails loudly.
            let p = fixed_program();
            let agent_key = fixed_agent_key();
            let owner = fixed_owner();
            let canonical = settlement_stake_position_pda(&p, &agent_key, &owner).0;
            let swapped = settlement_stake_position_pda(&p, &owner, &agent_key).0;
            assert_ne!(
                canonical, swapped,
                "stake-position PDA must be order-sensitive in its (agent_key, owner) seeds"
            );
        }
    }

    mod stake_args {
        use super::super::{serialize_stake_args, StakeArgs};
        use borsh::BorshDeserialize;

        fn fixture() -> StakeArgs {
            StakeArgs {
                amount: 0x1122_3344_5566_7788,
                lock_until: 0xaabb_ccdd_eeff_0011,
            }
        }

        #[test]
        fn encoded_bytes_match_struct_declaration_order() {
            // The on-chain Stake instruction declares (amount,
            // lock_until). Borsh serializes in struct-declaration
            // order, little-endian. A swap of the two fields would
            // produce the same total length and pass borsh
            // round-trip, so we pin the exact byte layout inline.
            let mut expected = Vec::with_capacity(16);
            expected.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
            expected.extend_from_slice(&0xaabb_ccdd_eeff_0011u64.to_le_bytes());
            assert_eq!(serialize_stake_args(&fixture()), expected);
        }

        #[test]
        fn encoded_length_is_two_u64s() {
            // Exactly 16 bytes (2 * 8). A regression that emitted
            // u32 instead of u64 (8 bytes total) or padded to
            // alignment (>16) would change this.
            assert_eq!(serialize_stake_args(&fixture()).len(), 16);
        }

        #[test]
        fn round_trip_via_borsh_returns_same_struct() {
            // The on-chain Anchor dispatcher decodes the args via
            // the same borsh wire format we emit here. A round-trip
            // with BorshDeserialize confirms our bytes parse back
            // identically — if a field were skipped or coerced to
            // the wrong width, the round-trip would fail or differ.
            let original = fixture();
            let bytes = serialize_stake_args(&original);
            let decoded = StakeArgs::try_from_slice(&bytes).expect("decodes");
            assert_eq!(decoded, original);
        }

        #[test]
        fn each_field_contributes_to_the_encoded_output() {
            // Mutating each field must change the encoded output;
            // otherwise borsh is silently dropping the field or
            // collapsing it with a sibling.
            let base = serialize_stake_args(&fixture());

            let mut a = fixture();
            a.amount = a.amount.wrapping_add(1);
            assert_ne!(serialize_stake_args(&a), base);

            let mut l = fixture();
            l.lock_until = l.lock_until.wrapping_add(1);
            assert_ne!(serialize_stake_args(&l), base);
        }
    }

    mod buy_credits_args {
        use super::super::{serialize_buy_credits_args, BuyCreditsArgs};
        use borsh::BorshDeserialize;

        fn fixture() -> BuyCreditsArgs {
            BuyCreditsArgs {
                amount_covnt: 0xdead_beef_cafe_babe,
            }
        }

        #[test]
        fn encoded_bytes_match_little_endian_u64() {
            // The on-chain buy_credits handler reads a single
            // u64 in little-endian. A regression that emitted
            // big-endian would still produce 8 bytes, so the
            // length-only test would pass; pin the byte layout
            // explicitly here.
            let expected = 0xdead_beef_cafe_babeu64.to_le_bytes();
            assert_eq!(serialize_buy_credits_args(&fixture()), expected);
        }

        #[test]
        fn encoded_length_is_one_u64() {
            // Exactly 8 bytes. A regression that grew the
            // struct without updating the on-chain mirror would
            // change this.
            assert_eq!(serialize_buy_credits_args(&fixture()).len(), 8);
        }

        #[test]
        fn round_trip_via_borsh_returns_same_struct() {
            let original = fixture();
            let bytes = serialize_buy_credits_args(&original);
            let decoded = BuyCreditsArgs::try_from_slice(&bytes).expect("decodes");
            assert_eq!(decoded, original);
        }

        #[test]
        fn different_amounts_produce_different_byte_streams() {
            let a = serialize_buy_credits_args(&BuyCreditsArgs { amount_covnt: 1 });
            let b = serialize_buy_credits_args(&BuyCreditsArgs { amount_covnt: 2 });
            assert_ne!(a, b, "amount_covnt must influence the encoded bytes");
        }
    }

    mod keypair_mode {
        use super::super::{check_keypair_mode, KeypairModeError};
        use std::io::Write;
        use tempfile::tempdir;

        #[cfg(unix)]
        fn set_mode(path: &std::path::Path, mode: u32) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .expect("set mode");
        }

        #[cfg(unix)]
        #[test]
        fn rejects_world_readable_mode_with_chmod_hint() {
            // Mode 0644 leaves the secret material readable by any
            // local user. The check must bail with a message that
            // names the file and points the operator at the chmod
            // remediation; otherwise the operator could miss the
            // exposure window.
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("id.json");
            let mut f = std::fs::File::create(&path).expect("create fixture");
            f.write_all(b"placeholder").expect("write");
            set_mode(&path, 0o644);
            let err = check_keypair_mode(&path).expect_err("must error");
            match &err {
                KeypairModeError::GroupOrWorldReadable { mode, .. } => {
                    assert_eq!(*mode, 0o644)
                }
                other => panic!("expected GroupOrWorldReadable, got {other:?}"),
            }
            let msg = err.to_string();
            assert!(msg.contains("chmod 0600"), "message hints at chmod: {msg}");
            assert!(
                msg.contains(path.to_string_lossy().as_ref()),
                "message names the offending path: {msg}",
            );
        }

        #[cfg(unix)]
        #[test]
        fn rejects_group_readable_mode() {
            // 0640 grants the group read access — still enough for
            // a co-tenant in the same posix group to scrape the
            // operator's signing key.
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("id.json");
            let mut f = std::fs::File::create(&path).expect("create fixture");
            f.write_all(b"placeholder").expect("write");
            set_mode(&path, 0o640);
            let err = check_keypair_mode(&path).expect_err("must error");
            assert!(
                matches!(
                    err,
                    KeypairModeError::GroupOrWorldReadable { mode: 0o640, .. }
                ),
                "expected GroupOrWorldReadable(0o640), got {err:?}"
            );
        }

        #[cfg(unix)]
        #[test]
        fn accepts_owner_only_mode() {
            // 0600 (and 0400) leave only the file owner with read
            // access; the check must pass without raising.
            for mode in [0o600u32, 0o400u32] {
                let dir = tempdir().expect("tempdir");
                let path = dir.path().join("id.json");
                let mut f = std::fs::File::create(&path).expect("create fixture");
                f.write_all(b"placeholder").expect("write");
                set_mode(&path, mode);
                check_keypair_mode(&path)
                    .unwrap_or_else(|e| panic!("mode {mode:o} must be accepted: {e}"));
            }
        }

        #[cfg(unix)]
        #[test]
        fn missing_file_returns_stat_variant() {
            // A path that does not exist must surface Stat, not the
            // generic GroupOrWorldReadable variant, so the operator
            // can tell "the key file is missing" from "the key file
            // is exposed".
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("absent.json");
            let err = check_keypair_mode(&path).expect_err("must error");
            assert!(
                matches!(err, KeypairModeError::Stat { .. }),
                "expected Stat, got {err:?}"
            );
        }
    }

    mod register_agent_arg_parsing {
        use super::super::{
            parse_hash32_arg, parse_pubkey_arg, parse_register_agent_cli_args, Hash32ArgError,
            PubkeyArgError,
        };
        use solana_sdk::pubkey::Pubkey;

        fn valid_pubkey_b58() -> String {
            // 32-byte all-1s array encodes to a known-valid 32-byte
            // base58 Pubkey; using a fixed value keeps the test
            // hermetic across architectures.
            Pubkey::new_from_array([1u8; 32]).to_string()
        }

        fn hex_32(b: u8) -> String {
            (0..32).map(|_| format!("{b:02x}")).collect::<String>()
        }

        #[test]
        fn rejects_short_agent_key_with_named_flag() {
            // A 31-byte base58 input would silently parse if we
            // accepted any byte length; the helper must surface the
            // flag name so the operator knows which value is wrong.
            let short_b58 = bs58::encode([1u8; 31]).into_string();
            let err = parse_pubkey_arg("agent-key", &short_b58).expect_err("must error");
            match err {
                PubkeyArgError::Invalid { flag, value, .. } => {
                    assert_eq!(flag, "agent-key");
                    assert_eq!(value, short_b58);
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }

        #[test]
        fn rejects_empty_pubkey() {
            let err = parse_pubkey_arg("program-id", "").expect_err("must error");
            assert!(matches!(err, PubkeyArgError::Empty { flag: "program-id" }));
        }

        #[test]
        fn parses_canonical_pubkey_round_trip() {
            let v = valid_pubkey_b58();
            let parsed = parse_pubkey_arg("agent-key", &v).expect("parses");
            assert_eq!(parsed.to_string(), v);
        }

        #[test]
        fn rejects_hex_hash_with_wrong_length() {
            // 63-char input must not silently truncate to 31 bytes.
            // The helper surfaces the actual char count so the
            // operator can fix the off-by-one.
            let too_short = "a".repeat(63);
            let err = parse_hash32_arg("metadata-hash", &too_short).expect_err("must error");
            match err {
                Hash32ArgError::WrongLength { flag, actual } => {
                    assert_eq!(flag, "metadata-hash");
                    assert_eq!(actual, 63);
                }
                other => panic!("expected WrongLength, got {other:?}"),
            }
        }

        #[test]
        fn rejects_hex_hash_with_non_hex_character() {
            let mut v = hex_32(0xab);
            v.replace_range(10..11, "g");
            let err = parse_hash32_arg("capability-hash", &v).expect_err("must error");
            match err {
                Hash32ArgError::BadHexChar {
                    flag, position, ch, ..
                } => {
                    assert_eq!(flag, "capability-hash");
                    assert_eq!(position, 10);
                    assert_eq!(ch, 'g');
                }
                other => panic!("expected BadHexChar, got {other:?}"),
            }
        }

        #[test]
        fn parses_canonical_hash_round_trip() {
            let bytes = parse_hash32_arg("metadata-hash", &hex_32(0x5a)).expect("parses");
            assert_eq!(bytes, [0x5a; 32]);
        }

        #[test]
        fn parses_full_cli_with_defaults() {
            let pubkey = valid_pubkey_b58();
            let meta = hex_32(0xab);
            let cap = hex_32(0xcd);
            let argv: Vec<String> = vec![
                "--program-id".into(),
                pubkey.clone(),
                "--agent-key".into(),
                pubkey.clone(),
                "--metadata-hash".into(),
                meta.clone(),
                "--capability-hash".into(),
                cap.clone(),
            ];
            let parsed = parse_register_agent_cli_args(&argv).expect("parses");
            assert_eq!(parsed.cluster, "devnet", "default cluster is devnet");
            assert_eq!(
                parsed.confirm_timeout_ms, 60_000,
                "default confirm-timeout-ms is 60000"
            );
            assert!(!parsed.as_json);
            assert!(parsed.rpc_url.is_none());
            assert!(parsed.keypair_path.is_none());
            assert_eq!(parsed.metadata_hash, [0xab; 32]);
            assert_eq!(parsed.capability_hash, [0xcd; 32]);
        }

        #[test]
        fn missing_required_program_id_errors() {
            let argv: Vec<String> = vec![
                "--agent-key".into(),
                valid_pubkey_b58(),
                "--metadata-hash".into(),
                hex_32(1),
                "--capability-hash".into(),
                hex_32(2),
            ];
            let err = parse_register_agent_cli_args(&argv).expect_err("must error");
            assert!(
                err.to_string().contains("--program-id"),
                "error names the missing flag: {err}"
            );
        }

        #[test]
        fn rejects_zero_confirm_timeout() {
            let pubkey = valid_pubkey_b58();
            let argv: Vec<String> = vec![
                "--program-id".into(),
                pubkey.clone(),
                "--agent-key".into(),
                pubkey,
                "--metadata-hash".into(),
                hex_32(1),
                "--capability-hash".into(),
                hex_32(2),
                "--confirm-timeout-ms".into(),
                "0".into(),
            ];
            let err = parse_register_agent_cli_args(&argv).expect_err("must error");
            assert!(
                err.to_string().contains("greater than zero"),
                "rejects 0 with named reason: {err}"
            );
        }

        #[test]
        fn rejects_unknown_flag() {
            let err = parse_register_agent_cli_args(&["--unknown".into()]).expect_err("must error");
            assert!(
                err.to_string().contains("--unknown"),
                "names the unknown flag: {err}"
            );
        }
    }

    mod register_agent_tx_shape {
        use super::super::{
            build_register_agent_instruction, sign_register_agent_tx, RegisterAgentArgs,
        };
        use solana_sdk::hash::Hash;
        use solana_sdk::pubkey::Pubkey;
        use solana_sdk::signer::keypair::Keypair;
        use solana_sdk::signer::Signer;

        fn fixed_program() -> Pubkey {
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_args() -> RegisterAgentArgs {
            RegisterAgentArgs {
                agent_key: [42u8; 32],
                metadata_hash: [43u8; 32],
                capability_hash: [44u8; 32],
            }
        }

        #[test]
        fn fee_payer_is_operator_pubkey() {
            // Anchor's dispatcher and the cluster's signature check
            // both read message.account_keys[0] as the fee payer.
            // A regression that derived the fee payer from a
            // different account would land the tx in
            // SignatureFailure at the cluster, which only surfaces
            // at submission. Pinning account_keys[0] here makes the
            // regression a local-test failure.
            let kp = Keypair::new();
            let tx = sign_register_agent_tx(&kp, &fixed_program(), &fixed_args(), Hash::default());
            assert_eq!(tx.message.account_keys[0], kp.pubkey());
        }

        #[test]
        fn single_signer_only_the_operator() {
            // The on-chain RegisterAgent struct expects exactly one
            // signer (the operator). A tx built with extra signing
            // keypairs would be rejected with SignerCountMismatch.
            let kp = Keypair::new();
            let tx = sign_register_agent_tx(&kp, &fixed_program(), &fixed_args(), Hash::default());
            assert_eq!(tx.signatures.len(), 1, "exactly one signature");
            assert_eq!(
                tx.message.header.num_required_signatures, 1,
                "exactly one required signature"
            );
        }

        #[test]
        fn instruction_matches_build_register_agent_instruction_output() {
            // The encoded instruction in the tx must equal the
            // standalone build_register_agent_instruction output for
            // the same inputs; otherwise a refactor that inlined the
            // builder could drift in shape without the standalone
            // unit tests catching it.
            let kp = Keypair::new();
            let program = fixed_program();
            let args = fixed_args();
            let expected_ix = build_register_agent_instruction(&program, &kp.pubkey(), &args);
            let tx = sign_register_agent_tx(&kp, &program, &args, Hash::default());

            assert_eq!(tx.message.instructions.len(), 1, "exactly one instruction");
            let actual_ix = &tx.message.instructions[0];
            assert_eq!(actual_ix.data, expected_ix.data, "instruction data matches");
            // The compiled message references each account-meta from the
            // instruction by index into message.account_keys; verify every
            // account from the builder is present in the compiled
            // message. The program_id is also added to account_keys by
            // Message::new, so account_keys.len() ≥ accounts.len() + 1
            // — count equality is intentionally not asserted.
            for meta in &expected_ix.accounts {
                assert!(
                    tx.message.account_keys.contains(&meta.pubkey),
                    "tx account_keys must contain {} from the instruction",
                    meta.pubkey
                );
            }
            assert!(
                tx.message.account_keys.contains(&program),
                "tx account_keys must contain the program id"
            );
        }
    }

    mod register_agent_json_envelopes {
        use super::super::{register_agent_confirmed_json, register_agent_timeout_json};

        #[test]
        fn confirmed_envelope_pins_documented_shape() {
            // Operators and downstream tooling consume this envelope
            // by key. Pinning the shape inline makes a renamed key
            // (e.g. "tx" → "transaction") fail loudly during local
            // tests instead of breaking downstream parsers silently.
            let v = register_agent_confirmed_json(
                "sig123",
                "https://api.devnet.solana.com",
                "devnet",
                "agentB58",
            );
            assert_eq!(v["kind"], "covenant.chain.tx.v1");
            assert_eq!(v["verb"], "register-agent");
            assert_eq!(v["signature"], "sig123");
            assert_eq!(v["rpc_url"], "https://api.devnet.solana.com");
            assert_eq!(v["cluster"], "devnet");
            assert_eq!(v["agent_key"], "agentB58");
            assert_eq!(v["status"], "confirmed");
        }

        #[test]
        fn timeout_envelope_uses_distinct_kind_and_status() {
            // Distinct kind + status let monitors disambiguate a
            // confirmed transaction from a submitted-but-not-yet-
            // confirmed one without parsing free-form text.
            let v = register_agent_timeout_json(
                "sig999",
                "http://127.0.0.1:8899",
                "localnet",
                "agentB58",
                30_000,
            );
            assert_eq!(v["kind"], "covenant.chain.tx.timeout.v1");
            assert_eq!(v["status"], "submitted-not-confirmed");
            assert_eq!(v["signature"], "sig999");
            assert_eq!(v["timeout_ms"], 30_000);
        }

        #[test]
        fn confirmed_envelope_pins_top_level_schema() {
            const EXPECTED_KEYS: &[&str] = &[
                "agent_key",
                "cluster",
                "kind",
                "rpc_url",
                "signature",
                "status",
                "verb",
            ];

            let value = register_agent_confirmed_json(
                "sig123",
                "https://api.devnet.solana.com",
                "devnet",
                "agentB58",
            );
            let object = value
                .as_object()
                .expect("register_agent_confirmed_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "register_agent_confirmed_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert_eq!(value["kind"].as_str(), Some("covenant.chain.tx.v1"));
            assert_eq!(value["verb"].as_str(), Some("register-agent"));
            assert_eq!(value["status"].as_str(), Some("confirmed"));
            assert!(
                value["signature"].is_string(),
                "signature must be a string: {value}"
            );
            assert!(
                value["rpc_url"].is_string(),
                "rpc_url must be a string: {value}"
            );
            assert!(
                value["cluster"].is_string(),
                "cluster must be a string: {value}"
            );
            assert!(
                value["agent_key"].is_string(),
                "agent_key must be a string: {value}"
            );
        }

        #[test]
        fn timeout_envelope_pins_top_level_schema() {
            const EXPECTED_KEYS: &[&str] = &[
                "agent_key",
                "cluster",
                "kind",
                "rpc_url",
                "signature",
                "status",
                "timeout_ms",
                "verb",
            ];

            let value = register_agent_timeout_json(
                "sig999",
                "http://127.0.0.1:8899",
                "localnet",
                "agentB58",
                30_000,
            );
            let object = value
                .as_object()
                .expect("register_agent_timeout_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "register_agent_timeout_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert_eq!(value["kind"].as_str(), Some("covenant.chain.tx.timeout.v1"));
            assert_eq!(value["verb"].as_str(), Some("register-agent"));
            assert_eq!(value["status"].as_str(), Some("submitted-not-confirmed"));
            assert!(
                value["signature"].is_string(),
                "signature must be a string: {value}"
            );
            assert!(
                value["rpc_url"].is_string(),
                "rpc_url must be a string: {value}"
            );
            assert!(
                value["cluster"].is_string(),
                "cluster must be a string: {value}"
            );
            assert!(
                value["agent_key"].is_string(),
                "agent_key must be a string: {value}"
            );
            assert!(
                value["timeout_ms"].is_u64(),
                "timeout_ms must serialize as u64, not string: {value}"
            );
        }
    }

    mod stake_arg_parsing {
        use super::super::{parse_stake_cli_args, parse_u64_arg};
        use solana_sdk::pubkey::Pubkey;

        fn valid_pubkey_b58() -> String {
            Pubkey::new_from_array([1u8; 32]).to_string()
        }

        fn minimal_argv() -> Vec<String> {
            let pk = valid_pubkey_b58();
            vec![
                "--program-id".into(),
                pk.clone(),
                "--agent-key".into(),
                pk.clone(),
                "--owner-covnt".into(),
                pk.clone(),
                "--stake-vault".into(),
                pk.clone(),
                "--covnt-mint".into(),
                pk,
                "--amount".into(),
                "1000".into(),
                "--lock-until".into(),
                "1700000000".into(),
            ]
        }

        #[test]
        fn parses_full_cli_with_defaults() {
            let parsed = parse_stake_cli_args(&minimal_argv()).expect("parses");
            assert_eq!(parsed.cluster, "devnet", "default cluster is devnet");
            assert_eq!(parsed.confirm_timeout_ms, 60_000);
            assert!(!parsed.as_json);
            assert!(parsed.rpc_url.is_none());
            assert!(parsed.keypair_path.is_none());
            assert_eq!(parsed.amount, 1000);
            assert_eq!(parsed.lock_until, 1_700_000_000);
        }

        #[test]
        fn rejects_zero_amount_with_named_reason() {
            // Zero amount opens a 0-balance StakePosition the
            // operator paid rent for and still costs a tx fee;
            // a typo for amount=1000 should not silently submit.
            let mut argv = minimal_argv();
            for (i, a) in argv.iter().enumerate() {
                if a == "1000" {
                    argv[i] = "0".into();
                    break;
                }
            }
            let err = parse_stake_cli_args(&argv).expect_err("must error");
            assert!(
                err.to_string().contains("greater than zero"),
                "names the reason: {err}"
            );
        }

        #[test]
        fn rejects_non_integer_amount_with_named_flag() {
            // A typo like --amount 1_000 would silently parse to
            // 1 with a stray underscore — u64::from_str rejects
            // it. The error must name --amount so the operator
            // knows which flag to fix.
            let mut argv = minimal_argv();
            for (i, a) in argv.iter().enumerate() {
                if a == "1000" {
                    argv[i] = "1_000".into();
                    break;
                }
            }
            let err = parse_stake_cli_args(&argv).expect_err("must error");
            assert!(err.to_string().contains("--amount"), "names flag: {err}");
        }

        #[test]
        fn rejects_negative_amount() {
            // A bare "-1" would fail u64::from_str; the error
            // surfaces the offending value so the operator can
            // see the typo without guessing.
            let mut argv = minimal_argv();
            for (i, a) in argv.iter().enumerate() {
                if a == "1000" {
                    argv[i] = "-1".into();
                    break;
                }
            }
            let err = parse_stake_cli_args(&argv).expect_err("must error");
            assert!(err.to_string().contains("--amount"), "names flag: {err}");
        }

        #[test]
        fn missing_each_required_flag_errors_with_its_name() {
            // Drop each required flag one at a time and confirm
            // the error names that flag. Iterating across all six
            // required flags prevents a future refactor from
            // silently making any one of them optional.
            let required = [
                "--program-id",
                "--agent-key",
                "--owner-covnt",
                "--stake-vault",
                "--covnt-mint",
                "--amount",
                "--lock-until",
            ];
            for flag in required {
                let base = minimal_argv();
                let mut filtered: Vec<String> = Vec::new();
                let mut i = 0;
                while i < base.len() {
                    if base[i] == flag {
                        i += 2;
                        continue;
                    }
                    filtered.push(base[i].clone());
                    i += 1;
                }
                let err = parse_stake_cli_args(&filtered)
                    .err()
                    .unwrap_or_else(|| panic!("expected error when {flag} is missing"));
                assert!(
                    err.to_string().contains(flag),
                    "error must name the missing flag {flag}: {err}"
                );
            }
        }

        #[test]
        fn parse_u64_arg_round_trips_max_value() {
            assert_eq!(
                parse_u64_arg("amount", &u64::MAX.to_string()).unwrap(),
                u64::MAX
            );
        }

        #[test]
        fn parse_u64_arg_rejects_empty_with_flag_name() {
            let err = parse_u64_arg("amount", "").expect_err("must error");
            assert!(err.to_string().contains("--amount"), "names flag: {err}");
        }
    }

    mod stake_tx_shape {
        use super::super::{build_stake_instruction, sign_stake_tx, StakeArgs};
        use solana_sdk::hash::Hash;
        use solana_sdk::pubkey::Pubkey;
        use solana_sdk::signer::keypair::Keypair;
        use solana_sdk::signer::Signer;

        fn fixed_program() -> Pubkey {
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_args() -> StakeArgs {
            StakeArgs {
                amount: 2_500_000,
                lock_until: 1_800_000_000,
            }
        }

        #[test]
        fn fee_payer_is_operator_pubkey() {
            // Same fee-payer invariant as register-agent. The
            // Stake instruction's `owner` Signer also doubles as
            // the fee payer at message.account_keys[0].
            let kp = Keypair::new();
            let agent_key = Pubkey::new_from_array([7u8; 32]);
            let owner_covnt = Pubkey::new_from_array([13u8; 32]);
            let stake_vault = Pubkey::new_from_array([17u8; 32]);
            let tx = sign_stake_tx(
                &kp,
                &fixed_program(),
                &agent_key,
                &owner_covnt,
                &stake_vault,
                &Pubkey::new_from_array([21u8; 32]),
                &fixed_args(),
                Hash::default(),
            );
            assert_eq!(tx.message.account_keys[0], kp.pubkey());
        }

        #[test]
        fn single_signer_only_the_operator() {
            // The on-chain Stake struct has exactly one Signer
            // field (owner). A tx with more or fewer signers
            // would be rejected.
            let kp = Keypair::new();
            let agent_key = Pubkey::new_from_array([7u8; 32]);
            let owner_covnt = Pubkey::new_from_array([13u8; 32]);
            let stake_vault = Pubkey::new_from_array([17u8; 32]);
            let tx = sign_stake_tx(
                &kp,
                &fixed_program(),
                &agent_key,
                &owner_covnt,
                &stake_vault,
                &Pubkey::new_from_array([21u8; 32]),
                &fixed_args(),
                Hash::default(),
            );
            assert_eq!(tx.signatures.len(), 1);
            assert_eq!(tx.message.header.num_required_signatures, 1);
        }

        #[test]
        fn instruction_matches_build_stake_instruction_output() {
            // The encoded instruction in the tx must equal the
            // standalone build_stake_instruction output for the
            // same inputs; a future refactor that inlined the
            // builder could drift in shape without the standalone
            // unit tests catching it.
            let kp = Keypair::new();
            let program = fixed_program();
            let agent_key = Pubkey::new_from_array([7u8; 32]);
            let owner_covnt = Pubkey::new_from_array([13u8; 32]);
            let stake_vault = Pubkey::new_from_array([17u8; 32]);
            let args = fixed_args();
            let expected_ix = build_stake_instruction(
                &program,
                &kp.pubkey(),
                &agent_key,
                &owner_covnt,
                &stake_vault,
                &Pubkey::new_from_array([21u8; 32]),
                &args,
            );
            let tx = sign_stake_tx(
                &kp,
                &program,
                &agent_key,
                &owner_covnt,
                &stake_vault,
                &Pubkey::new_from_array([21u8; 32]),
                &args,
                Hash::default(),
            );
            assert_eq!(tx.message.instructions.len(), 1);
            assert_eq!(tx.message.instructions[0].data, expected_ix.data);
            for meta in &expected_ix.accounts {
                assert!(
                    tx.message.account_keys.contains(&meta.pubkey),
                    "tx account_keys must contain {} from the instruction",
                    meta.pubkey
                );
            }
            assert!(
                tx.message.account_keys.contains(&program),
                "tx account_keys must contain the program id"
            );
        }
    }

    mod stake_json_envelopes {
        use super::super::{stake_confirmed_json, stake_timeout_json};

        #[test]
        fn confirmed_envelope_pins_documented_shape() {
            let v = stake_confirmed_json(
                "sigStake",
                "http://127.0.0.1:8899",
                "localnet",
                "agentB58",
                12_345,
                1_700_000_000,
            );
            assert_eq!(v["kind"], "covenant.chain.tx.v1");
            assert_eq!(v["verb"], "stake");
            assert_eq!(v["status"], "confirmed");
            assert_eq!(v["amount"], 12_345);
            assert_eq!(v["lock_until"], 1_700_000_000);
            assert_eq!(v["agent_key"], "agentB58");
            assert_eq!(v["cluster"], "localnet");
        }

        #[test]
        fn timeout_envelope_includes_amount_lock_until_and_timeout_ms() {
            let v = stake_timeout_json(
                "sigStake",
                "http://127.0.0.1:8899",
                "localnet",
                "agentB58",
                12_345,
                1_700_000_000,
                45_000,
            );
            assert_eq!(v["kind"], "covenant.chain.tx.timeout.v1");
            assert_eq!(v["verb"], "stake");
            assert_eq!(v["status"], "submitted-not-confirmed");
            assert_eq!(v["amount"], 12_345);
            assert_eq!(v["lock_until"], 1_700_000_000);
            assert_eq!(v["timeout_ms"], 45_000);
        }

        #[test]
        fn confirmed_envelope_pins_top_level_schema() {
            const EXPECTED_KEYS: &[&str] = &[
                "agent_key",
                "amount",
                "cluster",
                "kind",
                "lock_until",
                "rpc_url",
                "signature",
                "status",
                "verb",
            ];

            let value = stake_confirmed_json(
                "sigStake",
                "http://127.0.0.1:8899",
                "localnet",
                "agentB58",
                12_345,
                1_700_000_000,
            );
            let object = value
                .as_object()
                .expect("stake_confirmed_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "stake_confirmed_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert_eq!(value["kind"].as_str(), Some("covenant.chain.tx.v1"));
            assert_eq!(value["verb"].as_str(), Some("stake"));
            assert_eq!(value["status"].as_str(), Some("confirmed"));
            assert!(
                value["signature"].is_string(),
                "signature must be a string: {value}"
            );
            assert!(
                value["rpc_url"].is_string(),
                "rpc_url must be a string: {value}"
            );
            assert!(
                value["cluster"].is_string(),
                "cluster must be a string: {value}"
            );
            assert!(
                value["agent_key"].is_string(),
                "agent_key must be a string: {value}"
            );
            assert!(
                value["amount"].is_u64(),
                "amount must serialize as u64, not string: {value}"
            );
            assert!(
                value["lock_until"].is_u64(),
                "lock_until must serialize as u64, not string: {value}"
            );
        }

        #[test]
        fn timeout_envelope_pins_top_level_schema() {
            const EXPECTED_KEYS: &[&str] = &[
                "agent_key",
                "amount",
                "cluster",
                "kind",
                "lock_until",
                "rpc_url",
                "signature",
                "status",
                "timeout_ms",
                "verb",
            ];

            let value = stake_timeout_json(
                "sigStake",
                "http://127.0.0.1:8899",
                "localnet",
                "agentB58",
                12_345,
                1_700_000_000,
                45_000,
            );
            let object = value
                .as_object()
                .expect("stake_timeout_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "stake_timeout_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert_eq!(value["kind"].as_str(), Some("covenant.chain.tx.timeout.v1"));
            assert_eq!(value["verb"].as_str(), Some("stake"));
            assert_eq!(value["status"].as_str(), Some("submitted-not-confirmed"));
            assert!(
                value["signature"].is_string(),
                "signature must be a string: {value}"
            );
            assert!(
                value["rpc_url"].is_string(),
                "rpc_url must be a string: {value}"
            );
            assert!(
                value["cluster"].is_string(),
                "cluster must be a string: {value}"
            );
            assert!(
                value["agent_key"].is_string(),
                "agent_key must be a string: {value}"
            );
            assert!(
                value["amount"].is_u64(),
                "amount must serialize as u64, not string: {value}"
            );
            assert!(
                value["lock_until"].is_u64(),
                "lock_until must serialize as u64, not string: {value}"
            );
            assert!(
                value["timeout_ms"].is_u64(),
                "timeout_ms must serialize as u64, not string: {value}"
            );
        }
    }

    mod buy_credits_arg_parsing {
        use super::super::parse_buy_credits_cli_args;
        use solana_sdk::pubkey::Pubkey;

        fn valid_pubkey_b58() -> String {
            Pubkey::new_from_array([1u8; 32]).to_string()
        }

        fn minimal_argv() -> Vec<String> {
            let pk = valid_pubkey_b58();
            vec![
                "--program-id".into(),
                pk.clone(),
                "--owner-covnt".into(),
                pk.clone(),
                "--treasury".into(),
                pk.clone(),
                "--covnt-mint".into(),
                pk,
                "--amount-covnt".into(),
                "5000".into(),
            ]
        }

        #[test]
        fn parses_full_cli_with_defaults() {
            let parsed = parse_buy_credits_cli_args(&minimal_argv()).expect("parses");
            assert_eq!(parsed.cluster, "devnet");
            assert_eq!(parsed.confirm_timeout_ms, 60_000);
            assert!(!parsed.as_json);
            assert!(parsed.rpc_url.is_none());
            assert!(parsed.keypair_path.is_none());
            assert_eq!(parsed.amount_covnt, 5000);
        }

        #[test]
        fn rejects_zero_amount_covnt_with_named_reason() {
            let mut argv = minimal_argv();
            for (i, a) in argv.iter().enumerate() {
                if a == "5000" {
                    argv[i] = "0".into();
                    break;
                }
            }
            let err = parse_buy_credits_cli_args(&argv).expect_err("must error");
            assert!(
                err.to_string().contains("greater than zero"),
                "names the reason: {err}"
            );
        }

        #[test]
        fn rejects_non_integer_amount_with_named_flag() {
            let mut argv = minimal_argv();
            for (i, a) in argv.iter().enumerate() {
                if a == "5000" {
                    argv[i] = "5_000".into();
                    break;
                }
            }
            let err = parse_buy_credits_cli_args(&argv).expect_err("must error");
            assert!(
                err.to_string().contains("--amount-covnt"),
                "names flag: {err}"
            );
        }

        #[test]
        fn missing_each_required_flag_errors_with_its_name() {
            let required = [
                "--program-id",
                "--owner-covnt",
                "--treasury",
                "--covnt-mint",
                "--amount-covnt",
            ];
            for flag in required {
                let base = minimal_argv();
                let mut filtered: Vec<String> = Vec::new();
                let mut i = 0;
                while i < base.len() {
                    if base[i] == flag {
                        i += 2;
                        continue;
                    }
                    filtered.push(base[i].clone());
                    i += 1;
                }
                let err = parse_buy_credits_cli_args(&filtered)
                    .err()
                    .unwrap_or_else(|| panic!("expected error when {flag} is missing"));
                assert!(
                    err.to_string().contains(flag),
                    "error must name the missing flag {flag}: {err}"
                );
            }
        }

        #[test]
        fn rejects_unknown_flag() {
            let err = parse_buy_credits_cli_args(&["--unknown".into()]).expect_err("must error");
            assert!(
                err.to_string().contains("--unknown"),
                "names the unknown flag: {err}"
            );
        }
    }

    mod buy_credits_tx_shape {
        use super::super::{build_buy_credits_instruction, sign_buy_credits_tx, BuyCreditsArgs};
        use solana_sdk::hash::Hash;
        use solana_sdk::pubkey::Pubkey;
        use solana_sdk::signer::keypair::Keypair;
        use solana_sdk::signer::Signer;

        fn fixed_program() -> Pubkey {
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
                .parse()
                .expect("settlement program id parses")
        }

        fn fixed_args() -> BuyCreditsArgs {
            BuyCreditsArgs {
                amount_covnt: 999_999,
            }
        }

        #[test]
        fn fee_payer_is_operator_pubkey() {
            let kp = Keypair::new();
            let owner_covnt = Pubkey::new_from_array([19u8; 32]);
            let treasury = Pubkey::new_from_array([23u8; 32]);
            let tx = sign_buy_credits_tx(
                &kp,
                &fixed_program(),
                &owner_covnt,
                &treasury,
                &Pubkey::new_from_array([22u8; 32]),
                &fixed_args(),
                Hash::default(),
            );
            assert_eq!(tx.message.account_keys[0], kp.pubkey());
        }

        #[test]
        fn single_signer_only_the_operator() {
            let kp = Keypair::new();
            let owner_covnt = Pubkey::new_from_array([19u8; 32]);
            let treasury = Pubkey::new_from_array([23u8; 32]);
            let tx = sign_buy_credits_tx(
                &kp,
                &fixed_program(),
                &owner_covnt,
                &treasury,
                &Pubkey::new_from_array([22u8; 32]),
                &fixed_args(),
                Hash::default(),
            );
            assert_eq!(tx.signatures.len(), 1);
            assert_eq!(tx.message.header.num_required_signatures, 1);
        }

        #[test]
        fn instruction_matches_build_buy_credits_instruction_output() {
            let kp = Keypair::new();
            let program = fixed_program();
            let owner_covnt = Pubkey::new_from_array([19u8; 32]);
            let treasury = Pubkey::new_from_array([23u8; 32]);
            let args = fixed_args();
            let expected_ix = build_buy_credits_instruction(
                &program,
                &kp.pubkey(),
                &owner_covnt,
                &treasury,
                &Pubkey::new_from_array([22u8; 32]),
                &args,
            );
            let tx = sign_buy_credits_tx(
                &kp,
                &program,
                &owner_covnt,
                &treasury,
                &Pubkey::new_from_array([22u8; 32]),
                &args,
                Hash::default(),
            );
            assert_eq!(tx.message.instructions.len(), 1);
            assert_eq!(tx.message.instructions[0].data, expected_ix.data);
            for meta in &expected_ix.accounts {
                assert!(
                    tx.message.account_keys.contains(&meta.pubkey),
                    "tx account_keys must contain {} from the instruction",
                    meta.pubkey
                );
            }
            assert!(
                tx.message.account_keys.contains(&program),
                "tx account_keys must contain the program id"
            );
        }
    }

    mod buy_credits_json_envelopes {
        use super::super::{buy_credits_confirmed_json, buy_credits_timeout_json};

        #[test]
        fn confirmed_envelope_pins_documented_shape() {
            let v = buy_credits_confirmed_json(
                "sigBuy",
                "http://127.0.0.1:8899",
                "localnet",
                "ownerB58",
                42_000,
            );
            assert_eq!(v["kind"], "covenant.chain.tx.v1");
            assert_eq!(v["verb"], "buy-credits");
            assert_eq!(v["status"], "confirmed");
            assert_eq!(v["amount_covnt"], 42_000);
            assert_eq!(v["owner"], "ownerB58");
            assert_eq!(v["cluster"], "localnet");
        }

        #[test]
        fn timeout_envelope_includes_amount_covnt_and_timeout_ms() {
            let v = buy_credits_timeout_json(
                "sigBuy",
                "http://127.0.0.1:8899",
                "localnet",
                "ownerB58",
                42_000,
                15_000,
            );
            assert_eq!(v["kind"], "covenant.chain.tx.timeout.v1");
            assert_eq!(v["verb"], "buy-credits");
            assert_eq!(v["status"], "submitted-not-confirmed");
            assert_eq!(v["amount_covnt"], 42_000);
            assert_eq!(v["timeout_ms"], 15_000);
        }

        #[test]
        fn confirmed_envelope_pins_top_level_schema() {
            const EXPECTED_KEYS: &[&str] = &[
                "amount_covnt",
                "cluster",
                "kind",
                "owner",
                "rpc_url",
                "signature",
                "status",
                "verb",
            ];

            let value = buy_credits_confirmed_json(
                "sigBuy",
                "http://127.0.0.1:8899",
                "localnet",
                "ownerB58",
                42_000,
            );
            let object = value
                .as_object()
                .expect("buy_credits_confirmed_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "buy_credits_confirmed_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert_eq!(value["kind"].as_str(), Some("covenant.chain.tx.v1"));
            assert_eq!(value["verb"].as_str(), Some("buy-credits"));
            assert_eq!(value["status"].as_str(), Some("confirmed"));
            assert!(
                value["signature"].is_string(),
                "signature must be a string: {value}"
            );
            assert!(
                value["rpc_url"].is_string(),
                "rpc_url must be a string: {value}"
            );
            assert!(
                value["cluster"].is_string(),
                "cluster must be a string: {value}"
            );
            assert!(
                value["owner"].is_string(),
                "owner must be a string: {value}"
            );
            assert!(
                value["amount_covnt"].is_u64(),
                "amount_covnt must serialize as u64, not string: {value}"
            );
        }

        #[test]
        fn timeout_envelope_pins_top_level_schema() {
            const EXPECTED_KEYS: &[&str] = &[
                "amount_covnt",
                "cluster",
                "kind",
                "owner",
                "rpc_url",
                "signature",
                "status",
                "timeout_ms",
                "verb",
            ];

            let value = buy_credits_timeout_json(
                "sigBuy",
                "http://127.0.0.1:8899",
                "localnet",
                "ownerB58",
                42_000,
                15_000,
            );
            let object = value
                .as_object()
                .expect("buy_credits_timeout_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "buy_credits_timeout_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert_eq!(value["kind"].as_str(), Some("covenant.chain.tx.timeout.v1"));
            assert_eq!(value["verb"].as_str(), Some("buy-credits"));
            assert_eq!(value["status"].as_str(), Some("submitted-not-confirmed"));
            assert!(
                value["signature"].is_string(),
                "signature must be a string: {value}"
            );
            assert!(
                value["rpc_url"].is_string(),
                "rpc_url must be a string: {value}"
            );
            assert!(
                value["cluster"].is_string(),
                "cluster must be a string: {value}"
            );
            assert!(
                value["owner"].is_string(),
                "owner must be a string: {value}"
            );
            assert!(
                value["amount_covnt"].is_u64(),
                "amount_covnt must serialize as u64, not string: {value}"
            );
            assert!(
                value["timeout_ms"].is_u64(),
                "timeout_ms must serialize as u64, not string: {value}"
            );
        }
    }
}
