//! Verifier trait, registry, and policy.
//!
//! A [`Registry`] maps a type discriminator to a verifier. [`Policy`] decides
//! what happens for types nothing is registered for: accept without trust,
//! reject, or require an allowlist.

use std::collections::HashMap;

use crate::attestation::Attestation;
use crate::error::{AttestError, Result};

/// Verifier for one attestation type. Implementors declare the discriminator
/// they handle and validate the payload's shape. The signature and cert-hash
/// binding are checked by [`Registry::verify`] before `verify_payload` runs.
pub trait AttestationVerifier: Send + Sync {
    /// The discriminator this verifier handles.
    fn type_(&self) -> &str;

    /// Validate payload structure. Signature is already verified.
    fn verify_payload(&self, payload: &serde_json::Value) -> Result<()>;
}

/// What to do for attestation types with no registered verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Unknown types pass but are flagged `fully_verified = false`. Default.
    #[default]
    AcceptKnown,

    /// Every entry in `required_types` must be present and fully verified.
    RequireAll,

    /// Reject every attestation whose type is not registered.
    RejectUnknown,
}

/// Result of verifying one attestation.
#[derive(Debug, Clone)]
pub struct VerifiedAttestation {
    /// Type discriminator.
    pub type_: String,
    /// Signer's `did:key`.
    pub signer: String,
    /// `true` when a verifier ran a payload check and accepted it. `false`
    /// when the policy let an unknown type through without one.
    pub fully_verified: bool,
}

/// Verifiers keyed by type discriminator.
#[derive(Default)]
pub struct Registry {
    by_type: HashMap<String, Box<dyn AttestationVerifier>>,
    policy: Policy,
    required_types: Vec<String>,
}

impl Registry {
    /// Empty registry with the default policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a verifier, replacing any prior one for the same type.
    pub fn register(&mut self, verifier: Box<dyn AttestationVerifier>) {
        self.by_type.insert(verifier.type_().to_string(), verifier);
    }

    /// Switch policy.
    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.policy = policy;
        self
    }

    /// Type discriminators that must be present and verified under
    /// `Policy::RequireAll`. Ignored under other policies.
    pub fn require_types<I: IntoIterator<Item = String>>(mut self, types: I) -> Self {
        self.required_types = types.into_iter().collect();
        self
    }

    /// Active policy.
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// Verify signature, cert-hash binding, and (if a verifier is registered)
    /// payload structure.
    pub fn verify(
        &self,
        attestation: &Attestation,
        expected_cert_hash: [u8; 32],
    ) -> Result<VerifiedAttestation> {
        attestation.verify_signature(expected_cert_hash)?;

        let fully = match self.by_type.get(&attestation.type_) {
            Some(v) => {
                v.verify_payload(&attestation.payload)?;
                true
            }
            None => match self.policy {
                Policy::AcceptKnown => false,
                Policy::RejectUnknown | Policy::RequireAll => {
                    return Err(AttestError::UnknownType(attestation.type_.clone()));
                }
            },
        };

        Ok(VerifiedAttestation {
            type_: attestation.type_.clone(),
            signer: attestation.signer.clone(),
            fully_verified: fully,
        })
    }

    /// Verify a batch, then enforce `RequireAll`.
    pub fn verify_all(
        &self,
        attestations: &[Attestation],
        expected_cert_hash: [u8; 32],
    ) -> Result<Vec<VerifiedAttestation>> {
        let mut verified = Vec::with_capacity(attestations.len());
        for a in attestations {
            verified.push(self.verify(a, expected_cert_hash)?);
        }
        if self.policy == Policy::RequireAll {
            for required in &self.required_types {
                let present = verified
                    .iter()
                    .any(|v| &v.type_ == required && v.fully_verified);
                if !present {
                    return Err(AttestError::UnknownType(format!(
                        "required type '{required}' missing or not fully verified"
                    )));
                }
            }
        }
        Ok(verified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestation::{Attestation, AttestationPayload};
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use rand::TryRngCore;
    use serde::{Deserialize, Serialize};

    fn fresh() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[derive(Serialize, Deserialize)]
    struct Demo {
        a: String,
    }
    impl AttestationPayload for Demo {
        fn payload_type() -> &'static str {
            "demo/v1"
        }
    }

    struct DemoVerifier;
    impl AttestationVerifier for DemoVerifier {
        fn type_(&self) -> &str {
            "demo/v1"
        }
        fn verify_payload(&self, payload: &serde_json::Value) -> Result<()> {
            payload
                .get("a")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|_| ())
                .ok_or_else(|| AttestError::Payload("missing or empty 'a'".into()))
        }
    }

    fn sample_hash() -> [u8; 32] {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = (i + 1) as u8;
        }
        h
    }

    fn signed_demo(sk: &SigningKey, cert_hash: [u8; 32], a: &str) -> Attestation {
        Attestation::sign(sk, Demo { a: a.into() }, cert_hash).unwrap()
    }

    #[test]
    fn registry_accepts_registered_type() {
        let sk = fresh();
        let cert_hash = sample_hash();
        let mut reg = Registry::new();
        reg.register(Box::new(DemoVerifier));
        let att = signed_demo(&sk, cert_hash, "ok");
        let v = reg.verify(&att, cert_hash).unwrap();
        assert!(v.fully_verified);
    }

    #[test]
    fn accept_known_lets_unknown_pass_unverified() {
        let sk = fresh();
        let cert_hash = sample_hash();
        let reg = Registry::new().with_policy(Policy::AcceptKnown);
        let att = signed_demo(&sk, cert_hash, "ok");
        let v = reg.verify(&att, cert_hash).unwrap();
        assert!(!v.fully_verified);
    }

    #[test]
    fn reject_unknown_blocks_unregistered_types() {
        let sk = fresh();
        let cert_hash = sample_hash();
        let reg = Registry::new().with_policy(Policy::RejectUnknown);
        let att = signed_demo(&sk, cert_hash, "ok");
        let err = reg.verify(&att, cert_hash).unwrap_err();
        assert!(matches!(err, AttestError::UnknownType(_)));
    }

    #[test]
    fn require_all_enforces_presence() {
        let sk = fresh();
        let cert_hash = sample_hash();
        let mut reg = Registry::new()
            .with_policy(Policy::RequireAll)
            .require_types(["demo/v1".to_string()]);
        reg.register(Box::new(DemoVerifier));

        let err = reg.verify_all(&[], cert_hash).unwrap_err();
        assert!(matches!(err, AttestError::UnknownType(_)));

        let att = signed_demo(&sk, cert_hash, "ok");
        let v = reg.verify_all(&[att], cert_hash).unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn payload_check_failure_rejects() {
        let sk = fresh();
        let cert_hash = sample_hash();
        let mut reg = Registry::new();
        reg.register(Box::new(DemoVerifier));
        // Sign an empty `a` so the signature verifies but the payload check fails.
        let att = Attestation::sign(&sk, Demo { a: String::new() }, cert_hash).unwrap();
        let err = reg.verify(&att, cert_hash).unwrap_err();
        assert!(matches!(err, AttestError::Payload(_)));
    }
}
