//! Daemon-side escrow completion proofs for an external marketplace escrow
//! (e.g. Orbserv's OrbMarket holding funds in OrbWallet on Base).
//!
//! Covenant is the trust layer. A hirer locks funds in escrow against a job,
//! the worker runs under Covenant, and when the work is done the escrow asks
//! the daemon to prove completion. The daemon does not take the caller's word
//! that the work happened: it looks the job up in its own audit chain, derives
//! the result hash and validation outcome from the worker's actual run, and
//! signs a proof carrying those derived facts plus the escrow context. The
//! escrow verifies the signature against the daemon's published pubkey and
//! releases funds to the worker when `validation_passed`. Covenant holds no
//! funds and moves none — it produces the release signal and records it.
//!
//! Because the facts are derived from Covenant's own records rather than the
//! request, it is safe for the hirer wallet itself to call prove: it cannot
//! forge a result the audit chain does not show. A job with no run in the
//! chain is denied; a job whose run failed is attested with
//! `validation_passed = false`, so the escrow simply does not release.
//!
//! After releasing and executing the transfer, the escrow reports it back
//! ([`record_escrow_release`]) so the payout joins the proof in the audit
//! chain, idempotent on `decision_id`.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
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
    #[error("job_id {0:?} is not a valid uuid")]
    BadJobId(String),
    #[error("no run found in the audit chain for job {0}; nothing to prove")]
    NoCompletion(Uuid),
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

/// The escrow context an authenticated caller reports for a job it wants
/// proven. The completion facts (result hash, validation) are NOT taken from
/// here — the daemon derives them from the worker's run — so this carries only
/// what Covenant cannot know on its own: which escrow, which payee, and the
/// payment rail, all recorded into the proof and the audit row.
pub struct ProveRequest {
    pub escrow_id: String,
    /// The Covenant job/intent id the worker ran under, as a uuid string.
    pub job_id: String,
    pub hirer_address: String,
    pub worker_address: String,
    /// Atomic amount locked in escrow, as a decimal string.
    pub amount: String,
    pub asset: String,
    pub network: String,
    pub provider: String,
}

/// The canonical completion-proof envelope. Its JSON bytes are exactly what
/// the daemon signs and what an external verifier checks the signature
/// against, so the wire form carries the JSON verbatim rather than asking the
/// verifier to re-serialize (re-serialization is where cross-language
/// canonicalization bugs live). `result_hash_hex` and `validation_passed` are
/// derived by the daemon from the worker's run, not from the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionProof {
    pub proof_id: Uuid,
    pub escrow_id: String,
    pub job_id: Uuid,
    pub hirer_address: String,
    pub worker_address: String,
    pub amount: String,
    pub asset: String,
    pub network: String,
    pub provider: String,
    pub result_hash_hex: String,
    pub validation_passed: bool,
    /// Audit chain root at proof time, tying the proof to Covenant's
    /// tamper-evident work history. Excludes the proof's own row, which the
    /// daemon records immediately after.
    pub audit_root_hex: String,
    pub proven_at: u64,
}

/// A signed [`CompletionProof`]. `proof_blob_b64` is the single opaque token an
/// escrow stores and verifies: base64 of `{proof_json, signature_b58,
/// signer_pubkey_b58}`. To verify: base64-decode, parse, then
/// `ed25519_verify(signer_pubkey_b58, proof_json bytes, signature_b58)` and
/// trust the parsed `proof_json` fields.
#[derive(Debug, Clone)]
pub struct SignedCompletionProof {
    pub proof: CompletionProof,
    pub proof_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
    pub proof_blob_b64: String,
}

impl SignedCompletionProof {
    /// The id the escrow echoes back on `/escrow/release`.
    pub fn decision_id(&self) -> Uuid {
        self.proof.proof_id
    }
}

/// Subsystem handles borrowed for the duration of one proof. `identity` signs
/// the envelope; `audit` supplies both the run lookup and the chain root, and
/// records the proof.
pub struct ProveContext<'a> {
    pub identity: &'a LocalIdentity,
    pub audit: &'a dyn AuditLog,
    pub issuer: &'a AgentId,
}

/// The result hash and validation outcome of a job's run, as the audit chain
/// recorded it. `validation_passed` is whether the dispatch finished `ok`.
/// Scans newest-first so a re-run's latest outcome wins.
fn derive_completion(events: &[AuditEvent], job_id: Uuid) -> Option<(String, bool)> {
    events.iter().rev().find_map(|e| match &e.kind {
        AuditKind::IntentDispatched {
            intent_id,
            result_hash_hex,
            status,
            ..
        } if *intent_id == job_id => Some((result_hash_hex.clone(), status == "ok")),
        _ => None,
    })
}

/// Proves a job's completion from Covenant's own records and signs the result.
///
/// Looks the job up in the audit chain, derives the result hash and validation
/// outcome from the worker's run, binds them with the escrow context and the
/// chain root, signs the envelope, and writes one
/// [`AuditKind::EscrowCompletionProven`] row carrying the proof and its
/// signature. A job with no run is denied. If the audit write fails the proof
/// is not returned: a signal the chain did not record must not release funds.
pub async fn prove_completion(
    ctx: &ProveContext<'_>,
    config: &EscrowConfig,
    req: &ProveRequest,
) -> Result<SignedCompletionProof, EscrowError> {
    if !config.enabled {
        return Err(EscrowError::Disabled);
    }

    let job_id =
        Uuid::parse_str(&req.job_id).map_err(|_| EscrowError::BadJobId(req.job_id.clone()))?;

    // Derive the completion facts from our own audit chain, not the request.
    let events = ctx
        .audit
        .recent(usize::MAX)
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?;
    let (result_hash_hex, validation_passed) =
        derive_completion(&events, job_id).ok_or(EscrowError::NoCompletion(job_id))?;

    let report = ctx
        .audit
        .verify_integrity()
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?;

    let proof = CompletionProof {
        proof_id: Uuid::new_v4(),
        escrow_id: req.escrow_id.clone(),
        job_id,
        hirer_address: req.hirer_address.clone(),
        worker_address: req.worker_address.clone(),
        amount: req.amount.clone(),
        asset: req.asset.clone(),
        network: req.network.clone(),
        provider: req.provider.clone(),
        result_hash_hex,
        validation_passed,
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
    let bundle = serde_json::json!({
        "proof_json": proof_json,
        "signature_b58": signature_b58,
        "signer_pubkey_b58": signer_pubkey_b58,
    });
    let proof_blob_b64 =
        base64::engine::general_purpose::STANDARD.encode(bundle.to_string().as_bytes());

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: proof.proven_at,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::EscrowCompletionProven {
                proof_id: proof.proof_id,
                escrow_id: proof.escrow_id.clone(),
                job_id: proof.job_id,
                hirer_address: proof.hirer_address.clone(),
                worker_address: proof.worker_address.clone(),
                amount: proof.amount.clone(),
                asset: proof.asset.clone(),
                network: proof.network.clone(),
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
        job_id = %proof.job_id,
        validation_passed = proof.validation_passed,
        "issued escrow completion proof"
    );

    Ok(SignedCompletionProof {
        proof,
        proof_json,
        signature_b58,
        signer_pubkey_b58,
        proof_blob_b64,
    })
}

/// Facts an escrow reports after it released funds against a proof.
pub struct ReleaseFacts {
    /// The `decision_id` from the [`CompletionProof`] this release acted on
    /// (its `proof_id`). Joins the payout back to the proof.
    pub decision_id: Uuid,
    pub escrow_id: String,
    pub hirer_address: String,
    pub worker_address: String,
    pub amount: String,
    pub asset: String,
    pub network: String,
    pub provider: String,
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
/// receipt id and carrying the originating `decision_id`. Returns the epoch-ms
/// timestamp the payout was recorded at. Covenant custodies nothing, so this
/// moves no funds and debits no budget; it closes the loop the proof opened by
/// joining the payout to the proof in the audit chain.
///
/// Idempotent on `decision_id`: an escrow that retries a release report (its
/// success response was lost) joins the original receipt instead of writing a
/// duplicate row, mirroring `spend_authz::record_spend_settlement`.
pub async fn record_escrow_release(
    ctx: &ReleaseContext<'_>,
    config: &EscrowConfig,
    payee: &AgentId,
    facts: &ReleaseFacts,
) -> Result<u64, EscrowError> {
    if !config.enabled {
        return Err(EscrowError::Disabled);
    }

    let now = epoch_ms();

    if ctx
        .audit
        .released_receipt_for(facts.decision_id)
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?
        .is_some()
    {
        debug!(
            decision_id = %facts.decision_id,
            "escrow release already recorded; returning idempotently"
        );
        return Ok(now);
    }

    let receipt_id = Uuid::new_v4();

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
                decision_id: facts.decision_id,
                receipt_id,
                escrow_id: facts.escrow_id.clone(),
                hirer_address: facts.hirer_address.clone(),
                worker_address: facts.worker_address.clone(),
                amount: facts.amount.clone(),
                asset: facts.asset.clone(),
                network: facts.network.clone(),
                provider: facts.provider.clone(),
                tx_sig: facts.tx_sig.clone(),
            },
        })
        .await
        .map_err(|e| EscrowError::Audit(e.to_string()))?;

    debug!(
        provider = facts.provider,
        %receipt_id,
        decision_id = %facts.decision_id,
        "recorded escrow release"
    );
    Ok(now)
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

    const JOB: Uuid = Uuid::from_u128(0x7a5c);
    const WORKER: &str = "0x7A4D3Ae53E9F96599143e1BF057ba11A7e09Ab3E";
    const HIRER: &str = "0x0fA12125753428C58aE439E57fab3A94Bd93C78b";

    /// Seed the audit chain with the worker's run for `job`, the way a real
    /// dispatch would, so prove has something to derive from.
    async fn seed_run(audit: &InMemoryAuditLog, job: Uuid, status: &str, result_hash: &str) {
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: 1,
                issuer: agent(9),
                kind: AuditKind::IntentDispatched {
                    intent_id: job,
                    intent_text: "do the work".into(),
                    matched_agent: Some(WORKER.into()),
                    result_hash_hex: result_hash.into(),
                    status: status.into(),
                },
            })
            .await
            .unwrap();
    }

    fn prove_req() -> ProveRequest {
        ProveRequest {
            escrow_id: "escrow_xyz".into(),
            job_id: JOB.to_string(),
            hirer_address: HIRER.into(),
            worker_address: WORKER.into(),
            amount: "10000000".into(),
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            network: "eip155:84532".into(),
            provider: "orbserv".into(),
        }
    }

    #[tokio::test]
    async fn prove_derives_facts_from_the_run_and_signs_verifiably() {
        let audit = InMemoryAuditLog::new();
        seed_run(&audit, JOB, "ok", "9f86d081").await;
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };

        let signed = prove_completion(&ctx, &enabled(), &prove_req())
            .await
            .expect("prove");

        // Facts came from the run, not the request (the request never carried them).
        assert_eq!(signed.proof.result_hash_hex, "9f86d081");
        assert!(signed.proof.validation_passed);
        assert_eq!(signed.proof.job_id, JOB);
        assert_eq!(signed.proof.worker_address, WORKER);
        assert_eq!(signed.proof.escrow_id, "escrow_xyz");

        // The opaque blob verifies the way the escrow will check it.
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&signed.proof_blob_b64)
            .unwrap();
        let bundle: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let proof_json = bundle["proof_json"].as_str().unwrap();
        let sig = bundle["signature_b58"].as_str().unwrap();
        let pk = bundle["signer_pubkey_b58"].as_str().unwrap();
        verify_b58(pk, proof_json.as_bytes(), sig).expect("blob signature must verify");
        let parsed: CompletionProof = serde_json::from_str(proof_json).unwrap();
        assert_eq!(parsed, signed.proof);

        // One self-verifiable proof row.
        let events = audit.recent(10).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(&e.kind, AuditKind::EscrowCompletionProven { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn prove_attests_a_failed_run_as_not_validated() {
        let audit = InMemoryAuditLog::new();
        seed_run(&audit, JOB, "error", "deadbeef").await;
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };
        let signed = prove_completion(&ctx, &enabled(), &prove_req())
            .await
            .expect("prove");
        assert!(
            !signed.proof.validation_passed,
            "a failed run must attest validation_passed = false so the escrow does not release"
        );
    }

    #[tokio::test]
    async fn prove_denies_when_no_run_for_job() {
        let audit = InMemoryAuditLog::new(); // nothing seeded
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };
        let err = prove_completion(&ctx, &enabled(), &prove_req())
            .await
            .expect_err("no run");
        assert!(matches!(err, EscrowError::NoCompletion(j) if j == JOB));
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn prove_refuses_when_disabled() {
        let audit = InMemoryAuditLog::new();
        seed_run(&audit, JOB, "ok", "9f86d081").await;
        let identity = LocalIdentity::generate("daemon@local");
        let issuer = identity.agent_id();
        let ctx = ProveContext {
            identity: &identity,
            audit: &audit,
            issuer: &issuer,
        };
        let err = prove_completion(&ctx, &EscrowConfig::default(), &prove_req())
            .await
            .expect_err("disabled");
        assert!(matches!(err, EscrowError::Disabled));
    }

    fn release_facts(decision_id: Uuid) -> ReleaseFacts {
        ReleaseFacts {
            decision_id,
            escrow_id: "escrow_xyz".into(),
            hirer_address: HIRER.into(),
            worker_address: WORKER.into(),
            amount: "10000000".into(),
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            network: "eip155:84532".into(),
            provider: "orbserv".into(),
            tx_sig: Some("0xpayout".into()),
        }
    }

    #[tokio::test]
    async fn release_records_receipt_and_audits() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let issuer = agent(9);
        let payee = agent(3);
        let decision_id = Uuid::from_u128(0xbeef);
        let ctx = ReleaseContext {
            settlement: &settlement,
            audit: &audit,
            issuer: &issuer,
        };
        let recorded_at =
            record_escrow_release(&ctx, &enabled(), &payee, &release_facts(decision_id))
                .await
                .expect("release");
        assert!(recorded_at > 0);

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0].credits_consumed, 0,
            "escrow release debits no budget"
        );
        assert_eq!(receipts[0].tx_sig.as_deref(), Some("0xpayout"));

        match &audit.recent(10).await.unwrap()[0].kind {
            AuditKind::EscrowReleased {
                decision_id: did,
                tx_sig,
                escrow_id,
                ..
            } => {
                assert_eq!(*did, decision_id);
                assert_eq!(tx_sig.as_deref(), Some("0xpayout"));
                assert_eq!(escrow_id, "escrow_xyz");
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn release_is_idempotent_on_decision_id() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let issuer = agent(9);
        let payee = agent(3);
        let decision_id = Uuid::from_u128(0xbeef);
        let ctx = ReleaseContext {
            settlement: &settlement,
            audit: &audit,
            issuer: &issuer,
        };
        record_escrow_release(&ctx, &enabled(), &payee, &release_facts(decision_id))
            .await
            .unwrap();
        record_escrow_release(&ctx, &enabled(), &payee, &release_facts(decision_id))
            .await
            .unwrap();
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1, "one receipt");
        assert_eq!(audit.recent(10).await.unwrap().len(), 1, "one released row");
    }
}
