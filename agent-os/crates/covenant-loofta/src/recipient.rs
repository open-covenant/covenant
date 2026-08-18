//! The recipient's Covenant standing, and the oracle that supplies it.
//!
//! The gate runs on the destination pubkey a Loofta payout resolves to. What it
//! needs is a compact trust read for that pubkey: a reputation score, whether
//! the recipient has any Covenant history at all, and whether it carries an
//! abuse flag. Covenant already produces this (audit-derived reputation, the
//! on-chain reputation registry, and the deny signals surfaced through
//! `covenant_reputation`); [`RecipientOracle`] is the seam the daemon fills with
//! that source. The crate ships the contract and a static oracle for tests and
//! dry runs; the live read is wired in `covenantd`, not here, so the policy
//! stays pure and testable.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipientStanding {
    /// Destination Solana pubkey (base58) the payout routes to.
    pub recipient: String,
    /// Covenant reputation, 0-100. Meaningful only when `established`.
    pub score: u8,
    /// Whether Covenant has any history for this recipient. On Loofta most
    /// destinations are fresh (the privacy layer unlinks them), so `false` is
    /// common and not itself a red flag.
    pub established: bool,
    /// The recipient is on a Covenant abuse or deny signal. A hard refuse.
    pub flagged: bool,
    /// Trust label for the read, e.g. "covenant-reputation (mainnet)".
    pub source: String,
}

impl RecipientStanding {
    /// A recipient with no Covenant history: unestablished, unscored, unflagged.
    pub fn unknown(recipient: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            recipient: recipient.into(),
            score: 0,
            established: false,
            flagged: false,
            source: source.into(),
        }
    }
}

/// Reads a recipient's Covenant standing. The daemon's implementation calls the
/// live reputation source; tests and dry runs use [`StaticOracle`].
#[async_trait]
pub trait RecipientOracle: Send + Sync {
    async fn standing(&self, recipient: &str) -> Result<RecipientStanding>;
}

/// An oracle that returns one fixed standing for any recipient. Used by the
/// example, the tests, and the daemon's dry-run mode.
pub struct StaticOracle(pub RecipientStanding);

#[async_trait]
impl RecipientOracle for StaticOracle {
    async fn standing(&self, _recipient: &str) -> Result<RecipientStanding> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_unestablished_and_clean() {
        let s = RecipientStanding::unknown("Rcpt", "covenant-reputation (mainnet)");
        assert!(!s.established);
        assert!(!s.flagged);
        assert_eq!(s.score, 0);
    }

    #[tokio::test]
    async fn static_oracle_echoes_its_standing() {
        let want = RecipientStanding::unknown("Rcpt", "test");
        let got = StaticOracle(want.clone())
            .standing("anything")
            .await
            .unwrap();
        assert_eq!(got, want);
    }
}
