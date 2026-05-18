//! Payment signing trait.
//!
//! The x402 client doesn't know how to sign Solana transactions or
//! produce the `x-payment` header value. Real signing lives in the
//! daemon, which owns the funding key. This module exposes a
//! [`Signer`] trait the daemon implements and a [`MockSigner`] for
//! tests in this crate.

use async_trait::async_trait;

use crate::{Result, types::PaymentRequirements};

/// Constructs the `x-payment` header value for a chosen payment
/// option.
///
/// Implementations hold the funding key, build the on-chain
/// transaction (typically a SPL token transfer to
/// `requirements.pay_to`), sign it, and return the base64-encoded
/// payment payload the x402 spec expects in the header.
#[async_trait]
pub trait Signer: Send + Sync {
    async fn build_payment(
        &self,
        requirements: &PaymentRequirements,
    ) -> Result<String>;
}

/// Deterministic mock signer for tests.
///
/// Returns a header value of the form `mock:{network}:{amount}`.
/// The real client loop is what's under test here; the signer's job
/// is covered by integration tests in the daemon once the Solana
/// signer lands.
pub struct MockSigner;

#[async_trait]
impl Signer for MockSigner {
    async fn build_payment(
        &self,
        requirements: &PaymentRequirements,
    ) -> Result<String> {
        Ok(format!(
            "mock:{}:{}",
            requirements.network, requirements.amount
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_signer_emits_deterministic_header() {
        let signer = MockSigner;
        let req = PaymentRequirements {
            network: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into(),
            asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            amount: "80000".into(),
            amount_usdc: 0.08,
            pay_to: "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA".into(),
            scheme: "exact".into(),
        };
        let header = signer.build_payment(&req).await.expect("sign");
        assert_eq!(
            header,
            "mock:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:80000"
        );
    }
}
