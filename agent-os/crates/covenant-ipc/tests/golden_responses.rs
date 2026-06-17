//! Conformance golden vectors for the IPC `Response` wire surface.
//!
//! Each committed file under `tests/golden/responses/<kind>.json` is the
//! canonical serialized form of one `Response` variant — the frozen contract
//! consumers (the CLI, the HTTP gateway, third-party SDKs) decode. The shared
//! harness (`common::check_corpus`) re-serializes the in-code value and asserts
//! byte-equality with the committed file plus a round-trip back to the same
//! value, so any unreviewed wire drift fails the build.
//!
//! These freeze the v1 terminal `Response` frames the daemon emits by default.
//! The v2 streaming envelopes (`StreamEnvelope`) live under
//! `tests/fixtures/v2/`; see `docs/protocol-versioning.md`. List-bearing
//! variants pin the empty-list envelope — element payloads (`MemoryRecord`,
//! `SettlementReceipt`, …) keep their own crate-level shape pins.
//!
//! Regenerate deliberately, only after reviewing the resulting diff:
//!
//! ```text
//! COVENANT_BLESS_IPC_GOLDEN=1 cargo test -p covenant-ipc --test golden_responses
//! ```
//!
//! See `tests/golden/README.md` for the corpus layout and blessing workflow.

mod common;

use covenant_ipc::Response;
use uuid::Uuid;

const SUBDIR: &str = "responses";

/// Every `Response` variant's wire `kind`, in enum-declaration order. Same
/// `a2_a` snake_case split as the requests (`a2_a_task_queued`, …). Adding a
/// `Response` variant breaks `assert_response_variant_coverage` below until
/// this list, the generator, and the committed corpus are all updated.
const EXPECTED_RESPONSE_KINDS: &[&str] = &[
    "pong",
    "protocol_info",
    "authenticated",
    "authentication_failed",
    "intent_result",
    "memories",
    "memory_purged",
    "memory_repaired",
    "memory_compacted",
    "receipts",
    "chain_status",
    "receipt_batch_flushed",
    "receipt_batches",
    "settlement_receipts_backfilled",
    "memory_records_backfilled",
    "verify_report",
    "capabilities",
    "capability_granted",
    "capability_revoked",
    "identity_attestation",
    "ignore_report",
    "tool_list",
    "tool_result",
    "secret",
    "audit_events",
    "audit_integrity",
    "audit_purged",
    "capabilities_purged",
    "a2_a_task_queued",
    "a2_a_task_opt",
    "a2_a_result_posted",
    "a2_a_result_opt",
    "a2_a_tasks",
    "a2_a_results",
    "a2_a_queue",
    "a2_a_compacted",
    "a2_a_repaired",
    "a2_a_auto_retried",
    "peers_purged",
    "debits",
    "operator_token_rotated",
    "peer_list",
    "peer_revoked",
    "x402_paid",
    "sap_status",
    "sap_published_agent",
    "sap_published_audit_root",
    "sap_published_attestation",
    "provenance_actions",
    "capability_usage",
    "error",
];

/// One representative per `Response` variant. Scalar and nested-struct fields
/// are populated to show the envelope; list-bearing variants pin the empty
/// list so the contract freezes the outer frame, not the element payloads,
/// which carry their own crate-level shape pins.
fn golden_response_vectors() -> Vec<Response> {
    vec![
        Response::Pong,
        Response::ProtocolInfo {
            info: covenant_ipc::protocol_info(),
        },
        Response::Authenticated {
            display: "operator@local".into(),
        },
        Response::AuthenticationFailed {
            reason: "unknown token".into(),
        },
        Response::IntentResult {
            intent_id: Uuid::from_u128(81),
            status: "ok".into(),
            text: "echo".into(),
            sources: vec!["path/one.md".into()],
            settlement: None,
        },
        Response::Memories { records: vec![] },
        Response::MemoryPurged { purged: 42 },
        Response::MemoryRepaired {
            outcome: covenant_types::MemoryRepairOutcome {
                id: Uuid::nil(),
                action: covenant_types::MemoryRepairAction::DetachParent,
                mode: covenant_types::MemoryRepairMode::DryRun,
                would_change: false,
                changed: false,
                before: None,
                after: None,
            },
        },
        Response::MemoryCompacted {
            outcome: covenant_types::MemoryCompactionOutcome {
                mode: covenant_types::MemoryRepairMode::DryRun,
                would_change: false,
                changed: false,
                deleted: vec![],
                stale_marked: vec![],
                parents_detached: vec![],
            },
        },
        Response::Receipts { receipts: vec![] },
        Response::ChainStatus {
            status: covenant_ipc::ChainStatus {
                chain: "solana".into(),
                cluster: "devnet".into(),
                rpc_url: None,
                ws_url: None,
                program_id: None,
                covnt_mint: None,
                ready: false,
                missing: vec![],
            },
        },
        Response::ReceiptBatchFlushed {
            batch: covenant_ipc::ReceiptBatchSummary {
                batch_id: "batch-1".into(),
                merkle_root: "00".repeat(32),
                receipt_count: 0,
                tx_sig: None,
                slot: None,
            },
            receipts_updated: 0,
        },
        Response::ReceiptBatches { batches: vec![] },
        Response::SettlementReceiptsBackfilled {
            row_count: 0,
            rollback_path: None,
            dry_run: true,
        },
        Response::MemoryRecordsBackfilled {
            row_count: 7,
            savepoint_name: "backfill_receipt_correlation".into(),
            dry_run: false,
        },
        Response::VerifyReport {
            window: 0,
            checks: vec![],
            drift: vec![],
            orphans_total: 0,
        },
        Response::Capabilities {
            capabilities: vec![],
        },
        Response::CapabilityGranted {
            signature_b58: "sig-abc".into(),
            subject_display: "research@host".into(),
            action: "tool.call.fs.read".into(),
        },
        Response::CapabilityRevoked {
            signature_b58: "sig-xyz".into(),
            removed: true,
        },
        Response::IdentityAttestation {
            signature_b58: "sig-abc".into(),
            pubkey_b58: "pubkey-xyz".into(),
            ts: 1_000,
        },
        Response::IgnoreReport {
            ignored: true,
            matched_pattern: Some("deploy production rollout".into()),
            rules_loaded: 1,
        },
        Response::ToolList { tools: vec![] },
        Response::ToolResult {
            content: vec![covenant_mcp::Content::text("ok")],
            is_error: false,
        },
        Response::Secret {
            name: "openai_api_key".into(),
            value: "sk-redacted-illustrative-value".into(),
        },
        Response::AuditEvents { events: vec![] },
        Response::AuditIntegrity {
            report: covenant_audit::AuditIntegrityReport {
                events: 0,
                anchors: 0,
                valid: true,
                root_hash_hex: String::new(),
                failures: vec![],
            },
        },
        Response::AuditPurged { purged: 13 },
        Response::CapabilitiesPurged { purged: 5 },
        Response::A2ATaskQueued {
            task_id: Uuid::from_u128(91),
        },
        Response::A2ATaskOpt { task: None },
        Response::A2AResultPosted {
            task_id: Uuid::from_u128(123),
        },
        Response::A2AResultOpt { result: None },
        Response::A2ATasks { tasks: vec![] },
        Response::A2AResults { results: vec![] },
        Response::A2AQueue {
            tasks: vec![],
            results: vec![],
        },
        Response::A2ACompacted { dropped: 17 },
        Response::A2ARepaired {
            outcome: covenant_a2a::A2ARepairOutcome {
                task_id: Uuid::nil(),
                action: covenant_a2a::A2ARepairAction::Requeued,
                state: covenant_a2a::A2ARepairState::Queued,
                attempt: 0,
                result: None,
            },
        },
        Response::A2AAutoRetried {
            report: covenant_a2a::A2AAutoRetryReport::new(
                covenant_a2a::A2AAutoRetryPolicy::default(),
            ),
        },
        Response::PeersPurged { purged: 3 },
        Response::Debits { debits: vec![] },
        Response::OperatorTokenRotated {
            token_b58: "5DkPzkAozsAjEvyZ7DJ2dEoEoVHHFvjpSizMV2ohHfMr".into(),
        },
        Response::PeerList {
            peers: vec![],
            operator_pubkey_b58: String::new(),
            truncated: false,
        },
        Response::PeerRevoked {
            outcome: covenant_peer_auth::RevokeOutcome::NotFound,
        },
        Response::X402Paid {
            receipt_id: Uuid::from_u128(0xFEED_FACE_DEAD_BEEF),
            status: 200,
            body: "{\"ok\":true}".into(),
        },
        Response::SapStatus {
            enabled: false,
            cluster: "mainnet-beta".into(),
            program_id: "Cov1111111111111111111111111111111111111111".into(),
            rpc_url: "https://api.mainnet-beta.solana.com".into(),
            explorer_url: "https://explorer.solana.com".into(),
            has_signer: false,
        },
        Response::SapPublishedAgent {
            agent_pda: "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin".into(),
            signature: "5sigPublishAgentExampleBase58Signature".into(),
        },
        Response::SapPublishedAuditRoot {
            ledger_pda: "Ledger11111111111111111111111111111111111111".into(),
            signature: "5sigPublishAuditRootExampleBase58Signature".into(),
        },
        Response::SapPublishedAttestation {
            attestation_pda: "Attest1111111111111111111111111111111111111".into(),
            attester: "Verifier111111111111111111111111111111111111".into(),
            agent_pda: "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin".into(),
            signature: "5sigPublishAttestationExampleBase58Signature".into(),
        },
        Response::ProvenanceActions {
            integrity: covenant_audit::AuditIntegrityReport {
                events: 3,
                anchors: 3,
                valid: true,
                root_hash_hex: "ab".repeat(32),
                failures: vec![],
            },
            actions: vec![covenant_audit::PrivilegedAction {
                event_id: Uuid::nil(),
                timestamp_ms: 1_000,
                kind: "capability_granted".into(),
                actor: "research@agent".into(),
                action: "memory.write".into(),
                approver: Some("operator@local".into()),
                rule: Some("GrantSig".into()),
                outcome: "granted".into(),
            }],
            scanned: 7,
        },
        Response::CapabilityUsage {
            grants: vec![covenant_ipc::CapabilityUsageEntry {
                signature_b58: "3xS9Yk1f8wL2bN7pQz4mRtUvJh6cKaDe5gXyWnVoBqAr".into(),
                action: "tool.call.echo".into(),
                expires_at: Some(1_700_000_000_000),
                revoked: false,
                effective: covenant_ipc::CapabilityEffectiveStatus::Live,
                budget: Some(covenant_ipc::CapabilityUsageBudget {
                    max_uses: 5,
                    used: 2,
                    remaining: 3,
                }),
            }],
        },
        Response::Error {
            message: "capability check failed".into(),
        },
    ]
}

#[test]
fn response_golden_vectors_match_committed_corpus() {
    common::check_corpus(SUBDIR, &golden_response_vectors());
}

#[test]
fn response_golden_corpus_is_exhaustive() {
    common::check_exhaustive(SUBDIR, EXPECTED_RESPONSE_KINDS, &golden_response_vectors());
}

/// Compile-time exhaustiveness tripwire: adding or removing a `Response`
/// variant breaks this match, forcing a matching update to
/// `golden_response_vectors`, `EXPECTED_RESPONSE_KINDS`, and the committed
/// corpus. It is intentionally never called.
#[allow(dead_code)]
fn assert_response_variant_coverage(res: &Response) {
    match res {
        Response::Pong
        | Response::ProtocolInfo { .. }
        | Response::Authenticated { .. }
        | Response::AuthenticationFailed { .. }
        | Response::IntentResult { .. }
        | Response::Memories { .. }
        | Response::MemoryPurged { .. }
        | Response::MemoryRepaired { .. }
        | Response::MemoryCompacted { .. }
        | Response::Receipts { .. }
        | Response::ChainStatus { .. }
        | Response::ReceiptBatchFlushed { .. }
        | Response::ReceiptBatches { .. }
        | Response::SettlementReceiptsBackfilled { .. }
        | Response::MemoryRecordsBackfilled { .. }
        | Response::VerifyReport { .. }
        | Response::Capabilities { .. }
        | Response::CapabilityGranted { .. }
        | Response::CapabilityRevoked { .. }
        | Response::IdentityAttestation { .. }
        | Response::IgnoreReport { .. }
        | Response::ToolList { .. }
        | Response::ToolResult { .. }
        | Response::Secret { .. }
        | Response::AuditEvents { .. }
        | Response::AuditIntegrity { .. }
        | Response::AuditPurged { .. }
        | Response::CapabilitiesPurged { .. }
        | Response::A2ATaskQueued { .. }
        | Response::A2ATaskOpt { .. }
        | Response::A2AResultPosted { .. }
        | Response::A2AResultOpt { .. }
        | Response::A2ATasks { .. }
        | Response::A2AResults { .. }
        | Response::A2AQueue { .. }
        | Response::A2ACompacted { .. }
        | Response::A2ARepaired { .. }
        | Response::A2AAutoRetried { .. }
        | Response::PeersPurged { .. }
        | Response::Debits { .. }
        | Response::OperatorTokenRotated { .. }
        | Response::PeerList { .. }
        | Response::PeerRevoked { .. }
        | Response::X402Paid { .. }
        | Response::SapStatus { .. }
        | Response::SapPublishedAgent { .. }
        | Response::SapPublishedAuditRoot { .. }
        | Response::SapPublishedAttestation { .. }
        | Response::ProvenanceActions { .. }
        | Response::CapabilityUsage { .. }
        | Response::Error { .. } => {}
    }
}
