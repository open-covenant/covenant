//! Deterministic golden vector for `BondReceiptVerifier.sol`: a bond receipt
//! this crate signs, emitted for the Solidity parity test. Fixed issuer key
//! and fields on Base Sepolia, so the output never changes.
//!
//!   cargo run -p covenant-attestation --example bond_vector

use covenant_attestation::{BaseNetwork, BondReceipt};
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
    let network = BaseNetwork::Sepolia;
    let receipt = BondReceipt {
        network,
        subject: [0xAB; 32],
        bond_token: network.usdc(),
        bond_amount: 1_000_000,
        agent_return: [0x11; 20],
        slash_beneficiary: [0x22; 20],
        slash_beneficiary_bps: 8_000,
        nonce: [0xCD; 32],
        issued_at: 1_700_000_000,
        expiry: 1_800_000_000,
    };
    let signed = receipt.sign(&key).unwrap();
    let sig = signed.signature();

    println!("attestor            = {}", hex0x(&key.address()));
    println!("chainId             = {}", network.chain_id());
    println!("usdc                = {}", hex0x(&network.usdc()));
    println!("subject             = {}", hex0x(&receipt.subject));
    println!("bondToken           = {}", hex0x(&receipt.bond_token));
    println!("bondAmount          = {}", receipt.bond_amount);
    println!("agentReturn         = {}", hex0x(&receipt.agent_return));
    println!("slashBeneficiary    = {}", hex0x(&receipt.slash_beneficiary));
    println!("slashBeneficiaryBps = {}", receipt.slash_beneficiary_bps);
    println!("nonce               = {}", hex0x(&receipt.nonce));
    println!("issuedAt            = {}", receipt.issued_at);
    println!("expiry              = {}", receipt.expiry);
    println!("digest              = {}", hex0x(&signed.digest()));
    println!("r                   = {}", hex0x(&sig[..32]));
    println!("s                   = {}", hex0x(&sig[32..64]));
    println!("v                   = {}", sig[64]);
}
