//! Append-only audit log with hash-chain integrity verification.
//!
//! Every intent dispatch, capability check/grant/rejection, budget
//! enforcement, agent-to-agent messaging, tool approval and
//! invocation, memory maintenance, settlement receipt backfill, peer
//! and operator administration, and authentication-failure event
//! produces one [`AuditEvent`]
//! (successful revocations write tombstones to the capability and peer
//! registries instead; only rejected revocations land here). Wire
//! format is JSONL — one event per line, easy to tail or grep — and the
//! [`AuditLog`] trait abstracts over a JSONL-backed implementation
//! suitable for production and an in-memory implementation suitable
//! for tests. [`AuditLog::verify_integrity`] returns an
//! [`AuditIntegrityReport`] computed from the per-event hash chain;
//! this is the verdict that `covenant audit verify` and the HTTP
//! `GET /audit/verify` surface consume.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("chain corruption: events file has {events} rows, chain file has {chain}; refusing to rebuild — events > chain is the recoverable shape, rerun purge_older_than with the same cutoff to truncate the orphan events; events < chain means the chain was tampered or restored from a stale backup, rebuild from a trusted backup")]
    ChainCorruption { events: usize, chain: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: Uuid,
    pub timestamp_ms: u64,
    pub issuer: AgentId,
    pub kind: AuditKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditChainEntry {
    pub index: u64,
    pub event_id: Uuid,
    pub timestamp_ms: u64,
    pub event_hash_hex: String,
    pub previous_hash_hex: String,
    pub chain_hash_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditIntegrityReport {
    pub events: u64,
    pub anchors: u64,
    pub valid: bool,
    pub root_hash_hex: String,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditKind {
    IntentDispatched {
        intent_id: Uuid,
        intent_text: String,
        matched_agent: Option<String>,
        result_hash_hex: String,
        status: String,
    },
    /// A tool invocation inside a Hermes-runtime agent's run. `preview_hash_hex`
    /// is the SHA-256 of Hermes's short tool-input preview; we hash before
    /// persisting so the audit chain never embeds raw tool input.
    HermesToolInvoked {
        intent_id: Uuid,
        run_id: String,
        tool: String,
        preview_hash_hex: String,
    },
    /// A tool invocation in a Hermes run finished. `error` is `true` iff
    /// the tool itself raised; a `false` here followed by a failed run
    /// status means Hermes failed elsewhere in the loop.
    HermesToolCompleted {
        intent_id: Uuid,
        run_id: String,
        tool: String,
        duration_ms: u64,
        error: bool,
    },
    /// A Hermes run paused pending operator approval. Recorded so a run
    /// stalled at the approval prompt is auditable even when the
    /// operator console is closed.
    HermesApprovalRequested {
        intent_id: Uuid,
        run_id: String,
        choices: Vec<String>,
    },
    /// An operator (or auto-policy) answered a pending Hermes approval.
    /// `resolved` is the count of pending requests Hermes reports as
    /// resolved by this response.
    HermesApprovalResolved {
        intent_id: Uuid,
        run_id: String,
        choice: String,
        resolved: u64,
    },
    /// A Hermes run wrote a file inside its sandbox workspace. Recorded
    /// because workspace writes are the structural side-effect of a
    /// coding run — message and reasoning deltas are intentionally
    /// excluded from the chain (too high-volume; see the comment at
    /// `covenant-runtime/src/hermes.rs::map_hermes_event`). `path` is
    /// the sandbox-relative path; `bytes` is the file size and is
    /// `u64` so multi-GB writes can never silently truncate.
    HermesFileWritten {
        intent_id: Uuid,
        run_id: String,
        path: String,
        bytes: u64,
    },
    CapabilityCheck {
        agent_id: String,
        required_actions: Vec<String>,
        missing_actions: Vec<String>,
        passed: bool,
    },
    CapabilityGranted {
        subject_display: String,
        action: String,
        granted_by_display: String,
        signature_b58: String,
    },
    CapabilityGrantRejected {
        subject_display: String,
        action: String,
        reason: String,
    },
    CapabilityScopeRejected {
        agent_id: String,
        action: String,
        reason: String,
    },
    IntentIgnored {
        intent_id: Uuid,
        intent_text: String,
        matched_pattern: String,
    },
    /// Logged when `PostA2AResult` is rejected upstream of any
    /// capability check — e.g. the supplied `task_id` was never
    /// dispatched through this daemon. Stronger compromise indicator
    /// than a missing-cap rejection: no honest agent generates a
    /// nonexistent `task_id`.
    A2AResultRejected { task_id: Uuid, reason: String },
    /// Logged on every rejected authentication attempt — bad first
    /// frame on the IPC socket, missing/malformed `Authorization`
    /// header on HTTP, or a token the registry doesn't resolve.
    /// `transport` is `"ipc"` or `"http"`; `reason` is the same
    /// short message the caller saw.
    AuthenticationFailed { transport: String, reason: String },
    /// Logged when `SendA2ATask` is rejected because the supplied
    /// `task.sender` does not match the authenticated peer on the
    /// connection. Closes the sender-spoof attack class where a
    /// malicious local process claims to be a different agent on the
    /// wire than the one bound to its authenticated peer token.
    A2ASenderMismatch {
        peer_display: String,
        claimed_sender_display: String,
    },
    /// Logged when `SendA2ATask` is rejected because the recipient
    /// peer has not granted `a2a.recv.<sender>` to themselves. Closes
    /// the recipient inbox spam vector that becomes exploitable when a
    /// peer with a granted send-cap pushes tasks at a recipient that
    /// has not granted matching recv-caps: without this gate a
    /// malicious peer could route arbitrary `intent_text` into the
    /// recipient's `RecentA2ATasks` view via the bidirectional filter.
    /// Distinct from [`AuditKind::CapabilityCheck`] because the missing
    /// cap belongs to a *different subject* than the issuer of the
    /// audit row — keeping it as a `CapabilityCheck` would lie about
    /// which peer's caps were short.
    A2ARecipientRejected {
        sender_display: String,
        recipient_display: String,
        action: String,
    },
    /// Logged when an operator repairs an in-flight A2A lease. `action`
    /// is `requeue`, `force_error`, or `auto_requeue`; `duplicate_risk`
    /// is present only for requeue paths. Full task payloads stay in the
    /// mailbox log; the audit row records who acted, why, and which lease
    /// they intended to mutate.
    A2ARepairApplied {
        task_id: Uuid,
        action: String,
        reason: String,
        lease_id: Option<Uuid>,
        duplicate_risk: Option<String>,
        attempt: u32,
    },
    /// Logged by the disabled-by-default daemon scheduler after each
    /// automatic A2A retry scan. Requeued tasks still get individual
    /// [`AuditKind::A2ARepairApplied`] rows; this summary makes skipped
    /// and rejected scheduler runs visible without duplicating task
    /// payloads into the audit log.
    A2AAutoRetrySchedulerScan {
        enabled: bool,
        considered: u64,
        requeued: u64,
        skipped: u64,
        skipped_by_reason: BTreeMap<String, u64>,
        min_lease_age_ms: u64,
        max_attempts: u32,
        max_requeues: u64,
        scan_limit: u64,
        error: Option<String>,
    },
    /// Logged when an operator completes a memory repair request. The
    /// full before/after record shape is returned to the caller through
    /// the repair response; the audit row keeps the durable who/what/why
    /// envelope without duplicating memory text into the audit log.
    MemoryRepairApplied {
        memory_id: Uuid,
        action: String,
        mode: String,
        changed: bool,
        reason: String,
    },
    /// Logged when an operator runs bounded memory compaction. The row
    /// records ids only; memory text and before/after payloads stay out of
    /// the audit stream.
    MemoryCompactionApplied {
        mode: String,
        changed: bool,
        reason: String,
        deleted: Vec<Uuid>,
        stale_marked: Vec<Uuid>,
        parents_detached: Vec<Uuid>,
    },
    /// Logged when `RevokeCapability` is rejected because the
    /// authenticated peer is not the subject of the capability they
    /// asked to revoke. Enforces the subject-ownership invariant on
    /// the revoking peer's pubkey, closing the cross-peer-revoke gap
    /// where any authenticated peer could otherwise tombstone another
    /// peer's capability grants.
    CapabilityRevokeRejected {
        signature_b58: String,
        reason: String,
    },
    /// Logged when `dispatch_intent` rejects an intent because the
    /// matched agent's budget bucket is exhausted. `agent_display` is
    /// the synthesized `AgentId.display` for the matched agent (e.g.
    /// `research@agent`); `requested` is the credit cost the daemon
    /// tried to debit; `tokens_remaining` is what the bucket actually
    /// had at the moment of the check (precise `u64`; the wire response
    /// rounds to a coarse bucket so token-bucket state never leaks at
    /// per-credit resolution to unauthenticated callers);
    /// `refill_eta_ms` is the wall time until the bucket can satisfy
    /// `requested` again; `intent_text` carries the rejected intent so
    /// `covenant intents resume <intent-id>` can re-dispatch from this
    /// row alone — the audit log is the resume queue.
    BudgetExhausted {
        agent_display: String,
        intent_id: Uuid,
        intent_text: String,
        requested: u64,
        tokens_remaining: u64,
        refill_eta_ms: u64,
    },
    /// Logged when the budget-hard-preempt path successfully terminated
    /// an over-budget subprocess. Distinct from [`AuditKind::BudgetExhausted`] (a
    /// post-completion rejection) because preempt actively kills a
    /// still-running process. `signal_sent` is the name of the signal
    /// the daemon dispatched (`"SIGTERM"`, `"SIGKILL"`, or `"none"`
    /// when the subprocess exited naturally inside the grace window
    /// before any signal was needed). `exit_code` is None when the
    /// process did not return a code (signal-terminated) and the
    /// daemon observed termination via `child.wait()` only. The five
    /// fields are load-bearing for the operator's post-mortem and for
    /// any future tooling that classifies cooperative vs. forced
    /// terminations.
    BudgetPreempted {
        agent_display: String,
        intent_id: Uuid,
        reason: String,
        signal_sent: String,
        exit_code: Option<i32>,
    },
    /// Logged when the budget-hard-preempt path attempted to signal
    /// the subprocess but the syscall returned an error. `errno`
    /// distinguishes ESRCH (benign — subprocess exited first; the
    /// daemon may instead emit `BudgetPreempted` with `signal_sent =
    /// "none"`) from EPERM (security incident — daemon lacks
    /// signal-send permission for that pid). The four fields are
    /// load-bearing for incident triage.
    BudgetPreemptFailed {
        agent_display: String,
        intent_id: Uuid,
        reason: String,
        errno: i32,
    },
    /// Logged when `dispatch_intent` falls into the NoCapacity fail-open
    /// arm: the manifest opted in to budget enforcement
    /// (`budget_credits_per_hour > 0`) but no bucket was seeded for the
    /// agent — the operator forgot to call `register_agent_budgets`, or
    /// a hot-reload added the manifest without re-seeding. v0 logs and
    /// passes. Distinct from [`AuditKind::BudgetExhausted`] so /audit
    /// consumers can filter operator-misconfig vs. policy-rejection
    /// without special-casing sentinel values.
    BudgetUnseeded {
        agent_display: String,
        intent_id: Uuid,
        requested: u64,
    },
    /// Logged when the operator rotates their bootstrap token via
    /// `RotateOperatorToken`. Token bytes never enter the audit log;
    /// only 6-char base58 prefixes are recorded so an operator can
    /// correlate a rotation row with the on-disk file's first chars
    /// (which is also what `PeerToken::Debug` redacts to). The new
    /// token's prefix lets the operator verify, after a rotation
    /// they did or did not initiate, that the file on disk came
    /// from this row.
    OperatorTokenRotated {
        peer_display: String,
        old_token_prefix: String,
        new_token_prefix: String,
    },
    /// Logged when `RotateOperatorToken` is rejected because the
    /// authenticated peer's pubkey does not match the operator
    /// identity. The gate is silent in v0 single-peer (only the
    /// operator can authenticate, so the rejection branch is dead
    /// code); becomes load-bearing at Phase-1 multi-peer where a
    /// guest peer reaching this path is a probe worth surfacing in
    /// `/audit`.
    ///
    /// Issuer is the daemon identity (not the rejected peer) so the
    /// row passes the cross-peer audit-feed isolation filter and the
    /// operator can see probes on their own `/audit` — mirrors the
    /// [`AuditKind::AuthenticationFailed`] audience model. The
    /// rejected peer's identity lives entirely in the kind payload.
    ///
    /// `peer_pubkey_b58` carries the unforgeable identity — the
    /// `.display` is wire-supplied and a future attacker could
    /// register `user@local` against any pubkey. The base58 form
    /// matches `bs58::encode(peer.pubkey)` and survives operator
    /// grep through the audit log unmodified.
    ///
    /// Distinct from [`AuditKind::CapabilityCheck`] because no
    /// capability is checked (the gate is identity-pubkey equality)
    /// and from [`AuditKind::AuthenticationFailed`] because the
    /// peer authenticated successfully — they failed an
    /// authorization check, not authentication.
    OperatorTokenRotationRejected {
        peer_display: String,
        peer_pubkey_b58: String,
    },
    /// Logged when `ListPeers` is rejected because the authenticated
    /// peer is not the operator (`peer.pubkey != self.identity.pubkey`).
    /// Mirrors [`AuditKind::OperatorTokenRotationRejected`]'s daemon-as-issuer
    /// audience model so the row passes the cross-peer audit-feed
    /// isolation filter and the rejected peer's `/audit` does not
    /// double as a probe-was-logged oracle.
    ///
    /// Distinct from [`AuditKind::CapabilityCheck`] because no
    /// capability is checked (the gate is identity-pubkey equality)
    /// and from [`AuditKind::AuthenticationFailed`] because the peer
    /// authenticated successfully — they failed an authorization check.
    OperatorPeersListRejected {
        peer_display: String,
        peer_pubkey_b58: String,
    },
    /// Logged when the operator successfully revokes a peer registry
    /// entry via `RevokePeer`. Issuer is the operator (peer-event
    /// audience: `record_peer_event` panics in debug if the issuer's
    /// pubkey does not match the acting peer's pubkey) — the operator
    /// took the action. `peer_display` and `peer_pubkey_b58` describe
    /// the *revoked* peer (not the issuer). `token_prefix` is the
    /// same 6-char redaction `OperatorTokenRotated` records — full
    /// token bytes never enter the audit log.
    PeerRevoked {
        peer_display: String,
        peer_pubkey_b58: String,
        token_prefix: String,
    },
    /// Logged when `RevokePeer` is rejected because the authenticated
    /// peer is not the operator. Daemon-as-issuer audience model
    /// matching [`AuditKind::OperatorTokenRotationRejected`] and
    /// [`AuditKind::OperatorPeersListRejected`] — recording the
    /// rejection under the rejected peer would (a) hide the probe
    /// from the operator's `/audit` feed under the cross-peer
    /// audit-feed isolation filter and (b) turn the rejected peer's
    /// own feed into a probe-was-logged oracle. `peer_pubkey_b58` is
    /// the unforgeable identifier; the `display` is wire-supplied.
    OperatorPeerRevokeRejected {
        peer_display: String,
        peer_pubkey_b58: String,
    },
    /// Logged when the operator's `RevokePeer` request would have
    /// revoked their own bootstrap token but `force` was `false`. The
    /// daemon returns `RevokeOutcome::SelfRevokeForbidden` and the
    /// registry is unchanged. Issuer is the operator (peer-event
    /// audience: `record_peer_event` panics in debug if the issuer's
    /// pubkey does not match the acting peer's pubkey) — distinct
    /// from [`AuditKind::OperatorPeerRevokeRejected`] which records a
    /// non-operator's *probe* under the daemon-issuer audience. Here
    /// the operator IS the issuer and the audience; the row surfaces
    /// in their own `/audit` feed for triage of self-fat-fingers.
    /// `token_prefix` is the same 6-char redaction
    /// [`AuditKind::PeerRevoked`] records.
    PeerSelfRevokeBlocked {
        peer_display: String,
        peer_pubkey_b58: String,
        token_prefix: String,
    },
    /// An agent paid an external x402 endpoint. Recorded after the
    /// 402-then-pay loop returns success, alongside the paired budget
    /// debit and settlement receipt. `amount` is the atomic on-chain
    /// amount the provider charged (authoritative, from the live 402
    /// challenge); `network` and `asset` identify the settlement rail;
    /// `receipt_id` joins this row to the settlement receipt and the
    /// budget debit. `endpoint` is the called URL for operator triage.
    ExternalPaymentSettled {
        provider: String,
        endpoint: String,
        network: String,
        asset: String,
        amount: String,
        receipt_id: Uuid,
    },
    /// Logged when the operator runs the settlement receipt backfill. A
    /// dry run records `row_count` from the plan with `dry_run = true`
    /// and no `rollback_path`; an apply records the rewritten
    /// `row_count` and the `rollback_path` checkpoint the mutator wrote
    /// before the atomic rewrite (absent on a no-op apply that changed
    /// nothing). The daemon emits this only after `backfill_receipts`
    /// returns, i.e. after the rollback checkpoint, the rewritten store
    /// contents, and the renamed store file are fsynced, so the audit
    /// log never claims a mutation whose data did not durably land.
    ///
    /// Issuer is the acting peer (the operator), matching the
    /// [`AuditKind::MemoryRepairApplied`] audience model: the row
    /// surfaces on the operator's `/audit/recent` feed under the
    /// issuer-equals-peer filter rather than being mis-attributed to the
    /// daemon identity, which would hide a guest operator's backfill from
    /// their own feed at multi-peer. Best-effort like every other
    /// completed-mutation kind: the rewrite is already durable and the
    /// rollback file is on disk, so audit-write success is not a
    /// precondition for the response (unlike the suppressible rejection
    /// probes in `audit_kind_requires_persistence`).
    SettlementReceiptBackfillApplied {
        row_count: u64,
        rollback_path: Option<String>,
        dry_run: bool,
    },
    /// Logged when the operator runs the memory-record receipt-correlation
    /// backfill. A dry run records the planner-derived `row_count` with
    /// `dry_run = true` and no `savepoint_name`; an apply records the
    /// committed `row_count` and the `savepoint_name` the mutator wrapped
    /// the batch in (absent on a no-op apply that changed nothing, so the
    /// audit row never claims a SAVEPOINT was reserved for an empty
    /// batch). The daemon emits this only after
    /// [`SqliteStore::backfill_receipt_correlation`] returns Ok, i.e.
    /// after BEGIN IMMEDIATE + SAVEPOINT + per-row UPDATE + RELEASE
    /// SAVEPOINT + COMMIT all succeed, so the audit log never claims a
    /// mutation whose data did not durably land.
    ///
    /// Issuer is the acting peer (the operator), matching the
    /// [`AuditKind::MemoryRepairApplied`] and
    /// [`AuditKind::SettlementReceiptBackfillApplied`] audience model:
    /// the row surfaces on the operator's `/audit/recent` feed under the
    /// issuer-equals-peer filter rather than being mis-attributed to the
    /// daemon identity, which would hide a guest operator's backfill
    /// from their own feed at multi-peer. Best-effort like every other
    /// completed-mutation kind: the SAVEPOINT-wrapped batch already
    /// COMMITted, so audit-write success is not a precondition for the
    /// response.
    MemoryRecordBackfillApplied {
        row_count: u64,
        savepoint_name: Option<String>,
        dry_run: bool,
    },
    /// Logged when an operator installs a Solana Agent Skill into the
    /// daemon. Pins the install-time content `digest_hex` (SHA-256 over
    /// the normalized `SKILL.md` + `references/**` bytes) and the
    /// `source_url`/`source_tag`/`source_commit` origin coordinates so
    /// a post-approval URL or content swap at the upstream repo is
    /// detected at load-time (re-compute the digest, mismatch refuses
    /// the run) and surfaced in the operator's `/audit/recent`. The
    /// audit chain becomes the durable provenance record that
    /// instruction set the agent acted under for every later
    /// [`AuditKind::SkillContextInjected`] / [`AuditKind::SkillInvoked`]
    /// row tagged with the same `name`.
    SkillInstalled {
        name: String,
        version: String,
        digest_hex: String,
        source_url: String,
        source_tag: String,
        source_commit: String,
    },
    /// Logged when the runtime injects a skill's `SKILL.md` body (and
    /// any progressively-disclosed `references/**`) into an agent's
    /// system context. `skill_digest_hex` re-anchors the row to the
    /// install-time [`AuditKind::SkillInstalled`] so a verifier can
    /// recompute *which* instructions the agent ran under for this
    /// intent; `references` is the load-on-demand list actually
    /// injected (not the full declared set). Verifier and agent
    /// contexts stay strictly separate — only the agent context is
    /// recorded here.
    SkillContextInjected {
        skill_name: String,
        skill_digest_hex: String,
        references: Vec<String>,
    },
    /// Logged when an agent run invokes a previously-injected skill.
    /// Joins to the originating [`AuditKind::IntentDispatched`] row via
    /// `intent_id` and to the install record via `skill_name`, so the
    /// audit chain alone proves the agent ran under the same content
    /// that was capability-gated at dispatch.
    SkillInvoked { skill_name: String, intent_id: Uuid },
    /// Logged when the daemon's tx broker builds a Solana transaction
    /// *proposal* for a skill run, before any signing. `program` is the
    /// target program-id (base58), `instruction` is the Anchor/IDL
    /// instruction name (`"transfer"`, `"initialize"`, …), and
    /// `accounts_hash_hex` is the SHA-256 over the serialized account
    /// metas — raw account lists never enter the chain. `simulated_ok`
    /// records the devnet simulate result the broker observed; an
    /// out-of-envelope proposal is rejected here, never signed.
    SkillTxProposed {
        skill_name: String,
        program: String,
        instruction: String,
        accounts_hash_hex: String,
        simulated_ok: bool,
    },
    /// Logged when the daemon signs a skill-proposed transaction. W009
    /// "never sign without approval" is enforced upstream of this row:
    /// the broker rejects out-of-envelope proposals at
    /// [`AuditKind::SkillTxProposed`]/[`AuditKind::SkillRefused`] and
    /// only in-envelope proposals reach the signer. `signature_b58` is
    /// the ed25519 signature bytes encoded as 87..=88 base58 chars,
    /// matching the [`AuditKind::CapabilityGranted::signature_b58`]
    /// convention so external verifiers parse skill-tx and capability
    /// signatures with one decoder.
    SkillTxSigned {
        skill_name: String,
        signature_b58: String,
    },
    /// Logged when the daemon refuses to use, inject, or sign for a
    /// skill — ungranted `skill.use.{name}`, missing `chain.tx.*` cap
    /// on a proposed instruction, simulate failure, or an out-of-envelope
    /// approval-policy result. Distinct from
    /// [`AuditKind::CapabilityScopeRejected`] so operator triage can
    /// filter skill-driven refusals as a single column and so
    /// non-capability refusals (simulate failure, approval-policy deny)
    /// still surface under a skill-specific kind.
    SkillRefused { skill_name: String, reason: String },
    /// Logged when the daemon observes data sourced from outside the
    /// agent's trust boundary during a skill run — on-chain account
    /// reads, HTTP responses, fetched MCP tool output — and tags it for
    /// W011 "on-chain data is untrusted" enforcement. `source` is the
    /// short origin string (e.g. `"rpc:account_data:<pubkey>"`,
    /// `"http:GET:<host>"`) and `digest_hex` is the SHA-256 of the
    /// observed bytes; raw payloads never enter the chain. The
    /// Verifier-Refuter consumes these rows: a skill run whose signed
    /// actions causally followed an `UntrustedInputObserved` payload
    /// containing prompt-like text is refuted.
    UntrustedInputObserved { source: String, digest_hex: String },
}

#[async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError>;
    async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError>;
    /// Drop every event with `timestamp_ms < before_ms`. Returns the
    /// count deleted. Operator-driven retention: with no purge call the
    /// log grows unbounded for the lifetime of the daemon. Mirrors the
    /// `MemoryStore::purge_older_than` shape.
    async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError>;
    /// Verify the audit log's local tamper-evidence chain.
    async fn verify_integrity(&self) -> Result<AuditIntegrityReport, AuditError>;
}

pub struct JsonlAuditLog {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

const ZERO_CHAIN_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write to string");
    }
    out
}

fn chain_hash(previous_hash_hex: &str, event_hash_hex: &str) -> String {
    let material = format!("{previous_hash_hex}\n{event_hash_hex}");
    sha256_hex(material.as_bytes())
}

fn chain_entry_for_line(
    index: usize,
    event: &AuditEvent,
    line: &str,
    previous_hash_hex: &str,
) -> AuditChainEntry {
    let event_hash_hex = sha256_hex(line.as_bytes());
    AuditChainEntry {
        index: index as u64,
        event_id: event.id,
        timestamp_ms: event.timestamp_ms,
        previous_hash_hex: previous_hash_hex.into(),
        chain_hash_hex: chain_hash(previous_hash_hex, &event_hash_hex),
        event_hash_hex,
    }
}

fn build_chain_entries(events: &[AuditEvent]) -> Result<Vec<AuditChainEntry>, AuditError> {
    let mut previous = ZERO_CHAIN_HASH.to_string();
    let mut entries = Vec::with_capacity(events.len());
    for (index, event) in events.iter().enumerate() {
        let line = serde_json::to_string(event)?;
        let entry = chain_entry_for_line(index, event, &line, &previous);
        previous = entry.chain_hash_hex.clone();
        entries.push(entry);
    }
    Ok(entries)
}

async fn read_events(path: &PathBuf) -> Result<Vec<AuditEvent>, AuditError> {
    match fs::read_to_string(path).await {
        Ok(s) => s
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

async fn read_event_lines(path: &PathBuf) -> Result<Vec<String>, AuditError> {
    match fs::read_to_string(path).await {
        Ok(s) => Ok(s
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

async fn read_chain_entries(path: &PathBuf) -> Result<Vec<AuditChainEntry>, AuditError> {
    match fs::read_to_string(path).await {
        Ok(s) => s
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Crash-atomic chain rewrite via tmp + rename: write the full body to a
/// sibling `.tmp` path, flush the user-space buffer, then atomically rename it
/// over the chain. Without this, a power loss mid-write would leave the chain
/// file truncated at an arbitrary offset, which `read_chain_entries` would
/// surface as a serde error. With it, observers see either the old body or
/// the new one — never half a row.
///
/// Best-effort under hostile power loss: this does NOT `sync_all()` the tmp
/// file or fsync the parent directory, matching the pre-existing events
/// rewrite path. A crash AFTER the rename returns but BEFORE the filesystem
/// flushes the rename + new inode can still revert to the old body on next
/// boot. Strengthening this requires a paired upgrade of the events path —
/// tracked separately to keep the two halves of the audit log on the same
/// durability tier.
async fn write_chain_entries(
    path: &PathBuf,
    entries: &[AuditChainEntry],
) -> Result<(), AuditError> {
    let tmp_path = path.with_extension("jsonl.tmp");
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)
        .await?;
    for entry in entries {
        let line = serde_json::to_string(entry)?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
    }
    f.flush().await?;
    drop(f);
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

impl JsonlAuditLog {
    pub async fn open(path: PathBuf) -> Result<Self, AuditError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            lock: Arc::new(Mutex::new(())),
        })
    }

    fn chain_path(&self) -> PathBuf {
        self.path.with_extension("chain.jsonl")
    }
}

#[async_trait]
impl AuditLog for JsonlAuditLog {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        let _g = self.lock.lock().await;
        let existing_events = read_events(&self.path).await?;
        let chain_path = self.chain_path();
        let existing_chain = read_chain_entries(&chain_path).await?;
        // If the chain length doesn't match the events length, the chain file
        // has been truncated, deleted, or rewritten out-of-band. The previous
        // behaviour silently rebuilt over whatever the events file held,
        // which is precisely what an attacker who tampered with both files
        // wants: rebuild produces a chain that matches the tampered events,
        // and verify_integrity passes afterwards. Refuse instead — the
        // operator must run an external recovery to acknowledge the gap.
        if existing_chain.len() != existing_events.len() {
            return Err(AuditError::ChainCorruption {
                events: existing_events.len(),
                chain: existing_chain.len(),
            });
        }
        let line = serde_json::to_string(&event)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        drop(f);

        let previous_hash = existing_chain
            .last()
            .map(|entry| entry.chain_hash_hex.as_str())
            .unwrap_or(ZERO_CHAIN_HASH);
        let entry = chain_entry_for_line(existing_chain.len(), &event, &line, previous_hash);
        let mut chain_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&chain_path)
            .await?;
        let chain_line = serde_json::to_string(&entry)?;
        chain_file.write_all(chain_line.as_bytes()).await?;
        chain_file.write_all(b"\n").await?;
        chain_file.flush().await?;
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        let _g = self.lock.lock().await;
        let f = match fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(f);
        let mut all = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            all.push(serde_json::from_str(trimmed)?);
        }
        let start = all.len().saturating_sub(limit);
        Ok(all.split_off(start))
    }

    async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
        // Read-filter-rewrite under the same lock that record uses, so a
        // concurrent record can't race against the rewrite. Each file gets
        // its own tmp + rename so a power-loss mid-write never leaves a
        // half-written body on disk.
        //
        // Crash-atomicity ordering: write+rename CHAIN first, then
        // write+rename EVENTS. A crash between the two renames leaves
        // chain=K rows and events=N rows with N > K — that mismatch fails
        // `record()`'s length check (the security-correct refusal — see
        // record's comment on why rebuild is not safe), but it is the
        // recoverable shape: re-running purge_older_than with the same
        // cutoff re-derives the same K kept events, rewrites the chain to
        // the same K rows (no-op-ish), and renames events to match. The
        // reverse order — renaming events first — could leave events=K and
        // chain=N (M > K), which means the chain claims an event that the
        // events file no longer holds. That state is impossible to
        // distinguish from chain tampering (an attacker would prefer
        // exactly this shape so a future rebuild would accept their forged
        // events), so the audit log must stay refused until an operator
        // restores from a trusted backup. The chain-first ordering keeps
        // the recoverable shape on every crash window.
        let _g = self.lock.lock().await;
        let existing = read_events(&self.path).await?;
        if existing.is_empty() {
            return Ok(0);
        }
        let kept: Vec<AuditEvent> = existing
            .iter()
            .filter(|e| e.timestamp_ms >= before_ms)
            .cloned()
            .collect();
        let purged = (existing.len() - kept.len()) as u64;
        if purged == 0 {
            return Ok(0);
        }
        let chain_entries = build_chain_entries(&kept)?;
        write_chain_entries(&self.chain_path(), &chain_entries).await?;
        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for e in &kept {
            let line = serde_json::to_string(e)?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
        }
        f.flush().await?;
        drop(f);
        fs::rename(&tmp_path, &self.path).await?;
        Ok(purged)
    }

    async fn verify_integrity(&self) -> Result<AuditIntegrityReport, AuditError> {
        let _g = self.lock.lock().await;
        let event_lines = read_event_lines(&self.path).await?;
        let anchors = read_chain_entries(&self.chain_path()).await?;
        let mut failures = Vec::new();
        if anchors.len() != event_lines.len() {
            failures.push(format!(
                "chain length mismatch: {} event(s), {} anchor(s)",
                event_lines.len(),
                anchors.len()
            ));
        }
        let mut previous_hash_hex = ZERO_CHAIN_HASH.to_string();
        for (index, line) in event_lines.iter().enumerate() {
            let event_hash_hex = sha256_hex(line.as_bytes());
            let chain_hash_hex = chain_hash(&previous_hash_hex, &event_hash_hex);
            match serde_json::from_str::<AuditEvent>(line) {
                Ok(event) => {
                    let expected = AuditChainEntry {
                        index: index as u64,
                        event_id: event.id,
                        timestamp_ms: event.timestamp_ms,
                        event_hash_hex,
                        previous_hash_hex: previous_hash_hex.clone(),
                        chain_hash_hex: chain_hash_hex.clone(),
                    };
                    match anchors.get(index) {
                        Some(actual) if actual == &expected => {}
                        Some(_) => failures.push(format!("chain entry {index} mismatch")),
                        None => failures.push(format!("chain entry {index} missing")),
                    }
                }
                Err(e) => {
                    failures.push(format!("event line {index} parse error: {e}"));
                    match anchors.get(index) {
                        Some(actual)
                            if actual.index == index as u64
                                && actual.event_hash_hex == event_hash_hex
                                && actual.previous_hash_hex == previous_hash_hex
                                && actual.chain_hash_hex == chain_hash_hex => {}
                        Some(_) => failures.push(format!("chain entry {index} mismatch")),
                        None => failures.push(format!("chain entry {index} missing")),
                    }
                }
            }
            previous_hash_hex = chain_hash_hex;
        }
        if anchors.len() > event_lines.len() {
            failures.push(format!(
                "{} dangling chain anchor(s)",
                anchors.len() - event_lines.len()
            ));
        }
        Ok(AuditIntegrityReport {
            events: event_lines.len() as u64,
            anchors: anchors.len() as u64,
            valid: failures.is_empty(),
            root_hash_hex: previous_hash_hex,
            failures,
        })
    }
}

#[derive(Default)]
pub struct InMemoryAuditLog {
    events: Mutex<Vec<AuditEvent>>,
}

impl InMemoryAuditLog {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuditLog for InMemoryAuditLog {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        self.events.lock().await.push(event);
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        let g = self.events.lock().await;
        let start = g.len().saturating_sub(limit);
        Ok(g[start..].to_vec())
    }

    async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
        let mut g = self.events.lock().await;
        let len_before = g.len();
        g.retain(|e| e.timestamp_ms >= before_ms);
        Ok((len_before - g.len()) as u64)
    }

    async fn verify_integrity(&self) -> Result<AuditIntegrityReport, AuditError> {
        let g = self.events.lock().await;
        let entries = build_chain_entries(&g)?;
        Ok(AuditIntegrityReport {
            events: g.len() as u64,
            anchors: g.len() as u64,
            valid: true,
            root_hash_hex: entries
                .last()
                .map(|entry| entry.chain_hash_hex.clone())
                .unwrap_or_else(|| ZERO_CHAIN_HASH.into()),
            failures: Vec::new(),
        })
    }
}

/// Lowercase 64-char SHA-256 hex of `bytes`.
///
/// Used as the redaction barrier for AuditKind::IntentDispatched.result_hash_hex
/// (a stable fingerprint of an intent's textual result) and
/// HermesToolInvoked.preview_hash_hex (a digest of a tool input that must not
/// land in the audit chain in cleartext). The same primitive backs the chain
/// hash (see sha256_hex / chain_hash), so a covenantd audit chain has one
/// underlying digest function and one external-verification story.
///
/// Guarantees: collision resistance (2^128 work), preimage resistance for
/// high-entropy inputs, and a deterministic 64-character lowercase hex output
/// across Rust versions, platforms, and process restarts.
///
/// Does NOT guarantee: irreversibility for low-entropy inputs. A preview that
/// is one of a small set of guessable values (a known file path, a short
/// command name, a yes/no flag) is recoverable by hashing the candidate set.
/// preview_hash_hex blocks accidental cleartext leakage and pins integrity; it
/// is not a confidentiality primitive against an adversary who can guess the
/// input distribution. A keyed-MAC layer is the correct fix when that threat
/// applies, and is tracked separately from this primitive swap.
pub fn hash_hex(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(kind: AuditKind) -> AuditEvent {
        AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: 0,
            issuer: AgentId::new("user@local", [0u8; 32]),
            kind,
        }
    }

    fn intent_kind(status: &str) -> AuditKind {
        AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "find x".into(),
            matched_agent: Some("research".into()),
            result_hash_hex: hash_hex(b"some result"),
            status: status.into(),
        }
    }

    #[tokio::test]
    async fn in_memory_record_and_recent() {
        let log = InMemoryAuditLog::new();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("error"))).await.unwrap();
        let last_two = log.recent(2).await.unwrap();
        assert_eq!(last_two.len(), 2);
        match &last_two[1].kind {
            AuditKind::IntentDispatched { status, .. } => assert_eq!(status, "error"),
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn in_memory_recent_pins_saturating_sub_zero_limit_and_full_tail_boundary_arms() {
        // covenant_audit::InMemoryAuditLog::recent contract:
        //
        //   async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        //       let g = self.events.lock().await;
        //       let start = g.len().saturating_sub(limit);
        //       Ok(g[start..].to_vec())
        //   }
        //
        // Four boundary arms:
        //   (1) limit == 0       -> start == g.len() -> g[g.len()..] -> empty
        //   (2) limit == g.len() -> start == 0       -> g[0..]       -> all events
        //   (3) limit  > g.len() -> saturating_sub clamps to 0       -> all events,
        //                          NOT panic on integer underflow
        //   (4) limit  < g.len() -> tail-view of the last `limit` events,
        //                          NOT head-view of the first `limit`
        //
        // in_memory_record_and_recent only exercises arm 4
        // with three events and limit=2, and never reads last_two[0]
        // — only last_two[1] is asserted ('error'), so a refactor that
        // swapped the slice for the FIRST `limit` events would let the
        // existing test fail only on the [1] index; pinning BOTH ends of
        // the tail (last_two[0]=='b' AND last_two[1]=='c' below)
        // forecloses any single-index rewrite.
        // jsonl_recent_on_missing_file_is_empty exercises
        // limit=10 against a missing JsonlAuditLog file, which never
        // enters the populated-state saturating_sub branch on InMemory.
        //
        // A refactor that swapped .saturating_sub(limit) for plain
        // 'g.len() - limit' under a 'remove defensive arithmetic'
        // rationale would silently panic on arm 3 — exactly the request
        // shape an operator dashboard fires on a freshly-started daemon
        // with five events and the default 'recent 100' parameter.

        let log = InMemoryAuditLog::new();
        log.record(dummy(intent_kind("a"))).await.unwrap();
        log.record(dummy(intent_kind("b"))).await.unwrap();
        log.record(dummy(intent_kind("c"))).await.unwrap();

        fn status_of(event: &AuditEvent) -> &str {
            match &event.kind {
                AuditKind::IntentDispatched { status, .. } => status.as_str(),
                other => panic!("expected IntentDispatched; got {other:?}"),
            }
        }

        // Arm 1: limit == 0. saturating_sub gives start == g.len() so
        // the slice is empty. Pinning the empty Vec contract here
        // forecloses a refactor that promoted limit=0 to a Default
        // ('treat 0 as unlimited' or 'treat 0 as 1') — both common
        // ergonomic shortcuts that would silently widen the response.
        let zero = log.recent(0).await.unwrap();
        assert!(
            zero.is_empty(),
            "recent(0) must return an empty Vec — a refactor that \
             promoted limit=0 to 'unlimited' or 'treat as 1' under an \
             ergonomic-default rationale would silently widen every \
             operator query whose UI default sends 0 for an unset \
             field; the saturating_sub arithmetic guarantees this \
             behavior today (start == g.len() so g[g.len()..] is the \
             empty slice). got len={}",
            zero.len(),
        );

        // Arm 2: limit == g.len(). saturating_sub gives start == 0 so
        // the slice is the full event log. Pin every index so a
        // refactor that reversed the order would surface here.
        let exact = log.recent(3).await.unwrap();
        assert_eq!(exact.len(), 3, "limit == len must return all events");
        assert_eq!(
            status_of(&exact[0]),
            "a",
            "recent(len) must preserve insertion order at index 0 — the \
             first inserted event ('a') is at the head of the result; \
             a refactor that reversed the slice would surface here \
             without depending on saturating_sub",
        );
        assert_eq!(status_of(&exact[1]), "b");
        assert_eq!(
            status_of(&exact[2]),
            "c",
            "recent(len) must preserve insertion order at index len-1 \
             — the last inserted event ('c') is at the tail; pinning \
             both ends forecloses an off-by-one or reverse-order \
             refactor that the existing in_memory_record_and_recent \
             single-index assertion would let through",
        );

        // Arm 3: limit > g.len(). saturating_sub clamps the would-be
        // underflow (3 - 10 = -7 as i64; saturating as usize gives 0)
        // so the slice is the full event log without panicking. This
        // is the defensive contract that a non-saturating refactor
        // would silently lose.
        let oversized = log.recent(10).await.unwrap();
        assert_eq!(
            oversized.len(),
            3,
            "recent(limit) with limit > len must return all events \
             without panicking — a refactor that swapped \
             .saturating_sub(limit) for 'g.len() - limit' under a \
             'clippy says use checked_sub' rationale would silently \
             panic on this exact input shape (3 events, default UI \
             'recent 100'); the underflow surfaces only at runtime on \
             specific operator queries, with no compile-time signal. \
             got len={}",
            oversized.len(),
        );
        assert_eq!(
            status_of(&oversized[0]),
            "a",
            "oversized-limit must NOT change the surface order — same \
             insertion-order contract as the limit==len arm",
        );
        assert_eq!(status_of(&oversized[2]), "c");

        // Arm 4: limit < g.len(). The existing in_memory_record_and_recent
        // pins last_two[1] only; pin both ends here so a refactor that
        // returned the FIRST `limit` events instead of the LAST would
        // surface on the [0] index even if a co-author updated the
        // existing test's [1] assertion.
        let last_two = log.recent(2).await.unwrap();
        assert_eq!(last_two.len(), 2);
        assert_eq!(
            status_of(&last_two[0]),
            "b",
            "recent(limit) with limit < len must be tail-view: the \
             SECOND-to-last inserted event ('b') is at index 0 of the \
             result. A refactor that swapped g[start..] for \
             g[..limit.min(g.len())] (head-view) would surface 'a' \
             here while the existing in_memory_record_and_recent's [1] \
             check on 'error' could be co-edited to 'a' under the \
             rationale 'first limit events are the recent ones'. \
             Pinning the index-0 tail value forecloses the head-vs-\
             tail flip even under a coordinated test rewrite",
        );
        assert_eq!(
            status_of(&last_two[1]),
            "c",
            "tail-view's index 1 is the LAST inserted event ('c'), \
             cross-binds the existing in_memory_record_and_recent \
             assertion on last_two[1].status without depending on the \
             specific 'error'/'ok' fixture choice",
        );
    }

    #[tokio::test]
    async fn jsonl_round_trip_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();

        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        let recent = log2.recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);

        let raw = std::fs::read_to_string(&path).unwrap();
        let lines = raw.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(lines, 2);
    }

    #[tokio::test]
    async fn jsonl_integrity_report_accepts_untampered_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("error"))).await.unwrap();

        let report = log.verify_integrity().await.unwrap();
        assert!(report.valid, "{report:?}");
        assert_eq!(report.events, 2);
        assert_eq!(report.anchors, 2);
        assert_eq!(report.root_hash_hex.len(), 64);
        let chain_raw = std::fs::read_to_string(path.with_extension("chain.jsonl")).unwrap();
        assert_eq!(chain_raw.lines().filter(|l| !l.is_empty()).count(), 2);
    }

    #[tokio::test]
    async fn jsonl_integrity_report_detects_tampered_event_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, raw.replace("find x", "find y")).unwrap();

        let report = log.verify_integrity().await.unwrap();
        assert!(!report.valid);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("mismatch")));
    }

    #[tokio::test]
    async fn jsonl_integrity_report_surfaces_malformed_event_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        std::fs::write(&path, "{bad json}\n").unwrap();

        let report = log.verify_integrity().await.unwrap();
        assert!(!report.valid);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("parse error")));
    }

    #[tokio::test]
    async fn jsonl_record_pins_chain_corruption_on_length_mismatch_with_field_values() {
        // covenant_audit::JsonlAuditLog::record guards on chain-file
        // length parity before appending a new event. The check reads:
        //
        //   if existing_chain.len() != existing_events.len() {
        //       return Err(AuditError::ChainCorruption {
        //           events: existing_events.len(),
        //           chain: existing_chain.len(),
        //       });
        //   }
        //
        // The doc-comment above the check documents the threat: the
        // previous behaviour silently rebuilt over whatever the
        // events file held, which is precisely what an attacker who
        // tampered with both files wants — rebuild produces a chain
        // that matches the tampered events, and verify_integrity
        // passes afterwards. The check refuses instead, so the
        // operator must run an external recovery to acknowledge the
        // gap.
        //
        // No test fires the arm today. A refactor that removed the
        // check under a 'silently rebuild is fine for the common
        // case' rationale would re-open the documented threat
        // surface. A refactor that flipped the equality to chain >
        // events (one-directional) would silently let attackers
        // truncate the chain without firing. A refactor that swapped
        // events and chain field assignments under an 'alphabetize
        // struct-field initializers' rationale would silently
        // mis-report the counts in operator error messages and
        // confuse incident triage.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();

        // Seed two events so events.jsonl has 2 lines and chain.jsonl
        // has 2 entries. The chain file lives next to events.jsonl
        // with the .chain.jsonl extension.
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        let chain_path = path.with_extension("chain.jsonl");

        // Externally truncate the chain file to a single entry — the
        // attacker's tampered-rewrite scenario at half-completion.
        let chain_raw = std::fs::read_to_string(&chain_path).unwrap();
        let first_line = chain_raw
            .lines()
            .find(|l| !l.trim().is_empty())
            .expect("seeded chain.jsonl must have at least one entry");
        std::fs::write(&chain_path, format!("{first_line}\n")).unwrap();

        let err = log.record(dummy(intent_kind("ok"))).await.expect_err(
            "record must refuse to append when the chain file has \
                 been externally truncated — the previous behaviour \
                 silently rebuilt over whatever events held; the \
                 chain.len() != events.len() check in record closes \
                 that threat (see its doc-comment). A refactor that \
                 removed the check under a 'silently rebuild is fine' \
                 rationale would surface here as record returning Ok",
        );

        match err {
            AuditError::ChainCorruption { events, chain } => {
                assert_eq!(
                    events, 2,
                    "ChainCorruption.events must equal the actual \
                     events.jsonl row count (2 — the two seeded \
                     records). A refactor that swapped the field \
                     assignments under a 'sort fields alphabetically' \
                     rationale would surface here as events == 1 \
                     (the truncated chain count) with no other \
                     compile-time signal that operator-facing error \
                     messages now mis-report which file was tampered",
                );
                assert_eq!(
                    chain, 1,
                    "ChainCorruption.chain must equal the truncated \
                     chain.jsonl row count (1 — externally written \
                     with a single line above). Paired with the \
                     events assertion above, a field-swap regression \
                     fails BOTH assertions and the operator-facing \
                     error message diagnostic on AuditError::ChainCorruption \
                     ('events file has {{events}} rows, chain file has \
                     {{chain}}') survives intact",
                );
            }
            other => panic!(
                "record with truncated chain.jsonl must return \
                 AuditError::ChainCorruption (the equality check in \
                 record fires on chain.len() != events.len() in both \
                 directions); a one-directional comparison (e.g., \
                 chain > events) would silently let this truncated-\
                 chain case pass and the chain-entry rebuild via \
                 build_chain_entries would silently produce a chain \
                 matching the truncated state. Got: {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn jsonl_recent_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(log.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn jsonl_recent_propagates_non_notfound_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let err = log.recent(10).await.unwrap_err();
        assert!(
            matches!(err, AuditError::Io(_)),
            "recent must propagate a non-NotFound read fault as AuditError::Io, not \
             swallow it into an empty Vec the way only a genuinely missing file may; \
             an audit view that silently empties on a read fault would hide tampering \
             (audit-integrity fail-open regression class): {err:?}"
        );
    }

    #[tokio::test]
    async fn read_helpers_propagate_non_notfound_io_error() {
        // The tempdir path is a directory, so read_to_string fails with a
        // non-NotFound (IsADirectory) fault. Each helper backs append and
        // integrity verification; all three must surface AuditError::Io rather
        // than the missing-file Ok(empty) short-circuit, or a faulted read of
        // the log or chain would masquerade as an empty history.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        assert!(
            matches!(read_events(&path).await.unwrap_err(), AuditError::Io(_)),
            "read_events must surface a non-NotFound read fault as AuditError::Io"
        );
        assert!(
            matches!(
                read_event_lines(&path).await.unwrap_err(),
                AuditError::Io(_)
            ),
            "read_event_lines must surface a non-NotFound read fault as AuditError::Io"
        );
        assert!(
            matches!(
                read_chain_entries(&path).await.unwrap_err(),
                AuditError::Io(_)
            ),
            "read_chain_entries must surface a non-NotFound read fault as AuditError::Io"
        );
    }

    #[test]
    fn hash_hex_is_stable_for_same_input() {
        assert_eq!(hash_hex(b"hello"), hash_hex(b"hello"));
        assert_ne!(hash_hex(b"hello"), hash_hex(b"world"));
    }

    #[test]
    fn audit_error_chain_corruption_display_message_pins_prefix_count_slots_and_refusing_hint() {
        // covenant_audit::AuditError::ChainCorruption is the
        // operator-facing security boundary diagnostic for a
        // chain-file/events-file length mismatch. The format
        // string is:
        //
        //   chain corruption: events file has {events} rows, chain
        //   file has {chain}; refusing to rebuild
        //
        // Three load-bearing pieces: the 'chain corruption' prefix,
        // the {events}/{chain} count slot bindings, and the 'refusing
        // to rebuild' security-policy hint. The JsonlAuditLog::record
        // doc-comment explains why the daemon refuses rather than rebuilds:
        // a rebuild would produce a chain matching tampered events,
        // which is the attacker's goal. jsonl_record_pins_chain_corruption_on_length_mismatch_with_field_values
        // pins the field VALUES via destructure-and-assert
        // but never format!('{err}'). A typo, a slot swap (mis-
        // reporting which file was tampered in incident triage), or a
        // dropped 'refusing to rebuild' hint would silently degrade
        // the diagnostic.

        // Distinct values so a {events}/{chain} slot swap surfaces.
        let err = AuditError::ChainCorruption {
            events: 5,
            chain: 3,
        };
        let message = format!("{err}");

        assert!(
            message.contains("chain corruption"),
            "ChainCorruption Display must keep the 'chain corruption' \
             prefix — distinguishes this variant from Io/Serde \
             wrappers in dashboards that group errors by message \
             prefix: {message}"
        );
        assert!(
            message.contains("events file has 5 rows"),
            "ChainCorruption must bind {{events}} to the 'events file \
             has' slot — pinning the slot ordering directly so a swap \
             that bound {{chain}} here would surface as 'events file \
             has 3 rows'; mis-reporting which file was tampered is \
             precisely the attacker-favored regression the \
             JsonlAuditLog::record doc-comment warns against (an \
             operator triaging a \
             truncated chain would investigate the wrong file): \
             {message}"
        );
        assert!(
            message.contains("chain file has 3"),
            "ChainCorruption must bind {{chain}} to the 'chain file \
             has' slot — paired assertion with 'events file has 5 \
             rows' above so a slot swap fails BOTH and the operator-\
             facing AuditError::ChainCorruption diagnostic stays \
             anchored. Note the format string omits 'rows' after the \
             {{chain}} value (anchored separately below by the \
             semicolon check): {message}"
        );
        assert!(
            message.contains("chain file has 3;"),
            "ChainCorruption must keep the semicolon between the \
             {{chain}} value and 'refusing to rebuild' — pins the \
             punctuation that separates the count-report from the \
             policy hint. A refactor that swapped the semicolon for \
             a comma or a period would silently shift dashboards \
             that split the message at ';' to extract the policy \
             suffix: {message}"
        );
        assert!(
            message.contains("refusing to rebuild"),
            "ChainCorruption must keep the 'refusing to rebuild' hint \
             — the security-policy signal that distinguishes 'we \
             won't rebuild' (intentional) from 'we couldn't rebuild' \
             (bug). The JsonlAuditLog::record doc-comment documents \
             that rebuild would produce a chain matching tampered \
             events; \
             dropping the hint under a 'less verbose' pass would \
             silently let operators try a different rebuild path: \
             {message}"
        );

        // Negative-angle pin: the swapped form must NOT appear, so a
        // refactor that drifted BOTH count-slot assertions in
        // lockstep still surfaces from this second angle.
        assert!(
            !message.contains("events file has 3 rows"),
            "ChainCorruption must NOT emit 'events file has 3 rows' \
             — pins the slot ordering from the inverse angle. A swap \
             that bound {{chain}} (3) to the 'events file has' slot \
             AND {{events}} (5) to the 'chain file has' slot would \
             still pass naive prefix/hint substring checks; this \
             inverse assertion catches the swap: {message}"
        );

        // Recovery-hint pins: audit-purge-atomicity now leaves a
        // recoverable shape (events > chain) on a crash between the
        // chain rename and the events rename in purge_older_than.
        // The Display must name BOTH branches so an operator triaging
        // ChainCorruption knows which path applies to their counts —
        // events > chain is self-heal-via-purge; events < chain is
        // tamper or stale-backup and requires a trusted restore.
        // A future refactor that dropped the recovery hint under a
        // 'less verbose' pass would silently leave operators guessing
        // at the recovery procedure, which is exactly the
        // diagnostic-degradation pattern this test class is here to
        // catch.
        assert!(
            message.contains("rerun purge_older_than"),
            "ChainCorruption must point at the self-heal command for the \
             events > chain shape — operator-facing recovery hint added \
             by audit-purge-atomicity. Dropping this phrase leaves a \
             post-crash operator without a documented next step: \
             {message}"
        );
        assert!(
            message.contains("rebuild from a trusted backup"),
            "ChainCorruption must point at the trusted-backup path for \
             the events < chain shape — that shape is indistinguishable \
             from chain tampering, so the only safe action is restore. \
             Dropping this phrase leaves the dangerous shape with the \
             same diagnostic as the recoverable shape: {message}"
        );
        assert!(
            !message.contains("chain file has 5"),
            "ChainCorruption must NOT emit 'chain file has 5' — \
             paired with the inverse assertion above so a slot swap \
             fails both inverse checks and the operator-facing \
             AuditError::ChainCorruption diagnostic stays anchored \
             from four independent positions. The 5 here would be \
             the events count surfaced in the chain slot — exactly \
             the swap this assertion catches: {message}"
        );
    }

    #[test]
    fn hash_hex_pins_64_char_sha256_lowercase_hex_and_empty_input_safety() {
        // hash_hex populates AuditKind::IntentDispatched.result_hash_hex and
        // HermesToolInvoked.preview_hash_hex. The implementation delegates to
        // SHA-256 (sha256_hex), producing exactly 64 lowercase hex chars per
        // call. This test pins the three contract properties consumers and
        // operator dashboards rely on: exact 64-char width regardless of input
        // value (a refactor to {:x} would emit variable-length hex and break
        // fixed-width column alignment); lowercase hex charset (a refactor to
        // {:X} or to_uppercase would silently break grep workflows and
        // string-equality against known-good hashes); empty-input safety (a
        // refactor to a digest that panicked on empty input would crash the
        // daemon on the first empty-result intent). Replaces the prior
        // 16-char-DefaultHasher pin (covenant-audit-hash-hex-output-width-
        // and-charset-pin), which was contract-correct for the broken
        // primitive but blocked the SHA-256 upgrade; the SHA-256 swap
        // simultaneously closes failure modes #1 (preview reversibility for
        // predictable inputs), #2 (no collision resistance), and #3 (Rust-
        // version-unstable output) tracked in audit-hash-hex-cryptographic.
        // Mirrors chain_hash_pins_separator_and_sha256_composition's parallel
        // pin of the chain anchor's 64-char SHA-256 contract.
        for (label, input) in [
            ("empty", &b""[..]),
            ("single byte", &b"a"[..]),
            ("multi byte", &b"hello"[..]),
            (
                "longer payload",
                &b"the quick brown fox jumps over the lazy dog"[..],
            ),
        ] {
            let out = hash_hex(input);
            assert_eq!(
                out.len(),
                64,
                "hash_hex must produce exactly 64 hex characters for every \
                 input including {label} — SHA-256 emits a 32-byte digest \
                 hex-encoded to 64 lowercase chars; a refactor that swapped \
                 sha256_hex for a shorter digest or truncated the hex would \
                 break operator dashboards that fixed-width-pad the \
                 result_hash_hex column and downstream tools that parse by \
                 character position; got len {} for output {:?}",
                out.len(),
                out,
            );
            assert!(
                out.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "hash_hex output must be lowercase hex only for every input \
                 including {label}; a refactor that uppercased the hex \
                 (e.g., {{:X}} or to_uppercase) would silently break grep \
                 workflows and string-equality checks against known-good \
                 hashes in fixtures or integration tests; got output {:?}",
                out,
            );
        }

        // Empty-input safety as its own assertion — pinning that the call
        // returns at all (no panic, no hang, no Empty error) even when the
        // input byte slice is zero-length. A refactor to a hasher that
        // required non-zero input would crash the daemon on the first
        // dispatched intent whose result hashes to an empty byte slice (an
        // Empty error result or an intent that produced no output) and turn
        // the audit emit path into a denial-of-service surface that an
        // attacker could trigger by inducing an empty result.
        let empty = hash_hex(b"");
        assert_eq!(
            empty.len(),
            64,
            "hash_hex(b\"\") must succeed and produce 64 hex chars — pinning \
             that empty-input is a normal, non-panicking input so the audit \
             emit path stays safe when an intent's result is empty",
        );
        // FIPS 180-4 SHA-256 of the empty string — anchors the primitive
        // identity. A refactor that swapped SHA-256 for Blake2 / SHA-3 / a
        // truncated variant under any "faster digest" rationale would
        // silently invalidate every operator's on-disk audit chain because
        // existing rows hashed under SHA-256 would no longer match
        // independent re-verification. Mirrors
        // sha256_hex_pins_nist_vectors_and_lowercase_output.
        assert_eq!(
            empty, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "hash_hex(b\"\") must equal the FIPS 180-4 SHA-256 empty-string \
             vector — pinning that hash_hex delegates to SHA-256 and is not \
             silently swapped for another primitive",
        );
    }

    #[test]
    fn sha256_hex_pins_nist_vectors_and_lowercase_output() {
        // covenant_audit::sha256_hex is the foundation
        // of the audit chain: chain_hash hashes
        // (previous_chain || "\n" || event_hex) through it,
        // chain_entry_for_line hashes each event line through it, and
        // operators rely on the deterministic 64-character lowercase
        // hex output to verify the chain externally with any
        // independent SHA-256 implementation.
        //
        // chain_hash_pins_separator_and_sha256_composition
        // only asserts internal consistency between chain_hash and
        // sha256_hex; it never pins the actual hash function identity
        // against any external standard. A refactor that swapped
        // Sha256 for Sha3_256, Blake2, or any other digest under a
        // 'use a faster hash' rationale would silently invalidate
        // every operator's on-disk audit chain because the chain
        // hashes would no longer match independent SHA-256
        // verifications.

        // FIPS 180-4 test vector: SHA-256 of the empty string.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "empty input must hash to the NIST FIPS 180-4 SHA-256 \
             test vector for the empty message. A refactor that \
             swapped Sha256 for any other digest would surface here; \
             a refactor that emitted uppercase or different byte \
             ordering would also surface here",
        );

        // FIPS 180-4 test vector: SHA-256 of ASCII "abc".
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "the canonical NIST FIPS 180-4 'abc' test vector — pins \
             that sha256_hex implements SHA-256, not SHA-224, SHA-1, \
             SHA-3-256, or any other algorithm with a 32-byte output",
        );

        // Production-shaped input: the literal "covenant" so the test
        // pins a non-NIST vector that any external tool can verify.
        assert_eq!(
            sha256_hex(b"covenant"),
            "0667bd893799ba7a888de6d210b773825f25e1576e9ad503c0061015868192e1",
            "ASCII 'covenant' must hash to the value any external \
             SHA-256 implementation would produce — pins compatibility \
             with the third-party chain verifier audit operators are \
             expected to run against the on-disk audit JSONL",
        );

        // Length and case invariants — these would still hold under
        // most digest swaps that emit hex, but anchor the formatting
        // contract independent of which input is hashed.
        let out = sha256_hex(b"any input");
        assert_eq!(
            out.len(),
            64,
            "sha256_hex output must be exactly 64 hex characters (32 \
             bytes * 2 nibbles per byte) — pins that the output is \
             not truncated to a prefix and not zero-padded beyond \
             32 bytes. A refactor that truncated to 16 bytes under a \
             'shorter audit rows' rationale would silently weaken \
             collision resistance",
        );
        assert!(
            out.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "sha256_hex output must be lowercase ASCII hex — pins the \
             :02x format specifier. A refactor that emitted :02X \
             (uppercase) would break external chain-verification \
             tools that case-sensitively compare hex strings, and \
             would change the audit-row stable identifier on \
             round-trip. got: {out}",
        );
    }

    #[test]
    fn chain_hash_pins_separator_and_sha256_composition() {
        let prev = "a".repeat(64);
        let evt = "b".repeat(64);

        let chained = chain_hash(&prev, &evt);
        assert_eq!(chained.len(), 64, "chain_hash must return 64-char hex");
        assert!(
            chained
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "chain_hash must return lowercase hex, got {chained}",
        );

        let manual = sha256_hex(format!("{prev}\n{evt}").as_bytes());
        assert_eq!(
            chained, manual,
            "chain_hash must equal sha256_hex of 'prev\\nevt' verbatim; \
             changing the separator silently invalidates every on-disk audit chain",
        );

        assert_eq!(
            chain_hash(&prev, &evt),
            chained,
            "chain_hash must be deterministic across calls",
        );

        let no_separator = sha256_hex(format!("{prev}{evt}").as_bytes());
        assert_ne!(
            chained, no_separator,
            "chain_hash must NOT match a separator-collapsed concatenation; \
             that would create ambiguity across (prev,evt) boundaries",
        );

        let other_prev = "c".repeat(64);
        assert_ne!(
            chain_hash(&other_prev, &evt),
            chained,
            "different previous hash must produce a different chain hash",
        );
        let other_evt = "d".repeat(64);
        assert_ne!(
            chain_hash(&prev, &other_evt),
            chained,
            "different event hash must produce a different chain hash",
        );

        assert_eq!(
            chain_hash(ZERO_CHAIN_HASH, &evt),
            sha256_hex(format!("{ZERO_CHAIN_HASH}\n{evt}").as_bytes()),
            "the genesis previous-hash must compose the same way as any other previous hash",
        );
    }

    #[test]
    fn audit_chain_entry_serde_pins_six_required_fields() {
        // AuditChainEntry is the per-row on-disk audit-chain record
        // persisted alongside the events JSONL. Six wire keys bind every
        // audit event to its predecessor through a sha256 chain:
        //
        // * `index` / `event_id` / `timestamp_ms`: row identity.
        // * `event_hash_hex`: this row's event-payload digest.
        // * `previous_hash_hex`: chain link backward.
        // * `chain_hash_hex`: anchor the verifier replays against.
        //
        // None of the fields carry `#[serde(default)]` or
        // `#[serde(skip_serializing_if)]`, so every persisted JSONL row
        // must contain the six keys. A refactor that defaulted any of
        // them — particularly `chain_hash_hex` or `previous_hash_hex`
        // — would silently let a corrupted row decode with an empty
        // string and the verifier would accept the broken chain.

        let entry = AuditChainEntry {
            index: 7,
            event_id: Uuid::from_u128(0x42),
            timestamp_ms: 100,
            event_hash_hex: "a".repeat(64),
            previous_hash_hex: "b".repeat(64),
            chain_hash_hex: "c".repeat(64),
        };
        let wire = serde_json::to_value(&entry).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditChainEntry serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "chain_hash_hex",
                "event_hash_hex",
                "event_id",
                "index",
                "previous_hash_hex",
                "timestamp_ms",
            ],
            "AuditChainEntry wire object must contain exactly the six \
             documented fields; an addition, rename, or drop of any key \
             silently invalidates every persisted audit chain JSONL row"
        );

        let decoded: AuditChainEntry = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded, entry,
            "AuditChainEntry must round-trip through serde_json verbatim — \
             the Eq derive is the contract the verifier's read_chain_entries \
             path leans on"
        );

        let full_obj = serde_json::to_value(&entry).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in [
            "index",
            "event_id",
            "timestamp_ms",
            "event_hash_hex",
            "previous_hash_hex",
            "chain_hash_hex",
        ] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<AuditChainEntry>(serde_json::Value::Object(payload))
                    .is_err(),
                "AuditChainEntry must reject a wire payload that omits \
                 {required}; a stray #[serde(default)] introduction — \
                 particularly on chain_hash_hex (the verifier's anchor) or \
                 previous_hash_hex (the predecessor link) — would let a \
                 corrupted chain row decode with an empty default and break \
                 the verifier's integrity verdict"
            );
        }
    }

    #[test]
    fn audit_integrity_report_serde_pins_five_required_fields() {
        // AuditIntegrityReport is the audit-chain integrity verdict the
        // daemon emits inside Response::AuditIntegrity, rendered by CLI
        // `covenant audit verify` and consumed by HTTP `/audit/integrity-report`.
        // The five wire keys document the audit-chain replay outcome:
        // * events / anchors are u64 counts.
        // * valid is the boolean operator go/no-go signal.
        // * root_hash_hex is the audit-root subject the release signing
        //   path binds to.
        // * failures is the human-readable list of bad rows.
        // None of the fields carry #[serde(default)] or
        // #[serde(skip_serializing_if)] — a refactor that defaulted any
        // would silently shift the operator verdict shape.

        let healthy = AuditIntegrityReport {
            events: 100,
            anchors: 4,
            valid: true,
            root_hash_hex: "a".repeat(64),
            failures: vec![],
        };
        let wire = serde_json::to_value(&healthy).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditIntegrityReport serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["anchors", "events", "failures", "root_hash_hex", "valid"],
            "AuditIntegrityReport wire object must contain exactly the five \
             documented fields; an addition, rename, or drop of any key \
             silently shifts the operator's audit-verify output and the \
             release-evidence audit-root subject binding"
        );

        let decoded: AuditIntegrityReport = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded, healthy,
            "AuditIntegrityReport must round-trip through serde_json verbatim — \
             the Eq derive is the contract every fixture replay leans on"
        );

        let with_failures = AuditIntegrityReport {
            events: 12,
            anchors: 0,
            valid: false,
            root_hash_hex: "b".repeat(64),
            failures: vec!["row 7: hash mismatch".into(), "row 9: missing prev".into()],
        };
        let wire = serde_json::to_value(&with_failures).unwrap();
        let failures_array = wire
            .get("failures")
            .and_then(serde_json::Value::as_array)
            .expect("failures must serialise as a JSON array");
        let strings: Vec<&str> = failures_array
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(
            strings,
            vec!["row 7: hash mismatch", "row 9: missing prev"],
            "populated failures must surface each row as a JSON string verbatim \
             — release-evidence consumers destructure on this shape"
        );

        let full_obj = serde_json::to_value(&healthy).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in ["events", "anchors", "valid", "root_hash_hex", "failures"] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<AuditIntegrityReport>(serde_json::Value::Object(payload))
                    .is_err(),
                "AuditIntegrityReport must reject a wire payload that omits \
                 {required}; a stray #[serde(default)] introduction — \
                 particularly on `valid` (the operator's go/no-go signal) or \
                 `root_hash_hex` (the release-binding subject) — must fail the \
                 test loud"
            );
        }
    }

    #[test]
    fn audit_event_round_trips_through_serde() {
        let e = dummy(intent_kind("ok"));
        let json = serde_json::to_string(&e).unwrap();
        let back: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn audit_event_serde_pins_four_required_fields() {
        // AuditEvent is the load-bearing audit envelope every JSONL audit
        // row decodes into and every IPC/HTTP /audit response surfaces.
        // Four wire keys: id, timestamp_ms, issuer, kind — none carry
        // #[serde(default)] or #[serde(skip_serializing_if)], so the wire
        // must always contain the four keys. The chain_hash composition
        // and AuditChainEntry replay both lean on this stable shape; a
        // refactor that defaulted any field would silently let a
        // corrupted row decode, and the verifier would accept a broken
        // chain.
        let event = AuditEvent {
            id: Uuid::nil(),
            timestamp_ms: 1_700_000_000_000,
            issuer: AgentId::new("user@local", [0u8; 32]),
            kind: AuditKind::CapabilityCheck {
                agent_id: "x@y".into(),
                required_actions: vec!["memory.read".into()],
                missing_actions: vec![],
                passed: true,
            },
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditEvent serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["id", "issuer", "kind", "timestamp_ms"],
            "AuditEvent wire object must contain exactly four documented \
             fields; a skip_serializing_if on any one would silently shift \
             every persisted JSONL audit row and break chain_hash's \
             stable-serialization dependency"
        );

        // kind must carry an inner discriminator under "type", pinning
        // AuditKind's #[serde(tag = \"type\", rename_all = \"snake_case\")]
        // contract at the envelope boundary.
        let kind_obj = wire
            .get("kind")
            .and_then(serde_json::Value::as_object)
            .expect("kind must serialise as a JSON object");
        assert_eq!(
            kind_obj.get("type"),
            Some(&serde_json::json!("capability_check")),
            "AuditKind discriminator tag must be \"type\" and slug must be \
             snake_case; a refactor that drops the tag attribute would \
             silently break every CLI/HTTP consumer destructuring on the \
             type field"
        );

        // Round-trip pins the PartialEq + Eq derive contract on every field.
        let back: AuditEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, event);

        // Each strictly-required field must reject when omitted.
        for required in ["id", "timestamp_ms", "issuer", "kind"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditEvent>(serde_json::Value::Object(missing)).is_err(),
                "AuditEvent wire form must reject a payload missing {required:?}",
            );
        }
    }

    #[test]
    fn audit_kind_capability_check_serde_pins_four_field_variant() {
        // AuditKind::CapabilityCheck is the load-bearing audit row
        // emitted on every dispatch-time capability check through
        // covenantd::Server. Four required fields: agent_id (String),
        // required_actions (Vec<String>), missing_actions (Vec<String>),
        // passed (bool). audit_event_serde_pins_four_required_fields
        // uses CapabilityCheck only as the envelope's payload carrier
        // and does not pin the variant fields directly — a refactor
        // that flipped missing_actions to #[serde(default)] would let
        // a row with passed=false silently decode with an empty list
        // and erase the triage signal naming which capability was
        // short, and a bool→Option<bool> flip on passed would collapse
        // the pass/fail discriminator into policy-dependent None
        // handling.
        let kind = AuditKind::CapabilityCheck {
            agent_id: "agent@local".into(),
            required_actions: vec!["memory.read".into()],
            missing_actions: vec!["memory.write".into()],
            passed: false,
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "agent_id",
                "missing_actions",
                "passed",
                "required_actions",
                "type",
            ],
            "AuditKind::CapabilityCheck wire form must be exactly five keys: the four variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("capability_check")),
            "AuditKind discriminator slug must be snake_case 'capability_check'; a titlecase or kebab-case regression silently strands every prior capability-check audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::CapabilityCheck must round-trip through serde_json verbatim — the PartialEq derive is the contract dispatch-time capability-enforcement triage joins on",
        );

        for required in ["agent_id", "required_actions", "missing_actions", "passed"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::CapabilityCheck wire form must reject a payload missing {required:?}; a stray #[serde(default)] on missing_actions would silently let a passed=false row decode with an empty list and erase which capability was short, and a bool→Option<bool> flip on passed would collapse the pass/fail discriminator into policy-dependent None handling",
            );
        }
    }

    #[test]
    fn audit_kind_intent_dispatched_serde_pins_five_field_variant() {
        // AuditKind::IntentDispatched is the load-bearing audit variant
        // emitted on every successful dispatch through
        // covenantd::Server::dispatch_intent. Five fields:
        //
        // * intent_id: Uuid — strictly required
        // * intent_text: String — strictly required
        // * matched_agent: Option<String> — no #[serde(default)] and no
        //   #[serde(skip_serializing_if)], so the wire must always emit
        //   the key (None as JSON null)
        // * result_hash_hex: String — strictly required
        // * status: String — strictly required
        //
        // audit_event_serde_pins_four_required_fields uses
        // CapabilityCheck only as a payload carrier and is now joined
        // by audit_kind_capability_check_serde_pins_four_field_variant
        // which pins that variant's wire form directly; this test pins
        // the IntentDispatched wire form so a refactor that flipped
        // result_hash_hex to #[serde(default)] (chain_hash absorbs an
        // empty string and the verifier passes) or renamed any field
        // would fail loud instead of producing a silently-broken
        // audit row.
        let kind = AuditKind::IntentDispatched {
            intent_id: Uuid::nil(),
            intent_text: "hi".into(),
            matched_agent: Some("a@b".into()),
            result_hash_hex: "deadbeef".into(),
            status: "ok".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "intent_id",
                "intent_text",
                "matched_agent",
                "result_hash_hex",
                "status",
                "type",
            ],
            "AuditKind::IntentDispatched wire form must be exactly six keys: the five variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("intent_dispatched")),
            "AuditKind discriminator slug must be snake_case 'intent_dispatched'; a titlecase or kebab-case regression would silently break every prior audit row",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::IntentDispatched must round-trip through serde_json verbatim — the PartialEq derive is the contract audit replay leans on",
        );

        for required in ["intent_id", "intent_text", "result_hash_hex", "status"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::IntentDispatched wire form must reject a payload missing {required:?}; a stray #[serde(default)] on a required field would silently let a malformed audit row decode and the chain_hash verifier would accept tampered state",
            );
        }

        let none_matched = AuditKind::IntentDispatched {
            intent_id: Uuid::nil(),
            intent_text: "hi".into(),
            matched_agent: None,
            result_hash_hex: "deadbeef".into(),
            status: "no_match".into(),
        };
        let wire = serde_json::to_value(&none_matched).unwrap();
        assert_eq!(
            wire.get("matched_agent"),
            Some(&serde_json::Value::Null),
            "matched_agent: None must surface as JSON null — the field has no #[serde(skip_serializing_if)] so the wire shape stays stable across matched and unmatched dispatch rows",
        );
        assert_eq!(
            wire.as_object().unwrap().len(),
            6,
            "AuditKind::IntentDispatched with matched_agent=None must still surface six keys on the wire; a skip_serializing_if regression would silently shrink the wire form for unmatched intents",
        );
    }

    #[test]
    fn audit_kind_memory_repair_applied_serde_pins_five_field_variant() {
        // AuditKind::MemoryRepairApplied is the audit row covenantd::Server
        // emits when an operator completes a memory repair request. The
        // full before/after record shape is returned to the caller
        // through the repair response; the audit row keeps the durable
        // who/what/why envelope without duplicating memory text into the
        // audit log. Five required fields: memory_id (Uuid), action
        // (String), mode (String), changed (bool), reason (String). A
        // refactor that #[serde(default)]-ed memory_id would let a
        // malformed row decode with Uuid::nil() and erase the unforgeable
        // target identifier; a default on changed would mask the
        // mutation-vs-no-op triage signal that distinguishes a repair
        // that actually edited from one that found nothing to change.
        let kind = AuditKind::MemoryRepairApplied {
            memory_id: Uuid::nil(),
            action: "rebind".into(),
            mode: "apply".into(),
            changed: true,
            reason: "operator-corrected receipt".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["action", "changed", "memory_id", "mode", "reason", "type"],
            "AuditKind::MemoryRepairApplied wire form must be exactly six keys: the five variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("memory_repair_applied")),
            "AuditKind discriminator slug must be snake_case 'memory_repair_applied'; a titlecase or kebab-case regression silently strands every prior memory-repair audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::MemoryRepairApplied must round-trip through serde_json verbatim — the PartialEq derive is the contract memory-repair audit triage joins on",
        );

        for required in ["memory_id", "action", "mode", "changed", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::MemoryRepairApplied wire form must reject a payload missing {required:?}; a stray #[serde(default)] on memory_id would let a malformed row decode with Uuid::nil() and erase the unforgeable target identifier, and a default on changed would mask the mutation-vs-no-op triage signal",
            );
        }
    }

    #[test]
    fn audit_kind_memory_compaction_applied_serde_pins_six_field_variant() {
        // AuditKind::MemoryCompactionApplied is the audit row
        // covenantd::Server emits when an operator runs bounded memory
        // compaction. The row records ids only; memory text and
        // before/after payloads stay out of the audit stream. Six
        // required fields: mode (String), changed (bool), reason
        // (String), deleted (Vec<Uuid>), stale_marked (Vec<Uuid>),
        // parents_detached (Vec<Uuid>). A #[serde(default)] regression
        // on any of the three id lists would let a malformed row
        // decode with empty Vec<Uuid> and erase which memory ids were
        // touched; a default on `changed` would mask the
        // compaction-mutation-vs-no-op triage signal.
        let kind = AuditKind::MemoryCompactionApplied {
            mode: "apply".into(),
            changed: true,
            reason: "operator-bounded compaction".into(),
            deleted: vec![Uuid::nil()],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "changed",
                "deleted",
                "mode",
                "parents_detached",
                "reason",
                "stale_marked",
                "type",
            ],
            "AuditKind::MemoryCompactionApplied wire form must be exactly seven keys: the six variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("memory_compaction_applied")),
            "AuditKind discriminator slug must be snake_case 'memory_compaction_applied'; a titlecase or kebab-case regression silently strands every prior memory-compaction audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::MemoryCompactionApplied must round-trip through serde_json verbatim — the PartialEq derive is the contract memory-compaction audit triage joins on",
        );

        for required in [
            "mode",
            "changed",
            "reason",
            "deleted",
            "stale_marked",
            "parents_detached",
        ] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::MemoryCompactionApplied wire form must reject a payload missing {required:?}; a stray #[serde(default)] on any of the three Vec<Uuid> id lists would let a malformed row decode with empty lists and erase which memory ids were touched, and a default on changed would mask the compaction-mutation-vs-no-op triage signal",
            );
        }
    }

    #[test]
    fn audit_kind_budget_exhausted_serde_pins_six_field_variant() {
        // AuditKind::BudgetExhausted is the audit row covenantd::Server::
        // dispatch_intent emits when the matched agent's budget bucket
        // is exhausted. The row doubles as the resume queue — `covenant
        // intents resume <intent-id>` re-dispatches from this exact row,
        // so the six fields (agent_display, intent_id, intent_text,
        // requested, tokens_remaining, refill_eta_ms) are load-bearing.
        // A rename or #[serde(default)] regression on intent_text or
        // intent_id would silently empty the resume queue or re-dispatch
        // a meaningless intent.
        let kind = AuditKind::BudgetExhausted {
            agent_display: "research@agent".into(),
            intent_id: Uuid::nil(),
            intent_text: "find papers".into(),
            requested: 100,
            tokens_remaining: 5,
            refill_eta_ms: 3_600_000,
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "agent_display",
                "intent_id",
                "intent_text",
                "refill_eta_ms",
                "requested",
                "tokens_remaining",
                "type",
            ],
            "AuditKind::BudgetExhausted wire form must be exactly seven keys: the six variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("budget_exhausted")),
            "AuditKind discriminator slug must be snake_case 'budget_exhausted'; a titlecase or kebab-case regression strands every operator's resume tooling",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::BudgetExhausted must round-trip through serde_json verbatim — the PartialEq derive is the contract resume tooling joins on",
        );

        for required in [
            "agent_display",
            "intent_id",
            "intent_text",
            "requested",
            "tokens_remaining",
            "refill_eta_ms",
        ] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::BudgetExhausted wire form must reject a payload missing {required:?}; a stray #[serde(default)] on intent_text or intent_id would silently let the resume queue re-dispatch a meaningless intent",
            );
        }
    }

    #[test]
    fn audit_kind_budget_preempted_serde_pins_five_field_variant_and_optional_exit_code() {
        // BudgetPreempted is the audit row covenantd's preempt_dispatcher
        // (future C2 slice) emits after a successful subprocess
        // termination. Operator post-mortem joins on (intent_id,
        // signal_sent, exit_code) to classify cooperative SIGTERM exits
        // vs. SIGKILL fallbacks vs. natural-exit races. The five fields
        // plus the 'type' discriminator are load-bearing; a refactor
        // that dropped signal_sent (e.g. unified into reason) would
        // silently strand that classification.
        let kind = AuditKind::BudgetPreempted {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            reason: "projected_overshoot".into(),
            signal_sent: "SIGTERM".into(),
            exit_code: Some(143),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "agent_display",
                "exit_code",
                "intent_id",
                "reason",
                "signal_sent",
                "type",
            ],
            "AuditKind::BudgetPreempted wire form must be exactly six keys: the five variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("budget_preempted")),
            "AuditKind discriminator slug must be snake_case 'budget_preempted'; a titlecase regression or a merge with BudgetExhausted's slug would silently collapse the killed-during-execution vs. denied-pre-dispatch split that /audit consumers filter on",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::BudgetPreempted must round-trip through serde_json verbatim",
        );

        // exit_code is Option<i32>; None must serialize as JSON null
        // and round-trip to None. Required-field tests below check the
        // non-optional surface; this check pins the optional surface
        // separately so a refactor that dropped Option or applied
        // skip_serializing_if would surface.
        let none_kind = AuditKind::BudgetPreempted {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            reason: "projected_overshoot".into(),
            signal_sent: "none".into(),
            exit_code: None,
        };
        let none_wire = serde_json::to_value(&none_kind).unwrap();
        assert_eq!(
            none_wire.as_object().and_then(|o| o.get("exit_code")),
            Some(&serde_json::Value::Null),
            "BudgetPreempted::exit_code must serialize as null when None; skip_serializing_if would hide the signal-terminated case from consumers that filter on the key's presence"
        );
        let back: AuditKind = serde_json::from_value(none_wire).unwrap();
        assert_eq!(back, none_kind);

        // exit_code is Option<i32>; serde decodes Option from a missing
        // wire key as None, so it is intentionally NOT in the
        // required-omission walk. The wire-key count assertion above
        // (six keys including exit_code) catches a stray
        // skip_serializing_if regression; the null-on-wire round-trip
        // catches a refactor that dropped Option entirely.
        for required in ["agent_display", "intent_id", "reason", "signal_sent"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::BudgetPreempted wire form must reject a payload missing {required:?}; a stray #[serde(default)] would silently let an invalid preempt row decode",
            );
        }
    }

    #[test]
    fn audit_kind_budget_preempt_failed_serde_pins_four_field_variant() {
        // BudgetPreemptFailed is the security-relevant counterpart of
        // BudgetPreempted. errno=ESRCH means the subprocess already
        // exited (benign — the daemon may also choose to emit
        // BudgetPreempted with signal_sent="none"); errno=EPERM means
        // the daemon ran without permission to signal that pid, which
        // is a configuration or security incident. The four fields
        // plus 'type' are load-bearing.
        let kind = AuditKind::BudgetPreemptFailed {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            reason: "projected_overshoot".into(),
            errno: 1, // EPERM
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent_display", "errno", "intent_id", "reason", "type"],
            "AuditKind::BudgetPreemptFailed wire form must be exactly five keys: the four variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("budget_preempt_failed")),
            "AuditKind discriminator slug must be snake_case 'budget_preempt_failed'; a merge with BudgetPreempted's slug would silently collapse the successful-preempt vs. failed-signal split that operator incident triage joins on",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, kind);

        for required in ["agent_display", "intent_id", "reason", "errno"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::BudgetPreemptFailed wire form must reject a payload missing {required:?}",
            );
        }
    }

    #[test]
    fn audit_kind_capability_granted_serde_pins_four_field_variant() {
        // AuditKind::CapabilityGranted is the durable audit row that
        // ties a SignedCapability's signature_b58 back to the actor who
        // issued the grant. The audit verifier and operator triage
        // tooling correlate on signature_b58, so the four fields
        // (subject_display, action, granted_by_display, signature_b58)
        // are load-bearing. A rename or #[serde(default)] regression on
        // signature_b58 would silently break the grant-audit correlation
        // chain.
        let kind = AuditKind::CapabilityGranted {
            subject_display: "research@local".into(),
            action: "memory.write".into(),
            granted_by_display: "authority@local".into(),
            signature_b58: "deadbeef".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "action",
                "granted_by_display",
                "signature_b58",
                "subject_display",
                "type",
            ],
            "AuditKind::CapabilityGranted wire form must be exactly five keys: the four variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("capability_granted")),
            "AuditKind discriminator slug must be snake_case 'capability_granted'; a titlecase 'CapabilityGranted' regression breaks every prior grant audit row",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::CapabilityGranted must round-trip through serde_json verbatim — the PartialEq derive is the contract the grant-audit correlation chain leans on",
        );

        for required in [
            "subject_display",
            "action",
            "granted_by_display",
            "signature_b58",
        ] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::CapabilityGranted wire form must reject a payload missing {required:?}; a stray #[serde(default)] on signature_b58 would silently let the row decode with an empty signature and break the SignedCapability correlation",
            );
        }

        let titlecase = serde_json::json!({
            "type": "CapabilityGranted",
            "subject_display": "research@local",
            "action": "memory.write",
            "granted_by_display": "authority@local",
            "signature_b58": "deadbeef",
        });
        assert!(
            serde_json::from_value::<AuditKind>(titlecase).is_err(),
            "titlecase 'CapabilityGranted' must reject — the rename_all = snake_case contract is what keeps every prior grant audit row decoding stably across rebuilds",
        );
    }

    #[test]
    fn audit_kind_intent_ignored_serde_pins_three_field_variant() {
        // AuditKind::IntentIgnored records which CLI-installed ignore
        // pattern fired on a dispatched intent. matched_pattern is the
        // only durable link back to the operator's decision to suppress
        // — a rename or default would silently break ignore-rule
        // diagnostics.
        let kind = AuditKind::IntentIgnored {
            intent_id: Uuid::nil(),
            intent_text: "ignored".into(),
            matched_pattern: "rule-a".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["intent_id", "intent_text", "matched_pattern", "type"],
        );
        assert_eq!(obj.get("type"), Some(&serde_json::json!("intent_ignored")),);

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, kind);

        for required in ["intent_id", "intent_text", "matched_pattern"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::IntentIgnored wire form must reject a payload missing {required:?}",
            );
        }
    }

    #[test]
    fn audit_kind_capability_grant_rejected_serde_pins_three_field_variant() {
        // AuditKind::CapabilityGrantRejected records denied authority
        // claims. reason is the durable record of *why* the grant was
        // denied — a rename or default would break the rejection trail.
        let kind = AuditKind::CapabilityGrantRejected {
            subject_display: "research@local".into(),
            action: "memory.write".into(),
            reason: "scope rejected".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["action", "reason", "subject_display", "type"],);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("capability_grant_rejected")),
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, kind);

        for required in ["subject_display", "action", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::CapabilityGrantRejected wire form must reject a payload missing {required:?}",
            );
        }
    }

    #[test]
    fn audit_kind_capability_scope_rejected_serde_pins_three_field_variant() {
        // AuditKind::CapabilityScopeRejected records every scope-mismatched
        // dispatch — the action field carries the dotted-path scope key
        // (memory.write, a2a.send.<sender>) and is the load-bearing
        // diagnostic field.
        let kind = AuditKind::CapabilityScopeRejected {
            agent_id: "research@local".into(),
            action: "memory.write".into(),
            reason: "tier mismatch".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["action", "agent_id", "reason", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("capability_scope_rejected")),
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, kind);

        for required in ["agent_id", "action", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::CapabilityScopeRejected wire form must reject a payload missing {required:?}",
            );
        }
    }

    #[test]
    fn audit_kind_authentication_failed_serde_pins_two_field_variant() {
        // AuditKind::AuthenticationFailed records every rejected auth
        // attempt; transport ('ipc' / 'http') is the per-channel
        // attack-attribution signal a rename or default would break.
        let kind = AuditKind::AuthenticationFailed {
            transport: "ipc".into(),
            reason: "unknown token".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["reason", "transport", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("authentication_failed")),
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, kind);

        for required in ["transport", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::AuthenticationFailed wire form must reject a payload missing {required:?}",
            );
        }
    }

    #[test]
    fn audit_kind_operator_peer_revoke_rejected_serde_pins_two_field_variant() {
        // AuditKind::OperatorPeerRevokeRejected is the daemon-as-issuer
        // probe row emitted when RevokePeer is rejected because the
        // authenticated peer is not the operator. Same audience model
        // as OperatorTokenRotationRejected and OperatorPeersListRejected.
        // peer_pubkey_b58 is the unforgeable identifier; peer_display
        // is wire-supplied.
        let kind = AuditKind::OperatorPeerRevokeRejected {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: "guestPubkeyB58".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["peer_display", "peer_pubkey_b58", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("operator_peer_revoke_rejected")),
            "AuditKind discriminator slug must be snake_case 'operator_peer_revoke_rejected'; a titlecase or kebab-case regression silently strands every prior revoke-probe row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::OperatorPeerRevokeRejected must round-trip through serde_json verbatim — the PartialEq derive is the contract revoke-probe triage joins on",
        );

        for required in ["peer_display", "peer_pubkey_b58"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::OperatorPeerRevokeRejected wire form must reject a payload missing {required:?}; a stray #[serde(default)] on peer_pubkey_b58 would leave only the wire-controlled peer_display and erase the unforgeable probe-attribution signal",
            );
        }
    }

    #[test]
    fn audit_kind_peer_revoked_serde_pins_three_field_variant() {
        // AuditKind::PeerRevoked records every successful operator
        // RevokePeer call. peer_display and peer_pubkey_b58 describe
        // the *revoked* peer (not the operator issuer). token_prefix
        // is the 6-char base58 redaction OperatorTokenRotated uses —
        // full token bytes never enter the audit log. A rename or
        // default on peer_pubkey_b58 erases the unforgeable identity
        // of the revoked peer; a refactor that swapped token_prefix
        // for full token bytes converts an audit-row leak into
        // credential theft of the revoked peer's prior token.
        let kind = AuditKind::PeerRevoked {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: "guestPubkeyB58".into(),
            token_prefix: "abcdef".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["peer_display", "peer_pubkey_b58", "token_prefix", "type"],
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("peer_revoked")),
            "AuditKind discriminator slug must be snake_case 'peer_revoked'; a titlecase or kebab-case regression silently strands every prior revocation audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::PeerRevoked must round-trip through serde_json verbatim — the PartialEq derive is the contract revocation audit triage joins on",
        );

        for required in ["peer_display", "peer_pubkey_b58", "token_prefix"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::PeerRevoked wire form must reject a payload missing {required:?}; a stray #[serde(default)] on peer_pubkey_b58 would erase the unforgeable identity of the revoked peer, and on token_prefix would mask the durable redacted-token correlation",
            );
        }
    }

    #[test]
    fn audit_kind_peer_self_revoke_blocked_serde_pins_three_field_variant() {
        // AuditKind::PeerSelfRevokeBlocked records the operator's own
        // RevokePeer call rejected by SelfRevokeForbidden because
        // `force` was false. Operator is both the issuer and the
        // audience — distinct from OperatorPeerRevokeRejected which
        // records a non-operator's probe under the daemon-issuer
        // audience. peer_display and peer_pubkey_b58 describe the
        // operator's own identity here; token_prefix is the 6-char
        // base58 redaction PeerRevoked and OperatorTokenRotated use —
        // full token bytes never enter the audit log. A rename or
        // default on peer_pubkey_b58 erases the unforgeable
        // operator-identity binding; a refactor that swapped
        // token_prefix for full token bytes converts the audit-row
        // leak into operator bootstrap-token theft.
        let kind = AuditKind::PeerSelfRevokeBlocked {
            peer_display: "user@local".into(),
            peer_pubkey_b58: "operatorPubkeyB58".into(),
            token_prefix: "abcdef".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["peer_display", "peer_pubkey_b58", "token_prefix", "type"],
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("peer_self_revoke_blocked")),
            "AuditKind discriminator slug must be snake_case 'peer_self_revoke_blocked'; a titlecase or kebab-case regression silently strands every prior self-revoke-block audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::PeerSelfRevokeBlocked must round-trip through serde_json verbatim — the PartialEq derive is the contract self-fat-finger audit triage joins on",
        );

        for required in ["peer_display", "peer_pubkey_b58", "token_prefix"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::PeerSelfRevokeBlocked wire form must reject a payload missing {required:?}; a stray #[serde(default)] on peer_pubkey_b58 would erase the unforgeable operator-identity binding, and on token_prefix would mask the durable redacted-token correlation",
            );
        }
    }

    #[test]
    fn audit_kind_operator_peers_list_rejected_serde_pins_two_field_variant() {
        // AuditKind::OperatorPeersListRejected is the daemon-as-issuer
        // probe row emitted when ListPeers is rejected because the
        // authenticated peer is not the operator. Mirrors the
        // OperatorTokenRotationRejected audience model so the row
        // surfaces on the operator's /audit feed without making the
        // rejected peer's own feed a probe-was-logged oracle.
        // peer_pubkey_b58 is the unforgeable identifier; peer_display
        // is wire-supplied.
        let kind = AuditKind::OperatorPeersListRejected {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: "guestPubkeyB58".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["peer_display", "peer_pubkey_b58", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("operator_peers_list_rejected")),
            "AuditKind discriminator slug must be snake_case 'operator_peers_list_rejected'; a titlecase or kebab-case regression silently strands every prior peer-enumeration probe row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::OperatorPeersListRejected must round-trip through serde_json verbatim — the PartialEq derive is the contract peer-enumeration-probe triage joins on",
        );

        for required in ["peer_display", "peer_pubkey_b58"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::OperatorPeersListRejected wire form must reject a payload missing {required:?}; a stray #[serde(default)] on peer_pubkey_b58 would leave only the wire-controlled peer_display and erase the unforgeable probe-attribution signal",
            );
        }
    }

    #[test]
    fn audit_kind_operator_token_rotation_rejected_serde_pins_two_field_variant() {
        // AuditKind::OperatorTokenRotationRejected is the daemon-as-
        // issuer probe row emitted when RotateOperatorToken is rejected
        // because the authenticated peer's pubkey doesn't match the
        // operator identity. peer_pubkey_b58 is the unforgeable
        // identifier — peer_display is wire-supplied and an attacker
        // could register any display against any pubkey, so collapsing
        // pubkey_b58 with #[serde(default)] would leave only the
        // wire-controlled display and erase the probe-attribution
        // signal that becomes load-bearing at Phase-1 multi-peer.
        let kind = AuditKind::OperatorTokenRotationRejected {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: "guestPubkeyB58".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["peer_display", "peer_pubkey_b58", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("operator_token_rotation_rejected")),
            "AuditKind discriminator slug must be snake_case 'operator_token_rotation_rejected'; a titlecase or kebab-case regression silently strands every prior rotation-rejection probe row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::OperatorTokenRotationRejected must round-trip through serde_json verbatim — the PartialEq derive is the contract Phase-1 multi-peer probe triage will lean on",
        );

        for required in ["peer_display", "peer_pubkey_b58"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::OperatorTokenRotationRejected wire form must reject a payload missing {required:?}; a stray #[serde(default)] on peer_pubkey_b58 would leave only the wire-controlled peer_display and erase the unforgeable probe-attribution signal",
            );
        }
    }

    #[test]
    fn audit_kind_operator_token_rotated_serde_pins_three_field_variant() {
        // AuditKind::OperatorTokenRotated records every operator
        // bootstrap-token rotation. Token bytes never enter the audit
        // log — only 6-char base58 prefixes (matching PeerToken::Debug
        // redaction) so an operator can correlate a rotation row with
        // the on-disk file's first chars. old_token_prefix and
        // new_token_prefix together form the verification link letting
        // the operator confirm whether a rotation they did or did not
        // initiate matches the durable file state. A rename or default
        // breaks that link; a refactor that swapped prefixes for full
        // token bytes converts an audit-row leak into credential theft.
        let kind = AuditKind::OperatorTokenRotated {
            peer_display: "user@local".into(),
            old_token_prefix: "aaaaaa".into(),
            new_token_prefix: "bbbbbb".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "new_token_prefix",
                "old_token_prefix",
                "peer_display",
                "type",
            ],
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("operator_token_rotated")),
            "AuditKind discriminator slug must be snake_case 'operator_token_rotated'; a titlecase regression silently strands every prior rotation audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::OperatorTokenRotated must round-trip through serde_json verbatim — the PartialEq derive is the contract the on-disk-file-vs-audit-row rotation verification leans on",
        );

        for required in ["peer_display", "old_token_prefix", "new_token_prefix"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::OperatorTokenRotated wire form must reject a payload missing {required:?}; a stray #[serde(default)] on new_token_prefix would break the on-disk-file-vs-audit-row correlation an operator uses to confirm a rotation matches the durable file state, masking a silent rotation or compromise",
            );
        }
    }

    #[test]
    fn audit_kind_budget_unseeded_serde_pins_three_field_variant() {
        // AuditKind::BudgetUnseeded is the audit row emitted when
        // dispatch_intent falls into the NoCapacity fail-open arm:
        // the manifest opted in to budget enforcement but no bucket
        // was seeded for the agent. Distinct from BudgetExhausted so
        // /audit consumers can filter operator-misconfig (forgot
        // register_agent_budgets) vs. policy-rejection without
        // special-casing sentinel values. A rename, default, or
        // shared-slug regression would collapse the two arms and
        // operators would lose the operator-misconfig signal.
        let kind = AuditKind::BudgetUnseeded {
            agent_display: "research@agent".into(),
            intent_id: Uuid::nil(),
            requested: 100,
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent_display", "intent_id", "requested", "type"],
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("budget_unseeded")),
            "AuditKind discriminator slug must be snake_case 'budget_unseeded'; a titlecase regression or a merge with BudgetExhausted's slug would silently collapse the operator-misconfig vs. policy-rejection split that /audit consumers filter on",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::BudgetUnseeded must round-trip through serde_json verbatim — the PartialEq derive is the contract operator-misconfig diagnosis leans on",
        );

        for required in ["agent_display", "intent_id", "requested"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::BudgetUnseeded wire form must reject a payload missing {required:?}; a stray #[serde(default)] on agent_display would silently let the row decode with an empty string and break the back-correlation to the agent whose bucket is missing",
            );
        }
    }

    #[test]
    fn audit_kind_capability_revoke_rejected_serde_pins_two_field_variant() {
        // AuditKind::CapabilityRevokeRejected is the audit row emitted
        // when RevokeCapability is rejected because the authenticated
        // peer is not the subject of the capability they asked to
        // revoke. Enforces the subject-ownership invariant on the
        // revoking peer's pubkey, closing the cross-peer-revoke gap.
        // signature_b58 is the durable correlation back to the
        // SignedCapability the rejecting peer attempted to tombstone —
        // a rename or #[serde(default)] would mask a real cross-peer-
        // revoke probe behind a generic empty-signature row.
        let kind = AuditKind::CapabilityRevokeRejected {
            signature_b58: "deadbeef".into(),
            reason: "not subject".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["reason", "signature_b58", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("capability_revoke_rejected")),
            "AuditKind discriminator slug must be snake_case 'capability_revoke_rejected'; a titlecase or kebab-case regression silently strands every prior cross-peer-revoke probe row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::CapabilityRevokeRejected must round-trip through serde_json verbatim — the PartialEq derive is the contract the cross-peer-revoke audit correlation leans on",
        );

        for required in ["signature_b58", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::CapabilityRevokeRejected wire form must reject a payload missing {required:?}; a stray #[serde(default)] on signature_b58 would silently let the row decode with an empty signature and mask a real cross-peer-revoke probe behind a generic empty-signature row",
            );
        }
    }

    #[test]
    fn audit_kind_a2a_recipient_rejected_serde_pins_three_field_variant() {
        // AuditKind::A2ARecipientRejected is the audit row emitted when
        // SendA2ATask is rejected because the recipient peer has not
        // granted `a2a.recv.<sender>` to themselves. Distinct from
        // CapabilityCheck because the missing cap belongs to a different
        // subject than the issuer of the audit row — collapsing this
        // into CapabilityCheck would misattribute which peer's caps
        // were short. sender_display, recipient_display, and action
        // (the missing scope name) are all load-bearing for triage; a
        // rename or #[serde(default)] would collapse the two-party
        // diagnostic and lose the missing-scope correlation back to the
        // recipient's grant decisions.
        let kind = AuditKind::A2ARecipientRejected {
            sender_display: "attacker@local".into(),
            recipient_display: "victim@local".into(),
            action: "a2a.recv.attacker@local".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["action", "recipient_display", "sender_display", "type"],
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("a2_a_recipient_rejected")),
            "AuditKind discriminator slug must be 'a2_a_recipient_rejected' — serde's rename_all = snake_case splits the 'A2A' prefix on each digit/uppercase boundary, producing 'a2_a_…'. This is the durable wire form every persisted A2ARecipientRejected audit row uses; a refactor that 'fixed' the slug to 'a2a_recipient_rejected' would silently strand every prior recipient-cap-rejection audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::A2ARecipientRejected must round-trip through serde_json verbatim — the PartialEq derive is the contract recipient-cap-rejection triage joins on",
        );

        for required in ["sender_display", "recipient_display", "action"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::A2ARecipientRejected wire form must reject a payload missing {required:?}; a stray #[serde(default)] on action would silently let the row decode with an empty scope string and break the missing-cap correlation back to the recipient's grant decisions",
            );
        }
    }

    #[test]
    fn audit_kind_a2a_sender_mismatch_serde_pins_two_field_variant() {
        // AuditKind::A2ASenderMismatch is the audit row emitted when
        // SendA2ATask is rejected because the supplied task.sender does
        // not match the authenticated peer on the connection. Closes
        // the sender-spoof attack class — a malicious local process
        // claiming to be a different agent on the wire than the one
        // bound to its peer token. peer_display (the authenticated
        // peer) and claimed_sender_display (the spoofed identity) are
        // both load-bearing for triage; a rename or #[serde(default)]
        // on either would collapse the two identities into one
        // diagnostic and the spoof attribution would be lost.
        let kind = AuditKind::A2ASenderMismatch {
            peer_display: "attacker@local".into(),
            claimed_sender_display: "victim@local".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["claimed_sender_display", "peer_display", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("a2_a_sender_mismatch")),
            "AuditKind discriminator slug must be 'a2_a_sender_mismatch' — serde's rename_all = snake_case splits the 'A2A' prefix on each digit/uppercase boundary, producing 'a2_a_…'. This is the durable wire form every persisted A2ASenderMismatch audit row uses; a refactor that 'fixed' the slug to 'a2a_sender_mismatch' would silently strand every prior sender-spoof audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::A2ASenderMismatch must round-trip through serde_json verbatim — the PartialEq derive is the contract sender-spoof attribution leans on",
        );

        for required in ["peer_display", "claimed_sender_display"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::A2ASenderMismatch wire form must reject a payload missing {required:?}; a stray #[serde(default)] on either identity would collapse the two-party spoof event into a one-sided diagnostic",
            );
        }
    }

    #[test]
    fn audit_kind_a2a_result_rejected_serde_pins_two_field_variant() {
        // AuditKind::A2AResultRejected is the audit row emitted when
        // PostA2AResult is rejected upstream of any capability check
        // — e.g. the supplied task_id was never dispatched through this
        // daemon. Stronger compromise indicator than a missing-cap
        // rejection: no honest agent generates a nonexistent task_id.
        // task_id is the durable correlation handle back to the
        // originating dispatch — a rename or #[serde(default)] would let
        // a malformed row decode with Uuid::nil() and mask the real
        // upstream-compromise event behind a generic nil-uuid row.
        let kind = AuditKind::A2AResultRejected {
            task_id: Uuid::nil(),
            reason: "unknown task".into(),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["reason", "task_id", "type"]);
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("a2_a_result_rejected")),
            "AuditKind discriminator slug must be 'a2_a_result_rejected' — serde's rename_all = snake_case splits the 'A2A' prefix on each digit/uppercase boundary, producing 'a2_a_…'. This is the durable wire form every persisted A2AResultRejected audit row uses; a refactor that 'fixed' the slug to 'a2a_result_rejected' would silently strand every prior upstream-compromise audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::A2AResultRejected must round-trip through serde_json verbatim — the PartialEq derive is the contract the upstream-compromise audit correlation leans on",
        );

        for required in ["task_id", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::A2AResultRejected wire form must reject a payload missing {required:?}; a stray #[serde(default)] on task_id would silently let the row decode with Uuid::nil() and mask a real upstream-compromise event behind a generic nil-uuid row",
            );
        }
    }

    #[test]
    fn audit_kind_a2a_repair_applied_serde_pins_six_field_variant() {
        // AuditKind::A2ARepairApplied is the audit row covenantd::Server
        // emits when an operator repairs an in-flight A2A lease.
        // `action` is `requeue`, `force_error`, or `auto_requeue`.
        // Full task payloads stay in the mailbox log; the audit row
        // records who acted, why, and which lease they intended to
        // mutate. Six required fields: task_id (Uuid), action (String),
        // reason (String), lease_id (Option<Uuid>), duplicate_risk
        // (Option<String>), attempt (u32). Neither Option field carries
        // #[serde(skip_serializing_if)] so both keys must surface on
        // the wire (null when None) — a skip_serializing_if regression
        // would silently shrink the wire form for the None case and
        // consumers destructuring on a fixed-key set would drop into a
        // different decode path. The durable slug is
        // 'a2_a_repair_applied' per the serde rename_all=snake_case
        // digit/upper split, matching the existing A2A* pins.
        let kind = AuditKind::A2ARepairApplied {
            task_id: Uuid::nil(),
            action: "requeue".into(),
            reason: "operator-cleared stuck lease".into(),
            lease_id: Some(Uuid::nil()),
            duplicate_risk: Some("low".into()),
            attempt: 1,
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "action",
                "attempt",
                "duplicate_risk",
                "lease_id",
                "reason",
                "task_id",
                "type",
            ],
            "AuditKind::A2ARepairApplied wire form must be exactly seven keys: the six variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("a2_a_repair_applied")),
            "AuditKind discriminator slug must be 'a2_a_repair_applied' — serde's rename_all = snake_case splits the 'A2A' prefix on each digit/uppercase boundary, producing 'a2_a_…'. This is the durable wire form every persisted A2ARepairApplied audit row uses; a refactor that 'fixed' the slug to 'a2a_repair_applied' would silently strand every prior A2A-lease-repair audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::A2ARepairApplied must round-trip through serde_json verbatim — the PartialEq derive is the contract A2A-lease-repair audit triage joins on",
        );

        // Omission-rejection only walks the four strictly-required
        // fields; lease_id and duplicate_risk are Option and serde
        // accepts a missing key as None per stdlib semantics. The
        // null-on-wire assertion below is what pins the
        // skip_serializing_if regression for those two fields.
        for required in ["task_id", "action", "reason", "attempt"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::A2ARepairApplied wire form must reject a payload missing {required:?}; a stray #[serde(default)] on task_id would erase the unforgeable target identifier, and a default on attempt would mask the retry-count signal that distinguishes a first repair from a re-repair",
            );
        }

        let none_case = AuditKind::A2ARepairApplied {
            task_id: Uuid::nil(),
            action: "force_error".into(),
            reason: "operator-aborted lease".into(),
            lease_id: None,
            duplicate_risk: None,
            attempt: 2,
        };
        let wire_none = serde_json::to_value(&none_case).unwrap();
        let obj_none = wire_none.as_object().unwrap();
        assert_eq!(
            obj_none.get("lease_id"),
            Some(&serde_json::Value::Null),
            "lease_id: None must surface as JSON null — the field has no #[serde(skip_serializing_if)] so the wire shape stays stable across Some and None repair rows",
        );
        assert_eq!(
            obj_none.get("duplicate_risk"),
            Some(&serde_json::Value::Null),
            "duplicate_risk: None must surface as JSON null — the field has no #[serde(skip_serializing_if)] so the wire shape stays stable across Some and None repair rows",
        );
        assert_eq!(
            obj_none.len(),
            7,
            "AuditKind::A2ARepairApplied with lease_id=None and duplicate_risk=None must still surface seven keys on the wire; a skip_serializing_if regression would silently shrink the wire form for the None case",
        );
    }

    #[test]
    fn audit_kind_settlement_receipt_backfill_applied_serde_pins_three_field_variant() {
        // AuditKind::SettlementReceiptBackfillApplied is the audit row
        // covenantd emits after the settlement receipt backfill mutator
        // returns. Three fields: row_count (u64), rollback_path
        // (Option<String>), dry_run (bool). Integrity reports and replay
        // join on the durable slug; a rename would silently strand every
        // prior backfill row at decode time. rollback_path carries no
        // #[serde(skip_serializing_if)] so the key surfaces as null on a
        // dry run or a no-op apply — a consumer that filters on the
        // applied-vs-dry split reads dry_run, and one that wants the
        // rollback checkpoint reads rollback_path, so both must stay on
        // the wire across the Some and None cases.
        let kind = AuditKind::SettlementReceiptBackfillApplied {
            row_count: 3,
            rollback_path: Some("/home/op/receipts/working.jsonl.bak".into()),
            dry_run: false,
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["dry_run", "rollback_path", "row_count", "type"],
            "AuditKind::SettlementReceiptBackfillApplied wire form must be exactly four keys: the three variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("settlement_receipt_backfill_applied")),
            "AuditKind discriminator slug must be snake_case 'settlement_receipt_backfill_applied'; a rename would strand every prior backfill audit row at decode time and break integrity replay",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::SettlementReceiptBackfillApplied must round-trip through serde_json verbatim",
        );

        // row_count and dry_run are strictly required; rollback_path is
        // Option and serde decodes a missing key as None, so it is
        // intentionally absent from the omission walk. The null-on-wire
        // round-trip below pins the skip_serializing_if regression for
        // rollback_path; a #[serde(default)] on row_count would let a
        // backfill row decode claiming zero changed rows, and a default
        // on dry_run would let an applied rewrite masquerade as a dry run.
        for required in ["row_count", "dry_run"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::SettlementReceiptBackfillApplied wire form must reject a payload missing {required:?}",
            );
        }

        let dry_run_kind = AuditKind::SettlementReceiptBackfillApplied {
            row_count: 2,
            rollback_path: None,
            dry_run: true,
        };
        let dry_wire = serde_json::to_value(&dry_run_kind).unwrap();
        let dry_obj = dry_wire.as_object().unwrap();
        assert_eq!(
            dry_obj.get("rollback_path"),
            Some(&serde_json::Value::Null),
            "rollback_path: None must surface as JSON null — the field has no #[serde(skip_serializing_if)] so the wire shape stays stable across dry-run/no-op and applied rows",
        );
        assert_eq!(
            dry_obj.len(),
            4,
            "AuditKind::SettlementReceiptBackfillApplied with rollback_path=None must still surface four keys on the wire",
        );
        let back: AuditKind = serde_json::from_value(dry_wire).unwrap();
        assert_eq!(back, dry_run_kind);
    }

    #[test]
    fn audit_kind_memory_record_backfill_applied_serde_pins_three_field_variant() {
        // AuditKind::MemoryRecordBackfillApplied is the audit row covenantd
        // will emit after the memory-record receipt-correlation backfill
        // mutator returns. Three fields: row_count (u64), savepoint_name
        // (Option<String>), dry_run (bool). The shape mirrors
        // SettlementReceiptBackfillApplied so the operator's audit
        // dashboards can JOIN both backfill families under a stable column
        // set. Integrity reports and replay join on the durable slug; a
        // rename would silently strand every prior memory backfill row at
        // decode time. savepoint_name carries no
        // #[serde(skip_serializing_if)] so the key surfaces as null on a
        // dry run or no-op apply — a consumer that filters on
        // applied-vs-dry reads dry_run, and one that wants the SAVEPOINT
        // identifier reads savepoint_name, so both must stay on the wire
        // across Some and None cases.
        let kind = AuditKind::MemoryRecordBackfillApplied {
            row_count: 3,
            savepoint_name: Some("backfill_receipt_correlation".into()),
            dry_run: false,
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["dry_run", "row_count", "savepoint_name", "type"],
            "AuditKind::MemoryRecordBackfillApplied wire form must be exactly four keys: the three variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("memory_record_backfill_applied")),
            "AuditKind discriminator slug must be snake_case 'memory_record_backfill_applied'; a rename (e.g., shortened to 'memory_backfill_applied') would strand every prior backfill audit row at decode time and break integrity replay",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::MemoryRecordBackfillApplied must round-trip through serde_json verbatim",
        );

        // row_count and dry_run are strictly required; savepoint_name is
        // Option and serde decodes a missing key as None, so it is
        // intentionally absent from the omission walk. The null-on-wire
        // round-trip below pins the skip_serializing_if regression for
        // savepoint_name; a #[serde(default)] on row_count would let a
        // backfill row decode claiming zero changed rows, and a default
        // on dry_run would let an applied rewrite masquerade as a dry run.
        for required in ["row_count", "dry_run"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::MemoryRecordBackfillApplied wire form must reject a payload missing {required:?}",
            );
        }

        let dry_run_kind = AuditKind::MemoryRecordBackfillApplied {
            row_count: 2,
            savepoint_name: None,
            dry_run: true,
        };
        let dry_wire = serde_json::to_value(&dry_run_kind).unwrap();
        let dry_obj = dry_wire.as_object().unwrap();
        assert_eq!(
            dry_obj.get("savepoint_name"),
            Some(&serde_json::Value::Null),
            "savepoint_name: None must surface as JSON null — the field has no #[serde(skip_serializing_if)] so the wire shape stays stable across dry-run/no-op and applied rows",
        );
        assert_eq!(
            dry_obj.len(),
            4,
            "AuditKind::MemoryRecordBackfillApplied with savepoint_name=None must still surface four keys on the wire",
        );
        let back: AuditKind = serde_json::from_value(dry_wire).unwrap();
        assert_eq!(back, dry_run_kind);
    }

    #[test]
    fn audit_kind_a2a_auto_retry_scheduler_scan_serde_pins_ten_field_variant() {
        // AuditKind::A2AAutoRetrySchedulerScan is the summary audit row
        // covenantd's disabled-by-default scheduler emits after each
        // automatic A2A retry scan. Requeued tasks still get individual
        // A2ARepairApplied rows; this row makes skipped and rejected
        // scheduler runs visible without duplicating task payloads.
        // Ten fields: enabled (bool), considered (u64), requeued (u64),
        // skipped (u64), skipped_by_reason (BTreeMap<String, u64>),
        // min_lease_age_ms (u64), max_attempts (u32), max_requeues
        // (u64), scan_limit (u64), error (Option<String>). The error
        // field has no #[serde(skip_serializing_if)] so the wire shape
        // must surface the key as null on success scans; a regression
        // there would shrink the wire form for success vs. failure
        // cases. The durable slug is 'a2_a_auto_retry_scheduler_scan'
        // per the serde rename_all = snake_case digit/upper split.
        let mut skipped_by_reason = BTreeMap::new();
        skipped_by_reason.insert("max_attempts".into(), 5);
        skipped_by_reason.insert("lease_age".into(), 3);
        let kind = AuditKind::A2AAutoRetrySchedulerScan {
            enabled: true,
            considered: 10,
            requeued: 2,
            skipped: 8,
            skipped_by_reason: skipped_by_reason.clone(),
            min_lease_age_ms: 30_000,
            max_attempts: 3,
            max_requeues: 100,
            scan_limit: 200,
            error: Some("lock contention".into()),
        };

        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "considered",
                "enabled",
                "error",
                "max_attempts",
                "max_requeues",
                "min_lease_age_ms",
                "requeued",
                "scan_limit",
                "skipped",
                "skipped_by_reason",
                "type",
            ],
            "AuditKind::A2AAutoRetrySchedulerScan wire form must be exactly eleven keys: the ten variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("a2_a_auto_retry_scheduler_scan")),
            "AuditKind discriminator slug must be 'a2_a_auto_retry_scheduler_scan' — serde's rename_all = snake_case splits the 'A2A' prefix on each digit/uppercase boundary, producing 'a2_a_…'. This is the durable wire form every persisted A2AAutoRetrySchedulerScan summary row uses; a refactor that 'fixed' the slug to 'a2a_auto_retry_scheduler_scan' would silently strand every prior scheduler-summary audit row at decode time",
        );

        let back: AuditKind = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::A2AAutoRetrySchedulerScan must round-trip through serde_json verbatim — the PartialEq derive is the contract retry-scheduler observability joins on",
        );

        // Omission-rejection only walks the nine strictly-required
        // fields; error is Option<String> and serde accepts a missing
        // key as None. The null-on-wire assertion below is what pins
        // the skip_serializing_if regression for the error field.
        for required in [
            "enabled",
            "considered",
            "requeued",
            "skipped",
            "skipped_by_reason",
            "min_lease_age_ms",
            "max_attempts",
            "max_requeues",
            "scan_limit",
        ] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<AuditKind>(serde_json::Value::Object(missing)).is_err(),
                "AuditKind::A2AAutoRetrySchedulerScan wire form must reject a payload missing {required:?}; a stray #[serde(default)] on skipped_by_reason would let a malformed row decode with an empty BTreeMap and erase the per-reason breakdown distinguishing operator-misconfig from policy gates",
            );
        }

        let success_case = AuditKind::A2AAutoRetrySchedulerScan {
            enabled: true,
            considered: 5,
            requeued: 5,
            skipped: 0,
            skipped_by_reason: BTreeMap::new(),
            min_lease_age_ms: 30_000,
            max_attempts: 3,
            max_requeues: 100,
            scan_limit: 200,
            error: None,
        };
        let wire_success = serde_json::to_value(&success_case).unwrap();
        let obj_success = wire_success.as_object().unwrap();
        assert_eq!(
            obj_success.get("error"),
            Some(&serde_json::Value::Null),
            "error: None must surface as JSON null — the field has no #[serde(skip_serializing_if)] so the wire shape stays stable across success and failure scheduler scans",
        );
        assert_eq!(
            obj_success.len(),
            11,
            "AuditKind::A2AAutoRetrySchedulerScan with error=None must still surface eleven keys on the wire; a skip_serializing_if regression would silently shrink the wire form for success scans",
        );
    }

    fn dated(ts: u64) -> AuditEvent {
        AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: ts,
            issuer: AgentId::new("user@local", [0u8; 32]),
            kind: intent_kind("ok"),
        }
    }

    #[tokio::test]
    async fn in_memory_purge_drops_old_events_and_keeps_new() {
        let log = InMemoryAuditLog::new();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();
        let purged = log.purge_older_than(250).await.unwrap();
        assert_eq!(purged, 2);
        let remaining = log.recent(10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].timestamp_ms, 300);
    }

    #[tokio::test]
    async fn in_memory_purge_older_than_pins_cutoff_equality_keep_arm() {
        // covenant_audit::InMemoryAuditLog::purge_older_than:
        //
        //   async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
        //       let mut g = self.events.lock().await;
        //       let len_before = g.len();
        //       g.retain(|e| e.timestamp_ms >= before_ms);
        //       Ok((len_before - g.len()) as u64)
        //   }
        //
        // The retain predicate uses `>=`, so records with timestamp
        // EXACTLY equal to before_ms are KEPT — the function's
        // 'older_than' name documents 'older' as STRICTLY less than the
        // cutoff. in_memory_purge_drops_old_events_and_keeps_new
        // records timestamps 100/200/300 with cutoff=250 — no
        // event sits at the cutoff, so the equality arm is exercised
        // by zero tests.
        //
        // A refactor that flipped `>=` to `>` (or rewrote the
        // predicate to 'e.timestamp_ms > before_ms') under a 'purge
        // older OR EQUAL TO before_ms' rereading would silently shift
        // every cutoff-equal record from kept to purged. Operator
        // dashboards running a daily purge with cutoff aligned to a
        // calendar-tick boundary would lose records emitted exactly on
        // that boundary tick on every cycle; the existing strict-less-
        // than test would still pass, and audit chain integrity reports
        // would still verify because the purge is an in-place vec
        // mutation, not a chain rewrite. Pin BOTH the cutoff-equality
        // keep arm AND the strict-less-than purge arm in one test so a
        // coordinated rewrite that swaps both halves at once cannot
        // land silently.

        let log = InMemoryAuditLog::new();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();

        // Phase 1: cutoff=200 puts the boundary on the middle event.
        // - 100 is strictly less than 200 -> PURGED
        // - 200 is equal to 200          -> KEPT (the equality arm)
        // - 300 is strictly greater      -> KEPT
        let purged = log.purge_older_than(200).await.unwrap();
        assert_eq!(
            purged, 1,
            "cutoff=200 must purge exactly the 100-stamped event — the \
             200-stamped event sits at the cutoff and the `>=` predicate \
             keeps it; a refactor that flipped to `>` would purge BOTH \
             100 and 200, returning 2 here and silently losing every \
             cutoff-equal record on every daily purge cycle. got: {purged}",
        );
        let remaining = log.recent(10).await.unwrap();
        let mut timestamps: Vec<u64> = remaining.iter().map(|e| e.timestamp_ms).collect();
        timestamps.sort();
        assert_eq!(
            timestamps,
            vec![200, 300],
            "after cutoff=200 the survivors must be the cutoff-equal \
             event (200) and the strictly-greater event (300); a \
             refactor that purged cutoff-equal records would leave only \
             [300] here, and a refactor that inverted the predicate \
             would leave only [100]. Pinning the explicit survivor set \
             forecloses any single-direction predicate flip",
        );

        // Phase 2: re-purge with cutoff=300 puts the boundary on the
        // remaining higher-value event.
        // - 200 (still present)         -> strictly less than 300 -> PURGED
        // - 300                         -> equal to 300          -> KEPT
        let purged = log.purge_older_than(300).await.unwrap();
        assert_eq!(
            purged, 1,
            "re-purge with cutoff=300 must drop the now-strictly-less \
             200-stamped event while keeping the cutoff-equal 300; \
             confirms the equality arm survives a SECOND purge cycle \
             with the boundary moved to a different timestamp, which \
             pins that the arm is invariant to cutoff value (not \
             coincidentally satisfied by the phase-1 fixture)",
        );
        let remaining = log.recent(10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].timestamp_ms, 300,
            "the survivor must be the 300-stamped event; cross-binds \
             the cutoff-equality contract on a different timestamp \
             value than phase 1 so a refactor that hardcoded the \
             equality arm to a specific value (e.g., `before_ms == 200`) \
             during a misguided 'inline this constant' pass would \
             surface here",
        );
    }

    #[tokio::test]
    async fn jsonl_purge_older_than_pins_cutoff_equality_keep_arm() {
        // covenant_audit::JsonlAuditLog::purge_older_than (line 548-)
        // keeps records via the same '>= before_ms' predicate as
        // InMemoryAuditLog: events at the EXACT cutoff are RETAINED,
        // strictly-older events are PURGED. The atomic-rewrite path
        // (tempfile + rename) persists the result to disk, so a
        // cutoff-equality flip would silently lose the equal-stamped
        // event on every daemon restart that consumes the rewritten
        // JSONL log.
        //
        // jsonl_purge_rewrites_only_when_something_drops
        // uses cutoff=150 against events at 100/200/300 — no event
        // sits at the cutoff. jsonl_purge_no_op_when_nothing_old
        // uses cutoff=50 — also no boundary case. The
        // in_memory_purge_older_than_pins_cutoff_equality_keep_arm
        // sibling (added in an earlier autonomous slice) pins the
        // equality arm on the InMemory backend; this pin mirrors that
        // contract on the Jsonl surface so a refactor that lifts the
        // predicate into a shared helper at one boundary cannot drift
        // relative to the other without surfacing on at least one
        // boundary pin.
        //
        // The pin also re-opens the log via a second JsonlAuditLog
        // handle, which forces the read-back through the atomic-
        // rewrite path's persisted state — the equality survival has
        // to be observable AFTER the rewrite lands on disk, not just
        // in the in-process state. verify_integrity is asserted at
        // the end to confirm the chain rewrite is consistent with the
        // purged-and-equality-kept event set.

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();

        // cutoff=200 places the boundary on the middle event:
        //   100 strictly less -> PURGED
        //   200 equal          -> KEPT (the equality arm)
        //   300 strictly more  -> KEPT
        let purged = log.purge_older_than(200).await.unwrap();
        assert_eq!(
            purged, 1,
            "Jsonl cutoff=200 must purge only the 100-stamped event \
             (strictly less than 200); the 200-stamped event sits at \
             the cutoff and the `>= before_ms` predicate keeps it. A \
             refactor that flipped `>=` to `>` in the kept-predicate \
             would purge BOTH 100 and 200, return 2 here, AND persist \
             the regression to disk via the atomic rewrite — the next \
             daemon restart would consume the truncated log with no \
             signal that an event was silently lost. got: {purged}"
        );

        // Re-open via a second handle to force the survivors through
        // the persisted-state read path; this proves the equality arm
        // survives the tempfile+rename atomic-rewrite roundtrip, not
        // just the in-process Vec retain.
        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        let mut kept_ts: Vec<u64> = log2
            .recent(10)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.timestamp_ms)
            .collect();
        kept_ts.sort();
        assert_eq!(
            kept_ts,
            vec![200, 300],
            "after cutoff=200 the persisted survivors must be the \
             cutoff-equal event (200) and the strictly-greater event \
             (300); the explicit survivor list catches both a strict-\
             greater-only refactor (would leave [300]) and an \
             inversion (would leave [100])",
        );

        // The chain rewrite must be consistent with the purge: every
        // surviving event has a fresh anchor and the chain hashes
        // verify against the new sequence. A refactor that purged the
        // event but skipped its chain anchor (or vice versa) would
        // surface here as verify_integrity reporting invalid=false or
        // mismatched event/anchor counts.
        let report = log2.verify_integrity().await.unwrap();
        assert!(
            report.valid,
            "verify_integrity must report valid=true after a boundary-\
             keeping purge — the chain rewrite re-anchors every \
             surviving event and the integrity check must agree with \
             the new sequence; if this fails the atomic-rewrite path \
             dropped or skipped an anchor for the cutoff-equal event. \
             report: {report:?}",
        );
        assert_eq!(report.events, 2);
        assert_eq!(report.anchors, 2);
    }

    #[tokio::test]
    async fn jsonl_purge_rewrites_only_when_something_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();

        let purged = log.purge_older_than(150).await.unwrap();
        assert_eq!(purged, 1);
        // Re-open to confirm the rewrite landed on disk and the survivors
        // can still be parsed back.
        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        let kept = log2.recent(10).await.unwrap();
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|e| e.timestamp_ms >= 150));
        let report = log2.verify_integrity().await.unwrap();
        assert!(report.valid, "{report:?}");
        assert_eq!(report.events, 2);
        assert_eq!(report.anchors, 2);
    }

    #[tokio::test]
    async fn jsonl_purge_no_op_when_nothing_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        let purged = log.purge_older_than(50).await.unwrap();
        assert_eq!(purged, 0);
        // No tempfile.tmp left lying around — atomic-rename path skipped.
        assert!(!path.with_extension("jsonl.tmp").exists());
        let kept = log.recent(10).await.unwrap();
        assert_eq!(kept.len(), 2);
    }

    #[tokio::test]
    async fn jsonl_purge_on_missing_file_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(log.purge_older_than(1_000_000).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn jsonl_purge_self_heals_chain_first_crash_window_orphan_events_shape() {
        // Crash-simulation test for audit-purge-atomicity.
        //
        // purge_older_than now writes the CHAIN file first (tmp + rename)
        // and only then writes EVENTS (tmp + rename). A power-loss between
        // the two renames leaves the chain at K rows and events still at N
        // rows (N > K) — the "orphan events" shape. record() refuses
        // (ChainCorruption — the security-correct boundary so an attacker
        // can't trick rebuild into accepting forged events), but rerunning
        // purge_older_than with the same cutoff re-derives the same K
        // kept events, rewrites the chain idempotently, and renames events
        // to match.
        //
        // The reverse ordering (events-first) would leave events < chain on
        // crash — a shape indistinguishable from an attacker who truncated
        // events under a valid chain, so it would be unrecoverable without
        // operator action. This test pins the recoverability of the
        // chain-first shape and would fail if a future refactor swapped the
        // order back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let chain_path = path.with_extension("chain.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();

        // Snapshot the steady-state events file BEFORE the purge so we can
        // restore it afterwards — the restore reproduces the on-disk shape
        // a crash between the chain rename and the events rename would
        // leave behind (chain at the new shorter body, events still at the
        // old longer body).
        let pre_purge_events = std::fs::read_to_string(&path).unwrap();

        let purged = log.purge_older_than(150).await.unwrap();
        assert_eq!(purged, 1);

        // Idempotency pin: snapshot the chain bytes the original purge
        // produced so we can assert the rerun self-heal yields BYTE-
        // IDENTICAL chain content. A refactor that re-derived a different
        // chain on the rerun (e.g., by changing the index column or
        // chain-hash chaining) would silently break external verifiers
        // that re-read the chain after a recovery.
        let post_purge_chain = std::fs::read_to_string(&chain_path).unwrap();

        // Reproduce the crash window: chain has K=2 rows (already on disk
        // from the successful rename), events file is rewound to N=3 rows
        // (the rename that the simulated crash prevented).
        std::fs::write(&path, &pre_purge_events).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            3,
            "events file restored to N=3 rows (the orphan-events crash shape)",
        );
        assert_eq!(
            std::fs::read_to_string(&chain_path)
                .unwrap()
                .lines()
                .count(),
            2,
            "chain file remains at K=2 rows after the simulated crash",
        );

        // record() must refuse the orphan-events state — that is the
        // security-correct boundary that this whole subsystem exists to
        // protect, and the security-policy hint "refusing to rebuild"
        // pinned by audit_error_chain_corruption_display_message_pins_*
        // is the operator-facing signal that this refusal is intentional.
        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        match log2.record(dated(400)).await {
            Err(AuditError::ChainCorruption { events, chain }) => {
                assert_eq!(
                    events, 3,
                    "ChainCorruption.events reports the orphan-events row count"
                );
                assert_eq!(
                    chain, 2,
                    "ChainCorruption.chain reports the post-rename chain row count"
                );
            }
            other => panic!("expected ChainCorruption on orphan-events shape, got {other:?}"),
        }

        // Rerun purge with the same cutoff — the documented self-heal.
        // It re-derives the same K=2 kept events from the read-events of
        // the restored events file, rewrites the chain to the same body
        // (idempotent), and renames events to match.
        let recovered = log2.purge_older_than(150).await.unwrap();
        assert_eq!(
            recovered, 1,
            "self-heal purges the same orphan that the original purge would have dropped"
        );

        // Idempotency assertion: the chain file after self-heal must
        // be byte-identical to the chain file the original purge
        // produced. If a refactor changed chain-derivation between
        // calls (e.g., a non-deterministic field, a timestamp added,
        // a chain-hash seed perturbed) this would catch the drift.
        let post_heal_chain = std::fs::read_to_string(&chain_path).unwrap();
        assert_eq!(
            post_heal_chain, post_purge_chain,
            "self-heal must produce the same chain bytes the original purge produced — idempotency",
        );

        // After recovery, the log is consistent and record() works again.
        log2.record(dated(400))
            .await
            .expect("record after self-heal");
        let report = log2.verify_integrity().await.unwrap();
        assert!(
            report.valid,
            "verify_integrity must pass after self-heal: {report:?}"
        );
        assert_eq!(
            report.events, 3,
            "post-heal events: 200, 300, plus the new 400"
        );
        assert_eq!(report.anchors, 3, "post-heal chain matches events length");
    }

    #[tokio::test]
    async fn jsonl_record_refuses_events_lt_chain_shape_so_events_first_ordering_would_be_unsafe() {
        // Asymmetry pin for audit-purge-atomicity. The chain-first
        // ordering in purge_older_than is the safe choice because the
        // REVERSE crash shape — events file shorter than the chain file
        // — is indistinguishable from an attacker who truncated events
        // under a valid chain (e.g., to drop a damning row), and the
        // safe response is operator-mediated restore from a trusted
        // backup, not silent self-heal. This test reproduces that
        // dangerous shape directly and verifies record() refuses with
        // ChainCorruption — without that refusal, the events-first
        // ordering would be silently exploitable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let chain_path = path.with_extension("chain.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();

        // Truncate events to 2 rows; chain remains at 3. This is the
        // shape a hypothetical events-first ordering would leave on a
        // crash between the events rename and the chain rewrite.
        let events_raw = std::fs::read_to_string(&path).unwrap();
        let mut keep_lines = events_raw.lines().take(2).collect::<Vec<_>>().join("\n");
        keep_lines.push('\n');
        std::fs::write(&path, keep_lines).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(
            std::fs::read_to_string(&chain_path)
                .unwrap()
                .lines()
                .count(),
            3,
        );

        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        match log2.record(dated(400)).await {
            Err(AuditError::ChainCorruption { events, chain }) => {
                assert_eq!(events, 2, "events count reflects the truncated body");
                assert_eq!(chain, 3, "chain count reflects the unchanged body");
                assert!(
                    events < chain,
                    "the dangerous shape — chain claims more events than the events file holds — must refuse, not self-heal",
                );
            }
            other => panic!("expected ChainCorruption for events<chain, got {other:?}"),
        }
    }

    fn pin_audit_variant(kind: AuditKind, slug: &str, expected_keys: &[&str]) {
        let wire = serde_json::to_value(&kind).unwrap();
        let obj = wire
            .as_object()
            .expect("AuditKind serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        let mut expected: Vec<&str> = expected_keys.iter().copied().chain(["type"]).collect();
        expected.sort();
        assert_eq!(
            keys, expected,
            "AuditKind::{slug} wire form must be exactly the listed keys plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!(slug)),
            "AuditKind discriminator slug must be snake_case {slug:?}",
        );
        let back: AuditKind = serde_json::from_value(wire).unwrap();
        assert_eq!(
            back, kind,
            "AuditKind::{slug} must round-trip through serde_json verbatim",
        );
    }

    #[test]
    fn audit_kind_hermes_tool_invoked_serde_pins_four_field_variant() {
        // Drives the audit fold for HermesRunner. Each tool start
        // becomes one audit row carrying the run id, tool name, and a
        // SHA-256 of the short tool-input preview — raw preview must
        // never appear in the chain.
        pin_audit_variant(
            AuditKind::HermesToolInvoked {
                intent_id: Uuid::nil(),
                run_id: "run_abc".into(),
                tool: "terminal".into(),
                preview_hash_hex: "deadbeef".into(),
            },
            "hermes_tool_invoked",
            &["intent_id", "preview_hash_hex", "run_id", "tool"],
        );
    }

    #[test]
    fn audit_kind_hermes_tool_completed_serde_pins_five_field_variant() {
        pin_audit_variant(
            AuditKind::HermesToolCompleted {
                intent_id: Uuid::nil(),
                run_id: "run_abc".into(),
                tool: "terminal".into(),
                duration_ms: 42,
                error: true,
            },
            "hermes_tool_completed",
            &["duration_ms", "error", "intent_id", "run_id", "tool"],
        );
    }

    #[test]
    fn audit_kind_hermes_approval_requested_serde_pins_three_field_variant() {
        pin_audit_variant(
            AuditKind::HermesApprovalRequested {
                intent_id: Uuid::nil(),
                run_id: "run_abc".into(),
                choices: vec!["once".into(), "deny".into()],
            },
            "hermes_approval_requested",
            &["choices", "intent_id", "run_id"],
        );
    }

    #[test]
    fn audit_kind_hermes_approval_resolved_serde_pins_four_field_variant() {
        pin_audit_variant(
            AuditKind::HermesApprovalResolved {
                intent_id: Uuid::nil(),
                run_id: "run_abc".into(),
                choice: "once".into(),
                resolved: 3u64,
            },
            "hermes_approval_resolved",
            &["choice", "intent_id", "resolved", "run_id"],
        );
    }

    #[test]
    fn audit_kind_hermes_file_written_serde_pins_four_field_variant() {
        pin_audit_variant(
            AuditKind::HermesFileWritten {
                intent_id: Uuid::nil(),
                run_id: "run_abc".into(),
                path: "src/main.rs".into(),
                bytes: 1_024u64,
            },
            "hermes_file_written",
            &["bytes", "intent_id", "path", "run_id"],
        );
    }

    #[test]
    fn audit_kind_skill_installed_serde_pins_six_field_variant() {
        pin_audit_variant(
            AuditKind::SkillInstalled {
                name: "covenant".into(),
                version: "0.1.0".into(),
                digest_hex: "deadbeef".into(),
                source_url: "https://github.com/open-covenant/covenant-skill/tree/v0.1.0/skill"
                    .into(),
                source_tag: "v0.1.0".into(),
                source_commit: "0".repeat(40),
            },
            "skill_installed",
            &[
                "digest_hex",
                "name",
                "source_commit",
                "source_tag",
                "source_url",
                "version",
            ],
        );
    }

    #[test]
    fn audit_kind_skill_context_injected_serde_pins_three_field_variant() {
        pin_audit_variant(
            AuditKind::SkillContextInjected {
                skill_name: "covenant".into(),
                skill_digest_hex: "deadbeef".into(),
                references: vec!["identity-capabilities.md".into(), "audit-witness.md".into()],
            },
            "skill_context_injected",
            &["references", "skill_digest_hex", "skill_name"],
        );
    }

    #[test]
    fn audit_kind_skill_invoked_serde_pins_two_field_variant() {
        pin_audit_variant(
            AuditKind::SkillInvoked {
                skill_name: "covenant".into(),
                intent_id: Uuid::nil(),
            },
            "skill_invoked",
            &["intent_id", "skill_name"],
        );
    }

    #[test]
    fn audit_kind_skill_tx_proposed_serde_pins_five_field_variant() {
        pin_audit_variant(
            AuditKind::SkillTxProposed {
                skill_name: "covenant".into(),
                program: "11111111111111111111111111111111".into(),
                instruction: "transfer".into(),
                accounts_hash_hex: "deadbeef".into(),
                simulated_ok: true,
            },
            "skill_tx_proposed",
            &[
                "accounts_hash_hex",
                "instruction",
                "program",
                "simulated_ok",
                "skill_name",
            ],
        );
    }

    #[test]
    fn audit_kind_skill_tx_signed_serde_pins_two_field_variant() {
        pin_audit_variant(
            AuditKind::SkillTxSigned {
                skill_name: "covenant".into(),
                signature_b58: "z".repeat(88),
            },
            "skill_tx_signed",
            &["signature_b58", "skill_name"],
        );
    }

    #[test]
    fn audit_kind_skill_refused_serde_pins_two_field_variant() {
        pin_audit_variant(
            AuditKind::SkillRefused {
                skill_name: "covenant".into(),
                reason: "missing skill.use.covenant".into(),
            },
            "skill_refused",
            &["reason", "skill_name"],
        );
    }

    #[test]
    fn audit_kind_untrusted_input_observed_serde_pins_two_field_variant() {
        pin_audit_variant(
            AuditKind::UntrustedInputObserved {
                source: "rpc:account_data:11111111111111111111111111111111".into(),
                digest_hex: "deadbeef".into(),
            },
            "untrusted_input_observed",
            &["digest_hex", "source"],
        );
    }

    #[test]
    fn zero_chain_hash_pins_64_char_all_zero_string() {
        // ZERO_CHAIN_HASH is the genesis seed every audit
        // hash chain uses for its first event's previous_hash_hex.
        // The value '0' * 64 matches the conventional zero/genesis
        // form that Bitcoin's coinbase parent hash, audit-log replay
        // tools, and any independent SHA-256 verifier expect when
        // seeding the chain replay from the first event.
        // build_chain_entries, the JsonlAuditLog::verify
        // path, and the root_hash_hex defaulting paths
        // all reference this constant.
        //
        // chain_hash_pins_separator_and_sha256_composition (sibling)
        // uses ZERO_CHAIN_HASH on BOTH sides of its
        // composition assertion, so the constant's value is a
        // reference, not a target — a refactor that changed
        // ZERO_CHAIN_HASH to a different 64-char string (e.g., the
        // SHA-256 of empty input 'e3b0c44...' under a 'use a
        // meaningful genesis' rationale, or 'f' * 64 under an
        // 'anti-zero genesis' rationale) would make both sides drift
        // together and the existing pin would silently pass while
        // every operator's persisted audit chain became unreplayable
        // with external tools.

        assert_eq!(
            ZERO_CHAIN_HASH, "0000000000000000000000000000000000000000000000000000000000000000",
            "ZERO_CHAIN_HASH must remain the literal 64-character \
             all-zero string. Operators running independent SHA-256 \
             replay tools (the documented external-verification \
             contract documented at sha256_hex_pins_nist_vectors_and_lowercase_output) \
             seed their chain replay with this exact value; a \
             refactor that changed it under any rationale would \
             silently shift the chain's genesis and produce mismatches \
             at index 0 of every operator's audit JSONL",
        );
        assert_eq!(
            ZERO_CHAIN_HASH.len(),
            64,
            "ZERO_CHAIN_HASH must remain exactly 64 hex characters \
             (32 bytes * 2 nibbles per byte) — the SHA-256 output \
             length. A refactor that swapped the underlying digest \
             to SHA-1 (40 hex chars) or SHA-512 (128 hex chars) \
             would require this length to change in lockstep; the \
             length pin surfaces the algorithm swap before external \
             verifiers silently desync",
        );
        assert!(
            ZERO_CHAIN_HASH.chars().all(|c| c == '0'),
            "ZERO_CHAIN_HASH must contain only the ASCII digit '0' \
             — a refactor that introduced a '0x' prefix for hex-string \
             consistency with Solana/Ethereum conventions, uppercased \
             to '0X' or 'F', or substituted the letter 'O' for the \
             digit would silently shift the canonical format. The \
             all-chars-zero pin cross-binds the lowercase-hex contract \
             pinned by sha256_hex_pins_nist_vectors_and_lowercase_output",
        );
    }

    #[test]
    fn audit_error_io_and_serde_display_messages_pin_prefix_and_external_source_display_delegation()
    {
        let io_err = AuditError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "events.jsonl missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "AuditError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish events/chain file disk faults from JSON-parse faults and from the security-relevant chain-corruption surface (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("events.jsonl missing"),
            "AuditError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: NotFound, error: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "AuditError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would leak internal struct fields (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = AuditError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "AuditError::Serde must surface the literal 'serde: ' bootstrap-stage prefix so audit-log filters can distinguish JSON-parse faults on stored rows from disk faults and from chain-corruption (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "AuditError::Serde must surface the inner serde_json::Error Display rendering after the colon (serde_json renders parse failures with 'expected ...' Display strings); a Debug refactor on {{0}} would render 'Error(\"...\", line: N, column: M)' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "AuditError::Serde must NOT surface the serde_json::Error Debug rendering; a Debug refactor on {{0}} would expose 'Error(\"...\", line: N, column: M)' buffer-position structs (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "AuditError::Io and AuditError::Serde Display must not converge; merging the two prefixes would lose the disk-fault vs JSON-parse-fault discriminator (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert!(
            !io_message.starts_with("serde:") && !serde_message.starts_with("io:"),
            "AuditError::Io must not start with 'serde:' and AuditError::Serde must not start with 'io:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): io={io_message} serde={serde_message}"
        );
        assert!(
            !io_message.starts_with("chain corruption:"),
            "AuditError::Io must not converge with AuditError::ChainCorruption 'chain corruption:' prefix; a disk fault must not be mis-routed as a security-relevant chain-tampering surface (ChainCorruption-convergence regression class): {io_message}"
        );
        assert!(
            !serde_message.starts_with("chain corruption:"),
            "AuditError::Serde must not converge with AuditError::ChainCorruption 'chain corruption:' prefix; a JSON-parse fault must not be mis-routed as a security-relevant chain-tampering surface (ChainCorruption-convergence regression class): {serde_message}"
        );
    }

    #[test]
    fn audit_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "events.jsonl perms");
        let expected_display = format!("{inner}");
        let err = AuditError::Io(inner);
        let source = err.source().expect(
            "AuditError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side audit-log retry-policy classifiers can downcast source() to std::io::Error and extract io::ErrorKind for distinct retry decisions (Interrupted retries immediately, WouldBlock backs off briefly, PermissionDenied escalates to operator-attention); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "AuditError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "AuditError::Io source() must downcast_ref to std::io::Error so audit-log retry-policy classifiers can extract io::ErrorKind for retry decisions; a refactor that wrapped the inner in a project-local newtype (e.g., AuditIoError(std::io::Error)) under a 'tag audit IO failures distinctly from sibling Io variants in other crates' rationale would silently break downcast_ref::<std::io::Error>() at every downstream callsite even if the wrapper's Display still surfaced the inner io::Error text (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn audit_error_serde_source_delegation_pin_returns_inner_serde_json_error_via_std_error_source()
    {
        use std::error::Error;

        let inner =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let expected_display = format!("{inner}");
        let err = AuditError::Serde(inner);
        let source = err.source().expect(
            "AuditError::Serde must surface the inner serde_json::Error via std::error::Error::source so daemon-side audit-chain integrity diagnostics can walk the error chain and downcast source() to serde_json::Error to inspect line/column or classify() for malformed-event-row identification (line/column points the operator at the offending events.jsonl row, classify() distinguishes Syntax-vs-Data-vs-EOF for incident triage on a corrupted audit row); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "AuditError::Serde source() Display must match a direct format!() of the same serde_json::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "AuditError::Serde source() must downcast_ref to serde_json::Error so daemon-side audit-chain integrity diagnostics can call serde_json::Error::line/column/classify for malformed-event-row identification; a refactor that wrapped the inner in a project-local newtype (e.g., AuditSerdeError(serde_json::Error) under a 'consolidate parse errors into one Wire variant' rationale) would silently break downcast_ref::<serde_json::Error>() at every downstream callsite that classifies audit-event row parse faults (concrete-source-type downcast regression class)"
        );
    }
}
