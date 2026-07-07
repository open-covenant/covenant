//! Opt-in live assertions that the deployed contracts match `deployments.json`
//! and the "verify it yourself" claims on opencovenant.org/docs/multichain.
//!
//!   cargo test --manifest-path agent-os/evm/Cargo.toml -- --ignored live_deployment
//!
//! `eth_call` reads back the constants an attacker cannot forge: the bond
//! verifier's trusted attestor and USDC on Base mainnet, and the ENS resolver's
//! owner, gateway URL, and signer allowlist on Ethereum L1. Override endpoints
//! with `COVENANT_BASE_MAINNET_RPC` / `COVENANT_ETH_MAINNET_RPC`.

use std::process::Command;

const ISSUER: &str = "0x186953d5b4a290f8f53b8377cb38eda75d664211";
const BOND_VERIFIER: &str = "0xBee387DD4A2fF215d6f997E5DA464C92285BCb6e";
const BASE_USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
const RESOLVER: &str = "0xADDE5F806613FFE71c8d5E137998C511a24e9630";
const GATEWAY_SIGNER: &str = "0x70d879160ead90b9267bbb41f80bbab694824af2";
const DEPLOYER: &str = "0x5fa1d0c0bffe257a20027c523093f941834f5d66";

fn eth_call(url: &str, to: &str, data: &str) -> String {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{ "to": to, "data": data }, "latest"],
    });
    let out = Command::new("curl")
        .args(["-s", "-m", "25", "-X", "POST", url, "-H", "content-type: application/json", "-d"])
        .arg(body.to_string())
        .output()
        .expect("curl must be installed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("RPC returned non-JSON");
    assert!(parsed.get("error").is_none(), "RPC error: {}", parsed["error"]);
    parsed["result"].as_str().expect("result is a hex string").to_lowercase()
}

/// Last 20 bytes of a 32-byte word, as a `0x` address.
fn word_addr(word: &str) -> String {
    let hex = word.trim_start_matches("0x");
    format!("0x{}", &hex[hex.len() - 40..])
}

#[test]
#[ignore = "live: hits public Base + Ethereum RPCs"]
fn live_deployment_matches_the_recorded_addresses() {
    let base = std::env::var("COVENANT_BASE_MAINNET_RPC")
        .unwrap_or_else(|_| "https://mainnet.base.org".into());
    let eth = std::env::var("COVENANT_ETH_MAINNET_RPC")
        .unwrap_or_else(|_| "https://ethereum-rpc.publicnode.com".into());

    // Bond verifier (Base mainnet): trusts the issuer, denominated in native USDC.
    assert_eq!(
        word_addr(&eth_call(&base, BOND_VERIFIER, "0x64f2c5ae")),
        ISSUER,
        "bond verifier TRUSTED_ATTESTOR must be the issuer"
    );
    assert_eq!(
        word_addr(&eth_call(&base, BOND_VERIFIER, "0x89a30271")),
        BASE_USDC,
        "bond verifier USDC must be native Base USDC"
    );

    // ENS OffchainResolver (Ethereum L1): owned by the deployer, gateway signer allowlisted.
    assert_eq!(
        word_addr(&eth_call(&eth, RESOLVER, "0x8da5cb5b")),
        DEPLOYER,
        "resolver owner must be the deployer"
    );
    let signers_call = format!("0x736c0d5b000000000000000000000000{}", &GATEWAY_SIGNER[2..]);
    assert!(
        eth_call(&eth, RESOLVER, &signers_call).ends_with('1'),
        "gateway signer must be allowlisted in the resolver"
    );
    let url = eth_call(&eth, RESOLVER, "0x5600f04f");
    assert!(
        url.contains(&hex::encode("ens-gateway.opencovenant.org")),
        "resolver url must point at the gateway domain"
    );
}
