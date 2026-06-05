//! Anchor audit roots into SAP.
//!
//! Two complementary on-chain paths:
//!
//! - [`SapBridge::publish_audit_root`] appends the root to a
//!   self-anchored SAP ledger (the daemon signs for its own agent). The
//!   ledger is append-only, so it is the right vehicle for the evolving
//!   trail of successive roots, and its PDA is the public handle
//!   external parties follow.
//! - [`SapBridge::publish_attestation`] uses SAP's `create_attestation`
//!   primitive, signed by a *separately-keyed* verifier (the Witness
//!   Loop). The program enforces attester != agent owner on-chain, so
//!   this is a genuine third-party vouch for the daemon's agent + a
//!   given root — not a self-anchored write.
//!
//! In both paths only the 32-byte Merkle root and a small structured
//! envelope go on-chain — never the underlying audit-log contents.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::{worker, BridgeError, Result};

/// Number of lowercase ASCII hex characters in a valid 32-byte audit
/// root. The on-chain `content_hash` is a 32-byte digest; its hex form
/// is exactly twice that. Anything else is rejected here so a typo or
/// truncated digest fails BEFORE a subprocess round-trip.
const ROOT_HASH_HEX_LEN: usize = 64;

/// Reject anything that isn't exactly 64 lowercase ASCII hex characters
/// — the wire form the on-chain ledger demands. The worker also
/// validates this, but a JS-side rejection costs a full
/// spawn/serialize/parse round-trip; rejecting here is cheap and
/// surfaces a structured `BridgeError::Invalid` instead of an opaque
/// `BridgeError::Rpc("InvalidArgument:...")` from the SDK.
fn validate_root_hash_hex(s: &str) -> Result<()> {
    if s.len() != ROOT_HASH_HEX_LEN {
        return Err(BridgeError::Invalid(format!(
            "root_hash_hex must be {ROOT_HASH_HEX_LEN} hex characters, got {}",
            s.len()
        )));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(BridgeError::Invalid(
            "root_hash_hex must be lowercase ASCII hex only".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRootAttestation {
    /// 32-byte Merkle root as lowercase hex (goes on-chain as the
    /// ledger entry's `content_hash`). The worker rejects anything that
    /// is not exactly 32 bytes.
    pub root_hash_hex: String,
    pub release_target: String,
    pub release_subject: String,
    pub release_scope: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedAuditRoot {
    /// The ledger PDA the root was appended to. Stable across roots, so
    /// it doubles as the public handle to the daemon's audit trail.
    pub ledger_pda: String,
    pub signature: String,
}

/// Cross-party attestation request: a separately-keyed verifier vouches
/// for the daemon's agent and a given root via SAP's `create_attestation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAttestation {
    /// The agent PDA being attested (the daemon's registered SAP agent).
    pub agent_pda: String,
    /// 32-byte Merkle root as lowercase hex; goes on-chain as the
    /// attestation's `metadata_hash`.
    pub root_hash_hex: String,
    /// On-chain `attestation_type` label (<= 32 chars). The worker
    /// defaults this to "covenant.audit-root" when omitted.
    #[serde(default)]
    pub attestation_type: Option<String>,
    /// Unix expiry; `0` / `None` means never-expires.
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedAttestation {
    /// The attestation PDA, derived from [sap_attest, agent, attester].
    pub attestation_pda: String,
    /// The verifier wallet that signed the attestation.
    pub attester: String,
    pub agent_pda: String,
    pub signature: String,
}

impl SapBridge {
    pub async fn publish_audit_root(
        &self,
        attestation: &AuditRootAttestation,
    ) -> Result<PublishedAuditRoot> {
        self.require_enabled()?;
        validate_root_hash_hex(&attestation.root_hash_hex)?;
        worker::invoke(self.config(), "attest-root", attestation).await
    }

    /// Publish a cross-party attestation. Drives the worker's
    /// `attest-agent` command, which signs with the verifier keypair
    /// (`COVENANT_SAP_VERIFIER_KEYPAIR`). The root is pre-validated here
    /// so a malformed digest fails before a subprocess round-trip.
    pub async fn publish_attestation(
        &self,
        attestation: &AgentAttestation,
    ) -> Result<PublishedAttestation> {
        self.require_enabled()?;
        validate_root_hash_hex(&attestation.root_hash_hex)?;
        worker::invoke(self.config(), "attest-agent", attestation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_root_hash_hex_accepts_canonical_lowercase_64_char_hex() {
        validate_root_hash_hex(&"a".repeat(64)).expect("lowercase a*64");
        validate_root_hash_hex(&"0".repeat(64)).expect("zeroes");
        validate_root_hash_hex(&"abcdef0123456789".repeat(4)).expect("mixed lowercase hex");
    }

    #[test]
    fn validate_root_hash_hex_rejects_wrong_length_before_worker_spawn() {
        // Catches a truncated digest (e.g., a u32 that was hex-encoded as
        // 8 chars and accidentally passed through); the worker would
        // otherwise spend a full subprocess round-trip rejecting it.
        for too_short in ["", &"a".repeat(63), &"a".repeat(32)] {
            let err = validate_root_hash_hex(too_short).expect_err("must reject short hex");
            assert!(
                matches!(err, BridgeError::Invalid(_)),
                "wrong-length hex must surface as Invalid (not Rpc / Worker / Decode): {err:?}"
            );
        }
        let too_long = "a".repeat(65);
        let err = validate_root_hash_hex(&too_long).expect_err("must reject long hex");
        assert!(matches!(err, BridgeError::Invalid(_)));
    }

    #[test]
    fn validate_root_hash_hex_rejects_uppercase_and_non_hex_characters() {
        // Uppercase: the chain ledger pins lowercase; even a single
        // uppercase char would mismatch a downstream byte-for-byte
        // comparison against the on-chain `content_hash` hex.
        let upper = format!("{}A{}", "0".repeat(62), "");
        let err = validate_root_hash_hex(&upper).expect_err("must reject uppercase");
        assert!(matches!(err, BridgeError::Invalid(_)));
        // Non-hex: catches a Display-encoding accident (e.g. base58, base64)
        // that lands a non-hex char in the wire payload.
        let non_hex = format!("{}z{}", "0".repeat(62), "");
        let err = validate_root_hash_hex(&non_hex).expect_err("must reject non-hex");
        assert!(matches!(err, BridgeError::Invalid(_)));
    }
}
