//! Derive a labeled soft signal from an agent's Bento standing.
//!
//! Pure, no network. The [`BentoStanding`] it consumes is filled by the
//! on-chain reader once the account layout is published; this derivation is
//! the stable contract on top of it.

use crate::tools::TRUST_LABEL_REPUTATION;
use crate::types::{BentoStanding, ReputationSignal};

/// An agent is "Bento-clean" when it carries no strikes and is not locked at
/// the relayer. Any strike is a yellow flag the caller can weigh; a lock is
/// disqualifying.
pub fn clean_signal(standing: BentoStanding) -> ReputationSignal {
    let clean = !standing.locked && standing.strikes == 0;
    ReputationSignal {
        clean,
        strikes: standing.strikes,
        locked: standing.locked,
        trust: TRUST_LABEL_REPUTATION,
        standing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standing(strikes: u8, locked: bool) -> BentoStanding {
        BentoStanding {
            strikes,
            locked,
            actions_total: 10,
            blocked_total: u32::from(strikes),
            reputation_ema_bps: Some(9000),
        }
    }

    #[test]
    fn clean_needs_zero_strikes_and_no_lock() {
        assert!(clean_signal(standing(0, false)).clean);
        assert!(
            !clean_signal(standing(1, false)).clean,
            "a strike is not clean"
        );
        assert!(
            !clean_signal(standing(0, true)).clean,
            "a lock is not clean"
        );
    }

    #[test]
    fn signal_carries_the_standing_and_label() {
        let sig = clean_signal(standing(2, false));
        assert_eq!(sig.strikes, 2);
        assert_eq!(sig.standing.actions_total, 10);
        assert!(sig.trust.contains("soft signal"));
    }
}
