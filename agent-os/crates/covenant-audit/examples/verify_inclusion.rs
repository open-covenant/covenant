//! Offline verifier for an `AuditInclusionProof`. A third party can confirm a
//! generation's provenance is folded into the audit root WITHOUT trusting the
//! daemon, then compare `computed_root_hash_hex` against the SAP on-chain
//! anchor to close the chain of custody.
//!
//!   curl -s "$DAEMON/audit/inclusion/$EVENT_ID" -H "authorization: Bearer …" \
//!     | jq .proof > proof.json
//!   cargo run -p covenant-audit --example verify_inclusion -- proof.json

use covenant_audit::{verify_inclusion_proof, AuditInclusionProof};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: verify_inclusion <proof.json>");
    let bytes = std::fs::read(&path).expect("read proof file");
    let proof: AuditInclusionProof =
        serde_json::from_slice(&bytes).expect("parse AuditInclusionProof json");

    let check = verify_inclusion_proof(&proof);
    println!("event_id:      {}", proof.event_id);
    println!("leaf_index:    {}", proof.leaf_index);
    println!("events_after:  {}", proof.suffix_event_hashes_hex.len());
    println!("claimed root:  {}", proof.root_hash_hex);
    println!("computed root: {}", check.computed_root_hash_hex);
    println!("valid:         {}", check.valid);
    if let Some(reason) = check.reason {
        println!("reason:        {reason}");
        std::process::exit(1);
    }
}
