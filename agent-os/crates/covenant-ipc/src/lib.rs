//! Length-prefixed JSON IPC for the covenant daemon and CLI.
//!
//! Wire format: 4-byte big-endian length, then that many bytes of JSON.
//! Frames over [`MAX_FRAME`] bytes are rejected on the read side.

#![deny(unsafe_code)]

use covenant_a2a::{
    A2AAutoRetryPolicy, A2AAutoRetryReport, A2ARepairOutcome, A2ARepairRequest, A2ATask,
    A2ATaskQueueEntry, A2ATaskQueueState, A2ATaskResult,
};
use covenant_audit::{AuditEvent, AuditIntegrityReport};
use covenant_budget::BudgetDebit;
use covenant_mcp::{Content, ToolSpec};
use covenant_peer_auth::{PeerStatusFilter, PeerSummary, RevokeOutcome};
use covenant_permissions::SignedCapability;
use covenant_types::{
    MemoryCompactionOutcome, MemoryCompactionRequest, MemoryRecord, MemoryRepairOutcome,
    MemoryRepairRequest, MemoryTier, SettlementReceipt,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyDrift {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub message: String,
    pub repair: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainStatus {
    pub chain: String,
    pub cluster: String,
    pub rpc_url: Option<String>,
    pub ws_url: Option<String>,
    pub program_id: Option<String>,
    pub covnt_mint: Option<String>,
    pub ready: bool,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptBatchSummary {
    pub batch_id: String,
    pub merkle_root: String,
    pub receipt_count: u32,
    #[serde(default)]
    pub tx_sig: Option<String>,
    #[serde(default)]
    pub slot: Option<u64>,
}
// `Receipts` mirrors `Memories`: a list of `SettlementReceipt`. Kept as a
// distinct response variant so the CLI can format them differently.
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

pub const MAX_FRAME: u32 = 8 * 1024 * 1024;
pub const PROTOCOL_NAME: &str = "covenant.ipc";
pub const PROTOCOL_VERSION: u32 = 1;
pub const MIN_PROTOCOL_VERSION: u32 = 1;
pub const MAX_PROTOCOL_VERSION: u32 = PROTOCOL_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolInfo {
    pub protocol: String,
    pub version: u32,
    pub min_supported: u32,
    pub max_supported: u32,
}

pub fn protocol_info() -> ProtocolInfo {
    ProtocolInfo {
        protocol: PROTOCOL_NAME.into(),
        version: PROTOCOL_VERSION,
        min_supported: MIN_PROTOCOL_VERSION,
        max_supported: MAX_PROTOCOL_VERSION,
    }
}

// `PartialEq` only — `A2ATaskResult` (carried in `PostA2AResult`) holds a
// `serde_json::Value` which isn't `Eq`. Symmetric with `Response`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Ping,
    ProtocolInfo,
    /// Mandatory first privileged frame on every IPC connection. Clients may
    /// send `ProtocolInfo` first to negotiate compatibility. Daemon resolves
    /// the token through `covenant_peer_auth::PeerRegistry`; on success the
    /// resolved `AgentId` is bound to the connection for the lifetime of the
    /// socket.
    Authenticate {
        token_b58: String,
    },
    SubmitIntent {
        text: String,
    },
    RecentMemory {
        #[serde(default)]
        tier: Option<MemoryTier>,
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    RecentReceipts {
        #[serde(default = "default_recent_limit")]
        limit: usize,
        #[serde(default)]
        since_ms: Option<u64>,
    },
    ChainStatus,
    FlushReceipts {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    ReceiptBatches {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    SearchMemory {
        query: String,
        #[serde(default)]
        tier: Option<MemoryTier>,
        #[serde(default = "default_recent_limit")]
        limit: usize,
        /// Optional cosine-similarity floor in `[0.0, 1.0]`. When set,
        /// records whose score is strictly less than the threshold are
        /// dropped before the `limit` truncation. `#[serde(default)]`
        /// keeps stale CLIs working — a missing field reads as `None`,
        /// which is the pre-filter behaviour.
        #[serde(default)]
        min_relevance: Option<f32>,
    },
    PurgeMemory {
        #[serde(default)]
        tier: Option<MemoryTier>,
        before_ms: u64,
    },
    /// Operator-controlled repair for verifier memory drift findings.
    /// Dry-run is the default CLI posture; apply requires an explicit
    /// caller choice and daemon capability.
    RepairMemory {
        request: MemoryRepairRequest,
    },
    /// Operator-controlled compaction for memory retention and stale-context hygiene.
    CompactMemory {
        request: MemoryCompactionRequest,
    },
    RecentCapabilities {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    GrantCapability {
        action: String,
        #[serde(default)]
        scope: Option<serde_json::Value>,
        #[serde(default)]
        expires_at: Option<u64>,
    },
    RevokeCapability {
        signature_b58: String,
    },
    Verify {
        #[serde(default = "default_verify_window")]
        window: usize,
    },
    IgnoreCheck {
        text: String,
    },
    ListTools,
    CallTool {
        name: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    /// Recent audit events scoped to the calling peer's pubkey.
    ///
    /// `since_ms` narrows the result to entries with `timestamp_ms >=
    /// since_ms`. The filter runs before `limit` is applied so a recent
    /// burst cannot push older-but-still-in-window events out of the
    /// truncation slice. The field is `#[serde(default)]` so a stale CLI
    /// built before the filter landed sends frames without it; the new
    /// daemon parses them as `None`, which is the pre-filter behaviour.
    RecentAudit {
        #[serde(default = "default_recent_limit")]
        limit: usize,
        #[serde(default)]
        since_ms: Option<u64>,
    },
    VerifyAuditIntegrity,
    /// Drop audit events strictly older than `before_ms`. Operator-driven
    /// retention; no scheduled compaction in v0.
    PurgeAudit {
        before_ms: u64,
    },
    /// Drop revocation tombstones (and their matching grants) whose
    /// `revoked_at` is strictly older than `before_ms`. Live grants are
    /// untouched. Same operator-driven retention shape as `PurgeAudit`.
    PurgeCapabilities {
        before_ms: u64,
    },
    SendA2ATask {
        task: A2ATask,
    },
    TryRecvA2ATask,
    PostA2AResult {
        result: A2ATaskResult,
    },
    TryRecvA2AResult,
    RecentA2ATasks {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    RecentA2AResults {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    /// Inspect queued and in-flight A2A tasks plus pending results.
    /// In-flight tasks have been leased to a recipient and will not be
    /// redelivered automatically after restart.
    ///
    /// `deadline_within_ms` narrows the visible task set to entries
    /// whose `task.deadline_ms` is set AND falls within the next N ms
    /// from the daemon's clock — i.e., urgent or already-past-due
    /// tasks. Entries without a deadline are dropped so the operator
    /// can triage by remaining time without scraping the JSON. The
    /// field is `#[serde(default)]` so a stale CLI built before the
    /// filter landed sends frames without it; the new daemon parses
    /// them as `None`, which is the pre-filter behaviour.
    ///
    /// `state_filter` narrows the visible task set to either queued
    /// or in-flight entries. Same `#[serde(default)]` shape so a
    /// stale CLI does not break the new daemon and a new CLI talking
    /// to an older daemon still pulls the full queue (the older
    /// daemon ignores the unknown field and returns both states).
    A2AQueue {
        #[serde(default = "default_recent_limit")]
        limit: usize,
        #[serde(default)]
        min_lease_age_ms: Option<u64>,
        #[serde(default)]
        deadline_within_ms: Option<u64>,
        #[serde(default)]
        state_filter: Option<A2ATaskQueueState>,
    },
    /// Manually repair an in-flight A2A lease. The daemon enforces
    /// visibility, capability, and audit rules before delegating to the
    /// mailbox repair primitive.
    RepairA2ATask {
        request: A2ARepairRequest,
    },
    /// Explicitly scan stale in-flight A2A leases and requeue only
    /// eligible idempotent tasks when the supplied policy is enabled.
    /// The default policy is disabled, so callers must opt in.
    RetryA2AStale {
        policy: A2AAutoRetryPolicy,
    },
    /// Drop the on-disk event log lines for fully-resolved A2A tasks
    /// (TaskSent + TaskRecv + ≥1 ResultPosted with matching ResultRecv
    /// counts). Operator-driven; no scheduled compaction in v0.
    CompactA2A,
    /// Drop revocation tombstones (and their matching `Registered`
    /// entries) from `peers/registry.jsonl` whose `revoked_at` is
    /// strictly older than `before_ms`. Live registrations are
    /// untouched. Same operator-driven retention shape as
    /// `PurgeAudit` / `PurgeCapabilities`.
    PurgePeers {
        before_ms: u64,
    },
    /// Re-dispatch the intent that the audit log's most recent
    /// `BudgetExhausted` row records under `intent_id`. The audit row
    /// carries `intent_text`, satisfying the §11 pin's "queue a resume"
    /// semantic for Phase-0 single-shot agents: the resume verb scans
    /// the audit, extracts the text, and runs it through
    /// `dispatch_intent` like any fresh `SubmitIntent`. Caller's
    /// responsibility to wait until the bucket has refilled —
    /// `BudgetExhausted.refill_eta_ms` is the wait floor.
    ResumeIntent {
        intent_id: Uuid,
    },
    /// Aggregate the most recent budget-debit events across every agent
    /// the daemon's router knows about. Operator-facing surface for the
    /// per-agent burn-rate dashboard. Daemon-side fan-out: the underlying
    /// `BudgetLedger::recent_debits` is per-agent; the daemon iterates
    /// `router.agents()`, calls it per non-zero-budget card, and returns
    /// one flat list sorted newest-first. Each [`BudgetDebit`] carries
    /// `agent: AgentId` so the UI can re-group client-side.
    RecentDebits {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    /// Mint a fresh bootstrap token, register it in the peer registry,
    /// rewrite `$COVENANT_HOME/peers/operator.token` with mode 0600, and
    /// revoke the old token — all in that order so a crash mid-rotation
    /// leaves a working setup behind. Gated to `peer.pubkey ==
    /// self.identity.pubkey`; a guest peer cannot rotate the operator's
    /// own token. The new token is delivered in the response because
    /// HTTP callers (the web UI) cannot read the on-disk file. Live IPC
    /// connections authenticated under the old token survive until they
    /// drop; HTTP rejects the old token immediately.
    RotateOperatorToken,
    /// Operator-triage view of the peer registry. Returns redacted
    /// [`PeerSummary`] rows newest-first. By default surfaces both live
    /// and revoked entries (with `revoked_at: Some(_)`); `status_filter`
    /// narrows to a single half. `pubkey_prefix` filters server-side
    /// on `bs58::encode(agent_id.pubkey)` — paste the b58 from an
    /// `OperatorTokenRotationRejected` audit row to find the matching
    /// registry entry. Operator-only; a non-operator peer is rejected
    /// with an `OperatorPeersListRejected` audit row.
    ///
    /// `status_filter` carries `#[serde(default)]` so a stale CLI built
    /// before the field landed sends frames without it; the new daemon
    /// parses them as `None`, which is the pre-filter behaviour (both
    /// halves surface).
    ListPeers {
        #[serde(default = "default_recent_limit")]
        limit: usize,
        #[serde(default)]
        pubkey_prefix: Option<String>,
        #[serde(default)]
        status_filter: Option<PeerStatusFilter>,
    },
    /// Revoke a single peer registry entry by token-prefix. The
    /// operator pastes the 6-char `token_prefix` they see in `peers
    /// list` output (or any longer leading substring of the full
    /// base58 token). Operator-only; a non-operator peer is rejected
    /// with an `OperatorPeerRevokeRejected` audit row. Closes the
    /// post-incident response loop alongside `ListPeers`.
    ///
    /// `force` gates the daemon-side self-revoke guard. When `false`
    /// (the default), a unique live match against the operator's own
    /// bootstrap token returns `RevokeOutcome::SelfRevokeForbidden`
    /// without mutating the registry; the operator must `peers rotate`
    /// for the no-downtime token rotation, or pass `force: true` to
    /// deliberately brick auth for the recovery-flow test.
    /// `#[serde(default)]` lets a stale CLI built before the guard
    /// landed send frames without the field; the new daemon parses
    /// them as `force: false`, the safe default.
    ///
    /// `match_limit` caps `RevokeOutcome::Ambiguous.matches` when the
    /// prefix is non-unique. `None` (the default) lets the daemon use
    /// its built-in constant; `Some(N)` overrides it for the operator
    /// who wants more (or fewer) candidates rendered in one round-trip.
    /// `#[serde(default)]` keeps stale CLIs forward-compatible — the
    /// missing field deserialises as `None`, the daemon falls back to
    /// the constant, and the response shape is unchanged.
    RevokePeer {
        token_prefix: String,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        match_limit: Option<usize>,
    },
}

fn default_recent_limit() -> usize {
    10
}

fn default_verify_window() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Pong,
    ProtocolInfo {
        info: ProtocolInfo,
    },
    /// Sent in response to a successful `Authenticate`. `display` is the
    /// resolved peer's `AgentId.display` so the caller can confirm which
    /// identity the daemon bound the connection to.
    Authenticated {
        display: String,
    },
    /// Sent on a bad / unknown / revoked token. The daemon closes the
    /// connection immediately after.
    AuthenticationFailed {
        reason: String,
    },
    IntentResult {
        intent_id: Uuid,
        status: String,
        text: String,
        sources: Vec<String>,
        settlement: Option<SettlementReceipt>,
    },
    Memories {
        records: Vec<MemoryRecord>,
    },
    MemoryPurged {
        purged: u64,
    },
    MemoryRepaired {
        outcome: MemoryRepairOutcome,
    },
    MemoryCompacted {
        outcome: MemoryCompactionOutcome,
    },
    Receipts {
        receipts: Vec<SettlementReceipt>,
    },
    ChainStatus {
        status: ChainStatus,
    },
    ReceiptBatchFlushed {
        batch: ReceiptBatchSummary,
        receipts_updated: u64,
    },
    ReceiptBatches {
        batches: Vec<ReceiptBatchSummary>,
    },
    VerifyReport {
        window: usize,
        checks: Vec<VerifyCheck>,
        #[serde(default)]
        drift: Vec<VerifyDrift>,
        orphans_total: u64,
    },
    Capabilities {
        capabilities: Vec<SignedCapability>,
    },
    CapabilityGranted {
        signature_b58: String,
        subject_display: String,
        action: String,
    },
    CapabilityRevoked {
        signature_b58: String,
        removed: bool,
    },
    IgnoreReport {
        ignored: bool,
        matched_pattern: Option<String>,
        rules_loaded: usize,
    },
    ToolList {
        tools: Vec<ToolSpec>,
    },
    ToolResult {
        content: Vec<Content>,
        is_error: bool,
    },
    AuditEvents {
        events: Vec<AuditEvent>,
    },
    AuditIntegrity {
        report: AuditIntegrityReport,
    },
    AuditPurged {
        purged: u64,
    },
    CapabilitiesPurged {
        purged: u64,
    },
    A2ATaskQueued {
        task_id: Uuid,
    },
    A2ATaskOpt {
        task: Option<A2ATask>,
    },
    A2AResultPosted {
        task_id: Uuid,
    },
    A2AResultOpt {
        result: Option<A2ATaskResult>,
    },
    A2ATasks {
        tasks: Vec<A2ATask>,
    },
    A2AResults {
        results: Vec<A2ATaskResult>,
    },
    A2AQueue {
        tasks: Vec<A2ATaskQueueEntry>,
        results: Vec<A2ATaskResult>,
    },
    A2ACompacted {
        dropped: u64,
    },
    A2ARepaired {
        outcome: A2ARepairOutcome,
    },
    A2AAutoRetried {
        report: A2AAutoRetryReport,
    },
    PeersPurged {
        purged: u64,
    },
    Debits {
        debits: Vec<BudgetDebit>,
    },
    /// Successful response to [`Request::RotateOperatorToken`]. The
    /// daemon has registered the new token, written it to
    /// `$COVENANT_HOME/peers/operator.token` (mode 0600), and revoked
    /// the old token. The caller's *current* connection (authenticated
    /// against the old token) keeps working; *new* connections must
    /// authenticate with `token_b58`.
    OperatorTokenRotated {
        token_b58: String,
    },
    /// Successful response to [`Request::ListPeers`]. Token bytes are
    /// **never** carried — only the 6-char `token_prefix`.
    ///
    /// `operator_pubkey_b58` is the daemon's own identity pubkey
    /// (base58 of `self.identity.pubkey`) so callers can identify which
    /// row is the operator's bootstrap peer without a second round-trip.
    /// Web UI uses it to hide the revoke button on the operator's own
    /// row — clicking revoke there would brick auth in v0 single-peer.
    /// `#[serde(default)]` so a stale CLI built before the field landed
    /// still deserialises a new daemon's response (the field reads as
    /// `String::new()`, which never matches a real pubkey b58 — the
    /// consumer's predicate falls through to the legacy behaviour, which
    /// surfaces the revoke button on every row).
    ///
    /// `truncated` is `true` when more rows existed in the registry
    /// than the caller's `limit` allowed — the operator then knows the
    /// displayed list is incomplete and that a longer `pubkey_prefix`
    /// or a higher `limit` is needed to see the rest. `#[serde(default)]`
    /// so a stale CLI deserialises a new daemon's response (the field
    /// reads as `false`, which degrades to the pre-bound behaviour
    /// where the operator assumes the displayed peers are exhaustive).
    PeerList {
        peers: Vec<PeerSummary>,
        #[serde(default)]
        operator_pubkey_b58: String,
        #[serde(default)]
        truncated: bool,
    },
    /// Response to [`Request::RevokePeer`]. The four `RevokeOutcome`
    /// cases (Revoked / AlreadyRevoked / NotFound / Ambiguous) are
    /// distinct on the wire so the CLI can render each case clearly.
    /// Token bytes are **never** carried — `RevokeOutcome` carries
    /// `PeerSummary` (or `Vec<PeerSummary>` for ambiguous), which by
    /// the registry's redaction invariant excludes `PeerToken`.
    PeerRevoked {
        outcome: RevokeOutcome,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("frame too large: {got} bytes (max {MAX_FRAME})")]
    FrameTooLarge { got: u64 },
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, IpcError>
where
    R: AsyncReadExt + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(IpcError::FrameTooLarge { got: len as u64 });
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), IpcError>
where
    W: AsyncWriteExt + Unpin,
    T: serde::Serialize,
{
    let payload = serde_json::to_vec(value)?;
    let len = u32::try_from(payload.len()).map_err(|_| IpcError::FrameTooLarge {
        got: payload.len() as u64,
    })?;
    if len > MAX_FRAME {
        return Err(IpcError::FrameTooLarge { got: len as u64 });
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_roundtrip_via_pipe() {
        let (mut a, mut b) = tokio::io::duplex(8192);
        let req = Request::SubmitIntent {
            text: "hello".into(),
        };
        write_frame(&mut a, &req).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn ping_pong_via_pipe() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_frame(&mut a, &Request::Ping).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(got, Request::Ping);
    }

    #[tokio::test]
    async fn protocol_info_roundtrips_via_pipe() {
        let (mut a, mut b) = tokio::io::duplex(256);
        write_frame(&mut a, &Request::ProtocolInfo).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(got, Request::ProtocolInfo);

        let response = Response::ProtocolInfo {
            info: protocol_info(),
        };
        write_frame(&mut b, &response).await.unwrap();
        let got: Response = read_frame(&mut a).await.unwrap();
        assert_eq!(got, response);
    }

    #[test]
    fn protocol_info_matches_v1_fixture() {
        let response = Response::ProtocolInfo {
            info: protocol_info(),
        };
        let json = serde_json::to_value(response).unwrap();
        let fixture_path = fixtures_dir().join("protocol-info.v1.json");
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap()).unwrap();
        assert_eq!(json, fixture);
    }

    #[test]
    fn protocol_info_serde_pins_required_fields_and_rejected_renames() {
        // protocol_info_matches_v1_fixture pins serialization against the
        // checked-in v1 fixture; this test pins the deserialization side
        // of the contract: every field must remain required (no silent
        // #[serde(default)] introduction that would let a malformed
        // negotiation frame decode as version=0), and any rename of the
        // wire field names must fail loud at parse time so a refactor
        // cannot quietly shift the protocol surface for every existing
        // CLI client.
        let info = protocol_info();
        let wire = serde_json::to_value(&info).unwrap();
        let obj = wire.as_object().expect("ProtocolInfo serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["max_supported", "min_supported", "protocol", "version"],
            "ProtocolInfo wire object must contain exactly the four documented fields; an addition or rename of any field breaks every CLI's negotiation handshake",
        );

        for required in ["protocol", "version", "min_supported", "max_supported"] {
            let mut shortened = obj.clone();
            shortened.remove(required);
            let payload = serde_json::Value::Object(shortened);
            assert!(
                serde_json::from_value::<ProtocolInfo>(payload).is_err(),
                "ProtocolInfo must reject a wire payload that omits {required}; a stray #[serde(default)] would silently let a malformed handshake decode",
            );
        }

        let renamed = serde_json::json!({
            "proto": PROTOCOL_NAME,
            "version": PROTOCOL_VERSION,
            "min_supported": MIN_PROTOCOL_VERSION,
            "max_supported": MAX_PROTOCOL_VERSION,
        });
        assert!(
            serde_json::from_value::<ProtocolInfo>(renamed).is_err(),
            "renamed protocol field (proto) must be rejected so the contract surface stays the documented key set",
        );
    }

    #[test]
    fn v1_response_fixtures_replay_against_current_parser() {
        let mut fixture_count = 0;

        for path in fixture_json_files(1) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            fixture_count += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            let response: Response = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()));

            if name == "protocol-info.v1.json" {
                assert_eq!(
                    response,
                    Response::ProtocolInfo {
                        info: protocol_info()
                    }
                );
            }
        }

        assert!(fixture_count > 0, "expected at least one v1 IPC fixture");
    }

    #[test]
    fn v2_fixture_skeleton_fails_closed_until_protocol_bump() {
        let v2_dir = fixtures_dir().join("v2");
        assert!(v2_dir.is_dir(), "missing v2 fixture staging directory");
        assert!(
            v2_dir.join("README.md").is_file(),
            "v2 fixture staging directory must document the migration contract"
        );

        let v2_fixtures = fixture_json_files(2);
        if PROTOCOL_VERSION < 2 {
            assert!(
                v2_fixtures.is_empty(),
                "v2 fixtures must not be committed while PROTOCOL_VERSION is still {PROTOCOL_VERSION}"
            );
        }
    }

    #[test]
    fn supported_protocol_versions_have_fixture_and_migration_evidence() {
        for version in MIN_PROTOCOL_VERSION..=MAX_PROTOCOL_VERSION {
            if version == 1 {
                continue;
            }

            let fixtures = fixture_json_files(version);
            assert!(
                !fixtures.is_empty(),
                "protocol v{version} support requires committed *.v{version}.json fixtures"
            );

            let migration_note = repo_root()
                .join("docs")
                .join("protocol-migrations")
                .join(format!("v{version}.md"));
            assert!(
                migration_note.is_file(),
                "protocol v{version} support requires docs/protocol-migrations/v{version}.md"
            );
        }
    }

    fn fixtures_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
    }

    fn fixture_json_files(version: u32) -> Vec<std::path::PathBuf> {
        let dir = if version == 1 {
            fixtures_dir()
        } else {
            fixtures_dir().join(format!("v{version}"))
        };
        let suffix = format!(".v{version}.json");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };

        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(&suffix))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crate should live under agent-os/crates")
            .to_path_buf()
    }

    #[tokio::test]
    async fn recent_memory_request_uses_default_limit() {
        let json = r#"{"kind":"recent_memory"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::RecentMemory { tier, limit } => {
                assert!(tier.is_none());
                assert_eq!(limit, 10);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn intent_result_serialises_settlement_null() {
        let r = Response::IntentResult {
            intent_id: Uuid::nil(),
            status: "ok".into(),
            text: "echo".into(),
            sources: vec![],
            settlement: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"settlement\":null"));
        assert!(json.contains("\"kind\":\"intent_result\""));
    }

    #[tokio::test]
    async fn rejects_oversized_frame_header() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let bad_len = (MAX_FRAME + 1).to_be_bytes();
        a.write_all(&bad_len).await.unwrap();
        let r: Result<Request, _> = read_frame(&mut b).await;
        assert!(matches!(r, Err(IpcError::FrameTooLarge { .. })));
    }
}
