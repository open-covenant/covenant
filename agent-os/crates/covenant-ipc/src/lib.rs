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

    #[test]
    fn request_ping_serde_pins_unit_variant() {
        // Request::Ping is the unit variant the CLI sends as a
        // liveness probe to confirm the daemon's IPC loop is
        // responsive — it carries no payload and pairs with
        // Response::Pong (already pinned by
        // response_pong_serde_pins_unit_variant). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire form is the discriminator slug
        // alone, exactly one top-level key: kind='ping'. No prior
        // test pins the exact wire shape or round-trip of this
        // variant. A refactor that promoted Ping from a unit
        // variant to a struct or newtype variant carrying a
        // payload would add a second required key on the wire and
        // silently break every CLI heartbeat that sends
        // Request::Ping; a slug regression silently strands every
        // liveness probe by routing it to the daemon's error
        // branch instead of the Pong path.
        let event = Request::Ping;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::Ping wire form must be exactly one top-level \
             key: 'kind'. A refactor that promoted the variant from \
             unit to struct or newtype would add a second top-level \
             key and every CLI heartbeat that sends {{\"kind\":\"ping\"}} \
             alone would fail to decode on the daemon side — the \
             operator's liveness probes would silently start \
             returning Error responses instead of Pong, masking \
             actual daemon health behind a serde-shape regression",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("ping")),
            "Request discriminator slug must be the durable \
             'ping'; a slug regression silently routes incoming \
             liveness frames to the daemon's catch-all error \
             branch instead of the Pong path — every CLI liveness \
             probe reports the daemon as unresponsive even when it \
             is fully healthy, masking liveness exactly the way \
             the ping/pong round-trip is designed to detect",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::Ping must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI heartbeat consumer leans on to confirm liveness",
        );
    }

    #[test]
    fn request_protocol_info_serde_pins_unit_variant() {
        // Request::ProtocolInfo is the unit variant the CLI sends
        // BEFORE Authenticate to negotiate IPC protocol
        // compatibility — it is the only request the daemon
        // answers without a privileged token, and it pairs with
        // Response::ProtocolInfo (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire form is exactly one top-level
        // key: kind='protocol_info'. No prior test pins the exact
        // wire shape or round-trip of this variant. The
        // unauthenticated handshake is load-bearing — a slug
        // regression or newtype promotion silently routes the
        // negotiation frame to the daemon's authentication-required
        // branch, and stale CLIs talking to a newer daemon (or
        // vice versa) lose the documented version-handshake escape
        // hatch — every connection looks like an authentication
        // failure instead of a protocol-mismatch.
        let event = Request::ProtocolInfo;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::ProtocolInfo wire form must be exactly one \
             top-level key: 'kind'. A refactor that promoted the \
             variant from unit to struct or newtype would add a \
             second top-level key and every CLI handshake that \
             sends {{\"kind\":\"protocol_info\"}} alone would fail \
             to decode on the daemon side — the operator's stale \
             CLI would get routed to the authentication-required \
             branch and the documented protocol-negotiation escape \
             hatch would silently disappear",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("protocol_info")),
            "Request discriminator slug must be the durable \
             'protocol_info'; a slug regression silently routes \
             incoming negotiation frames to the daemon's catch-all \
             error branch — every CLI version-negotiation fails \
             with a confusing authentication-required error instead \
             of a protocol-mismatch error, masking the version skew \
             during incident triage",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::ProtocolInfo must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI protocol-negotiation consumer \
             leans on",
        );
    }

    #[test]
    fn request_chain_status_serde_pins_unit_variant() {
        // Request::ChainStatus is the unit variant the CLI sends
        // after Authenticate to dump the daemon's on-chain
        // readiness state — it pairs with Response::ChainStatus
        // (already pinned) which surfaces the settlement chain,
        // cluster, RPC URLs, program ID, mint, ready flag, and
        // missing-key list. With #[serde(tag = "kind", rename_all
        // = "snake_case")] on the Request enum, the wire form is
        // exactly one top-level key: kind='chain_status'. No prior
        // test pins the exact wire shape or round-trip of this
        // variant. The chain-status probe is the surface
        // operators use to confirm settlement is wired up before
        // flushing receipts or anchoring batches; a slug
        // regression silently strands the probe and the operator's
        // CLI prints an authentication-error fallback where the
        // real failure is a serde-shape regression.
        let event = Request::ChainStatus;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::ChainStatus wire form must be exactly one \
             top-level key: 'kind'. A refactor that promoted the \
             variant from unit to struct or newtype would add a \
             second top-level key and every CLI chain-status probe \
             that sends {{\"kind\":\"chain_status\"}} alone would \
             fail to decode on the daemon side — the operator's \
             pre-flush readiness check would silently start \
             returning Error responses instead of ChainStatus",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("chain_status")),
            "Request discriminator slug must be the durable \
             'chain_status'; a slug regression silently routes \
             incoming readiness-probe frames to the daemon's \
             catch-all error branch — every CLI chain readiness \
             probe fails with a confusing fallback message instead \
             of the settlement-state snapshot, masking on-chain \
             wiring gaps during incident triage",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::ChainStatus must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI chain-readiness consumer leans on",
        );
    }

    #[test]
    fn request_list_tools_serde_pins_unit_variant() {
        // Request::ListTools is the unit variant the CLI sends
        // after Authenticate to dump the daemon's registered-tool
        // inventory — it pairs with Response::ToolList (already
        // pinned) which surfaces the Vec<ToolSpec> the operator's
        // CLI renders for `tools list`. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Request enum, the
        // wire form is exactly one top-level key:
        // kind='list_tools'. No prior test pins the exact wire
        // shape or round-trip of this variant. The tool-list
        // probe is what the operator runs to confirm the MCP
        // transport surface is what they expect before calling a
        // tool; a slug regression silently strands the probe and
        // the operator's CLI prints an authentication-error
        // fallback where the real failure is a serde-shape
        // regression.
        let event = Request::ListTools;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::ListTools wire form must be exactly one \
             top-level key: 'kind'. A refactor that promoted the \
             variant from unit to struct or newtype would add a \
             second top-level key and every CLI tool-inventory \
             probe that sends {{\"kind\":\"list_tools\"}} alone \
             would fail to decode on the daemon side — the \
             operator's tool-discovery flow would silently start \
             returning Error responses instead of ToolList",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("list_tools")),
            "Request discriminator slug must be the durable \
             'list_tools'; a slug regression silently routes \
             incoming tool-inventory frames to the daemon's \
             catch-all error branch — every CLI tool-list probe \
             fails with a confusing fallback message instead of \
             the registered-tools inventory, masking transport \
             wiring gaps during incident triage",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::ListTools must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI tool-inventory consumer leans on",
        );
    }

    #[test]
    fn request_try_recv_a2a_task_serde_pins_unit_variant() {
        // Request::TryRecvA2ATask is the unit variant the CLI
        // sends as a non-blocking task-fetch poll — it pairs with
        // Response::A2ATaskOpt (already pinned) which carries
        // Some(task) on a hit and None on a miss, with the durable
        // null-on-wire contract for the Option. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire form is exactly one top-level
        // key: kind='try_recv_a2_a_task' — rename_all snake_case
        // splits A2A on digit/upper boundaries, matching the
        // durable a2_a slug rule for A2A* variants (project
        // memory: serde_snake_case_a2a_quirk). No prior test pins
        // the exact wire shape or round-trip of this variant. The
        // non-blocking poll is the load-bearing variant in the A2A
        // consumer loop — a slug regression silently strands every
        // CLI/agent that drains tasks via this Request, and stale
        // agents stop receiving work without an explicit failure.
        let event = Request::TryRecvA2ATask;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::TryRecvA2ATask wire form must be exactly one \
             top-level key: 'kind'. A refactor that promoted the \
             variant from unit to struct or newtype would add a \
             second top-level key and every A2A consumer that \
             sends {{\"kind\":\"try_recv_a2_a_task\"}} alone would \
             fail to decode on the daemon side — the operator's \
             agent would stop draining tasks silently, leaving \
             work queued indefinitely without an explicit failure",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("try_recv_a2_a_task")),
            "Request discriminator slug must be the durable \
             'try_recv_a2_a_task' (rename_all = snake_case splits \
             A2A on digit/upper boundaries); a slug regression \
             silently routes incoming poll frames to the daemon's \
             catch-all error branch — every A2A consumer fails \
             with a confusing fallback message instead of the \
             task-fetch outcome, silently breaking the consumer \
             loop while queued tasks pile up",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::TryRecvA2ATask must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every A2A task-consumer loop leans on",
        );
    }

    #[test]
    fn request_try_recv_a2a_result_serde_pins_unit_variant() {
        // Request::TryRecvA2AResult is the unit variant the CLI
        // sends as a non-blocking result-fetch poll — it pairs
        // with Response::A2AResultOpt (already pinned) which
        // carries Some(result) on a hit and None on a miss, with
        // the durable null-on-wire contract for the Option. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire form is exactly one top-level
        // key: kind='try_recv_a2_a_result' — rename_all snake_case
        // splits A2A on digit/upper boundaries, matching the
        // durable a2_a slug rule for A2A* variants (project
        // memory: serde_snake_case_a2a_quirk). No prior test pins
        // the exact wire shape or round-trip of this variant. The
        // non-blocking result poll is what the agent uses to drain
        // completed task outcomes — a slug regression silently
        // strands every result consumer, leaving completed work
        // undeliverable to the originator.
        let event = Request::TryRecvA2AResult;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::TryRecvA2AResult wire form must be exactly \
             one top-level key: 'kind'. A refactor that promoted \
             the variant from unit to struct or newtype would add \
             a second top-level key and every result consumer that \
             sends {{\"kind\":\"try_recv_a2_a_result\"}} alone \
             would fail to decode on the daemon side — the \
             originator would stop receiving completed task \
             outcomes silently, leaving the agent unable to \
             confirm dispatched work finished",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("try_recv_a2_a_result")),
            "Request discriminator slug must be the durable \
             'try_recv_a2_a_result' (rename_all = snake_case \
             splits A2A on digit/upper boundaries); a slug \
             regression silently routes incoming poll frames to \
             the daemon's catch-all error branch — every result \
             consumer fails with a confusing fallback message \
             instead of the result-fetch outcome",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::TryRecvA2AResult must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every A2A result-consumer loop leans on",
        );
    }

    #[test]
    fn request_verify_audit_integrity_serde_pins_unit_variant() {
        // Request::VerifyAuditIntegrity is the unit variant the
        // CLI, HTTP gateway, and daemon dispatch route to trigger a
        // full audit hash-chain integrity sweep — it pairs with
        // Response::AuditIntegrity (already pinned) which carries
        // the AuditIntegrityReport the operator inspects to confirm
        // the chain has not been tampered with. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire form is exactly one top-level key:
        // kind='verify_audit_integrity'. No prior test pins the
        // exact wire shape or round-trip of this variant. The
        // audit-integrity probe is the security boundary that
        // surfaces tampered or truncated audit chains; a slug
        // regression silently strands the probe and the operator
        // sees the daemon's catch-all Error response instead of the
        // integrity report, masking real tampering during incident
        // triage.
        let event = Request::VerifyAuditIntegrity;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::VerifyAuditIntegrity wire form must be \
             exactly one top-level key: 'kind'. A refactor that \
             promoted the variant from unit to struct or newtype \
             would add a second top-level key and every CLI/HTTP \
             audit-integrity probe that sends \
             {{\"kind\":\"verify_audit_integrity\"}} alone would \
             fail to decode on the daemon side — the operator \
             would stop receiving integrity evidence silently, \
             masking real audit-chain tampering during incident \
             triage",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("verify_audit_integrity")),
            "Request discriminator slug must be the durable \
             'verify_audit_integrity'; a slug regression silently \
             routes incoming integrity-probe frames to the \
             daemon's catch-all error branch — every CLI/HTTP \
             integrity probe fails with a confusing fallback \
             message instead of the AuditIntegrityReport, and \
             tampered audit chains go undetected until a human \
             notices the wrong response shape",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::VerifyAuditIntegrity must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every audit-integrity probe consumer leans on",
        );
    }

    #[test]
    fn request_compact_a2a_serde_pins_unit_variant() {
        // Request::CompactA2A is the unit variant the CLI and HTTP
        // gateway send to trigger operator-driven A2A queue
        // compaction — it pairs with Response::A2ACompacted
        // (already pinned) which carries the per-pass compaction
        // outcome the operator inspects to confirm rows were
        // dropped. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Request enum, the wire
        // form is exactly one top-level key:
        // kind='compact_a2_a' — rename_all snake_case splits A2A
        // on digit/upper boundaries, matching the durable a2_a
        // slug rule for A2A* variants (project memory:
        // serde_snake_case_a2a_quirk). No prior test pins the
        // exact wire shape or round-trip of this variant.
        // Compaction is the only operator-driven hand on the
        // on-disk A2A event log size; a slug regression silently
        // strands the request and the operator sees the daemon's
        // catch-all Error response while the log keeps growing.
        let event = Request::CompactA2A;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::CompactA2A wire form must be exactly one \
             top-level key: 'kind'. A refactor that promoted the \
             variant from unit to struct or newtype would add a \
             second top-level key and every CLI/HTTP compaction \
             trigger that sends {{\"kind\":\"compact_a2_a\"}} \
             alone would fail to decode on the daemon side — the \
             operator would stop compacting silently, the on-disk \
             A2A event log would keep growing, and rotation costs \
             would creep up without an explicit failure surface",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("compact_a2_a")),
            "Request discriminator slug must be the durable \
             'compact_a2_a' (rename_all = snake_case splits A2A on \
             digit/upper boundaries); a slug regression silently \
             routes incoming compaction frames to the daemon's \
             catch-all error branch — every CLI/HTTP compaction \
             probe fails with a confusing fallback message instead \
             of the A2ACompacted outcome, masking real compaction \
             wiring breakage during incident triage",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::CompactA2A must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every operator-driven A2A compaction \
             consumer leans on",
        );
    }

    #[test]
    fn request_rotate_operator_token_serde_pins_unit_variant() {
        // Request::RotateOperatorToken is the unit variant the CLI
        // and HTTP gateway send to mint a fresh bootstrap token,
        // register it in the peer registry, rewrite
        // $COVENANT_HOME/peers/operator.token (mode 0600), and
        // revoke the old token — gated to peer.pubkey ==
        // self.identity.pubkey so a guest peer cannot rotate the
        // operator's own token. It pairs with
        // Response::OperatorTokenRotated (already pinned) which
        // carries the new token to HTTP callers that cannot read
        // the on-disk file. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Request enum, the wire
        // form is exactly one top-level key:
        // kind='rotate_operator_token'. No prior test pins the
        // exact wire shape or round-trip of this variant. Token
        // rotation is the only operator-driven path to revoke a
        // leaked bootstrap token; a slug regression silently
        // strands the request and the operator sees the daemon's
        // catch-all Error response while the compromised token
        // stays live.
        let event = Request::RotateOperatorToken;

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["kind"],
            "Request::RotateOperatorToken wire form must be \
             exactly one top-level key: 'kind'. A refactor that \
             promoted the variant from unit to struct or newtype \
             would add a second top-level key and every CLI/HTTP \
             rotation trigger that sends \
             {{\"kind\":\"rotate_operator_token\"}} alone would \
             fail to decode on the daemon side — the operator \
             could not revoke a leaked bootstrap token through \
             the supported path, and the compromised token would \
             stay live until manual filesystem intervention",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("rotate_operator_token")),
            "Request discriminator slug must be the durable \
             'rotate_operator_token'; a slug regression silently \
             routes incoming rotation frames to the daemon's \
             catch-all error branch — every CLI/HTTP rotation \
             probe fails with a confusing fallback message instead \
             of OperatorTokenRotated, and the operator cannot \
             complete bootstrap-token rotation during a \
             credential-leak response",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RotateOperatorToken must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every operator-token-rotation consumer leans on",
        );
    }

    #[test]
    fn request_submit_intent_serde_pins_single_field_variant() {
        // Request::SubmitIntent is the single-field variant the
        // CLI sends to dispatch an operator-typed intent — it
        // carries text: String, the raw intent the daemon echoes
        // into the audit log and routes through the runtime
        // pipeline. It pairs with Response::IntentResult (already
        // pinned). With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Request enum, the
        // wire object is exactly two top-level keys:
        // kind='submit_intent' plus text. Only an async
        // round-trip via duplex pipe (request_roundtrip_via_pipe)
        // exercises this variant, and that test does not assert
        // any wire-shape invariant. A serde-shape regression on
        // Request::SubmitIntent silently breaks the operator's
        // primary CLI entrypoint — every `covenant intents submit`
        // call ends in an authentication-error fallback or a
        // missing-field decode error on the daemon side.
        let event = Request::SubmitIntent {
            text: "summarise the audit log".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "text"],
            "Request::SubmitIntent wire form must be exactly two \
             top-level keys: 'kind' plus the single 'text' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'text' \
             one level deeper and every operator's `covenant \
             intents submit` call would fail to decode on the \
             daemon side — the operator's primary intent-submission \
             flow would silently start returning Error responses \
             instead of IntentResult",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("submit_intent")),
            "Request discriminator slug must be the durable \
             'submit_intent'; a slug regression silently routes \
             incoming intent frames to the daemon's catch-all \
             error branch — every CLI intent submission fails \
             with a confusing fallback message instead of \
             IntentResult, leaving the operator's typed prompt \
             unprocessed",
        );
        assert_eq!(
            obj.get("text").and_then(serde_json::Value::as_str),
            Some("summarise the audit log"),
            "Request::SubmitIntent::text must surface as the \
             literal operator-typed intent string — the daemon's \
             audit log row and runtime pipeline both bind on this \
             exact field; a rename or retype would silently \
             re-route operator prompts to a different verb",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::SubmitIntent must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI intent-submission consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("text");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::SubmitIntent wire form must reject a payload \
             missing 'text'; a stray #[serde(default)] would let \
             a malformed frame decode with text=String::new() and \
             the daemon would dispatch an empty-prompt intent \
             through the runtime pipeline — every empty-prompt \
             audit row would falsely attribute to the operator \
             without the operator having submitted anything",
        );
    }

    #[test]
    fn request_authenticate_serde_pins_single_field_variant() {
        // Request::Authenticate is the mandatory first privileged
        // frame on every IPC connection — it carries token_b58:
        // String, the base58-encoded peer token the daemon
        // resolves through covenant_peer_auth::PeerRegistry to
        // bind an AgentId to the connection for the lifetime of
        // the socket. It pairs with Response::Authenticated
        // (already pinned) on success and
        // Response::AuthenticationFailed on failure. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='authenticate' plus token_b58. No prior test
        // pins the exact wire shape or missing-field rejection for
        // this variant. A serde-shape regression on the handshake
        // silently breaks every privileged CLI/HTTP entrypoint —
        // the operator's entire session model collapses.
        let event = Request::Authenticate {
            token_b58: "11111111111111111111111111111111".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "token_b58"],
            "Request::Authenticate wire form must be exactly two \
             top-level keys: 'kind' plus the single 'token_b58' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would \
             nest 'token_b58' one level deeper and every \
             privileged CLI/HTTP client that sends \
             {{\"kind\":\"authenticate\",\"token_b58\":\"...\"}} \
             would fail to decode on the daemon side — the \
             operator's entire session model collapses because \
             the mandatory first frame is rejected before any \
             verb runs",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("authenticate")),
            "Request discriminator slug must be the durable \
             'authenticate'; a slug regression silently routes \
             incoming handshake frames to the daemon's catch-all \
             error branch — every CLI/HTTP handshake fails with \
             a confusing fallback message instead of \
             Authenticated/AuthenticationFailed, masking the \
             real cause of the session-start regression",
        );
        assert_eq!(
            obj.get("token_b58").and_then(serde_json::Value::as_str),
            Some("11111111111111111111111111111111"),
            "Request::Authenticate::token_b58 must surface as the \
             literal base58 peer-token string — the daemon's \
             peer-registry resolver binds on this exact field; a \
             rename or retype would silently re-route every \
             handshake to an unknown-token reject branch",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::Authenticate must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP handshake consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("token_b58");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::Authenticate wire form must reject a payload \
             missing 'token_b58'; a stray #[serde(default)] would \
             let a malformed frame decode with \
             token_b58=String::new() and the daemon would attempt \
             to resolve the empty string against the peer \
             registry — operator triage could not distinguish a \
             real bad-token attempt from a serde-shape regression \
             in the client, and the AuthenticationFailed audit \
             row would falsely attribute the failure to the \
             operator's token rather than the wire-shape break",
        );
    }

    #[test]
    fn request_purge_audit_serde_pins_single_field_variant() {
        // Request::PurgeAudit is the operator-driven retention
        // variant the CLI and HTTP gateway send to drop audit
        // events strictly older than before_ms (no scheduled
        // compaction in v0). It pairs with Response::AuditPurged
        // (already pinned) which carries the dropped row count.
        // With #[serde(tag = "kind", rename_all = "snake_case")]
        // on the Request enum, the wire object is exactly two
        // top-level keys: kind='purge_audit' plus before_ms.
        // No prior test pins the exact wire shape, numeric u64
        // serialization, or missing-field rejection for this
        // variant. The audit-purge probe is the only
        // operator-driven hand on audit-log size and the row that
        // surfaces destructive retention; a serde-shape regression
        // silently strands the request and the operator either
        // sees the daemon's catch-all Error response while the log
        // keeps growing, or — worse — a malformed frame decodes as
        // a default-zero cutoff and the retention path runs
        // against the wrong window.
        let event = Request::PurgeAudit {
            before_ms: 1_700_000_000_000,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["before_ms", "kind"],
            "Request::PurgeAudit wire form must be exactly two \
             top-level keys: 'kind' plus the single 'before_ms' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed RetentionWindow \
             would nest 'before_ms' one level deeper and every \
             CLI/HTTP retention call that sends \
             {{\"kind\":\"purge_audit\",\"before_ms\":<n>}} \
             would fail to decode on the daemon side — the \
             operator could not prune the audit log through the \
             supported path and storage would grow without bound",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("purge_audit")),
            "Request discriminator slug must be the durable \
             'purge_audit'; a slug regression silently routes \
             incoming retention frames to the daemon's catch-all \
             error branch — every CLI/HTTP retention probe fails \
             with a confusing fallback message instead of \
             AuditPurged, and the audit log keeps growing without \
             the operator noticing",
        );
        assert_eq!(
            obj.get("before_ms").and_then(serde_json::Value::as_u64),
            Some(1_700_000_000_000),
            "Request::PurgeAudit::before_ms must surface as the \
             literal u64 cutoff in milliseconds — the daemon's \
             retention path binds on this exact field with strict \
             greater-than-or-equal semantics; a rename, retype, \
             or accidental signed coercion would silently shift \
             the window and either skip rows the operator meant \
             to drop or drop rows the operator meant to keep",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::PurgeAudit must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP audit-retention consumer \
             leans on",
        );

        let mut missing = obj.clone();
        missing.remove("before_ms");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::PurgeAudit wire form must reject a payload \
             missing 'before_ms'; a stray #[serde(default)] would \
             let a malformed frame decode with before_ms=0 and \
             the daemon would run retention against an unintended \
             cutoff — with a flipped comparison or off-by-one in \
             the retention path this becomes a fleet-wide audit \
             wipe in response to a request the operator did not \
             send, destroying the very evidence operator triage \
             needs to detect the incident",
        );
    }

    #[test]
    fn request_purge_capabilities_serde_pins_single_field_variant() {
        // Request::PurgeCapabilities is the operator-driven
        // retention variant the CLI and HTTP gateway send to drop
        // revocation tombstones (and their matching grant rows)
        // whose revoked_at is strictly older than before_ms —
        // live grants are untouched. It pairs with
        // Response::CapabilitiesPurged (already pinned) which
        // carries the dropped row count. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='purge_capabilities' plus before_ms. No prior
        // test pins the exact wire shape, numeric u64
        // serialization, or missing-field rejection for this
        // variant. A serde-shape regression on this retention path
        // either silently strands the operator's tombstone pruning
        // or — worse — decodes a malformed frame as a default-zero
        // cutoff and runs retention against an unintended window.
        let event = Request::PurgeCapabilities {
            before_ms: 1_700_000_000_000,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["before_ms", "kind"],
            "Request::PurgeCapabilities wire form must be exactly \
             two top-level keys: 'kind' plus the single \
             'before_ms' field. A refactor that promoted the \
             variant from struct to newtype wrapping a typed \
             RetentionWindow would nest 'before_ms' one level \
             deeper and every CLI/HTTP retention call that sends \
             {{\"kind\":\"purge_capabilities\",\"before_ms\":<n>}} \
             would fail to decode on the daemon side — \
             revoked-capability tombstones would accumulate \
             without bound and operator triage could not tell the \
             difference between an active grant and a \
             pre-revocation grant in the registry",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("purge_capabilities")),
            "Request discriminator slug must be the durable \
             'purge_capabilities'; a slug regression silently \
             routes incoming retention frames to the daemon's \
             catch-all error branch — every CLI/HTTP retention \
             probe fails with a confusing fallback message \
             instead of CapabilitiesPurged, and the capabilities \
             registry keeps growing without the operator noticing",
        );
        assert_eq!(
            obj.get("before_ms").and_then(serde_json::Value::as_u64),
            Some(1_700_000_000_000),
            "Request::PurgeCapabilities::before_ms must surface as \
             the literal u64 cutoff in milliseconds — the daemon's \
             retention path binds on this exact field with strict \
             greater-than-or-equal semantics; a rename, retype, or \
             accidental signed coercion would silently shift the \
             window and either skip tombstones the operator meant \
             to drop or drop tombstones the operator meant to keep",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::PurgeCapabilities must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP capabilities-retention \
             consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("before_ms");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::PurgeCapabilities wire form must reject a \
             payload missing 'before_ms'; a stray #[serde(default)] \
             would let a malformed frame decode with before_ms=0 \
             and the daemon would run retention against an \
             unintended cutoff — with a flipped comparison or \
             off-by-one in the retention path this becomes a \
             fleet-wide capability-history wipe in response to a \
             request the operator did not send, destroying the \
             revocation evidence operator triage needs to detect \
             tampering",
        );
    }

    #[test]
    fn request_purge_peers_serde_pins_single_field_variant() {
        // Request::PurgePeers is the operator-driven retention
        // variant the CLI and HTTP gateway send to drop revocation
        // tombstones and their matching Registered rows from
        // peers/registry.jsonl whose revoked_at is strictly older
        // than before_ms — live registrations are untouched. It
        // pairs with Response::PeersPurged (already pinned) which
        // carries the dropped row count. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='purge_peers' plus before_ms. No prior test
        // pins the exact wire shape, numeric u64 serialization,
        // or missing-field rejection for this variant. A
        // serde-shape regression on this retention path either
        // silently strands the operator's tombstone pruning or —
        // worse — decodes a malformed frame as a default-zero
        // cutoff and runs retention against an unintended window.
        let event = Request::PurgePeers {
            before_ms: 1_700_000_000_000,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["before_ms", "kind"],
            "Request::PurgePeers wire form must be exactly two \
             top-level keys: 'kind' plus the single 'before_ms' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed RetentionWindow \
             would nest 'before_ms' one level deeper and every \
             CLI/HTTP retention call that sends \
             {{\"kind\":\"purge_peers\",\"before_ms\":<n>}} \
             would fail to decode on the daemon side — \
             revoked-peer tombstones would accumulate without \
             bound in peers/registry.jsonl and operator triage \
             could not distinguish a live peer from a revoked one \
             cleanly",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("purge_peers")),
            "Request discriminator slug must be the durable \
             'purge_peers'; a slug regression silently routes \
             incoming retention frames to the daemon's catch-all \
             error branch — every CLI/HTTP retention probe fails \
             with a confusing fallback message instead of \
             PeersPurged, and the peer registry keeps growing \
             without the operator noticing",
        );
        assert_eq!(
            obj.get("before_ms").and_then(serde_json::Value::as_u64),
            Some(1_700_000_000_000),
            "Request::PurgePeers::before_ms must surface as the \
             literal u64 cutoff in milliseconds — the daemon's \
             retention path binds on this exact field with strict \
             greater-than-or-equal semantics; a rename, retype, \
             or accidental signed coercion would silently shift \
             the window and either skip tombstones the operator \
             meant to drop or drop tombstones the operator meant \
             to keep",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::PurgePeers must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP peers-retention consumer \
             leans on",
        );

        let mut missing = obj.clone();
        missing.remove("before_ms");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::PurgePeers wire form must reject a payload \
             missing 'before_ms'; a stray #[serde(default)] would \
             let a malformed frame decode with before_ms=0 and \
             the daemon would run retention against an unintended \
             cutoff — with a flipped comparison or off-by-one in \
             the retention path this becomes a fleet-wide \
             peer-history wipe in response to a request the \
             operator did not send, destroying the registry \
             evidence operator triage needs to attribute past \
             peer activity",
        );
    }

    #[test]
    fn request_flush_receipts_serde_pins_default_bearing_single_field_variant() {
        // Request::FlushReceipts is the operator-driven
        // settlement-receipt flush variant the CLI and HTTP gateway
        // send to batch-emit the most recent N locally-stored
        // receipts onto the persistent chain receipt sidecar. limit
        // defaults to default_recent_limit() (10) via
        // #[serde(default = "default_recent_limit")] so a stale CLI
        // can omit the field. It pairs with
        // Response::ReceiptBatchFlushed (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='flush_receipts' plus limit. No prior test
        // pins the exact wire shape, limit numeric serialization,
        // round-trip, or the default-on-missing decode contract for
        // this variant. The default-on-missing decode is a
        // load-bearing compatibility hinge — a stale CLI that does
        // not yet know to send limit must continue working.
        let event = Request::FlushReceipts { limit: 25 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit"],
            "Request::FlushReceipts wire form must be exactly two \
             top-level keys: 'kind' plus the single 'limit' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a typed FlushOptions would nest \
             'limit' one level deeper and every CLI/HTTP flush \
             trigger that sends \
             {{\"kind\":\"flush_receipts\",\"limit\":<n>}} would \
             fail to decode on the daemon side — the operator \
             could not flush local receipts onto the chain \
             sidecar through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("flush_receipts")),
            "Request discriminator slug must be the durable \
             'flush_receipts'; a slug regression silently routes \
             incoming flush frames to the daemon's catch-all error \
             branch — every CLI/HTTP flush probe fails with a \
             confusing fallback message instead of \
             ReceiptBatchFlushed, and the receipt sidecar stops \
             receiving batched writes",
        );
        assert_eq!(
            obj.get("limit").and_then(serde_json::Value::as_u64),
            Some(25),
            "Request::FlushReceipts::limit must surface as the \
             literal numeric batch cap — the daemon's flush path \
             binds on this exact field; a rename or retype would \
             silently emit a different batch size than the \
             operator asked for, distorting receipt accounting",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::FlushReceipts must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP receipt-flush consumer leans \
             on",
        );

        let stale = serde_json::json!({"kind": "flush_receipts"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::FlushReceipts must decode from a payload \
             missing 'limit' — the #[serde(default)] attribute is \
             the durable compatibility hinge that lets stale CLIs \
             continue flushing without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::FlushReceipts { limit: 10 },
            "Request::FlushReceipts with missing 'limit' must \
             default to default_recent_limit() = 10 — a refactor \
             that changes the default function return value or \
             drops the #[serde(default)] attribute silently \
             changes the batch size for every stale CLI in the \
             field; the operator's receipt-flush behaviour \
             diverges from the documented contract without a \
             single error surface",
        );
    }

    #[test]
    fn request_receipt_batches_serde_pins_default_bearing_single_field_variant() {
        // Request::ReceiptBatches is the read-side default-bearing
        // variant the CLI and HTTP gateway send to enumerate the
        // most recent N batched-receipt envelopes from the
        // persistent chain receipt sidecar. limit defaults to
        // default_recent_limit() (10) via
        // #[serde(default = "default_recent_limit")] so a stale CLI
        // can omit the field. It pairs with
        // Response::ReceiptBatches (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='receipt_batches' plus limit. No prior test
        // pins the exact wire shape, limit numeric serialization,
        // round-trip, or the default-on-missing decode contract
        // for this variant. The default-on-missing decode is the
        // compatibility hinge — a stale CLI that does not yet
        // know to send limit must continue listing without a
        // rebuild.
        let event = Request::ReceiptBatches { limit: 25 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit"],
            "Request::ReceiptBatches wire form must be exactly two \
             top-level keys: 'kind' plus the single 'limit' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a typed PageOptions would nest \
             'limit' one level deeper and every CLI/HTTP batches \
             probe that sends \
             {{\"kind\":\"receipt_batches\",\"limit\":<n>}} would \
             fail to decode on the daemon side — the operator \
             could not enumerate batched receipts through the \
             supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("receipt_batches")),
            "Request discriminator slug must be the durable \
             'receipt_batches'; a slug regression silently routes \
             incoming batches frames to the daemon's catch-all \
             error branch — every CLI/HTTP batches probe fails \
             with a confusing fallback message instead of \
             Response::ReceiptBatches",
        );
        assert_eq!(
            obj.get("limit").and_then(serde_json::Value::as_u64),
            Some(25),
            "Request::ReceiptBatches::limit must surface as the \
             literal numeric page size — the daemon's listing path \
             binds on this exact field; a rename or retype would \
             silently return a different row count than the \
             operator asked for, distorting CLI output and HTTP \
             response payload sizes",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::ReceiptBatches must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP batches-listing consumer \
             leans on",
        );

        let stale = serde_json::json!({"kind": "receipt_batches"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::ReceiptBatches must decode from a payload \
             missing 'limit' — the #[serde(default)] attribute is \
             the durable compatibility hinge that lets stale CLIs \
             continue listing without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::ReceiptBatches { limit: 10 },
            "Request::ReceiptBatches with missing 'limit' must \
             default to default_recent_limit() = 10 — a refactor \
             that changes the default function return value or \
             drops the #[serde(default)] attribute silently \
             changes the page size for every stale CLI in the \
             field; the operator's batches-listing behaviour \
             diverges from the documented contract without a \
             single error surface",
        );
    }

    #[test]
    fn request_revoke_capability_serde_pins_single_field_variant() {
        // Request::RevokeCapability is the operator-driven
        // capability-revocation variant the CLI and HTTP gateway
        // send to revoke a previously-granted SignedCapability by
        // its base58 ed25519 signature. It pairs with
        // Response::CapabilityRevoked (already pinned) which
        // carries the signature back plus a removed boolean. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='revoke_capability' plus signature_b58. No
        // prior test pins the exact wire shape, string
        // serialization, round-trip, or missing-field rejection
        // for this variant. A serde-shape regression on this
        // revocation path either silently strands the operator's
        // revocation order (the daemon's capability ledger keeps
        // honoring a compromised grant) or — worse — decodes a
        // malformed frame with a stray serde(default) and revokes
        // against an empty signature, collapsing every audit-row
        // attribution for the revocation.
        let event = Request::RevokeCapability {
            signature_b58: "3xS9Yk1f8wL2bN7pQz4mRtUvJh6cKaDe5gXyWnVoBqAr".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "signature_b58"],
            "Request::RevokeCapability wire form must be exactly \
             two top-level keys: 'kind' plus the single \
             'signature_b58' field. A refactor that promoted the \
             variant from struct to newtype wrapping a typed \
             RevocationTarget would nest 'signature_b58' one \
             level deeper and every CLI/HTTP revocation call that \
             sends \
             {{\"kind\":\"revoke_capability\",\"signature_b58\":\"<b58>\"}} \
             would fail to decode on the daemon side — the \
             operator's revocation request would fail silently or \
             with a confusing fallback message and the \
             compromised capability would remain live in the \
             daemon's ledger",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("revoke_capability")),
            "Request discriminator slug must be the durable \
             'revoke_capability'; a slug regression silently \
             routes incoming revocation frames to the daemon's \
             catch-all error branch — every CLI/HTTP revocation \
             probe fails with a confusing fallback message \
             instead of CapabilityRevoked, and the operator \
             cannot revoke a compromised signed capability \
             through the supported path",
        );
        assert_eq!(
            obj.get("signature_b58").and_then(serde_json::Value::as_str),
            Some("3xS9Yk1f8wL2bN7pQz4mRtUvJh6cKaDe5gXyWnVoBqAr"),
            "Request::RevokeCapability::signature_b58 must surface \
             as the literal base58 string — the daemon's \
             revocation path binds on this exact field to locate \
             the matching SignedCapability row; a rename, retype, \
             or accidental byte-array coercion would silently \
             miss every revocation target the operator submitted, \
             and the compromised capability would remain live",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RevokeCapability must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP capability-revocation \
             consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("signature_b58");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::RevokeCapability wire form must reject a \
             payload missing 'signature_b58'; a stray \
             #[serde(default)] would let a malformed frame decode \
             with signature_b58=\"\" and the daemon would attempt \
             revocation against an empty signature — the lookup \
             either no-ops silently (the operator believes they \
             revoked a capability they did not) or, with a \
             flipped equality in the revocation path, removes an \
             unintended row, and every audit-row attribution for \
             the revocation collapses to a meaningless \
             empty-string subject",
        );
    }

    #[test]
    fn request_ignore_check_serde_pins_single_field_variant() {
        // Request::IgnoreCheck is the operator-driven ignore-rule
        // probe the CLI and HTTP gateway send to test whether a
        // candidate intent string matches the local IgnoreList
        // policy. It pairs with Response::IgnoreReport (already
        // pinned) which carries ignored, matched_pattern, and
        // rules_loaded back. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='ignore_check' plus text. No prior test pins
        // the exact wire shape, string serialization, round-trip,
        // or missing-field rejection for this variant. A
        // serde-shape regression on this probe path either
        // silently breaks the operator's policy preview (every
        // probe returns an error fallback instead of a real
        // ignore decision) or — worse — decodes a malformed frame
        // with a stray serde(default) on text and runs the
        // matcher against an empty string, returning a meaningless
        // false-negative answer to a question the operator is
        // using to gate production routing.
        let event = Request::IgnoreCheck {
            text: "deploy production rollout".into(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "text"],
            "Request::IgnoreCheck wire form must be exactly two \
             top-level keys: 'kind' plus the single 'text' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a typed IgnoreCandidate would nest \
             'text' one level deeper and every CLI/HTTP ignore \
             probe that sends \
             {{\"kind\":\"ignore_check\",\"text\":\"<s>\"}} would \
             fail to decode on the daemon side — the operator's \
             policy preview surface goes dark and ignore-rule \
             rollout cannot be tested through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("ignore_check")),
            "Request discriminator slug must be the durable \
             'ignore_check'; a slug regression silently routes \
             incoming probe frames to the daemon's catch-all \
             error branch — every CLI/HTTP probe fails with a \
             confusing fallback message instead of IgnoreReport, \
             and the operator cannot validate a candidate intent \
             against the loaded IgnoreList through the supported \
             path",
        );
        assert_eq!(
            obj.get("text").and_then(serde_json::Value::as_str),
            Some("deploy production rollout"),
            "Request::IgnoreCheck::text must surface as the \
             literal JSON string — the daemon's matcher binds on \
             this exact field; a rename, retype, or accidental \
             byte-array coercion would silently miss every \
             candidate the operator submitted, leaving the \
             ignore-rule preview surface stuck on a no-match \
             default",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::IgnoreCheck must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP ignore-probe consumer leans \
             on",
        );

        let mut missing = obj.clone();
        missing.remove("text");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::IgnoreCheck wire form must reject a payload \
             missing 'text'; a stray #[serde(default)] would let \
             a malformed frame decode with text=\"\" and the \
             daemon would run the IgnoreList matcher against an \
             empty string — which matches nothing in any normal \
             policy and returns ignored:false with a meaningless \
             0-rule attribution, silently lying to the operator \
             about whether a real candidate string would have \
             been ignored, and that false-negative can leak a \
             production intent through a gate the operator \
             believed was active",
        );
    }

    #[test]
    fn request_verify_serde_pins_default_bearing_single_field_variant() {
        // Request::Verify is the operator-driven
        // local-audit-chain verifier the CLI and HTTP gateway
        // send to recompute hash-chain integrity over the most
        // recent N audit rows. window defaults to
        // default_verify_window() (100) via
        // #[serde(default = "default_verify_window")] so a stale
        // CLI can omit the field. It pairs with
        // Response::VerifyReport (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='verify' plus window. No prior test pins the
        // exact wire shape, window numeric serialization,
        // round-trip, or the default-on-missing decode contract
        // for this variant. The defaulting helper is
        // default_verify_window (returns 100), distinct from
        // default_recent_limit (returns 10) used by
        // FlushReceipts/ReceiptBatches — a refactor that
        // consolidated default helpers could silently shrink the
        // verifier window by a 10x factor without a single error
        // surface.
        let event = Request::Verify { window: 250 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "window"],
            "Request::Verify wire form must be exactly two \
             top-level keys: 'kind' plus the single 'window' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed VerifyOptions \
             would nest 'window' one level deeper and every \
             CLI/HTTP verifier probe that sends \
             {{\"kind\":\"verify\",\"window\":<n>}} would fail to \
             decode on the daemon side — the audit-chain \
             integrity verifier surface goes dark and the \
             operator cannot inspect drift through the supported \
             path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("verify")),
            "Request discriminator slug must be the durable \
             'verify'; a slug regression silently routes \
             incoming verifier frames to the daemon's catch-all \
             error branch — every CLI/HTTP verifier probe fails \
             with a confusing fallback message instead of \
             VerifyReport, and the operator cannot recompute the \
             audit hash chain through the supported path",
        );
        assert_eq!(
            obj.get("window").and_then(serde_json::Value::as_u64),
            Some(250),
            "Request::Verify::window must surface as the literal \
             numeric window — the daemon's verifier path binds on \
             this exact field to cap the recomputed hash-chain \
             slice; a rename or retype would silently emit a \
             different verification depth than the operator \
             requested, missing drift evidence at the tail of the \
             chain",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::Verify must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI/HTTP audit-verifier consumer leans on",
        );

        let stale = serde_json::json!({"kind": "verify"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::Verify must decode from a payload missing \
             'window' — the #[serde(default = \"default_verify_window\")] \
             attribute is the durable compatibility hinge that \
             lets stale CLIs continue verifying without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::Verify { window: 100 },
            "Request::Verify with missing 'window' must default to \
             default_verify_window() = 100 — distinct from \
             default_recent_limit() = 10 used by FlushReceipts \
             and ReceiptBatches. A refactor that consolidated \
             #[serde(default = \"default_verify_window\")] to \
             #[serde(default)] (zero) or to \
             #[serde(default = \"default_recent_limit\")] (10) \
             would silently shrink the verifier window by a 10x \
             or larger factor relative to the operator's \
             expectation, miss drift evidence at the tail of the \
             chain, and break stale-CLI compatibility without a \
             single error surface",
        );
    }

    #[test]
    fn request_recent_capabilities_serde_pins_default_bearing_single_field_variant() {
        // Request::RecentCapabilities is the read-side
        // default-bearing variant the CLI and HTTP gateway send to
        // enumerate the most recent N SignedCapability ledger rows
        // for operator triage. limit defaults to
        // default_recent_limit() (10) via
        // #[serde(default = "default_recent_limit")] so a stale
        // CLI can omit the field. It pairs with
        // Response::Capabilities (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='recent_capabilities' plus limit. No prior
        // test pins the exact wire shape, limit numeric
        // serialization, round-trip, or the default-on-missing
        // decode contract for this variant. The default-on-missing
        // decode is the compatibility hinge — a stale CLI that
        // does not yet know to send limit must continue listing
        // without a rebuild.
        let event = Request::RecentCapabilities { limit: 25 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit"],
            "Request::RecentCapabilities wire form must be exactly \
             two top-level keys: 'kind' plus the single 'limit' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed PageOptions would \
             nest 'limit' one level deeper and every CLI/HTTP \
             capabilities-listing probe that sends \
             {{\"kind\":\"recent_capabilities\",\"limit\":<n>}} \
             would fail to decode on the daemon side — the \
             operator's capability-ledger triage surface goes \
             dark and grant/revoke evidence cannot be enumerated \
             through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("recent_capabilities")),
            "Request discriminator slug must be the durable \
             'recent_capabilities'; a slug regression silently \
             routes incoming listing frames to the daemon's \
             catch-all error branch — every CLI/HTTP \
             capabilities-listing probe fails with a confusing \
             fallback message instead of Response::Capabilities, \
             and the operator cannot enumerate the \
             SignedCapability ledger through the supported path",
        );
        assert_eq!(
            obj.get("limit").and_then(serde_json::Value::as_u64),
            Some(25),
            "Request::RecentCapabilities::limit must surface as \
             the literal numeric page size — the daemon's listing \
             path binds on this exact field; a rename or retype \
             would silently return a different row count than the \
             operator asked for, distorting CLI output and HTTP \
             response payload sizes",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RecentCapabilities must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP capabilities-listing consumer \
             leans on",
        );

        let stale = serde_json::json!({"kind": "recent_capabilities"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::RecentCapabilities must decode from a \
             payload missing 'limit' — the #[serde(default)] \
             attribute is the durable compatibility hinge that \
             lets stale CLIs continue listing without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::RecentCapabilities { limit: 10 },
            "Request::RecentCapabilities with missing 'limit' \
             must default to default_recent_limit() = 10 — a \
             refactor that drops the #[serde(default)] attribute \
             or repoints it at a helper returning a different \
             constant silently changes the page size for every \
             stale CLI in the field; the operator's \
             capabilities-listing behaviour diverges from the \
             documented contract without a single error surface",
        );
    }

    #[test]
    fn request_resume_intent_serde_pins_single_field_variant() {
        // Request::ResumeIntent is the operator-driven
        // intent-resume verb the CLI and HTTP gateway send to
        // re-dispatch the intent the audit log's most recent
        // BudgetExhausted row records under intent_id. It pairs
        // with Response::IntentResult (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='resume_intent' plus intent_id. The intent_id
        // field is typed Uuid (not String) so a retyping refactor
        // would silently break wire compatibility — the typed
        // binding is the load-bearing surface. No prior test pins
        // the exact wire shape, Uuid hyphenated-string
        // serialization, round-trip, or missing-field rejection
        // for this variant. A serde-shape regression on this
        // resume path either silently strands the operator's
        // resume order (the budget-exhausted intent never
        // re-dispatches) or — with a stray serde(default) —
        // decodes against Uuid::nil() and the daemon scans the
        // audit chain for the all-zeros intent, finding nothing
        // and returning a confusing not-found instead of running
        // the actual resume.
        let intent_id = Uuid::from_u128(0xC0FF_EEDE_ADBE_EFCA_FEBA_BE0B_ADF0_0DDC);
        let event = Request::ResumeIntent { intent_id };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["intent_id", "kind"],
            "Request::ResumeIntent wire form must be exactly two \
             top-level keys: 'kind' plus the single 'intent_id' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed ResumeTarget \
             would nest 'intent_id' one level deeper and every \
             CLI/HTTP resume call that sends \
             {{\"kind\":\"resume_intent\",\"intent_id\":\"<uuid>\"}} \
             would fail to decode on the daemon side — the \
             budget-exhausted intent would never re-dispatch \
             through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("resume_intent")),
            "Request discriminator slug must be the durable \
             'resume_intent'; a slug regression silently routes \
             incoming resume frames to the daemon's catch-all \
             error branch — every CLI/HTTP resume probe fails \
             with a confusing fallback message instead of \
             IntentResult, and the operator cannot resume a \
             budget-exhausted intent through the supported path",
        );
        assert_eq!(
            obj.get("intent_id").and_then(serde_json::Value::as_str),
            Some(intent_id.to_string().as_str()),
            "Request::ResumeIntent::intent_id must surface as the \
             Uuid's hyphenated string form (8-4-4-4-12 hex). A \
             retype from Uuid to String would lose the typed \
             binding and let a malformed hex / missing-hyphen \
             payload decode successfully against the wrong audit \
             row; a refactor that swapped Uuid's serde repr to \
             the simple no-hyphen form or a byte array would \
             silently break every operator's resume scripts \
             against the prior wire contract",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::ResumeIntent must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP resume consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("intent_id");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::ResumeIntent wire form must reject a \
             payload missing 'intent_id'; a stray \
             #[serde(default)] would let a malformed frame decode \
             with intent_id=Uuid::nil() and the daemon would scan \
             the audit chain for the all-zeros intent — finding \
             nothing and returning a confusing not-found instead \
             of running the actual resume, with the operator's \
             budget bucket never re-spending against the real \
             dispatch and audit attribution collapsing to a \
             phantom subject",
        );
    }

    #[test]
    fn request_recent_a2a_tasks_serde_pins_default_bearing_single_field_variant() {
        // Request::RecentA2ATasks is the read-side default-bearing
        // variant the CLI and HTTP gateway send to enumerate the
        // most recent N A2A tasks the daemon has observed (queued
        // or in-flight) for operator triage. limit defaults to
        // default_recent_limit() (10) via
        // #[serde(default = "default_recent_limit")] so a stale
        // CLI can omit the field. It pairs with Response::A2ATasks
        // (already pinned). With #[serde(tag = "kind", rename_all
        // = "snake_case")] on the Request enum, the wire object is
        // exactly two top-level keys: kind='recent_a2_a_tasks'
        // plus limit. The snake_case slug splits A2A on the
        // digit/uppercase boundary into 'a2_a' — this is the
        // documented durable shape, not a typo, and a refactor
        // that 'fixes' it to 'a2a' would silently break every
        // CLI/HTTP listing client. No prior test pins the exact
        // wire shape, the a2_a slug split, limit numeric
        // serialization, round-trip, or the default-on-missing
        // decode contract for this variant.
        let event = Request::RecentA2ATasks { limit: 25 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit"],
            "Request::RecentA2ATasks wire form must be exactly two \
             top-level keys: 'kind' plus the single 'limit' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a typed PageOptions would nest \
             'limit' one level deeper and every CLI/HTTP \
             A2A-tasks listing probe that sends \
             {{\"kind\":\"recent_a2_a_tasks\",\"limit\":<n>}} \
             would fail to decode on the daemon side — operator \
             triage of queued/in-flight A2A tasks goes dark \
             through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("recent_a2_a_tasks")),
            "Request discriminator slug must be the durable \
             'recent_a2_a_tasks' (rename_all = snake_case splits \
             the A2A boundary digit/uppercase into 'a2_a' — this \
             is the documented form, not a typo). A refactor that \
             'fixes' the slug to 'recent_a2a_tasks' would silently \
             route incoming listing frames to the daemon's \
             catch-all error branch — every CLI/HTTP listing \
             probe fails and the operator cannot enumerate \
             queued/in-flight A2A tasks through the supported \
             path",
        );
        assert_eq!(
            obj.get("limit").and_then(serde_json::Value::as_u64),
            Some(25),
            "Request::RecentA2ATasks::limit must surface as the \
             literal numeric page size — the daemon's listing \
             path binds on this exact field; a rename or retype \
             would silently return a different row count than the \
             operator asked for, distorting CLI output and HTTP \
             response payload sizes",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RecentA2ATasks must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP A2A-tasks listing consumer \
             leans on",
        );

        let stale = serde_json::json!({"kind": "recent_a2_a_tasks"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::RecentA2ATasks must decode from a payload \
             missing 'limit' — the #[serde(default)] attribute is \
             the durable compatibility hinge that lets stale CLIs \
             continue listing without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::RecentA2ATasks { limit: 10 },
            "Request::RecentA2ATasks with missing 'limit' must \
             default to default_recent_limit() = 10 — a refactor \
             that drops the #[serde(default)] attribute or \
             repoints it at a helper returning a different \
             constant silently changes the page size for every \
             stale CLI in the field; the operator's A2A-tasks \
             listing behaviour diverges from the documented \
             contract without a single error surface",
        );
    }

    #[test]
    fn request_recent_a2a_results_serde_pins_default_bearing_single_field_variant() {
        // Request::RecentA2AResults is the read-side
        // default-bearing variant the CLI and HTTP gateway send to
        // enumerate the most recent N A2ATaskResult rows the
        // daemon has observed for operator triage. limit defaults
        // to default_recent_limit() (10) via
        // #[serde(default = "default_recent_limit")] so a stale
        // CLI can omit the field. It pairs with
        // Response::A2AResults (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='recent_a2_a_results' plus limit. The
        // snake_case slug splits A2A on the digit/uppercase
        // boundary into 'a2_a' — this is the documented durable
        // shape, not a typo. No prior test pins the exact wire
        // shape, the a2_a slug split, limit numeric serialization,
        // round-trip, or the default-on-missing decode contract
        // for this variant.
        let event = Request::RecentA2AResults { limit: 25 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit"],
            "Request::RecentA2AResults wire form must be exactly \
             two top-level keys: 'kind' plus the single 'limit' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed PageOptions would \
             nest 'limit' one level deeper and every CLI/HTTP \
             A2A-results listing probe that sends \
             {{\"kind\":\"recent_a2_a_results\",\"limit\":<n>}} \
             would fail to decode on the daemon side — operator \
             triage of A2A task results goes dark through the \
             supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("recent_a2_a_results")),
            "Request discriminator slug must be the durable \
             'recent_a2_a_results' (rename_all = snake_case \
             splits the A2A boundary digit/uppercase into 'a2_a' \
             — this is the documented form, not a typo). A \
             refactor that 'fixes' the slug to \
             'recent_a2a_results' would silently route incoming \
             listing frames to the daemon's catch-all error \
             branch — every CLI/HTTP listing probe fails and the \
             operator cannot enumerate A2A task results through \
             the supported path",
        );
        assert_eq!(
            obj.get("limit").and_then(serde_json::Value::as_u64),
            Some(25),
            "Request::RecentA2AResults::limit must surface as the \
             literal numeric page size — the daemon's listing \
             path binds on this exact field; a rename or retype \
             would silently return a different row count than the \
             operator asked for, distorting CLI output and HTTP \
             response payload sizes",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RecentA2AResults must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP A2A-results listing consumer \
             leans on",
        );

        let stale = serde_json::json!({"kind": "recent_a2_a_results"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::RecentA2AResults must decode from a payload \
             missing 'limit' — the #[serde(default)] attribute is \
             the durable compatibility hinge that lets stale CLIs \
             continue listing without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::RecentA2AResults { limit: 10 },
            "Request::RecentA2AResults with missing 'limit' must \
             default to default_recent_limit() = 10 — a refactor \
             that drops the #[serde(default)] attribute or \
             repoints it at a helper returning a different \
             constant silently changes the page size for every \
             stale CLI in the field; the operator's A2A-results \
             listing behaviour diverges from the documented \
             contract without a single error surface",
        );
    }

    #[test]
    fn request_recent_debits_serde_pins_default_bearing_single_field_variant() {
        // Request::RecentDebits is the operator-facing aggregate
        // variant the CLI and HTTP gateway send to enumerate the
        // most recent N budget-debit events across every agent
        // the daemon's router knows about. The daemon iterates
        // router.agents(), calls per-agent recent_debits on each
        // non-zero-budget card, and returns one flat list sorted
        // newest-first. limit defaults to default_recent_limit()
        // (10) via #[serde(default = "default_recent_limit")] so
        // a stale CLI can omit the field. It pairs with
        // Response::Debits (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='recent_debits' plus limit. No prior test
        // pins the exact wire shape, limit numeric serialization,
        // round-trip, or the default-on-missing decode contract
        // for this aggregate burn-rate path.
        let event = Request::RecentDebits { limit: 25 };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit"],
            "Request::RecentDebits wire form must be exactly two \
             top-level keys: 'kind' plus the single 'limit' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a typed PageOptions would \
             nest 'limit' one level deeper and every CLI/HTTP \
             burn-rate probe that sends \
             {{\"kind\":\"recent_debits\",\"limit\":<n>}} would \
             fail to decode on the daemon side — the operator's \
             per-agent burn-rate dashboard goes dark and budget \
             incidents are missed",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("recent_debits")),
            "Request discriminator slug must be the durable \
             'recent_debits'; a slug regression silently routes \
             incoming debits frames to the daemon's catch-all \
             error branch — every CLI/HTTP debits probe fails \
             with a confusing fallback message instead of \
             Response::Debits",
        );
        assert_eq!(
            obj.get("limit").and_then(serde_json::Value::as_u64),
            Some(25),
            "Request::RecentDebits::limit must surface as the \
             literal numeric page size — the daemon's aggregate \
             burn-rate path binds on this exact field; a rename \
             or retype would silently return a different row \
             count than the operator asked for, distorting CLI \
             output and HTTP response payload sizes",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RecentDebits must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP burn-rate consumer leans on",
        );

        let stale = serde_json::json!({"kind": "recent_debits"});
        let stale_decoded: Request = serde_json::from_value(stale).expect(
            "Request::RecentDebits must decode from a payload \
             missing 'limit' — the #[serde(default)] attribute is \
             the durable compatibility hinge that lets stale CLIs \
             continue listing without a rebuild",
        );
        assert_eq!(
            stale_decoded,
            Request::RecentDebits { limit: 10 },
            "Request::RecentDebits with missing 'limit' must \
             default to default_recent_limit() = 10 — a refactor \
             that drops the #[serde(default)] attribute or \
             repoints it at a helper returning a different \
             constant silently changes the page size for every \
             stale CLI in the field; the operator's burn-rate \
             aggregate returns zero rows when it should return \
             the documented default page, masking a real \
             overspend signal behind a phantom zero",
        );
    }

    #[test]
    fn request_send_a2a_task_serde_pins_struct_typed_single_field_variant() {
        // Request::SendA2ATask is the operator/agent-driven A2A
        // enqueue verb the CLI and HTTP gateway send to push an
        // A2ATask onto the daemon's queued mailbox. It pairs with
        // Response::A2ATaskQueued (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='send_a2_a_task' plus task. The A2A
        // snake_case quirk splits the slug into 'send_a2_a_task'
        // — durable documented form. Distinct from the
        // already-pinned single-field Request variants which carry
        // primitives (String/u64/usize/Uuid), this slice locks the
        // struct-typed outer wire shape: the nested A2ATask must
        // surface as a JSON object under 'task', not flattened
        // into the parent and not promoted to a tuple variant.
        // The inner A2ATask shape is pinned by covenant-a2a tests;
        // this slice only locks the outer Request variant shape.
        // A refactor that added #[serde(flatten)] to 'task',
        // dropped the nesting in favour of a tuple variant, or
        // rotated the field name would silently break every
        // CLI/HTTP enqueue caller.
        let task = covenant_a2a::A2ATask {
            id: Uuid::from_u128(0xA2A5_EEDE_F00D_CAFE_0000_BABE_BEEF),
            sender: covenant_types::AgentId::new("alice@local", [1u8; 32]),
            recipient: covenant_types::AgentId::new("bob@local", [2u8; 32]),
            intent_text: "fetch papers".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let event = Request::SendA2ATask { task: task.clone() };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "task"],
            "Request::SendA2ATask wire form must be exactly two \
             top-level keys: 'kind' plus the single 'task' field. \
             A refactor that added #[serde(flatten)] to 'task' \
             would collapse the inner A2ATask fields (id, sender, \
             recipient, intent_text) into the outer object next \
             to 'kind' and every CLI/HTTP enqueue caller that \
             sends {{\"kind\":\"send_a2_a_task\",\"task\":{{...}}}} \
             would fail to decode — the nested 'task' key would \
             vanish and the daemon's enqueue verb would go dark \
             through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("send_a2_a_task")),
            "Request discriminator slug must be the durable \
             'send_a2_a_task' (rename_all = snake_case splits the \
             A2A boundary digit/uppercase into 'a2_a' — this is \
             the documented form, not a typo). A refactor that \
             'fixes' the slug to 'send_a2a_task' would silently \
             route incoming enqueue frames to the daemon's \
             catch-all error branch and every CLI/HTTP A2A send \
             path would fail",
        );
        let task_value = obj.get("task").expect("'task' key must be present");
        assert!(
            task_value.is_object(),
            "Request::SendA2ATask::task must surface as a nested \
             JSON object — a refactor that promoted the variant \
             to a tuple (Request::SendA2ATask(A2ATask)) or that \
             changed the field to a string-encoded payload would \
             surface a non-object here; the operator's enqueue \
             path binds on the nested-object surface and any \
             other shape silently breaks every CLI/HTTP caller",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::SendA2ATask must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP A2A-enqueue consumer leans \
             on",
        );

        let mut missing = obj.clone();
        missing.remove("task");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::SendA2ATask wire form must reject a payload \
             missing 'task'; a stray #[serde(default)] would let \
             a malformed frame decode with A2ATask::default() and \
             the daemon would enqueue a phantom task with empty \
             id/sender/recipient/intent_text — every audit-row \
             attribution for the resulting flow would collapse to \
             a meaningless default subject and the mailbox would \
             accumulate ghost rows that operator triage could not \
             reconcile against a real sender",
        );
    }

    #[test]
    fn request_post_a2a_result_serde_pins_struct_typed_single_field_variant() {
        // Request::PostA2AResult is the agent-driven A2A
        // result-posting verb the CLI and HTTP gateway send to
        // push an A2ATaskResult back to the daemon for queue
        // consumers. It pairs with Response::A2AResultPosted
        // (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='post_a2_a_result' plus result. The A2A
        // snake_case quirk splits the slug into 'post_a2_a_result'
        // — durable documented form. This is the structural pair
        // to the just-pinned Request::SendA2ATask: same
        // struct-typed single-field shape, different inner type
        // (A2ATaskResult instead of A2ATask) and different slug.
        // The nested result must surface as a JSON object under
        // 'result', not flattened into the parent. The inner
        // A2ATaskResult shape is pinned by covenant-a2a tests;
        // this slice only locks the outer Request variant shape.
        let task_id = Uuid::from_u128(0xC0DE_F00D_BEEF_CAFE_1234_5678_9ABC_DEF0);
        let result = covenant_a2a::A2ATaskResult::ok(task_id, vec![]);
        let event = Request::PostA2AResult {
            result: result.clone(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "result"],
            "Request::PostA2AResult wire form must be exactly two \
             top-level keys: 'kind' plus the single 'result' \
             field. A refactor that added #[serde(flatten)] to \
             'result' would collapse the inner A2ATaskResult \
             fields (task_id, status, content, error_message) \
             into the outer object next to 'kind' and every \
             CLI/HTTP result-post caller that sends \
             {{\"kind\":\"post_a2_a_result\",\"result\":{{...}}}} \
             would fail to decode — the nested 'result' key would \
             vanish and the daemon's result-receive verb would go \
             dark through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("post_a2_a_result")),
            "Request discriminator slug must be the durable \
             'post_a2_a_result' (rename_all = snake_case splits \
             the A2A boundary digit/uppercase into 'a2_a' — this \
             is the documented form, not a typo). A refactor that \
             'fixes' the slug to 'post_a2a_result' would silently \
             route incoming result frames to the daemon's \
             catch-all error branch — every CLI/HTTP A2A \
             result-post fails and queued tasks never resolve \
             through the supported path",
        );
        let result_value = obj.get("result").expect("'result' key must be present");
        assert!(
            result_value.is_object(),
            "Request::PostA2AResult::result must surface as a \
             nested JSON object — a refactor that promoted the \
             variant to a tuple (Request::PostA2AResult(A2ATaskResult)) \
             or that changed the field to a string-encoded \
             payload would surface a non-object here; the \
             daemon's result-receive path binds on the \
             nested-object surface and any other shape silently \
             breaks every CLI/HTTP caller",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::PostA2AResult must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP A2A result-post consumer \
             leans on",
        );

        let mut missing = obj.clone();
        missing.remove("result");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::PostA2AResult wire form must reject a \
             payload missing 'result'; a stray #[serde(default)] \
             would let a malformed frame decode with \
             A2ATaskResult::default() and the daemon would record \
             a phantom result with empty task_id/status/content — \
             every audit-row attribution for the resulting \
             completion would collapse to a meaningless default \
             subject and queue consumers may never see the real \
             result they were waiting on",
        );
    }

    #[test]
    fn request_repair_memory_serde_pins_struct_typed_single_field_variant() {
        // Request::RepairMemory is the operator-controlled memory
        // drift repair verb the CLI and HTTP gateway send to push a
        // MemoryRepairRequest at the daemon. It pairs with
        // Response::MemoryRepaired (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='repair_memory' plus request. The slug has no
        // A2A quirk because the variant name is RepairMemory (no
        // digit/uppercase boundary), unlike the just-pinned
        // SendA2ATask/PostA2AResult pair whose slugs split into
        // 'send_a2_a_task' / 'post_a2_a_result'. This is the
        // structural sibling: same struct-typed single-field shape,
        // different inner type (MemoryRepairRequest instead of
        // A2ATask/A2ATaskResult) and a clean snake_case slug. The
        // nested request must surface as a JSON object under
        // 'request', not flattened into the parent and not promoted
        // to a tuple variant. The inner MemoryRepairRequest shape
        // is pinned by covenant-types tests; this slice only locks
        // the outer Request variant shape. A refactor that added
        // #[serde(flatten)] to 'request', dropped the nesting in
        // favour of a tuple variant, or rotated the field name
        // would silently break every CLI/HTTP memory-repair caller.
        let request = covenant_types::MemoryRepairRequest {
            mode: covenant_types::MemoryRepairMode::DryRun,
            command: covenant_types::MemoryRepairCommand::DeleteRecord { id: Uuid::nil() },
            reason: "test".into(),
        };
        let event = Request::RepairMemory {
            request: request.clone(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "request"],
            "Request::RepairMemory wire form must be exactly two \
             top-level keys: 'kind' plus the single 'request' \
             field. A refactor that added #[serde(flatten)] to \
             'request' would collapse the inner MemoryRepairRequest \
             fields (mode, command, reason) into the outer object \
             next to 'kind' and every CLI/HTTP memory-repair caller \
             that sends {{\"kind\":\"repair_memory\",\"request\":{{...}}}} \
             would fail to decode — the nested 'request' key would \
             vanish and the daemon's repair verb would go dark \
             through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("repair_memory")),
            "Request discriminator slug must be the durable \
             'repair_memory' (rename_all = snake_case on a plain \
             CamelCase name with no digit/uppercase boundary yields \
             a clean 'repair_memory'). A refactor that renamed the \
             variant (e.g., MemoryRepair) or removed the \
             rename_all attribute would shift the slug — incoming \
             frames would route to the daemon's catch-all error \
             branch and every CLI/HTTP memory-repair would fail \
             through the supported path",
        );
        let request_value = obj.get("request").expect("'request' key must be present");
        assert!(
            request_value.is_object(),
            "Request::RepairMemory::request must surface as a \
             nested JSON object — a refactor that promoted the \
             variant to a tuple (Request::RepairMemory(MemoryRepairRequest)) \
             or that changed the field to a string-encoded payload \
             would surface a non-object here; the daemon's repair \
             dispatch path binds on the nested-object surface and \
             any other shape silently breaks every CLI/HTTP caller",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RepairMemory must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP memory-repair consumer leans \
             on",
        );

        let mut missing = obj.clone();
        missing.remove("request");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::RepairMemory wire form must reject a payload \
             missing 'request'; a stray #[serde(default)] (paired \
             with a future Default on MemoryRepairRequest) would \
             let a malformed frame decode with a phantom repair \
             targeting a default record id and mode — every \
             audit-row attribution for the resulting repair would \
             collapse to a meaningless default subject and operator \
             drift remediation would silently mis-fire against the \
             wrong record",
        );
    }

    #[test]
    fn request_compact_memory_serde_pins_struct_typed_single_field_variant() {
        // Request::CompactMemory is the operator-controlled memory
        // retention/compaction verb the CLI and HTTP gateway send
        // to push a MemoryCompactionRequest at the daemon. It pairs
        // with Response::MemoryCompacted (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='compact_memory' plus request. The slug has no
        // A2A quirk because the variant name is CompactMemory (no
        // digit/uppercase boundary). This is the structural pair
        // to the just-pinned Request::RepairMemory: same
        // struct-typed single-field shape and same 'request' field
        // name, different inner type (MemoryCompactionRequest
        // instead of MemoryRepairRequest) and a different slug.
        // The nested request must surface as a JSON object under
        // 'request', not flattened into the parent and not promoted
        // to a tuple variant. The inner MemoryCompactionRequest
        // shape is pinned by covenant-types tests; this slice only
        // locks the outer Request variant shape.
        let request = covenant_types::MemoryCompactionRequest {
            mode: covenant_types::MemoryRepairMode::DryRun,
            policy: covenant_types::MemoryCompactionPolicy::default(),
            reason: "test".into(),
        };
        let event = Request::CompactMemory {
            request: request.clone(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "request"],
            "Request::CompactMemory wire form must be exactly two \
             top-level keys: 'kind' plus the single 'request' \
             field. A refactor that added #[serde(flatten)] to \
             'request' would collapse the inner MemoryCompactionRequest \
             fields (mode, policy, reason) into the outer object \
             next to 'kind' and every CLI/HTTP memory-compaction \
             caller that sends \
             {{\"kind\":\"compact_memory\",\"request\":{{...}}}} \
             would fail to decode — the nested 'request' key would \
             vanish and the daemon's compaction verb would go dark \
             through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("compact_memory")),
            "Request discriminator slug must be the durable \
             'compact_memory' (rename_all = snake_case on a plain \
             CamelCase name with no digit/uppercase boundary yields \
             a clean 'compact_memory'). A refactor that renamed \
             the variant (e.g., MemoryCompact) or removed the \
             rename_all attribute would shift the slug — incoming \
             frames would route to the daemon's catch-all error \
             branch and every CLI/HTTP memory-compaction would \
             fail through the supported path",
        );
        let request_value = obj.get("request").expect("'request' key must be present");
        assert!(
            request_value.is_object(),
            "Request::CompactMemory::request must surface as a \
             nested JSON object — a refactor that promoted the \
             variant to a tuple (Request::CompactMemory(MemoryCompactionRequest)) \
             or that changed the field to a string-encoded payload \
             would surface a non-object here; the daemon's \
             compaction dispatch path binds on the nested-object \
             surface and any other shape silently breaks every \
             CLI/HTTP caller",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::CompactMemory must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP memory-compaction consumer \
             leans on",
        );

        let mut missing = obj.clone();
        missing.remove("request");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::CompactMemory wire form must reject a \
             payload missing 'request'; a stray #[serde(default)] \
             would let a malformed frame decode with \
             MemoryCompactionRequest::default() (Default already \
             exists on MemoryCompactionPolicy, and a future Default \
             derive on the outer request would silently fire) and \
             the daemon would execute a phantom compaction with \
             empty policy targeting no tier and no cutoff — every \
             audit-row attribution for the resulting compaction \
             would collapse to a meaningless default subject and \
             operator-driven retention would silently no-op while \
             reporting success",
        );
    }

    #[test]
    fn request_repair_a2a_task_serde_pins_struct_typed_single_field_variant() {
        // Request::RepairA2ATask is the operator-controlled A2A
        // in-flight lease repair verb the CLI and HTTP gateway send
        // to push an A2ARepairRequest at the daemon. It pairs with
        // Response::A2ARepaired (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly two top-level
        // keys: kind='repair_a2_a_task' plus request. The A2A
        // snake_case quirk splits the slug into 'repair_a2_a_task'
        // — durable documented form, same shape as the already-
        // pinned send_a2_a_task / post_a2_a_result slugs. The
        // nested request must surface as a JSON object under
        // 'request', not flattened into the parent and not promoted
        // to a tuple variant. The inner A2ARepairRequest shape is
        // pinned by covenant-a2a tests; this slice only locks the
        // outer Request variant shape.
        let request = covenant_a2a::A2ARepairRequest {
            task_id: Uuid::nil(),
            command: covenant_a2a::A2ARepairCommand::Requeue {
                lease_id: None,
                duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
            },
            reason: "test".into(),
        };
        let event = Request::RepairA2ATask {
            request: request.clone(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "request"],
            "Request::RepairA2ATask wire form must be exactly two \
             top-level keys: 'kind' plus the single 'request' \
             field. A refactor that added #[serde(flatten)] to \
             'request' would collapse the inner A2ARepairRequest \
             fields (task_id, command, reason) into the outer \
             object next to 'kind' and every CLI/HTTP A2A-repair \
             caller that sends \
             {{\"kind\":\"repair_a2_a_task\",\"request\":{{...}}}} \
             would fail to decode — the nested 'request' key would \
             vanish and the daemon's lease-repair verb would go \
             dark through the supported path",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("repair_a2_a_task")),
            "Request discriminator slug must be the durable \
             'repair_a2_a_task' (rename_all = snake_case splits \
             the A2A boundary digit/uppercase into 'a2_a' — this \
             is the documented form, not a typo). A refactor that \
             'fixes' the slug to 'repair_a2a_task' would silently \
             route incoming A2A-repair frames to the daemon's \
             catch-all error branch — every CLI/HTTP A2A lease \
             repair fails and stale in-flight leases stop being \
             remediated through the supported path",
        );
        let request_value = obj.get("request").expect("'request' key must be present");
        assert!(
            request_value.is_object(),
            "Request::RepairA2ATask::request must surface as a \
             nested JSON object — a refactor that promoted the \
             variant to a tuple (Request::RepairA2ATask(A2ARepairRequest)) \
             or that changed the field to a string-encoded payload \
             would surface a non-object here; the daemon's \
             A2A-repair dispatch path binds on the nested-object \
             surface and any other shape silently breaks every \
             CLI/HTTP caller",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RepairA2ATask must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP A2A-repair consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("request");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::RepairA2ATask wire form must reject a \
             payload missing 'request'; a stray #[serde(default)] \
             paired with a future Default on A2ARepairRequest \
             would let a malformed frame decode as a phantom \
             repair targeting the nil task_id with an empty \
             command/reason — every audit-row attribution for the \
             resulting repair would collapse to a meaningless \
             default subject and operator stale-lease remediation \
             would silently mis-fire against the wrong lease",
        );
    }

    #[test]
    fn request_retry_a2a_stale_serde_pins_struct_typed_single_field_variant() {
        // Request::RetryA2AStale is the operator-controlled
        // stale-lease scan-and-requeue verb the CLI and HTTP
        // gateway send to push an A2AAutoRetryPolicy at the daemon.
        // It pairs with Response::A2AAutoRetried (already pinned).
        // With #[serde(tag = "kind", rename_all = "snake_case")] on
        // the Request enum, the wire object is exactly two
        // top-level keys: kind='retry_a2_a_stale' plus policy. The
        // A2A snake_case quirk splits the slug into
        // 'retry_a2_a_stale' — durable documented form, same shape
        // as the already-pinned send_a2_a_task / post_a2_a_result /
        // repair_a2_a_task slugs.
        //
        // Distinct from the other A2A struct-typed slices in that
        // the carried field name is 'policy' (not 'request' or
        // 'task' or 'result'); a refactor that rotated the field
        // name (e.g., to 'request' for surface parity with the
        // other A2A struct-typed variants) would silently break
        // every CLI/HTTP scan caller — the daemon would bind on
        // policy.* and the wire object would not contain that key.
        // The exact-keys assertion catches this. The inner
        // A2AAutoRetryPolicy shape is pinned by covenant-a2a
        // tests; this slice only locks the outer Request variant
        // shape.
        let policy = covenant_a2a::A2AAutoRetryPolicy::default();
        let event = Request::RetryA2AStale { policy };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "policy"],
            "Request::RetryA2AStale wire form must be exactly two \
             top-level keys: 'kind' plus the single 'policy' \
             field. A refactor that added #[serde(flatten)] to \
             'policy' would collapse the inner A2AAutoRetryPolicy \
             fields (enabled, min_lease_age_ms, max_attempts, \
             max_requeues, scan_limit) into the outer object next \
             to 'kind' and every CLI/HTTP stale-retry caller that \
             sends {{\"kind\":\"retry_a2_a_stale\",\"policy\":{{...}}}} \
             would fail to decode — the nested 'policy' key would \
             vanish and the daemon's opt-in stale-lease retry verb \
             would go dark through the supported path. A refactor \
             that renamed the field to 'request' for surface \
             parity with RepairA2ATask would also break this \
             assertion at the same point",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("retry_a2_a_stale")),
            "Request discriminator slug must be the durable \
             'retry_a2_a_stale' (rename_all = snake_case splits \
             the A2A boundary digit/uppercase into 'a2_a' — this \
             is the documented form, not a typo). A refactor that \
             'fixes' the slug to 'retry_a2a_stale' would silently \
             route incoming retry frames to the daemon's catch-all \
             error branch — every CLI/HTTP stale-retry call fails \
             and the opt-in retry scheduler stops being exercised \
             through the supported path",
        );
        let policy_value = obj.get("policy").expect("'policy' key must be present");
        assert!(
            policy_value.is_object(),
            "Request::RetryA2AStale::policy must surface as a \
             nested JSON object — a refactor that promoted the \
             variant to a tuple (Request::RetryA2AStale(A2AAutoRetryPolicy)) \
             or that changed the field to a boolean shorthand \
             (e.g., 'enable on/off') would surface a non-object \
             here; the daemon's stale-retry dispatch path binds on \
             the nested-object surface and any other shape \
             silently breaks every CLI/HTTP caller",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Request::RetryA2AStale must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP stale-retry consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("policy");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing)).is_err(),
            "Request::RetryA2AStale wire form must reject a \
             payload missing 'policy'; a stray #[serde(default)] \
             would let a malformed frame decode with \
             A2AAutoRetryPolicy::default() and the daemon would \
             silently run a no-op scan under the disabled default \
             policy — operators expecting an explicit policy push \
             would see 'success' with zero scanned leases and \
             never learn their opt-in policy never reached the \
             daemon. The missing-field rejection makes the \
             policy-omission failure mode loud at the IPC boundary",
        );
    }

    #[test]
    fn request_purge_memory_serde_pins_two_field_variant() {
        // Request::PurgeMemory is the operator-driven memory
        // retention verb the CLI and HTTP gateway send to drop
        // memory rows strictly older than before_ms, optionally
        // scoped to a single tier. It pairs with
        // Response::MemoryPurged (already pinned). With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Request enum, the wire object is exactly three top-level
        // keys: kind='purge_memory' plus tier plus before_ms.
        //
        // tier is Option<MemoryTier> with #[serde(default)] and NO
        // skip_serializing_if, so the durable wire form keeps tier
        // on the wire as JSON null when None and as a JSON string
        // when Some — three keys in both cases. The first
        // multi-field Request variant pin in this slice family.
        //
        // This slice locks: the exact wire shape (kind, tier,
        // before_ms), the snake_case discriminator 'purge_memory',
        // tier=null on the miss path, tier='working' on the hit
        // path (MemoryTier::Working serializes lowercase), round-
        // trip for both shapes, rejection of a frame missing
        // before_ms (the required field), and acceptance of a
        // frame missing tier (the #[serde(default)] decode-as-None
        // contract). A refactor that added skip_serializing_if on
        // tier would shrink the miss wire form from three keys to
        // two and silently break consumers that distinguish
        // scoped-vs-all purges on key presence.
        let miss = Request::PurgeMemory {
            tier: None,
            before_ms: 0,
        };

        let wire = serde_json::to_value(&miss).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["before_ms", "kind", "tier"],
            "Request::PurgeMemory wire form must be exactly three \
             top-level keys: 'kind' plus the two variant fields \
             ('tier', 'before_ms'). A refactor that added \
             #[serde(skip_serializing_if = \"Option::is_none\")] to \
             tier would shrink the miss-path wire form from three \
             keys to two and silently break CLI/HTTP consumers that \
             switch on the tier key's presence to distinguish \
             scoped-vs-all purges — the operator's CLI silently \
             reclassifies a scoped purge to all-tiers (or vice \
             versa) at the wire layer",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("purge_memory")),
            "Request discriminator slug must be the durable \
             'purge_memory'. A refactor that renamed the variant \
             (e.g., MemoryPurge for surface parity with the \
             internal verb order) would shift the slug — incoming \
             purge frames route to the daemon's catch-all error \
             branch and operator-driven retention goes dark \
             through the supported path",
        );
        assert_eq!(
            obj.get("tier"),
            Some(&serde_json::Value::Null),
            "Request::PurgeMemory::tier must surface as JSON null \
             when None (the durable null-on-wire surface, NOT a \
             missing key); a stray #[serde(skip_serializing_if = \
             \"Option::is_none\")] would shrink the miss-path wire \
             form and silently break CLI consumers that switch on \
             the key's presence to distinguish scoped vs all-tier \
             purges",
        );
        assert_eq!(
            obj.get("before_ms"),
            Some(&serde_json::json!(0)),
            "Request::PurgeMemory::before_ms must surface as a \
             JSON number; a refactor that promoted before_ms to a \
             string (e.g., for ISO-8601 timestamps) without a \
             corresponding bump would silently mismatch every \
             CLI/HTTP caller that sends a numeric u64 millisecond \
             cutoff",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, miss,
            "Request::PurgeMemory (miss) must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP memory-purge consumer leans \
             on",
        );

        let hit = Request::PurgeMemory {
            tier: Some(covenant_types::MemoryTier::Working),
            before_ms: 1_700_000_000_000,
        };
        let hit_wire = serde_json::to_value(&hit).unwrap();
        let hit_obj = hit_wire.as_object().unwrap();
        assert_eq!(
            hit_obj.get("tier").and_then(serde_json::Value::as_str),
            Some("working"),
            "populated tier must round-trip as the durable \
             lowercase MemoryTier slug 'working' (rename_all = \
             \"lowercase\" on MemoryTier); the three-key shape \
             stays stable across hit and miss",
        );
        let hit_back: Request = serde_json::from_value(hit_wire.clone()).unwrap();
        assert_eq!(
            hit_back, hit,
            "Request::PurgeMemory (hit) must round-trip through \
             serde_json verbatim",
        );

        let mut missing_required = obj.clone();
        missing_required.remove("before_ms");
        assert!(
            serde_json::from_value::<Request>(serde_json::Value::Object(missing_required)).is_err(),
            "Request::PurgeMemory wire form must reject a payload \
             missing 'before_ms'; a stray #[serde(default)] on \
             before_ms would let a malformed frame decode as \
             Request::PurgeMemory {{ before_ms: 0 }} and the daemon \
             would execute a no-op retention against the epoch — \
             the operator believes their retention ran while no \
             rows were touched",
        );

        let mut missing_optional = obj.clone();
        missing_optional.remove("tier");
        let parsed: Request =
            serde_json::from_value(serde_json::Value::Object(missing_optional)).unwrap();
        assert_eq!(
            parsed, miss,
            "Request::PurgeMemory wire form must accept a payload \
             missing 'tier' (Option<T> with #[serde(default)] \
             decodes as None); this is the documented forward-\
             compatibility contract for stale CLIs that predate the \
             tier filter. A refactor that dropped #[serde(default)] \
             on tier would silently break every CLI built before \
             the field was added",
        );
    }

    #[test]
    fn request_recent_memory_serde_pins_two_field_variant() {
        // Request::RecentMemory is the operator/CLI verb for paging
        // the most recent memory records, optionally scoped to a
        // single tier. It pairs with Response::Memories (already
        // pinned). With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Request enum, the wire object is
        // exactly three top-level keys: kind='recent_memory' plus
        // tier plus limit.
        //
        // tier is Option<MemoryTier> with #[serde(default)] and NO
        // skip_serializing_if. limit has #[serde(default =
        // "default_recent_limit")] returning 10. Both fields are
        // default-tolerant, so a stale CLI omitting either field
        // still decodes — distinct from the just-pinned
        // PurgeMemory which has a required before_ms field. This
        // slice pins the all-default-tolerant variant of the
        // multi-field shape.
        //
        // This slice locks: the exact wire shape (kind, tier,
        // limit), the snake_case discriminator 'recent_memory',
        // tier=null on the miss path, tier='working' on the hit
        // path, round-trip for both shapes, acceptance of a frame
        // missing tier (Option<T> with serde(default)), and
        // acceptance of a frame missing limit (decodes as 10 via
        // default_recent_limit).
        let miss = Request::RecentMemory {
            tier: None,
            limit: 10,
        };

        let wire = serde_json::to_value(&miss).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit", "tier"],
            "Request::RecentMemory wire form must be exactly three \
             top-level keys: 'kind' plus the two variant fields \
             ('tier', 'limit'). A refactor that added \
             #[serde(skip_serializing_if = \"Option::is_none\")] to \
             tier would shrink the miss-path wire form from three \
             keys to two and silently break CLI/HTTP consumers \
             that switch on the tier key's presence to distinguish \
             scoped-vs-all-tier pages",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("recent_memory")),
            "Request discriminator slug must be the durable \
             'recent_memory'. A refactor that renamed the variant \
             (e.g., MemoryRecent for surface parity with the \
             internal verb order) would shift the slug and \
             operator-driven memory inspection goes dark through \
             the supported path",
        );
        assert_eq!(
            obj.get("tier"),
            Some(&serde_json::Value::Null),
            "Request::RecentMemory::tier must surface as JSON null \
             when None (the durable null-on-wire surface, NOT a \
             missing key); a stray skip_serializing_if would shrink \
             the miss-path wire form and silently break CLI \
             consumers that switch on the key's presence",
        );
        assert_eq!(
            obj.get("limit"),
            Some(&serde_json::json!(10)),
            "Request::RecentMemory::limit must surface as a JSON \
             number; a refactor that promoted limit to a string or \
             enum would silently mismatch every CLI/HTTP caller \
             that sends a numeric usize page size",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, miss,
            "Request::RecentMemory (miss) must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI/HTTP recent-memory consumer leans \
             on",
        );

        let hit = Request::RecentMemory {
            tier: Some(covenant_types::MemoryTier::Working),
            limit: 25,
        };
        let hit_wire = serde_json::to_value(&hit).unwrap();
        let hit_obj = hit_wire.as_object().unwrap();
        assert_eq!(
            hit_obj.get("tier").and_then(serde_json::Value::as_str),
            Some("working"),
            "populated tier must round-trip as the durable \
             lowercase MemoryTier slug 'working' (rename_all = \
             \"lowercase\" on MemoryTier); the three-key shape \
             stays stable across hit and miss",
        );
        let hit_back: Request = serde_json::from_value(hit_wire.clone()).unwrap();
        assert_eq!(
            hit_back, hit,
            "Request::RecentMemory (hit) must round-trip through \
             serde_json verbatim",
        );

        let mut missing_tier = obj.clone();
        missing_tier.remove("tier");
        let parsed_no_tier: Request =
            serde_json::from_value(serde_json::Value::Object(missing_tier)).unwrap();
        assert_eq!(
            parsed_no_tier, miss,
            "Request::RecentMemory wire form must accept a payload \
             missing 'tier' (Option<T> with #[serde(default)] \
             decodes as None); this is the documented forward-\
             compatibility contract for stale CLIs that predate \
             the tier filter",
        );

        let mut missing_limit = obj.clone();
        missing_limit.remove("limit");
        let parsed_no_limit: Request =
            serde_json::from_value(serde_json::Value::Object(missing_limit)).unwrap();
        assert_eq!(
            parsed_no_limit, miss,
            "Request::RecentMemory wire form must accept a payload \
             missing 'limit' (decodes as 10 via \
             default_recent_limit); a refactor that dropped the \
             default would silently break stale CLIs that omit \
             limit, returning an empty page where operators expect \
             the latest rows",
        );
    }

    #[test]
    fn request_recent_receipts_serde_pins_two_field_variant() {
        // Request::RecentReceipts is the operator/CLI verb for
        // paging the most recent settlement receipts, optionally
        // bounded by since_ms. It pairs with Response::Receipts
        // (already pinned). With #[serde(tag = "kind", rename_all
        // = "snake_case")] on the Request enum, the wire object is
        // exactly three top-level keys: kind='recent_receipts'
        // plus limit plus since_ms.
        //
        // limit has #[serde(default = "default_recent_limit")]
        // returning 10. since_ms is Option<u64> with
        // #[serde(default)] and NO skip_serializing_if. The wire
        // form keeps since_ms on the wire as JSON null when None
        // and as a JSON number when Some — three keys in both
        // cases. Both fields are default-tolerant. Structural
        // twin of the just-pinned RecentMemory variant, different
        // Option payload type (u64 instead of MemoryTier) and
        // different slug.
        //
        // This slice locks: the exact wire shape, the snake_case
        // discriminator 'recent_receipts', since_ms=null on the
        // unbounded path, since_ms=<number> on the bounded path,
        // round-trip for both shapes, acceptance of frames missing
        // either field.
        let unbounded = Request::RecentReceipts {
            limit: 10,
            since_ms: None,
        };

        let wire = serde_json::to_value(&unbounded).unwrap();
        let obj = wire
            .as_object()
            .expect("Request serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "limit", "since_ms"],
            "Request::RecentReceipts wire form must be exactly \
             three top-level keys: 'kind' plus the two variant \
             fields ('limit', 'since_ms'). A refactor that added \
             #[serde(skip_serializing_if = \"Option::is_none\")] to \
             since_ms would shrink the unbounded-path wire form \
             from three keys to two and silently break CLI/HTTP \
             consumers that switch on the since_ms key's presence \
             to distinguish bounded-vs-unbounded pages",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("recent_receipts")),
            "Request discriminator slug must be the durable \
             'recent_receipts'. A refactor that renamed the variant \
             (e.g., ReceiptsRecent for surface parity) would shift \
             the slug and operator-driven receipt inspection goes \
             dark through the supported path",
        );
        assert_eq!(
            obj.get("since_ms"),
            Some(&serde_json::Value::Null),
            "Request::RecentReceipts::since_ms must surface as JSON \
             null when None (the durable null-on-wire surface, NOT \
             a missing key); a stray skip_serializing_if would \
             shrink the unbounded wire form and silently break CLI \
             consumers that switch on the key's presence",
        );
        assert_eq!(
            obj.get("limit"),
            Some(&serde_json::json!(10)),
            "Request::RecentReceipts::limit must surface as a JSON \
             number; a refactor that promoted limit to a string or \
             enum would silently mismatch every CLI/HTTP caller \
             that sends a numeric usize page size",
        );

        let back: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, unbounded,
            "Request::RecentReceipts (unbounded) must round-trip \
             through serde_json verbatim — the PartialEq derive \
             is the contract every CLI/HTTP recent-receipts \
             consumer leans on",
        );

        let bounded = Request::RecentReceipts {
            limit: 25,
            since_ms: Some(1_700_000_000_000),
        };
        let bounded_wire = serde_json::to_value(&bounded).unwrap();
        let bounded_obj = bounded_wire.as_object().unwrap();
        assert_eq!(
            bounded_obj.get("since_ms"),
            Some(&serde_json::json!(1_700_000_000_000u64)),
            "populated since_ms must round-trip as a JSON number \
             matching the u64 input; the three-key shape stays \
             stable across bounded and unbounded",
        );
        let bounded_back: Request = serde_json::from_value(bounded_wire.clone()).unwrap();
        assert_eq!(
            bounded_back, bounded,
            "Request::RecentReceipts (bounded) must round-trip \
             through serde_json verbatim",
        );

        let mut missing_since = obj.clone();
        missing_since.remove("since_ms");
        let parsed_no_since: Request =
            serde_json::from_value(serde_json::Value::Object(missing_since)).unwrap();
        assert_eq!(
            parsed_no_since, unbounded,
            "Request::RecentReceipts wire form must accept a \
             payload missing 'since_ms' (Option<T> with \
             #[serde(default)] decodes as None); this is the \
             forward-compatibility contract for stale CLIs that \
             predate the since_ms filter",
        );

        let mut missing_limit = obj.clone();
        missing_limit.remove("limit");
        let parsed_no_limit: Request =
            serde_json::from_value(serde_json::Value::Object(missing_limit)).unwrap();
        assert_eq!(
            parsed_no_limit, unbounded,
            "Request::RecentReceipts wire form must accept a \
             payload missing 'limit' (decodes as 10 via \
             default_recent_limit); a refactor that dropped the \
             default would silently break stale CLIs that omit \
             limit, returning an empty page where operators expect \
             the latest receipts",
        );
    }

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
        let obj = wire
            .as_object()
            .expect("ProtocolInfo serializes as a JSON object");
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
            payload.insert(
                "name".into(),
                serde_json::Value::String("hash_chain".into()),
            );
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
            payload.insert(
                "kind".into(),
                serde_json::Value::String("memory_drift".into()),
            );
            payload.insert(
                "message".into(),
                serde_json::Value::String("missing receipt".into()),
            );
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
            payload.insert(
                "batch_id".into(),
                serde_json::Value::String("batch-1".into()),
            );
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
            vec![
                "intent_id",
                "kind",
                "settlement",
                "sources",
                "status",
                "text"
            ],
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

    #[test]
    fn response_tool_list_serde_pins_single_field_variant() {
        // Response::ToolList is the variant the daemon sends after
        // ListTools returns the ToolSpec rows the caller's
        // authorization permits. It carries tools: Vec<ToolSpec> —
        // the tool catalog the CLI renders to the operator. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='tool_list' plus tools. No prior test pins the
        // exact wire shape, round-trip, or omission rejection of
        // this variant's required field. The inner ToolSpec element
        // wire form is pinned by covenant-mcp/covenant-tooling
        // tests; this slice locks the outer Response variant shape
        // only — an empty Vec is sufficient to catch the slug, key
        // set, and default-attribute regressions on the outer
        // variant. A refactor that promoted ToolList from a struct
        // variant to a newtype variant would nest 'tools' one level
        // deeper; a stray #[serde(default)] on tools would let a
        // malformed row decode with an empty list and the operator
        // would see a phantom empty tool catalog — masking a real
        // fetch failure as a clean state.
        let event = Response::ToolList { tools: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "tools"],
            "Response::ToolList wire form must be exactly two \
             top-level keys: 'kind' plus the single 'tools' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'tools' \
             one level deeper and every CLI consumer that \
             destructures the top-level array would silently fail — \
             the operator's tool catalog would render empty even \
             when the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("tool_list")),
            "Response discriminator slug must be the durable \
             'tool_list'; a slug regression silently strands every \
             CLI parser that classifies tool-catalog outcomes by \
             this exact value — the operator's CLI prints a \
             confusing fallback instead of rendering the tool list",
        );
        let tools_arr = obj
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .expect("Response::ToolList::tools must serialize as an array");
        assert_eq!(
            tools_arr.len(),
            0,
            "Response::ToolList::tools must round-trip the exact \
             element count from the wire payload — the empty-vec \
             construction is sufficient to lock the outer variant \
             shape; element-level wire form is pinned by \
             covenant-mcp/covenant-tooling",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::ToolList must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI tool-catalog consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("tools");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::ToolList wire form must reject a payload \
             missing 'tools'; a stray #[serde(default)] would let a \
             malformed row decode with an empty list and the CLI \
             would surface a phantom empty tool catalog — a real \
             fetch failure (truncated frame, partial decode error) \
             would be silently reclassified as a clean state where \
             the operator believes no tools are registered when in \
             fact the daemon's tool registry is populated",
        );
    }

    #[test]
    fn response_audit_events_serde_pins_single_field_variant() {
        // Response::AuditEvents is the variant the daemon sends
        // after RecentAudit returns the AuditEvent rows the caller's
        // authorization permits. It carries events: Vec<AuditEvent>
        // — the audit timeline the CLI renders to the operator.
        // With #[serde(tag = "kind", rename_all = "snake_case")] on
        // the Response enum, the wire object is exactly two
        // top-level keys: kind='audit_events' plus events. No prior
        // test pins the exact wire shape, round-trip, or omission
        // rejection of this variant's required field. The inner
        // AuditEvent element wire form is pinned by covenant-audit
        // tests; this slice locks the outer Response variant shape
        // only — an empty Vec is sufficient to catch the slug, key
        // set, and default-attribute regressions on the outer
        // variant. A stray #[serde(default)] on events would mask a
        // real fetch failure as a clean state — a particularly
        // dangerous failure mode for the audit surface, where a
        // phantom empty timeline could mislead a security reviewer.
        let event = Response::AuditEvents { events: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["events", "kind"],
            "Response::AuditEvents wire form must be exactly two \
             top-level keys: 'kind' plus the single 'events' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'events' \
             one level deeper and every CLI consumer that \
             destructures the top-level array would silently fail — \
             the operator's audit timeline would render empty even \
             when the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("audit_events")),
            "Response discriminator slug must be the durable \
             'audit_events'; a slug regression silently strands \
             every CLI parser that classifies recent-audit outcomes \
             by this exact value — the operator's CLI prints a \
             confusing fallback instead of rendering the audit \
             timeline",
        );
        let events_arr = obj
            .get("events")
            .and_then(serde_json::Value::as_array)
            .expect("Response::AuditEvents::events must serialize as an array");
        assert_eq!(
            events_arr.len(),
            0,
            "Response::AuditEvents::events must round-trip the \
             exact element count from the wire payload — the \
             empty-vec construction is sufficient to lock the outer \
             variant shape; element-level wire form is pinned by \
             covenant-audit",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::AuditEvents must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI recent-audit consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("events");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::AuditEvents wire form must reject a payload \
             missing 'events'; a stray #[serde(default)] would let \
             a malformed row decode with an empty list and the CLI \
             would surface a phantom empty audit timeline — a real \
             fetch failure (truncated frame, partial decode error) \
             would be silently reclassified as a clean state where \
             a security reviewer believes no audit events exist \
             when the daemon's audit log is in fact populated",
        );
    }

    #[test]
    fn response_debits_serde_pins_single_field_variant() {
        // Response::Debits is the variant the daemon sends after
        // RecentDebits returns the BudgetDebit rows the caller's
        // authorization permits. It carries debits: Vec<BudgetDebit>
        // — the budget ledger the CLI renders to the operator. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='debits' plus debits. No prior test pins the
        // exact wire shape, round-trip, or omission rejection of
        // this variant's required field. The inner BudgetDebit
        // element wire form is pinned by covenant-budget tests;
        // this slice locks the outer Response variant shape only —
        // an empty Vec is sufficient to catch the slug, key set,
        // and default-attribute regressions on the outer variant.
        // A stray #[serde(default)] on debits would let a malformed
        // row decode with an empty list and the operator would see
        // a phantom empty ledger — hiding cost overruns from
        // triage.
        let event = Response::Debits { debits: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["debits", "kind"],
            "Response::Debits wire form must be exactly two \
             top-level keys: 'kind' plus the single 'debits' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'debits' \
             one level deeper and every CLI consumer that \
             destructures the top-level array would silently fail — \
             the operator's debit list would render empty even when \
             the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("debits")),
            "Response discriminator slug must be the durable \
             'debits'; a slug regression silently strands every CLI \
             parser that classifies recent-debits outcomes by this \
             exact value — the operator's CLI prints a confusing \
             fallback instead of rendering the budget ledger",
        );
        let debits_arr = obj
            .get("debits")
            .and_then(serde_json::Value::as_array)
            .expect("Response::Debits::debits must serialize as an array");
        assert_eq!(
            debits_arr.len(),
            0,
            "Response::Debits::debits must round-trip the exact \
             element count from the wire payload — the empty-vec \
             construction is sufficient to lock the outer variant \
             shape; element-level wire form is pinned by \
             covenant-budget",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::Debits must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI recent-debits consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("debits");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::Debits wire form must reject a payload \
             missing 'debits'; a stray #[serde(default)] would let \
             a malformed row decode with an empty list and the CLI \
             would surface a phantom empty budget ledger — a real \
             fetch failure (truncated frame, partial decode error) \
             would be silently reclassified as a clean state where \
             the operator believes no spend has occurred when the \
             daemon's budget ledger is in fact populated, hiding \
             cost overruns",
        );
    }

    #[test]
    fn response_a2a_tasks_serde_pins_single_field_variant() {
        // Response::A2ATasks is the variant the daemon sends after
        // RecentA2ATasks returns the A2ATask rows the caller's
        // authorization permits. It carries tasks: Vec<A2ATask> —
        // the agent-to-agent task list the CLI renders to the
        // operator. With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly two top-level keys: kind='a2_a_tasks' plus tasks.
        // The 'a2_a_' shape (not 'a2a_') is a serde rename_all
        // snake_case quirk where the rule splits on every Upper
        // boundary, including the digit-to-upper transition — the
        // wire form is durable and any "fix" would silently break
        // every CLI A2A queue parser. No prior test pins the exact
        // wire shape, round-trip, or omission rejection of this
        // variant's required field. The inner A2ATask element wire
        // form is pinned by covenant-a2a tests; this slice locks
        // the outer Response variant shape only.
        let event = Response::A2ATasks { tasks: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "tasks"],
            "Response::A2ATasks wire form must be exactly two \
             top-level keys: 'kind' plus the single 'tasks' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'tasks' \
             one level deeper and every CLI A2A queue consumer that \
             destructures the top-level array would silently fail — \
             the operator's A2A task list would render empty even \
             when the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_tasks")),
            "Response discriminator slug must be the durable \
             'a2_a_tasks'; serde rename_all=snake_case splits on \
             every Upper boundary including the digit→upper \
             transition, so A2ATasks emits 'a2_a_tasks' on the \
             wire. A refactor that 'fixes' the slug to 'a2a_tasks' \
             would silently strand every CLI A2A queue parser",
        );
        let tasks_arr = obj
            .get("tasks")
            .and_then(serde_json::Value::as_array)
            .expect("Response::A2ATasks::tasks must serialize as an array");
        assert_eq!(
            tasks_arr.len(),
            0,
            "Response::A2ATasks::tasks must round-trip the exact \
             element count from the wire payload — the empty-vec \
             construction is sufficient to lock the outer variant \
             shape; element-level wire form is pinned by \
             covenant-a2a",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2ATasks must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI A2A queue consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("tasks");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2ATasks wire form must reject a payload \
             missing 'tasks'; a stray #[serde(default)] would let a \
             malformed row decode with an empty list and the CLI \
             would surface a phantom empty A2A queue — a real fetch \
             failure (truncated frame, partial decode error) would \
             be silently reclassified as a clean state where the \
             operator believes no agent tasks are pending when the \
             daemon's queue is in fact populated",
        );
    }

    #[test]
    fn response_a2a_results_serde_pins_single_field_variant() {
        // Response::A2AResults is the variant the daemon sends
        // after RecentA2AResults returns the A2ATaskResult rows the
        // caller's authorization permits. It carries results:
        // Vec<A2ATaskResult> — the agent-to-agent result list the
        // CLI renders to the operator. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the
        // wire object is exactly two top-level keys:
        // kind='a2_a_results' plus results. The 'a2_a_' shape (not
        // 'a2a_') is a serde rename_all snake_case quirk where the
        // rule splits on every Upper boundary, including the
        // digit-to-upper transition — the wire form is durable and
        // any "fix" would silently break every CLI A2A result
        // parser. No prior test pins the exact wire shape,
        // round-trip, or omission rejection of this variant's
        // required field. The inner A2ATaskResult element wire form
        // is pinned by covenant-a2a tests; this slice locks the
        // outer Response variant shape only.
        let event = Response::A2AResults { results: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "results"],
            "Response::A2AResults wire form must be exactly two \
             top-level keys: 'kind' plus the single 'results' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'results' one level deeper and every CLI consumer that \
             destructures the top-level array would silently fail — \
             the operator's A2A result list would render empty even \
             when the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_results")),
            "Response discriminator slug must be the durable \
             'a2_a_results'; serde rename_all=snake_case splits on \
             every Upper boundary including the digit→upper \
             transition, so A2AResults emits 'a2_a_results' on the \
             wire. A refactor that 'fixes' the slug to 'a2a_results' \
             would silently strand every CLI A2A result parser",
        );
        let results_arr = obj
            .get("results")
            .and_then(serde_json::Value::as_array)
            .expect("Response::A2AResults::results must serialize as an array");
        assert_eq!(
            results_arr.len(),
            0,
            "Response::A2AResults::results must round-trip the \
             exact element count from the wire payload — the \
             empty-vec construction is sufficient to lock the outer \
             variant shape; element-level wire form is pinned by \
             covenant-a2a",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2AResults must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI A2A result consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("results");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2AResults wire form must reject a payload \
             missing 'results'; a stray #[serde(default)] would let \
             a malformed row decode with an empty list and the CLI \
             would surface a phantom empty A2A result history — a \
             real fetch failure (truncated frame, partial decode \
             error) would be silently reclassified as a clean state \
             where the operator believes no agent tasks have \
             completed when the daemon's result store is in fact \
             populated",
        );
    }

    #[test]
    fn response_protocol_info_serde_pins_single_field_variant() {
        // Response::ProtocolInfo is the variant the daemon sends in
        // response to Request::ProtocolInfo — it carries info:
        // ProtocolInfo, the struct containing the daemon's protocol
        // name, current version, and supported version range that
        // every CLI uses to pick the correct wire dialect. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='protocol_info' plus info. The inner
        // ProtocolInfo struct shape is already pinned by
        // protocol_info_serde_pins_required_fields_and_rejected_renames
        // and protocol_info_matches_v1_fixture; this slice locks the
        // outer Response variant shape only — that info appears as a
        // nested object, that the discriminator slug is the durable
        // 'protocol_info', and that the variant rejects a payload
        // missing 'info'. A refactor that promoted the variant from
        // struct to newtype wrapping ProtocolInfo would either
        // inline its fields next to kind or nest one level deeper;
        // either form would silently break every CLI consumer that
        // reads .info.protocol.
        let event = Response::ProtocolInfo {
            info: protocol_info(),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["info", "kind"],
            "Response::ProtocolInfo wire form must be exactly two \
             top-level keys: 'kind' plus the single 'info' field. A \
             refactor that promoted the variant from struct to \
             newtype wrapping ProtocolInfo would either inline \
             ProtocolInfo's fields next to 'kind' or nest 'info' \
             one level deeper — either form silently breaks every \
             CLI consumer that reads .info.protocol",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("protocol_info")),
            "Response discriminator slug must be the durable \
             'protocol_info'; a slug regression silently strands \
             every CLI parser that branches on kind=protocol_info — \
             clients fall through to a generic-error branch and \
             either reject the response or fail-open with a default \
             dialect, masking a real protocol mismatch",
        );
        assert!(
            obj.get("info")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::ProtocolInfo::info must serialize as a nested \
             JSON object — the inner ProtocolInfo shape is pinned by \
             sibling tests; this slice only locks that info appears \
             as one keyed object under the outer variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::ProtocolInfo must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI version-probe consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("info");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::ProtocolInfo wire form must reject a payload \
             missing 'info'; a stray #[serde(default)] would let a \
             malformed row decode with a default ProtocolInfo and \
             the CLI would commit to an incorrect protocol dialect \
             — a real fetch failure (truncated frame, partial \
             decode error) would be silently reclassified as a \
             clean version-probe and downstream calls would fail in \
             confusing ways",
        );
    }

    #[test]
    fn response_chain_status_serde_pins_single_field_variant() {
        // Response::ChainStatus is the variant the daemon sends in
        // response to Request::ChainStatus — it carries status:
        // ChainStatus, the struct containing the chain name,
        // cluster, RPC and WebSocket URLs, program ID, COVNT mint,
        // a readiness flag, and a list of missing configuration
        // entries. With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly two top-level keys: kind='chain_status' plus
        // status. No prior test pins the exact wire shape,
        // round-trip, or omission rejection of this variant's
        // required field. The inner ChainStatus struct shape is
        // pinned by covenant-settlement tests; this slice locks the
        // outer Response variant shape only — a minimally
        // constructed ChainStatus is sufficient to catch the slug,
        // key set, and default-attribute regressions on the outer
        // variant.
        let event = Response::ChainStatus {
            status: ChainStatus {
                chain: "solana".into(),
                cluster: "devnet".into(),
                rpc_url: None,
                ws_url: None,
                program_id: None,
                covnt_mint: None,
                ready: false,
                missing: vec![],
            },
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "status"],
            "Response::ChainStatus wire form must be exactly two \
             top-level keys: 'kind' plus the single 'status' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping ChainStatus would either inline its \
             fields next to 'kind' or nest 'status' one level \
             deeper — either form silently breaks every CLI \
             consumer that reads .status.ready or \
             .status.last_anchored_receipt",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("chain_status")),
            "Response discriminator slug must be the durable \
             'chain_status'; a slug regression silently strands \
             every CLI parser that branches on kind=chain_status — \
             the operator's CLI prints a confusing fallback instead \
             of the chain-status panel",
        );
        assert!(
            obj.get("status")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::ChainStatus::status must serialize as a \
             nested JSON object — the inner ChainStatus shape is \
             pinned by covenant-settlement tests; this slice only \
             locks that status appears as one keyed object under \
             the outer variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::ChainStatus must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI chain-status consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("status");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::ChainStatus wire form must reject a payload \
             missing 'status'; a stray #[serde(default)] would let \
             a malformed row decode with a default ChainStatus and \
             the CLI would surface a phantom 'clean chain' state — \
             a real fetch failure (truncated frame, partial decode \
             error) would be silently reclassified as a clean state \
             where the operator believes no settlement pressure \
             exists when the chain is in fact lagging or stuck",
        );
    }

    #[test]
    fn response_receipt_batches_serde_pins_single_field_variant() {
        // Response::ReceiptBatches is the variant the daemon sends
        // in response to Request::ReceiptBatches — it carries
        // batches: Vec<ReceiptBatchSummary>, the receipt batch
        // summaries (batch_id, merkle_root, receipt_count) the CLI
        // renders to the operator. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the
        // wire object is exactly two top-level keys:
        // kind='receipt_batches' plus batches. No prior test pins
        // the exact wire shape, round-trip, or omission rejection
        // of this variant's required field. The inner
        // ReceiptBatchSummary element wire form is exercised by
        // sibling ReceiptBatchFlushed tests; this slice locks the
        // outer Response variant shape only — an empty Vec is
        // sufficient to catch the slug, key set, and
        // default-attribute regressions on the outer variant.
        let event = Response::ReceiptBatches { batches: vec![] };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["batches", "kind"],
            "Response::ReceiptBatches wire form must be exactly two \
             top-level keys: 'kind' plus the single 'batches' \
             field. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'batches' one level deeper and every CLI consumer that \
             destructures the top-level array would silently fail — \
             the operator's batch list would render empty even when \
             the daemon returned populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("receipt_batches")),
            "Response discriminator slug must be the durable \
             'receipt_batches'; a slug regression silently strands \
             every CLI parser that classifies receipt-batches \
             outcomes by this exact value — the operator's CLI \
             prints a confusing fallback instead of the batch list",
        );
        let batches_arr = obj
            .get("batches")
            .and_then(serde_json::Value::as_array)
            .expect("Response::ReceiptBatches::batches must serialize as an array");
        assert_eq!(
            batches_arr.len(),
            0,
            "Response::ReceiptBatches::batches must round-trip the \
             exact element count from the wire payload — the \
             empty-vec construction is sufficient to lock the outer \
             variant shape; element-level wire form is exercised by \
             sibling ReceiptBatchFlushed tests",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::ReceiptBatches must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI receipt-batches consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("batches");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::ReceiptBatches wire form must reject a \
             payload missing 'batches'; a stray #[serde(default)] \
             would let a malformed row decode with an empty list \
             and the CLI would surface a phantom empty batch \
             history — a real fetch failure (truncated frame, \
             partial decode error) would be silently reclassified \
             as a clean state where the operator believes no \
             receipt batches have been anchored when the daemon's \
             chain has in fact flushed batches",
        );
    }

    #[test]
    fn response_memory_repaired_serde_pins_single_field_variant() {
        // Response::MemoryRepaired is the variant the daemon sends
        // after Request::MemoryRepair applies (or dry-runs) a scoped
        // repair against a memory row — detaching a stale parent,
        // deleting a record, or backfilling provenance — and reports
        // the per-row outcome. It carries outcome: MemoryRepairOutcome,
        // the struct the CLI surfaces (id, action, mode, would_change,
        // changed, optional before/after snapshots) so the operator
        // can confirm exactly which row was touched and how. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='memory_repaired' plus outcome. No prior test
        // pins the exact wire shape, round-trip, or omission
        // rejection of this variant's required field. The inner
        // MemoryRepairOutcome shape is pinned by covenant-memory and
        // covenant-types tests; this slice locks the outer Response
        // variant shape only — a minimally constructed outcome is
        // sufficient to catch the slug, key set, and default-attribute
        // regressions on the outer variant.
        let event = Response::MemoryRepaired {
            outcome: MemoryRepairOutcome {
                id: Uuid::nil(),
                action: covenant_types::MemoryRepairAction::DetachParent,
                mode: covenant_types::MemoryRepairMode::DryRun,
                would_change: false,
                changed: false,
                before: None,
                after: None,
            },
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "outcome"],
            "Response::MemoryRepaired wire form must be exactly two \
             top-level keys: 'kind' plus the single 'outcome' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping MemoryRepairOutcome would either inline \
             its fields next to 'kind' or nest 'outcome' one level \
             deeper — either form silently breaks every CLI consumer \
             that reads .outcome.id, .outcome.action, or \
             .outcome.changed",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("memory_repaired")),
            "Response discriminator slug must be the durable \
             'memory_repaired'; a slug regression silently strands \
             every CLI parser that classifies memory-repair outcomes \
             by this exact value — the operator's CLI prints a \
             confusing fallback instead of the repair outcome, \
             masking whether the row was changed, would-be-changed \
             (dry-run), or untouched",
        );
        assert!(
            obj.get("outcome")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::MemoryRepaired::outcome must serialize as a \
             nested JSON object — the inner MemoryRepairOutcome shape \
             is pinned by covenant-types tests; this slice only locks \
             that outcome appears as one keyed object under the outer \
             variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::MemoryRepaired must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI memory-repair consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("outcome");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::MemoryRepaired wire form must reject a payload \
             missing 'outcome'; a stray #[serde(default)] would let a \
             malformed row decode with a synthetic default \
             MemoryRepairOutcome and the CLI would surface a phantom \
             'unchanged' state — a real fetch failure (truncated \
             frame, partial decode error) would be silently \
             reclassified as a clean apply where the operator \
             believes the repair completed without mutation when in \
             fact the daemon's repair path produced an error the \
             boundary swallowed",
        );
    }

    #[test]
    fn response_memory_compacted_serde_pins_single_field_variant() {
        // Response::MemoryCompacted is the variant the daemon sends
        // after Request::MemoryCompact runs a bounded compaction pass
        // against the local memory store — deleting expired
        // working/episodic rows, marking long-term rows stale, and
        // optionally detaching stale parents. It carries outcome:
        // MemoryCompactionOutcome, the per-pass evidence the CLI
        // surfaces (mode, would_change, changed, deleted ids,
        // stale_marked ids, parents_detached ids) so the operator
        // can confirm exactly which rows were touched and how. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='memory_compacted' plus outcome. No prior test
        // pins the exact wire shape, round-trip, or omission
        // rejection of this variant's required field. The inner
        // MemoryCompactionOutcome shape is pinned by
        // covenant-types/covenant-memory tests; this slice locks the
        // outer Response variant shape only — a minimally constructed
        // outcome is sufficient to catch the slug, key set, and
        // default-attribute regressions on the outer variant.
        let event = Response::MemoryCompacted {
            outcome: MemoryCompactionOutcome {
                mode: covenant_types::MemoryRepairMode::DryRun,
                would_change: false,
                changed: false,
                deleted: vec![],
                stale_marked: vec![],
                parents_detached: vec![],
            },
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "outcome"],
            "Response::MemoryCompacted wire form must be exactly two \
             top-level keys: 'kind' plus the single 'outcome' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping MemoryCompactionOutcome would either \
             inline its fields next to 'kind' or nest 'outcome' one \
             level deeper — either form silently breaks every CLI \
             consumer that reads .outcome.deleted, \
             .outcome.stale_marked, or .outcome.changed",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("memory_compacted")),
            "Response discriminator slug must be the durable \
             'memory_compacted'; a slug regression silently strands \
             every CLI parser that classifies memory-compaction \
             outcomes by this exact value — the operator's CLI prints \
             a confusing fallback instead of the compaction outcome, \
             masking whether the pass was a dry-run, a no-op apply, \
             or actually deleted/stale-marked/detached rows",
        );
        assert!(
            obj.get("outcome")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::MemoryCompacted::outcome must serialize as a \
             nested JSON object — the inner MemoryCompactionOutcome \
             shape is pinned by covenant-types tests; this slice only \
             locks that outcome appears as one keyed object under the \
             outer variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::MemoryCompacted must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI memory-compaction consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("outcome");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::MemoryCompacted wire form must reject a \
             payload missing 'outcome'; a stray #[serde(default)] \
             would let a malformed row decode with a synthetic \
             default MemoryCompactionOutcome (empty deleted, \
             stale_marked, parents_detached lists with changed=false) \
             and the CLI would surface a phantom no-op state — a real \
             fetch failure (truncated frame, partial decode error) \
             would be silently reclassified as a clean apply where \
             the operator believes no rows were compacted when in \
             fact the daemon's compaction path produced an error the \
             boundary swallowed",
        );
    }

    #[test]
    fn response_audit_integrity_serde_pins_single_field_variant() {
        // Response::AuditIntegrity is the variant the daemon sends in
        // response to Request::AuditIntegrity — it carries report:
        // AuditIntegrityReport, the per-pass integrity evidence the
        // CLI surfaces (events, anchors, valid, root_hash_hex,
        // failures) so the operator can confirm the daemon's
        // hash-chain sidecar matches the audit log. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='audit_integrity' plus report. No prior test
        // pins the exact wire shape, round-trip, or omission
        // rejection of this variant's required field. The inner
        // AuditIntegrityReport shape is pinned by covenant-audit
        // tests; this slice locks the outer Response variant shape
        // only — a minimally constructed report is sufficient to
        // catch the slug, key set, and default-attribute regressions
        // on the outer variant.
        let event = Response::AuditIntegrity {
            report: AuditIntegrityReport {
                events: 0,
                anchors: 0,
                valid: true,
                root_hash_hex: String::new(),
                failures: vec![],
            },
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "report"],
            "Response::AuditIntegrity wire form must be exactly two \
             top-level keys: 'kind' plus the single 'report' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping AuditIntegrityReport would either \
             inline its fields next to 'kind' or nest 'report' one \
             level deeper — either form silently breaks every CLI \
             consumer that reads .report.valid, .report.root_hash_hex, \
             or .report.failures",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("audit_integrity")),
            "Response discriminator slug must be the durable \
             'audit_integrity'; a slug regression silently strands \
             every CLI parser that classifies integrity-report \
             outcomes by this exact value — the operator's CLI prints \
             a confusing fallback instead of the integrity verdict, \
             masking whether the audit chain is valid, broken, or \
             empty",
        );
        assert!(
            obj.get("report")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::AuditIntegrity::report must serialize as a \
             nested JSON object — the inner AuditIntegrityReport \
             shape is pinned by covenant-audit tests; this slice only \
             locks that report appears as one keyed object under the \
             outer variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::AuditIntegrity must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI integrity-report consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("report");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::AuditIntegrity wire form must reject a payload \
             missing 'report'; a stray #[serde(default)] would let a \
             malformed row decode with a synthetic default \
             AuditIntegrityReport (events=0, anchors=0, valid=false, \
             empty root_hash_hex, empty failures) and the CLI would \
             surface a phantom 'no events' state — a real fetch \
             failure (truncated frame, partial decode error) would be \
             silently reclassified as a clean empty chain where the \
             operator believes the audit log holds nothing when the \
             daemon's chain is in fact populated and the fetch failed",
        );
    }

    #[test]
    fn response_a2a_repaired_serde_pins_single_field_variant() {
        // Response::A2ARepaired is the variant the daemon sends after
        // Request::A2ARepair runs a per-task repair against the A2A
        // mailbox — requeuing a stale leased task or force-erroring a
        // stuck one — and reports the per-task outcome. It carries
        // outcome: A2ARepairOutcome, the struct the CLI surfaces
        // (task_id, action, state, attempt, optional result snapshot)
        // so the operator can confirm exactly which task was touched
        // and the resulting state. With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the wire
        // object is exactly two top-level keys: kind='a2_a_repaired'
        // (the rename_all = snake_case rule splits A2A on the
        // digit/upper boundary into 'a2_a_...'; the durable wire form
        // matches the sibling A2ACompacted/A2ATaskQueued/
        // A2AResultPosted pins) plus outcome. No prior test pins the
        // exact wire shape, round-trip, or omission rejection of this
        // variant's required field. The inner A2ARepairOutcome shape
        // is pinned by covenant-a2a tests; this slice locks the outer
        // Response variant shape only — a minimally constructed
        // outcome is sufficient to catch the slug, key set, and
        // default-attribute regressions on the outer variant.
        let event = Response::A2ARepaired {
            outcome: A2ARepairOutcome {
                task_id: Uuid::nil(),
                action: covenant_a2a::A2ARepairAction::Requeued,
                state: covenant_a2a::A2ARepairState::Queued,
                attempt: 0,
                result: None,
            },
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "outcome"],
            "Response::A2ARepaired wire form must be exactly two \
             top-level keys: 'kind' plus the single 'outcome' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping A2ARepairOutcome would either inline \
             its fields next to 'kind' or nest 'outcome' one level \
             deeper — either form silently breaks every CLI consumer \
             that reads .outcome.task_id, .outcome.action, or \
             .outcome.state",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_repaired")),
            "Response discriminator slug must be the durable \
             'a2_a_repaired' (rename_all = snake_case splits A2A on \
             digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies A2A-repair \
             outcomes by this exact value — the operator's CLI prints \
             a confusing fallback instead of the repair outcome, \
             masking whether the task was requeued or force-errored",
        );
        assert!(
            obj.get("outcome")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::A2ARepaired::outcome must serialize as a \
             nested JSON object — the inner A2ARepairOutcome shape \
             is pinned by covenant-a2a tests; this slice only locks \
             that outcome appears as one keyed object under the outer \
             variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2ARepaired must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI A2A-repair consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("outcome");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2ARepaired wire form must reject a payload \
             missing 'outcome'; a stray #[serde(default)] would let a \
             malformed row decode with a synthetic default \
             A2ARepairOutcome and the CLI would surface a phantom \
             no-op state — a real fetch failure (truncated frame, \
             partial decode error) would be silently reclassified as \
             a clean apply where the operator believes the repair \
             touched nothing when in fact the daemon's repair path \
             produced an error the boundary swallowed",
        );
    }

    #[test]
    fn response_a2a_auto_retried_serde_pins_single_field_variant() {
        // Response::A2AAutoRetried is the variant the daemon sends
        // after Request::A2AAutoRetry runs a bounded scan of stale
        // leases and reports which were requeued versus skipped (and
        // why). It carries report: A2AAutoRetryReport, the per-pass
        // evidence the CLI surfaces (policy, considered count,
        // requeued list, skipped list) so the operator can confirm
        // the auto-retry scheduler made meaningful progress. With
        // #[serde(tag = "kind", rename_all = "snake_case")] on the
        // Response enum, the wire object is exactly two top-level
        // keys: kind='a2_a_auto_retried' (the rename_all =
        // snake_case rule splits A2A on the digit/upper boundary
        // into 'a2_a_...'; the durable wire form matches the sibling
        // A2ARepaired/A2ACompacted/A2ATaskQueued pins) plus report.
        // No prior test pins the exact wire shape, round-trip, or
        // omission rejection of this variant's required field. The
        // inner A2AAutoRetryReport shape is pinned by covenant-a2a
        // tests; this slice locks the outer Response variant shape
        // only — a minimally constructed report is sufficient to
        // catch the slug, key set, and default-attribute regressions
        // on the outer variant.
        let event = Response::A2AAutoRetried {
            report: A2AAutoRetryReport::new(A2AAutoRetryPolicy::default()),
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "report"],
            "Response::A2AAutoRetried wire form must be exactly two \
             top-level keys: 'kind' plus the single 'report' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping A2AAutoRetryReport would either inline \
             its fields next to 'kind' or nest 'report' one level \
             deeper — either form silently breaks every CLI consumer \
             that reads .report.considered, .report.requeued, or \
             .report.skipped",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_auto_retried")),
            "Response discriminator slug must be the durable \
             'a2_a_auto_retried' (rename_all = snake_case splits A2A \
             on digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies auto-retry \
             outcomes by this exact value — the operator's CLI prints \
             a confusing fallback instead of the auto-retry report, \
             masking whether the scheduler made progress",
        );
        assert!(
            obj.get("report")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::A2AAutoRetried::report must serialize as a \
             nested JSON object — the inner A2AAutoRetryReport shape \
             is pinned by covenant-a2a tests; this slice only locks \
             that report appears as one keyed object under the outer \
             variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2AAutoRetried must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI auto-retry consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("report");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::A2AAutoRetried wire form must reject a payload \
             missing 'report'; a stray #[serde(default)] would let a \
             malformed row decode with a synthetic default \
             A2AAutoRetryReport and the CLI would surface a phantom \
             'no progress' state — a real fetch failure (truncated \
             frame, partial decode error) would be silently \
             reclassified as a clean apply where the operator \
             believes the scheduler scanned zero rows when in fact \
             the daemon's scheduler ran and the response payload was \
             corrupted",
        );
    }

    #[test]
    fn response_peer_revoked_serde_pins_single_field_variant() {
        // Response::PeerRevoked is the variant the daemon sends in
        // response to Request::RevokePeer. It carries outcome:
        // RevokeOutcome, the four-case tagged enum the CLI surfaces
        // (Revoked, AlreadyRevoked, NotFound, Ambiguous) so the
        // operator can see exactly which outcome the prefix matched.
        // With #[serde(tag = "kind", rename_all = "snake_case")] on
        // the Response enum, the wire object is exactly two top-level
        // keys: kind='peer_revoked' plus outcome. No prior test pins
        // the exact wire shape, round-trip, or omission rejection of
        // this variant's required field. The inner RevokeOutcome
        // shape is pinned by covenant-peer-auth tests; this slice
        // locks the outer Response variant shape only — the NotFound
        // unit-variant case is the lowest-construction-cost
        // RevokeOutcome and exercises the same outer surface as every
        // other case.
        let event = Response::PeerRevoked {
            outcome: RevokeOutcome::NotFound,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "outcome"],
            "Response::PeerRevoked wire form must be exactly two \
             top-level keys: 'kind' plus the single 'outcome' field. \
             A refactor that promoted the variant from struct to \
             newtype wrapping RevokeOutcome would either inline its \
             fields next to 'kind' or nest 'outcome' one level \
             deeper — either form silently breaks every CLI consumer \
             that reads .outcome.type to switch on the four \
             RevokeOutcome cases",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("peer_revoked")),
            "Response discriminator slug must be the durable \
             'peer_revoked'; a slug regression silently strands every \
             CLI parser that classifies revoke outcomes by this exact \
             value — the operator's CLI prints a confusing fallback \
             instead of the revoke outcome, masking whether the \
             prefix-matched peer was actually removed",
        );
        assert!(
            obj.get("outcome")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::PeerRevoked::outcome must serialize as a \
             nested JSON object — the inner RevokeOutcome shape is \
             pinned by covenant-peer-auth tests; this slice only \
             locks that outcome appears as one keyed object under the \
             outer variant",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::PeerRevoked must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI revoke-outcome consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("outcome");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::PeerRevoked wire form must reject a payload \
             missing 'outcome'; a stray #[serde(default)] would let a \
             malformed row decode with a synthetic default \
             RevokeOutcome (none of the four cases) and the CLI would \
             surface a phantom 'unknown' state — a real fetch failure \
             (truncated frame, partial decode error) would be \
             silently reclassified as a clean apply where the \
             operator believes the prefix matched nothing when in \
             fact the daemon's revoke path produced an error the \
             boundary swallowed",
        );
    }

    #[test]
    fn response_a2a_task_opt_serde_pins_single_optional_field_variant() {
        // Response::A2ATaskOpt is the variant the daemon sends after
        // Request::A2ATask resolves a single A2A task lookup —
        // either a hit (Some(task)) or a miss (None). It carries
        // task: Option<A2ATask> without a
        // #[serde(skip_serializing_if = "Option::is_none")] attribute,
        // so the wire form is stable across hit and miss: the two
        // keys (kind + task) are always emitted, with task=null on
        // the miss path. This is the durable null-on-wire contract —
        // a refactor that adds skip_serializing_if would silently
        // drop the task key on the miss path and every CLI consumer
        // that destructures .task to switch between hit and miss
        // would silently treat misses as malformed responses.
        //
        // Per the project rule for Option-only variants, this test
        // skips an omission walk on the task field: Option<T>
        // decodes from a missing key as None, so an omission assertion
        // would be vacuously true and would not catch a
        // skip_serializing_if regression — the null-on-wire assertion
        // is the regression catcher. The inner A2ATask shape is
        // pinned by covenant-a2a tests; this slice locks the outer
        // Response variant null-on-wire shape only.
        let event = Response::A2ATaskOpt { task: None };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "task"],
            "Response::A2ATaskOpt wire form must always be exactly \
             two top-level keys: 'kind' plus 'task' — adding \
             skip_serializing_if to task would silently drop the key \
             on the miss path and the wire shape would shrink from \
             two keys to one, breaking every CLI consumer that \
             destructures .task to detect hit vs miss",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_task_opt")),
            "Response discriminator slug must be the durable \
             'a2_a_task_opt' (rename_all = snake_case splits A2A on \
             digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies single-task \
             lookups by this exact value — the operator's CLI prints \
             a confusing fallback instead of the lookup outcome",
        );
        assert_eq!(
            obj.get("task"),
            Some(&serde_json::Value::Null),
            "Response::A2ATaskOpt::task must surface as JSON null on \
             the miss path — this null-on-wire surface is the \
             durable contract that lets every CLI consumer \
             destructure .task across hit and miss without a \
             shape-shift; adding skip_serializing_if would silently \
             drop this assertion and the miss path would emit only \
             kind, breaking the contract",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2ATaskOpt must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI single-task-lookup consumer leans on",
        );
    }

    #[test]
    fn response_a2a_result_opt_serde_pins_single_optional_field_variant() {
        // Response::A2AResultOpt is the variant the daemon sends
        // after Request::A2AResult resolves a single A2A task result
        // lookup — either a hit (Some(result)) or a miss (None). It
        // carries result: Option<A2ATaskResult> without a
        // #[serde(skip_serializing_if = "Option::is_none")] attribute,
        // so the wire form is stable across hit and miss: the two
        // keys (kind + result) are always emitted, with result=null
        // on the miss path. This is the durable null-on-wire contract
        // — a refactor that adds skip_serializing_if would silently
        // drop the result key on the miss path and every CLI
        // consumer that destructures .result to switch between hit
        // and miss would silently treat misses as malformed
        // responses.
        //
        // Per the project rule for Option-only variants, this test
        // skips an omission walk on the result field: Option<T>
        // decodes from a missing key as None, so an omission
        // assertion would be vacuously true and would not catch a
        // skip_serializing_if regression — the null-on-wire
        // assertion is the regression catcher. The inner
        // A2ATaskResult shape is pinned by covenant-a2a tests; this
        // slice locks the outer Response variant null-on-wire shape
        // only.
        let event = Response::A2AResultOpt { result: None };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "result"],
            "Response::A2AResultOpt wire form must always be exactly \
             two top-level keys: 'kind' plus 'result' — adding \
             skip_serializing_if to result would silently drop the \
             key on the miss path and the wire shape would shrink \
             from two keys to one, breaking every CLI consumer that \
             destructures .result to detect hit vs miss",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_result_opt")),
            "Response discriminator slug must be the durable \
             'a2_a_result_opt' (rename_all = snake_case splits A2A \
             on digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies single-result \
             lookups by this exact value — the operator's CLI prints \
             a confusing fallback instead of the lookup outcome",
        );
        assert_eq!(
            obj.get("result"),
            Some(&serde_json::Value::Null),
            "Response::A2AResultOpt::result must surface as JSON null \
             on the miss path — this null-on-wire surface is the \
             durable contract that lets every CLI consumer \
             destructure .result across hit and miss without a \
             shape-shift; adding skip_serializing_if would silently \
             drop this assertion and the miss path would emit only \
             kind, breaking the contract",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2AResultOpt must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI single-result-lookup consumer leans \
             on",
        );
    }

    #[test]
    fn response_receipt_batch_flushed_serde_pins_two_field_variant() {
        // Response::ReceiptBatchFlushed is the variant the daemon
        // sends after Request::FlushReceiptBatch anchors a batch of
        // local receipts into a single on-chain Merkle root. It
        // carries batch: ReceiptBatchSummary (batch_id, merkle_root,
        // receipt_count, tx_sig, slot) plus receipts_updated: u64
        // (the count of local receipts re-correlated to the new
        // batch). With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly three top-level keys: kind='receipt_batch_flushed'
        // plus the two variant fields. No prior test pins the exact
        // wire shape, round-trip, or omission rejection of these
        // required fields at the outer Response level. The inner
        // ReceiptBatchSummary shape is pinned by
        // receipt_batch_summary_serde_pins_default_not_skip_and_required_fields;
        // this slice locks the outer Response variant shape only.
        let event = Response::ReceiptBatchFlushed {
            batch: ReceiptBatchSummary {
                batch_id: "batch-1".into(),
                merkle_root: "00".repeat(32),
                receipt_count: 0,
                tx_sig: None,
                slot: None,
            },
            receipts_updated: 0,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["batch", "kind", "receipts_updated"],
            "Response::ReceiptBatchFlushed wire form must be exactly \
             three top-level keys: 'kind' plus the two variant \
             fields. A refactor that promoted the variant from \
             struct to newtype wrapping a payload struct would nest \
             'batch' and 'receipts_updated' one level deeper and \
             every CLI consumer that destructures .batch.merkle_root \
             or .receipts_updated would silently fail — the \
             operator's flush confirmation would read blank for every \
             anchor pass",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("receipt_batch_flushed")),
            "Response discriminator slug must be the durable \
             'receipt_batch_flushed'; a slug regression silently \
             strands every CLI parser that classifies flush outcomes \
             by this exact value — the operator's CLI prints a \
             confusing fallback instead of the flush evidence, \
             masking whether the daemon anchored a batch and how \
             many receipts were re-correlated",
        );
        assert!(
            obj.get("batch")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "Response::ReceiptBatchFlushed::batch must serialize as a \
             nested JSON object — the inner ReceiptBatchSummary shape \
             is pinned by sibling tests; this slice only locks that \
             batch appears as one keyed object under the outer \
             variant",
        );
        assert_eq!(
            obj.get("receipts_updated")
                .and_then(serde_json::Value::as_u64),
            Some(0),
            "Response::ReceiptBatchFlushed::receipts_updated must \
             surface as the u64 row count verbatim — the operator \
             reads this exact integer to confirm how many local \
             receipts were re-correlated to the new on-chain anchor",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::ReceiptBatchFlushed must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI flush-confirmation consumer leans on",
        );

        for required in ["batch", "receipts_updated"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::ReceiptBatchFlushed wire form must reject \
                 a payload missing {required:?}; a stray \
                 #[serde(default)] on receipts_updated would let a \
                 malformed row decode with receipts_updated=0 and \
                 the CLI would surface a phantom 'no anchor progress' \
                 state — a default on batch would let a malformed \
                 row decode with a synthetic empty batch and the \
                 operator would believe no anchor evidence exists \
                 when in fact the daemon's flush path produced an \
                 error the boundary swallowed",
            );
        }
    }

    #[test]
    fn response_a2a_queue_serde_pins_two_field_variant() {
        // Response::A2AQueue is the variant the daemon sends after
        // Request::A2AQueueState dumps the operator-visible A2A
        // mailbox state in one payload. It carries tasks:
        // Vec<A2ATaskQueueEntry> (queued/in-flight entries with lease
        // metadata) plus results: Vec<A2ATaskResult> (terminal
        // results not yet pruned). With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the wire
        // object is exactly three top-level keys: kind='a2_a_queue'
        // plus the two variant fields. No prior test pins the exact
        // wire shape, round-trip, or omission rejection of these
        // required fields at the outer Response level. The inner
        // A2ATaskQueueEntry and A2ATaskResult shapes are pinned by
        // covenant-a2a tests; this slice locks the outer Response
        // variant shape only — empty Vec constructions are sufficient
        // to catch the slug, key set, and default-attribute
        // regressions on the outer variant.
        let event = Response::A2AQueue {
            tasks: vec![],
            results: vec![],
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "results", "tasks"],
            "Response::A2AQueue wire form must be exactly three \
             top-level keys: 'kind' plus the two variant fields. A \
             refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest 'tasks' \
             and 'results' one level deeper and every CLI consumer \
             that destructures .tasks or .results would silently \
             fail — the operator's queue-state dump would read blank \
             for every snapshot",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("a2_a_queue")),
            "Response discriminator slug must be the durable \
             'a2_a_queue' (rename_all = snake_case splits A2A on \
             digit/upper boundaries); a slug regression silently \
             strands every CLI parser that classifies queue-state \
             outcomes by this exact value — the operator's CLI \
             prints a confusing fallback instead of the queue \
             snapshot, masking the mailbox state during incident \
             triage",
        );
        assert_eq!(
            obj.get("tasks")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0),
            "Response::A2AQueue::tasks must serialize as an array — \
             the empty-vec construction is sufficient to lock the \
             outer variant shape; element-level wire form is \
             exercised by covenant-a2a tests",
        );
        assert_eq!(
            obj.get("results")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0),
            "Response::A2AQueue::results must serialize as an array — \
             the empty-vec construction is sufficient to lock the \
             outer variant shape; element-level wire form is \
             exercised by covenant-a2a tests",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::A2AQueue must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI queue-snapshot consumer leans on",
        );

        for required in ["tasks", "results"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::A2AQueue wire form must reject a payload \
                 missing {required:?}; a stray #[serde(default)] \
                 would let a malformed row decode with an empty list \
                 and the CLI would surface a phantom drained mailbox \
                 — a real fetch failure (truncated frame, partial \
                 decode error) would be silently reclassified as a \
                 clean empty state where the operator believes the \
                 mailbox is drained when in fact the daemon's \
                 snapshot path produced an error the boundary \
                 swallowed",
            );
        }
    }

    #[test]
    fn response_verify_report_serde_pins_four_field_variant() {
        // Response::VerifyReport is the variant the daemon sends
        // after Request::Verify runs the bounded local audit-chain
        // verifier and reports per-check results plus any drift
        // rows the verifier surfaced. It carries window: usize
        // (rows scanned), checks: Vec<VerifyCheck> (per-check
        // pass/fail evidence), drift: Vec<VerifyDrift> annotated
        // with #[serde(default)] (rows the verifier flagged as
        // drifted from expected state — the field is serde(default)
        // WITHOUT skip_serializing_if so the wire form stays stable
        // across drift/no-drift states), and orphans_total: u64
        // (count of rows the verifier could not bind to a parent).
        // With #[serde(tag = "kind", rename_all = "snake_case")] on
        // the Response enum, the wire object is exactly five
        // top-level keys: kind='verify_report' plus the four
        // variant fields. No prior test pins the exact wire shape,
        // round-trip, or the asymmetric serde(default)-not-skip
        // contract on `drift` at the outer Response level. The
        // inner VerifyCheck/VerifyDrift shapes are pinned by
        // sibling tests; this slice locks the outer variant shape
        // only — empty Vec constructions are sufficient to catch
        // the slug, key set, and default-attribute regressions on
        // the outer variant.
        let event = Response::VerifyReport {
            window: 0,
            checks: vec![],
            drift: vec![],
            orphans_total: 0,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["checks", "drift", "kind", "orphans_total", "window"],
            "Response::VerifyReport wire form must be exactly five \
             top-level keys: 'kind' plus the four variant fields. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest the \
             fields one level deeper and every CLI consumer that \
             destructures .checks or .drift would silently fail — \
             the operator's audit verify confirmation would read \
             blank for every pass even when the daemon ran the \
             verifier against populated rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("verify_report")),
            "Response discriminator slug must be the durable \
             'verify_report'; a slug regression silently strands \
             every CLI parser that classifies verify outcomes by \
             this exact value — the operator's CLI prints a \
             confusing fallback instead of the verify report, \
             masking whether the verifier ran or what it found",
        );
        assert!(
            obj.get("drift")
                .and_then(serde_json::Value::as_array)
                .is_some(),
            "Response::VerifyReport::drift must always surface as \
             a JSON array (the durable not-skip-serializing-if \
             surface — the five-key shape stays stable across \
             drift/no-drift states); a stray \
             #[serde(skip_serializing_if = \"Vec::is_empty\")] on \
             drift would shrink the wire shape and silently break \
             CLI consumers that switch on the key's presence to \
             distinguish clean-pass from no-drift-this-window",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::VerifyReport must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI verify-report consumer leans on",
        );

        for required in ["window", "checks", "orphans_total"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::VerifyReport wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] would let a malformed row decode \
                 with a synthetic default and the CLI would surface \
                 a phantom 'verifier ran clean' state — a real \
                 fetch failure (truncated frame, partial decode \
                 error) would be silently reclassified where the \
                 operator believes the audit-chain is clean when in \
                 fact the daemon's verify path produced an error \
                 the boundary swallowed",
            );
        }

        let forward_compat = serde_json::json!({
            "kind": "verify_report",
            "window": 0,
            "checks": [],
            "orphans_total": 0,
        });
        let decoded: Response = serde_json::from_value(forward_compat).unwrap();
        assert_eq!(
            decoded,
            Response::VerifyReport {
                window: 0,
                checks: vec![],
                drift: vec![],
                orphans_total: 0,
            },
            "Response::VerifyReport with drift omitted must decode \
             as an empty Vec; dropping #[serde(default)] from drift \
             would break stale CLIs built before the drift field \
             landed (or a newer CLI talking to an older daemon \
             that omits the key) by surfacing a confusing serde \
             error instead of degrading cleanly",
        );
    }

    #[test]
    fn response_ignore_report_serde_pins_three_field_variant() {
        // Response::IgnoreReport is the variant the daemon sends
        // after Request::IgnoreCheck classifies an intent string
        // against the loaded ignore rules. It carries
        // ignored: bool (whether the intent matched a rule),
        // matched_pattern: Option<String> (the rule body,
        // populated only on hit), and rules_loaded: usize (total
        // rule count, for confirming the ruleset surface is what
        // the operator expects). With #[serde(tag = "kind",
        // rename_all = "snake_case")] on the Response enum, the
        // wire object is exactly four top-level keys:
        // kind='ignore_report' plus the three variant fields.
        // matched_pattern has no #[serde(skip_serializing_if)]
        // attribute, so the wire form is stable across hit and
        // miss: matched_pattern surfaces as JSON null when None
        // and as a JSON string when Some. No prior test pins the
        // exact wire shape, round-trip, or omission rejection of
        // these fields at the outer Response level. This slice
        // locks the outer Response variant shape and the durable
        // null-on-wire contract for matched_pattern only.
        let miss = Response::IgnoreReport {
            ignored: false,
            matched_pattern: None,
            rules_loaded: 0,
        };

        let wire = serde_json::to_value(&miss).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["ignored", "kind", "matched_pattern", "rules_loaded"],
            "Response::IgnoreReport wire form must be exactly four \
             top-level keys: 'kind' plus the three variant fields. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest the \
             fields one level deeper and every CLI consumer that \
             destructures .ignored or .matched_pattern would \
             silently fail — the operator's `ignore check` output \
             would read blank for every intent even when the \
             daemon's ruleset matched the row",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("ignore_report")),
            "Response discriminator slug must be the durable \
             'ignore_report'; a slug regression silently strands \
             every CLI parser that classifies ignore-check \
             outcomes by this exact value — the operator's CLI \
             prints a confusing fallback instead of the ignore \
             classification, masking whether the intent was \
             suppressed",
        );
        assert_eq!(
            obj.get("matched_pattern"),
            Some(&serde_json::Value::Null),
            "Response::IgnoreReport::matched_pattern must surface \
             as JSON null when None (the durable null-on-wire \
             surface, NOT a missing key); a stray \
             #[serde(skip_serializing_if = \"Option::is_none\")] \
             would shrink the miss-path wire form from four keys \
             to three and silently break CLI consumers that switch \
             on the key's presence to distinguish hit vs miss",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, miss,
            "Response::IgnoreReport (miss) must round-trip through \
             serde_json verbatim — the PartialEq derive is the \
             contract every CLI ignore-check consumer leans on",
        );

        let hit = Response::IgnoreReport {
            ignored: true,
            matched_pattern: Some("deploy".into()),
            rules_loaded: 1,
        };
        let hit_wire = serde_json::to_value(&hit).unwrap();
        let hit_obj = hit_wire.as_object().unwrap();
        assert_eq!(
            hit_obj
                .get("matched_pattern")
                .and_then(serde_json::Value::as_str),
            Some("deploy"),
            "populated matched_pattern must round-trip verbatim on \
             the wire — the four-key shape stays stable across hit \
             and miss",
        );
        let hit_back: Response = serde_json::from_value(hit_wire.clone()).unwrap();
        assert_eq!(
            hit_back, hit,
            "Response::IgnoreReport (hit) must round-trip through \
             serde_json verbatim",
        );

        for required in ["ignored", "rules_loaded"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
                "Response::IgnoreReport wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] would let a malformed row decode \
                 with a synthetic default and the CLI would surface \
                 a phantom 'clean state with empty ruleset' — a \
                 real fetch failure (truncated frame, partial \
                 decode error) would be silently reclassified as a \
                 no-match outcome where the operator believes the \
                 intent is safe to dispatch when in fact the \
                 daemon's classifier produced an error the boundary \
                 swallowed",
            );
        }
    }

    #[test]
    fn response_peer_list_serde_pins_three_field_variant() {
        // Response::PeerList is the variant the daemon sends after
        // Request::ListPeers dumps the operator-visible peer
        // registry snapshot in one payload. It carries
        // peers: Vec<PeerSummary> (the rendered rows, post-filter,
        // post-prefix-match), operator_pubkey_b58: String
        // annotated with #[serde(default)] (the local operator
        // pubkey so the CLI can self-mark its own row), and
        // truncated: bool annotated with #[serde(default)] (whether
        // the result was capped at limit and a follow-up page
        // exists). With #[serde(tag = "kind", rename_all =
        // "snake_case")] on the Response enum, the wire object is
        // exactly four top-level keys: kind='peer_list' plus the
        // three variant fields. Both serde(default) fields have NO
        // skip_serializing_if, so the wire form is stable across
        // populated and default states — operator_pubkey_b58
        // surfaces as a JSON string (empty when unset) and
        // truncated surfaces as a JSON bool. No prior test pins
        // the exact wire shape, round-trip, or the forward-compat
        // decode contract at the outer Response level. The inner
        // PeerSummary shape is pinned by covenant-peer-auth tests;
        // this slice locks the outer variant shape and the
        // asymmetric serde(default)-not-skip-serializing-if
        // contract on operator_pubkey_b58 and truncated only.
        let event = Response::PeerList {
            peers: vec![],
            operator_pubkey_b58: String::new(),
            truncated: false,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("Response serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["kind", "operator_pubkey_b58", "peers", "truncated"],
            "Response::PeerList wire form must be exactly four \
             top-level keys: 'kind' plus the three variant fields. \
             A refactor that promoted the variant from struct to \
             newtype wrapping a payload struct would nest the \
             fields one level deeper and every CLI consumer that \
             destructures .peers or .truncated would silently fail \
             — the operator's peers list would read blank for \
             every snapshot even when the daemon returned populated \
             rows",
        );
        assert_eq!(
            obj.get("kind"),
            Some(&serde_json::json!("peer_list")),
            "Response discriminator slug must be the durable \
             'peer_list'; a slug regression silently strands every \
             CLI parser that classifies peer-list outcomes by this \
             exact value — the operator's CLI prints a confusing \
             fallback instead of the peer snapshot, masking the \
             registry state during incident triage",
        );
        assert_eq!(
            obj.get("operator_pubkey_b58")
                .and_then(serde_json::Value::as_str),
            Some(""),
            "Response::PeerList::operator_pubkey_b58 must always \
             surface as a JSON string (the durable \
             not-skip-serializing-if surface — the four-key shape \
             stays stable across populated and default states); a \
             stray #[serde(skip_serializing_if = \
             \"String::is_empty\")] would shrink the default-state \
             wire form to three keys and silently break CLI \
             consumers that switch on the key's presence to render \
             the self-mark",
        );
        assert_eq!(
            obj.get("truncated").and_then(serde_json::Value::as_bool),
            Some(false),
            "Response::PeerList::truncated must always surface as \
             a JSON bool (the durable not-skip-serializing-if \
             surface); a stray \
             #[serde(skip_serializing_if = \"std::ops::Not::not\")] \
             would shrink the default-state wire form to three keys \
             and silently break CLI consumers that switch on the \
             key's presence to render the truncation banner",
        );
        assert_eq!(
            obj.get("peers")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0),
            "Response::PeerList::peers must serialize as an array — \
             the empty-vec construction is sufficient to lock the \
             outer variant shape; element-level wire form is \
             pinned by covenant-peer-auth tests",
        );

        let back: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "Response::PeerList must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract every \
             CLI peers-list consumer leans on",
        );

        let mut missing = obj.clone();
        missing.remove("peers");
        assert!(
            serde_json::from_value::<Response>(serde_json::Value::Object(missing)).is_err(),
            "Response::PeerList wire form must reject a payload \
             missing 'peers'; a stray #[serde(default)] on peers \
             would let a malformed row decode with an empty list \
             and the CLI would surface a phantom empty registry — \
             a real fetch failure (truncated frame, partial decode \
             error) would be silently reclassified as a clean \
             empty state where the operator believes no peers \
             exist when in fact the daemon's snapshot path \
             produced an error the boundary swallowed",
        );

        let forward_compat = serde_json::json!({
            "kind": "peer_list",
            "peers": [],
        });
        let decoded: Response = serde_json::from_value(forward_compat).unwrap();
        assert_eq!(
            decoded,
            Response::PeerList {
                peers: vec![],
                operator_pubkey_b58: String::new(),
                truncated: false,
            },
            "Response::PeerList with operator_pubkey_b58 and \
             truncated omitted must decode with both at their \
             defaults; dropping #[serde(default)] from either field \
             would break stale CLIs built before the fields landed \
             (or a newer CLI talking to an older daemon that omits \
             the keys) by surfacing a confusing serde error \
             instead of degrading cleanly to an empty self-mark or \
             a no-truncation banner",
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
