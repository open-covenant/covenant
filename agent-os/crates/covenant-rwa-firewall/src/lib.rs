//! RWA-aware pre-trade firewall for tokenized equities on an EVM venue.
//!
//! [`covenant_evm_firewall`] bounds *what* an agent may call: the contract, the
//! selector, the native value. A tokenized-equity trade needs the other half,
//! bounds on the *risk* the call carries. A Stock Token is an ERC-20 whose price
//! lives in a per-asset oracle and whose share ratio moves with corporate actions
//! through an on-chain multiplier, and whose underlying trades on an exchange
//! calendar while the token trades around the clock. A spend cap says nothing
//! about any of that.
//!
//! This is the pure decision half. Given a proposed trade and the live asset
//! context (the oracle price and its age, the multiplier, whether the market is
//! open), [`RwaPolicy::evaluate`] returns an allow verdict or a precise refusal.
//! Reading the chain, the Chainlink feed and `uiMultiplier()`, stays with the
//! caller; the example wires it to Robinhood Chain.
//!
//! Prices are USD scaled by 1e8 (the Chainlink convention). The multiplier is
//! scaled by 1e18 (1e18 = 1.0), per ERC-8056. Token amounts are raw base units
//! (18 decimals). Notional and the multiplier-adjusted share amount are computed
//! from the raw amount and the multiplier-folded oracle price, so the firewall is
//! multiplier-correct by construction rather than trusting a caller's per-share
//! math.

use thiserror::Error;

const WAD: u128 = 1_000_000_000_000_000_000; // 1e18
const BPS: u128 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// A proposed Stock Token trade, before it is signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RwaTrade {
    /// The Stock Token contract.
    pub asset: [u8; 20],
    pub side: Side,
    /// Token base units (18 decimals).
    pub raw_amount: u128,
    /// The execution price per whole token the agent intends to fill at, USD*1e8.
    /// This is the venue quote the firewall checks against the oracle, not the
    /// oracle itself.
    pub quoted_price_usd_e8: u128,
}

/// The live on-chain context for the asset, read by the caller just before the
/// decision. Every field comes off the chain: the oracle answer and its
/// `updatedAt`, `uiMultiplier()`, and a market-hours signal for the underlying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetContext {
    /// Chainlink answer for the asset, per whole token, USD*1e8. The feed folds
    /// the corporate-action multiplier, so raw_amount * this / 1e18 is the
    /// notional.
    pub oracle_price_usd_e8: u128,
    /// The feed's `updatedAt` (unix seconds).
    pub oracle_updated_at: u64,
    /// `uiMultiplier()` (ERC-8056), scaled 1e18. Zero means the token is not
    /// initialized and nothing should trade against it.
    pub ui_multiplier_e18: u128,
    /// Now (unix seconds), for the staleness check.
    pub now: u64,
    /// Whether the underlying's exchange is open.
    pub market_open: bool,
}

/// The bounds an agent's tokenized-equity trading runs inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RwaPolicy {
    /// Per-trade notional ceiling, USD*1e8.
    pub per_trade_notional_cap_usd_e8: u128,
    /// How far the venue quote may sit from the oracle, in basis points.
    pub fair_value_band_bps: u32,
    /// The oldest an oracle answer may be before it is refused, seconds.
    pub max_feed_staleness_secs: u64,
    /// Refuse a trade while the underlying's market is closed. The 24/7 token can
    /// gap against a closed exchange, so a blind off-hours fill is held.
    pub require_market_hours: bool,
}

impl RwaPolicy {
    /// Judge a trade against the live context. Fail-closed: an unpriceable asset,
    /// a stale feed, a closed market, a quote off the oracle, or a notional over
    /// the cap is refused, and the reason names the offending value.
    pub fn evaluate(&self, trade: &RwaTrade, ctx: &AssetContext) -> Result<Verdict, RwaDenial> {
        if ctx.oracle_price_usd_e8 == 0 {
            return Err(RwaDenial::PriceUnavailable);
        }
        if ctx.ui_multiplier_e18 == 0 {
            return Err(RwaDenial::MultiplierUnset);
        }

        let age = ctx.now.saturating_sub(ctx.oracle_updated_at);
        if age > self.max_feed_staleness_secs {
            return Err(RwaDenial::StalePriceFeed {
                age_secs: age,
                max_secs: self.max_feed_staleness_secs,
            });
        }

        if self.require_market_hours && !ctx.market_open {
            return Err(RwaDenial::MarketClosed);
        }

        let oracle = ctx.oracle_price_usd_e8;
        let diff = trade.quoted_price_usd_e8.abs_diff(oracle);
        let diff_bps = mul_div(diff, BPS, oracle).unwrap_or(u128::MAX);
        if diff_bps > u128::from(self.fair_value_band_bps) {
            return Err(RwaDenial::PriceOutsideBand {
                quoted_usd_e8: trade.quoted_price_usd_e8,
                oracle_usd_e8: oracle,
                diff_bps,
                band_bps: self.fair_value_band_bps,
            });
        }

        let notional_usd_e8 =
            mul_div(trade.raw_amount, oracle, WAD).ok_or(RwaDenial::AmountTooLarge)?;
        if notional_usd_e8 > self.per_trade_notional_cap_usd_e8 {
            return Err(RwaDenial::NotionalOverCap {
                notional_usd_e8,
                cap_usd_e8: self.per_trade_notional_cap_usd_e8,
            });
        }

        let shares_e18 = mul_div(trade.raw_amount, ctx.ui_multiplier_e18, WAD)
            .ok_or(RwaDenial::AmountTooLarge)?;
        Ok(Verdict {
            notional_usd_e8,
            shares_e18,
            oracle_price_usd_e8: oracle,
            ui_multiplier_e18: ctx.ui_multiplier_e18,
            diff_bps,
        })
    }
}

/// What an allowed trade resolved to. The caller keeps this as the receipt of the
/// decision: the notional it was sized at, the share amount after the multiplier,
/// and how far the fill sat from the oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub notional_usd_e8: u128,
    /// Multiplier-adjusted share amount, 1e18-scaled.
    pub shares_e18: u128,
    pub oracle_price_usd_e8: u128,
    pub ui_multiplier_e18: u128,
    pub diff_bps: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RwaDenial {
    #[error("no usable oracle price for the asset")]
    PriceUnavailable,
    #[error("uiMultiplier is zero: the token is not initialized")]
    MultiplierUnset,
    #[error("oracle price is {age_secs}s old, past the {max_secs}s limit")]
    StalePriceFeed { age_secs: u64, max_secs: u64 },
    #[error("the underlying's market is closed")]
    MarketClosed,
    #[error("quote {quoted_usd_e8} is {diff_bps}bps from oracle {oracle_usd_e8}, past the {band_bps}bps band")]
    PriceOutsideBand {
        quoted_usd_e8: u128,
        oracle_usd_e8: u128,
        diff_bps: u128,
        band_bps: u32,
    },
    #[error("notional {notional_usd_e8} exceeds the per-trade cap {cap_usd_e8} (USD*1e8)")]
    NotionalOverCap {
        notional_usd_e8: u128,
        cap_usd_e8: u128,
    },
    #[error("trade amount is too large to price without overflow")]
    AmountTooLarge,
}

/// `a * b / d`, `None` on a zero divisor or on a product that overflows u128. An
/// overflow only happens for an amount far past any real trade, and mapping it to
/// `None` keeps the firewall fail-closed: an unpriceable-because-absurd size is
/// refused, never waved through on a wrap.
fn mul_div(a: u128, b: u128, d: u128) -> Option<u128> {
    if d == 0 {
        return None;
    }
    a.checked_mul(b).map(|p| p / d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AAPL: [u8; 20] = [0xaa; 20];

    fn policy() -> RwaPolicy {
        RwaPolicy {
            per_trade_notional_cap_usd_e8: 10_000 * 100_000_000, // $10,000
            fair_value_band_bps: 50,                             // 0.5%
            max_feed_staleness_secs: 3_600,
            require_market_hours: true,
        }
    }

    fn ctx() -> AssetContext {
        AssetContext {
            oracle_price_usd_e8: 200 * 100_000_000, // $200.00
            oracle_updated_at: 1_000_000,
            ui_multiplier_e18: WAD, // 1.0
            now: 1_000_060,         // 60s later
            market_open: true,
        }
    }

    fn trade(raw_amount: u128, quoted_usd_e8: u128) -> RwaTrade {
        RwaTrade {
            asset: AAPL,
            side: Side::Buy,
            raw_amount,
            quoted_price_usd_e8: quoted_usd_e8,
        }
    }

    #[test]
    fn allows_a_fair_in_hours_trade_within_cap() {
        // 10 tokens at $200 = $2,000 notional, quote on the oracle.
        let v = policy()
            .evaluate(&trade(10 * WAD, 200 * 100_000_000), &ctx())
            .expect("allowed");
        assert_eq!(v.notional_usd_e8, 2_000 * 100_000_000);
        assert_eq!(v.shares_e18, 10 * WAD);
        assert_eq!(v.diff_bps, 0);
    }

    #[test]
    fn refuses_a_quote_outside_the_band() {
        // Quote 1% above oracle, band is 0.5%.
        let bad = 202 * 100_000_000;
        let d = policy().evaluate(&trade(WAD, bad), &ctx()).unwrap_err();
        match d {
            RwaDenial::PriceOutsideBand { diff_bps, .. } => assert_eq!(diff_bps, 100),
            other => panic!("expected band denial, got {other:?}"),
        }
    }

    #[test]
    fn refuses_notional_over_the_cap() {
        // 60 tokens at $200 = $12,000 > $10,000 cap.
        let d = policy()
            .evaluate(&trade(60 * WAD, 200 * 100_000_000), &ctx())
            .unwrap_err();
        assert!(matches!(d, RwaDenial::NotionalOverCap { .. }));
    }

    #[test]
    fn refuses_a_stale_feed() {
        let mut c = ctx();
        c.now = c.oracle_updated_at + 3_601;
        assert!(matches!(
            policy().evaluate(&trade(WAD, 200 * 100_000_000), &c),
            Err(RwaDenial::StalePriceFeed { .. })
        ));
    }

    #[test]
    fn refuses_off_hours_when_required() {
        let mut c = ctx();
        c.market_open = false;
        assert_eq!(
            policy().evaluate(&trade(WAD, 200 * 100_000_000), &c),
            Err(RwaDenial::MarketClosed)
        );
    }

    #[test]
    fn allows_off_hours_when_not_required() {
        let mut c = ctx();
        c.market_open = false;
        let p = RwaPolicy {
            require_market_hours: false,
            ..policy()
        };
        assert!(p.evaluate(&trade(WAD, 200 * 100_000_000), &c).is_ok());
    }

    #[test]
    fn refuses_an_uninitialized_or_unpriced_asset() {
        let mut c = ctx();
        c.ui_multiplier_e18 = 0;
        assert_eq!(
            policy().evaluate(&trade(WAD, 200 * 100_000_000), &c),
            Err(RwaDenial::MultiplierUnset)
        );
        let mut c = ctx();
        c.oracle_price_usd_e8 = 0;
        assert_eq!(
            policy().evaluate(&trade(WAD, 0), &c),
            Err(RwaDenial::PriceUnavailable)
        );
    }

    #[test]
    fn multiplier_scales_the_reported_share_amount() {
        // After a 2:1 split the multiplier is 2.0: one raw token represents two
        // shares. The notional (raw * multiplier-folded oracle) is unchanged; the
        // reported share amount doubles.
        let mut c = ctx();
        c.ui_multiplier_e18 = 2 * WAD;
        let v = policy()
            .evaluate(&trade(10 * WAD, 200 * 100_000_000), &c)
            .expect("allowed");
        assert_eq!(v.shares_e18, 20 * WAD);
        assert_eq!(v.notional_usd_e8, 2_000 * 100_000_000);
    }
}
