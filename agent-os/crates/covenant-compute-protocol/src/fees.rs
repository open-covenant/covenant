//! Marketplace fee capture (C7): the coordinator's take on each
//! released hold, expressed in integer basis points of the gross
//! amount — never floats, this is money.
//!
//! The math lives here, in the crate both sides already share, so the
//! coordinator (which withholds the fee from the payout push) and the
//! node (which credits its own earnings ledger net of the disclosed
//! fee) can never fork on the rounding. The fee itself floors: the
//! sub-micro-USDC rounding dust stays with the operator, not the
//! marketplace — a fee schedule that rounds against the party who did
//! the work is how trust dies at scale.

/// A fee at or above 10_000 bps would pay the operator zero or
/// negatively; [`MarketplaceFee::new`] makes that unrepresentable.
pub const MAX_FEE_BPS: u32 = 10_000;

/// A validated marketplace take. `Default` is zero — a coordinator
/// that never configures a fee takes nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarketplaceFee(u32);

impl MarketplaceFee {
    pub const fn zero() -> Self {
        Self(0)
    }

    /// Refuses `MAX_FEE_BPS` and above: a 100% take is confiscation,
    /// not a fee.
    pub fn new(bps: u32) -> Result<Self, String> {
        if bps >= MAX_FEE_BPS {
            return Err(format!(
                "marketplace fee {bps} bps is at or above {MAX_FEE_BPS} (a 100% take)"
            ));
        }
        Ok(Self(bps))
    }

    pub fn bps(self) -> u32 {
        self.0
    }

    /// The fee withheld from a gross release of `amount_micro_usdc`.
    /// Floors, so the operator's net is the ceiling.
    pub fn take_of(self, amount_micro_usdc: u64) -> u64 {
        fee_take_micro_usdc(amount_micro_usdc, self.0)
    }
}

/// Floor of `amount × fee_bps / 10_000` in u128, so a maximal u64
/// amount can't overflow mid-multiply. The node uses this directly
/// with the `fee_bps` disclosed in its `RegisterResponse`.
pub fn fee_take_micro_usdc(amount_micro_usdc: u64, fee_bps: u32) -> u64 {
    let take = u128::from(amount_micro_usdc) * u128::from(fee_bps) / u128::from(MAX_FEE_BPS);
    u64::try_from(take).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_confiscatory_fee_is_unrepresentable() {
        assert!(MarketplaceFee::new(10_000).is_err());
        assert!(MarketplaceFee::new(10_001).is_err());
        assert_eq!(MarketplaceFee::new(9_999).unwrap().bps(), 9_999);
        assert_eq!(MarketplaceFee::default().bps(), 0);
    }

    #[test]
    fn the_fee_floors_in_the_operators_favor() {
        // 2.5% of 1_000 is exactly 25.
        assert_eq!(fee_take_micro_usdc(1_000, 250), 25);
        // 2.5% of 999 is 24.975 — the .975 dust stays with the operator.
        assert_eq!(fee_take_micro_usdc(999, 250), 24);
        // 1 bps of a sub-10_000 amount floors to zero: no fee is
        // manufactured out of rounding.
        assert_eq!(fee_take_micro_usdc(9_999, 1), 0);
        assert_eq!(fee_take_micro_usdc(u64::MAX, 0), 0);
    }

    #[test]
    fn a_maximal_amount_does_not_overflow() {
        let take = fee_take_micro_usdc(u64::MAX, 9_999);
        assert!(take < u64::MAX);
        assert_eq!(
            u128::from(take),
            u128::from(u64::MAX) * 9_999 / 10_000,
            "u128 intermediate math, floored"
        );
    }
}
