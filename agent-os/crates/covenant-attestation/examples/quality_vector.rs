//! Deterministic golden vector for `SpendGrantEscrow.sol`: a quality verdict
//! this crate signs, emitted for the Solidity parity test. Fixed attestor key,
//! chain id, and escrow address, so the output never changes.
//!
//!   cargo run -p covenant-attestation --example quality_vector

use covenant_attestation::QualityAttestation;
use covenant_identity::Secp256k1IssuerKey;

fn hex0x(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn main() {
    let key = Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap();
    let mut verifying_contract = [0u8; 20];
    verifying_contract[18] = 0xc0;
    verifying_contract[19] = 0xde;
    let att = QualityAttestation {
        chain_id: 42_161,
        verifying_contract,
        call_id: 42,
        result_hash: [0x11; 32],
        passed: true,
        spec_id: [0x22; 32],
        deadline: 1_700_003_600,
    };
    let signed = att.sign(&key).unwrap();
    let sig = signed.signature();

    println!("attestor            = {}", hex0x(&key.address()));
    println!("chainId             = {}", att.chain_id);
    println!("verifyingContract   = {}", hex0x(&att.verifying_contract));
    println!("callId              = {}", att.call_id);
    println!("resultHash          = {}", hex0x(&att.result_hash));
    println!("passed              = {}", att.passed);
    println!("specId              = {}", hex0x(&att.spec_id));
    println!("deadline            = {}", att.deadline);
    println!("domainSeparator     = {}", hex0x(&att.domain_separator()));
    println!("digest              = {}", hex0x(&signed.digest()));
    println!("r                   = {}", hex0x(&sig[..32]));
    println!("s                   = {}", hex0x(&sig[32..64]));
    println!("v                   = {}", sig[64]);
}
