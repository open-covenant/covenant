//! Produce and verify a real Covenant audit-root provenance attestation on the
//! Base **mainnet** EAS domain, from the operator's own keys and audit chain.
//!
//!   COVENANT_EVM_ISSUER_KEY=~/.covenant/evm-issuer.key \
//!     cargo run -p covenant-evm-signer --example provenance_attest
//!
//! It reads the real ed25519 identity, the real secp256k1 issuer, and the real
//! audit root (the last chain hash of `~/.covenant/audit/events.chain.jsonl`),
//! issues a verifiable credential over that root, projects it as an EAS
//! off-chain attestation under the Base mainnet domain (version 1.0.1), and
//! confirms `ecrecover` over the recomputed digest returns the issuer address.
//! Nothing is broadcast; the attestation is a signed statement anyone can check.

use std::path::PathBuf;

use covenant_attestation::{AttestationInput, AttestationIssuer};
use covenant_evm_signer::EasAttestationSigner;
use covenant_identity::{LocalIdentity, Secp256k1IssuerKey};

fn hex_0x(bytes: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn home(rel: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME")).join(rel)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// The current audit root: the `chain_hash_hex` of the last event in the
/// hash-chained log.
fn audit_root_hex() -> String {
    let path = home(".covenant/audit/events.chain.jsonl");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let last = text
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .expect("audit chain is empty");
    let v: serde_json::Value = serde_json::from_str(last).expect("audit line is json");
    v["chain_hash_hex"]
        .as_str()
        .expect("chain_hash_hex")
        .to_string()
}

fn main() {
    let issuer_path = std::env::var("COVENANT_EVM_ISSUER_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home(".covenant/evm-issuer.key"));
    let issuer = Secp256k1IssuerKey::load_or_create(&issuer_path).expect("load issuer key");
    let identity =
        LocalIdentity::load_or_create(&home(".covenant/identity/local.key"), "covenant-agent")
            .expect("load ed25519 identity");

    let root = audit_root_hex();
    let issued = now_unix();
    let input = AttestationInput {
        credential_id: format!("urn:uuid:{}", uuid_from_root(&root)),
        subject_id: format!("did:pkh:eip155:8453:{}", hex_0x(&issuer.address())),
        audit_root_hex: root.clone(),
        created_unix: issued,
        valid_from_unix: issued,
        valid_until_unix: issued + 31_536_000, // one year
        status: None,
    };

    let vc = AttestationIssuer::new(identity.signing_key().clone(), issuer.clone())
        .issue(&input)
        .expect("issue vc");
    let attestation = EasAttestationSigner::base_mainnet(issuer.clone())
        .attest(&vc)
        .expect("attest");

    let recovered = attestation.recover_signer().expect("recover");
    let issuer_addr = issuer.address();

    println!("== Covenant audit-root provenance attestation (Base mainnet EAS) ==");
    println!("audit root:        {root}");
    println!("issuer address:    {}", hex_0x(&issuer_addr));
    println!("recovered address: {}", hex_0x(&recovered));
    println!(
        "ecrecover:         {}",
        if recovered == issuer_addr {
            "MATCH (attestation verifies to the issuer)"
        } else {
            "MISMATCH"
        }
    );
    println!("\n{}", attestation.to_json_string());
    assert_eq!(
        recovered, issuer_addr,
        "attestation must recover the issuer"
    );
}

/// A stable UUID-shaped id derived from the root, so re-running over the same
/// root yields the same credential id (no wall-clock randomness).
fn uuid_from_root(root: &str) -> String {
    let h = &root.as_bytes()[..32.min(root.len())];
    let s: String = h.iter().map(|b| *b as char).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}
