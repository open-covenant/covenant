//! Opt-in live proof of the deployed reputation registry on Robinhood Chain (4663).
//!
//!   cargo test -p covenant-attestation --test live_rh_reputation_registry -- --ignored live_
//!
//! Sibling of `live_rh_bond_verifier`. It calls the *deployed*
//! `CovenantReputationRegistry` — recorded as `robinhoodMainnet.reputationRegistry`
//! in `agent-os/evm/deployments.json` — through its `view` methods. Together the
//! legs prove a canonical-issuer-signed reputation score verifies on 4663:
//!
//!   1. `TRUSTED_ATTESTOR()` pins the canonical attestor (the same key the Base
//!      and 4663 bond verifiers pin), not a 4663-local one;
//!   2. `domainSeparator()`/`digest()` equal the bytes this crate builds — and,
//!      unlike the bond verifier, the domain is chain-agnostic (constant salt, no
//!      chain id), so the same score verifies here, on Base, and off-chain;
//!   3. the `ecrecover` precompile recovers the signer of a crate-signed score,
//!      so the signature is valid on this chain;
//!   4. `verify()` reverts `UntrustedSigner` for a live (unexpired) score signed
//!      by a test key rather than the canonical attestor — the gate is the issuer
//!      identity, and a real happy path is one canonical signature away;
//!   5. `verify()` reverts `Expired` for a past-dated score before it ever reaches
//!      the signer gate — fail-closed on staleness.
//!
//! Every leg is an `eth_call`: no key, funds, or state change touch the chain.
//! Override the endpoint with `COVENANT_RH_MAINNET_RPC` and the registry address
//! with `COVENANT_RH_REPUTATION_REGISTRY`.

use std::process::Command;

use covenant_attestation::{
    ReputationAttestation, SignedReputationAttestation, SOURCE_CHAIN_SOLANA,
};
use covenant_identity::Secp256k1IssuerKey;

const DEFAULT_RPC: &str = "https://rpc.mainnet.chain.robinhood.com";
const DEFAULT_REGISTRY: &str = "0xa691dd0f06999233d56f2b397c41cd7542c74aed";
const ECRECOVER: &str = "0x0000000000000000000000000000000000000001";

// Canonical Covenant issuer / attestor — parity with the bond verifiers.
const CANONICAL_ATTESTOR: &str = "186953d5b4a290f8f53b8377cb38eda75d664211";

const SEL_ATTESTOR: &str = "64f2c5ae"; // TRUSTED_ATTESTOR()
const SEL_DOMAIN: &str = "f698da25"; // domainSeparator()
const SEL_DIGEST: &str = "82e5b984"; // digest((bytes32,uint32,uint8,uint64,string,bytes32))
const SEL_VERIFY: &str = "8d8a6e1d"; // verify(tuple,uint8,bytes32,bytes32)
const ERR_UNTRUSTED_SIGNER: &str = "d0b145db";
const ERR_EXPIRED: &str = "203d82d8";

// Pinned parity constants (also pinned in reputation.rs `eip712_encoding_is_pinned`).
const DOMAIN_SEPARATOR: &str = "0xa1810486e59f4b39150c8c9cf9944cf3cf07150d1371650d7eb96d1b71e562fb";
const GOLDEN_DIGEST: &str = "0xe9a0fe2c860337d88659e7a68324eb92f8942f0ca36a0742aa3c45ee4dcccef5";

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

fn registry() -> String {
    std::env::var("COVENANT_RH_REPUTATION_REGISTRY").unwrap_or_else(|_| DEFAULT_REGISTRY.into())
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

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// ABI-encode the `Reputation` tuple body: 6 head words then the `sourceChain`
/// string tail. The string offset is a constant `0xc0` (6 words) because the
/// string is the tuple's only dynamic member and follows the head. Validated
/// byte-for-byte against `cast calldata`.
fn encode_reputation_tuple(a: &ReputationAttestation) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(&a.subject);
    out.extend_from_slice(&word_u64(a.score as u64));
    out.extend_from_slice(&word_u64(a.score_decimals as u64));
    out.extend_from_slice(&word_u64(a.valid_until));
    out.extend_from_slice(&word_u64(0xc0));
    out.extend_from_slice(&a.solana_attestation);

    let bytes = a.source_chain.as_bytes();
    out.extend_from_slice(&word_u64(bytes.len() as u64));
    let mut tail = bytes.to_vec();
    let rem = tail.len() % 32;
    if rem != 0 {
        tail.resize(tail.len() + (32 - rem), 0);
    }
    out.extend_from_slice(&tail);
    out
}

/// `digest(tuple)` calldata: selector, offset-to-tuple (single dynamic param),
/// then the encoded tuple.
fn digest_calldata(a: &ReputationAttestation) -> String {
    let mut s = String::from(SEL_DIGEST);
    s.push_str(&hex(&word_u64(0x20)));
    s.push_str(&hex(&encode_reputation_tuple(a)));
    s
}

/// `verify(tuple,v,r,s)` calldata: selector, offset-to-tuple (4 head words →
/// `0x80`), the static `v`/`r`/`s`, then the encoded tuple tail.
fn verify_calldata(signed: &SignedReputationAttestation) -> String {
    let sig = signed.signature();
    let mut s = String::from(SEL_VERIFY);
    s.push_str(&hex(&word_u64(0x80)));
    s.push_str(&hex(&word_u64(sig[64] as u64)));
    s.push_str(&hex(&sig[..32]));
    s.push_str(&hex(&sig[32..64]));
    s.push_str(&hex(&encode_reputation_tuple(signed.attestation())));
    s
}

/// The exact score pinned by `eip712_encoding_is_pinned` in `reputation.rs`,
/// whose digest is `e9a0fe2c…dcccef5`. Past-dated (`validUntil` in 2023).
fn golden() -> ReputationAttestation {
    ReputationAttestation {
        subject: [0xAB; 32],
        score: 9_500,
        score_decimals: 4,
        valid_until: 1_700_003_600,
        source_chain: SOURCE_CHAIN_SOLANA.to_string(),
        solana_attestation: [0x22; 32],
    }
}

/// The same score, dated far in the future so it clears the `Expired` check and
/// the revert isolates the signer gate.
fn live_dated() -> ReputationAttestation {
    ReputationAttestation {
        valid_until: 4_000_000_000,
        ..golden()
    }
}

fn test_attestor() -> Secp256k1IssuerKey {
    Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap()
}

fn assert_rh_mainnet() {
    assert_eq!(
        rpc("eth_chainId", serde_json::json!([]))["result"],
        "0x1237",
        "not Robinhood Chain 4663"
    );
}

/// The deployed registry pins the canonical attestor in its immutable — the same
/// attestor as the bond verifiers, so a production covenantd-signed reputation
/// score verifies on 4663 with no chain-local trust root.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_registry_pins_canonical_attestor() {
    assert_rh_mainnet();
    let r = registry();

    let attestor = eth_call(&r, SEL_ATTESTOR.into());
    let attestor = attestor["result"].as_str().expect("attestor result");
    assert!(
        attestor.to_lowercase().ends_with(CANONICAL_ATTESTOR),
        "registry does not pin the canonical attestor: {attestor}"
    );
}

/// The deployed EIP-712 encoding is byte-identical to this crate's, and the
/// domain separator is chain-agnostic (constant salt) — the same bytes the crate
/// builds and pins, independent of `block.chainid`.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_domain_and_digest_match_crate() {
    assert_rh_mainnet();
    let r = registry();
    let a = golden();

    let onchain_domain = eth_call(&r, SEL_DOMAIN.into());
    let onchain_domain = onchain_domain["result"].as_str().expect("domain result");
    assert_eq!(
        onchain_domain,
        format!("0x{}", hex(&ReputationAttestation::domain_separator())),
        "on-chain domainSeparator() != crate domain_separator()"
    );
    assert_eq!(
        onchain_domain, DOMAIN_SEPARATOR,
        "on-chain domainSeparator drifted from the pinned vector"
    );

    let onchain_digest = eth_call(&r, digest_calldata(&a));
    let onchain_digest = onchain_digest["result"].as_str().expect("digest result");
    assert_eq!(
        onchain_digest,
        format!("0x{}", hex(&a.digest())),
        "on-chain digest() != crate digest()"
    );
    assert_eq!(
        onchain_digest, GOLDEN_DIGEST,
        "on-chain digest drifted from the pinned vector"
    );
}

/// The `ecrecover` precompile on live 4663 recovers the signer of a crate-signed
/// score — the signature is valid on this chain, so the only reason `verify()`
/// can reject it is the attestor gate (next test).
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_ecrecover_precompile_recovers_signer() {
    assert_rh_mainnet();
    let key = test_attestor();
    let signed = golden().sign(&key).unwrap();

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

/// `verify()` reverts `UntrustedSigner` for a live, well-formed, validly-signed
/// score whose signer is a test key rather than the canonical attestor. The
/// score clears every field, malleability, and expiry check and the signature
/// recovers cleanly (previous test), so the revert isolates the issuer-identity
/// gate — a canonical signature is all that is missing.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_verify_enforces_canonical_attestor_gate() {
    assert_rh_mainnet();
    let r = registry();
    let signed = live_dated().sign(&test_attestor()).unwrap();

    let response = eth_call(&r, verify_calldata(&signed));
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

/// `verify()` reverts `Expired` for a past-dated score before it reaches the
/// signer gate — staleness is rejected fail-closed, ahead of signature recovery.
#[test]
#[ignore = "live: hits the Robinhood Chain 4663 RPC"]
fn live_rh_verify_rejects_expired() {
    assert_rh_mainnet();
    let r = registry();
    let signed = golden().sign(&test_attestor()).unwrap();

    let response = eth_call(&r, verify_calldata(&signed));
    assert!(
        response.get("result").is_none(),
        "verify() unexpectedly succeeded for an expired score: {response}"
    );
    let blob = response.to_string();
    assert!(
        blob.contains(ERR_EXPIRED),
        "verify() reverted, but not with Expired: {response}"
    );
}
