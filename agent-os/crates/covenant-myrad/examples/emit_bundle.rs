//! Emit the bundle an x402 signal endpoint serves: the cohort artifact and the
//! Covenant receipt issued over it.
//!
//! ```text
//! cargo run -p covenant-myrad --example emit_bundle -- out.json
//! cargo run -p covenant-myrad --example emit_bundle -- out.json signals.json profiles.csv
//! ```
//!
//! The issuer holds the attestor key, so issuance happens here (or in the
//! daemon) and never in the endpoint process.
//!
//! With no export given it emits a synthetic cohort. A real export is
//! contributor data and stays out of the repository.

use std::fs;

use covenant_myrad::{IntegrityPolicy, SignalReceipt};
use ed25519_dalek::SigningKey;
use serde_json::json;

#[path = "shared/reference.rs"]
mod reference;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first() else {
        eprintln!("usage: emit_bundle <out.json> [signals.json [profiles.csv]]");
        std::process::exit(2);
    };

    let records = match args.get(1) {
        Some(signals) => reference::load(signals, args.get(2).map(String::as_str)),
        None => reference::synthetic(),
    };
    assert!(!records.is_empty(), "no contributions to issue over");

    // The largest cohort in the export: the one most likely to clear min_k.
    let (cohort_id, records) = reference::by_cohort(records)
        .into_iter()
        .max_by_key(|(_, records)| records.len())
        .expect("at least one cohort");

    let delivered = reference::aggregate(&cohort_id, &records);

    // Demo key. Production issuance uses the Covenant attestor the daemon
    // custodies, and the buyer pins its public half out of band.
    let attestor = SigningKey::from_bytes(&[42u8; 32]);
    let receipt = SignalReceipt::build(&delivered, &records, IntegrityPolicy::default())
        .expect("build receipt");
    let signed = receipt.attest(&attestor).expect("sign receipt");

    let bundle = json!({
        "resource": "covenant.myrad.signal.bundle.v1",
        "signal": delivered,
        "receipt": signed,
        "verify": {
            "steps": [
                "recompute sha256 over the RFC 8785 canonical form of `signal`; it must equal receipt.receipt.signal.delivered_sha256",
                "recompute sha256 over the RFC 8785 canonical form of `receipt.receipt`; it must equal receipt.root_hash_hex",
                "check receipt.signature_b64 over the ASCII of receipt.root_hash_hex against a pinned attestor key",
                "read receipt.receipt.integrity: `fail` means the cohort did not clear a blocking check"
            ]
        }
    });

    fs::write(
        out,
        serde_json::to_string_pretty(&bundle).expect("encode bundle"),
    )
    .unwrap_or_else(|e| panic!("write {out}: {e}"));

    let contributors = signed.receipt.evidence.contributors;
    let status = signed.receipt.integrity.status;
    println!("{out}: cohort {cohort_id}, {contributors} contributor(s), integrity {status:?}");
}
