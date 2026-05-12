//! Append-only audit log.
//!
//! Every intent dispatch, capability check, capability grant, and
//! capability revocation produces one [`AuditEvent`]. Wire format is
//! JSONL — one event per line, easy to tail or grep — and the
//! [`AuditLog`] trait abstracts over a JSONL-backed implementation
//! suitable for production and an in-memory implementation suitable
//! for tests.

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
    #[error("chain corruption: events file has {events} rows, chain file has {chain}; refusing to rebuild")]
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

async fn write_chain_entries(
    path: &PathBuf,
    entries: &[AuditChainEntry],
) -> Result<(), AuditError> {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    for entry in entries {
        let line = serde_json::to_string(entry)?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
    }
    f.flush().await?;
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
        // concurrent record can't race against the rewrite. Atomicity of
        // the rewrite comes from `tempfile + rename` — readers see either
        // the old log or the new one, never a partial rewrite.
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
        let chain_entries = build_chain_entries(&kept)?;
        write_chain_entries(&self.chain_path(), &chain_entries).await?;
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

pub fn hash_hex(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
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
    async fn jsonl_recent_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(log.recent(10).await.unwrap().is_empty());
    }

    #[test]
    fn hash_hex_is_stable_for_same_input() {
        assert_eq!(hash_hex(b"hello"), hash_hex(b"hello"));
        assert_ne!(hash_hex(b"hello"), hash_hex(b"world"));
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
        assert_eq!(
            keys,
            vec!["claimed_sender_display", "peer_display", "type"]
        );
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
}
