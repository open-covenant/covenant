//! Opt-in live dry-run of the on-chain reputation `attest` against the deployed
//! Base Sepolia EAS predeploy.
//!
//!   cargo test -p covenant-evm-signer --test live_relay_dry_run -- --ignored live_
//!
//! `eth_call` simulates the write with no signer, funds, or state change. It
//! proves the pinned EAS address and version, the derived `attest` selector, and
//! the nested-tuple ABI encoding are accepted by the live contract. The
//! reputation schema is not yet registered on Base Sepolia — registering it is a
//! separate gated write — so `attest` reverts `InvalidSchema()`, which is exactly
//! the precondition the staged transaction documents. Override the endpoint with
//! `COVENANT_BASE_SEPOLIA_RPC`.

use std::process::Command;

use covenant_evm_signer::{
    stage_reputation_attestation, RelayPolicy, ReputationProjection, ReputationScore, BASE_SEPOLIA,
    SOLANA_MAINNET_CAIP2,
};

const EAS: &str = "0x4200000000000000000000000000000000000021";

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

fn projection() -> ReputationProjection {
    ReputationProjection::new(
        ReputationScore::from_ratio(95, 100, 4).unwrap(),
        SOLANA_MAINNET_CAIP2,
        [0xAB; 32],
        1_700_000_000,
        1_800_000_000,
    )
}

#[test]
#[ignore = "live: hits a public Base Sepolia RPC"]
fn live_base_sepolia_reputation_attest_dry_run() {
    assert_eq!(
        rpc("eth_chainId", serde_json::json!([]))["result"],
        "0x14a34",
        "not Base Sepolia"
    );

    // The EAS predeploy at the pinned address reports the version the off-chain
    // domain signs under. "312e322e30" is the ABI-encoded UTF-8 of "1.2.0".
    let version = rpc(
        "eth_call",
        serde_json::json!([{ "to": EAS, "data": "0x54fd4d50" }, "latest"]),
    );
    let version_hex = version["result"].as_str().unwrap_or_default();
    assert!(
        version_hex.contains("312e322e30"),
        "unexpected EAS version() at {EAS}: {version_hex}"
    );

    // The real reputation calldata, from the same builder an operator submits.
    let tx =
        stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default()).unwrap();
    let response = rpc(
        "eth_call",
        serde_json::json!([
            { "from": "0x1111111111111111111111111111111111111111", "to": EAS, "data": tx.calldata_hex() },
            "latest"
        ]),
    );

    // Either the schema is registered and attest returns a bytes32 UID, or it is
    // not and attest reverts InvalidSchema() — 0xbf37b20e, keccak256("InvalidSchema()")[..4],
    // observed live. Both prove the calldata ABI-decoded on the live contract; a
    // malformed request would fail before the schema-existence check.
    if let Some(result) = response.get("result").and_then(serde_json::Value::as_str) {
        assert_eq!(result.len(), 66, "attest UID should be 32 bytes, got {result}");
    } else {
        let data = response["error"]["data"].as_str().unwrap_or_default();
        assert!(
            data.starts_with("0xbf37b20e"),
            "expected InvalidSchema (schema not yet registered) or a UID, got {response}"
        );
    }
}
