//! The Covenant payout authorization receipt: a signed record of a gate
//! decision on a Loofta payout.
//!
//! Whatever the verdict, the decision is captured, canonicalized (RFC 8785),
//! hashed, and signed by a Covenant attestor. A verifier recomputes the hash and
//! checks the signature against the published key, so an agent-initiated payout
//! is never an unaccountable one. This is the off-chain half; anchoring the root
//! hash on-chain is the daemon's step, the same path `covenant-attestation`
//! already runs for other receipt kinds.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::policy::Verdict;

pub const SCHEMA: &str = "covenant.loofta.payout.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allowed,
    Refused,
    PendingApproval,
}

impl Decision {
    fn split(verdict: &Verdict) -> (Self, Option<String>) {
        match verdict {
            Verdict::Allow => (Decision::Allowed, None),
            Verdict::Refuse(r) => (Decision::Refused, Some(r.clone())),
            Verdict::NeedsApproval(r) => (Decision::PendingApproval, Some(r.clone())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayoutReceipt {
    pub schema: String,
    /// Destination pubkey the decision was made about.
    pub recipient: String,
    pub amount_usd: f64,
    pub decision: Decision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Trust label of the standing the decision rested on.
    pub source: String,
    pub decided_at: u64,
}

impl PayoutReceipt {
    pub fn new(
        recipient: impl Into<String>,
        amount_usd: f64,
        verdict: &Verdict,
        source: impl Into<String>,
    ) -> Self {
        let (decision, reason) = Decision::split(verdict);
        Self {
            schema: SCHEMA.to_string(),
            recipient: recipient.into(),
            amount_usd,
            decision,
            reason,
            source: source.into(),
            decided_at: crate::unix_now(),
        }
    }

    /// RFC 8785 (JCS) canonical JSON. The on-chain anchor carries the hash of
    /// this exact byte string.
    pub fn canonical_json(&self) -> Result<String> {
        serde_jcs::to_string(self).map_err(|e| Error::Decode(e.to_string()))
    }

    pub fn root_hash_hex(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(
            self.canonical_json()?.as_bytes(),
        )))
    }

    /// Sign the receipt with a Covenant attestor key. A verifier recomputes the
    /// JCS hash and checks the signature against the published pubkey.
    pub fn attest(&self, attestor: &SigningKey) -> Result<SignedReceipt> {
        let root_hash_hex = self.root_hash_hex()?;
        let sig = attestor.sign(root_hash_hex.as_bytes());
        Ok(SignedReceipt {
            receipt: self.clone(),
            root_hash_hex,
            attestor_pubkey_b64: STANDARD.encode(attestor.verifying_key().to_bytes()),
            signature_b64: STANDARD.encode(sig.to_bytes()),
            anchor: None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedReceipt {
    pub receipt: PayoutReceipt,
    pub root_hash_hex: String,
    pub attestor_pubkey_b64: String,
    pub signature_b64: String,
    /// On-chain anchor reference (tx signature / asset id), set once anchored.
    /// Sits outside the signed bytes, so it never affects [`Self::verify`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

impl SignedReceipt {
    pub fn verify(&self) -> bool {
        use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

        let Ok(recomputed) = self.receipt.root_hash_hex() else {
            return false;
        };
        if recomputed != self.root_hash_hex {
            return false;
        }
        let Ok(pk) = STANDARD.decode(&self.attestor_pubkey_b64) else {
            return false;
        };
        let Ok(pk): std::result::Result<[u8; 32], _> = pk.as_slice().try_into() else {
            return false;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
            return false;
        };
        let Ok(sig_bytes) = STANDARD.decode(&self.signature_b64) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&sig_bytes) else {
            return false;
        };
        vk.verify(self.root_hash_hex.as_bytes(), &sig).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_verifies_and_detects_tamper() {
        let attestor = SigningKey::from_bytes(&[3u8; 32]);
        let verdict = Verdict::Refuse("recipient carries a Covenant abuse flag".into());
        let signed = PayoutReceipt::new("Rcpt", 42.0, &verdict, "covenant-reputation (mainnet)")
            .attest(&attestor)
            .unwrap();
        assert!(signed.verify());
        assert_eq!(signed.receipt.decision, Decision::Refused);
        assert_eq!(
            signed.receipt.reason.as_deref(),
            Some("recipient carries a Covenant abuse flag")
        );

        let mut tampered = signed.clone();
        tampered.receipt.amount_usd = 4_200.0;
        assert!(!tampered.verify());
    }

    #[test]
    fn anchor_is_outside_the_signed_bytes() {
        let attestor = SigningKey::from_bytes(&[5u8; 32]);
        let mut signed = PayoutReceipt::new("Rcpt", 10.0, &Verdict::Allow, "test")
            .attest(&attestor)
            .unwrap();
        signed.anchor = Some("5xtx...".into());
        assert!(
            signed.verify(),
            "setting the anchor must not break the signature"
        );
    }
}
