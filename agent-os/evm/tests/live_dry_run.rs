//! Opt-in live dry-run against the deployed Base Sepolia IdentityRegistry.
//!
//!   cargo test --manifest-path agent-os/evm/Cargo.toml -- --ignored live_
//!
//! `eth_call` simulates `register(string)` without a signer, funds, or any state change,
//! and returns the `uint256` agentId that a real submission would mint. It proves the pinned
//! address, selector, and encoding are accepted by the live contract. Override the endpoint
//! with `COVENANT_BASE_SEPOLIA_RPC`.

use std::process::Command;

use covenant_erc8004_register::{encode_register_call, BASE_SEPOLIA};

fn rpc(method: &str, params: serde_json::Value) -> serde_json::Value {
    let url = std::env::var("COVENANT_BASE_SEPOLIA_RPC")
        .unwrap_or_else(|_| "https://sepolia.base.org".into());
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });

    let out = Command::new("curl")
        .args([
            "-s",
            "-m",
            "25",
            "-X",
            "POST",
            &url,
            "-H",
            "content-type: application/json",
            "-d",
        ])
        .arg(body.to_string())
        .output()
        .expect("curl must be installed to run the live dry-run");
    assert!(
        out.status.success(),
        "curl failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("RPC returned non-JSON");
    assert!(
        parsed.get("error").is_none(),
        "{method} RPC error: {}",
        parsed["error"]
    );
    parsed["result"].clone()
}

#[test]
#[ignore = "live: hits a public Base Sepolia RPC"]
fn live_erc8004_base_sepolia_register_dry_run() {
    assert_eq!(
        rpc("eth_chainId", serde_json::json!([])),
        "0x14a34",
        "not Base Sepolia"
    );

    let data = format!(
        "0x{}",
        hex::encode(encode_register_call("ipfs://covenant-agent-card-dry-run"))
    );
    let result = rpc(
        "eth_call",
        serde_json::json!([
            {
                "from": "0x1111111111111111111111111111111111111111",
                "to": BASE_SEPOLIA.identity_registry,
                "data": data,
            },
            "latest"
        ]),
    );

    let agent_id = result
        .as_str()
        .expect("eth_call result should be a hex string");
    assert_eq!(
        agent_id.len(),
        66,
        "expected a 32-byte uint256 agentId, got {agent_id}"
    );
    let value = u128::from_str_radix(
        agent_id.trim_start_matches("0x").trim_start_matches('0'),
        16,
    )
    .unwrap_or(0);
    assert!(
        value > 0,
        "registry should assign a positive agentId, got {agent_id}"
    );
}
