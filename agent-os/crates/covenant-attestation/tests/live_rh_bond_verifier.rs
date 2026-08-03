//! Opt-in live proof of the deployed bond verifier on Robinhood Chain (4663).
//!
//!   cargo test -p covenant-attestation --test live_rh_bond_verifier -- --ignored live_
//!
//! Unlike the Base Sepolia dry-run (`live_bond_verifier`), which exercises only
//! the `ecrecover` precompile, this test calls the *deployed* verifier — the one
//! recorded as `robinhoodMainnet.bondReceiptVerifier` in
//! `agent-os/evm/deployments.json` — through its own `view` methods. Together the
//! legs prove a canonical-issuer-signed USDG bond verifies on 4663:
//!
//!   1. its immutables pin the canonical attestor and USDG (not a 4663-local key);
//!   2. its EIP-712 `domainSeparator()`/`digest()` equal the bytes this crate
//!      builds for a `RobinhoodMainnet` receipt — same encoding, same chain id;
//!   3. the `ecrecover` precompile recovers the signer of a crate-signed receipt,
//!      so the signature itself is valid on 4663;
//!   4. `verify()` reverts `UntrustedSigner` for that same receipt because it was
//!      signed by a test key, not the canonical attestor — proving the gate is
//!      live and the *only* thing between a well-formed receipt and acceptance is
//!      the issuer identity. A real happy path is one canonical signature away,
//!      producible only by the operator-custodied issuer key.
//!
//! Every leg is an `eth_call`: no key, funds, or state change touch the chain.
//! Override the endpoint with `COVENANT_RH_MAINNET_RPC` and the verifier address
//! with `COVENANT_RH_BOND_VERIFIER`.

use std::process::Command;

use covenant_attestation::{BondNetwork, BondReceipt};
use covenant_identity::Secp256k1IssuerKey;

const DEFAULT_RPC: &str = "https://rpc.mainnet.chain.robinhood.com";
const DEFAULT_VERIFIER: &str = "0x116e14bcf539c1095aa60140e74d904eb2fd4dd5";
const ECRECOVER: &str = "0x0000000000000000000000000000000000000001";

// Canonical Covenant issuer / bond attestor — parity with the Base verifier.
const CANONICAL_ATTESTOR: &str = "186953d5b4a290f8f53b8377cb38eda75d664211";
const USDG: &str = "5fc5360d0400a0fd4f2af552add042d716f1d168";

const SEL_ATTESTOR: &str = "64f2c5ae";
const SEL_USDC: &str = "89a30271";
const SEL_DOMAIN: &str = "f698da25";
const SEL_DIGEST: &str = "4ce308da";
const SEL_VERIFY: &str = "57e325b2";
const ERR_UNTRUSTED_SIGNER: &str = "d0b145db";

fn rpc(method: &str, params: serde_json::Value) -> serde_json::Value {
    let url = std::env::var("COVENANT_RH_MAINNET_RPC").unwrap_or_else(|_| DEFAULT_RPC.into());
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
    serde_json::from_slice(&out.stdout).expect("RPC returned non-JSON")
}

fn verifier() -> String {
    std::env::var("COVENANT_RH_BOND_VERIFIER").unwrap_or_else(|_| DEFAULT_VERIFIER.into())
}

fn eth_call(to: &str, data: String) -> serde_json::Value {
    rpc(
        "eth_call",
        serde_json::json!([{ "to": to, "data": format!("0x{data}") }, "latest"]),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn word_u128(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// ABI-encode the `BondReceipt` calldata tuple (9 static words, 288 bytes),
/// field order matching the Solidity struct.
fn encode_receipt(r: &BondReceipt) -> [u8; 288] {
    let mut w = [0u8; 288];
    w[..32].copy_from_slice(&r.subject);
    w[44..64].copy_from_slice(&r.bond_token);
    w[64..96].copy_from_slice(&word_u128(r.bond_amount));
    w[108..128].copy_from_slice(&r.agent_return);
    w[140..160].copy_from_slice(&r.slash_beneficiary);
    w[160..192].copy_from_slice(&word_u64(r.slash_beneficiary_bps as u64));
    w[192..224].copy_from_slice(&r.nonce);
    w[224..256].copy_from_slice(&word_u64(r.issued_at));
    w[256..288].copy_from_slice(&word_u64(r.expiry));
    w
}

/// The exact receipt pinned by `eip712_encoding_is_pinned` in `bond.rs`
/// (`sample(RobinhoodMainnet)`), whose digest is `399e5590…67bcfa`.
fn sample() -> BondReceipt {
    BondReceipt {
        network: BondNetwork::RobinhoodMainnet,
        subject: [0xAB; 32],
        bond_token: BondNetwork::RobinhoodMainnet.bond_token(),
        bond_amount: 1_000_000,
        agent_return: [0x11; 20],
        slash_beneficiary: [0x22; 20],
        slash_beneficiary_bps: 8_000,
        nonce: [0xCD; 32],
        issued_at: 1_700_000_000,
        expiry: 1_800_000_000,
    }
}

fn test_attestor() -> Secp256k1IssuerKey {
    Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap()
}

fn assert_rh_mainnet() {
    assert_eq!(
        rpc("eth_chainId", serde_json::json!([]))["result"],
        "0x1237",
        "not Robinhood Chain 4663"
    );
}

/// The deployed verifier pins the canonical attestor and USDG in its
/// immutables — the same attestor as the Base verifier, so a production
/// covenantd-signed bond verifies on 4663 with no chain-local trust root.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_getters_pin_canonical_attestor_and_usdg() {
    assert_rh_mainnet();
    let v = verifier();

    let attestor = eth_call(&v, SEL_ATTESTOR.into());
    let attestor = attestor["result"].as_str().expect("attestor result");
    assert!(
        attestor.to_lowercase().ends_with(CANONICAL_ATTESTOR),
        "verifier does not pin the canonical attestor: {attestor}"
    );

    let usdc = eth_call(&v, SEL_USDC.into());
    let usdc = usdc["result"].as_str().expect("usdc result");
    assert!(
        usdc.to_lowercase().ends_with(USDG),
        "verifier bond token is not USDG: {usdc}"
    );
}

/// The deployed EIP-712 encoding is byte-identical to this crate's for a
/// `RobinhoodMainnet` receipt: same domain separator (chain id 4663 baked in)
/// and same digest as the pinned `399e5590…` vector.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_domain_and_digest_match_crate() {
    assert_rh_mainnet();
    let v = verifier();
    let r = sample();

    let onchain_domain = eth_call(&v, SEL_DOMAIN.into());
    let onchain_domain = onchain_domain["result"].as_str().expect("domain result");
    assert_eq!(
        onchain_domain,
        format!("0x{}", hex(&r.domain_separator())),
        "on-chain domainSeparator() != crate domain_separator()"
    );

    let onchain_digest = eth_call(&v, format!("{SEL_DIGEST}{}", hex(&encode_receipt(&r))));
    let onchain_digest = onchain_digest["result"].as_str().expect("digest result");
    assert_eq!(
        onchain_digest,
        format!("0x{}", hex(&r.digest())),
        "on-chain digest() != crate digest()"
    );
    assert_eq!(
        onchain_digest, "0x399e55908e91ebece6739af5364ef571099e4859c11331e5795ceb1b1b67bcfa",
        "on-chain digest drifted from the pinned vector"
    );
}

/// The `ecrecover` precompile on live 4663 recovers the signer of a
/// crate-signed receipt — the signature is valid on this chain, so the only
/// reason `verify()` can reject it is the attestor gate (next test).
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_ecrecover_precompile_recovers_signer() {
    assert_rh_mainnet();
    let key = test_attestor();
    let signed = sample().sign(&key).unwrap();

    let calldata = format!("0x{}", hex(&signed.ecrecover_precompile_calldata()));
    let response = rpc(
        "eth_call",
        serde_json::json!([{ "to": ECRECOVER, "data": calldata }, "latest"]),
    );
    let result = response["result"]
        .as_str()
        .unwrap_or_else(|| panic!("ecrecover precompile returned no result: {response}"));
    assert_eq!(
        result,
        format!("0x000000000000000000000000{}", hex(&key.address())),
        "on-chain ecrecover did not return the signer address"
    );
}

/// `verify()` reverts `UntrustedSigner` for a well-formed, validly-signed
/// receipt whose signer is a test key rather than the canonical attestor.
/// This is the gate: the receipt clears every field and malleability check
/// and the signature recovers cleanly (previous test), so the revert isolates
/// the issuer-identity check — a canonical signature is all that is missing.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_verify_enforces_canonical_attestor_gate() {
    assert_rh_mainnet();
    let v = verifier();
    let signed = sample().sign(&test_attestor()).unwrap();
    let sig = signed.signature();

    let mut data = String::from(SEL_VERIFY);
    data.push_str(&hex(&encode_receipt(signed.receipt())));
    data.push_str(&hex(&word_u64(sig[64] as u64)));
    data.push_str(&hex(&sig[..32]));
    data.push_str(&hex(&sig[32..64]));

    let response = eth_call(&v, data);
    assert!(
        response.get("result").is_none(),
        "verify() unexpectedly succeeded for a non-canonical signer: {response}"
    );
    let blob = response.to_string();
    assert!(
        blob.contains(ERR_UNTRUSTED_SIGNER),
        "verify() reverted, but not with UntrustedSigner: {response}"
    );
}
