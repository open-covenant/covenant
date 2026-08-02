//! Live check that [`covenant_x402::EvmSigner`] produces a payment a real
//! x402 facilitator accepts. `#[ignore]`d and env-gated: it needs a Base
//! Sepolia facilitator and a funding key, neither of which lives in CI.
//!
//! ```bash
//! COVENANT_X402_EVM_FACILITATOR_URL=https://x402.org/facilitator \
//! COVENANT_X402_EVM_SECRET_HEX=<32-byte hex secp256k1 secret> \
//! cargo test -p covenant-x402 -- --ignored live_evm
//! ```
//!
//! The signer never touches an RPC — the EIP-3009 authorization is signed
//! locally — so this exercises exactly one boundary: does the facilitator's
//! `/verify` recover our signature and accept the `exact`-scheme payload
//! shape? A funded key additionally clears the balance check; an unfunded
//! key still proves the signature and wire format are what the facilitator
//! expects (it rejects on balance, not on signature).
//!
//! When the env vars are unset the test skips LOUDLY, naming what is
//! missing — a silent early return would let a run that verified nothing
//! read as facilitator coverage.

use serde_json::{json, Value};

use covenant_x402::{EvmSigner, PaymentExtra, PaymentRequirements, Signer, USDC_BASE_SEPOLIA};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn secret_bytes(hex: &str) -> [u8; 32] {
    let body = hex.strip_prefix("0x").unwrap_or(hex);
    assert_eq!(
        body.len(),
        64,
        "COVENANT_X402_EVM_SECRET_HEX must be 32 bytes"
    );
    let mut out = [0u8; 32];
    for (i, pair) in body.as_bytes().chunks_exact(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
            .expect("secret hex must be hexadecimal");
    }
    out
}

#[tokio::test]
#[ignore = "live: needs a Base Sepolia x402 facilitator URL and a funding key"]
async fn live_evm_facilitator_accepts_exact_payment() {
    let facilitator = env("COVENANT_X402_EVM_FACILITATOR_URL");
    let secret_hex = env("COVENANT_X402_EVM_SECRET_HEX");
    let mut missing = Vec::new();
    if facilitator.is_none() {
        missing.push("COVENANT_X402_EVM_FACILITATOR_URL");
    }
    if secret_hex.is_none() {
        missing.push("COVENANT_X402_EVM_SECRET_HEX");
    }
    if !missing.is_empty() {
        // Loud, named skip: a bare early return reads as a pass in test
        // output, and a run that verified nothing must not be mistaken
        // for facilitator coverage.
        eprintln!(
            "SKIPPED (verified nothing): live_evm_facilitator_accepts_exact_payment \
             ran without {} — set the unset var(s) to exercise the facilitator boundary",
            missing.join(" and ")
        );
        return;
    }
    let (facilitator, secret_hex) = (facilitator.unwrap(), secret_hex.unwrap());

    let signer = EvmSigner::from_secret_bytes(&secret_bytes(&secret_hex)).expect("signer");

    let requirement = PaymentRequirements {
        network: "base-sepolia".into(),
        asset: USDC_BASE_SEPOLIA.into(),
        amount: "1000".into(),
        amount_usdc: 0.001,
        pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
        scheme: "exact".into(),
        extra: Some(PaymentExtra {
            fee_payer: None,
            name: Some("USDC".into()),
            version: Some("2".into()),
        }),
    };

    let header = signer
        .build_payment(&requirement)
        .await
        .expect("build payment");
    let payment_payload: Value = {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        serde_json::from_slice(&BASE64.decode(header).expect("base64")).expect("json")
    };

    let body = json!({
        "x402Version": 1,
        "paymentPayload": payment_payload,
        "paymentRequirements": {
            "scheme": requirement.scheme,
            "network": requirement.network,
            "maxAmountRequired": requirement.amount,
            "resource": "https://example.test/live-evm",
            "description": "covenant-x402 live EVM signer check",
            "mimeType": "application/json",
            "payTo": requirement.pay_to,
            "maxTimeoutSeconds": 120,
            "asset": requirement.asset,
            "extra": { "name": "USDC", "version": "2" },
        },
    });

    let url = format!("{}/verify", facilitator.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("facilitator reachable");
    let status = resp.status();
    let text = resp.text().await.expect("read body");
    let verdict: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("facilitator /verify returned non-JSON ({status}): {e}: {text}")
    });

    eprintln!("facilitator /verify -> {status}: {verdict}");
    assert!(
        verdict.get("isValid").is_some() || verdict.get("invalidReason").is_some(),
        "facilitator response must carry an x402 verify verdict, got: {verdict}"
    );
    // An unfunded key legitimately fails only the balance check. Accept
    // exactly funding-class reasons and fail on everything else —
    // signature, scheme, network, payload shape, or any reason string
    // this test has never seen — so a wire regression cannot pass as
    // "probably a funding shortfall".
    if let Some(reason) = verdict.get("invalidReason").and_then(Value::as_str) {
        let r = reason.to_ascii_lowercase();
        assert!(
            r.contains("fund") || r.contains("balance"),
            "facilitator rejected for a non-funding reason: {reason}"
        );
    }
}
