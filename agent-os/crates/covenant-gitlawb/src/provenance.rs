//! The seam between Covenant's commit-scoped provenance and gitlawb's
//! ref-update certificates.
//!
//! A [`CommitProvenance`] is a Covenant-shaped record, what the audit layer
//! already knows about an agent's commit: which repo, which ref, the new
//! commit SHA (and the previous SHA if any), the signer, and a stable id from
//! the audit entry that birthed this update. [`into_ref_update_cert`] turns
//! that record into a signed [`RefUpdateCert`] without the caller having to
//! reach into the cert format or the signer encoding.
//!
//! The audit-id is used verbatim as the cert's nonce, which means the same
//! audit entry always produces a byte-identical cert body. That keeps retries
//! and replays idempotent on the gitlawb side: if a node already has the cert,
//! it can dedupe by `(repo, ref_name, seq, nonce)` instead of re-signing.

use ed25519_dalek::SigningKey;

use crate::cert::RefUpdateCert;
use crate::Result;

/// What Covenant's audit layer hands to the bridge.
#[derive(Debug, Clone)]
pub struct CommitProvenance {
    /// Repository DID (`did:key:...`).
    pub repo_did: String,
    /// Git ref being updated, e.g. `refs/heads/main`.
    pub ref_name: String,
    /// Previous commit SHA-256 hex (64 chars), or 64 zeros for a new ref.
    pub from_sha: String,
    /// New commit SHA-256 hex (64 chars).
    pub to_sha: String,
    /// Monotonic per-ref sequence number from the audit log.
    pub seq: u64,
    /// Stable id from the audit entry. Used as the cert nonce so a retry of
    /// the same audit event yields the same cert body.
    pub audit_id: String,
}

/// Convert audit-side provenance into a signed gitlawb ref-update cert.
pub fn into_ref_update_cert(
    provenance: &CommitProvenance,
    signer: &SigningKey,
) -> Result<RefUpdateCert> {
    RefUpdateCert::sign_new(
        signer,
        provenance.repo_did.clone(),
        provenance.ref_name.clone(),
        provenance.from_sha.clone(),
        provenance.to_sha.clone(),
        provenance.seq,
        provenance.audit_id.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::did_key_from_verifying_key;
    use ed25519_dalek::SigningKey;
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

    #[test]
    fn provenance_signs_into_valid_cert() {
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let p = CommitProvenance {
            repo_did: repo.clone(),
            ref_name: "refs/heads/main".into(),
            from_sha: sha('0'),
            to_sha: sha('a'),
            seq: 1,
            audit_id: "audit-event-0001".into(),
        };
        let cert = into_ref_update_cert(&p, &sk).unwrap();
        let valid = cert.verify_all().unwrap();
        assert_eq!(valid, vec![repo]);
        assert_eq!(cert.body.nonce, "audit-event-0001");
    }

    #[test]
    fn same_audit_id_yields_same_body_except_timestamp() {
        // The cert body is identical apart from `timestamp`; the audit_id
        // becomes the nonce, so dedup keys (repo, ref_name, seq, nonce) match.
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let p = CommitProvenance {
            repo_did: repo,
            ref_name: "refs/heads/main".into(),
            from_sha: sha('0'),
            to_sha: sha('a'),
            seq: 7,
            audit_id: "audit-7".into(),
        };
        let a = into_ref_update_cert(&p, &sk).unwrap();
        let b = into_ref_update_cert(&p, &sk).unwrap();
        assert_eq!(a.body.repo, b.body.repo);
        assert_eq!(a.body.ref_name, b.body.ref_name);
        assert_eq!(a.body.from, b.body.from);
        assert_eq!(a.body.to, b.body.to);
        assert_eq!(a.body.seq, b.body.seq);
        assert_eq!(a.body.nonce, b.body.nonce);
    }
}
