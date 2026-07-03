//! End-to-end behavior over the public API, and the cross-crate
//! invariant the whole design turns on: the credential names the exact
//! audit root `covenant-audit` produces — the same value the sap-bridge
//! anchors on Solana. The root here comes from a real `covenant-audit`
//! chain, so this is a genuine link, not a hand-made hex.

use covenant_attestation::{
    AttestationInput, AttestationIssuer, StatusPointer, VerifiableCredential,
};
use covenant_audit::{AuditEvent, AuditKind, AuditLog, InMemoryAuditLog};
use covenant_types::AgentId;
use uuid::Uuid;

fn input(audit_root_hex: &str) -> AttestationInput {
    AttestationInput {
        credential_id: "urn:uuid:9f1c8b2a-0000-4000-8000-000000000001".into(),
        subject_id: "did:covenant:agent:research".into(),
        audit_root_hex: audit_root_hex.into(),
        created_unix: 1_700_000_000,
        valid_from_unix: 1_700_000_000,
        valid_until_unix: 1_800_000_000,
        status: Some(StatusPointer {
            id: "https://covenant.dev/status/1#42".into(),
            status_purpose: "revocation".into(),
            status_list_index: "42".into(),
            status_list_credential: "https://covenant.dev/status/1".into(),
        }),
    }
}

/// Drive a real in-memory audit chain and return the root the daemon
/// would anchor: `verify_integrity().root_hash_hex`.
async fn anchored_audit_root() -> String {
    let log = InMemoryAuditLog::new();
    log.record(AuditEvent {
        id: Uuid::new_v4(),
        timestamp_ms: 0,
        issuer: AgentId::new("issuer@covenant", [0u8; 32]),
        kind: AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "settle completed task".into(),
            matched_agent: Some("research".into()),
            result_hash_hex: covenant_audit::hash_hex(b"result"),
            status: "ok".into(),
        },
    })
    .await
    .unwrap();

    let report = log.verify_integrity().await.unwrap();
    assert!(report.valid);
    report.root_hash_hex
}

#[tokio::test]
async fn credential_binds_a_real_covenant_audit_root() {
    let root = anchored_audit_root().await;
    let issuer = AttestationIssuer::generate();
    let vc = issuer.issue(&input(&root)).unwrap();

    let report = vc.verify().expect("both proofs verify");

    // The off-chain credential and the on-chain anchor name the same bytes.
    assert_eq!(vc.audit_root_hex().unwrap(), root);
    assert_eq!(report.audit_root_hex, root);
    assert_eq!(report.issuer_address, issuer.issuer_address());
}

#[tokio::test]
async fn verification_survives_a_json_transport_round_trip() {
    let root = anchored_audit_root().await;
    let vc = AttestationIssuer::generate().issue(&input(&root)).unwrap();

    // Serialize, ship as text, parse, verify — the path a relying party takes.
    let received = VerifiableCredential::from_json(&vc.to_json()).unwrap();
    let report = received
        .verify()
        .expect("both proofs verify after transport");
    assert_eq!(report.audit_root_hex, root);
    assert_eq!(
        received.canonical_hash_hex().unwrap(),
        vc.canonical_hash_hex().unwrap()
    );
}
