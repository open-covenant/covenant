//! Run the payout gate end to end against a static oracle and mint a signed
//! receipt for each decision. No network, no Loofta API: this exercises the
//! decision half the daemon wires Loofta into.
//!
//!   cargo run -p covenant-loofta --example payout_gate

use covenant_loofta::{
    Decision, EnclaveAttestation, PaymentRecord, PayoutPolicy, PayoutReceipt, RecipientOracle,
    RecipientStanding, StaticOracle,
};
use ed25519_dalek::SigningKey;

const SRC: &str = "covenant-reputation (mainnet)";

#[tokio::main]
async fn main() {
    let policy = PayoutPolicy::conservative();
    let attestor = SigningKey::from_bytes(&[7u8; 32]);

    let cases = [
        (
            "established, clean, $120",
            RecipientStanding {
                recipient: "Es7mSde1oW9r3wJ5vY2kQ8hX4bN6tG1pZ0cV7uM2aD3f".into(),
                score: 82,
                established: true,
                flagged: false,
                source: SRC.into(),
            },
            120.0,
        ),
        (
            "fresh recipient, $20",
            RecipientStanding::unknown("Fr8nQw2eR4tY6uI8oP0aS1dF3gH5jK7lZ9xC2vB4nM6q", SRC),
            20.0,
        ),
        (
            "fresh recipient, $400",
            RecipientStanding::unknown("Fr8nQw2eR4tY6uI8oP0aS1dF3gH5jK7lZ9xC2vB4nM6q", SRC),
            400.0,
        ),
        (
            "flagged, $10",
            RecipientStanding {
                recipient: "FL4gGeD1x9c8v7b6n5m4k3j2h1g0f9d8s7a6p5o4i3u2".into(),
                score: 0,
                established: true,
                flagged: true,
                source: SRC.into(),
            },
            10.0,
        ),
    ];

    for (label, standing, amount) in cases {
        let oracle = StaticOracle(standing);
        let standing = oracle.standing("").await.unwrap();
        let verdict = policy.authorize(&standing, amount);
        let signed = PayoutReceipt::new(&standing.recipient, amount, &verdict, &standing.source)
            .attest(&attestor)
            .unwrap();
        println!(
            "{label:<28} -> {verdict:?}\n  receipt {}… verifies={}",
            &signed.root_hash_hex[..16],
            signed.verify()
        );
    }

    // A settled payment becomes a privacy-preserving provenance record: only the
    // commitment (a hash) is anchored, and the payer opens the record to prove
    // it later. The nonce blinds the commitment; the daemon supplies real random
    // bytes.
    println!("\n-- provenance for a settled payment --");
    let record = PaymentRecord::new(
        "loofta-pay-8f2c1a",
        "Es7mSde1oW9r3wJ5vY2kQ8hX4bN6tG1pZ0cV7uM2aD3f",
        120.0,
        Decision::Allowed,
        1_722_600_000,
        &[42u8; 32],
    );
    let signed = record.attest(&attestor).unwrap();
    let commitment = signed.commitment_hex.clone();

    // The daemon attaches a live DCAP verification of the MagicBlock Private ER
    // (Intel TDX enclave) the payment ran in. Shown here with the real mainnet
    // verified-ER validator and a report_data that binds this commitment; the
    // mr_td is illustrative (the real value comes from the live verify).
    let binding = covenant_tee::Binding::new(
        "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo",
        &commitment,
        "11".repeat(32),
    )
    .expect("valid binding");
    let signed = signed.with_enclave(EnclaveAttestation {
        validator: "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo".into(),
        tcb_status: "UpToDate".into(),
        mr_td_hex: "ab".repeat(48),
        advisory_ids: vec![],
        report_data_hex: binding.challenge_hex(),
        verified_at: 1_722_600_050,
        binding,
    });

    println!("commitment {}… (nothing else anchored)", &commitment[..16]);
    println!(
        "signature verifies={}, record opens it={}",
        signed.verify(),
        signed.opens(&record)
    );
    let e = signed.enclave.as_ref().unwrap();
    println!(
        "enclave: validator={} tcb={} -> enclave_proven={}",
        e.validator,
        e.tcb_status,
        signed.enclave_proven()
    );
}
