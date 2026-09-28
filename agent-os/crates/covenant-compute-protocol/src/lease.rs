//! Buyer-chosen lease terms over the envelope's `Vec<Content>`.
//!
//! A `LeaseSession` job rents a whole machine for a bounded window
//! instead of buying one generation. Its terms — the window's ceiling
//! and the per-second rate — travel as one `Content::Json` block shaped
//! `{"lease": {…}}`, exactly the way sampling knobs travel for an
//! inference job (`crate::generation`), so the envelope wire form and
//! every existing signature are untouched and the terms are signed like
//! the rest of the paid input.
//!
//! The envelope's `price_micro_usdc` is the buyer's spending CEILING:
//! it must equal `rate × max_duration` exactly, the whole ceiling is
//! escrowed at admission, and settlement bills only the seconds the
//! session actually ran — the coordinator meters, the remainder goes
//! back to the buyer. The operator's receipt never sizes a lease
//! payout; a session is worth what the coordinator observed of it.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

/// Longest window one lease may ask for. A day bounds how much of a
/// buyer's balance a single signed envelope can commit and keeps the
/// deadline math inside every existing sweep's assumptions; a longer
/// engagement is consecutive leases.
pub const MAX_LEASE_DURATION_SECS: u64 = 86_400;
/// An OpenSSH public-key line (`ssh-ed25519 AAAA… comment`) is well
/// under this; anything bigger is not a key.
pub const MAX_CLIENT_KEY_BYTES: usize = 1_024;
/// The least room a lease envelope's deadline must leave beyond the
/// window itself, for acceptance latency and result settlement — a
/// deadline the window consumes whole would turn every full-window
/// session into an expired refund.
pub const LEASE_DEADLINE_SLACK_MS: u64 = 60_000;

/// The signed terms of one lease session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTerms {
    /// The window's ceiling in seconds. The session ends at this bound
    /// (or earlier); the envelope's deadline must leave room for it.
    pub max_duration_secs: u64,
    /// What one second of the session costs. Fixed for the whole
    /// lease — repricing is a new lease.
    pub rate_micro_usdc_per_sec: u64,
    /// The public key the buyer will present to the session (an
    /// OpenSSH `authorized_keys` line). Optional: an access path that
    /// carries its own credential exchange leaves it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_public_key: Option<String>,
}

impl LeaseTerms {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.max_duration_secs == 0 {
            return Err(ProtocolError::Invalid(
                "lease max_duration_secs must be at least 1".into(),
            ));
        }
        if self.max_duration_secs > MAX_LEASE_DURATION_SECS {
            return Err(ProtocolError::Invalid(format!(
                "lease max_duration_secs {} exceeds the {MAX_LEASE_DURATION_SECS}s cap",
                self.max_duration_secs
            )));
        }
        if self.rate_micro_usdc_per_sec == 0 {
            return Err(ProtocolError::Invalid(
                "lease rate_micro_usdc_per_sec must be at least 1 — a free lease is not \
                 a lease"
                    .into(),
            ));
        }
        self.max_price_micro_usdc()?;
        if let Some(key) = &self.client_public_key {
            if key.trim().is_empty() {
                return Err(ProtocolError::Invalid(
                    "lease client_public_key is present but empty".into(),
                ));
            }
            if key.len() > MAX_CLIENT_KEY_BYTES {
                return Err(ProtocolError::Invalid(format!(
                    "lease client_public_key of {} bytes exceeds the {MAX_CLIENT_KEY_BYTES}-byte cap",
                    key.len()
                )));
            }
            if key.chars().any(|c| c == '\n' || c == '\r') {
                return Err(ProtocolError::Invalid(
                    "lease client_public_key must be a single line".into(),
                ));
            }
        }
        Ok(())
    }

    /// The whole-window cost — what the envelope's `price_micro_usdc`
    /// must equal and the escrow holds. Errors when the multiplication
    /// leaves `u64`: an unrepresentable ceiling is a malformed ask,
    /// not a saturation.
    pub fn max_price_micro_usdc(&self) -> Result<u64, ProtocolError> {
        self.rate_micro_usdc_per_sec
            .checked_mul(self.max_duration_secs)
            .ok_or_else(|| {
                ProtocolError::Invalid(format!(
                    "lease ceiling overflows: {} micro-USDC/s × {}s does not fit in u64",
                    self.rate_micro_usdc_per_sec, self.max_duration_secs
                ))
            })
    }

    /// What an observed run of `elapsed_ms` costs under these terms,
    /// clamped to the ceiling. Pro-rata to the millisecond, rounded up
    /// — a started fraction of a second is billed as served, and the
    /// rounding dust never exceeds one second's rate. u128 inside so
    /// the intermediate product cannot overflow.
    pub fn metered_micro_usdc(&self, elapsed_ms: u64) -> u64 {
        let rate = u128::from(self.rate_micro_usdc_per_sec);
        let billed = (rate * u128::from(elapsed_ms)).div_ceil(1_000);
        let ceiling = rate * u128::from(self.max_duration_secs);
        u64::try_from(billed.min(ceiling)).unwrap_or(u64::MAX)
    }

    /// The elapsed a settled `charge` reflects: the inverse of
    /// [`Self::metered_micro_usdc`] for a charge that method produced. A
    /// crash-recovery redelivery no longer knows the seconds a lease ran,
    /// only the amount its hold already settled; recovering a millisecond
    /// figure that re-meters back to exactly that amount lets the record's
    /// pinned meter match the charge rather than a fresh clock reading taken
    /// after the recovery gap. `floor(1000 · charge / rate)` is the largest
    /// such elapsed, and it round-trips exactly (`metered_micro_usdc` of it
    /// equals `charge`) because a settled charge is always one that method
    /// produced — including the ceiling, whose inverse lands on the window.
    pub fn elapsed_ms_for(&self, charge_micro_usdc: u64) -> u64 {
        if self.rate_micro_usdc_per_sec == 0 {
            return 0;
        }
        let rate = u128::from(self.rate_micro_usdc_per_sec);
        let elapsed = u128::from(charge_micro_usdc) * 1_000 / rate;
        u64::try_from(elapsed).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseInputBlock {
    lease: LeaseTerms,
}

/// Packs validated lease terms as the envelope-input block.
pub fn lease_input(terms: LeaseTerms) -> Result<Content, ProtocolError> {
    terms.validate()?;
    let value = serde_json::to_value(LeaseInputBlock { lease: terms })
        .expect("lease terms serialize infallibly");
    Ok(Content::json(value))
}

/// Reads lease terms back out of job input.
///
/// Unlike a generation block, a lease block is not advisory:
/// [`crate::SignedJobEnvelope::verify`] requires one on every
/// `LeaseSession` envelope — a lease without terms has no price to
/// check and no window to bound. `Ok(None)` here just means the block
/// is absent; the envelope-level requirement lives with the other
/// kind-specific input checks.
pub fn parse_lease_terms(input: &[Content]) -> Result<Option<LeaseTerms>, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("lease").is_none() {
            continue;
        }
        let block: LeaseInputBlock = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("lease input: {e}")))?;
        block.lease.validate()?;
        return Ok(Some(block.lease));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> LeaseTerms {
        LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 50,
            client_public_key: Some("ssh-ed25519 AAAAC3Nza test@host".into()),
        }
    }

    #[test]
    fn terms_round_trip_through_the_input_block() {
        let packed = lease_input(terms()).unwrap();
        let parsed = parse_lease_terms(&[Content::text("junk"), packed])
            .unwrap()
            .unwrap();
        assert_eq!(parsed, terms());
    }

    #[test]
    fn absent_block_is_none_and_malformed_block_is_an_error() {
        assert!(parse_lease_terms(&[Content::text("no block")])
            .unwrap()
            .is_none());
        let garbage = Content::json(serde_json::json!({"lease": {"max_duration_secs": "ten"}}));
        assert!(parse_lease_terms(&[garbage]).is_err());
    }

    #[test]
    fn validation_refuses_meaningless_terms() {
        for (mutate, needle) in [
            (
                Box::new(|t: &mut LeaseTerms| t.max_duration_secs = 0) as Box<dyn Fn(&mut _)>,
                "at least 1",
            ),
            (
                Box::new(|t: &mut LeaseTerms| t.max_duration_secs = MAX_LEASE_DURATION_SECS + 1),
                "cap",
            ),
            (
                Box::new(|t: &mut LeaseTerms| t.rate_micro_usdc_per_sec = 0),
                "free lease",
            ),
            (
                Box::new(|t: &mut LeaseTerms| {
                    t.rate_micro_usdc_per_sec = u64::MAX;
                    t.max_duration_secs = 2;
                }),
                "overflows",
            ),
            (
                Box::new(|t: &mut LeaseTerms| t.client_public_key = Some("  ".into())),
                "empty",
            ),
            (
                Box::new(|t: &mut LeaseTerms| {
                    t.client_public_key = Some("a\nb".into());
                }),
                "single line",
            ),
            (
                Box::new(|t: &mut LeaseTerms| {
                    t.client_public_key = Some("k".repeat(MAX_CLIENT_KEY_BYTES + 1));
                }),
                "cap",
            ),
        ] {
            let mut bad = terms();
            mutate(&mut bad);
            let err = bad.validate().expect_err("must refuse");
            assert!(
                err.to_string().contains(needle),
                "wanted {needle:?} in {err}"
            );
        }
    }

    #[test]
    fn metering_bills_prorata_rounded_up_and_clamps_at_the_ceiling() {
        let t = terms(); // 50 micro/s over 600s → ceiling 30_000
        assert_eq!(t.metered_micro_usdc(0), 0);
        // 1ms of a 50-micro second rounds up to the first micro.
        assert_eq!(t.metered_micro_usdc(1), 1);
        assert_eq!(t.metered_micro_usdc(1_000), 50);
        assert_eq!(t.metered_micro_usdc(1_500), 75);
        // Past the window the bill stops at the signed ceiling.
        assert_eq!(t.metered_micro_usdc(600_000), 30_000);
        assert_eq!(t.metered_micro_usdc(u64::MAX), 30_000);
    }

    #[test]
    fn the_ceiling_matches_the_price_the_buyer_signs() {
        assert_eq!(terms().max_price_micro_usdc().unwrap(), 30_000);
    }

    #[test]
    fn elapsed_for_a_settled_charge_re_meters_to_that_charge() {
        // The recovery inverse: for any charge metered_micro_usdc produced, the
        // elapsed it maps back to re-meters to exactly that charge — so a
        // crash-recovered lease can pin a stamp consistent with its settled
        // amount, never a fresh over-measurement taken after the recovery gap.
        for rate in [1u64, 3, 50, 999, 1_000, 3_000, 1_000_000] {
            let t = LeaseTerms {
                max_duration_secs: 600,
                rate_micro_usdc_per_sec: rate,
                client_public_key: None,
            };
            for elapsed_ms in [1u64, 2, 500, 999, 1_000, 1_500, 123_456, 600_000, 900_000] {
                let charge = t.metered_micro_usdc(elapsed_ms);
                assert_eq!(
                    t.metered_micro_usdc(t.elapsed_ms_for(charge)),
                    charge,
                    "rate {rate}, elapsed {elapsed_ms} → charge {charge} must round-trip"
                );
            }
        }
        // A zero rate cannot pass validation, but the inverse must not divide
        // by zero on the way to reporting one.
        let free = LeaseTerms {
            max_duration_secs: 1,
            rate_micro_usdc_per_sec: 0,
            client_public_key: None,
        };
        assert_eq!(free.elapsed_ms_for(0), 0);
    }
}
