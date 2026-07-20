//! Trustless on-chain read of Krexa's `KrexitScore` account.
//!
//! The REST score is a soft signal: Krexa's server computes it and we take
//! the response on faith. The same score is committed on-chain in a
//! `KrexitScore` account (an Anchor account on Krexa's score program), and
//! every field the REST endpoint reports is a field of that account.
//! Decoding the account directly drops the trust in Krexa's server — the
//! number is whatever the chain holds, cross-checked against the agent we
//! asked about via [`KrexitScoreAccount::matches_agent`].
//!
//! The account is fixed-size (648 bytes, no `Vec`/`Option`/enum payloads),
//! so this is a plain little-endian cursor decode and needs no borsh crate.
//! Layout and discriminator are Krexa's, provided 2026-07-08. If Krexa
//! ships the full Anchor IDL the generated types supersede this, but the
//! hand layout is meant to match it byte-for-byte.

use crate::{KrexaError, Result};

/// Anchor account discriminator, `sha256("account:KrexitScore")[..8]`.
pub const KREXIT_SCORE_DISCRIMINATOR: [u8; 8] = [173, 162, 27, 120, 191, 151, 202, 220];

/// Serialized length of a `KrexitScore` account: 8-byte discriminator plus
/// the fixed struct.
pub const KREXIT_SCORE_LEN: usize = 648;

/// One slot of the 30-entry score-history ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreHistoryEntry {
    pub timestamp: i64,
    pub old_score: u16,
    pub new_score: u16,
    pub event_type: u8,
    pub delta_bps: i16,
}

/// A decoded `KrexitScore` account. Field order and widths mirror Krexa's
/// on-chain struct exactly; `agent`/`owner` stay raw 32-byte pubkeys so this
/// module needs no base58 codec.
#[derive(Debug, Clone, PartialEq)]
pub struct KrexitScoreAccount {
    pub agent: [u8; 32],
    pub owner: [u8; 32],
    pub score: u16,
    pub credit_level: u8,
    pub kya_tier: u8,
    pub c1_repayment: u16,
    pub c2_profitability: u16,
    pub c3_behavioral: u16,
    pub c4_usage: u16,
    pub c5_maturity: u16,
    pub on_time_repayments: u32,
    pub late_repayments: u16,
    pub missed_repayments: u16,
    pub liquidations: u16,
    pub defaults: u16,
    pub credit_cycles_completed: u32,
    pub cumulative_borrowed: u64,
    pub cumulative_repaid: u64,
    pub current_debt: u64,
    pub pnl_ratio_bps: i32,
    pub max_drawdown_bps: u16,
    pub sharpe_ratio_bps: i16,
    pub green_time_bps: u16,
    pub yellow_time_bps: u16,
    pub orange_time_bps: u16,
    pub red_time_bps: u16,
    pub venue_entropy_bps: u16,
    pub unique_venues: u8,
    pub total_transactions: u32,
    pub avg_daily_volume: u64,
    pub registered_at: i64,
    pub last_score_update: i64,
    pub last_critical_event: i64,
    pub last_repayment: i64,
    pub history: [ScoreHistoryEntry; 30],
    pub history_index: u8,
    /// 0=Trader, 1=Service, 2=Hybrid.
    pub agent_type: u8,
    pub revenue_health_bps: u16,
    pub milestone_completion_rate_bps: u16,
    pub is_active: bool,
    pub is_blacklisted: bool,
    pub bump: u8,
}

impl KrexitScoreAccount {
    /// Decode raw account data (including the 8-byte discriminator) into a
    /// `KrexitScoreAccount`. Rejects a wrong discriminator or a buffer too
    /// short for the fixed layout; trailing bytes past the struct are
    /// ignored so a future account that only grows still decodes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let disc = r.take(8)?;
        if disc != KREXIT_SCORE_DISCRIMINATOR {
            return Err(KrexaError::Decode(format!(
                "not a KrexitScore account: discriminator {disc:02x?}"
            )));
        }
        let agent = r.pubkey()?;
        let owner = r.pubkey()?;
        let score = r.u16()?;
        let credit_level = r.u8()?;
        let kya_tier = r.u8()?;
        let c1_repayment = r.u16()?;
        let c2_profitability = r.u16()?;
        let c3_behavioral = r.u16()?;
        let c4_usage = r.u16()?;
        let c5_maturity = r.u16()?;
        let on_time_repayments = r.u32()?;
        let late_repayments = r.u16()?;
        let missed_repayments = r.u16()?;
        let liquidations = r.u16()?;
        let defaults = r.u16()?;
        let credit_cycles_completed = r.u32()?;
        let cumulative_borrowed = r.u64()?;
        let cumulative_repaid = r.u64()?;
        let current_debt = r.u64()?;
        let pnl_ratio_bps = r.i32()?;
        let max_drawdown_bps = r.u16()?;
        let sharpe_ratio_bps = r.i16()?;
        let green_time_bps = r.u16()?;
        let yellow_time_bps = r.u16()?;
        let orange_time_bps = r.u16()?;
        let red_time_bps = r.u16()?;
        let venue_entropy_bps = r.u16()?;
        let unique_venues = r.u8()?;
        let total_transactions = r.u32()?;
        let avg_daily_volume = r.u64()?;
        let registered_at = r.i64()?;
        let last_score_update = r.i64()?;
        let last_critical_event = r.i64()?;
        let last_repayment = r.i64()?;
        let mut history = [ScoreHistoryEntry {
            timestamp: 0,
            old_score: 0,
            new_score: 0,
            event_type: 0,
            delta_bps: 0,
        }; 30];
        for slot in &mut history {
            *slot = ScoreHistoryEntry {
                timestamp: r.i64()?,
                old_score: r.u16()?,
                new_score: r.u16()?,
                event_type: r.u8()?,
                delta_bps: r.i16()?,
            };
        }
        let history_index = r.u8()?;
        let agent_type = r.u8()?;
        let revenue_health_bps = r.u16()?;
        let milestone_completion_rate_bps = r.u16()?;
        let is_active = r.bool()?;
        let is_blacklisted = r.bool()?;
        let bump = r.u8()?;

        Ok(Self {
            agent,
            owner,
            score,
            credit_level,
            kya_tier,
            c1_repayment,
            c2_profitability,
            c3_behavioral,
            c4_usage,
            c5_maturity,
            on_time_repayments,
            late_repayments,
            missed_repayments,
            liquidations,
            defaults,
            credit_cycles_completed,
            cumulative_borrowed,
            cumulative_repaid,
            current_debt,
            pnl_ratio_bps,
            max_drawdown_bps,
            sharpe_ratio_bps,
            green_time_bps,
            yellow_time_bps,
            orange_time_bps,
            red_time_bps,
            venue_entropy_bps,
            unique_venues,
            total_transactions,
            avg_daily_volume,
            registered_at,
            last_score_update,
            last_critical_event,
            last_repayment,
            history,
            history_index,
            agent_type,
            revenue_health_bps,
            milestone_completion_rate_bps,
            is_active,
            is_blacklisted,
            bump,
        })
    }

    /// The account's own `agent` field must equal the pubkey we queried.
    /// Krexa hands back the `scorePda` address in its REST response; a
    /// buggy or dishonest server could point us at a different agent's
    /// (flattering) account. Comparing the decoded `agent` closes that gap
    /// without re-deriving the PDA. Full self-derivation (not trusting the
    /// REST `scorePda` at all) is now possible: score program `2Gwt…`
    /// (devnet only today), PDA seed `["krexit_score", agent_pubkey]`. It
    /// needs an sdk-free `find_program_address`, so the agent-field check
    /// stands until that's worth adding.
    pub fn matches_agent(&self, agent_pubkey: &[u8; 32]) -> bool {
        &self.agent == agent_pubkey
    }
}

/// A forward-only little-endian reader over account bytes. Every read is
/// bounds-checked and surfaces a [`KrexaError::Decode`] on overrun, so a
/// short or malformed account can never panic the daemon.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos + n;
        let slice = self.data.get(self.pos..end).ok_or_else(|| {
            KrexaError::Decode(format!(
                "KrexitScore truncated: need {n} bytes at offset {}, have {}",
                self.pos,
                self.data.len()
            ))
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn pubkey(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().expect("32-byte slice"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize an account back to bytes the way Krexa's on-chain struct
    /// does, so `decode` can be exercised against a known-good buffer over
    /// every field rather than a hand-picked few.
    fn encode(a: &KrexitScoreAccount) -> Vec<u8> {
        let mut b = Vec::with_capacity(KREXIT_SCORE_LEN);
        b.extend_from_slice(&KREXIT_SCORE_DISCRIMINATOR);
        b.extend_from_slice(&a.agent);
        b.extend_from_slice(&a.owner);
        b.extend_from_slice(&a.score.to_le_bytes());
        b.push(a.credit_level);
        b.push(a.kya_tier);
        for v in [
            a.c1_repayment,
            a.c2_profitability,
            a.c3_behavioral,
            a.c4_usage,
            a.c5_maturity,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&a.on_time_repayments.to_le_bytes());
        for v in [
            a.late_repayments,
            a.missed_repayments,
            a.liquidations,
            a.defaults,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&a.credit_cycles_completed.to_le_bytes());
        b.extend_from_slice(&a.cumulative_borrowed.to_le_bytes());
        b.extend_from_slice(&a.cumulative_repaid.to_le_bytes());
        b.extend_from_slice(&a.current_debt.to_le_bytes());
        b.extend_from_slice(&a.pnl_ratio_bps.to_le_bytes());
        b.extend_from_slice(&a.max_drawdown_bps.to_le_bytes());
        b.extend_from_slice(&a.sharpe_ratio_bps.to_le_bytes());
        for v in [
            a.green_time_bps,
            a.yellow_time_bps,
            a.orange_time_bps,
            a.red_time_bps,
            a.venue_entropy_bps,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(a.unique_venues);
        b.extend_from_slice(&a.total_transactions.to_le_bytes());
        b.extend_from_slice(&a.avg_daily_volume.to_le_bytes());
        for v in [
            a.registered_at,
            a.last_score_update,
            a.last_critical_event,
            a.last_repayment,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for e in &a.history {
            b.extend_from_slice(&e.timestamp.to_le_bytes());
            b.extend_from_slice(&e.old_score.to_le_bytes());
            b.extend_from_slice(&e.new_score.to_le_bytes());
            b.push(e.event_type);
            b.extend_from_slice(&e.delta_bps.to_le_bytes());
        }
        b.push(a.history_index);
        b.push(a.agent_type);
        b.extend_from_slice(&a.revenue_health_bps.to_le_bytes());
        b.extend_from_slice(&a.milestone_completion_rate_bps.to_le_bytes());
        b.push(a.is_active as u8);
        b.push(a.is_blacklisted as u8);
        b.push(a.bump);
        b
    }

    /// An account with a distinct value in every field, so a transposed or
    /// mis-width decode fails instead of reading a coincidentally-equal 0.
    fn sample() -> KrexitScoreAccount {
        let mut history = [ScoreHistoryEntry {
            timestamp: 0,
            old_score: 0,
            new_score: 0,
            event_type: 0,
            delta_bps: 0,
        }; 30];
        for (i, slot) in history.iter_mut().enumerate() {
            let i = i as i64;
            *slot = ScoreHistoryEntry {
                timestamp: 1_700_000_000 + i,
                old_score: 300 + i as u16,
                new_score: 305 + i as u16,
                event_type: (i % 5) as u8,
                delta_bps: (5 - i) as i16,
            };
        }
        KrexitScoreAccount {
            agent: [7u8; 32],
            owner: [9u8; 32],
            score: 742,
            credit_level: 3,
            kya_tier: 2,
            c1_repayment: 8800,
            c2_profitability: 7200,
            c3_behavioral: 6100,
            c4_usage: 5500,
            c5_maturity: 4000,
            on_time_repayments: 128,
            late_repayments: 3,
            missed_repayments: 1,
            liquidations: 0,
            defaults: 0,
            credit_cycles_completed: 42,
            cumulative_borrowed: 5_000_000_000,
            cumulative_repaid: 4_800_000_000,
            current_debt: 200_000_000,
            pnl_ratio_bps: -1250,
            max_drawdown_bps: 3300,
            sharpe_ratio_bps: 180,
            green_time_bps: 6000,
            yellow_time_bps: 2500,
            orange_time_bps: 1000,
            red_time_bps: 500,
            venue_entropy_bps: 7700,
            unique_venues: 12,
            total_transactions: 9001,
            avg_daily_volume: 250_000_000,
            registered_at: 1_699_000_000,
            last_score_update: 1_700_500_000,
            last_critical_event: 1_699_900_000,
            last_repayment: 1_700_400_000,
            history,
            history_index: 7,
            agent_type: 1,
            revenue_health_bps: 8200,
            milestone_completion_rate_bps: 9100,
            is_active: true,
            is_blacklisted: false,
            bump: 254,
        }
    }

    #[test]
    fn layout_is_648_bytes() {
        assert_eq!(encode(&sample()).len(), KREXIT_SCORE_LEN);
    }

    #[test]
    fn decode_roundtrips_every_field() {
        let a = sample();
        assert_eq!(KrexitScoreAccount::decode(&encode(&a)).unwrap(), a);
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let a = sample();
        let mut bytes = encode(&a);
        bytes.extend_from_slice(&[0xAB; 16]);
        assert_eq!(KrexitScoreAccount::decode(&bytes).unwrap(), a);
    }

    #[test]
    fn wrong_discriminator_is_rejected() {
        let mut bytes = encode(&sample());
        bytes[0] ^= 0xFF;
        let err = KrexitScoreAccount::decode(&bytes).unwrap_err();
        assert!(matches!(err, KrexaError::Decode(_)));
    }

    #[test]
    fn short_buffer_is_rejected_not_panicked() {
        let bytes = encode(&sample());
        let err = KrexitScoreAccount::decode(&bytes[..600]).unwrap_err();
        assert!(matches!(err, KrexaError::Decode(_)));
        // Even shorter than the discriminator.
        assert!(KrexitScoreAccount::decode(&bytes[..4]).is_err());
    }

    #[test]
    fn matches_agent_guards_against_swapped_account() {
        let a = sample();
        assert!(a.matches_agent(&[7u8; 32]));
        assert!(!a.matches_agent(&[8u8; 32]));
    }
}
