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
//! The enclave binding is live substrate, not a promise. Loofta's rail is a
//! MagicBlock Private Ephemeral Rollup, an Intel TDX enclave; Covenant already
//! DCAP-verifies those enclaves on mainnet (the verified-ER attestation in the
//! Solana Attestation Service, the `covenant_verify_enclave` MCP tool, and a paid
//! x402 check), with the running verifier in `services/er-registry/tee.mjs`.
//! [`EnclaveAttestation`] carries that DCAP result and [`EnclaveAttestation::binds`]
//! checks the quote committed to this exact payment. The one packaging gap is a
//! `covenant-tee` crate that folds the agent and provenance root into the quote
//! challenge as a first-class signed attestation; the binding itself is
//! exercisable today through the live endpoint.

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
            enclave: None,
            anchor: None,
        })
    }
}

/// A DCAP verification result for the TDX enclave a payment ran in. Mirrors what
/// `services/er-registry/tee.mjs` returns and what the verified-ER attestation
/// carries. The daemon fills this from a live `covenant_verify_enclave` /
/// verified-ER read; it is DCAP-verifiable on its own, so it rides outside the
/// attestor signature like [`SignedCommitment::anchor`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnclaveAttestation {
    /// ER validator identity (the enclave's stable pubkey), base58.
    pub validator: String,
    /// DCAP TCB status, e.g. "UpToDate".
    pub tcb_status: String,
    /// TDX measurement (`mr_td`), hex: identifies the enclave image.
    pub mr_td_hex: String,
    /// Intel advisory ids; empty when the TCB is clean.
    #[serde(default)]
    pub advisory_ids: Vec<String>,
    /// The quote's `report_data`, hex. The enclave binds whatever challenge it
    /// was handed here; a payment-bound quote carries this binding's challenge.
    pub report_data_hex: String,
    pub verified_at: u64,
    /// The binding the quote was requested with: its `subject_commitment` is
    /// this payment's commitment, and its challenge is what the enclave signed
    /// into `report_data`. Recomputing the challenge and matching it is the tie
    /// between a genuine enclave and this specific payment.
    pub binding: covenant_tee::Binding,
}

impl EnclaveAttestation {
    /// The enclave quote committed to this payment. Two things must hold: the
    /// binding is for this payment's commitment, and its challenge is the value
    /// the enclave actually signed into `report_data`. The challenge is
    /// `sha512(domain | agent | commitment | nonce)`, recomputed and matched by
    /// [`covenant_tee::Binding`], so this is a full binding check, not a
    /// substring of `report_data`.
    pub fn binds(&self, commitment_hex: &str) -> bool {
        !commitment_hex.is_empty()
            && self
                .binding
                .subject_commitment
                .eq_ignore_ascii_case(commitment_hex)
            && self.binding.matches_report_data(&self.report_data_hex)
    }

    /// The TCB is current with no outstanding advisories.
    pub fn is_up_to_date(&self) -> bool {
        self.tcb_status.eq_ignore_ascii_case("UpToDate") && self.advisory_ids.is_empty()
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
    /// DCAP attestation of the enclave the payment ran in. Independently
    /// verifiable, so it sits outside the signature. `None` until the daemon
    /// attaches a live verify with [`SignedCommitment::with_enclave`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclave: Option<EnclaveAttestation>,
    /// On-chain anchor reference, set once the daemon folds the commitment into
    /// the provenance root. Outside the signed bytes, so it never affects
    /// [`Self::verify`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

impl SignedCommitment {
    /// Attach the DCAP attestation of the enclave the payment ran in. The daemon
    /// calls this after a live verify; the attestation is independently
    /// DCAP-checkable, so it does not change the commitment signature.
    pub fn with_enclave(mut self, enclave: EnclaveAttestation) -> Self {
        self.enclave = Some(enclave);
        self
    }

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

    /// Fully proven: the signature holds, the enclave attestation is present and
    /// current, and its quote committed to this payment. A payer showing this,
    /// plus the record, proves a specific private payment ran in a genuine
    /// verified enclave.
    pub fn enclave_proven(&self) -> bool {
        self.verify()
            && self
                .enclave
                .as_ref()
                .is_some_and(|e| e.is_up_to_date() && e.binds(&self.commitment_hex))
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

    fn enclave_for(commitment_hex: &str) -> EnclaveAttestation {
        let binding = covenant_tee::Binding::new(
            "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo",
            commitment_hex,
            "11".repeat(32),
        )
        .expect("valid binding");
        EnclaveAttestation {
            validator: "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo".into(),
            tcb_status: "UpToDate".into(),
            mr_td_hex: "ab".repeat(48),
            advisory_ids: vec![],
            report_data_hex: binding.challenge_hex(),
            verified_at: 1_722_600_050,
            binding,
        }
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
            signed.enclave.is_none(),
            "enclave is attached by the daemon, not at attest time"
        );

        // A different record does not open it.
        let wrong = record(999.0, &[2u8; 32]);
        assert!(!signed.opens(&wrong));
    }

    #[test]
    fn enclave_binds_the_commitment() {
        let commitment = "a".repeat(64);
        let e = enclave_for(&commitment);
        assert!(e.is_up_to_date());
        assert!(e.binds(&commitment), "report_data carries the commitment");
        assert!(
            !e.binds(&"b".repeat(64)),
            "a different commitment does not bind"
        );
        assert!(!e.binds(""), "empty commitment never binds");
    }

    #[test]
    fn binding_requires_report_data_to_carry_the_challenge() {
        // The commitment matches, but report_data is not the binding's
        // challenge. binds() must fail on the covenant-tee match, not pass on
        // the commitment equality alone.
        let commitment = "a".repeat(64);
        let mut e = enclave_for(&commitment);
        assert!(e.binds(&commitment));
        e.report_data_hex = "ff".repeat(64);
        assert!(
            !e.binds(&commitment),
            "right commitment but wrong challenge in report_data must not bind"
        );
    }

    #[test]
    fn outdated_tcb_is_not_up_to_date() {
        let mut e = enclave_for(&"a".repeat(64));
        e.advisory_ids = vec!["INTEL-SA-00001".into()];
        assert!(!e.is_up_to_date(), "an outstanding advisory is not clean");
        e.advisory_ids = vec![];
        e.tcb_status = "OutOfDate".into();
        assert!(!e.is_up_to_date());
    }

    #[test]
    fn enclave_proven_requires_signature_current_tcb_and_binding() {
        let attestor = SigningKey::from_bytes(&[11u8; 32]);
        let r = record(120.0, &[12u8; 32]);
        let signed = r.attest(&attestor).unwrap();
        assert!(!signed.enclave_proven(), "no enclave attached yet");

        let bound = signed
            .clone()
            .with_enclave(enclave_for(&signed.commitment_hex));
        assert!(bound.verify() && bound.opens(&r));
        assert!(
            bound.enclave_proven(),
            "signed, current TCB, and quote binds the payment"
        );

        // An enclave that verified a different payment does not prove this one:
        // its binding commits to a different commitment, so the challenge in
        // report_data is a different value.
        let mismatched = signed.with_enclave(enclave_for(&"c".repeat(64)));
        assert!(!mismatched.enclave_proven());
    }

    #[test]
    fn tampered_signature_fails_verify() {
        let attestor = SigningKey::from_bytes(&[4u8; 32]);
        let mut signed = record(50.0, &[5u8; 32]).attest(&attestor).unwrap();
        signed.commitment_hex = "00".repeat(32);
        assert!(!signed.verify());
    }

    #[test]
    fn anchor_and_enclave_are_outside_the_signed_bytes() {
        let attestor = SigningKey::from_bytes(&[6u8; 32]);
        let signed = record(10.0, &[8u8; 32]).attest(&attestor).unwrap();
        let enclave = enclave_for(&signed.commitment_hex);
        let mut signed = signed.with_enclave(enclave);
        signed.anchor = Some("5xtx...".into());
        assert!(
            signed.verify(),
            "attaching enclave + anchor must not break the signature"
        );
    }
}
