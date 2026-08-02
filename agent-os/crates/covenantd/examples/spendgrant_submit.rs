//! Emit the daemon's `SpendGrantEscrow` submission for one call: the escrow
//! address and the ABI-encoded `releaseCallAttested`/`refundCallAttested`
//! calldata, chosen by the verdict. This is `SpendGrantConfig::submission` —
//! the daemon signs and encodes; any funded submitter (`cast send $TO
//! $CALLDATA`, an RPC signer later) broadcasts. Inputs ride the environment,
//! same set as `spendgrant_sign`:
//!
//!   SG_KEY SG_CHAIN SG_ESCROW SG_CALLID SG_RESULTHASH SG_PASSED SG_SPECID SG_DEADLINE
//!
//! Prints `<to> <calldata>` (both `0x`-hex) on stdout; the recovered signer on
//! stderr.

use std::path::Path;

use covenantd::escrow::CompletionProof;
use covenantd::spend_grant::{SpendGrantAttestor, SpendGrantConfig};
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
    let config = SpendGrantConfig::new(
        attestor,
        env("SG_CHAIN").parse().unwrap(),
        hex_bytes::<20>(&env("SG_ESCROW")),
    );
    let passed: bool = env("SG_PASSED").parse().unwrap();
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
    let s = config
        .settle(
            &proof,
            env("SG_CALLID").parse().unwrap(),
            hex_bytes::<32>(&env("SG_SPECID")),
            env("SG_DEADLINE").parse().unwrap(),
        )
        .unwrap();
    eprintln!(
        "daemon {} verdict, attestor 0x{}",
        if s.release {
            "PASS -> release"
        } else {
            "FAIL -> refund"
        },
        hex_str(&s.attestor)
    );
    println!("0x{} 0x{}", hex_str(&s.to), hex_str(&s.calldata));
}
