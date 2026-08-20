//! Trading reputation derived from trade receipts. This is the input PROOF's
//! `SignalKind::Trading` consumes: it turns a receipt history into a portable
//! score an agent can prove without exposing the account. It scores mandate
//! discipline and activity from the receipts themselves; realized profitability
//! is layered in later, once fill and exit data is recorded.

use serde::{Deserialize, Serialize};

use crate::receipt::{Decision, TradeReceipt};

const ESTABLISHED_MIN: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingReputation {
    pub agent: String,
    pub score: u8,
    pub established: bool,
    pub decisions: usize,
    pub executed: usize,
    pub blocked: usize,
    pub pending: usize,
    /// Executed over decisive (executed + blocked). 1.0 means the agent never
    /// tried to breach the mandate; low means it repeatedly attempted orders
    /// the gate had to refuse.
    pub mandate_adherence: f64,
    pub executed_notional_usd: f64,
}

pub fn trading_reputation(agent: &str, receipts: &[TradeReceipt]) -> TradingReputation {
    let mut executed = 0usize;
    let mut blocked = 0usize;
    let mut pending = 0usize;
    let mut executed_notional_usd = 0.0f64;

    for r in receipts {
        match r.decision {
            Decision::Executed => {
                executed += 1;
                if let Some(price) = r.reference_price {
                    executed_notional_usd += r.order.notional(price);
                }
            }
            Decision::Blocked => blocked += 1,
            Decision::PendingApproval => pending += 1,
        }
    }

    let decisive = executed + blocked;
    let mandate_adherence = if decisive == 0 {
        0.0
    } else {
        executed as f64 / decisive as f64
    };

    TradingReputation {
        agent: agent.to_string(),
        score: (mandate_adherence * 100.0).round() as u8,
        established: receipts.len() >= ESTABLISHED_MIN,
        decisions: receipts.len(),
        executed,
        blocked,
        pending,
        mandate_adherence,
        executed_notional_usd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::Mode;
    use crate::types::{OrderRequest, Side};

    fn receipt(decision: Decision) -> TradeReceipt {
        TradeReceipt::new(
            "agent1",
            "ph",
            Mode::Live,
            decision,
            OrderRequest::market("BTC-USD", Side::Buy, 0.001),
        )
        .reference_price(60_000.0)
    }

    #[test]
    fn empty_history_is_zero_and_unestablished() {
        let r = trading_reputation("agent1", &[]);
        assert_eq!(r.score, 0);
        assert!(!r.established);
    }

    #[test]
    fn clean_record_scores_full_and_sums_notional() {
        let receipts: Vec<_> = (0..5).map(|_| receipt(Decision::Executed)).collect();
        let r = trading_reputation("agent1", &receipts);
        assert_eq!(r.score, 100);
        assert!(r.established);
        assert_eq!(r.executed, 5);
        assert!((r.executed_notional_usd - 300.0).abs() < 1e-6);
    }

    #[test]
    fn repeated_breach_attempts_lower_the_score() {
        let receipts = vec![
            receipt(Decision::Executed),
            receipt(Decision::Executed),
            receipt(Decision::Executed),
            receipt(Decision::Blocked),
            receipt(Decision::PendingApproval),
        ];
        let r = trading_reputation("agent1", &receipts);
        assert_eq!(r.executed, 3);
        assert_eq!(r.blocked, 1);
        assert_eq!(r.pending, 1);
        assert_eq!(r.score, 75); // 3 executed / 4 decisive
    }
}
