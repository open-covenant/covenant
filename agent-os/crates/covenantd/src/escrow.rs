//! Experimental daemon-signed statements for an external marketplace escrow.
//!
//! The result hash and `status == "ok"` observation come from a local
//! `IntentDispatched` audit row. Escrow id, hirer, worker, amount, asset,
//! network, and provider come from the authenticated caller. The daemon does
//! not bind those fields to a prior lock, validate work quality, or verify a
//! payout onchain. A consumer must pin the daemon key, match every
//! caller-supplied field against its own precommit, and make a separate release
//! decision. This statement is not a release authorization.
//!
//! The legacy release-reporting endpoint is parked. Covenant cannot safely
//! turn caller-supplied payout fields into settlement or audit facts without
//! independently binding them to an authorization and an onchain transfer.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
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
    #[error(
        "escrow release reporting is disabled until payout facts are independently verified and bound to the completion statement"
    )]
    ReleaseReportingDisabled,
}

/// Domain prefix for the exact bytes signed in a completion-statement bundle.
pub const ESCROW_COMPLETION_DOMAIN: &str = "covenant.escrow-completion.v1\n";

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Escrow context reported by the authenticated caller. None of these fields is
/// matched to a daemon-owned precommit.
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
    /// True only when the newest local dispatch row has status `ok`. This is
    /// not independent validation of completion, delivery, or quality.
    pub validation_passed: bool,
    /// Audit chain root at proof time, tying the proof to Covenant's
    /// tamper-evident work history. Excludes the proof's own row, which the
    /// daemon records immediately after.
    pub audit_root_hex: String,
    pub proven_at: u64,
}

/// A signed [`CompletionProof`]. `proof_blob_b64` is the single opaque token an
/// escrow stores and verifies: base64 of `{domain, proof_json, signature_b58,
/// signer_pubkey_b58}`. A verifier must require [`ESCROW_COMPLETION_DOMAIN`],
/// compare `signer_pubkey_b58` with a separately pinned key, verify the
/// signature over `domain || proof_json`, then match the parsed caller-supplied
/// context with its own precommit.
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

/// The result hash and status observation of a local dispatch row.
/// `validation_passed` is whether the recorded status equals `ok`.
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

/// Signs a local dispatch observation plus caller-supplied escrow context.
///
/// Looks the job up in the audit chain, derives the result hash and status,
/// copies the caller's unverified escrow context, signs the envelope, and writes one
/// [`AuditKind::EscrowCompletionProven`] row carrying the proof and its
/// signature. A job with no run is denied. If the audit write fails, the
/// statement is not returned. The result is not sufficient to release funds.
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

    // Derive only the result hash and dispatch status from the audit chain.
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
    let signed_message = format!("{ESCROW_COMPLETION_DOMAIN}{proof_json}");
    let signature = ctx.identity.sign(signed_message.as_bytes());
    let signature_b58 = bs58::encode(signature.to_bytes()).into_string();
    let signer_pubkey_b58 = bs58::encode(ctx.identity.pubkey_bytes()).into_string();
    let bundle = serde_json::json!({
        "domain": ESCROW_COMPLETION_DOMAIN,
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

/// Caller-supplied fields retained for the parked release request's wire shape.
pub struct ReleaseFacts {
    /// Intended to reference a [`CompletionProof`], but not trusted or joined
    /// while release reporting is disabled.
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

/// Rejects the legacy release-reporting path without writing settlement,
/// accounting, or audit state.
///
/// The request fields are caller supplied. Until Covenant can independently
/// verify a payout and atomically bind it to a prior authorization, recording
/// them would convert an assertion into trusted state.
pub fn record_escrow_release(config: &EscrowConfig) -> Result<u64, EscrowError> {
    if !config.enabled {
        return Err(EscrowError::Disabled);
    }

    Err(EscrowError::ReleaseReportingDisabled)
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
        let domain = bundle["domain"].as_str().unwrap();
        assert_eq!(domain, ESCROW_COMPLETION_DOMAIN);
        let proof_json = bundle["proof_json"].as_str().unwrap();
        let sig = bundle["signature_b58"].as_str().unwrap();
        let pk = bundle["signer_pubkey_b58"].as_str().unwrap();
        let signed_message = format!("{domain}{proof_json}");
        verify_b58(pk, signed_message.as_bytes(), sig).expect("blob signature must verify");
        let wrong_domain = format!("covenant.other.v1\n{proof_json}");
        assert!(
            verify_b58(pk, wrong_domain.as_bytes(), sig).is_err(),
            "the completion signature must not verify under another protocol domain"
        );
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

    #[tokio::test]
    async fn release_reporting_is_parked_without_writes() {
        use covenant_settlement::Settlement as _;

        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let err =
            record_escrow_release(&enabled()).expect_err("release reporting must stay parked");

        assert!(matches!(err, EscrowError::ReleaseReportingDisabled));
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[test]
    fn release_reporting_respects_the_global_surface_switch() {
        let err = record_escrow_release(&EscrowConfig::default()).expect_err("disabled surface");

        assert!(matches!(err, EscrowError::Disabled));
    }
}
