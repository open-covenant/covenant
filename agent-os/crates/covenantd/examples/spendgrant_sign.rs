//! Sign a Quality verdict for one call of a deployed `SpendGrantEscrow` — the
//! daemon's spec-gate leg. Prints the 65-byte `r‖s‖v` signature (`0x` hex) that
//! `releaseCallAttested` (passed=true) or `refundCallAttested` (passed=false)
//! authenticates with one `ecrecover`. Inputs ride the environment so a demo
//! script can drive the real attestor without an arg parser:
//!
//!   SG_KEY SG_CHAIN SG_ESCROW SG_CALLID SG_RESULTHASH SG_PASSED SG_SPECID SG_DEADLINE

use std::path::Path;

use covenantd::escrow::CompletionProof;
use covenantd::spend_grant::{QualityBinding, SpendGrantAttestor};
use uuid::Uuid;

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("missing env {k}"))
}

fn hex_bytes<const N: usize>(s: &str) -> [u8; N] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    assert_eq!(s.len(), 2 * N, "expected {N} bytes of hex");
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex digit");
    }
    out
}

fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let attestor = SpendGrantAttestor::load_or_create(Path::new(&env("SG_KEY"))).unwrap();
    let passed: bool = env("SG_PASSED").parse().unwrap();
    let binding = QualityBinding {
        chain_id: env("SG_CHAIN").parse().unwrap(),
        verifying_contract: hex_bytes::<20>(&env("SG_ESCROW")),
        call_id: env("SG_CALLID").parse().unwrap(),
        spec_id: hex_bytes::<32>(&env("SG_SPECID")),
        deadline: env("SG_DEADLINE").parse().unwrap(),
    };
    let proof = CompletionProof {
        proof_id: Uuid::nil(),
        escrow_id: "demo".into(),
        job_id: Uuid::nil(),
        hirer_address: String::new(),
        worker_address: String::new(),
        amount: String::new(),
        asset: "USDG".into(),
        network: "robinhood-testnet".into(),
        provider: String::new(),
        result_hash_hex: env("SG_RESULTHASH"),
        validation_passed: passed,
        audit_root_hex: "00".repeat(32),
        proven_at: 0,
    };
    let signed = attestor.attest(&proof, &binding).unwrap();
    eprintln!(
        "attestor {} verdict recovers to 0x{}",
        if passed { "PASS" } else { "FAIL" },
        hex_str(&signed.recover_signer().unwrap())
    );
    println!("0x{}", hex_str(signed.signature()));
}
