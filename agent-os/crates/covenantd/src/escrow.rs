//! Daemon-side escrow completion proofs for an external marketplace escrow
//! (e.g. Orbserv's OrbMarket holding funds in OrbWallet).
//!
//! Covenant is the trust layer. An external escrow funds a task, the worker
//! runs under Covenant, and when the work is done the daemon issues a
//! **signed completion proof**: an envelope binding the task, the worker, the
//! result hash, the validation outcome, and the audit chain root at proof
//! time, signed with the daemon's ed25519 identity. The escrow verifies the
//! signature against the daemon's published pubkey and releases funds against
//! it. Covenant holds no funds and moves none — it produces the release
//! signal and records it.
//!
//! After the escrow releases and executes the transfer, it reports the
//! settlement back so the payout joins the proof in the audit chain
//! ([`record_escrow_release`]). That close mirrors the spend-settlement path
//! in `spend_authz.rs` and is idempotent on `proof_id`.
//!
//! What the proof guarantees today: it is attributable to Covenant (signed by
//! the daemon key), bound to Covenant's tamper-evident audit root at proof
//! time, and immutably recorded in the chain. What it does not yet do: re-
//! derive `result_hash_hex` from the daemon's own run records rather than
//! trusting the authenticated, capability-gated caller's report. Binding the
//! proof to an internal `IntentDispatched`/run record is a planned tightening,
//! the same shape `spend_authz` notes for settlement-vs-approval.

use std::time::{SystemTime, UNIX_EPOCH};

use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_identity::LocalIdentity;
use covenant_settlement::Settlement;
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

/// Opt-in switch for the daemon's escrow surface. Defaults to `false` so a
/// daemon with no operator opt-in proves nothing.
#[derive(Debug, Clone, Default)]
pub struct EscrowConfig {
    pub enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EscrowError {
    #[error("escrow surface is disabled")]
    Disabled,
    #[error("audit: {0}")]
    Audit(String),
    #[error("settlement: {0}")]
    Settlement(String),
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Facts an authenticated caller reports for a finished task, which the
/// daemon binds into a signed proof. `worker_pubkey` is the bs58 ed25519 key
/// of the agent the escrow should pay; `result_hash_hex` is the hash of the
/// delivered result (the same value the audit chain carries on
/// `IntentDispatched`); `validation_passed` is whether the work validated.
pub struct CompletionFacts {
    pub task_id: Uuid,
    pub worker_pubkey: String,
    pub provider: String,
    pub result_hash_hex: String,
    pub validation_passed: bool,
}

/// The canonical completion-proof envelope. Its JSON bytes are exactly what
/// the daemon signs and what an external verifier checks the signature
/// against, so the wire form carries the JSON verbatim rather than asking the
/// verifier to re-serialize (re-serialization is where cross-language
/// canonicalization bugs live).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionProof {
    pub proof_id: Uuid,
    pub task_id: Uuid,
    pub worker_pubkey: String,
    pub provider: String,
    pub result_hash_hex: String,
    pub validation_passed: bool,
    /// Audit chain root at proof time, tying the proof to Covenant's
    /// tamper-evident work history. Excludes the proof's own row, which the
    /// daemon records immediately after.
    pub audit_root_hex: String,
    pub proven_at: u64,
}

/// A [`CompletionProof`] plus the signature and the exact bytes it was signed
/// over. `proof_json` is the canonical message: verify
/// `ed25519_verify(signer_pubkey, proof_json.as_bytes(), signature)`, then
/// trust the parsed fields. Release escrow to `proof.worker_pubkey` when
/// `proof.validation_passed`.
#[derive(Debug, Clone)]
pub struct SignedCompletionProof {
    pub proof: CompletionProof,
    pub proof_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

/// Subsystem handles borrowed for the duration of one proof. `identity` signs
/// the envelope; `audit` supplies the chain root and records the proof.
pub struct ProveContext<'a> {
    pub identity: &'a LocalIdentity,
    pub audit: &'a dyn AuditLog,
    pub issuer: &'a AgentId,
}

/// Issues a signed completion proof and records it.
///
/// Binds the reported facts to the audit chain root and signs the envelope
/// with the daemon identity, then writes one
/// [`AuditKind::EscrowCompletionProven`] row carrying the proof and its
/// signature, so the chain holds a self-verifiable record of every release
/// signal Covenant produced. If the audit write fails the proof is not
/// returned: a signal the chain did not record must not trigger a release.
pub async fn prove_completion(
    ctx: &ProveContext<'_>,
    config: &EscrowConfig,
    facts: &CompletionFacts,
) -> Result<SignedCompletionProof, EscrowError> {
    if !config.enabled {
        return Err(EscrowError::Disabled);
    }

    let report = ctx
        .audit
        .verify_integrity()
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?;

    let proof = CompletionProof {
        proof_id: Uuid::new_v4(),
        task_id: facts.task_id,
        worker_pubkey: facts.worker_pubkey.clone(),
        provider: facts.provider.clone(),
        result_hash_hex: facts.result_hash_hex.clone(),
        validation_passed: facts.validation_passed,
        audit_root_hex: report.root_hash_hex,
        proven_at: epoch_ms(),
    };

    // serde_json on a struct emits fields in declaration order, so this is a
    // stable canonical form for both the signer and the verifier.
    let proof_json = serde_json::to_string(&proof)
        .map_err(|e| EscrowError::Audit(format!("serialize proof: {e}")))?;
    let signature = ctx.identity.sign(proof_json.as_bytes());
    let signature_b58 = bs58::encode(signature.to_bytes()).into_string();
    let signer_pubkey_b58 = bs58::encode(ctx.identity.pubkey_bytes()).into_string();

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: proof.proven_at,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::EscrowCompletionProven {
                proof_id: proof.proof_id,
                task_id: proof.task_id,
                worker_pubkey: proof.worker_pubkey.clone(),
                provider: proof.provider.clone(),
                result_hash_hex: proof.result_hash_hex.clone(),
                validation_passed: proof.validation_passed,
                audit_root_hex: proof.audit_root_hex.clone(),
                signature_b58: signature_b58.clone(),
            },
        })
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?;

    debug!(
        provider = proof.provider,
        %proof.proof_id,
        task_id = %proof.task_id,
        validation_passed = proof.validation_passed,
        "issued escrow completion proof"
    );

    Ok(SignedCompletionProof {
        proof,
        proof_json,
        signature_b58,
        signer_pubkey_b58,
    })
}

/// Facts an escrow reports after it released funds against a proof.
pub struct ReleaseFacts {
    /// The `proof_id` from the [`CompletionProof`] this release acted on.
    pub proof_id: Uuid,
    pub provider: String,
    pub network: String,
    pub asset: String,
    /// Atomic amount actually released, as a decimal string.
    pub amount: String,
    /// On-chain transaction signature or hash, when the escrow has it.
    pub tx_sig: Option<String>,
}

/// Subsystem handles borrowed for the duration of one release record.
pub struct ReleaseContext<'a> {
    pub settlement: &'a dyn Settlement,
    pub audit: &'a dyn AuditLog,
    pub issuer: &'a AgentId,
}

/// Records that an escrow released funds against a completion proof: a
/// [`SettlementReceipt`] and one [`AuditKind::EscrowReleased`] row sharing the
/// receipt id and carrying the originating `proof_id`. Returns the receipt id.
/// Covenant custodies nothing, so this moves no funds and debits no budget; it
/// closes the loop the proof opened by joining the payout to the proof in the
/// audit chain.
///
/// Idempotent on `proof_id`: an escrow that retries a release report (its
/// success response was lost) joins the original receipt instead of writing a
/// duplicate row, mirroring `spend_authz::record_spend_settlement`.
pub async fn record_escrow_release(
    ctx: &ReleaseContext<'_>,
    config: &EscrowConfig,
    payee: &AgentId,
    facts: &ReleaseFacts,
) -> Result<Uuid, EscrowError> {
    if !config.enabled {
        return Err(EscrowError::Disabled);
    }

    if let Some(receipt_id) = ctx
        .audit
        .released_receipt_for(facts.proof_id)
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?
    {
        debug!(
            proof_id = %facts.proof_id,
            %receipt_id,
            "escrow release already recorded; returning original receipt"
        );
        return Ok(receipt_id);
    }

    let receipt_id = Uuid::new_v4();
    let now = epoch_ms();

    ctx.settlement
        .record(SettlementReceipt {
            id: receipt_id,
            payer: payee.clone(),
            resource: ResourceKind::Tool,
            memory_record_id: None,
            credits_consumed: 0,
            settled_at: now,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: facts.tx_sig.clone(),
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        })
        .await
        .map_err(|e| EscrowError::Settlement(e.to_string()))?;

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::EscrowReleased {
                proof_id: facts.proof_id,
                receipt_id,
                provider: facts.provider.clone(),
                network: facts.network.clone(),
                asset: facts.asset.clone(),
                amount: facts.amount.clone(),
                tx_sig: facts.tx_sig.clone(),
            },
        })
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?;

    debug!(
        provider = facts.provider,
        %receipt_id,
        proof_id = %facts.proof_id,
        "recorded escrow release"
    );
    Ok(receipt_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::InMemoryAuditLog;
    use covenant_identity::verify_b58;

    fn agent(tag: u8) -> AgentId {
        AgentId::new("operator@local", [tag; 32])
    }

    fn enabled() -> EscrowConfig {
        EscrowConfig { enabled: true }
    }

    fn facts() -> CompletionFacts {
        CompletionFacts {
            task_id: Uuid::from_u128(0x7a5c),
            worker_pubkey: bs58::encode([3u8; 32]).into_string(),
            provider: "orbserv".into(),
            result_hash_hex: "abc123".into(),
            validation_passed: true,
        }
    }

    #[tokio::test]
    async fn proof_is_signed_verifiably_and_audited() {
        let audit = InMemoryAuditLog::new();
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };

        let signed = prove_completion(&ctx, &enabled(), &facts())
            .await
            .expect("prove");

        // External verification: the signature checks out over the exact
        // proof_json bytes against the advertised signer pubkey.
        verify_b58(
            &signed.signer_pubkey_b58,
            signed.proof_json.as_bytes(),
            &signed.signature_b58,
        )
        .expect("signature must verify");

        // The signer is the daemon identity.
        assert_eq!(
            signed.signer_pubkey_b58,
            bs58::encode(identity.pubkey_bytes()).into_string()
        );
        // proof_json parses back to the proof.
        let parsed: CompletionProof = serde_json::from_str(&signed.proof_json).unwrap();
        assert_eq!(parsed, signed.proof);
        assert!(parsed.validation_passed);
        assert_eq!(parsed.task_id, Uuid::from_u128(0x7a5c));

        // Exactly one proof row, carrying the signature for re-verification.
        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::EscrowCompletionProven {
                proof_id,
                signature_b58,
                validation_passed,
                ..
            } => {
                assert_eq!(*proof_id, signed.proof.proof_id);
                assert_eq!(signature_b58, &signed.signature_b58);
                assert!(validation_passed);
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tampering_breaks_proof_verification() {
        let audit = InMemoryAuditLog::new();
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };
        let signed = prove_completion(&ctx, &enabled(), &facts())
            .await
            .expect("prove");

        // Flip validation_passed in the JSON: the signature must no longer verify.
        let tampered = signed
            .proof_json
            .replace("\"validation_passed\":true", "\"validation_passed\":false");
        assert_ne!(tampered, signed.proof_json);
        assert!(verify_b58(
            &signed.signer_pubkey_b58,
            tampered.as_bytes(),
            &signed.signature_b58,
        )
        .is_err());
    }

    #[tokio::test]
    async fn prove_refuses_when_disabled() {
        let audit = InMemoryAuditLog::new();
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };
        let err = prove_completion(&ctx, &EscrowConfig::default(), &facts())
            .await
            .expect_err("disabled");
        assert!(matches!(err, EscrowError::Disabled));
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    fn release_facts(proof_id: Uuid) -> ReleaseFacts {
        ReleaseFacts {
            proof_id,
            provider: "orbserv".into(),
            network: "eip155:8453".into(),
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            amount: "80000".into(),
            tx_sig: Some("0xsig".into()),
        }
    }

    #[tokio::test]
    async fn release_records_receipt_and_audits() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let issuer = agent(9);
        let payee = agent(3);
        let proof_id = Uuid::from_u128(0xbeef);

        let ctx = ReleaseContext {
            settlement: &settlement,
            audit: &audit,
            issuer: &issuer,
        };
        let receipt_id = record_escrow_release(&ctx, &enabled(), &payee, &release_facts(proof_id))
            .await
            .expect("release");

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, receipt_id);
        assert_eq!(
            receipts[0].credits_consumed, 0,
            "escrow release debits no budget"
        );
        assert_eq!(receipts[0].tx_sig.as_deref(), Some("0xsig"));

        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::EscrowReleased {
                proof_id: pid,
                receipt_id: rid,
                tx_sig,
                ..
            } => {
                assert_eq!(*pid, proof_id);
                assert_eq!(*rid, receipt_id);
                assert_eq!(tx_sig.as_deref(), Some("0xsig"));
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn release_is_idempotent_on_proof_id() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let issuer = agent(9);
        let payee = agent(3);
        let proof_id = Uuid::from_u128(0xbeef);
        let ctx = ReleaseContext {
            settlement: &settlement,
            audit: &audit,
            issuer: &issuer,
        };

        let first = record_escrow_release(&ctx, &enabled(), &payee, &release_facts(proof_id))
            .await
            .unwrap();
        let second = record_escrow_release(&ctx, &enabled(), &payee, &release_facts(proof_id))
            .await
            .unwrap();

        assert_eq!(first, second, "retry returns the original receipt id");
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1, "one receipt");
        assert_eq!(audit.recent(10).await.unwrap().len(), 1, "one released row");
    }

    #[tokio::test]
    async fn release_refuses_when_disabled() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let issuer = agent(9);
        let payee = agent(3);
        let ctx = ReleaseContext {
            settlement: &settlement,
            audit: &audit,
            issuer: &issuer,
        };
        let err = record_escrow_release(
            &ctx,
            &EscrowConfig::default(),
            &payee,
            &release_facts(Uuid::from_u128(1)),
        )
        .await
        .expect_err("disabled");
        assert!(matches!(err, EscrowError::Disabled));
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }
}
