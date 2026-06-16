//! Conformance golden vectors for the IPC `Request` wire surface.
//!
//! Each committed file under `tests/golden/requests/<kind>.json` is the
//! canonical serialized form of one `Request` variant — the frozen contract
//! external clients (the CLI, the HTTP gateway, third-party SDKs) encode
//! against. The shared harness (`common::check_corpus`) re-serializes the
//! in-code value and asserts byte-equality with the committed file plus a
//! round-trip back to the same value, so any unreviewed wire drift fails the
//! build instead of silently breaking consumers.
//!
//! Regenerate deliberately, only after reviewing the resulting diff:
//!
//! ```text
//! COVENANT_BLESS_IPC_GOLDEN=1 cargo test -p covenant-ipc --test golden_requests
//! ```
//!
//! See `tests/golden/README.md` for the corpus layout and blessing workflow.

mod common;

use covenant_ipc::Request;
use uuid::Uuid;

const SUBDIR: &str = "requests";

/// Every `Request` variant's wire `kind`, in enum-declaration order. The
/// exhaustiveness guard cross-checks this list against both the generated
/// vectors and the committed files, so it is the single written record of the
/// full request surface. The `rename_all = "snake_case"` derive splits the
/// `A2A` token into `a2_a` (e.g. `send_a2_a_task`); these are the real wire
/// slugs, not typos. Adding a `Request` variant breaks
/// `assert_request_variant_coverage` below until this list, the generator, and
/// the committed corpus are all updated.
const EXPECTED_REQUEST_KINDS: &[&str] = &[
    "ping",
    "protocol_info",
    "authenticate",
    "submit_intent",
    "recent_memory",
    "recent_receipts",
    "chain_status",
    "flush_receipts",
    "receipt_batches",
    "pay_x402",
    "backfill_settlement_receipts",
    "backfill_memory_records",
    "search_memory",
    "purge_memory",
    "repair_memory",
    "compact_memory",
    "recent_capabilities",
    "grant_capability",
    "revoke_capability",
    "sign_attestation",
    "verify",
    "ignore_check",
    "list_tools",
    "call_tool",
    "get_secret",
    "recent_audit",
    "verify_audit_integrity",
    "purge_audit",
    "purge_capabilities",
    "send_a2_a_task",
    "try_recv_a2_a_task",
    "post_a2_a_result",
    "try_recv_a2_a_result",
    "recent_a2_a_tasks",
    "recent_a2_a_results",
    "a2_a_queue",
    "repair_a2_a_task",
    "retry_a2_a_stale",
    "compact_a2_a",
    "purge_peers",
    "resume_intent",
    "recent_debits",
    "rotate_operator_token",
    "list_peers",
    "revoke_peer",
    "sap_status",
    "sap_publish_agent",
    "sap_publish_audit_root",
    "sap_publish_attestation",
    "query_provenance",
];

/// One fully-populated representative per `Request` variant. Optional and
/// `skip_serializing_if` fields are set so the committed contract shows the
/// complete envelope a client can send; nested domain payloads (`A2ATask`,
/// `MemoryRepairRequest`, …) keep their own crate-level shape pins — these
/// vectors freeze the outer request envelope.
fn golden_request_vectors() -> Vec<Request> {
    let ts = 1_700_000_000_000u64;
    vec![
        Request::Ping,
        Request::ProtocolInfo,
        Request::Authenticate {
            token_b58: "11111111111111111111111111111111".into(),
        },
        Request::SubmitIntent {
            text: "summarise the audit log".into(),
            prefer_stream: Some(true),
        },
        Request::RecentMemory {
            tier: Some(covenant_types::MemoryTier::Working),
            limit: 25,
            prefer_stream: Some(true),
        },
        Request::RecentReceipts {
            limit: 25,
            since_ms: Some(ts),
        },
        Request::ChainStatus,
        Request::FlushReceipts { limit: 25 },
        Request::ReceiptBatches { limit: 25 },
        Request::PayX402 {
            provider: "xona".into(),
            endpoint: "https://api.xona-agent.com/img".into(),
            method: "POST".into(),
            body: Some(serde_json::json!({ "prompt": "hi" })),
            network: "solana:mainnet".into(),
            asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            per_call_cap: "100000".into(),
            credits: 8,
        },
        Request::BackfillSettlementReceipts {
            dry_run: true,
            scope_pubkey: Some("subjectpubkeyb58".into()),
        },
        Request::BackfillMemoryRecords {
            dry_run: true,
            scope_pubkey: Some("subjectpubkeyb58".into()),
        },
        Request::SearchMemory {
            query: "hello".into(),
            tier: Some(covenant_types::MemoryTier::Working),
            limit: 25,
            min_relevance: Some(0.75),
        },
        Request::PurgeMemory {
            tier: Some(covenant_types::MemoryTier::Working),
            before_ms: ts,
        },
        Request::RepairMemory {
            request: covenant_types::MemoryRepairRequest {
                mode: covenant_types::MemoryRepairMode::DryRun,
                command: covenant_types::MemoryRepairCommand::DeleteRecord {
                    id: Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888),
                },
                reason: "verifier flagged a dangling parent".into(),
            },
        },
        Request::CompactMemory {
            request: covenant_types::MemoryCompactionRequest {
                mode: covenant_types::MemoryRepairMode::DryRun,
                policy: covenant_types::MemoryCompactionPolicy::default(),
                reason: "retention sweep".into(),
            },
        },
        Request::RecentCapabilities { limit: 25 },
        Request::GrantCapability {
            action: "memory.read".into(),
            scope: Some(serde_json::json!({ "tier": "working" })),
            expires_at: Some(ts),
        },
        Request::RevokeCapability {
            signature_b58: "3xS9Yk1f8wL2bN7pQz4mRtUvJh6cKaDe5gXyWnVoBqAr".into(),
        },
        Request::SignAttestation {
            message_b58: "11111111111111111111111111111111".into(),
            ts,
        },
        Request::Verify { window: 250 },
        Request::IgnoreCheck {
            text: "deploy production rollout".into(),
        },
        Request::ListTools,
        Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        },
        Request::GetSecret {
            name: "openai_api_key".into(),
        },
        Request::RecentAudit {
            limit: 25,
            since_ms: Some(ts),
            prefer_stream: Some(true),
        },
        Request::VerifyAuditIntegrity,
        Request::PurgeAudit { before_ms: ts },
        Request::PurgeCapabilities { before_ms: ts },
        Request::SendA2ATask {
            task: covenant_a2a::A2ATask {
                id: Uuid::from_u128(0xA2A5_EEDE_F00D_CAFE_0000_0000_BABE_BEEF),
                sender: covenant_types::AgentId::new("alice@local", [1u8; 32]),
                recipient: covenant_types::AgentId::new("bob@local", [2u8; 32]),
                intent_text: "fetch papers".into(),
                task_kind: None,
                parent: None,
                deadline_ms: None,
                idempotency: None,
            },
        },
        Request::TryRecvA2ATask,
        Request::PostA2AResult {
            result: covenant_a2a::A2ATaskResult::ok(
                Uuid::from_u128(0xC0DE_F00D_BEEF_CAFE_1234_5678_9ABC_DEF0),
                vec![],
            ),
        },
        Request::TryRecvA2AResult,
        Request::RecentA2ATasks { limit: 25 },
        Request::RecentA2AResults { limit: 25 },
        Request::A2AQueue {
            limit: 25,
            min_lease_age_ms: Some(5_000),
            deadline_within_ms: Some(60_000),
            state_filter: Some(covenant_a2a::A2ATaskQueueState::InFlight),
        },
        Request::RepairA2ATask {
            request: covenant_a2a::A2ARepairRequest {
                task_id: Uuid::from_u128(0xA2A5_EEDE_F00D_CAFE_0000_0000_BABE_BEEF),
                command: covenant_a2a::A2ARepairCommand::Requeue {
                    lease_id: Some(Uuid::from_u128(0x0BAD_C0DE_0BAD_C0DE_0BAD_C0DE_0BAD_C0DE)),
                    duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
                },
                reason: "stale lease, idempotent task".into(),
            },
        },
        Request::RetryA2AStale {
            policy: covenant_a2a::A2AAutoRetryPolicy::default(),
        },
        Request::CompactA2A,
        Request::PurgePeers { before_ms: ts },
        Request::ResumeIntent {
            intent_id: Uuid::from_u128(0xA2A5_EEDE_F00D_CAFE_0000_0000_BABE_BEEF),
        },
        Request::RecentDebits { limit: 25 },
        Request::RotateOperatorToken,
        Request::ListPeers {
            limit: 25,
            pubkey_prefix: Some("5x".into()),
            status_filter: Some(covenant_peer_auth::PeerStatusFilter::Live),
        },
        Request::RevokePeer {
            token_prefix: "abc123".into(),
            force: true,
            match_limit: Some(7),
        },
        Request::SapStatus,
        Request::SapPublishAgent {
            manifest_json: r#"{"name":"example","authority":"example"}"#.into(),
        },
        Request::SapPublishAuditRoot {
            root_hash_hex: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                .into(),
            release_target: "mainnet".into(),
            release_subject: "agent-v1".into(),
            release_scope: "public".into(),
        },
        Request::SapPublishAttestation {
            agent_pda: "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin".into(),
            root_hash_hex: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                .into(),
            attestation_type: Some("verification".into()),
            expires_at_unix: Some(1_700_000_000),
        },
        Request::QueryProvenance {
            since_ms: Some(1),
            until_ms: Some(2),
            actor: Some("research@agent".into()),
            approver: Some("operator@local".into()),
            rule: Some("Sig".into()),
            outcome: Some("granted".into()),
            limit: 5,
        },
    ]
}

#[test]
fn request_golden_vectors_match_committed_corpus() {
    common::check_corpus(SUBDIR, &golden_request_vectors());
}

#[test]
fn request_golden_corpus_is_exhaustive() {
    common::check_exhaustive(SUBDIR, EXPECTED_REQUEST_KINDS, &golden_request_vectors());
}

#[test]
fn harness_detects_drift() {
    // Drives the real `common::detect_drift` — the comparison `check_corpus`
    // delegates to — so the suite proves the harness actually bites: an exact
    // canonical vector is clean, and a committed vector that differs from its
    // value by a single field is flagged as drift.
    let value = Request::PurgeAudit { before_ms: 1 };
    assert!(
        common::detect_drift(&common::canonical_json(&value), &value).is_ok(),
        "an exact canonical vector must not be reported as drift"
    );

    let tampered = common::canonical_json(&Request::PurgeAudit { before_ms: 2 });
    assert!(
        common::detect_drift(&tampered, &value).is_err(),
        "a committed vector that differs from its canonical value must be flagged as drift"
    );
}

#[test]
fn harness_detects_round_trip_asymmetry() {
    // Covers `detect_drift`'s second leg in isolation: a value whose canonical
    // bytes are byte-stable (leg 1 clean) yet do not deserialize back to the
    // same value must still be flagged. A `skip_deserializing` field that still
    // serializes is the minimal such asymmetry — it round-trips to the field's
    // default, not the serialized value.
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Asymmetric {
        #[serde(skip_deserializing)]
        write_only: u8,
    }

    let value = Asymmetric { write_only: 7 };
    let canonical = common::canonical_json(&value);
    assert!(
        canonical.contains("\"write_only\": 7"),
        "the field must serialize so leg 1 (byte-equality) stays clean"
    );
    assert!(
        common::detect_drift(&canonical, &value).is_err(),
        "canonical bytes that do not round-trip back to the value must be flagged as drift"
    );
}

/// Compile-time exhaustiveness tripwire: adding or removing a `Request`
/// variant breaks this match, forcing a matching update to
/// `golden_request_vectors`, `EXPECTED_REQUEST_KINDS`, and the committed
/// corpus. It is intentionally never called.
#[allow(dead_code)]
fn assert_request_variant_coverage(req: &Request) {
    match req {
        Request::Ping
        | Request::ProtocolInfo
        | Request::Authenticate { .. }
        | Request::SubmitIntent { .. }
        | Request::RecentMemory { .. }
        | Request::RecentReceipts { .. }
        | Request::ChainStatus
        | Request::FlushReceipts { .. }
        | Request::ReceiptBatches { .. }
        | Request::PayX402 { .. }
        | Request::BackfillSettlementReceipts { .. }
        | Request::BackfillMemoryRecords { .. }
        | Request::SearchMemory { .. }
        | Request::PurgeMemory { .. }
        | Request::RepairMemory { .. }
        | Request::CompactMemory { .. }
        | Request::RecentCapabilities { .. }
        | Request::GrantCapability { .. }
        | Request::RevokeCapability { .. }
        | Request::SignAttestation { .. }
        | Request::Verify { .. }
        | Request::IgnoreCheck { .. }
        | Request::ListTools
        | Request::CallTool { .. }
        | Request::GetSecret { .. }
        | Request::RecentAudit { .. }
        | Request::VerifyAuditIntegrity
        | Request::PurgeAudit { .. }
        | Request::PurgeCapabilities { .. }
        | Request::SendA2ATask { .. }
        | Request::TryRecvA2ATask
        | Request::PostA2AResult { .. }
        | Request::TryRecvA2AResult
        | Request::RecentA2ATasks { .. }
        | Request::RecentA2AResults { .. }
        | Request::A2AQueue { .. }
        | Request::RepairA2ATask { .. }
        | Request::RetryA2AStale { .. }
        | Request::CompactA2A
        | Request::PurgePeers { .. }
        | Request::ResumeIntent { .. }
        | Request::RecentDebits { .. }
        | Request::RotateOperatorToken
        | Request::ListPeers { .. }
        | Request::RevokePeer { .. }
        | Request::SapStatus
        | Request::SapPublishAgent { .. }
        | Request::SapPublishAuditRoot { .. }
        | Request::SapPublishAttestation { .. }
        | Request::QueryProvenance { .. } => {}
    }
}
