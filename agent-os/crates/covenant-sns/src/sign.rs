//! SNS write requests and the signer interface.
//!
//! The daemon never holds the parent-domain key. Write tools build a
//! [`SignerRequest`] and hand it to a [`SnsSigner`]; the daemon's
//! implementation drives the `covenant-sns-signer` sidecar, which holds the
//! key and builds/submits the actual SNS instructions (sub-registrar for
//! subdomains, sns-records for records). This crate stays `solana-sdk`-free.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A write the signer sidecar should build, sign, and submit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignerRequest {
    /// Issue `<subdomain>.<parent>` bound to `owner`, under the parent the
    /// sidecar holds the key for.
    RegisterSubdomain {
        parent: String,
        subdomain: String,
        owner: String,
    },
    /// Write a typed record on a domain Covenant controls.
    SetRecord {
        domain: String,
        record: String,
        value: String,
    },
}

/// The sidecar's reply: the confirmed signature plus the affected name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerResponse {
    pub signature: String,
    pub domain: String,
    pub cluster: String,
}

/// The write side of the profile: hand a built request to whatever holds the
/// parent-domain key. The daemon drives the sidecar subprocess; tests stand in.
#[async_trait]
pub trait SnsSigner: Send + Sync {
    async fn sign(&self, request: SignerRequest) -> Result<SignerResponse, String>;
}

/// A `.sol` label is 1..=32 chars and carries no dot (a dot would cross a
/// domain boundary). Validated before a request reaches the sidecar.
pub fn validate_label(label: &str) -> Result<(), String> {
    if label.is_empty() || label.len() > 32 {
        return Err(format!("label must be 1..=32 chars, got {}", label.len()));
    }
    if label.contains('.') {
        return Err("label must not contain a dot".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_serde() {
        let r = SignerRequest::RegisterSubdomain {
            parent: "covenant.sol".into(),
            subdomain: "foundation".into(),
            owner: "OwnerWallet".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<SignerRequest>(&json).unwrap(), r);
        assert!(json.contains("\"kind\":\"register_subdomain\""));
    }

    #[test]
    fn label_validation() {
        assert!(validate_label("foundation").is_ok());
        assert!(validate_label("").is_err());
        assert!(validate_label(&"a".repeat(33)).is_err());
        assert!(validate_label("a.b").is_err());
    }
}
