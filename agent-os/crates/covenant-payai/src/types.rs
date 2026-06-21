use serde::{Deserialize, Serialize};

/// A single USDC settlement parsed from a PayAI Solana transaction. Attributed
/// to OWNER wallets, not token accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub mint: String,
    /// Wallet that paid (buyer / source-ATA authority).
    pub payer: String,
    /// Wallet that was paid (seller / destination-ATA owner).
    pub pay_to: String,
    /// USDC atomic units (6 decimals).
    pub amount_micro: u128,
}

/// Volume tier shown as the badge. Thresholds are whole USDC of lifetime
/// inbound settled volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Bronze,
    Silver,
    Gold,
    Platinum,
}

impl Tier {
    pub fn from_volume_micro(v: u128) -> Tier {
        const USDC: u128 = 1_000_000;
        if v >= 100_000 * USDC {
            Tier::Platinum
        } else if v >= 10_000 * USDC {
            Tier::Gold
        } else if v >= 1_000 * USDC {
            Tier::Silver
        } else {
            Tier::Bronze
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tier::Bronze => "BRONZE",
            Tier::Silver => "SILVER",
            Tier::Gold => "GOLD",
            Tier::Platinum => "PLATINUM",
        }
    }
}

/// Settlement-grounded reputation for one wallet. Recompute over the same
/// settlements to reproduce these numbers exactly — nothing self-reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayaiReputation {
    pub wallet: String,
    /// Inbound settlements where this wallet was paid.
    pub settled_jobs: u64,
    /// Distinct payer wallets.
    pub distinct_counterparties: u64,
    /// Lifetime inbound USDC, atomic units.
    pub volume_micro_usdc: u128,
    pub tier: Tier,
    /// 0..=1000 composite.
    pub score: u32,
    /// The facilitator fee-payer the settlements were read from.
    pub source_fee_payer: String,
}

impl PayaiReputation {
    /// Volume in whole USDC, for display only.
    pub fn volume_usdc(&self) -> f64 {
        self.volume_micro_usdc as f64 / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_thresholds() {
        let usdc = 1_000_000u128;
        assert_eq!(Tier::from_volume_micro(0), Tier::Bronze);
        assert_eq!(Tier::from_volume_micro(999 * usdc), Tier::Bronze);
        assert_eq!(Tier::from_volume_micro(1_000 * usdc), Tier::Silver);
        assert_eq!(Tier::from_volume_micro(10_000 * usdc), Tier::Gold);
        assert_eq!(Tier::from_volume_micro(100_000 * usdc), Tier::Platinum);
        assert_eq!(Tier::Gold.label(), "GOLD");
    }
}
