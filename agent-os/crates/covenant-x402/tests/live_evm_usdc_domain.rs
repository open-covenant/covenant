//! Live on-chain verification of the pinned Base USDC EIP-712 domains.
//!
//! `evm.rs` pins a fallback domain per USDC deployment: Base mainnet
//! (`"USD Coin"`/`"2"`) and Base Sepolia (`"USDC"`/`"2"`). Rather than
//! asserting those against a transcribed `DOMAIN_SEPARATOR` constant —
//! which would only prove we copied a value — this test asks the token
//! contracts themselves over `eth_call` (`name()`, `version()`,
//! `DOMAIN_SEPARATOR()`) and recomputes the separator from primitives
//! independently of the crate's encoder, so a wrong pinned name or
//! version fails here against the chain.
//!
//! `#[ignore]`d: it needs outbound HTTPS to a Base RPC. There is no
//! silent-skip path — endpoints default to the public
//! `https://mainnet.base.org` / `https://sepolia.base.org`, overridable
//! via `COVENANT_X402_EVM_RPC_BASE_MAINNET` /
//! `COVENANT_X402_EVM_RPC_BASE_SEPOLIA`. When selected, the test either
//! verifies or fails.
//!
//! ```bash
//! cargo test -p covenant-x402 --test live_evm_usdc_domain -- --ignored
//! ```

use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use covenant_x402::{USDC_BASE_MAINNET, USDC_BASE_SEPOLIA};

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(value: &str) -> Vec<u8> {
    let body = value.strip_prefix("0x").unwrap_or(value);
    assert!(body.len().is_multiple_of(2), "odd-length hex: {value:?}");
    (0..body.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&body[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The 4-byte selector, computed from the signature string so no
/// transcribed constant can drift.
fn selector(signature: &str) -> String {
    format!("0x{}", hex(&keccak256(signature.as_bytes())[..4]))
}

async fn eth_call(rpc: &str, to: &str, signature: &str) -> Vec<u8> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{ "to": to, "data": selector(signature) }, "latest"],
    });
    let resp: Value = reqwest::Client::new()
        .post(rpc)
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {rpc}: {e}"))
        .json()
        .await
        .unwrap_or_else(|e| panic!("{rpc} returned non-JSON: {e}"));
    let result = resp["result"]
        .as_str()
        .unwrap_or_else(|| panic!("{rpc} eth_call {signature} on {to} gave no result: {resp}"));
    unhex(result)
}

/// Decode a solidity `string` return: offset word, length word, bytes.
fn abi_string(data: &[u8]) -> String {
    assert!(
        data.len() >= 64,
        "ABI string return needs offset+length words, got {} bytes",
        data.len()
    );
    let offset = u64::from_be_bytes(data[24..32].try_into().unwrap()) as usize;
    let len = u64::from_be_bytes(data[offset + 24..offset + 32].try_into().unwrap()) as usize;
    String::from_utf8(data[offset + 32..offset + 32 + len].to_vec()).expect("UTF-8 string")
}

/// Independent EIP-712 domain-separator recompute — deliberately not the
/// crate's encoder, so the on-chain value cross-checks both the pinned
/// domain strings and (via the crate's own Ether Mail vectors) its
/// encoding.
fn domain_separator(name: &str, version: &str, chain_id: u64, contract: &str) -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * 5);
    buf.extend_from_slice(&keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    buf.extend_from_slice(&keccak256(name.as_bytes()));
    buf.extend_from_slice(&keccak256(version.as_bytes()));
    let mut chain_word = [0u8; 32];
    chain_word[24..].copy_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(&chain_word);
    let mut addr_word = [0u8; 32];
    addr_word[12..].copy_from_slice(&unhex(contract));
    buf.extend_from_slice(&addr_word);
    keccak256(&buf)
}

fn rpc_url(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

async fn assert_usdc_domain(
    rpc: &str,
    chain_id: u64,
    contract: &str,
    want_name: &str,
    want_version: &str,
) {
    let name = abi_string(&eth_call(rpc, contract, "name()").await);
    assert_eq!(
        name, want_name,
        "on-chain name() for {contract} on chain {chain_id}"
    );
    let version = abi_string(&eth_call(rpc, contract, "version()").await);
    assert_eq!(
        version, want_version,
        "on-chain version() for {contract} on chain {chain_id}"
    );
    let sep = eth_call(rpc, contract, "DOMAIN_SEPARATOR()").await;
    let expected = domain_separator(want_name, want_version, chain_id, contract);
    assert_eq!(
        sep,
        expected.to_vec(),
        "recomputed EIP-712 domain separator for {contract} on chain {chain_id}"
    );
    eprintln!(
        "verified chain {chain_id} {contract}: name {want_name:?}, version {want_version:?}, \
         DOMAIN_SEPARATOR 0x{}",
        hex(&sep)
    );
}

#[tokio::test]
#[ignore = "live: eth_calls Base mainnet USDC over a public RPC"]
async fn live_base_mainnet_usdc_domain_matches_pin() {
    let rpc = rpc_url(
        "COVENANT_X402_EVM_RPC_BASE_MAINNET",
        "https://mainnet.base.org",
    );
    assert_usdc_domain(&rpc, 8453, USDC_BASE_MAINNET, "USD Coin", "2").await;
}

#[tokio::test]
#[ignore = "live: eth_calls Base Sepolia USDC over a public RPC"]
async fn live_base_sepolia_usdc_domain_matches_pin() {
    let rpc = rpc_url(
        "COVENANT_X402_EVM_RPC_BASE_SEPOLIA",
        "https://sepolia.base.org",
    );
    assert_usdc_domain(&rpc, 84532, USDC_BASE_SEPOLIA, "USDC", "2").await;
}
