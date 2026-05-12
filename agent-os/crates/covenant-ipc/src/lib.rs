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
    fn verify_check_serde_pins_three_required_fields_and_rejected_renames() {
        // VerifyCheck is the per-check row every Response::VerifyReport
        // carries inside the `checks` vector. CLI `covenant verify` and
        // HTTP `/verify` consumers render one line per check. All three
        // fields are required — none carry `#[serde(default)]` or
        // `#[serde(skip_serializing_if)]`. A refactor that adds default
        // to any field would silently let a malformed payload decode
        // with an empty-string or false default; the CLI would then
        // render a misleading row (`passed: false` with empty name and
        // message), hiding the real failure source.

        let check = VerifyCheck {
            name: "hash_chain".into(),
            passed: true,
            message: "verified 100 events".into(),
        };
        let wire = serde_json::to_value(&check).unwrap();
        let obj = wire
            .as_object()
            .expect("VerifyCheck serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["message", "name", "passed"],
            "VerifyCheck wire object must contain exactly the three \
             documented fields; an addition or rename of any field breaks \
             every verify-report consumer's per-check destructuring"
        );

        let decoded: VerifyCheck = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded, check,
            "VerifyCheck must round-trip through serde_json verbatim — the \
             Eq/PartialEq derive is the contract every fixture-replay test \
             leans on"
        );

        for required in ["name", "passed", "message"] {
            let mut payload = serde_json::Map::new();
            payload.insert("name".into(), serde_json::Value::String("hash_chain".into()));
            payload.insert("passed".into(), serde_json::Value::Bool(true));
            payload.insert(
                "message".into(),
                serde_json::Value::String("verified 100 events".into()),
            );
            payload.remove(required);
            assert!(
                serde_json::from_value::<VerifyCheck>(serde_json::Value::Object(payload)).is_err(),
                "VerifyCheck must reject a wire payload that omits {required}; \
                 a stray #[serde(default)] would silently let the field default \
                 and the CLI would render a misleading row hiding the real \
                 verification outcome"
            );
        }

        let renamed = serde_json::json!({
            "check_name": "hash_chain",
            "passed": true,
            "message": "verified 100 events",
        });
        assert!(
            serde_json::from_value::<VerifyCheck>(renamed).is_err(),
            "renamed VerifyCheck::name field (check_name) must be rejected so \
             the contract surface stays the documented three-key set"
        );

        let renamed_passed = serde_json::json!({
            "name": "hash_chain",
            "ok": true,
            "message": "verified 100 events",
        });
        assert!(
            serde_json::from_value::<VerifyCheck>(renamed_passed).is_err(),
            "renamed VerifyCheck::passed field (ok) must be rejected — the wire \
             key is the contract every JSON consumer destructures on"
        );
    }

    #[test]
    fn verify_drift_serde_pins_skip_empty_id_and_required_fields() {
        // VerifyDrift is the drift row every Response::VerifyReport carries
        // through CLI `covenant verify`, HTTP `/verify`, and the IPC Verify
        // path; daemons surface hundreds of rows in real verify windows
        // across memory, A2A, audit, and receipt namespaces. The contract
        // surface this test pins:
        //
        // 1. `id: Option<String>` carries `#[serde(default,
        //    skip_serializing_if = "Option::is_none")]` so a None id stays
        //    compact on the wire — a refactor that drops the predicate
        //    would emit `"id": null` on every None row, bloating every
        //    verify response.
        // 2. `kind`, `message`, `repair` carry no `#[serde(default)]`; the
        //    wire payload must contain each one. A stray `#[serde(default)]`
        //    introduction would silently let a malformed payload decode
        //    with an empty-string default, and the CLI would render an
        //    empty/unattributed drift row instead of failing loud at the
        //    IPC boundary.

        let none_id = VerifyDrift {
            kind: "memory_drift".into(),
            id: None,
            message: "missing receipt".into(),
            repair: "covenant memory repair".into(),
        };
        let wire = serde_json::to_value(&none_id).unwrap();
        let obj = wire
            .as_object()
            .expect("VerifyDrift serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "message", "repair"],
            "VerifyDrift with id=None must not emit an id key on the wire; \
             dropping skip_serializing_if = Option::is_none would bloat every \
             None-id row in a verify report"
        );

        let some_id = VerifyDrift {
            kind: "memory_drift".into(),
            id: Some("uuid-123".into()),
            message: "missing receipt".into(),
            repair: "covenant memory repair".into(),
        };
        let wire = serde_json::to_value(&some_id).unwrap();
        let obj = wire
            .as_object()
            .expect("VerifyDrift serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["id", "kind", "message", "repair"],
            "VerifyDrift with id=Some must surface the id key alongside the \
             three required fields"
        );
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("uuid-123"),
            "populated VerifyDrift::id must round-trip verbatim on the wire"
        );

        let no_id_wire = serde_json::json!({
            "kind": "memory_drift",
            "message": "missing receipt",
            "repair": "covenant memory repair",
        });
        let decoded: VerifyDrift = serde_json::from_value(no_id_wire).unwrap();
        assert_eq!(
            decoded.id, None,
            "VerifyDrift with id key omitted must decode as None; the \
             #[serde(default)] on Option<String> is the forward-compatible \
             contract every CLI built before the field landed depends on"
        );

        for required in ["kind", "message", "repair"] {
            let mut payload = serde_json::Map::new();
            payload.insert("kind".into(), serde_json::Value::String("memory_drift".into()));
            payload.insert("message".into(), serde_json::Value::String("missing receipt".into()));
            payload.insert(
                "repair".into(),
                serde_json::Value::String("covenant memory repair".into()),
            );
            payload.remove(required);
            assert!(
                serde_json::from_value::<VerifyDrift>(serde_json::Value::Object(payload)).is_err(),
                "VerifyDrift must reject a wire payload that omits {required}; \
                 a stray #[serde(default)] would silently let an empty-string \
                 default decode and the CLI would render an unattributed drift row"
            );
        }
    }

    #[test]
    fn chain_status_serde_pins_strict_required_fields() {
        // ChainStatus is the daemon's settlement-chain readiness response,
        // used by IPC `Request::ChainStatus`, HTTP `/chain/status`, and
        // CLI `covenant chain status`. None of the fields carry
        // `#[serde(skip_serializing_if)]`, so the wire payload always
        // carries every key — the four Option<String> fields surface as
        // JSON null when None instead of being absent. Of the eight
        // fields, four are non-Option (chain, cluster, ready, missing)
        // and must be present on decode; the four Option<String> fields
        // (rpc_url, ws_url, program_id, covnt_mint) are auto-defaulted
        // to None by serde when missing, which is the documented contract
        // every fixture replay leans on.

        let unconfigured = ChainStatus {
            chain: "solana".into(),
            cluster: "devnet".into(),
            rpc_url: None,
            ws_url: None,
            program_id: None,
            covnt_mint: None,
            ready: false,
            missing: vec!["rpc_url".into(), "program_id".into()],
        };
        let wire = serde_json::to_value(&unconfigured).unwrap();
        let obj = wire
            .as_object()
            .expect("ChainStatus serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "chain",
                "cluster",
                "covnt_mint",
                "missing",
                "program_id",
                "ready",
                "rpc_url",
                "ws_url",
            ],
            "ChainStatus wire object must always carry the eight documented \
             keys — adding skip_serializing_if to any Option field would \
             silently drop keys on the not-yet-configured path"
        );
        for nullable in ["rpc_url", "ws_url", "program_id", "covnt_mint"] {
            assert_eq!(
                obj.get(nullable),
                Some(&serde_json::Value::Null),
                "None {nullable} must surface as JSON null on the wire — \
                 dropping a key here would shift the chain-status shape \
                 for the not-yet-configured path"
            );
        }

        let configured = ChainStatus {
            chain: "solana".into(),
            cluster: "devnet".into(),
            rpc_url: Some("https://api.devnet.solana.com".into()),
            ws_url: Some("wss://api.devnet.solana.com".into()),
            program_id: Some("EUvV1vfsS5KwxHf6M6yLXKFwFKKSyxbjio7b5JH6DbX2".into()),
            covnt_mint: Some("4uTpj4kb8r1NbMGbTwNKoDPvrPpevGNZN2hP4FWUW58E".into()),
            ready: true,
            missing: vec![],
        };
        let decoded: ChainStatus =
            serde_json::from_value(serde_json::to_value(&configured).unwrap()).unwrap();
        assert_eq!(
            decoded, configured,
            "ChainStatus must round-trip every populated field verbatim — \
             the Eq derive is the contract every fixture-replay test leans on"
        );

        let full_obj = serde_json::to_value(&configured).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in ["chain", "cluster", "ready", "missing"] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<ChainStatus>(serde_json::Value::Object(payload)).is_err(),
                "ChainStatus must reject a wire payload that omits {required}; \
                 a stray #[serde(default)] introduction on any of the non-Option \
                 fields would silently let the field default and the CLI would \
                 render chain readiness inconsistently with the underlying \
                 configuration"
            );
        }

        for nullable in ["rpc_url", "ws_url", "program_id", "covnt_mint"] {
            let mut payload = full_map.clone();
            payload.remove(nullable);
            let decoded =
                serde_json::from_value::<ChainStatus>(serde_json::Value::Object(payload)).unwrap();
            let got = match nullable {
                "rpc_url" => decoded.rpc_url.as_deref(),
                "ws_url" => decoded.ws_url.as_deref(),
                "program_id" => decoded.program_id.as_deref(),
                "covnt_mint" => decoded.covnt_mint.as_deref(),
                _ => unreachable!(),
            };
            assert_eq!(
                got, None,
                "ChainStatus with {nullable} omitted must decode as None — \
                 serde's auto-default for Option<T> is the forward-compatible \
                 contract every stale CLI built before a Solana key landed leans on"
            );
        }

        // Wire-key rename detection lives in the eight-key set assertion
        // above: a refactor that renamed rpc_url to (say) rpc would change
        // the serialized key set, the sorted-keys assertion would go red,
        // and the rename would fail loud before any decode-side change
        // could silently shift consumers to the new shape.
    }

    #[test]
    fn receipt_batch_summary_serde_pins_default_not_skip_and_required_fields() {
        // ReceiptBatchSummary lives inside Response::ReceiptBatchFlushed
        // and Response::ReceiptBatches; every CLI `receipts flush` /
        // `receipts batches` output and HTTP `/receipts/batches` consumer
        // deserialises it. The struct documents the on-chain confirmation
        // boundary — batch_id / merkle_root / receipt_count are local-
        // evidence required fields, while tx_sig and slot reflect the
        // Solana confirmation state.
        //
        // Both Option fields carry `#[serde(default)]` but NOT
        // `#[serde(skip_serializing_if = "Option::is_none")]`, an
        // asymmetric contract:
        //
        // * Serialize: `None` surfaces as JSON `null` — the five-key wire
        //   shape stays stable across local-only and confirmed states so
        //   destructuring CLI/HTTP consumers never see a missing key.
        // * Deserialize: a missing `tx_sig` / `slot` decodes as `None`,
        //   so a stale CLI built before flush evidence existed (or a
        //   newer CLI talking to an older daemon that does not emit the
        //   keys) stays forward-compatible.
        //
        // A refactor that adds `skip_serializing_if` would silently
        // drop the keys on the local-only path; a refactor that drops
        // `#[serde(default)]` would break stale CLIs at decode time.

        let unconfirmed = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "00".repeat(32),
            receipt_count: 7,
            tx_sig: None,
            slot: None,
        };
        let wire = serde_json::to_value(&unconfirmed).unwrap();
        let obj = wire
            .as_object()
            .expect("ReceiptBatchSummary serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["batch_id", "merkle_root", "receipt_count", "slot", "tx_sig"],
            "ReceiptBatchSummary wire object must always carry the five \
             documented keys — adding skip_serializing_if to tx_sig or \
             slot would silently break callers that destructure the shape"
        );
        assert_eq!(
            obj.get("tx_sig"),
            Some(&serde_json::Value::Null),
            "None tx_sig must surface as JSON null on the wire so the \
             five-key shape stays stable across confirmation states"
        );
        assert_eq!(
            obj.get("slot"),
            Some(&serde_json::Value::Null),
            "None slot must surface as JSON null on the wire so the \
             five-key shape stays stable across confirmation states"
        );

        let confirmed = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "00".repeat(32),
            receipt_count: 7,
            tx_sig: Some("sig123".into()),
            slot: Some(42),
        };
        let wire = serde_json::to_value(&confirmed).unwrap();
        let obj = wire.as_object().unwrap();
        assert_eq!(
            obj.get("tx_sig").and_then(serde_json::Value::as_str),
            Some("sig123"),
            "populated tx_sig must round-trip verbatim on the wire"
        );
        assert_eq!(
            obj.get("slot").and_then(serde_json::Value::as_u64),
            Some(42),
            "populated slot must round-trip verbatim on the wire"
        );

        let forward_compat = serde_json::json!({
            "batch_id": "batch-1",
            "merkle_root": "00".repeat(32),
            "receipt_count": 7,
        });
        let decoded: ReceiptBatchSummary = serde_json::from_value(forward_compat).unwrap();
        assert_eq!(
            decoded.tx_sig, None,
            "ReceiptBatchSummary with tx_sig omitted must decode as None; \
             dropping #[serde(default)] would break stale CLIs built before \
             the flush evidence fields landed"
        );
        assert_eq!(
            decoded.slot, None,
            "ReceiptBatchSummary with slot omitted must decode as None; \
             dropping #[serde(default)] would break stale CLIs built before \
             the flush evidence fields landed"
        );

        for required in ["batch_id", "merkle_root", "receipt_count"] {
            let mut payload = serde_json::Map::new();
            payload.insert("batch_id".into(), serde_json::Value::String("batch-1".into()));
            payload.insert(
                "merkle_root".into(),
                serde_json::Value::String("00".repeat(32)),
            );
            payload.insert(
                "receipt_count".into(),
                serde_json::Value::Number(serde_json::Number::from(7u64)),
            );
            payload.remove(required);
            assert!(
                serde_json::from_value::<ReceiptBatchSummary>(serde_json::Value::Object(payload))
                    .is_err(),
                "ReceiptBatchSummary must reject a wire payload that omits \
                 {required}; a stray #[serde(default)] introduction would let \
                 a malformed flush evidence frame decode at the IPC boundary"
            );
        }

        let too_large = serde_json::json!({
            "batch_id": "batch-1",
            "merkle_root": "00".repeat(32),
            "receipt_count": u64::from(u32::MAX) + 1,
        });
        assert!(
            serde_json::from_value::<ReceiptBatchSummary>(too_large).is_err(),
            "receipt_count must remain a u32 on the wire; a refactor that \
             widens it to u64 would silently change the wire type contract \
             every batch-rendering consumer depends on"
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

    #[test]
    fn response_authenticated_serde_pins_single_field_variant() {
        // Response::Authenticated is the variant the daemon sends
        // after a successful Request::Authenticate handshake — it
        // carries display: String so the caller can confirm the
        // AgentId.display the daemon bound to the connection. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='authenticated' plus display. No prior test pins
        // the exact wire shape, round-trip, or omission rejection
        // for this variant — only the
        // v1_response_fixtures_replay_against_current_parser test
        // covers any handshake response indirectly. A refactor that
        // promoted Authenticated from a struct variant to a newtype
        // variant wrapping a payload struct would nest display one
        // level deeper next to 'kind' and the CLI could not extract
        // the bound identity; a slug regression would silently strand
        // every consumer that classifies handshakes by 'authenticated'.
        let event = Response::Authenticated {
            display: "operator@local".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["display", "kind"],
            "Response::Authenticated wire form must be exactly two \
             top-level keys: 'kind' plus the single 'display' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'display' \
             one level deeper and every CLI consumer that \
             destructures on the bound identity would silently fail \
             to confirm which AgentId the daemon resolved their \
             token to",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("authenticated")),
            "Response discriminator slug must be snake_case \
             'authenticated'; a slug regression silently strands \
             every CLI consumer that classifies handshakes by this \
             exact value — the operator's connection appears to \
             succeed but downstream verbs fall through to \
             AuthenticationFailed branches because the CLI cannot \
             recognise the auth-success class",
        );
        assert_eq!(
            obj.get("display").and_then(serde_json::Value::as_str),
            Some("operator@local"),
            "Response::Authenticated::display must surface as the \
             literal AgentId display string — operator triage scripts \
             grep this exact field to confirm which identity the \
             daemon bound on the connection",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Authenticated must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI handshake consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("display");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::Authenticated wire form must reject a payload \
             missing 'display'; a stray #[serde(default)] would let \
             a malformed row decode with display=String::new() and \
             the CLI would bind the connection to an empty identity \
             — every subsequent verb would appear to authenticate \
             against an empty AgentId and operator triage could not \
             map the connection back to a real peer registry row",
        );
    }

    #[test]
    fn response_authentication_failed_serde_pins_single_field_variant() {
        // Response::AuthenticationFailed is the variant the daemon
        // sends on every rejected handshake — bad/unknown/revoked
        // token, malformed first frame — before it closes the
        // connection. It carries reason: String, the short message
        // the operator sees in CLI output. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the wire
        // object is exactly two top-level keys: kind='authentication_failed'
        // plus reason. The sibling Response::Authenticated pin
        // already landed; this variant has no test pinning its exact
        // wire shape, round-trip, or omission rejection. A refactor
        // that promoted AuthenticationFailed from a struct variant
        // to a newtype variant would nest reason one level deeper
        // next to 'kind' and operator triage would see the
        // failed-auth discriminator but no diagnostic message,
        // forcing them to consult the daemon's audit log to learn
        // why the token was rejected.
        let event = Response::AuthenticationFailed {
            reason: "unknown token".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "reason"],
            "Response::AuthenticationFailed wire form must be \
             exactly two top-level keys: 'kind' plus the single \
             'reason' field. A refactor that promoted the variant \
             from struct to newtype wrapping a payload struct would \
             nest 'reason' one level deeper and every CLI consumer \
             that destructures on the diagnostic message would \
             silently drop it — the operator sees a failed-auth \
             discriminator but cannot tell why",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("authentication_failed")),
            "Response discriminator slug must be snake_case \
             'authentication_failed'; a slug regression silently \
             strands every CLI consumer that classifies handshake \
             failures by this exact value — the operator's CLI \
             prints a confusing fallback message and masks the \
             security-relevant signal that a bad token was rejected",
        );
        assert_eq!(
            obj.get("reason").and_then(serde_json::Value::as_str),
            Some("unknown token"),
            "Response::AuthenticationFailed::reason must surface as \
             the literal diagnostic string — the CLI prints this \
             verbatim and operator triage greps the audit log for \
             the matching short message",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::AuthenticationFailed must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI handshake failure consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("reason");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::AuthenticationFailed wire form must reject a \
             payload missing 'reason'; a stray #[serde(default)] \
             would let a malformed row decode with reason=String::new() \
             and the CLI would print an empty diagnostic — the \
             operator could not distinguish a real auth rejection \
             from a malformed daemon response and might retry with \
             the same bad token instead of rotating it",
        );
    }

    #[test]
    fn response_error_serde_pins_single_field_variant() {
        // Response::Error is the catch-all variant the daemon sends
        // for every error reply that does not fit a more specific
        // outcome — capability check failures, malformed requests,
        // internal errors. It carries message: String, the
        // operator-facing diagnostic. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the wire
        // object is exactly two top-level keys: kind='error' plus
        // message. No prior test pins the exact wire shape,
        // round-trip, or omission rejection for this variant. A
        // refactor that promoted Error from a struct variant to a
        // newtype variant would nest message one level deeper next
        // to 'kind' and operator triage would see the error
        // discriminator but no explanation; a slug regression on the
        // discriminator silently strands every CLI fallback error
        // branch that classifies by kind='error'.
        let event = Response::Error {
            message: "capability check failed".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "message"],
            "Response::Error wire form must be exactly two top-level \
             keys: 'kind' plus the single 'message' field. A refactor \
             that promoted the variant from struct to newtype \
             wrapping a payload struct would nest 'message' one level \
             deeper and every CLI consumer that surfaces the \
             operator-facing diagnostic would silently drop it — \
             operator triage would see the error discriminator but \
             cannot tell what failed",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("error")),
            "Response discriminator slug must be snake_case 'error'; \
             a slug regression silently strands every CLI fallback \
             error branch that classifies by this exact value — the \
             operator's CLI prints a confusing unknown-response \
             fallback instead of the daemon's diagnostic and masks \
             the signal that the operation failed at all",
        );
        assert_eq!(
            obj.get("message").and_then(serde_json::Value::as_str),
            Some("capability check failed"),
            "Response::Error::message must surface as the literal \
             diagnostic string — the CLI prints this verbatim and \
             operator triage greps the audit log for the matching \
             text",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Error must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI error consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("message");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::Error wire form must reject a payload missing \
             'message'; a stray #[serde(default)] would let a \
             malformed row decode with message=String::new() and the \
             CLI would print an empty diagnostic — the operator \
             could not distinguish a real error from a malformed \
             daemon response and might retry the same broken request \
             instead of escalating",
        );
    }

    #[test]
    fn response_capability_granted_serde_pins_three_field_variant() {
        // Response::CapabilityGranted is the variant the daemon
        // sends after a successful GrantCapability request — it
        // carries three fields the CLI surfaces to operators:
        // signature_b58 (the persisted SignedCapability signature,
        // base58), subject_display (the AgentId display the
        // capability is bound to), and action (the dispatch verb the
        // grant authorises). With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly four top-level keys: kind='capability_granted'
        // plus the three variant fields. No prior test pins the
        // exact wire shape, round-trip, or omission rejection. A
        // refactor that promoted CapabilityGranted from a struct
        // variant to a newtype variant would nest the three fields
        // one level deeper next to 'kind' and every CLI consumer
        // that prints the granted-capability confirmation would
        // silently drop the signature or subject — operator triage
        // could not tell which capability was actually granted or
        // to whom.
        let event = Response::CapabilityGranted {
            signature_b58: "sig-abc".into(),
            subject_display: "research@host".into(),
            action: "tool.call.fs.read".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["action", "kind", "signature_b58", "subject_display"],
            "Response::CapabilityGranted wire form must be exactly \
             four top-level keys: 'kind' plus the three variant \
             fields. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             the three fields one level deeper and every CLI \
             consumer that prints the granted-capability \
             confirmation would silently drop the signature or \
             subject",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("capability_granted")),
            "Response discriminator slug must be snake_case \
             'capability_granted'; a slug regression silently \
             strands every CLI parser that classifies grant outcomes \
             by this exact value — the operator cannot confirm the \
             grant succeeded and the audit row attribution becomes \
             ambiguous",
        );
        assert_eq!(
            obj.get("signature_b58").and_then(serde_json::Value::as_str),
            Some("sig-abc"),
            "Response::CapabilityGranted::signature_b58 must surface \
             as the literal base58 signature — operator triage greps \
             on this exact value to correlate the CLI confirmation \
             with the persisted SignedCapability row",
        );
        assert_eq!(
            obj.get("subject_display")
                .and_then(serde_json::Value::as_str),
            Some("research@host"),
            "Response::CapabilityGranted::subject_display must \
             surface as the literal AgentId display — the CLI prints \
             this verbatim so operators can confirm which agent the \
             capability is bound to before they act on the grant",
        );
        assert_eq!(
            obj.get("action").and_then(serde_json::Value::as_str),
            Some("tool.call.fs.read"),
            "Response::CapabilityGranted::action must surface as the \
             literal dispatch verb — a refactor that renamed or \
             skipped the field would strand operators that grep CLI \
             output for the matching capability slug",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::CapabilityGranted must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI grant-confirmation consumer leans on",
        );

        for required in ["signature_b58", "subject_display", "action"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::CapabilityGranted wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] on subject_display or action would \
                 let a malformed row decode with empty strings and \
                 the CLI would print a granted-capability \
                 confirmation with no subject or no action — \
                 operators could not grep their CLI output for the \
                 matching audit row and the post-grant audit trail \
                 would be silently broken",
            );
        }
    }

    #[test]
    fn response_capability_revoked_serde_pins_two_field_variant() {
        // Response::CapabilityRevoked is the variant the daemon
        // sends after a successful RevokeCapability request. It
        // carries signature_b58 (the SignedCapability signature
        // targeted for revocation, base58) and removed (bool — true
        // if a live binding was removed, false if the signature was
        // already tombstoned or never existed). With #[serde(tag =
        // "kind", rename_all = "snake_case")] on the Response enum,
        // the wire object is exactly three top-level keys:
        // kind='capability_revoked' plus the two variant fields. No
        // prior test pins the exact wire shape, round-trip, or
        // omission rejection. A refactor that added
        // skip_serializing_if on removed would drop the column when
        // false and stale CLIs that branch on key presence would
        // silently treat every revoke as if it had removed a live
        // binding — masking the idempotent already-revoked path.
        for (event, expected_removed) in [
            (
                Response::CapabilityRevoked {
                    signature_b58: "sig-xyz".into(),
                    removed: true,
                },
                true,
            ),
            (
                Response::CapabilityRevoked {
                    signature_b58: "sig-xyz".into(),
                    removed: false,
                },
                false,
            ),
        ] {
            let wire = serde_json::to_value(&event).unwrap();
            let obj = wire
                .as_object()
                .expect("Response serializes as a JSON object");
            let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(
                keys,
                vec!["kind", "removed", "signature_b58"],
                "Response::CapabilityRevoked wire form must be \
                 exactly three top-level keys for both removed=true \
                 and removed=false: 'kind' plus the two variant \
                 fields. A refactor that added skip_serializing_if \
                 on removed would drop the column when false and \
                 stale CLIs that branch on key presence would \
                 silently treat every revoke as a live-removal — \
                 masking the idempotent already-revoked path",
            );
            assert_eq!(
                obj.get("kind"),
                Some(&serde_json::json!("capability_revoked")),
                "Response discriminator slug must be snake_case \
                 'capability_revoked'; a slug regression silently \
                 strands every CLI parser that classifies revoke \
                 outcomes by this exact value — the operator cannot \
                 correlate the CLI confirmation with the persisted \
                 tombstone row",
            );
            assert_eq!(
                obj.get("signature_b58").and_then(serde_json::Value::as_str),
                Some("sig-xyz"),
                "Response::CapabilityRevoked::signature_b58 must \
                 surface as the literal base58 signature — operator \
                 triage greps on this exact value to find the \
                 tombstone row in the persisted capability store",
            );
            assert_eq!(
                obj.get("removed").and_then(serde_json::Value::as_bool),
                Some(expected_removed),
                "Response::CapabilityRevoked::removed must surface \
                 verbatim — the CLI distinguishes a live-removal \
                 from an idempotent already-revoked outcome by this \
                 exact bool",
            );

            let back: Response = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(
                back, event,
                "Response::CapabilityRevoked must round-trip through \
                 serde_json verbatim — the PartialEq derive is the \
                 contract every CLI revoke-confirmation consumer \
                 leans on",
            );
        }

        let wire = serde_json::to_value(Response::CapabilityRevoked {
            signature_b58: "sig-xyz".into(),
            removed: true,
        })
        .unwrap();
        let obj = wire.as_object().unwrap();
        for required in ["signature_b58", "removed"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::CapabilityRevoked wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] on removed would let a malformed \
                 row decode with removed=false and the CLI would \
                 silently render every revoke as already-revoked — \
                 a default on signature_b58 would let a malformed \
                 row decode with an empty string and operator triage \
                 could not correlate the response to the tombstone \
                 row",
            );
        }
    }

    #[test]
    fn response_a2a_task_queued_serde_pins_single_field_variant() {
        // Response::A2ATaskQueued is the variant the daemon sends
        // after SendA2ATask successfully enqueues a task on the
        // recipient's mailbox. It carries task_id: Uuid — the id the
        // CLI needs to correlate subsequent recv_result or repair
        // verbs to the queued task. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the wire
        // object is exactly two top-level keys: kind='a2_a_task_queued'
        // (the rename_all = snake_case rule splits A2A on the
        // digit/upper boundary into 'a2_a_...'; the durable wire form
        // matches the analogous AuditKind::A2A* slugs) plus task_id.
        // No prior test pins the exact wire shape,
        // round-trip, or omission rejection. A refactor that
        // promoted A2ATaskQueued from a struct variant to a newtype
        // variant would nest task_id one level deeper next to 'kind'
        // and the CLI could no longer follow the task through the
        // mailbox lifecycle; a stray #[serde(default)] on task_id
        // would let a malformed row decode with Uuid::nil() and
        // every subsequent recv_result or repair verb would correlate
        // against a phantom nil id.
        let task_id = Uuid::from_u128(91);
        let event = Response::A2ATaskQueued { task_id };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "task_id"],
            "Response::A2ATaskQueued wire form must be exactly two \
             top-level keys: 'kind' plus the single 'task_id' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'task_id' \
             one level deeper and every CLI consumer that correlates \
             subsequent recv_result or repair verbs against the \
             queued task id would silently fail",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_task_queued")),
            "Response discriminator slug must be the durable \
             'a2_a_task_queued' (rename_all = snake_case splits A2A \
             on digit/upper boundaries); a slug regression silently strands \
             every CLI parser that classifies enqueue outcomes by \
             this exact value — the operator's CLI prints a \
             confusing fallback instead of the queued-task \
             confirmation and the recipient's mailbox acceptance is \
             masked",
        );
        assert_eq!(
            obj.get("task_id").and_then(serde_json::Value::as_str),
            Some(task_id.to_string().as_str()),
            "Response::A2ATaskQueued::task_id must surface as the \
             Uuid's hyphenated string form — operator triage \
             correlates this exact representation with subsequent \
             recv_result, recv_task, and repair verbs",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2ATaskQueued must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI A2A enqueue consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("task_id");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2ATaskQueued wire form must reject a payload \
             missing 'task_id'; a stray #[serde(default)] would let \
             a malformed row decode with Uuid::nil() and the CLI \
             would correlate every subsequent recv_result or repair \
             verb against a phantom nil task id — operator triage \
             could not find the task in the mailbox queue and a \
             lease force-error could land against the wrong id",
        );
    }

    #[test]
    fn response_a2a_result_posted_serde_pins_single_field_variant() {
        // Response::A2AResultPosted is the variant the daemon sends
        // after PostA2AResult successfully writes a result back to
        // the originator peer's mailbox. It carries task_id: Uuid —
        // the id the originator needs so its CLI can correlate the
        // posted result against the originally sent task. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='a2_a_result_posted' (the rename_all = snake_case
        // rule splits A2A on the digit/upper boundary into
        // 'a2_a_...'; the durable wire form matches the analogous
        // AuditKind::A2A* slugs and the sibling A2ATaskQueued pin)
        // plus task_id. No prior test pins the exact wire shape,
        // round-trip, or omission rejection. A refactor that
        // promoted A2AResultPosted from a struct variant to a
        // newtype variant would nest task_id one level deeper next
        // to 'kind' and every CLI consumer that confirms the post
        // landed against the original task id would silently fail;
        // a stray #[serde(default)] on task_id would let a
        // malformed row decode with Uuid::nil() and the originator
        // side would bind the confirmation to a phantom nil task id.
        let task_id = Uuid::from_u128(123);
        let event = Response::A2AResultPosted { task_id };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "task_id"],
            "Response::A2AResultPosted wire form must be exactly two \
             top-level keys: 'kind' plus the single 'task_id' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'task_id' \
             one level deeper and every CLI consumer that confirms \
             a posted result against the original outbound task id \
             would silently fail",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_result_posted")),
            "Response discriminator slug must be the durable \
             'a2_a_result_posted' (rename_all = snake_case splits A2A \
             on digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies post-result \
             outcomes by this exact value — the operator's CLI prints \
             a confusing fallback instead of the result-posted \
             confirmation and the signal that the originator's \
             mailbox accepted the reply is masked",
        );
        assert_eq!(
            obj.get("task_id").and_then(serde_json::Value::as_str),
            Some(task_id.to_string().as_str()),
            "Response::A2AResultPosted::task_id must surface as the \
             Uuid's hyphenated string form — the originator's CLI \
             correlates this exact representation against the \
             originally sent task and against subsequent recv_result \
             lookups",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2AResultPosted must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI consumer that confirms a posted \
             result leans on",
        );

        let mut missing = obj.clone();
        missing.remove("task_id");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2AResultPosted wire form must reject a payload \
             missing 'task_id'; a stray #[serde(default)] would let \
             a malformed row decode with Uuid::nil() and the \
             originator's CLI would bind the post confirmation to a \
             phantom nil task id — operator triage could not match \
             the confirmation to the original outbound task and the \
             recv_result loop may surface or skip the wrong reply",
        );
    }

    #[test]
    fn response_operator_token_rotated_serde_pins_single_field_variant() {
        // Response::OperatorTokenRotated is the variant the daemon
        // sends after RotateOperatorToken installs a fresh operator
        // peer-auth token. It carries token_b58: String — the
        // base58-encoded new token the CLI must persist and use to
        // authenticate new connections (the daemon writes it to
        // $COVENANT_HOME/peers/operator.token mode 0600 and revokes
        // the prior token). With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly two top-level keys: kind='operator_token_rotated'
        // plus token_b58. No prior test pins the exact wire shape,
        // round-trip, or omission rejection. A refactor that
        // promoted OperatorTokenRotated from a struct variant to a
        // newtype variant would nest token_b58 one level deeper and
        // the CLI's rotation flow could no longer extract the new
        // token to persist; a stray #[serde(default)] on token_b58
        // would let a malformed row decode with an empty string and
        // the CLI would persist a zero-length token to
        // operator.token — bricking operator auth in single-peer v0
        // until manual recovery.
        let token_b58 = "5DkPzkAozsAjEvyZ7DJ2dEoEoVHHFvjpSizMV2ohHfMr".to_string();
        let event = Response::OperatorTokenRotated {
            token_b58: token_b58.clone(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "token_b58"],
            "Response::OperatorTokenRotated wire form must be exactly \
             two top-level keys: 'kind' plus the single 'token_b58' \
             field. A refactor that promoted the variant from struct \
             to newtype wrapping a payload struct would nest \
             'token_b58' one level deeper and the CLI's rotation \
             flow could no longer extract the new token — the \
             operator would be locked out of new connections in \
             single-peer v0 because the old token has already been \
             revoked",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("operator_token_rotated")),
            "Response discriminator slug must be the durable \
             'operator_token_rotated'; a slug regression silently \
             strands every CLI parser that classifies rotation \
             outcomes by this exact value — the operator's CLI \
             prints a confusing fallback message instead of \
             confirming rotation and may even discard the new token \
             while the daemon has already revoked the old one",
        );
        assert_eq!(
            obj.get("token_b58").and_then(serde_json::Value::as_str),
            Some(token_b58.as_str()),
            "Response::OperatorTokenRotated::token_b58 must surface \
             as the base58 string verbatim — the CLI persists this \
             exact byte sequence to $COVENANT_HOME/peers/operator.token \
             (mode 0600) and any transformation would silently \
             corrupt the token before the next authenticate call",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::OperatorTokenRotated must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI rotation consumer leans on to \
             confirm the new token bytes match what was sent",
        );

        let mut missing = obj.clone();
        missing.remove("token_b58");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::OperatorTokenRotated wire form must reject a \
             payload missing 'token_b58'; a stray #[serde(default)] \
             would let a malformed row decode with an empty string \
             and the CLI would persist a zero-length token to \
             operator.token — the file exists so the auth bootstrap \
             accepts it, but the empty bytes never match any \
             registry entry and every subsequent operator command \
             fails to authenticate, locking the operator out of \
             single-peer v0",
        );
    }

    #[test]
    fn response_a2a_compacted_serde_pins_single_field_variant() {
        // Response::A2ACompacted is the variant the daemon sends
        // after A2ACompact removes terminal or orphaned rows from
        // the local A2A mailbox. It carries dropped: u64 — the row
        // count the bounded compaction pass dropped, which the CLI
        // surfaces so the operator can confirm queue hygiene made
        // progress. With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly two top-level keys: kind='a2_a_compacted' (the
        // rename_all = snake_case rule splits A2A on the digit/upper
        // boundary into 'a2_a_...'; the durable wire form matches
        // the analogous AuditKind::A2A* slugs and the sibling
        // A2ATaskQueued/A2AResultPosted pins) plus dropped. No prior
        // test pins the exact wire shape, round-trip, or omission
        // rejection. A refactor that promoted A2ACompacted from a
        // struct variant to a newtype variant would nest dropped
        // one level deeper next to 'kind' and the CLI would read
        // zero for every compaction; a stray #[serde(default)] on
        // dropped would let a malformed row decode with 0 and the
        // operator would believe every compaction is a no-op — the
        // only progress signal the bounded compactor exposes.
        let event = Response::A2ACompacted { dropped: 17 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["dropped", "kind"],
            "Response::A2ACompacted wire form must be exactly two \
             top-level keys: 'kind' plus the single 'dropped' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'dropped' one level deeper and the CLI surface that \
             confirms compaction progress would silently read zero \
             — operator triage could not tell whether the bounded \
             compactor made progress or hit a corruption guard",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_compacted")),
            "Response discriminator slug must be the durable \
             'a2_a_compacted' (rename_all = snake_case splits A2A \
             on digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies compaction \
             outcomes by this exact value — the operator's CLI \
             prints a confusing fallback instead of confirming the \
             dropped-row count and masks the only signal the \
             bounded compactor exposes",
        );
        assert_eq!(
            obj.get("dropped").and_then(serde_json::Value::as_u64),
            Some(17),
            "Response::A2ACompacted::dropped must surface as the \
             u64 row count verbatim — the operator reads this \
             exact integer to confirm A2A queue hygiene made \
             progress",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2ACompacted must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI compaction consumer leans on to \
             render the dropped-row count to the operator",
        );

        let mut missing = obj.clone();
        missing.remove("dropped");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2ACompacted wire form must reject a payload \
             missing 'dropped'; a stray #[serde(default)] would let \
             a malformed row decode with 0 and the CLI would \
             surface a phantom zero count — every compaction would \
             look like a no-op to the operator even when many rows \
             were dropped, breaking the metric operators use to \
             confirm A2A queue hygiene",
        );
    }

    #[test]
    fn response_memory_purged_serde_pins_single_field_variant() {
        // Response::MemoryPurged is the variant the daemon sends
        // after PurgeMemories removes scoped memory rows matching
        // the requested predicate. It carries purged: u64 — the
        // count of memory rows the predicate matched and removed,
        // which the CLI surfaces so the operator can confirm the
        // destructive purge took effect. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the
        // wire object is exactly two top-level keys:
        // kind='memory_purged' plus purged. No prior test pins the
        // exact wire shape, round-trip, or omission rejection. A
        // refactor that promoted MemoryPurged from a struct variant
        // to a newtype variant would nest purged one level deeper
        // and operator-facing CLIs would silently read zero; a
        // stray #[serde(default)] on purged would let a malformed
        // row decode with 0 and the operator would believe every
        // destructive memory purge is a no-op even when many rows
        // were removed — tempting duplicate destructive runs
        // against an already-cleared scope.
        let event = Response::MemoryPurged { purged: 42 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "purged"],
            "Response::MemoryPurged wire form must be exactly two \
             top-level keys: 'kind' plus the single 'purged' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'purged' \
             one level deeper and every CLI consumer that confirms \
             the destructive purge removed rows would silently read \
             zero — operator triage could not distinguish a \
             successful destructive purge from a no-op match",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("memory_purged")),
            "Response discriminator slug must be the durable \
             'memory_purged'; a slug regression silently strands \
             every CLI parser that classifies purge outcomes by \
             this exact value — the operator's CLI prints a \
             confusing fallback instead of confirming the purged \
             count and may tempt the operator to re-issue the \
             destructive command",
        );
        assert_eq!(
            obj.get("purged").and_then(serde_json::Value::as_u64),
            Some(42),
            "Response::MemoryPurged::purged must surface as the u64 \
             row count verbatim — the operator reads this exact \
             integer to confirm the scoped memory purge removed the \
             expected number of rows",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::MemoryPurged must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI purge consumer leans on to render \
             the destructive removal count to the operator",
        );

        let mut missing = obj.clone();
        missing.remove("purged");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::MemoryPurged wire form must reject a payload \
             missing 'purged'; a stray #[serde(default)] would let \
             a malformed row decode with 0 and the CLI would \
             surface a phantom zero count — the destructive memory \
             purge would look like a no-op even when many rows \
             were removed, risking duplicate destructive runs \
             against an already-cleared scope",
        );
    }

    #[test]
    fn response_audit_purged_serde_pins_single_field_variant() {
        // Response::AuditPurged is the variant the daemon sends
        // after PurgeAudit removes audit-log rows older than the
        // cutoff that the caller's signed capability authorizes.
        // It carries purged: u64 — the count of audit rows the
        // cutoff dropped, which the CLI surfaces so the operator
        // can confirm the destructive retention pass took effect.
        // With #[serde(tag = "kind", rename_all = "snake_case")] on
        // the Response enum, the wire object is exactly two
        // top-level keys: kind='audit_purged' plus purged. No prior
        // test pins the exact wire shape, round-trip, or omission
        // rejection. A refactor that promoted AuditPurged from a
        // struct variant to a newtype variant would nest purged one
        // level deeper and operator-facing CLIs would silently read
        // zero; a stray #[serde(default)] on purged would let a
        // malformed row decode with 0 and the operator would
        // believe the destructive audit-log trim is a no-op even
        // when many rows were removed — risking duplicate
        // destructive runs against an already-trimmed audit log.
        let event = Response::AuditPurged { purged: 13 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "purged"],
            "Response::AuditPurged wire form must be exactly two \
             top-level keys: 'kind' plus the single 'purged' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'purged' \
             one level deeper and every CLI consumer that confirms \
             the destructive audit-log retention pass dropped rows \
             would silently read zero — operator triage could not \
             distinguish a successful destructive trim from a no-op \
             match",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("audit_purged")),
            "Response discriminator slug must be the durable \
             'audit_purged'; a slug regression silently strands \
             every CLI parser that classifies audit-purge outcomes \
             by this exact value — the operator's CLI prints a \
             confusing fallback instead of confirming the \
             dropped-row count and may tempt the operator to \
             re-issue the destructive command",
        );
        assert_eq!(
            obj.get("purged").and_then(serde_json::Value::as_u64),
            Some(13),
            "Response::AuditPurged::purged must surface as the u64 \
             row count verbatim — the operator reads this exact \
             integer to confirm the audit-log retention pass \
             advanced",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::AuditPurged must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI audit-purge consumer leans on to \
             render the destructive removal count to the operator",
        );

        let mut missing = obj.clone();
        missing.remove("purged");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::AuditPurged wire form must reject a payload \
             missing 'purged'; a stray #[serde(default)] would let \
             a malformed row decode with 0 and the CLI would \
             surface a phantom zero count — the destructive \
             audit-log trim would look like a no-op even when many \
             rows were removed, risking duplicate destructive runs \
             against an already-trimmed audit log",
        );
    }

    #[test]
    fn response_capabilities_purged_serde_pins_single_field_variant() {
        // Response::CapabilitiesPurged is the variant the daemon
        // sends after PurgeCapabilities removes expired or revoked
        // signed-capability rows that the caller's authorization
        // permits. It carries purged: u64 — the count of capability
        // rows the purge removed, which the CLI surfaces so the
        // operator can confirm the destructive capability-registry
        // cleanup took effect. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the
        // wire object is exactly two top-level keys:
        // kind='capabilities_purged' plus purged. No prior test
        // pins the exact wire shape, round-trip, or omission
        // rejection. A refactor that promoted CapabilitiesPurged
        // from a struct variant to a newtype variant would nest
        // purged one level deeper and operator-facing CLIs would
        // silently read zero; a stray #[serde(default)] on purged
        // would let a malformed row decode with 0 and the operator
        // would believe the destructive capability-registry trim
        // is a no-op even when many rows were removed — risking
        // duplicate destructive runs against an already-cleared
        // registry.
        let event = Response::CapabilitiesPurged { purged: 5 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "purged"],
            "Response::CapabilitiesPurged wire form must be exactly \
             two top-level keys: 'kind' plus the single 'purged' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'purged' one level deeper and every CLI consumer that \
             confirms the capability-registry purge removed rows \
             would silently read zero — operator triage could not \
             distinguish a successful destructive trim from a no-op \
             match",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("capabilities_purged")),
            "Response discriminator slug must be the durable \
             'capabilities_purged'; a slug regression silently \
             strands every CLI parser that classifies \
             capability-purge outcomes by this exact value — the \
             operator's CLI prints a confusing fallback instead of \
             confirming the dropped-row count and masks the \
             destructive operation's only success signal",
        );
        assert_eq!(
            obj.get("purged").and_then(serde_json::Value::as_u64),
            Some(5),
            "Response::CapabilitiesPurged::purged must surface as \
             the u64 row count verbatim — the operator reads this \
             exact integer to confirm expired-capability cleanup \
             advanced",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::CapabilitiesPurged must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI capability-purge consumer leans on \
             to render the destructive removal count to the \
             operator",
        );

        let mut missing = obj.clone();
        missing.remove("purged");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::CapabilitiesPurged wire form must reject a \
             payload missing 'purged'; a stray #[serde(default)] \
             would let a malformed row decode with 0 and the CLI \
             would surface a phantom zero count — the destructive \
             capability-registry trim would look like a no-op even \
             when many rows were removed, risking duplicate \
             destructive runs against an already-cleared registry",
        );
    }

    #[test]
    fn response_peers_purged_serde_pins_single_field_variant() {
        // Response::PeersPurged is the variant the daemon sends
        // after PurgePeers removes revoked peer-registry rows that
        // the caller's authorization permits. It carries purged:
        // u64 — the count of peer rows the purge removed, which
        // the CLI surfaces so the operator can confirm the
        // destructive peer-registry cleanup took effect. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='peers_purged' plus purged. No prior test
        // pins the exact wire shape, round-trip, or omission
        // rejection. A refactor that promoted PeersPurged from a
        // struct variant to a newtype variant would nest purged
        // one level deeper and operator-facing CLIs would silently
        // read zero; a stray #[serde(default)] on purged would let
        // a malformed row decode with 0 and the operator would
        // believe the destructive peer-registry trim is a no-op
        // even when many rows were removed — risking duplicate
        // destructive runs against an already-cleared registry.
        let event = Response::PeersPurged { purged: 3 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "purged"],
            "Response::PeersPurged wire form must be exactly two \
             top-level keys: 'kind' plus the single 'purged' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'purged' \
             one level deeper and every CLI consumer that confirms \
             the peer-registry purge removed rows would silently \
             read zero — operator triage could not distinguish a \
             successful destructive trim from a no-op match",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("peers_purged")),
            "Response discriminator slug must be the durable \
             'peers_purged'; a slug regression silently strands \
             every CLI parser that classifies peer-purge outcomes \
             by this exact value — the operator's CLI prints a \
             confusing fallback instead of confirming the \
             dropped-row count and masks the destructive \
             operation's only success signal",
        );
        assert_eq!(
            obj.get("purged").and_then(serde_json::Value::as_u64),
            Some(3),
            "Response::PeersPurged::purged must surface as the u64 \
             row count verbatim — the operator reads this exact \
             integer to confirm revoked-peer cleanup advanced",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::PeersPurged must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI peer-purge consumer leans on to \
             render the destructive removal count to the operator",
        );

        let mut missing = obj.clone();
        missing.remove("purged");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::PeersPurged wire form must reject a payload \
             missing 'purged'; a stray #[serde(default)] would let \
             a malformed row decode with 0 and the CLI would \
             surface a phantom zero count — the destructive \
             peer-registry trim would look like a no-op even when \
             many rows were removed, risking duplicate destructive \
             runs against an already-cleared registry",
        );
    }

    #[test]
    fn response_tool_result_serde_pins_two_field_variant() {
        // Response::ToolResult is the variant the daemon sends
        // after a ToolCall reaches an MCP tool and the tool
        // produces a result. It carries content: Vec<Content> (the
        // MCP content blocks the tool returned) and is_error: bool
        // (whether the tool reported an in-band error — distinct
        // from a transport-level error). With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the
        // wire object is exactly three top-level keys:
        // kind='tool_result' plus content plus is_error. No prior
        // test pins the exact wire shape, round-trip, or omission
        // rejection. A refactor that promoted ToolResult from a
        // struct variant to a newtype variant would nest both
        // fields one level deeper; a stray #[serde(default)] on
        // is_error would let a malformed row decode with
        // is_error=false and an actual in-band tool error would be
        // silently reclassified as a successful call — downstream
        // automation that branches on is_error never trips and the
        // operator's UI shows the error string but the call is
        // treated as healthy.
        for (event, expected_is_error) in [
            (
                Response::ToolResult {
                    content: vec![Content::text("ok")],
                    is_error: false,
                },
                false,
            ),
            (
                Response::ToolResult {
                    content: vec![Content::text("ok")],
                    is_error: true,
                },
                true,
            ),
        ] {
            let wire = serde_json::to_value(&event).unwrap();
            let obj = wire
                .as_object()
                .expect("Response serializes as a JSON object");
            let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(
                keys,
                vec!["content", "is_error", "kind"],
                "Response::ToolResult wire form must be exactly \
                 three top-level keys for both is_error=true and \
                 is_error=false: 'kind' plus the two variant \
                 fields. A refactor that promoted the variant from \
                 struct to newtype wrapping a payload struct would \
                 nest 'content' and 'is_error' one level deeper and \
                 every CLI consumer that destructures the top-level \
                 fields would silently fail",
            );
            assert_eq!(
                obj.get("kind"),
                Some(&serde_json::json!("tool_result")),
                "Response discriminator slug must be the durable \
                 'tool_result'; a slug regression silently strands \
                 every CLI parser that classifies tool-call \
                 outcomes by this exact value — the operator's CLI \
                 prints a confusing fallback instead of the tool's \
                 response and masks both the tool output and the \
                 is_error signal",
            );
            assert_eq!(
                obj.get("is_error").and_then(serde_json::Value::as_bool),
                Some(expected_is_error),
                "Response::ToolResult::is_error must surface as the \
                 bool verbatim — downstream automation distinguishes \
                 a healthy tool call from an in-band tool error by \
                 this exact bool",
            );
            let content_arr = obj
                .get("content")
                .and_then(serde_json::Value::as_array)
                .expect("Response::ToolResult::content must serialize as an array");
            assert_eq!(
                content_arr.len(),
                1,
                "Response::ToolResult::content must round-trip the \
                 exact element count from the wire payload — a \
                 length regression silently truncates tool output \
                 blocks before the operator's UI renders them",
            );

            let back: Response = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(
                back, event,
                "Response::ToolResult must round-trip through \
                 serde_json verbatim — the PartialEq derive is the \
                 contract every CLI tool-call consumer leans on to \
                 render the tool output and classify success vs. \
                 in-band error",
            );
        }

        let wire = serde_json::to_value(Response::ToolResult {
            content: vec![Content::text("ok")],
            is_error: false,
        })
        .unwrap();
        let obj = wire.as_object().unwrap();
        for required in ["content", "is_error"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::ToolResult wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] on is_error would let a malformed \
                 row decode with is_error=false and an actual \
                 in-band tool error would be silently reclassified \
                 as a successful call — a default on content would \
                 let a malformed row decode with an empty content \
                 vec, masquerading as a successful no-output tool \
                 call",
            );
        }
    }

    #[test]
    fn response_pong_serde_pins_unit_variant() {
        // Response::Pong is the unit variant the daemon sends in
        // response to Request::Ping — it carries no payload. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire form for a unit variant is the
        // discriminator slug alone, exactly one top-level key:
        // kind='pong'. No prior test pins the exact wire shape or
        // round-trip. A refactor that promoted Pong from a unit
        // variant to a struct or newtype variant carrying a payload
        // would add a second top-level key on the wire and silently
        // break every CLI heartbeat consumer that classifies the
        // ping response by kind='pong' alone; a slug regression
        // silently strands every liveness probe that uses this
        // exact value and the operator's CLI reports the daemon as
        // unresponsive even when it is fully healthy.
        let event = Response::Pong;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Response::Pong wire form must be exactly one top-level \
             key: 'kind'. A refactor that promoted the variant from \
             unit to struct or newtype would add a second top-level \
             key and every CLI heartbeat consumer that classifies \
             the ping response by 'kind' alone would silently fail \
             — the operator's connection probe would report the \
             daemon as unresponsive even when it is healthy",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("pong")),
            "Response discriminator slug must be the durable \
             'pong'; a slug regression silently strands every CLI \
             heartbeat parser that uses this exact value as the \
             liveness signal — the CLI reports the daemon as \
             unresponsive even when it is fully healthy and \
             masking liveness is exactly the failure mode the \
             ping/pong round-trip is designed to detect",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Pong must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI ping consumer leans on to confirm liveness",
        );
    }

    #[test]
    fn response_capabilities_serde_pins_single_field_variant() {
        // Response::Capabilities is the variant the daemon sends
        // after ListCapabilities returns the SignedCapability rows
        // the caller's authorization permits. It carries
        // capabilities: Vec<SignedCapability> — the capability list
        // the CLI renders to the operator. With #[serde(tag =
        // "kind", rename_all = "snake_case")] on the Response enum,
        // the wire object is exactly two top-level keys:
        // kind='capabilities' plus capabilities. No prior test pins
        // the exact wire shape, round-trip, or omission rejection
        // of this variant's required field. The inner
        // SignedCapability element wire form is pinned by
        // covenant-permissions tests; this slice locks the outer
        // Response variant shape only — an empty Vec is sufficient
        // to catch the slug, key set, and default-attribute
        // regressions on the outer variant. A refactor that
        // promoted Capabilities from a struct variant to a newtype
        // variant would nest 'capabilities' one level deeper; a
        // stray #[serde(default)] on capabilities would let a
        // malformed row decode with an empty list and the operator
        // would see a phantom empty registry — masking a real
        // fetch failure as a clean state.
        let event = Response::Capabilities {
            capabilities: vec![],
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["capabilities", "kind"],
            "Response::Capabilities wire form must be exactly two \
             top-level keys: 'kind' plus the single 'capabilities' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'capabilities' one level deeper and every CLI consumer \
             that destructures the top-level array would silently \
             fail — the operator's capability list would render \
             empty even when the daemon returned a populated \
             registry",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("capabilities")),
            "Response discriminator slug must be the durable \
             'capabilities'; a slug regression silently strands \
             every CLI parser that classifies capability-list \
             outcomes by this exact value — the operator's CLI \
             prints a confusing fallback instead of rendering the \
             capability list",
        );
        let capabilities_arr = obj
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .expect("Response::Capabilities::capabilities must serialize as an array");
        assert_eq!(
            capabilities_arr.len(),
            0,
            "Response::Capabilities::capabilities must round-trip \
             the exact element count from the wire payload — the \
             empty-vec construction is sufficient to lock the outer \
             variant shape; element-level wire form is pinned by \
             covenant-permissions",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Capabilities must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI capability-list consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("capabilities");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::Capabilities wire form must reject a payload \
             missing 'capabilities'; a stray #[serde(default)] \
             would let a malformed row decode with an empty list \
             and the CLI would surface a phantom empty registry — \
             a real fetch failure (truncated frame, partial decode \
             error) would be silently reclassified as a clean \
             state where the operator believes no capabilities \
             exist when the daemon's registry is in fact populated",
        );
    }

    #[test]
    fn response_intent_result_serde_pins_five_field_variant() {
        // Response::IntentResult is the variant the daemon sends
        // after dispatch_intent completes — it carries intent_id,
        // status, text, sources, and settlement (Option<SettlementReceipt>).
        // With #[serde(tag = "kind", rename_all = "snake_case")] on
        // the Response enum, the wire object is exactly six keys:
        // kind='intent_result' plus the five variant fields. The
        // sibling intent_result_serialises_settlement_null test only
        // does substring matches on the serialised JSON; it does not
        // pin the exact key set, the round-trip PartialEq, or
        // omission rejection on the four required fields. A refactor
        // that promoted IntentResult from a struct variant to a
        // newtype variant wrapping a payload struct would nest the
        // five fields one level deeper next to 'kind' and break every
        // CLI consumer that destructures on intent_id / status / text
        // / sources / settlement; a stray skip_serializing_if =
        // Option::is_none on settlement would drop the column for
        // legacy intent rows and stale CLIs that branch on key
        // presence would silently treat every unsettled result as if
        // the field had never existed.
        let intent_id = Uuid::from_u128(81);
        let event = Response::IntentResult {
            intent_id,
            status: "ok".into(),
            text: "echo".into(),
            sources: vec!["path/one.md".into()],
            settlement: None,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["intent_id", "kind", "settlement", "sources", "status", "text"],
            "Response::IntentResult wire form must be exactly six \
             top-level keys: 'kind' plus the five variant fields. A \
             refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest the five \
             fields one level deeper and every CLI consumer that \
             destructures on intent_id / status / text / sources / \
             settlement would silently fail to extract the result",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("intent_result")),
            "Response discriminator slug must be snake_case \
             'intent_result'; a slug regression silently strands \
             every CLI consumer that classifies dispatch outcomes by \
             this exact value — the daemon returns a successful \
             response but the CLI cannot map it back to a successful \
             dispatch",
        );
        assert_eq!(
            obj.get("intent_id").and_then(serde_json::Value::as_str),
            Some(intent_id.to_string().as_str()),
            "Response::IntentResult::intent_id must surface as the \
             Uuid's hyphenated string form — operator triage scripts \
             correlate audit rows with intent results by this exact \
             representation",
        );
        assert_eq!(
            obj.get("settlement"),
            Some(&serde_json::Value::Null),
            "Response::IntentResult::settlement must surface as null \
             when the result has not settled; there is no \
             skip_serializing_if applied, so a refactor that added it \
             would drop the column for legacy unsettled rows and \
             stale CLIs that branch on key presence would silently \
             degrade to treating every result as if the field had \
             never existed",
        );
        assert_eq!(
            obj.get("sources"),
            Some(&serde_json::json!(["path/one.md"])),
            "Response::IntentResult::sources must surface as a JSON \
             array even when it holds a single source — a refactor \
             that flattened or renamed the field would break the \
             operator's at-a-glance view of which memory rows \
             contributed to the dispatched intent",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::IntentResult must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI consumer leans on when matching \
             dispatch outcomes",
        );

        for required in ["intent_id", "status", "text", "sources"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::IntentResult wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] on intent_id would let a malformed \
                 row decode with Uuid::nil() and operator triage \
                 would attribute the result to a phantom dispatch — \
                 a default on status would mask whether the daemon \
                 succeeded or fell through to a fail-open arm",
            );
        }
    }

    #[test]
    fn response_memories_serde_pins_single_field_variant() {
        // Response::Memories is the variant the daemon sends after
        // RecentMemory returns the MemoryRecord rows the caller's
        // authorization permits. It carries records:
        // Vec<MemoryRecord> — the recent-memory list the CLI renders
        // to the operator. With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly two top-level keys: kind='memories' plus records.
        // No prior test pins the exact wire shape, round-trip, or
        // omission rejection of this variant's required field. The
        // inner MemoryRecord element wire form is pinned by
        // covenant-memory tests; this slice locks the outer Response
        // variant shape only — an empty Vec is sufficient to catch
        // the slug, key set, and default-attribute regressions on the
        // outer variant. A refactor that promoted Memories from a
        // struct variant to a newtype variant would nest 'records'
        // one level deeper; a stray #[serde(default)] on records
        // would let a malformed row decode with an empty list and
        // the operator would see a phantom empty store — masking a
        // real fetch failure as a clean state.
        let event = Response::Memories { records: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "records"],
            "Response::Memories wire form must be exactly two \
             top-level keys: 'kind' plus the single 'records' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'records' one level deeper and every CLI consumer that \
             destructures the top-level array would silently fail — \
             the operator's recent-memory list would render empty \
             even when the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("memories")),
            "Response discriminator slug must be the durable \
             'memories'; a slug regression silently strands every \
             CLI parser that classifies recent-memory outcomes by \
             this exact value — the operator's CLI prints a \
             confusing fallback instead of rendering the memory \
             list",
        );
        let records_arr = obj
            .get("records")
            .and_then(serde_json::Value::as_array)
            .expect("Response::Memories::records must serialize as an array");
        assert_eq!(
            records_arr.len(),
            0,
            "Response::Memories::records must round-trip the exact \
             element count from the wire payload — the empty-vec \
             construction is sufficient to lock the outer variant \
             shape; element-level wire form is pinned by \
             covenant-memory",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Memories must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI recent-memory consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("records");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::Memories wire form must reject a payload \
             missing 'records'; a stray #[serde(default)] would let \
             a malformed row decode with an empty list and the CLI \
             would surface a phantom empty store — a real fetch \
             failure (truncated frame, partial decode error) would \
             be silently reclassified as a clean state where the \
             operator believes no recent memories exist when the \
             daemon's store is in fact populated",
        );
    }

    #[test]
    fn response_receipts_serde_pins_single_field_variant() {
        // Response::Receipts is the variant the daemon sends after
        // RecentReceipts returns the SettlementReceipt rows the
        // caller's authorization permits. It carries receipts:
        // Vec<SettlementReceipt> — the receipt list the CLI renders
        // to the operator. With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly two top-level keys: kind='receipts' plus receipts.
        // No prior test pins the exact wire shape, round-trip, or
        // omission rejection of this variant's required field. The
        // inner SettlementReceipt element wire form is pinned by
        // covenant-settlement tests; this slice locks the outer
        // Response variant shape only — an empty Vec is sufficient
        // to catch the slug, key set, and default-attribute
        // regressions on the outer variant. A refactor that promoted
        // Receipts from a struct variant to a newtype variant would
        // nest 'receipts' one level deeper; a stray #[serde(default)]
        // on receipts would let a malformed row decode with an empty
        // list and the operator would see a phantom empty settlement
        // history — masking a real fetch failure as a clean state.
        let event = Response::Receipts { receipts: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "receipts"],
            "Response::Receipts wire form must be exactly two \
             top-level keys: 'kind' plus the single 'receipts' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'receipts' one level deeper and every CLI consumer \
             that destructures the top-level array would silently \
             fail — the operator's receipt list would render empty \
             even when the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("receipts")),
            "Response discriminator slug must be the durable \
             'receipts'; a slug regression silently strands every \
             CLI parser that classifies recent-receipts outcomes by \
             this exact value — the operator's CLI prints a \
             confusing fallback instead of rendering the receipt \
             list",
        );
        let receipts_arr = obj
            .get("receipts")
            .and_then(serde_json::Value::as_array)
            .expect("Response::Receipts::receipts must serialize as an array");
        assert_eq!(
            receipts_arr.len(),
            0,
            "Response::Receipts::receipts must round-trip the exact \
             element count from the wire payload — the empty-vec \
             construction is sufficient to lock the outer variant \
             shape; element-level wire form is pinned by \
             covenant-settlement",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Receipts must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI recent-receipts consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("receipts");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::Receipts wire form must reject a payload \
             missing 'receipts'; a stray #[serde(default)] would \
             let a malformed row decode with an empty list and the \
             CLI would surface a phantom empty settlement history — \
             a real fetch failure (truncated frame, partial decode \
             error) would be silently reclassified as a clean state \
             where the operator believes no settlement receipts \
             exist when the daemon's settlement store is in fact \
             populated",
        );
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
