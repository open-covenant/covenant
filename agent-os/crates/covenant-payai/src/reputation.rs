use std::collections::HashSet;

use covenant_identity::LocalIdentity;

use crate::sign::SignedEnvelope;
use crate::types::{PayaiReputation, Settlement, Tier};
use crate::Result;

/// Compute a wallet's settlement-grounded reputation from a settlement set.
/// Counts only inbound settlements (wallet == `pay_to`). Deterministic: the
/// same settlements always yield the same numbers.
pub fn compute_reputation(
    settlements: &[Settlement],
    wallet: &str,
    source_fee_payer: &str,
) -> PayaiReputation {
    let mut settled_jobs = 0u64;
    let mut volume: u128 = 0;
    let mut counterparties: HashSet<&str> = HashSet::new();
    for s in settlements {
        if s.pay_to == wallet {
            settled_jobs += 1;
            volume = volume.saturating_add(s.amount_micro);
            counterparties.insert(s.payer.as_str());
        }
    }
    let distinct = counterparties.len() as u64;
    PayaiReputation {
        wallet: wallet.to_string(),
        settled_jobs,
        distinct_counterparties: distinct,
        volume_micro_usdc: volume,
        tier: Tier::from_volume_micro(volume),
        score: score(settled_jobs, distinct, volume),
        source_fee_payer: source_fee_payer.to_string(),
    }
}

/// 0..=1000 composite: jobs (≤400, 4/job capped at 100), distinct
/// counterparties (≤200, 4 each capped at 50), volume (≤400, 1 point per $100
/// capped at $40k). Phase 1 folds in proven delivery + disputes.
fn score(jobs: u64, distinct: u64, volume_micro: u128) -> u32 {
    let jobs_score = (jobs.min(100) as u32) * 4;
    let cp_score = (distinct.min(50) as u32) * 4;
    let vol_usdc = (volume_micro / 1_000_000) as u64;
    let vol_score = ((vol_usdc / 100).min(400)) as u32;
    (jobs_score + cp_score + vol_score).min(1000)
}

/// Sign a reputation read with the Covenant identity → a verifiable credential.
pub fn sign_reputation(identity: &LocalIdentity, rep: &PayaiReputation) -> Result<SignedEnvelope> {
    SignedEnvelope::sign(identity, rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PAYAI_SOLANA_FEE_PAYER;

    fn settle(payer: &str, pay_to: &str, micro: u128) -> Settlement {
        Settlement {
            signature: "sig".into(),
            slot: 1,
            block_time: Some(1),
            mint: crate::USDC_MAINNET_MINT.into(),
            payer: payer.into(),
            pay_to: pay_to.into(),
            amount_micro: micro,
        }
    }

    #[test]
    fn aggregates_inbound_only_with_distinct_counterparties() {
        let s = vec![
            settle("buyerA", "seller", 2_000_000),
            settle("buyerB", "seller", 3_000_000),
            settle("buyerA", "seller", 1_000_000), // repeat payer
            settle("buyerC", "other", 9_000_000),  // not seller, ignored
            settle("seller", "buyerA", 5_000_000), // seller as payer, ignored
        ];
        let rep = compute_reputation(&s, "seller", PAYAI_SOLANA_FEE_PAYER);
        assert_eq!(rep.settled_jobs, 3);
        assert_eq!(rep.distinct_counterparties, 2, "buyerA and buyerB");
        assert_eq!(rep.volume_micro_usdc, 6_000_000);
        assert_eq!(rep.tier, Tier::Bronze);
        assert_eq!(rep.source_fee_payer, PAYAI_SOLANA_FEE_PAYER);
    }

    #[test]
    fn unknown_wallet_is_zeroed() {
        let rep = compute_reputation(&[settle("a", "b", 1)], "ghost", PAYAI_SOLANA_FEE_PAYER);
        assert_eq!(rep.settled_jobs, 0);
        assert_eq!(rep.distinct_counterparties, 0);
        assert_eq!(rep.volume_micro_usdc, 0);
        assert_eq!(rep.score, 0);
    }

    #[test]
    fn score_is_bounded_and_monotonic() {
        // Big numbers saturate at 1000, not overflow.
        let big: Vec<Settlement> = (0..200)
            .map(|i| settle(&format!("buyer{i}"), "seller", 1_000_000_000))
            .collect();
        let rep = compute_reputation(&big, "seller", PAYAI_SOLANA_FEE_PAYER);
        assert_eq!(rep.score, 1000);
        assert_eq!(rep.tier, Tier::Platinum);
    }

    #[test]
    fn signed_reputation_verifies_and_round_trips() {
        let id = LocalIdentity::generate("daemon@local");
        let rep = compute_reputation(
            &[settle("buyerA", "seller", 12_000_000_000)],
            "seller",
            PAYAI_SOLANA_FEE_PAYER,
        );
        let env = sign_reputation(&id, &rep).unwrap();
        env.verify().unwrap();
        assert_eq!(env.deserialize::<PayaiReputation>().unwrap(), rep);
        assert_eq!(rep.tier, Tier::Gold, "$12k inbound is Gold");
    }
}
