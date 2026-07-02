//! Token pricing. Drives the live cap in the proxy; the receipt labels any
//! figure derived here as an estimate, since it is a list-price reconstruction
//! rather than the provider's own billed number.
//!
//! Rates are per million tokens, current as of 2026-07. Cache reads bill at
//! 0.1x the input rate; 5-minute cache writes at 1.25x. Model families are
//! matched by prefix so a dated or context-tier suffix (e.g. the `[1m]` marker
//! Claude Code prints) resolves to the same rate.

/// Per-million-token input and output rates for one model.
#[derive(Debug, Clone, Copy)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
}

/// A single call's token counts, split by how each tier bills.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// 5-minute cache writes, billed at 1.25x input.
    pub cache_creation: u64,
    /// 1-hour cache writes, billed at 2x input. Kept separate because
    /// under-counting these would let a run exceed its cap.
    pub cache_creation_1h: u64,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation + self.cache_creation_1h
    }
}

/// Look up the rate for a model id. Unknown models fall back to Opus-tier
/// pricing; an overestimate is the safe direction for a spend cap.
pub fn rate_for(model: &str) -> Rate {
    let base = model.trim().split(['[', ' ', '@']).next().unwrap_or(model);
    if base.starts_with("claude-fable-5") || base.starts_with("claude-mythos-5") {
        Rate {
            input: 10.0,
            output: 50.0,
        }
    } else if base.starts_with("claude-opus-4") {
        Rate {
            input: 5.0,
            output: 25.0,
        }
    } else if base.starts_with("claude-sonnet-5") || base.starts_with("claude-sonnet-4") {
        Rate {
            input: 3.0,
            output: 15.0,
        }
    } else if base.starts_with("claude-haiku-4") {
        Rate {
            input: 1.0,
            output: 5.0,
        }
    } else if base.starts_with("gpt-5") || base.starts_with("o4") || base.starts_with("codex") {
        // Codex-side placeholder until the second host is wired end to end.
        Rate {
            input: 2.0,
            output: 10.0,
        }
    } else {
        Rate {
            input: 5.0,
            output: 25.0,
        }
    }
}

/// Dollar cost of one call, list-price reconstruction.
pub fn cost_usd(model: &str, usage: &Usage) -> f64 {
    let r = rate_for(model);
    let per = 1_000_000.0;
    let cache_read_rate = r.input * 0.1;
    let cache_write_5m_rate = r.input * 1.25;
    let cache_write_1h_rate = r.input * 2.0;
    (usage.input as f64 * r.input
        + usage.output as f64 * r.output
        + usage.cache_read as f64 * cache_read_rate
        + usage.cache_creation as f64 * cache_write_5m_rate
        + usage.cache_creation_1h as f64 * cache_write_1h_rate)
        / per
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_and_family_resolve_to_one_rate() {
        assert_eq!(rate_for("claude-opus-4-8").input, 5.0);
        assert_eq!(rate_for("claude-opus-4-8[1m]").input, 5.0);
        assert_eq!(rate_for("claude-opus-4-7").output, 25.0);
        assert_eq!(rate_for("claude-haiku-4-5-20251001").input, 1.0);
        assert_eq!(rate_for("claude-fable-5").output, 50.0);
    }

    #[test]
    fn unknown_model_falls_back_high_not_free() {
        // A zero rate would let a runaway on an unrecognized model bypass the
        // cap entirely; the fallback must cost something.
        assert!(rate_for("some-future-model").input >= 5.0);
    }

    #[test]
    fn cost_counts_every_tier() {
        let u = Usage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 1_000_000,
            cache_creation_1h: 0,
        };
        // opus: 5 + 25 + 0.5 + 6.25 = 36.75
        let c = cost_usd("claude-opus-4-8", &u);
        assert!((c - 36.75).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn one_hour_cache_writes_bill_double() {
        let u = Usage {
            cache_creation_1h: 1_000_000,
            ..Default::default()
        };
        // opus input is $5/1M, 1h write is 2x = $10
        let c = cost_usd("claude-opus-4-8", &u);
        assert!((c - 10.0).abs() < 1e-9, "got {c}");
    }
}
