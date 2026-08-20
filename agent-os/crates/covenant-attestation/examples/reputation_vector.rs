//! Deterministic golden vector for `CovenantReputationRegistry.sol`: a
//! reputation score this crate signs, emitted for the Solidity parity test and
//! the live 4663 registry check. Fixed attestor key and inputs, so the output
//! never changes.
//!
//!   cargo run -p covenant-attestation --example reputation_vector

use covenant_attestation::{ReputationAttestation, SOURCE_CHAIN_SOLANA};
use covenant_identity::Secp256k1IssuerKey;
use sha3::{Digest, Keccak256};

fn hex0x(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

fn main() {
    let key = Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap();
    let att = ReputationAttestation {
        subject: [0xAB; 32],
        score: 9_500,
        score_decimals: 4,
        valid_until: 1_700_003_600,
        source_chain: SOURCE_CHAIN_SOLANA.to_string(),
        solana_attestation: [0x22; 32],
    };
    let signed = att.sign(&key).unwrap();
    let sig = signed.signature();

    let domain_typehash = keccak(b"EIP712Domain(string name,string version,bytes32 salt)");
    let reputation_typehash = keccak(
        b"Reputation(bytes32 subject,uint32 score,uint8 scoreDecimals,uint64 validUntil,string sourceChain,bytes32 solanaAttestation)",
    );

    println!("attestor            = {}", hex0x(&key.address()));
    println!("subject             = {}", hex0x(&att.subject));
    println!("score               = {}", att.score);
    println!("scoreDecimals       = {}", att.score_decimals);
    println!("validUntil          = {}", att.valid_until);
    println!("sourceChain         = {}", att.source_chain);
    println!("solanaAttestation   = {}", hex0x(&att.solana_attestation));
    println!("domainTypeHash      = {}", hex0x(&domain_typehash));
    println!("reputationTypeHash  = {}", hex0x(&reputation_typehash));
    println!(
        "domainSeparator     = {}",
        hex0x(&ReputationAttestation::domain_separator())
    );
    println!("digest              = {}", hex0x(&signed.digest()));
    println!("r                   = {}", hex0x(&sig[..32]));
    println!("s                   = {}", hex0x(&sig[32..64]));
    println!("v                   = {}", sig[64]);
}
