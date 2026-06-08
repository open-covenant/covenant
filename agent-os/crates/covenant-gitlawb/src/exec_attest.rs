//! `covenant/exec/v1` attestation payload and verifier.
//!
//! A signed claim that a commit was produced inside a known sandbox, under a
//! specific capability grant, against an auditable execution trail. The
//! capability, sandbox, and audit roots are 64-lowercase-hex SHA-256; `commit`
//! is the git commit object id (40-hex SHA-1, or 64-hex under git's SHA-256
//! object format). `issued_at` is when the runtime emitted the attestation,
//! not when the commit was authored.

use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use gitlawb_attest::{
    Attestation, AttestationPayload, AttestationVerifier, AttestedRefUpdateCert,
    Result as AttestResult,
};
use serde::{Deserialize, Serialize};

use crate::cert::RefUpdateCert;
use crate::error::Result;

/// The `covenant/exec/v1` attestation payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CovenantExecPayload {
    /// `did:key` of the agent that produced the commit.
    pub agent_did: String,

    /// SHA-256 hex of the capability tree root the agent held at exec time.
    pub capability_root: String,

    /// SHA-256 hex of the sandbox image digest the agent ran inside.
    pub sandbox_digest: String,

    /// Merkle root of the agent's audit log up to and including the commit.
    pub audit_root: String,

    /// The git commit object id this attestation is for — the ref's new target.
    /// Lowercase hex, 40 chars (SHA-1) or 64 (git's SHA-256 object format).
    /// Cross-checked against the cert's `to`/`new_sha` by callers that want a
    /// stronger binding.
    pub commit: String,

    /// RFC 3339 timestamp the Covenant runtime emitted this attestation.
    pub issued_at: DateTime<Utc>,
}

impl AttestationPayload for CovenantExecPayload {
    fn payload_type() -> &'static str {
        "covenant/exec/v1"
    }
}

/// Verifier for `covenant/exec/v1`. Checks payload shape only; signature and
/// cert-hash binding are handled by [`gitlawb_attest::Registry`]. Callers that
/// want stronger binding should also check `payload.commit == cert.to` and
/// that `payload.agent_did` matches one of the cert signers; the example at
/// `examples/gitlawb-attest-demo` shows the pattern.
pub struct CovenantVerifier;

impl AttestationVerifier for CovenantVerifier {
    fn type_(&self) -> &str {
        CovenantExecPayload::payload_type()
    }

    fn verify_payload(&self, payload: &serde_json::Value) -> AttestResult<()> {
        let p: CovenantExecPayload = serde_json::from_value(payload.clone())
            .map_err(|e| gitlawb_attest::AttestError::Payload(format!("decode: {e}")))?;

        for (name, hash) in [
            ("capability_root", &p.capability_root),
            ("sandbox_digest", &p.sandbox_digest),
            ("audit_root", &p.audit_root),
        ] {
            if !is_lower_hex_sha256(hash) {
                return Err(gitlawb_attest::AttestError::Payload(format!(
                    "{name} must be 64 lowercase hex chars"
                )));
            }
        }
        if !is_git_object_id(&p.commit) {
            return Err(gitlawb_attest::AttestError::Payload(
                "commit must be a git object id (40 or 64 lowercase hex chars)".into(),
            ));
        }
        if !p.agent_did.starts_with("did:key:") {
            return Err(gitlawb_attest::AttestError::Payload(
                "agent_did must be a did:key".into(),
            ));
        }
        if p.issued_at > Utc::now() + chrono::Duration::minutes(5) {
            return Err(gitlawb_attest::AttestError::Payload(
                "issued_at is too far in the future".into(),
            ));
        }
        Ok(())
    }
}

fn is_lower_hex_sha256(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// A git commit object id: lowercase hex, 40 chars (SHA-1) or 64 (git's
/// SHA-256 object format).
fn is_git_object_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Sign and attach a `covenant/exec/v1` attestation to a cert. The returned
/// envelope is wire-compatible with a bare cert when serialized.
pub fn attach_covenant_attestation(
    cert: &RefUpdateCert,
    payload: CovenantExecPayload,
    signer: &SigningKey,
) -> Result<AttestedRefUpdateCert> {
    let mut envelope = AttestedRefUpdateCert::from_cert(cert)?;
    let hash = envelope.cert_hash()?;
    let att = Attestation::sign(signer, payload, hash)?;
    envelope.attach(att);
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::did_key_from_verifying_key;
    use ed25519_dalek::SigningKey;
    use gitlawb_attest::{Policy, Registry};
    use rand::rngs::OsRng;
    use rand::TryRngCore;

    fn fresh() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn sample_payload(agent_did: &str) -> CovenantExecPayload {
        CovenantExecPayload {
            agent_did: agent_did.into(),
            capability_root: sha('1'),
            sandbox_digest: sha('2'),
            audit_root: sha('3'),
            commit: sha('a'),
            issued_at: Utc::now(),
        }
    }

    fn sample_cert(sk: &SigningKey) -> RefUpdateCert {
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        RefUpdateCert::sign_new(
            sk,
            repo,
            "refs/heads/main",
            sha('0'),
            sha('a'),
            1,
            "audit-1",
        )
        .unwrap()
    }

    #[test]
    fn attach_then_verify_through_registry() {
        let repo_sk = fresh();
        let agent_sk = fresh();
        let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());

        let cert = sample_cert(&repo_sk);
        let payload = sample_payload(&agent_did);
        let envelope = attach_covenant_attestation(&cert, payload.clone(), &agent_sk).unwrap();

        let mut reg = Registry::new();
        reg.register(Box::new(CovenantVerifier));
        let hash = envelope.cert_hash().unwrap();
        let verified = reg.verify_all(&envelope.attestations, hash).unwrap();

        assert_eq!(verified.len(), 1);
        assert!(verified[0].fully_verified);
        assert_eq!(verified[0].type_, "covenant/exec/v1");
        assert_eq!(
            envelope.attestations[0]
                .payload_as::<CovenantExecPayload>()
                .unwrap(),
            payload
        );
    }

    #[test]
    fn require_all_with_covenant_passes() {
        let repo_sk = fresh();
        let agent_sk = fresh();
        let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());

        let cert = sample_cert(&repo_sk);
        let payload = sample_payload(&agent_did);
        let envelope = attach_covenant_attestation(&cert, payload, &agent_sk).unwrap();

        let mut reg = Registry::new()
            .with_policy(Policy::RequireAll)
            .require_types(["covenant/exec/v1".to_string()]);
        reg.register(Box::new(CovenantVerifier));

        let hash = envelope.cert_hash().unwrap();
        reg.verify_all(&envelope.attestations, hash).unwrap();
    }

    #[test]
    fn bad_commit_hash_fails_payload_check() {
        let agent_sk = fresh();
        let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());
        let cert = sample_cert(&fresh());

        let mut payload = sample_payload(&agent_did);
        payload.commit = "not-hex".into();
        let envelope = attach_covenant_attestation(&cert, payload, &agent_sk).unwrap();

        let mut reg = Registry::new();
        reg.register(Box::new(CovenantVerifier));
        let hash = envelope.cert_hash().unwrap();
        let err = reg.verify_all(&envelope.attestations, hash).unwrap_err();
        assert!(matches!(err, gitlawb_attest::AttestError::Payload(_)));
    }

    #[test]
    fn sha1_git_commit_is_accepted() {
        // gitlawb/git commit ids are SHA-1 (40 hex), not SHA-256 (64).
        let agent_sk = fresh();
        let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());
        let cert = sample_cert(&fresh());

        let mut payload = sample_payload(&agent_did);
        payload.commit = "a".repeat(40);
        let envelope = attach_covenant_attestation(&cert, payload, &agent_sk).unwrap();

        let mut reg = Registry::new();
        reg.register(Box::new(CovenantVerifier));
        let hash = envelope.cert_hash().unwrap();
        let verified = reg.verify_all(&envelope.attestations, hash).unwrap();
        assert!(verified[0].fully_verified);
    }

    #[test]
    fn future_issued_at_fails_payload_check() {
        let agent_sk = fresh();
        let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());
        let cert = sample_cert(&fresh());

        let mut payload = sample_payload(&agent_did);
        payload.issued_at = Utc::now() + chrono::Duration::hours(1);
        let envelope = attach_covenant_attestation(&cert, payload, &agent_sk).unwrap();

        let mut reg = Registry::new();
        reg.register(Box::new(CovenantVerifier));
        let hash = envelope.cert_hash().unwrap();
        let err = reg.verify_all(&envelope.attestations, hash).unwrap_err();
        assert!(matches!(err, gitlawb_attest::AttestError::Payload(_)));
    }
}
