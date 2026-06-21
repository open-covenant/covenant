use covenant_identity::LocalIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sign::SignedEnvelope;
use crate::{PayaiError, Result};

/// Covenant-signed proof of what was delivered for a PayAI settlement, binding
/// the on-chain settlement to the work output. Covenant never moved the money;
/// this attests the delivery the payment paid for, closing x402's gap of an
/// irreversible payment with no delivery proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkReceipt {
    pub kind: String,
    /// The settlement this receipt is bound to (Solana tx signature).
    pub settlement_tx: String,
    pub network: String,
    /// The x402 resource that was paid for.
    pub resource: String,
    /// SHA-256 of the delivered output, hex.
    pub output_hash_hex: String,
    /// Seller wallet (the issuer).
    pub seller: String,
    /// Buyer wallet, when known.
    pub buyer: Option<String>,
    pub issued_at_ms: u64,
}

impl WorkReceipt {
    pub const KIND: &'static str = "covenant_work_receipt_v1";

    /// Build a receipt, hashing `output`. `issued_at_ms` is caller-supplied (no
    /// implicit clock, so the type stays deterministic/testable).
    pub fn new(
        settlement_tx: impl Into<String>,
        network: impl Into<String>,
        resource: impl Into<String>,
        output: &[u8],
        seller: impl Into<String>,
        buyer: Option<String>,
        issued_at_ms: u64,
    ) -> Self {
        WorkReceipt {
            kind: Self::KIND.to_string(),
            settlement_tx: settlement_tx.into(),
            network: network.into(),
            resource: resource.into(),
            output_hash_hex: sha256_hex(output),
            seller: seller.into(),
            buyer,
            issued_at_ms,
        }
    }
}

/// Seller issues (signs) a receipt.
pub fn issue_receipt(identity: &LocalIdentity, receipt: &WorkReceipt) -> Result<SignedEnvelope> {
    SignedEnvelope::sign(identity, receipt)
}

/// Buyer counter-signs a seller's receipt = acceptance of delivery.
pub fn countersign(identity: &LocalIdentity, receipt: &SignedEnvelope) -> SignedEnvelope {
    SignedEnvelope::countersign(identity, receipt)
}

/// Verify a counter-signature accepts exactly the seller's receipt: both
/// signatures valid and over byte-identical payloads.
pub fn verify_countersignature(
    receipt: &SignedEnvelope,
    countersig: &SignedEnvelope,
) -> Result<()> {
    receipt.verify()?;
    countersig.verify()?;
    if receipt.payload != countersig.payload {
        return Err(PayaiError::Verify(
            "counter-signature is over a different receipt".into(),
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> WorkReceipt {
        WorkReceipt::new(
            "5xTxSig",
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "https://seller.example/api/summarize",
            b"the delivered output bytes",
            "SellerWallet",
            Some("BuyerWallet".into()),
            1_781_980_000_000,
        )
    }

    #[test]
    fn issue_then_verify() {
        let seller = LocalIdentity::generate("seller@local");
        let r = receipt();
        let env = issue_receipt(&seller, &r).unwrap();
        env.verify().unwrap();
        let back: WorkReceipt = env.deserialize().unwrap();
        assert_eq!(back, r);
        assert_eq!(back.kind, WorkReceipt::KIND);
        assert_eq!(back.output_hash_hex.len(), 64);
    }

    #[test]
    fn output_hash_is_deterministic_and_content_bound() {
        let a = WorkReceipt::new("t", "n", "r", b"alpha", "s", None, 1);
        let b = WorkReceipt::new("t", "n", "r", b"alpha", "s", None, 1);
        let c = WorkReceipt::new("t", "n", "r", b"beta", "s", None, 1);
        assert_eq!(a.output_hash_hex, b.output_hash_hex);
        assert_ne!(a.output_hash_hex, c.output_hash_hex);
    }

    #[test]
    fn countersign_accepts_matching_receipt() {
        let seller = LocalIdentity::generate("seller@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let env = issue_receipt(&seller, &receipt()).unwrap();
        let cs = countersign(&buyer, &env);
        verify_countersignature(&env, &cs).unwrap();
    }

    #[test]
    fn countersign_rejects_mismatched_receipt() {
        let seller = LocalIdentity::generate("seller@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let env = issue_receipt(&seller, &receipt()).unwrap();
        // Buyer counter-signs a DIFFERENT receipt.
        let other = issue_receipt(
            &seller,
            &WorkReceipt::new("other", "n", "r", b"x", "s", None, 2),
        )
        .unwrap();
        let cs = countersign(&buyer, &other);
        assert!(verify_countersignature(&env, &cs).is_err());
    }
}
