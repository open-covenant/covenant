//! Privacy-preserving provenance for a completed Loofta payment.
//!
//! A Loofta payment leaves no useful public trace by design; the privacy layer
//! unlinks sender and recipient. This turns a settled payment into something the
//! payer can stand behind later without giving that privacy up: a commitment
//! over the payment's private record, foldable into Covenant's onchain
//! provenance root and bound to a Covenant attestation. Only the commitment (a
//! hash) is ever anchored, so no amount or party is revealed. To prove a payment
//! the payer opens the record; anyone recomputes the hash and checks the
//! signature.
//!
//! The commitment is one leaf. The daemon folds it into the Covenant audit /
//! provenance root through the sap-bridge, the same anchoring path
//! `covenant-attestation` already runs; this crate produces and signs the leaf.
//!
//! Roadmap, stated as a hole and not a claim: [`SignedCommitment::enclave_quote`]
//! is `None` today. When MagicBlock's private/TEE ERs and a `covenant-tee` crate
//! land, the same attestation can also bind the enclave that ran the payment, so
//! the proof covers not just that the payment happened but where it ran. Until
//! then the field stays empty.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::receipt::Decision;

pub const SCHEMA: &str = "covenant.loofta.payment.v1";

/// The private payment record. It never leaves the payer; only its commitment
/// (the hash) is anchored. `nonce_hex` is 32 caller-supplied random bytes that
/// blind the commitment, so two identical payments don't hash alike and a small
/// amount space can't be brute-forced back out of the hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRecord {
    pub schema: String,
    /// Loofta's opaque payment reference.
    pub payment_id: String,
    /// Destination pubkey. Private; never anchored in the clear.
    pub recipient: String,
    /// Amount normalized to USD. Private.
    pub amount_usd: f64,
    /// What the pre-send gate decided for this payment.
    pub gate_decision: Decision,
    pub settled_at: u64,
    /// Blinding nonce, 64 hex chars (32 bytes), kept private with the record.
    pub nonce_hex: String,
}

impl PaymentRecord {
    pub fn new(
        payment_id: impl Into<String>,
        recipient: impl Into<String>,
        amount_usd: f64,
        gate_decision: Decision,
        settled_at: u64,
        nonce: &[u8; 32],
    ) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            payment_id: payment_id.into(),
            recipient: recipient.into(),
            amount_usd,
            gate_decision,
            settled_at,
            nonce_hex: hex::encode(nonce),
        }
    }

    /// The commitment: sha256 over the JCS-canonical record. This is the only
    /// value anchored, and it reveals nothing without the record.
    pub fn commitment_hex(&self) -> Result<String> {
        let canon = serde_jcs::to_string(self).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(hex::encode(Sha256::digest(canon.as_bytes())))
    }

    /// Attest the commitment (not the record) with a Covenant attestor key. The
    /// signed artifact carries no amount or party.
    pub fn attest(&self, attestor: &SigningKey) -> Result<SignedCommitment> {
        let commitment_hex = self.commitment_hex()?;
        let sig = attestor.sign(commitment_hex.as_bytes());
        Ok(SignedCommitment {
            schema: SCHEMA.to_string(),
            commitment_hex,
            settled_at: self.settled_at,
            attestor_pubkey_b64: STANDARD.encode(attestor.verifying_key().to_bytes()),
            signature_b64: STANDARD.encode(sig.to_bytes()),
            enclave_quote: None,
            anchor: None,
        })
    }
}

/// The public, anchorable artifact: the commitment and its signature. Carries no
/// amount or party. `settled_at` is a public hint; the record it opens to is the
/// authoritative source, since the commitment binds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedCommitment {
    pub schema: String,
    pub commitment_hex: String,
    pub settled_at: u64,
    pub attestor_pubkey_b64: String,
    pub signature_b64: String,
    /// Enclave attestation of the run. Roadmap (see module docs); `None` today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclave_quote: Option<String>,
    /// On-chain anchor reference, set once the daemon folds the commitment into
    /// the provenance root. Outside the signed bytes, so it never affects
    /// [`Self::verify`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

impl SignedCommitment {
    /// The signature is valid for this commitment under the published key.
    pub fn verify(&self) -> bool {
        use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

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
        vk.verify(self.commitment_hex.as_bytes(), &sig).is_ok()
    }

    /// The revealed record opens to this commitment: it hashes to the committed
    /// value. With [`Self::verify`], this proves the payment happened as recorded
    /// without exposing it to anyone the payer doesn't show the record to.
    pub fn opens(&self, record: &PaymentRecord) -> bool {
        record
            .commitment_hex()
            .map(|c| c == self.commitment_hex)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(amount: f64, nonce: &[u8; 32]) -> PaymentRecord {
        PaymentRecord::new(
            "loofta-pay-abc123",
            "So11111111111111111111111111111111111111112",
            amount,
            Decision::Allowed,
            1_722_600_000,
            nonce,
        )
    }

    #[test]
    fn commitment_is_stable_and_amount_sensitive() {
        let r = record(120.0, &[1u8; 32]);
        assert_eq!(r.commitment_hex().unwrap(), r.commitment_hex().unwrap());
        let mut other = r.clone();
        other.amount_usd = 121.0;
        assert_ne!(r.commitment_hex().unwrap(), other.commitment_hex().unwrap());
    }

    #[test]
    fn nonce_blinds_identical_payments() {
        // Same payment, different blinding nonce: the commitments must differ, or
        // a guessable amount could be recovered from the hash.
        let a = record(20.0, &[7u8; 32]);
        let b = record(20.0, &[9u8; 32]);
        assert_ne!(a.commitment_hex().unwrap(), b.commitment_hex().unwrap());
    }

    #[test]
    fn attests_verifies_and_opens() {
        let attestor = SigningKey::from_bytes(&[3u8; 32]);
        let r = record(120.0, &[2u8; 32]);
        let signed = r.attest(&attestor).unwrap();
        assert!(signed.verify(), "signature must be valid");
        assert!(signed.opens(&r), "the real record must open the commitment");
        assert!(
            signed.enclave_quote.is_none(),
            "enclave binding is roadmap, not shipped"
        );

        // A different record does not open it.
        let wrong = record(999.0, &[2u8; 32]);
        assert!(!signed.opens(&wrong));
    }

    #[test]
    fn tampered_signature_fails_verify() {
        let attestor = SigningKey::from_bytes(&[4u8; 32]);
        let mut signed = record(50.0, &[5u8; 32]).attest(&attestor).unwrap();
        signed.commitment_hex = "00".repeat(32);
        assert!(!signed.verify());
    }

    #[test]
    fn anchor_is_outside_the_signed_bytes() {
        let attestor = SigningKey::from_bytes(&[6u8; 32]);
        let mut signed = record(10.0, &[8u8; 32]).attest(&attestor).unwrap();
        signed.anchor = Some("5xtx...".into());
        assert!(signed.verify(), "anchoring must not break the signature");
    }
}
