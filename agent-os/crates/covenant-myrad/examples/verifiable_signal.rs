//! End to end over a real Myrad export: group contributions into cohorts, build
//! a receipt for each, sign it, verify it back, and show one contributor proving
//! membership under the evidence root.
//!
//! ```text
//! cargo run -p covenant-myrad --example verifiable_signal
//! cargo run -p covenant-myrad --example verifiable_signal -- signals.json profiles.csv
//! ```
//!
//! With no arguments it runs a synthetic cohort, so the example is
//! self-contained. Given Myrad's signal export (and optionally the profiles CSV,
//! which carries the subject ids and consent flags the payload does not), it
//! runs over the real thing. Those files are contributor data and stay out of
//! the repository.

use covenant_myrad::{
    Contribution, IntegrityPolicy, MerkleTree, SignalReceipt, SignalRecord, SignedReceipt, Status,
};
use ed25519_dalek::SigningKey;

#[path = "shared/reference.rs"]
mod reference;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let records = match args.as_slice() {
        [] => {
            println!("no export given, running a synthetic cohort\n");
            reference::synthetic()
        }
        [signals] => reference::load(signals, None),
        [signals, profiles, ..] => reference::load(signals, Some(profiles)),
    };

    let cohorts = reference::by_cohort(records);
    let attestor = SigningKey::from_bytes(&[42u8; 32]);
    let policy = IntegrityPolicy::default();
    println!("{} cohort(s), min_k = {}\n", cohorts.len(), policy.min_k);

    let mut sellable = 0usize;
    for (cohort_id, records) in &cohorts {
        // What Myrad's pipeline would sell for this cohort. Covenant binds the
        // artifact; it does not compute it.
        let delivered = reference::aggregate(cohort_id, records);

        let receipt = match SignalReceipt::build(&delivered, records, policy) {
            Ok(r) => r,
            Err(e) => {
                println!("{cohort_id}\n  cannot receipt: {e}\n");
                continue;
            }
        };
        let signed = receipt.attest(&attestor).expect("sign");

        println!("{cohort_id}");
        println!(
            "  contributors     {}  evidence root {}…",
            receipt.evidence.contributors,
            &receipt.evidence.merkle_root[..16]
        );
        println!(
            "  delivered        sha256 {}…  covered {}",
            &receipt.signal.delivered_sha256[..16],
            receipt.covers(&delivered)
        );
        println!(
            "  emitted          {} .. {}  activity spans {} month(s), window claims {}",
            short(&receipt.freshness.generated_from),
            short(&receipt.freshness.generated_to),
            receipt.freshness.observed_span_months,
            receipt
                .freshness
                .window_days_claimed
                .map(|d| format!("{d}d"))
                .unwrap_or_else(|| "mixed".into())
        );
        println!("  signature        {}", verdict(signed.verify()));
        println!("  integrity        {:?}", receipt.integrity.status);
        for finding in &receipt.integrity.findings {
            let mark = match finding.status {
                Status::Pass => "  ok  ",
                Status::Warn => " warn ",
                Status::Fail => " FAIL ",
            };
            println!("   {mark}{:<22} {}", finding.check, finding.detail);
        }
        println!("  membership       {}", membership(records, &receipt));

        if receipt.sellable() {
            sellable += 1;
        }
        println!();

        // The buyer's check, on the bytes and the receipt alone.
        let round_tripped: SignedReceipt =
            serde_json::from_str(&serde_json::to_string(&signed).expect("encode")).expect("decode");
        assert!(round_tripped.verify(), "receipt must survive serialization");
        assert!(round_tripped.receipt.covers(&delivered));
    }

    println!(
        "{sellable} of {} cohort(s) clear every blocking check at min_k = {}",
        cohorts.len(),
        policy.min_k
    );
}

/// One contributor proving it is under the evidence root, without the rest of
/// the set being published.
fn membership(records: &[SignalRecord], receipt: &SignalReceipt) -> String {
    let commitments: Vec<String> = records
        .iter()
        .map(|r| {
            Contribution::from_record(r)
                .expect("leaf")
                .commitment_hex()
                .expect("commit")
        })
        .collect();
    let tree = MerkleTree::build(&commitments).expect("tree");
    let proof = tree.inclusion_proof(&commitments[0]).expect("inclusion");
    format!(
        "one contribution proves inclusion in {} step(s): {}",
        proof.path.len(),
        verdict(proof.verify(&receipt.evidence.merkle_root))
    )
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "verifies"
    } else {
        "DOES NOT VERIFY"
    }
}

fn short(ts: &str) -> &str {
    ts.get(..10).unwrap_or(ts)
}
