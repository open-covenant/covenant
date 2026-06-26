//! Receiver-side cross-host A2A admission (multi-host slice 4b-2).
//!
//! [`SignedA2ATask`](covenant_a2a::SignedA2ATask) (slice 4b-1) proves a remote
//! task's *authenticity*. This module adds the receiving daemon's *admission*
//! decision: the half that turns an authentic envelope into a task on the local
//! mailbox, or refuses it. A remote sender holds no local peer token, so this
//! path authenticates by the envelope signature rather than the bearer gate —
//! authority then comes from the known-hosts registry, the recipient's recv
//! grant, a freshness window, and a restart-durable replay cache.
//!
//! [`Server::admit_remote_a2a_task`] runs the pipeline in fail-closed order:
//! open → known-host authorization → recipient-is-self → freshness → recv-gate →
//! anti-replay claim → enqueue. Every decision emits a best-effort row on the
//! operator's audit feed (like other daemon-authored events); persisting an
//! *admission* fail-closed — so a task can never reach the mailbox without an
//! attributable record — is carried forward to slice 4b-2b with the route. The
//! value returned to the caller is deliberately coarse ([`RemoteAdmission`]) so
//! the transport that will front this method (slice 4b-2b) cannot leak which
//! stage rejected an envelope to a remote prober.
//!
//! This slice builds and exercises the pipeline but does not expose it over the
//! network — there is no externally reachable route until 4b-2b, by which point
//! the security core here has been reviewed in isolation.

use std::collections::HashMap;
use std::path::PathBuf;

use covenant_a2a::{A2AEnvelopeError, SignedA2ATask};
use covenant_audit::{AuditEvent, AuditKind};
use covenant_permissions::A2aScopeRequest;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{A2aScopeCheck, Server};

/// How far an envelope's signed `issued_at_ms` may trail "now" before it is
/// refused as stale. Bounds the replay window: a captured envelope is only
/// admissible for this long, after which freshness rejects it regardless of the
/// dedup cache. Wide enough to absorb cross-host clock skew, queueing, and a
/// sender's own retry backoff; narrow enough that the replay cache only has to
/// retain a few minutes of keys.
pub const CROSS_HOST_MAX_AGE_MS: u64 = 5 * 60 * 1000;

/// How far an envelope's signed `issued_at_ms` may lead "now" before it is
/// refused as forged-future. A small allowance for the sender's clock running
/// ahead; an envelope dated far in the future would otherwise dodge the staleness
/// floor indefinitely.
pub const CROSS_HOST_MAX_SKEW_MS: u64 = 60 * 1000;

/// The payload-identity tuple a cross-host envelope is deduplicated on.
///
/// Deliberately **not** the signature bytes: ed25519's non-strict verification
/// accepts a malleated signature over the same payload, so a signature-keyed
/// cache would let an attacker re-encode the signature and bypass it. The
/// `(sender, task id, issued_at_ms)` tuple is bound into the signed message, so
/// a replay that opens to the same task collides here no matter how its
/// signature is re-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    pub sender_pubkey_b58: String,
    pub task_id: Uuid,
    pub issued_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct DedupRecord {
    key: DedupKey,
    recorded_at_ms: u64,
}

/// Restart-durable anti-replay cache for admitted cross-host envelopes.
///
/// Keys are appended to a JSONL log under the daemon home and replayed on
/// [`open`](Self::open), so an envelope admitted before a restart is still
/// recognized as a duplicate after. [`claim_fresh`](Self::claim_fresh) is the
/// only mutation and is atomic: it durably records a key before reporting it
/// fresh, so a crash between record and enqueue can only *lose* a task, never
/// re-admit a replay. Stale keys (older than the freshness horizon, hence no
/// longer admissible anyway) are pruned and the log compacted on the next claim
/// that observes them, bounding both memory and file growth to the window.
pub struct JsonlCrossHostDedup {
    path: PathBuf,
    seen: AsyncMutex<HashMap<DedupKey, u64>>,
}

impl JsonlCrossHostDedup {
    /// Open (or create) the dedup log at `path`, replaying recorded keys. A
    /// missing file is an empty cache; a malformed line fails closed — the
    /// daemon refuses to start on a corrupt replay log rather than silently
    /// forgetting which envelopes were already admitted.
    pub async fn open(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut seen = HashMap::new();
        match tokio::fs::read_to_string(&path).await {
            Ok(raw) => {
                for line in raw.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let record: DedupRecord = serde_json::from_str(line).map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("corrupt cross-host dedup line: {e}"),
                        )
                    })?;
                    seen.insert(record.key, record.recorded_at_ms);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            path,
            seen: AsyncMutex::new(seen),
        })
    }

    /// Atomically claim `key` as freshly seen at `now_ms`. Returns `Ok(true)`
    /// when the key was newly recorded (the caller may proceed to enqueue) and
    /// `Ok(false)` when it was already present (a replay to absorb). The durable
    /// append happens before the in-memory insert, so a returned `true` is
    /// always backed by a persisted record.
    pub async fn claim_fresh(&self, key: &DedupKey, now_ms: u64) -> std::io::Result<bool> {
        let horizon = CROSS_HOST_MAX_AGE_MS + CROSS_HOST_MAX_SKEW_MS;
        let mut seen = self.seen.lock().await;
        let before = seen.len();
        seen.retain(|k, _| now_ms.saturating_sub(k.issued_at_ms) <= horizon);
        if seen.len() != before {
            self.rewrite(&seen).await?;
        }
        if seen.contains_key(key) {
            return Ok(false);
        }
        self.append(&DedupRecord {
            key: key.clone(),
            recorded_at_ms: now_ms,
        })
        .await?;
        seen.insert(key.clone(), now_ms);
        Ok(true)
    }

    async fn append(&self, record: &DedupRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(record).expect("DedupRecord serializes (plain data)");
        line.push(b'\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(&line).await?;
        file.flush().await?;
        Ok(())
    }

    async fn rewrite(&self, seen: &HashMap<DedupKey, u64>) -> std::io::Result<()> {
        let mut buf = Vec::new();
        for (key, recorded_at_ms) in seen {
            buf.extend_from_slice(
                &serde_json::to_vec(&DedupRecord {
                    key: key.clone(),
                    recorded_at_ms: *recorded_at_ms,
                })
                .expect("DedupRecord serializes (plain data)"),
            );
            buf.push(b'\n');
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        tokio::fs::write(&tmp, &buf).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }
}

/// The receiver's admission verdict for a cross-host envelope. Intentionally
/// coarse: the rejection reason stays on the audit feed, never in this value, so
/// the network front (slice 4b-2b) cannot turn the three open() error variants
/// or the later gates into a probing oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAdmission {
    /// Verified, authorized, fresh, and enqueued to the local mailbox.
    Admitted { task_id: Uuid },
    /// A replay of an already-admitted envelope within the freshness window —
    /// absorbed idempotently, not enqueued again.
    Duplicate { task_id: Uuid },
    /// Refused at some stage. The stage is on the audit feed only.
    Rejected,
}

impl Server {
    /// Admit a cross-host [`SignedA2ATask`] onto the local mailbox, or refuse it.
    ///
    /// `now_ms` is the receiver's wall clock at admission, passed in so the
    /// freshness window and dedup horizon are deterministic under test; the
    /// transport supplies `epoch_ms()`. The pipeline is fail-closed and ordered
    /// so each gate only runs on input the previous one vouched for, and so a
    /// rejection never mutates durable state a legitimate retry would need.
    pub async fn admit_remote_a2a_task(
        &self,
        envelope: SignedA2ATask,
        now_ms: u64,
    ) -> RemoteAdmission {
        // 1. Authenticity. open() parses the payload and verifies the signature
        //    against the pubkey the payload names as sender, so a returned task's
        //    sender is provably the signer. Before this point there is no trusted
        //    sender, so the audit row records an empty principal.
        let task = match envelope.open() {
            Ok(task) => task,
            Err(e) => {
                let reason = match e {
                    A2AEnvelopeError::SignatureInvalid => "signature_invalid",
                    A2AEnvelopeError::MalformedSignature => "malformed_signature",
                    A2AEnvelopeError::MalformedPayload => "malformed_payload",
                };
                self.audit_cross_host(now_ms, "", "", "rejected", reason).await;
                return RemoteAdmission::Rejected;
            }
        };
        let sender_b58 = task.sender.pubkey_base58();
        let recipient_display = task.recipient.display.clone();

        // 2. Authorization is not authenticity. The proven sender must be a
        //    known-hosts peer whose registry-bound pubkey is exactly the signing
        //    key. Unknown host, malformed id, and a registry binding to a
        //    different key all collapse to one rejection so a caller cannot
        //    distinguish "no such host" from "wrong key for this host".
        match self.known_hosts().resolve_agent(&task.sender) {
            Ok(endpoint) if endpoint.pubkey == task.sender.pubkey => {}
            _ => {
                self.audit_cross_host(
                    now_ms,
                    &sender_b58,
                    &recipient_display,
                    "rejected",
                    "unknown_principal",
                )
                .await;
                return RemoteAdmission::Rejected;
            }
        }

        // 3. Recipient-is-self. open() authenticates only the sender, so an
        //    envelope addressed to a different daemon still opens here; compare on
        //    pubkey bytes (not the wire display) against this daemon's identity.
        if task.recipient.pubkey != self.identity.agent_id().pubkey {
            self.audit_cross_host(
                now_ms,
                &sender_b58,
                &recipient_display,
                "rejected",
                "recipient_mismatch",
            )
            .await;
            return RemoteAdmission::Rejected;
        }

        // 4. Freshness. issued_at_ms is bound into the signature, so it cannot be
        //    rewritten on a captured envelope. Reject envelopes too old (stale or
        //    replayed) or too far ahead (forged-future). Saturating subtraction
        //    avoids u64 wrap at the boundaries.
        let issued_at_ms = envelope.issued_at_ms();
        if now_ms.saturating_sub(issued_at_ms) > CROSS_HOST_MAX_AGE_MS {
            self.audit_cross_host(now_ms, &sender_b58, &recipient_display, "rejected", "stale")
                .await;
            return RemoteAdmission::Rejected;
        }
        if issued_at_ms.saturating_sub(now_ms) > CROSS_HOST_MAX_SKEW_MS {
            self.audit_cross_host(
                now_ms,
                &sender_b58,
                &recipient_display,
                "rejected",
                "future_skew",
            )
            .await;
            return RemoteAdmission::Rejected;
        }

        // 5. Recv-gate. The recipient (this daemon) must have granted
        //    a2a.recv.<sender> to itself, the same admission the local cross-peer
        //    send path enforces. Run BEFORE the dedup claim so a task refused here
        //    is never recorded as seen — a legitimate retry after the grant lands
        //    must not be swallowed as a duplicate.
        let recv_alternatives = task.sender.scoped_action_alternatives("a2a.recv");
        let task_id_s = task.id.to_string();
        let recv_scope = A2aScopeRequest {
            peer_pubkey_b58: Some(&sender_b58),
            task_id: Some(&task_id_s),
            lease_id: None,
            duplicate_risk: None,
        };
        match self
            .recipient_has_recv_for(&task.recipient, &recv_alternatives, recv_scope)
            .await
        {
            Ok(A2aScopeCheck { allowed: true, .. }) => {}
            _ => {
                self.audit_cross_host(
                    now_ms,
                    &sender_b58,
                    &recipient_display,
                    "rejected",
                    "recv_not_granted",
                )
                .await;
                return RemoteAdmission::Rejected;
            }
        }

        // 6. Anti-replay claim. Atomic and restart-durable: a captured envelope
        //    re-sent within the freshness window — including across a daemon
        //    restart — is absorbed here, not re-enqueued.
        let dedup = match &self.cross_host_dedup {
            Some(dedup) => dedup,
            None => {
                self.audit_cross_host(
                    now_ms,
                    &sender_b58,
                    &recipient_display,
                    "rejected",
                    "dedup_unconfigured",
                )
                .await;
                return RemoteAdmission::Rejected;
            }
        };
        let key = DedupKey {
            sender_pubkey_b58: sender_b58.clone(),
            task_id: task.id,
            issued_at_ms,
        };
        match dedup.claim_fresh(&key, now_ms).await {
            Ok(true) => {}
            Ok(false) => {
                self.audit_cross_host(
                    now_ms,
                    &sender_b58,
                    &recipient_display,
                    "duplicate",
                    "duplicate",
                )
                .await;
                return RemoteAdmission::Duplicate { task_id: task.id };
            }
            Err(_) => {
                // Fail closed: without a durable claim a later replay could be
                // re-admitted, so refuse rather than enqueue an unrecorded task.
                self.audit_cross_host(
                    now_ms,
                    &sender_b58,
                    &recipient_display,
                    "rejected",
                    "dedup_write_failed",
                )
                .await;
                return RemoteAdmission::Rejected;
            }
        }

        // 7. Enqueue onto the same mailbox the local path uses. The claim is
        //    already durable; if the append fails the task is claimed-but-not-
        //    enqueued — a bounded loss under mailbox IO failure, preferred over
        //    holding the replay window open by claiming only after a successful
        //    enqueue.
        let task_id = task.id;
        match self.mailbox.send_task(task).await {
            Ok(()) => {
                self.audit_cross_host(now_ms, &sender_b58, &recipient_display, "admitted", "")
                    .await;
                RemoteAdmission::Admitted { task_id }
            }
            Err(_) => {
                self.audit_cross_host(
                    now_ms,
                    &sender_b58,
                    &recipient_display,
                    "rejected",
                    "enqueue_failed",
                )
                .await;
                RemoteAdmission::Rejected
            }
        }
    }

    /// Record one cross-host admission decision on the operator's audit feed.
    /// Daemon-authored (`issuer = self.identity`): the remote sender holds no
    /// local token, so its identity lives in `sender_pubkey_b58`, not the issuer.
    async fn audit_cross_host(
        &self,
        now_ms: u64,
        sender_pubkey_b58: &str,
        recipient_display: &str,
        outcome: &str,
        reason: &str,
    ) {
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now_ms,
            issuer: self.identity.agent_id(),
            kind: AuditKind::CrossHostA2AAdmission {
                sender_pubkey_b58: sender_pubkey_b58.to_string(),
                recipient_display: recipient_display.to_string(),
                outcome: outcome.to_string(),
                reason: reason.to_string(),
            },
        };
        self.record_daemon_event(event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(task_id: u128, issued_at_ms: u64) -> DedupKey {
        DedupKey {
            sender_pubkey_b58: "3yZe7d8mUS517G5965aydkZ46HS38QLi7UQiSojurfbQ".into(),
            task_id: Uuid::from_u128(task_id),
            issued_at_ms,
        }
    }

    #[tokio::test]
    async fn claim_fresh_is_true_once_then_false() {
        let dir = tempfile::tempdir().unwrap();
        let dedup = JsonlCrossHostDedup::open(dir.path().join("dedup.jsonl"))
            .await
            .unwrap();
        let k = key(1, 1_000);
        assert!(dedup.claim_fresh(&k, 1_000).await.unwrap(), "first claim is fresh");
        assert!(
            !dedup.claim_fresh(&k, 1_001).await.unwrap(),
            "a second claim of the same key is a duplicate"
        );
    }

    #[tokio::test]
    async fn distinct_keys_each_claim_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let dedup = JsonlCrossHostDedup::open(dir.path().join("dedup.jsonl"))
            .await
            .unwrap();
        assert!(dedup.claim_fresh(&key(1, 1_000), 1_000).await.unwrap());
        assert!(
            dedup.claim_fresh(&key(2, 1_000), 1_000).await.unwrap(),
            "a different task id is independent"
        );
        assert!(
            dedup.claim_fresh(&key(1, 2_000).clone(), 2_000).await.unwrap(),
            "the same task id at a different issued_at_ms is a distinct envelope"
        );
    }

    #[tokio::test]
    async fn claims_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.jsonl");
        let k = key(7, 1_000);
        {
            let dedup = JsonlCrossHostDedup::open(path.clone()).await.unwrap();
            assert!(dedup.claim_fresh(&k, 1_000).await.unwrap());
        }
        let reopened = JsonlCrossHostDedup::open(path).await.unwrap();
        assert!(
            !reopened.claim_fresh(&k, 1_500).await.unwrap(),
            "a key claimed before restart is still a duplicate after reopen"
        );
    }

    #[tokio::test]
    async fn stale_keys_are_pruned_and_reclaimable() {
        let dir = tempfile::tempdir().unwrap();
        let dedup = JsonlCrossHostDedup::open(dir.path().join("dedup.jsonl"))
            .await
            .unwrap();
        let k = key(9, 1_000);
        assert!(dedup.claim_fresh(&k, 1_000).await.unwrap());
        assert!(!dedup.claim_fresh(&k, 1_000).await.unwrap(), "duplicate while fresh");
        let past_horizon = 1_000 + CROSS_HOST_MAX_AGE_MS + CROSS_HOST_MAX_SKEW_MS + 1;
        assert!(
            dedup.claim_fresh(&k, past_horizon).await.unwrap(),
            "a key older than the freshness horizon is pruned and no longer dedup-blocks"
        );
    }

    #[tokio::test]
    async fn open_rejects_a_corrupt_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.jsonl");
        tokio::fs::write(&path, b"not json\n").await.unwrap();
        assert!(
            JsonlCrossHostDedup::open(path).await.is_err(),
            "a corrupt replay log must fail closed, not silently drop keys"
        );
    }
}
