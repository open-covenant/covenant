//! Live proof that covenantd drives the whole spend-grant loop in-process:
//! `chargeCall` (bounded spend) → `releaseCallAttested` (pass) and
//! `refundCallAttested` (junk), signed and broadcast by the daemon itself with
//! no `cast` in the loop. `#[ignore]`d and env-gated — it needs a funded
//! spender key, the deployed escrow, and the escrow's attestor secret, none in
//! CI.
//!
//! ```bash
//! set -a; . "$HOME/.config/covenant/keys.env"; set +a
//! COVENANT_SG_RPC="$RH_TESTNET_RPC" \
//! COVENANT_SG_CHAIN=46630 \
//! COVENANT_SG_ESCROW=0x57316dfc56f1f07fe7ff828f941e9d07f81e2534 \
//! COVENANT_SG_ATTESTOR_KEYFILE="$RH_TESTNET_ATTESTOR_KEYFILE" \
//! COVENANT_SG_ATTESTOR_ADDR="$RH_TESTNET_ATTESTOR_ADDR" \
//! COVENANT_SG_SPENDER_KEY="$RH_TESTNET_AGENT_KEY" \
//! COVENANT_SG_GRANT=1 \
//! COVENANT_SG_PROVIDER="$RH_TESTNET_PROVIDER_ADDR" \
//! cargo test -p covenantd --test live_spend_grant_loop -- --ignored --nocapture
//! ```
//!
//! The grant must be active, name the spender key as its spender, allowlist the
//! provider, and carry budget within `perCallCeiling`. Each run spends a token
//! amount (0.001 USDG) twice — once released to the provider, once refunded.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use covenantd::escrow::CompletionProof;
use covenantd::spend_grant::{SpendGrantAttestor, SpendGrantConfig, SpendGrantSubmitter};
use uuid::Uuid;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn secret_bytes(hex: &str) -> [u8; 32] {
    let body = hex.trim().strip_prefix("0x").unwrap_or(hex.trim());
    assert_eq!(body.len(), 64, "secret hex must be 32 bytes");
    let mut out = [0u8; 32];
    for (i, pair) in body.as_bytes().chunks_exact(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
            .expect("secret hex must be hexadecimal");
    }
    out
}

fn addr20(hex: &str) -> [u8; 20] {
    let body = hex.trim().strip_prefix("0x").unwrap_or(hex.trim());
    assert_eq!(body.len(), 40, "address hex must be 20 bytes");
    let mut out = [0u8; 20];
    for (i, pair) in body.as_bytes().chunks_exact(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
            .expect("address hex must be hexadecimal");
    }
    out
}

fn hexs(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn unique_call_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn proof(result_hash_hex: &str, passed: bool, provider_hex: &str) -> CompletionProof {
    CompletionProof {
        proof_id: Uuid::nil(),
        escrow_id: "esc-live".into(),
        job_id: Uuid::nil(),
        hirer_address: "0xhirer".into(),
        worker_address: provider_hex.into(),
        amount: "1000".into(),
        asset: "USDG".into(),
        network: "robinhood".into(),
        provider: provider_hex.into(),
        result_hash_hex: result_hash_hex.into(),
        validation_passed: passed,
        audit_root_hex: "00".repeat(32),
        proven_at: now(),
    }
}

#[tokio::test]
#[ignore = "live: needs a funded RH-Chain spender + the deployed escrow's attestor key"]
async fn live_covenantd_drives_charge_release_and_refund_in_process() {
    let (
        Some(rpc),
        Some(chain),
        Some(escrow_hex),
        Some(spender_key),
        Some(grant),
        Some(provider_hex),
    ) = (
        env("COVENANT_SG_RPC"),
        env("COVENANT_SG_CHAIN"),
        env("COVENANT_SG_ESCROW"),
        env("COVENANT_SG_SPENDER_KEY"),
        env("COVENANT_SG_GRANT"),
        env("COVENANT_SG_PROVIDER"),
    )
    else {
        eprintln!(
            "skipping: set COVENANT_SG_RPC, COVENANT_SG_CHAIN, COVENANT_SG_ESCROW, \
             COVENANT_SG_SPENDER_KEY, COVENANT_SG_GRANT, COVENANT_SG_PROVIDER, and one of \
             COVENANT_SG_ATTESTOR_KEYFILE / COVENANT_SG_ATTESTOR_KEY to enable"
        );
        return;
    };

    let chain_id: u64 = chain.parse().expect("chain id");
    let escrow = addr20(&escrow_hex);
    let provider = addr20(&provider_hex);
    let grant_id: u128 = grant.parse().expect("grant id");

    // The escrow's attestor role, from either the raw covenant-identity key
    // file (production form) or an inline hex secret.
    let attestor = if let Some(keyfile) = env("COVENANT_SG_ATTESTOR_KEYFILE") {
        SpendGrantAttestor::load_or_create(Path::new(&keyfile)).expect("attestor keyfile")
    } else if let Some(hex) = env("COVENANT_SG_ATTESTOR_KEY") {
        SpendGrantAttestor::from_secret_bytes(&secret_bytes(&hex)).expect("attestor hex")
    } else {
        eprintln!("skipping: set COVENANT_SG_ATTESTOR_KEYFILE or COVENANT_SG_ATTESTOR_KEY");
        return;
    };
    if let Some(want) = env("COVENANT_SG_ATTESTOR_ADDR") {
        assert_eq!(
            hexs(&attestor.address()),
            want.trim().trim_start_matches("0x").to_ascii_lowercase(),
            "attestor key does not recover to the escrow's attestor role — release would revert",
        );
    }
    let submitter = SpendGrantSubmitter::new(rpc, chain_id, &secret_bytes(&spender_key)).unwrap();
    eprintln!(
        "submitter (spender/gas payer): 0x{}",
        hexs(&submitter.address())
    );
    let cfg = SpendGrantConfig::new(attestor, chain_id, escrow)
        .with_submitter(submitter)
        .unwrap();

    let deadline = now() + 3600;
    let amount = 1000u128; // 0.001 USDG (6dp)
    let spec_id = [0x22u8; 32];

    // Pass path — bounded spend then spec-gated release. The daemon charges the
    // hold as the spender, then signs the passing verdict and lands the release,
    // paying the provider. No human, no cast.
    let call_release = unique_call_id();
    let charged = cfg
        .broadcast_charge(grant_id, provider, amount, call_release, deadline)
        .await
        .expect("chargeCall (release path) must confirm");
    eprintln!(
        "charged call {call_release}: block {} gas {} tx 0x{}",
        charged.block_number,
        charged.gas_used,
        hexs(&charged.tx_hash)
    );

    let (sub, released) = cfg
        .settle_and_broadcast(
            &proof(&"11".repeat(32), true, &provider_hex),
            call_release,
            spec_id,
            deadline,
        )
        .await
        .expect("releaseCallAttested must confirm");
    assert!(sub.release, "a passing verdict must route to release");
    assert_eq!(sub.to, escrow);
    assert!(released.block_number > 0);
    eprintln!(
        "released call {call_release}: block {} gas {} tx 0x{}",
        released.block_number,
        released.gas_used,
        hexs(&released.tx_hash)
    );

    // Junk path — charge a fresh hold, then a failing verdict refunds it to the
    // grant immediately. Provider unpaid, capital returned, all in-process.
    let call_refund = call_release + 1;
    let charged2 = cfg
        .broadcast_charge(grant_id, provider, amount, call_refund, deadline)
        .await
        .expect("chargeCall (refund path) must confirm");
    eprintln!(
        "charged call {call_refund}: block {} gas {} tx 0x{}",
        charged2.block_number,
        charged2.gas_used,
        hexs(&charged2.tx_hash)
    );

    let (sub2, refunded) = cfg
        .settle_and_broadcast(
            &proof(&"22".repeat(32), false, &provider_hex),
            call_refund,
            spec_id,
            deadline,
        )
        .await
        .expect("refundCallAttested must confirm");
    assert!(!sub2.release, "a failing verdict must route to refund");
    assert!(refunded.block_number > 0);
    eprintln!(
        "refunded call {call_refund}: block {} gas {} tx 0x{}",
        refunded.block_number,
        refunded.gas_used,
        hexs(&refunded.tx_hash)
    );
}
