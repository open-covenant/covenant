//! Conformance golden vectors for the full audit record wire schema.
//!
//! `tests/fixtures/audit-kinds.v1.json` freezes, for exactly one representative
//! [`AuditEvent`] per [`AuditKind`] variant, the canonical `serde_json` wire
//! form — the compact byte sequence hashed into the tamper-evident chain — and
//! its SHA-256 `event_hash_hex`. Every recognized audit kind has one frozen
//! vector, so a change to any event's fields, field ordering, the kind
//! discriminator, or the `AgentId` envelope fails loudly here instead of
//! silently changing that event's hash and breaking every downstream verifier.
//!
//! This is the per-variant *surface* freeze. The genesis-seeded chain fold over
//! an ordered sequence is frozen separately by the provenance record contract in
//! `tests/fixtures/provenance-records.v1.json` (see the inline
//! `provenance_record_schema_golden_vectors_are_frozen` test in `src/lib.rs`).
//!
//! [`audit_kind_slug`] is the compile-time exhaustiveness tripwire: a `match`
//! with no wildcard arm, so adding an [`AuditKind`] variant fails to compile
//! until a slug — and therefore a golden vector in `golden_audit_kind_records`
//! plus an entry in `EXPECTED_KINDS` — is added for it.
//!
//! Regenerate deliberately, only after reviewing the resulting diff:
//!
//! ```text
//! COVENANT_BLESS_AUDIT_KINDS_GOLDEN=1 cargo test -p covenant-audit \
//!   --test golden_audit_kinds audit_kind_wire_schema_golden_vectors_are_frozen
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use covenant_audit::{hash_hex, AuditEvent, AuditKind, CapabilityAuthorization, ToolCallOutcome};
use covenant_types::AgentId;
use uuid::Uuid;

/// Set to (re)generate the committed fixture instead of asserting against it.
const BLESS_ENV: &str = "COVENANT_BLESS_AUDIT_KINDS_GOLDEN";

const DRIFT: &str = "audit record schema/hash drift: \
    tests/fixtures/audit-kinds.v1.json is a frozen external conformance contract. \
    A mismatch means the AuditEvent wire shape, field ordering, kind discriminator, \
    AgentId envelope, or hashing changed and every downstream verifier would break. \
    Update the fixture only as a deliberate, reviewed schema change (bump the .v<n> \
    suffix for an incompatible shape) — never blindly regenerate to silence this test.";

/// The committed fixture's `description`. Code-pinned so the human-facing
/// contract note has a single source of truth: a hand-edit to the fixture's
/// prose fails the suite, and a reword here forces a re-bless.
const DESCRIPTION: &str = "Frozen golden vectors for the full audit record wire schema: one \
    representative AuditEvent per AuditKind variant. canonical_json is the exact \
    serde_json::to_string wire form of the event — the byte sequence hashed into the \
    tamper-evident chain — and event_hash_hex is sha256(canonical_json). Every \
    recognized audit kind has one vector; covenant-audit's audit_kind_slug match is \
    the compile-time tripwire that keeps this corpus exhaustive. This is an external \
    conformance contract other implementations verify against: a change here changes \
    the on-chain event hash and breaks every downstream verifier. Update only as a \
    deliberate, reviewed schema change (bump the .v<n> suffix for an incompatible \
    shape) — never blindly regenerate to make a failing test pass. The genesis-seeded \
    chain fold over an ordered sequence is frozen separately in provenance-records.v1.json.";

/// Every recognized audit kind discriminator, in [`AuditKind`] declaration
/// order. [`audit_kind_slug`]'s wildcard-free `match` is the compile-time
/// tripwire that keeps this list — and the golden corpus — exhaustive.
const EXPECTED_KINDS: &[&str] = &[
    "intent_dispatched",
    "hermes_tool_invoked",
    "hermes_tool_completed",
    "hermes_approval_requested",
    "hermes_approval_resolved",
    "hermes_file_written",
    "capability_check",
    "capability_granted",
    "capability_grant_rejected",
    "capability_scope_rejected",
    "secret_access_granted",
    "secret_access_denied",
    "intent_ignored",
    "a2_a_result_rejected",
    "authentication_failed",
    "a2_a_sender_mismatch",
    "a2_a_recipient_rejected",
    "a2_a_repair_applied",
    "a2_a_auto_retry_scheduler_scan",
    "cross_host_a2_a_admission",
    "cross_host_a2_a_delivery",
    "memory_repair_applied",
    "memory_compaction_applied",
    "capability_revoked",
    "capability_revoke_rejected",
    "budget_exhausted",
    "budget_preempted",
    "budget_preempt_failed",
    "budget_unseeded",
    "operator_token_rotated",
    "operator_token_rotation_rejected",
    "operator_peers_list_rejected",
    "peer_revoked",
    "operator_peer_revoke_rejected",
    "peer_self_revoke_blocked",
    "external_payment_settled",
    "settlement_receipt_backfill_applied",
    "memory_record_backfill_applied",
    "tool_call_completed",
    "tool_call_schema_rejected",
    "capability_budget_exhausted",
];

/// The serde discriminator for one audit kind. A wildcard-free `match`: adding
/// an [`AuditKind`] variant fails to compile here, forcing a slug (and a golden
/// vector in `golden_audit_kind_records` plus an entry in `EXPECTED_KINDS`) for
/// the new kind. The returned slug must equal the serialized `type` tag; the
/// suite asserts that for every record so a mis-slugged arm is caught.
fn audit_kind_slug(kind: &AuditKind) -> &'static str {
    match kind {
        AuditKind::IntentDispatched { .. } => "intent_dispatched",
        AuditKind::HermesToolInvoked { .. } => "hermes_tool_invoked",
        AuditKind::HermesToolCompleted { .. } => "hermes_tool_completed",
        AuditKind::HermesApprovalRequested { .. } => "hermes_approval_requested",
        AuditKind::HermesApprovalResolved { .. } => "hermes_approval_resolved",
        AuditKind::HermesFileWritten { .. } => "hermes_file_written",
        AuditKind::CapabilityCheck { .. } => "capability_check",
        AuditKind::CapabilityGranted { .. } => "capability_granted",
        AuditKind::CapabilityGrantRejected { .. } => "capability_grant_rejected",
        AuditKind::CapabilityScopeRejected { .. } => "capability_scope_rejected",
        AuditKind::SecretAccessGranted { .. } => "secret_access_granted",
        AuditKind::SecretAccessDenied { .. } => "secret_access_denied",
        AuditKind::IntentIgnored { .. } => "intent_ignored",
        AuditKind::A2AResultRejected { .. } => "a2_a_result_rejected",
        AuditKind::AuthenticationFailed { .. } => "authentication_failed",
        AuditKind::A2ASenderMismatch { .. } => "a2_a_sender_mismatch",
        AuditKind::A2ARecipientRejected { .. } => "a2_a_recipient_rejected",
        AuditKind::A2ARepairApplied { .. } => "a2_a_repair_applied",
        AuditKind::A2AAutoRetrySchedulerScan { .. } => "a2_a_auto_retry_scheduler_scan",
        AuditKind::CrossHostA2AAdmission { .. } => "cross_host_a2_a_admission",
        AuditKind::CrossHostA2ADelivery { .. } => "cross_host_a2_a_delivery",
        AuditKind::MemoryRepairApplied { .. } => "memory_repair_applied",
        AuditKind::MemoryCompactionApplied { .. } => "memory_compaction_applied",
        AuditKind::CapabilityRevoked { .. } => "capability_revoked",
        AuditKind::CapabilityRevokeRejected { .. } => "capability_revoke_rejected",
        AuditKind::BudgetExhausted { .. } => "budget_exhausted",
        AuditKind::BudgetPreempted { .. } => "budget_preempted",
        AuditKind::BudgetPreemptFailed { .. } => "budget_preempt_failed",
        AuditKind::BudgetUnseeded { .. } => "budget_unseeded",
        AuditKind::OperatorTokenRotated { .. } => "operator_token_rotated",
        AuditKind::OperatorTokenRotationRejected { .. } => "operator_token_rotation_rejected",
        AuditKind::OperatorPeersListRejected { .. } => "operator_peers_list_rejected",
        AuditKind::PeerRevoked { .. } => "peer_revoked",
        AuditKind::OperatorPeerRevokeRejected { .. } => "operator_peer_revoke_rejected",
        AuditKind::PeerSelfRevokeBlocked { .. } => "peer_self_revoke_blocked",
        AuditKind::ExternalPaymentSettled { .. } => "external_payment_settled",
        AuditKind::SettlementReceiptBackfillApplied { .. } => "settlement_receipt_backfill_applied",
        AuditKind::MemoryRecordBackfillApplied { .. } => "memory_record_backfill_applied",
        AuditKind::ToolCallCompleted { .. } => "tool_call_completed",
        AuditKind::ToolCallSchemaRejected { .. } => "tool_call_schema_rejected",
        AuditKind::CapabilityBudgetExhausted { .. } => "capability_budget_exhausted",
    }
}

fn event(seq: u128, kind: AuditKind) -> AuditEvent {
    AuditEvent {
        id: Uuid::from_u128(seq),
        timestamp_ms: 1_700_000_000_000 + seq as u64,
        issuer: AgentId::new("operator@covenant", [7u8; 32]),
        kind,
    }
}

/// One representative event per audit kind, in declaration order. Optional
/// fields are set to `Some`/non-empty where that exercises the richer wire
/// shape, so the freeze pins the present-field form a verifier must accept.
fn golden_audit_kind_records() -> Vec<AuditEvent> {
    vec![
        event(
            1,
            AuditKind::IntentDispatched {
                intent_id: Uuid::from_u128(0xA1),
                intent_text: "summarize the q3 report".into(),
                matched_agent: Some("research@agent".into()),
                result_hash_hex: hash_hex(b"intent-result"),
                status: "ok".into(),
            },
        ),
        event(
            2,
            AuditKind::HermesToolInvoked {
                intent_id: Uuid::from_u128(0xA2),
                run_id: "run-7c1f".into(),
                tool: "fs.write".into(),
                preview_hash_hex: hash_hex(b"tool-input-preview"),
            },
        ),
        event(
            3,
            AuditKind::HermesToolCompleted {
                intent_id: Uuid::from_u128(0xA3),
                run_id: "run-7c1f".into(),
                tool: "fs.write".into(),
                duration_ms: 1234,
                error: false,
            },
        ),
        event(
            4,
            AuditKind::HermesApprovalRequested {
                intent_id: Uuid::from_u128(0xA4),
                run_id: "run-7c1f".into(),
                choices: vec!["approve".into(), "deny".into()],
            },
        ),
        event(
            5,
            AuditKind::HermesApprovalResolved {
                intent_id: Uuid::from_u128(0xA5),
                run_id: "run-7c1f".into(),
                choice: "approve".into(),
                resolved: 1,
            },
        ),
        event(
            6,
            AuditKind::HermesFileWritten {
                intent_id: Uuid::from_u128(0xA6),
                run_id: "run-7c1f".into(),
                path: "src/main.rs".into(),
                bytes: 2048,
            },
        ),
        event(
            7,
            AuditKind::CapabilityCheck {
                agent_id: "research@agent".into(),
                required_actions: vec!["memory.write".into(), "a2a.send".into()],
                missing_actions: vec!["a2a.send".into()],
                passed: false,
                authorized_by: vec![CapabilityAuthorization {
                    action: "memory.write".into(),
                    granted_by_display: "operator@covenant".into(),
                    signature_b58: "GrantSigWrite".into(),
                }],
            },
        ),
        event(
            8,
            AuditKind::CapabilityGranted {
                subject_display: "research@agent".into(),
                action: "memory.write".into(),
                granted_by_display: "operator@covenant".into(),
                signature_b58: "GrantSigWrite".into(),
            },
        ),
        event(
            9,
            AuditKind::CapabilityGrantRejected {
                subject_display: "research@agent".into(),
                action: "memory.write".into(),
                reason: "capability expired".into(),
            },
        ),
        event(
            10,
            AuditKind::CapabilityScopeRejected {
                agent_id: "research@agent".into(),
                action: "audit.purge".into(),
                reason: "before_ms exceeds granted scope".into(),
            },
        ),
        event(
            11,
            AuditKind::SecretAccessGranted {
                agent_id: "research@agent".into(),
                secret_name: "openai-api-key".into(),
                signature_b58: "GrantSigSecret".into(),
            },
        ),
        event(
            12,
            AuditKind::SecretAccessDenied {
                agent_id: "research@agent".into(),
                secret_name: "stripe-key".into(),
                reason: "scope does not admit secret name".into(),
            },
        ),
        event(
            13,
            AuditKind::IntentIgnored {
                intent_id: Uuid::from_u128(0xB1),
                intent_text: "thanks!".into(),
                matched_pattern: "^thanks".into(),
            },
        ),
        event(
            14,
            AuditKind::A2AResultRejected {
                task_id: Uuid::from_u128(0xB2),
                reason: "task_id was never dispatched through this daemon".into(),
            },
        ),
        event(
            15,
            AuditKind::AuthenticationFailed {
                transport: "ipc".into(),
                reason: "unknown peer token".into(),
            },
        ),
        event(
            16,
            AuditKind::A2ASenderMismatch {
                peer_display: "peer@remote".into(),
                claimed_sender_display: "other@remote".into(),
            },
        ),
        event(
            17,
            AuditKind::A2ARecipientRejected {
                sender_display: "peer@remote".into(),
                recipient_display: "research@agent".into(),
                action: "a2a.recv.peer@remote".into(),
            },
        ),
        event(
            18,
            AuditKind::A2ARepairApplied {
                task_id: Uuid::from_u128(0xB3),
                action: "requeue".into(),
                reason: "operator repair".into(),
                lease_id: Some(Uuid::from_u128(0xB4)),
                duplicate_risk: Some("idempotent".into()),
                attempt: 2,
            },
        ),
        event(
            19,
            AuditKind::A2AAutoRetrySchedulerScan {
                enabled: true,
                considered: 5,
                requeued: 2,
                skipped: 3,
                skipped_by_reason: BTreeMap::from([
                    ("lease_too_young".to_string(), 2u64),
                    ("max_attempts".to_string(), 1u64),
                ]),
                min_lease_age_ms: 60_000,
                max_attempts: 5,
                max_requeues: 3,
                scan_limit: 100,
                error: None,
            },
        ),
        event(
            20,
            AuditKind::MemoryRepairApplied {
                memory_id: Uuid::from_u128(0xC1),
                action: "delete".into(),
                mode: "apply".into(),
                changed: true,
                reason: "operator repair".into(),
            },
        ),
        event(
            21,
            AuditKind::MemoryCompactionApplied {
                mode: "apply".into(),
                changed: true,
                reason: "scheduled compaction".into(),
                deleted: vec![Uuid::from_u128(0xC2)],
                stale_marked: vec![Uuid::from_u128(0xC3)],
                parents_detached: vec![Uuid::from_u128(0xC4)],
            },
        ),
        event(
            22,
            AuditKind::CapabilityRevoked {
                subject_display: "research@agent".into(),
                action: "memory.write".into(),
                signature_b58: "GrantSigWrite".into(),
                removed: true,
            },
        ),
        event(
            23,
            AuditKind::CapabilityRevokeRejected {
                signature_b58: "OtherSig".into(),
                reason: "not the capability subject".into(),
            },
        ),
        event(
            24,
            AuditKind::BudgetExhausted {
                agent_display: "research@agent".into(),
                intent_id: Uuid::from_u128(0xD1),
                intent_text: "expensive task".into(),
                requested: 1000,
                tokens_remaining: 250,
                refill_eta_ms: 45_000,
            },
        ),
        event(
            25,
            AuditKind::BudgetPreempted {
                agent_display: "research@agent".into(),
                intent_id: Uuid::from_u128(0xD2),
                reason: "budget exceeded mid-run".into(),
                signal_sent: "SIGTERM".into(),
                exit_code: Some(143),
            },
        ),
        event(
            26,
            AuditKind::BudgetPreemptFailed {
                agent_display: "research@agent".into(),
                intent_id: Uuid::from_u128(0xD3),
                reason: "signal syscall failed".into(),
                errno: 1,
            },
        ),
        event(
            27,
            AuditKind::BudgetUnseeded {
                agent_display: "research@agent".into(),
                intent_id: Uuid::from_u128(0xD4),
                requested: 1000,
            },
        ),
        event(
            28,
            AuditKind::OperatorTokenRotated {
                peer_display: "operator@local".into(),
                old_token_prefix: "abc123".into(),
                new_token_prefix: "def456".into(),
            },
        ),
        event(
            29,
            AuditKind::OperatorTokenRotationRejected {
                peer_display: "guest@remote".into(),
                peer_pubkey_b58: "11111111111111111111111111111111".into(),
            },
        ),
        event(
            30,
            AuditKind::OperatorPeersListRejected {
                peer_display: "guest@remote".into(),
                peer_pubkey_b58: "11111111111111111111111111111111".into(),
            },
        ),
        event(
            31,
            AuditKind::PeerRevoked {
                peer_display: "guest@remote".into(),
                peer_pubkey_b58: "11111111111111111111111111111111".into(),
                token_prefix: "ghi789".into(),
            },
        ),
        event(
            32,
            AuditKind::OperatorPeerRevokeRejected {
                peer_display: "guest@remote".into(),
                peer_pubkey_b58: "11111111111111111111111111111111".into(),
            },
        ),
        event(
            33,
            AuditKind::PeerSelfRevokeBlocked {
                peer_display: "operator@local".into(),
                peer_pubkey_b58: "22222222222222222222222222222222".into(),
                token_prefix: "jkl012".into(),
            },
        ),
        event(
            34,
            AuditKind::ExternalPaymentSettled {
                provider: "xona".into(),
                endpoint: "https://api.example.com/x402".into(),
                network: "solana".into(),
                asset: "USDC".into(),
                amount: "1000000".into(),
                receipt_id: Uuid::from_u128(0xE1),
            },
        ),
        event(
            35,
            AuditKind::SettlementReceiptBackfillApplied {
                row_count: 12,
                rollback_path: Some("/var/covenant/receipts.rollback.jsonl".into()),
                dry_run: false,
            },
        ),
        event(
            36,
            AuditKind::MemoryRecordBackfillApplied {
                row_count: 8,
                savepoint_name: Some("backfill_receipt_correlation".into()),
                dry_run: false,
            },
        ),
        event(
            37,
            AuditKind::ToolCallCompleted {
                tool: "echo".into(),
                arguments_hash_hex: hash_hex(b"{\"text\":\"hi\"}"),
                duration_ms: 5,
                outcome: ToolCallOutcome::Ok,
            },
        ),
        event(
            38,
            AuditKind::ToolCallSchemaRejected {
                tool: "echo".into(),
                arguments_hash_hex: hash_hex(b"{\"text\":42}"),
                reason: "field `text` expects type `string`, got number".into(),
            },
        ),
        event(
            39,
            AuditKind::CapabilityBudgetExhausted {
                signature_b58: "3yZe7d8mUS517G5965aydkZ46HS38QLi7UQiSojurfbQ".into(),
                action: "tool.call.echo".into(),
                max_uses: 5,
                used: 5,
            },
        ),
        event(
            40,
            AuditKind::CrossHostA2AAdmission {
                sender_pubkey_b58: "3yZe7d8mUS517G5965aydkZ46HS38QLi7UQiSojurfbQ".into(),
                recipient_display: "bob@host2".into(),
                outcome: "rejected".into(),
                reason: "unknown_principal".into(),
            },
        ),
        event(
            41,
            AuditKind::CrossHostA2ADelivery {
                recipient_pubkey_b58: "3yZe7d8mUS517G5965aydkZ46HS38QLi7UQiSojurfbQ".into(),
                recipient_display: "bob@host2".into(),
                outcome: "delivered".into(),
                reason: "".into(),
            },
        ),
    ]
}

fn fixture_path() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audit-kinds.v1.json")
}

/// Canonical wire form: the compact `serde_json` string, exactly the bytes the
/// audit chain hashes (`sha256(serde_json::to_string(event))`).
fn canonical(event: &AuditEvent) -> String {
    serde_json::to_string(event).expect("audit event serializes")
}

/// `Err(reason)` on drift — either the serialized bytes differ from the
/// committed vector, or the committed text does not deserialize back to the same
/// event. The second leg catches a serialize/deserialize asymmetry byte equality
/// alone cannot see. Pure (no IO) so the suite can self-test that it bites.
fn detect_drift(committed: &str, event: &AuditEvent) -> Result<(), String> {
    let canonical = canonical(event);
    if committed != canonical {
        return Err(format!(
            "serialized bytes differ from the committed vector\n  committed: {committed:?}\n  current:   {canonical:?}"
        ));
    }
    match serde_json::from_str::<AuditEvent>(committed) {
        Ok(parsed) if parsed == *event => Ok(()),
        Ok(parsed) => Err(format!(
            "committed bytes do not round-trip back to the event\n  expected: {event:?}\n  parsed:   {parsed:?}"
        )),
        Err(error) => Err(format!("committed bytes failed to deserialize: {error}")),
    }
}

#[test]
fn audit_kind_wire_schema_golden_vectors_are_frozen() {
    let bless = std::env::var_os(BLESS_ENV).is_some();
    let records = golden_audit_kind_records();

    if bless {
        let items: Vec<serde_json::Value> = records
            .iter()
            .map(|event| {
                let canonical = canonical(event);
                serde_json::json!({
                    "name": audit_kind_slug(&event.kind),
                    "canonical_json": canonical,
                    "event_hash_hex": hash_hex(canonical.as_bytes()),
                })
            })
            .collect();
        let doc = serde_json::json!({
            "description": DESCRIPTION,
            "records": items,
        });
        let mut text = serde_json::to_string_pretty(&doc).expect("fixture serializes");
        text.push('\n');
        std::fs::write(fixture_path(), text)
            .unwrap_or_else(|error| panic!("write {}: {error}", fixture_path().display()));
        return;
    }

    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_path()).unwrap_or_else(|error| {
            panic!(
                "missing golden fixture {} ({error}); regenerate with {BLESS_ENV}=1",
                fixture_path().display()
            )
        }),
    )
    .expect("golden fixture is valid JSON");

    assert_eq!(
        fixture["description"].as_str(),
        Some(DESCRIPTION),
        "{DRIFT} (fixture description must match the code-pinned contract note)",
    );

    let golden = fixture["records"]
        .as_array()
        .expect("fixture.records is an array");

    assert_eq!(
        records.len(),
        golden.len(),
        "{DRIFT} (record count: code has {}, fixture has {})",
        records.len(),
        golden.len(),
    );

    for (event, expected) in records.iter().zip(golden) {
        let slug = audit_kind_slug(&event.kind);

        // The slug must equal the serialized discriminator, independent of the
        // fixture — a mis-slugged `match` arm is caught here even if the fixture
        // was blessed with the same wrong slug.
        let wire_type = serde_json::to_value(event)
            .expect("audit event serializes")
            .get("kind")
            .and_then(|kind| kind.get("type"))
            .and_then(serde_json::Value::as_str)
            .expect("kind carries a string 'type' discriminator")
            .to_string();
        assert_eq!(
            slug, wire_type,
            "audit_kind_slug disagrees with the serialized discriminator for {slug:?}",
        );

        assert_eq!(
            Some(slug),
            expected["name"].as_str(),
            "{DRIFT} (record order/name mismatch near {slug:?})",
        );

        let committed = expected["canonical_json"]
            .as_str()
            .expect("record.canonical_json is a string");
        if let Err(reason) = detect_drift(committed, event) {
            panic!("audit kind {slug:?} drifted from its committed vector: {reason}\n{DRIFT}");
        }
        assert_eq!(
            hash_hex(committed.as_bytes()),
            expected["event_hash_hex"].as_str().unwrap(),
            "{DRIFT} (event hash for {slug:?})",
        );
    }
}

#[test]
fn audit_kind_corpus_is_exhaustive() {
    let expected: BTreeSet<&str> = EXPECTED_KINDS.iter().copied().collect();
    assert_eq!(
        expected.len(),
        EXPECTED_KINDS.len(),
        "EXPECTED_KINDS contains duplicate slugs"
    );

    let records = golden_audit_kind_records();
    let covered: BTreeSet<&str> = records.iter().map(|e| audit_kind_slug(&e.kind)).collect();
    assert_eq!(
        covered.len(),
        records.len(),
        "two golden vectors share an audit kind"
    );
    assert_eq!(
        covered, expected,
        "golden vectors must cover exactly the recognized audit kinds — every variant needs one vector"
    );

    if std::env::var_os(BLESS_ENV).is_some() {
        return;
    }

    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path()).expect("fixture exists"))
            .expect("golden fixture is valid JSON");
    let on_disk: BTreeSet<&str> = fixture["records"]
        .as_array()
        .expect("fixture.records is an array")
        .iter()
        .map(|record| record["name"].as_str().expect("record.name is a string"))
        .collect();
    assert_eq!(
        on_disk, expected,
        "committed fixture records must match the recognized audit kinds exactly — no orphans, none missing",
    );
}

#[test]
fn audit_kind_golden_harness_detects_drift() {
    // Drives the real `detect_drift` so the suite proves the comparison bites:
    // an exact canonical vector is clean, and a vector that differs by a single
    // field is flagged as drift.
    let clean = event(
        1,
        AuditKind::AuthenticationFailed {
            transport: "ipc".into(),
            reason: "unknown peer token".into(),
        },
    );
    let tampered = event(
        1,
        AuditKind::AuthenticationFailed {
            transport: "http".into(),
            reason: "unknown peer token".into(),
        },
    );
    assert!(
        detect_drift(&canonical(&clean), &clean).is_ok(),
        "an exact canonical vector must not be reported as drift"
    );
    assert!(
        detect_drift(&canonical(&tampered), &clean).is_err(),
        "a vector that differs from its event by one field must be flagged as drift"
    );
}
