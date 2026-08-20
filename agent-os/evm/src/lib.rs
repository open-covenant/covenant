//! ERC-8004 Identity Registry registration staging for Covenant agents.
//!
//! Covenant's canonical agent identity lives on Solana. This crate builds — and lets an
//! operator dry-run — the calldata to *also* register a Covenant agent in the EVM-native
//! ERC-8004 Identity Registry, so the agent is discoverable in EVM tooling (8004scan and
//! friends). It stops at the transaction boundary: it never holds a key, signs, or submits.
//!
//! Registry addresses and the ABI are pinned from `erc-8004/erc-8004-contracts` at commit
//! [`DEPLOYMENTS_SOURCE_COMMIT`]. Every registry is an upgradeable ERC-721 proxy: the address
//! is stable but the implementation behind it can change, so a staged mainnet call must be
//! re-verified against the live contract immediately before it is submitted.

use sha3::{Digest, Keccak256};

/// `erc-8004/erc-8004-contracts` commit the addresses and ABI in this crate are pinned to.
pub const DEPLOYMENTS_SOURCE_COMMIT: &str = "68fc6765761a10fb26f0692df21c8a6f9d12b1be";

/// Canonical registration signature. The 4-byte selector is derived from this, never hard-coded.
pub const REGISTER_STRING_SIGNATURE: &str = "register(string)";

/// A pinned ERC-8004 Identity Registry deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deployment {
    pub network: &'static str,
    pub chain_id: u64,
    /// EIP-55 checksummed IdentityRegistry (upgradeable ERC-721 proxy) address.
    pub identity_registry: &'static str,
}

pub const BASE_MAINNET: Deployment = Deployment {
    network: "Base",
    chain_id: 8453,
    identity_registry: "0x8004A169FB4a3325136EB29fA0ceB6D2e539a432",
};

pub const BASE_SEPOLIA: Deployment = Deployment {
    network: "Base Sepolia",
    chain_id: 84532,
    identity_registry: "0x8004A818BFB912233c491871b3d84c89A494BD9e",
};

pub const ETHEREUM_SEPOLIA: Deployment = Deployment {
    network: "Ethereum Sepolia",
    chain_id: 11155111,
    identity_registry: "0x8004A818BFB912233c491871b3d84c89A494BD9e",
};

pub const DEPLOYMENTS: &[Deployment] = &[BASE_MAINNET, BASE_SEPOLIA, ETHEREUM_SEPOLIA];

pub fn deployment_for_chain(chain_id: u64) -> Option<Deployment> {
    DEPLOYMENTS.iter().copied().find(|d| d.chain_id == chain_id)
}

/// The 4-byte function selector for a Solidity signature.
pub fn selector(signature: &str) -> [u8; 4] {
    let digest = Keccak256::digest(signature.as_bytes());
    [digest[0], digest[1], digest[2], digest[3]]
}

pub fn register_string_selector() -> [u8; 4] {
    selector(REGISTER_STRING_SIGNATURE)
}

/// Build `register(string agentURI)` calldata for the ERC-8004 Identity Registry.
pub fn encode_register_call(agent_uri: &str) -> Vec<u8> {
    let mut data = register_string_selector().to_vec();
    data.extend(encode_string_arg(agent_uri));
    data
}

/// ABI head+tail for a single dynamic `string` argument: offset word, length word,
/// then the bytes right-padded to a 32-byte boundary.
fn encode_string_arg(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(64 + bytes.len().next_multiple_of(32));
    out.extend_from_slice(&word(32));
    out.extend_from_slice(&word(bytes.len() as u64));
    out.extend_from_slice(bytes);
    out.resize(out.len() + (32 - bytes.len() % 32) % 32, 0);
    out
}

fn word(value: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&value.to_be_bytes());
    w
}

/// How resistant an agentURI is to having its target content swapped after registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentUriClass {
    /// The URI fixes its own content — content-addressed (`ipfs://`, `ar://`) or inline
    /// (`data:`) — so the registered target cannot be silently repointed.
    ContentAddressed,
    /// A mutable location; trust would have to come from a signature over the fetched document.
    MutableLocation,
}

pub fn classify_agent_uri(uri: &str) -> AgentUriClass {
    let lower = uri.trim().to_ascii_lowercase();
    if lower.starts_with("ipfs://") || lower.starts_with("ar://") || lower.starts_with("data:") {
        AgentUriClass::ContentAddressed
    } else {
        AgentUriClass::MutableLocation
    }
}

/// Gate an agentURI before it is pinned into a registration. ERC-8004's on-chain handle
/// resolves to whatever the agentURI points at, so a mutable location would let the
/// registered identity be silently repointed at different content. A staged registration
/// must therefore use a content-addressed URI.
pub fn ensure_immutable_agent_uri(uri: &str) -> Result<(), String> {
    if uri.trim().is_empty() {
        return Err("agentURI is empty".into());
    }
    match classify_agent_uri(uri) {
        AgentUriClass::ContentAddressed => Ok(()),
        AgentUriClass::MutableLocation => Err(format!(
            "agentURI {uri:?} is a mutable location; pin a content-addressed URI (ipfs://, ar://, data:)"
        )),
    }
}

/// Validate an address is EIP-55 checksummed. Doubles as a transcription guard on the
/// pinned registry constants: EIP-55 can't catch a digit-only typo in isolation, but these
/// addresses are alpha-rich, so any changed nibble re-derives the hash and breaks the casing.
pub fn is_eip55_checksummed(addr: &str) -> bool {
    let Some(hex) = addr.strip_prefix("0x") else {
        return false;
    };
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let hash = Keccak256::digest(hex.to_ascii_lowercase().as_bytes());
    hex.char_indices()
        .filter(|(_, c)| c.is_ascii_alphabetic())
        .all(|(i, c)| {
            let nibble = (hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 })) & 0xf;
            (nibble >= 8) == c.is_ascii_uppercase()
        })
}

/// A ready-to-submit EVM transaction request. Unsigned on purpose: submission is operator-gated.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StagedCall {
    pub network: &'static str,
    pub chain_id: u64,
    pub to: &'static str,
    pub function: &'static str,
    pub agent_uri: String,
    /// 0x-prefixed calldata.
    pub data: String,
    /// Wei value; registration is non-payable, so always `0x0`.
    pub value: String,
    pub source_commit: &'static str,
    pub notes: Vec<String>,
}

pub fn stage_register_call(deployment: Deployment, agent_uri: &str) -> Result<StagedCall, String> {
    ensure_immutable_agent_uri(agent_uri)?;
    let data = encode_register_call(agent_uri);
    Ok(StagedCall {
        network: deployment.network,
        chain_id: deployment.chain_id,
        to: deployment.identity_registry,
        function: REGISTER_STRING_SIGNATURE,
        agent_uri: agent_uri.to_string(),
        data: format!("0x{}", hex::encode(data)),
        value: "0x0".into(),
        source_commit: DEPLOYMENTS_SOURCE_COMMIT,
        notes: vec![
            "IdentityRegistry is an upgradeable ERC-721 proxy; re-verify the live implementation against this calldata immediately before submission.".into(),
            "Submission needs a funded EOA and an operator signature; this request is intentionally unsigned.".into(),
        ],
    })
}
