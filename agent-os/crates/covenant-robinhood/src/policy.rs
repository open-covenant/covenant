//! The trading policy and its pre-execution decision. `TradingPolicy`
//! serializes to the JSON that lives in a signed Covenant capability's `scope`;
//! [`TradingPolicy::authorize`] is the logic the daemon's
//! `robinhood_order_scope_allows` predicate calls before an order is sent.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::receipt::Mode;
use crate::types::{OrderRequest, OrderType, Side};

const DAY_SECS: u64 = 86_400;
const RATE_WINDOW_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingPolicy {
    #[serde(default = "version_one")]
    pub version: u8,
    #[serde(default = "default_venue")]
    pub venue: String,
    pub mode: Mode,
    #[serde(default)]
    pub caps: Caps,
    #[serde(default)]
    pub risk: Risk,
    #[serde(default)]
    pub universe: Universe,
    #[serde(default)]
    pub order_types: Vec<OrderType>,
    #[serde(default)]
    pub rate: Rate,
    #[serde(default)]
    pub approvals: Approvals,
}

fn version_one() -> u8 {
    1
}
fn default_venue() -> String {
    "robinhood-crypto".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Caps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_order_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_notional_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Risk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_loss_stop_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Universe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<Vec<Side>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_orders_per_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Approvals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_human_over_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    Block(String),
    NeedsApproval(String),
}

/// Rolling per-day and per-window counters. The daemon keeps one per agent, the
/// way `covenant-guard`'s ledger keeps a spend counter that trips the gate.
#[derive(Debug, Default)]
pub struct PolicyState {
    day_start: u64,
    day_notional_usd: f64,
    day_realized_loss_usd: f64,
    order_times: Vec<u64>,
    last_order_at: Option<u64>,
}

impl PolicyState {
    fn roll_day(&mut self, now: u64) {
        if self.day_start == 0 {
            self.day_start = now;
        } else if now.saturating_sub(self.day_start) >= DAY_SECS {
            self.day_start = now;
            self.day_notional_usd = 0.0;
            self.day_realized_loss_usd = 0.0;
        }
    }

    fn orders_last_min(&self, now: u64) -> u32 {
        self.order_times
            .iter()
            .filter(|t| now.saturating_sub(**t) < RATE_WINDOW_SECS)
            .count() as u32
    }

    /// Record an accepted, executed order against the day and rate counters.
    pub fn record_order(&mut self, notional_usd: f64, now: u64) {
        self.roll_day(now);
        self.day_notional_usd += notional_usd;
        self.order_times.push(now);
        self.order_times
            .retain(|t| now.saturating_sub(*t) < RATE_WINDOW_SECS);
        self.last_order_at = Some(now);
    }

    /// Feed realized P&L (negative is a loss) so the daily-loss stop can trip.
    pub fn record_realized_pnl(&mut self, pnl_usd: f64, now: u64) {
        self.roll_day(now);
        if pnl_usd < 0.0 {
            self.day_realized_loss_usd += -pnl_usd;
        }
    }

    pub fn day_notional_usd(&self) -> f64 {
        self.day_notional_usd
    }
    pub fn day_realized_loss_usd(&self) -> f64 {
        self.day_realized_loss_usd
    }
}

impl TradingPolicy {
    /// SHA-256 over the JCS-canonical policy. Binds a receipt to the exact
    /// policy in force and matches the capability scope hash.
    pub fn scope_hash(&self) -> String {
        let canon = serde_jcs::to_string(self).unwrap_or_default();
        hex::encode(Sha256::digest(canon.as_bytes()))
    }

    pub fn authorize(
        &self,
        order: &OrderRequest,
        reference_price: f64,
        now: u64,
        state: &PolicyState,
    ) -> Verdict {
        let symbol = &order.symbol;

        if let Some(deny) = &self.universe.deny {
            if deny.iter().any(|s| s == symbol) {
                return Verdict::Block(format!("symbol {symbol} is on the deny list"));
            }
        }
        if let Some(allow) = &self.universe.allow {
            if !allow.iter().any(|s| s == symbol) {
                return Verdict::Block(format!("symbol {symbol} is not on the allow list"));
            }
        }
        if let Some(sides) = &self.universe.sides {
            if !sides.contains(&order.side) {
                return Verdict::Block(format!("side {:?} is not permitted", order.side));
            }
        }
        if !self.order_types.is_empty() && !self.order_types.contains(&order.order_type) {
            return Verdict::Block(format!(
                "order type {:?} is not permitted",
                order.order_type
            ));
        }
        if let Some(stop) = self.risk.daily_loss_stop_usd {
            if state.day_realized_loss_usd >= stop {
                return Verdict::Block(format!(
                    "daily loss stop hit ({:.2} >= {:.2})",
                    state.day_realized_loss_usd, stop
                ));
            }
        }

        let notional = order.notional(reference_price);
        if let Some(cap) = self.caps.per_order_usd {
            if notional > cap {
                return Verdict::Block(format!(
                    "per-order cap exceeded ({notional:.2} > {cap:.2})"
                ));
            }
        }
        if let Some(cap) = self.caps.daily_notional_usd {
            let projected = state.day_notional_usd + notional;
            if projected > cap {
                return Verdict::Block(format!(
                    "daily notional cap exceeded ({projected:.2} > {cap:.2})"
                ));
            }
        }
        if let Some(max) = self.rate.max_orders_per_min {
            if state.orders_last_min(now) >= max {
                return Verdict::Block(format!("rate limit hit ({max} orders/min)"));
            }
        }
        if let (Some(cool), Some(last)) = (self.rate.cooldown_secs, state.last_order_at) {
            if now.saturating_sub(last) < cool {
                return Verdict::Block(format!("cooldown active ({cool}s)"));
            }
        }
        if let Some(threshold) = self.approvals.require_human_over_usd {
            if notional >= threshold {
                return Verdict::NeedsApproval(format!(
                    "notional {notional:.2} at or above approval threshold {threshold:.2}"
                ));
            }
        }
        Verdict::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrderRequest, OrderType, Side};

    fn policy() -> TradingPolicy {
        TradingPolicy {
            version: 1,
            venue: "robinhood-crypto".into(),
            mode: Mode::Live,
            caps: Caps {
                per_order_usd: Some(500.0),
                daily_notional_usd: Some(1000.0),
            },
            risk: Risk {
                daily_loss_stop_usd: Some(200.0),
            },
            universe: Universe {
                allow: Some(vec!["BTC-USD".into(), "ETH-USD".into()]),
                deny: None,
                sides: Some(vec![Side::Buy]),
            },
            order_types: vec![OrderType::Market],
            rate: Rate {
                max_orders_per_min: Some(3),
                cooldown_secs: Some(5),
            },
            approvals: Approvals {
                require_human_over_usd: Some(400.0),
            },
        }
    }

    #[test]
    fn allows_within_policy() {
        let st = PolicyState::default();
        assert_eq!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Buy, 0.001),
                60_000.0,
                100,
                &st
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn blocks_off_universe() {
        let st = PolicyState::default();
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("DOGE-USD", Side::Buy, 1.0),
                0.1,
                100,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn blocks_wrong_side() {
        let st = PolicyState::default();
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Sell, 0.001),
                60_000.0,
                100,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn blocks_disallowed_order_type() {
        let st = PolicyState::default();
        let o = OrderRequest::limit("BTC-USD", Side::Buy, 0.001, 59_000.0);
        assert!(matches!(
            policy().authorize(&o, 60_000.0, 100, &st),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn blocks_over_per_order_cap() {
        let st = PolicyState::default();
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Buy, 0.01),
                60_000.0,
                100,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn blocks_over_daily_notional() {
        let mut st = PolicyState::default();
        st.record_order(900.0, 100);
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("ETH-USD", Side::Buy, 0.05),
                3_000.0,
                101,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn blocks_on_daily_loss_stop() {
        let mut st = PolicyState::default();
        st.record_realized_pnl(-250.0, 100);
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Buy, 0.001),
                60_000.0,
                101,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn needs_approval_over_threshold() {
        let st = PolicyState::default();
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Buy, 0.0075),
                60_000.0,
                100,
                &st
            ),
            Verdict::NeedsApproval(_)
        ));
    }

    #[test]
    fn blocks_on_rate_limit() {
        let mut st = PolicyState::default();
        st.record_order(10.0, 100);
        st.record_order(10.0, 101);
        st.record_order(10.0, 102);
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Buy, 0.0001),
                60_000.0,
                150,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn blocks_on_cooldown() {
        let mut st = PolicyState::default();
        st.record_order(10.0, 100);
        assert!(matches!(
            policy().authorize(
                &OrderRequest::market("BTC-USD", Side::Buy, 0.0001),
                60_000.0,
                102,
                &st
            ),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn scope_hash_is_stable_and_sensitive() {
        let a = policy().scope_hash();
        assert_eq!(a, policy().scope_hash());
        let mut p = policy();
        p.caps.per_order_usd = Some(999.0);
        assert_ne!(a, p.scope_hash());
    }
}
