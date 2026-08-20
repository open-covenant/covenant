//! Daemon glue for enclave-bound attestation.
//!
//! `covenant-tee` is the pure, offline half: it builds the 64-byte `report_data`
//! challenge that names an agent and a subject, and it checks a returned quote
//! against that binding. It deliberately does not fetch quotes, run DCAP, or
//! decide which `mr_td` measurement is one you trust, those are the ER
//! verifier's job. This wrapper adds the one piece the daemon owns: a policy
//! gate over the trusted enclave images, so a genuine, correctly-bound quote
//! from an image the operator never approved still fails verification.
//!
//! It stays inference-agnostic on purpose. `subject_commitment` is an opaque
//! hex string the caller derives ([`CovenantTee::commit`] is offered for the
//! common `hex(sha256(work))` case, but any 32-byte commitment works), so the
//! inference gateway can bind a receipt hash or a `model_digest || output_hash`
//! without this module depending on the inference-protocol crates.
//!
//! Seam: an HTTP endpoint that hands the base64 challenge to the enclave's
//! `GET /quote?challenge=` and feeds the ER verifier's DCAP result back into
//! [`CovenantTee::verify`] plugs in here later. This pass keeps that surface out
//! of `http.rs` and the IPC `Request`/`Response` enums.

use std::collections::BTreeSet;

use covenant_tee::{Binding, BoundQuote, NONCE_LEN};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The daemon's allow-list of enclave images it will accept a quote from. The
/// binding and DCAP checks live in `covenant-tee`; the trust decision over
/// `mr_td` is the operator's, so it lives here.
#[derive(Debug, Clone)]
pub struct CovenantTee {
    trusted_mr_td: BTreeSet<String>,
}

/// A binding plus the base64 challenge to hand the enclave. Keep the `binding`:
/// [`CovenantTee::verify`] needs it to recompute the challenge the quote must
/// echo, and it carries the nonce that has to be published with the attestation.
#[derive(Debug, Clone)]
pub struct BoundChallenge {
    pub binding: Binding,
    /// `report_data` in the form the quote endpoint takes; percent-encode before
    /// placing it in the query string (it can contain `+`, `/`, `=`).
    pub challenge_base64: String,
}

/// The DCAP fields the ER verifier returns for a quote, as this daemon consumes
/// them. Advisory handling stays with the verifier's TCB status for now; when
/// the verifier surface is wired, advisory ids fold in alongside these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DcapResult {
    pub validator: String,
    pub tcb_status: String,
    pub mr_td: String,
    pub report_data: String,
}

/// The three questions kept apart, because they fail for different reasons: a
/// stale platform is a fleet problem, a binding mismatch is an integration bug,
/// and an untrusted image is a policy decision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundVerdict {
    /// The platform is genuine and current (`covenant-tee`'s TCB half).
    pub enclave_ok: bool,
    /// The quote's `report_data` is the challenge this binding produces.
    pub binding_ok: bool,
    /// `mr_td` is on the daemon's allow-list.
    pub trusted_mr_td: bool,
}

impl BoundVerdict {
    pub fn ok(&self) -> bool {
        self.enclave_ok && self.binding_ok && self.trusted_mr_td
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TeeError {
    #[error(transparent)]
    Tee(#[from] covenant_tee::Error),
    #[error("nonce generation failed: {0}")]
    Rng(String),
}

impl CovenantTee {
    pub fn new(trusted_mr_td: impl IntoIterator<Item = String>) -> Self {
        // Normalise case and an optional `0x` prefix on the way in so a correct
        // image can't miss the gate over formatting alone; `mr_td` arrives from
        // several decoders and the verifier emits it prefix-less lowercase.
        Self {
            trusted_mr_td: trusted_mr_td
                .into_iter()
                .map(|m| {
                    let lower = m.to_ascii_lowercase();
                    lower.strip_prefix("0x").unwrap_or(&lower).to_owned()
                })
                .collect(),
        }
    }

    /// Canonical subject commitment over arbitrary work bytes: `hex(sha256(work))`.
    /// sha256 is 32 bytes, exactly the commitment width a [`Binding`] expects.
    /// The caller may substitute any 32-byte hex commitment; this is the default.
    pub fn commit(work: &[u8]) -> String {
        hex::encode(Sha256::digest(work))
    }

    /// A fresh random nonce plus the binding and base64 challenge it produces.
    /// The nonce is what keeps the freshness the random challenge used to give:
    /// it must be unpredictable before the quote is produced.
    pub fn challenge(
        &self,
        agent: &str,
        subject_commitment: &str,
    ) -> Result<BoundChallenge, TeeError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).map_err(|e| TeeError::Rng(e.to_string()))?;
        self.challenge_with_nonce(agent, subject_commitment, &hex::encode(nonce))
    }

    /// The deterministic core of [`challenge`](Self::challenge), with the nonce
    /// supplied. Split out so tests (and any caller replaying a published nonce)
    /// get a reproducible challenge.
    pub fn challenge_with_nonce(
        &self,
        agent: &str,
        subject_commitment: &str,
        nonce: &str,
    ) -> Result<BoundChallenge, TeeError> {
        let binding = Binding::new(agent, subject_commitment, nonce)?;
        let challenge_base64 = binding.challenge_base64();
        Ok(BoundChallenge {
            binding,
            challenge_base64,
        })
    }

    /// Verify a DCAP result against the binding it was requested under. The
    /// enclave and binding halves are `covenant-tee`'s own check (not
    /// reimplemented here); the `mr_td` allow-list is the gate the crate omits.
    pub fn verify(&self, binding: &Binding, dcap: &DcapResult) -> BoundVerdict {
        // Reconstruct the crate's envelope so its TCB and binding logic stay the
        // single source of truth. `advisory_ids`/`verified_at` are not consulted
        // by `check()` here: the verifier folds advisories into `tcb_status`.
        let quote = BoundQuote::new(
            binding.clone(),
            dcap.validator.clone(),
            dcap.tcb_status.clone(),
            dcap.mr_td.clone(),
            Vec::new(),
            dcap.report_data.clone(),
            0,
        );
        let check = quote.check();
        BoundVerdict {
            enclave_ok: check.tcb_current,
            binding_ok: check.bound,
            trusted_mr_td: self
                .trusted_mr_td
                .contains(&dcap.mr_td.to_ascii_lowercase()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use covenant_tee::TCB_UP_TO_DATE;

    const AGENT: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";
    const VALIDATOR: &str = "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo";

    fn nonce() -> String {
        "b".repeat(64)
    }

    fn mr_td() -> String {
        format!("feb748{}", "0".repeat(90))
    }

    #[test]
    fn commit_is_deterministic_and_changes_with_input() {
        assert_eq!(CovenantTee::commit(b"same"), CovenantTee::commit(b"same"));
        assert_ne!(CovenantTee::commit(b"a"), CovenantTee::commit(b"b"));
        // hex(sha256) is 64 chars, exactly a binding's subject_commitment width.
        assert_eq!(CovenantTee::commit(b"anything").len(), 64);
    }

    #[test]
    fn challenge_fills_report_data_and_is_reproducible_with_a_fixed_nonce() {
        let tee = CovenantTee::new([]);
        let commitment = CovenantTee::commit(b"receipt-bytes");

        let a = tee
            .challenge_with_nonce(AGENT, &commitment, &nonce())
            .unwrap();
        let b = tee
            .challenge_with_nonce(AGENT, &commitment, &nonce())
            .unwrap();
        assert_eq!(a.challenge_base64, b.challenge_base64);
        assert_eq!(STANDARD.decode(&a.challenge_base64).unwrap().len(), 64);

        // The random path still fills the 64 bytes exactly.
        let r = tee.challenge(AGENT, &commitment).unwrap();
        assert_eq!(STANDARD.decode(&r.challenge_base64).unwrap().len(), 64);
        // ...and its nonce is fresh, so it is not the fixed-nonce challenge.
        assert_ne!(r.challenge_base64, a.challenge_base64);
    }

    #[test]
    fn a_matching_report_data_binds_and_a_wrong_one_does_not() {
        let tee = CovenantTee::new([mr_td()]);
        let bound = tee
            .challenge_with_nonce(AGENT, &CovenantTee::commit(b"the-work"), &nonce())
            .unwrap();

        // The enclave echoes the challenge back as report_data (hex form).
        let good = DcapResult {
            validator: VALIDATOR.into(),
            tcb_status: TCB_UP_TO_DATE.into(),
            mr_td: mr_td(),
            report_data: bound.binding.challenge_hex(),
        };
        let v = tee.verify(&bound.binding, &good);
        assert!(v.binding_ok);
        assert!(v.enclave_ok);
        assert!(v.trusted_mr_td);
        assert!(v.ok());

        let wrong = DcapResult {
            report_data: "00".repeat(64),
            ..good.clone()
        };
        let v = tee.verify(&bound.binding, &wrong);
        assert!(!v.binding_ok, "a quote about another subject must not bind");
        assert!(
            v.enclave_ok,
            "a binding mismatch says nothing about the TCB"
        );
        assert!(!v.ok());
    }

    #[test]
    fn an_untrusted_image_or_stale_tcb_fails_only_its_own_gate() {
        let tee = CovenantTee::new([mr_td()]);
        let bound = tee
            .challenge_with_nonce(AGENT, &CovenantTee::commit(b"job"), &nonce())
            .unwrap();
        let report_data = bound.binding.challenge_hex();

        // Genuine, bound, current, but the image is not on the allow-list.
        let untrusted = DcapResult {
            validator: VALIDATOR.into(),
            tcb_status: TCB_UP_TO_DATE.into(),
            mr_td: "ff".repeat(48),
            report_data: report_data.clone(),
        };
        let v = tee.verify(&bound.binding, &untrusted);
        assert!(
            v.enclave_ok && v.binding_ok,
            "enclave and binding still hold"
        );
        assert!(!v.trusted_mr_td, "the daemon never approved this image");
        assert!(!v.ok());

        // Trusted image, correctly bound, but the platform is behind.
        let stale = DcapResult {
            validator: VALIDATOR.into(),
            tcb_status: "OutOfDate".into(),
            mr_td: mr_td(),
            report_data,
        };
        let v = tee.verify(&bound.binding, &stale);
        assert!(!v.enclave_ok, "an out-of-date TCB is not a current enclave");
        assert!(v.binding_ok && v.trusted_mr_td);
        assert!(!v.ok());
    }
}
