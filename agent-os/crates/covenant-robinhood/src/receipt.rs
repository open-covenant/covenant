//! The Covenant Trade Receipt: a signed record of every trading decision,
//! placed or blocked. Phase 0 builds and locally signs the receipt (the
//! off-chain half) and computes the root hash the on-chain anchor will carry;
//! Phase 1 swaps the local signer for `covenant-attestation` plus the Metaplex
//! AppData write.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::types::OrderRequest;

pub const SCHEMA: &str = "covenant.robinhood.trade.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    DryRun,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Executed,
    Blocked,
    PendingApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeReceipt {
    pub schema: String,
    pub agent: String,
    pub policy_hash: String,
    pub mode: Mode,
    pub decision: Decision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub order: OrderRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venue_order_id: Option<String>,
    pub requested_at: u64,
    pub decided_at: u64,
}

impl TradeReceipt {
    pub fn new(
        agent: impl Into<String>,
        policy_hash: impl Into<String>,
        mode: Mode,
        decision: Decision,
        order: OrderRequest,
    ) -> Self {
        let ts = crate::unix_now();
        Self {
            schema: SCHEMA.to_string(),
            agent: agent.into(),
            policy_hash: policy_hash.into(),
            mode,
            decision,
            reason: None,
            order,
            reference_price: None,
            venue_order_id: None,
            requested_at: ts,
            decided_at: ts,
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn reference_price(mut self, price: f64) -> Self {
        self.reference_price = Some(price);
        self
    }

    pub fn venue_order_id(mut self, id: impl Into<String>) -> Self {
        self.venue_order_id = Some(id.into());
        self
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
    /// JCS hash and checks the signature against the published pubkey. Anyone
    /// can check it; nothing here has to be trusted.
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
    pub receipt: TradeReceipt,
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
    use crate::types::Side;

    #[test]
    fn receipt_signs_verifies_and_detects_tamper() {
        let attestor = SigningKey::from_bytes(&[3u8; 32]);
        let order = OrderRequest::market("BTC-USD", Side::Buy, 0.01);
        let signed = TradeReceipt::new(
            "agent1",
            "policyhashabc",
            Mode::DryRun,
            Decision::Executed,
            order,
        )
        .reference_price(60_000.0)
        .attest(&attestor)
        .unwrap();
        assert!(signed.verify());

        let mut tampered = signed.clone();
        tampered.receipt.order.symbol = "ETH-USD".to_string();
        assert!(!tampered.verify());
    }

    #[test]
    fn blocked_receipt_carries_reason() {
        let attestor = SigningKey::from_bytes(&[4u8; 32]);
        let order = OrderRequest::market("DOGE-USD", Side::Buy, 100_000.0);
        let signed = TradeReceipt::new("agent1", "ph", Mode::Live, Decision::Blocked, order)
            .reason("per_order_usd cap exceeded")
            .attest(&attestor)
            .unwrap();
        assert!(signed.verify());
        assert_eq!(signed.receipt.decision, Decision::Blocked);
        assert_eq!(
            signed.receipt.reason.as_deref(),
            Some("per_order_usd cap exceeded")
        );
    }
}
