//! The challenge that makes a quote say something.
//!
//! A TDX quote carries 64 bytes of `report_data` chosen by whoever asked for
//! it. Covenant's live ER verifier fills those bytes with
//! `crypto.randomBytes(64)` and checks the quote echoes them back, which proves
//! the quote is fresh and was produced by a genuine enclave. It proves nothing
//! about what ran inside.
//!
//! This constructs the same 64 bytes from something meaningful instead:
//!
//! ```text
//! challenge = sha512(DOMAIN || "|" || agent || "|" || subject_commitment || "|" || nonce)
//! ```
//!
//! sha512 is exactly 64 bytes, so the result drops into the existing
//! `GET /quote?challenge=<base64>` call unchanged. Nothing on the enclave side
//! has to know this crate exists, which is the property that makes the binding
//! deployable rather than a proposal.
//!
//! The nonce is what keeps the freshness the random challenge gave us. Without
//! it the challenge would be a pure function of the subject, and a quote taken
//! once could be replayed forever for that same subject. With it, a verifier
//! who holds `(agent, subject_commitment, nonce)` recomputes the 64 bytes and
//! checks the quote echoed them, which is the enclave confirming a fresh quote
//! that names this triple.
//!
//! What a match does not prove: that the agent authorized the binding. The
//! quote endpoint takes its `report_data` from the requester, so anyone who can
//! reach it can have the enclave echo any 64 bytes, including the challenge for
//! a triple they chose. So a match says the quote is genuine, fresh, and names
//! this triple, not that the agent signed off on it. Carrying an agent
//! signature over `(subject_commitment, nonce)` in the envelope would add that,
//! and is left as a deliberate later addition rather than folded in here.
//!
//! What this does not do: fetch a quote, verify a DCAP signature chain, or
//! decide whether an enclave measurement is one you trust. Those live in the
//! ER verifier against the Intel PCCS. This crate owns the one question that
//! service cannot answer, which is whether the quote it verified was about the
//! thing you care about.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use crate::error::{Error, Result};

/// Domain separator, so these 64 bytes can never collide with a challenge
/// constructed for some other purpose against the same enclave.
pub const DOMAIN: &str = "covenant.tee.challenge.v1";

/// Nonce length in bytes. 32 is the same order as the random challenge it
/// replaces; the freshness argument rests entirely on this being unpredictable
/// to whoever produces the quote.
pub const NONCE_LEN: usize = 32;

/// What a quote is being bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "BindingWire")]
#[non_exhaustive]
pub struct Binding {
    /// The agent the enclave ran for, base58.
    pub agent: String,
    /// What ran, as a 32-byte commitment in lowercase hex. Covenant never sees
    /// the thing itself: a payment record, an audit root, or a job spec all
    /// reduce to a hash before they reach here.
    pub subject_commitment: String,
    /// 32 fresh random bytes, lowercase hex. Held by whoever requested the
    /// quote and published with the attestation so a third party can recompute.
    pub nonce: String,
}

// Deserialization routes through here so the validation `new` applies also
// holds on the verifier's real input path (serde, not `new`). Without it a
// field carrying the `"|"` separator deserializes unchecked and the challenge
// preimage stops being injective.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindingWire {
    agent: String,
    subject_commitment: String,
    nonce: String,
}

impl TryFrom<BindingWire> for Binding {
    type Error = Error;
    fn try_from(w: BindingWire) -> Result<Self> {
        Binding::new(w.agent, w.subject_commitment, w.nonce)
    }
}

impl Binding {
    /// Validate and assemble a binding. `agent` is a base58 pubkey (32-44
    /// chars); `subject_commitment` and `nonce` are 32-byte values in lowercase
    /// hex. The commitment is yours to derive: hash whatever ran (a payment
    /// record, an audit root, a job spec) to 32 bytes before it reaches here.
    /// This and the `Deserialize` path are the only ways to build a `Binding`,
    /// so a constructed one is always in the form the challenge is built from.
    pub fn new(
        agent: impl Into<String>,
        subject_commitment: impl Into<String>,
        nonce: impl Into<String>,
    ) -> Result<Self> {
        let agent = agent.into();
        let subject_commitment = subject_commitment.into();
        let nonce = nonce.into();

        validate_base58("agent", &agent)?;
        validate_hex("subjectCommitment", &subject_commitment, 32)?;
        validate_hex("nonce", &nonce, NONCE_LEN)?;

        Ok(Self {
            agent,
            subject_commitment,
            nonce,
        })
    }

    /// The 64 bytes to hand the enclave as `report_data`.
    pub fn challenge(&self) -> [u8; 64] {
        let mut h = Sha512::new();
        h.update(DOMAIN.as_bytes());
        h.update(b"|");
        h.update(self.agent.as_bytes());
        h.update(b"|");
        h.update(self.subject_commitment.as_bytes());
        h.update(b"|");
        h.update(self.nonce.as_bytes());
        h.finalize().into()
    }

    /// The challenge as lowercase hex, the form `report_data` takes when the
    /// verifier reads it back off a quote.
    pub fn challenge_hex(&self) -> String {
        hex::encode(self.challenge())
    }

    /// Standard base64, the form the quote endpoint takes:
    /// `GET /quote?challenge=<base64>`. It can contain `+`, `/`, and `=`, so
    /// percent-encode it before placing it in a query string.
    pub fn challenge_base64(&self) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        STANDARD.encode(self.challenge())
    }

    /// Whether a quote's `report_data` is the challenge this binding produces.
    ///
    /// Accepts the `0x` prefix and either case, because the field arrives from
    /// several decoders and rejecting a correct value over formatting would
    /// read as a failed binding.
    pub fn matches_report_data(&self, report_data: &str) -> bool {
        let lowered = report_data.to_ascii_lowercase();
        let observed = lowered.strip_prefix("0x").unwrap_or(&lowered);
        observed == self.challenge_hex()
    }
}

fn validate_hex(name: &str, s: &str, bytes: usize) -> Result<()> {
    let want = bytes * 2;
    if s.len() != want {
        return Err(Error::Binding(format!(
            "{name} must be {want} hex characters ({bytes} bytes), got {}",
            s.len()
        )));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(Error::Binding(format!(
            "{name} must be lowercase ASCII hex"
        )));
    }
    Ok(())
}

fn validate_base58(name: &str, s: &str) -> Result<()> {
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if !(32..=44).contains(&s.len()) {
        return Err(Error::Binding(format!(
            "{name} must be a base58 pubkey of 32-44 characters, got {}",
            s.len()
        )));
    }
    if !s.bytes().all(|b| BASE58.contains(&b)) {
        return Err(Error::Binding(format!("{name} must be base58")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";
    const OTHER_AGENT: &str = "24i43XkDyJAJJBi7X3ARRCt3WBh16uJuSfVRLKXVEYBQ";

    fn binding() -> Binding {
        Binding::new(AGENT, "a".repeat(64), "b".repeat(64)).unwrap()
    }

    #[test]
    fn the_challenge_fills_report_data_exactly() {
        // TDX report_data is 64 bytes. A shorter digest would need padding,
        // and padding is where a binding gets ambiguous.
        assert_eq!(binding().challenge().len(), 64);
        assert_eq!(binding().challenge_hex().len(), 128);
    }

    #[test]
    fn the_same_binding_always_produces_the_same_challenge() {
        assert_eq!(binding().challenge(), binding().challenge());
    }

    #[test]
    fn every_input_changes_the_challenge() {
        let base = binding().challenge();
        let cases = [
            Binding::new(OTHER_AGENT, "a".repeat(64), "b".repeat(64)).unwrap(),
            Binding::new(AGENT, "c".repeat(64), "b".repeat(64)).unwrap(),
            Binding::new(AGENT, "a".repeat(64), "d".repeat(64)).unwrap(),
        ];
        for c in cases {
            assert_ne!(base, c.challenge(), "{c:?} collided with the base binding");
        }
    }

    #[test]
    fn a_quote_for_another_payment_does_not_match() {
        let paid = binding();
        let report_data = paid.challenge_hex();

        // Same agent, same nonce, different subject: this is the replay the
        // random challenge could not catch.
        let other = Binding::new(AGENT, "c".repeat(64), "b".repeat(64)).unwrap();
        assert!(paid.matches_report_data(&report_data));
        assert!(!other.matches_report_data(&report_data));
    }

    #[test]
    fn a_stale_quote_does_not_match_a_fresh_nonce() {
        let old = binding();
        let fresh = Binding::new(AGENT, "a".repeat(64), "e".repeat(64)).unwrap();
        assert!(!fresh.matches_report_data(&old.challenge_hex()));
    }

    #[test]
    fn report_data_is_read_in_the_forms_decoders_actually_emit() {
        let b = binding();
        let hex = b.challenge_hex();
        assert!(b.matches_report_data(&hex));
        assert!(b.matches_report_data(&format!("0x{hex}")));
        assert!(b.matches_report_data(&hex.to_uppercase()));
        assert!(b.matches_report_data(&format!("0X{}", hex.to_uppercase())));
    }

    #[test]
    fn a_truncated_or_empty_report_data_never_matches() {
        let b = binding();
        let hex = b.challenge_hex();
        for bad in ["", "0x", &hex[..126], &format!("{hex}00")] {
            assert!(!b.matches_report_data(bad), "{bad} matched");
        }
    }

    #[test]
    fn the_base64_form_round_trips_to_the_same_bytes() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let b = binding();
        let decoded = STANDARD.decode(b.challenge_base64()).unwrap();
        assert_eq!(decoded, b.challenge().to_vec());
    }

    #[test]
    fn malformed_inputs_are_refused_at_construction() {
        assert!(Binding::new("not-base58!!", "a".repeat(64), "b".repeat(64)).is_err());
        assert!(Binding::new(AGENT, "a".repeat(63), "b".repeat(64)).is_err());
        assert!(Binding::new(AGENT, "A".repeat(64), "b".repeat(64)).is_err());
        assert!(Binding::new(AGENT, "z".repeat(64), "b".repeat(64)).is_err());
        // The nonce is 32 bytes, not 64.
        assert!(Binding::new(AGENT, "a".repeat(64), "b".repeat(128)).is_err());
    }

    #[test]
    fn deserialization_is_validated_like_construction() {
        // The verifier reaches a Binding through serde, not new(). A field
        // carrying the separator would break preimage injectivity, so the wire
        // path has to refuse it too.
        let injected = format!(
            r#"{{"agent":"{AGENT}","subjectCommitment":"a|","nonce":"{}"}}"#,
            "b".repeat(64)
        );
        assert!(serde_json::from_str::<Binding>(&injected).is_err());

        let well_formed = format!(
            r#"{{"agent":"{AGENT}","subjectCommitment":"{}","nonce":"{}"}}"#,
            "a".repeat(64),
            "b".repeat(64)
        );
        let b: Binding = serde_json::from_str(&well_formed).unwrap();
        assert_eq!(b, binding());
    }

    #[test]
    fn the_challenge_matches_the_javascript_construction() {
        // Canary against the live requester in examples/magicblock/bound-quote.mjs.
        // The enclave is asked for a quote by JS and the result is checked in
        // Rust, so the two constructions have to agree byte for byte or a
        // correctly bound quote reads as unbound.
        let b = Binding::new(AGENT, "a".repeat(64), "b".repeat(64)).unwrap();
        assert_eq!(
            b.challenge_hex(),
            "fadd468d8ba05f91c1630cb81d0761cbe39ac21269d7366a992d1bf53a36502b\
             48986c5ea9a963f867f953784fd0f9a424339c92e53306fffb0018a85a26c8aa"
                .replace(char::is_whitespace, "")
        );
    }

    #[test]
    fn the_domain_separator_is_part_of_the_preimage() {
        // A challenge built without the domain tag must not equal ours, or the
        // same agent and subject could be bound by an unrelated protocol.
        let b = binding();
        let mut h = Sha512::new();
        h.update(b.agent.as_bytes());
        h.update(b"|");
        h.update(b.subject_commitment.as_bytes());
        h.update(b"|");
        h.update(b.nonce.as_bytes());
        let undomained: [u8; 64] = h.finalize().into();
        assert_ne!(b.challenge(), undomained);
    }
}
