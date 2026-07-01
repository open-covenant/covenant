//! Opt-in live dry-run of the bond-receipt verifier against Base Sepolia.
//!
//!   cargo test -p covenant-attestation --test live_bond_verifier -- --ignored live_
//!
//! No key, funds, or state change touch the chain. The bond-receipt is
//! verified on-chain the same way the reference `BondReceiptVerifier.sol`
//! verifies it — through the `ecrecover` precompile (address `0x01`) — so
//! these tests prove the exact digest and signature this crate produces
//! recover Covenant's attestor on the live chain, and that expiry is
//! judged against real Base Sepolia block time. Override the endpoint with
//! `COVENANT_BASE_SEPOLIA_RPC`.

use std::process::Command;

use covenant_attestation::{BaseNetwork, BondReceipt};
use covenant_identity::Secp256k1IssuerKey;

const ECRECOVER: &str = "0x0000000000000000000000000000000000000001";

fn rpc(method: &str, params: serde_json::Value) -> serde_json::Value {
    let url = std::env::var("COVENANT_BASE_SEPOLIA_RPC")
        .unwrap_or_else(|_| "https://sepolia.base.org".into());
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });

    let out = Command::new("curl")
        .args([
            "-s", "-m", "25", "-X", "POST", &url, "-H", "content-type: application/json", "-d",
        ])
        .arg(body.to_string())
        .output()
        .expect("curl must be installed to run the live dry-run");
    assert!(
        out.status.success(),
        "curl failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("RPC returned non-JSON")
}

fn attestor() -> Secp256k1IssuerKey {
    Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap()
}

fn receipt(expiry: u64) -> BondReceipt {
    BondReceipt {
        network: BaseNetwork::Sepolia,
        subject: [0xAB; 32],
        bond_token: BaseNetwork::Sepolia.usdc(),
        bond_amount: 1_000_000,
        agent_return: [0x11; 20],
        slash_beneficiary: [0x22; 20],
        slash_beneficiary_bps: 8_000,
        nonce: [0xCD; 32],
        issued_at: 1_700_000_000,
        expiry,
    }
}

fn assert_base_sepolia() {
    assert_eq!(
        rpc("eth_chainId", serde_json::json!([]))["result"],
        "0x14a34",
        "not Base Sepolia"
    );
}

fn latest_block_timestamp() -> u64 {
    let block = rpc("eth_getBlockByNumber", serde_json::json!(["latest", false]));
    let hex = block["result"]["timestamp"]
        .as_str()
        .expect("block has a timestamp")
        .strip_prefix("0x")
        .expect("timestamp is 0x-hex");
    u64::from_str_radix(hex, 16).expect("timestamp parses")
}

/// The binding leg: the `ecrecover` precompile on live Base Sepolia
/// recovers Covenant's attestor from the exact digest and signature the
/// crate builds — the same call the on-chain verifier makes.
#[test]
#[ignore = "live: hits a public Base Sepolia RPC"]
fn live_ecrecover_precompile_recovers_attestor() {
    assert_base_sepolia();
    let key = attestor();
    let signed = receipt(1_800_000_000).sign(&key).unwrap();

    let calldata = format!(
        "0x{}",
        signed
            .ecrecover_precompile_calldata()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let response = rpc(
        "eth_call",
        serde_json::json!([{ "to": ECRECOVER, "data": calldata }, "latest"]),
    );
    let result = response["result"]
        .as_str()
        .unwrap_or_else(|| panic!("ecrecover precompile returned no result: {response}"));

    // The precompile returns the address right-aligned in a 32-byte word.
    let expected = format!(
        "0x000000000000000000000000{}",
        key.address()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(
        result, expected,
        "on-chain ecrecover did not return the attestor address"
    );
}

/// Expiry is judged against real chain time: a receipt that expired before
/// the latest Base Sepolia block is rejected, one valid past it is
/// accepted. This is the exact `block.timestamp <= expiry` check the
/// reference verifier performs.
#[test]
#[ignore = "live: hits a public Base Sepolia RPC"]
fn live_expiry_is_judged_against_base_sepolia_block_time() {
    assert_base_sepolia();
    let key = attestor();
    let now = latest_block_timestamp();
    let addr = key.address();

    let expired = receipt(now - 1).sign(&key).unwrap();
    assert!(
        matches!(
            expired.verify(&addr, now),
            Err(covenant_attestation::BondError::Expired { .. })
        ),
        "a bond that expired before the latest block must be rejected"
    );

    let live = receipt(now + 86_400).sign(&key).unwrap();
    assert!(
        live.verify(&addr, now).is_ok(),
        "a bond valid a day past the latest block must verify"
    );
}
