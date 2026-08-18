//! The promise, and whether it was kept.
//!
//! An SLA here is three numbers: how fast, how many misses are tolerated, and
//! over what window. Deliberately small. A richer SLA is easy to write and hard
//! to agree on, and every extra term is another thing an operator can dispute
//! after the fact. These three are enough to say "you took on work and did not
//! do it in time, this many times" without anyone needing to see the work.
//!
//! Evaluation is a pure function of the log, the SLA, and the window. Two
//! parties running it on the same record get the same verdict, which is the
//! property that lets a slash be argued about without being negotiated.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::observation::ObservationLog;

/// What the operator promised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sla {
    /// Time from accepting an obligation to completing it.
    pub settle_within_ms: u64,
    /// Misses allowed inside one window before it is a breach. Zero means any
    /// miss breaches, which is usually too sharp for a real rail: a single
    /// upstream blip should not slash a bond.
    pub tolerated_misses: u32,
    /// The window misses are counted over.
    pub window_ms: u64,
}

impl Sla {
    pub fn new(settle_within_ms: u64, tolerated_misses: u32, window_ms: u64) -> Result<Self> {
        if settle_within_ms == 0 {
            return Err(Error::Sla(
                "settleWithinMs must be greater than zero".into(),
            ));
        }
        if window_ms == 0 {
            return Err(Error::Sla("windowMs must be greater than zero".into()));
        }
        if window_ms < settle_within_ms {
            return Err(Error::Sla(format!(
                "windowMs {window_ms} is shorter than settleWithinMs {settle_within_ms}, so no obligation could complete inside it"
            )));
        }
        Ok(Self {
            settle_within_ms,
            tolerated_misses,
            window_ms,
        })
    }
}

/// The verdict over one window. Every input to the decision is reported, so a
/// disagreement is about the record rather than about the arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    pub sla: Sla,
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    /// Obligations accepted inside the window.
    pub observed: u32,
    pub misses: u32,
    pub breached: bool,
    /// The ids that missed, so the operator can go look at them. Bounded, since
    /// a total outage would otherwise produce an unbounded list inside a signed
    /// artifact.
    pub missed_ids: Vec<String>,
}

/// Cap on the ids carried in an assessment. Past this the count is the signal.
const MAX_REPORTED_IDS: usize = 32;

/// Evaluate an operator's log over the window ending at `window_end_ms`.
///
/// A window with no obligations is not a breach. An operator given no work
/// cannot fail to do it, and slashing for silence would punish a quiet period
/// rather than a broken promise.
pub fn assess(log: &ObservationLog, sla: Sla, window_end_ms: u64) -> Assessment {
    let window_start_ms = window_end_ms.saturating_sub(sla.window_ms);

    let mut observed = 0u32;
    let mut missed_ids = Vec::new();
    let mut misses = 0u32;

    for o in log.in_window(window_start_ms, window_end_ms) {
        observed = observed.saturating_add(1);
        if o.outcome.is_miss(o.accepted_at_ms, sla.settle_within_ms) {
            misses = misses.saturating_add(1);
            if missed_ids.len() < MAX_REPORTED_IDS {
                missed_ids.push(o.id.clone());
            }
        }
    }

    Assessment {
        sla,
        window_start_ms,
        window_end_ms,
        observed,
        misses,
        breached: misses > sla.tolerated_misses,
        missed_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{Obligation, Outcome};

    fn sla() -> Sla {
        Sla::new(1_000, 1, 10_000).unwrap()
    }

    fn log(entries: Vec<Obligation>) -> ObservationLog {
        ObservationLog::new(entries)
    }

    fn on_time(id: &str, at: u64) -> Obligation {
        Obligation::new(id, at, Outcome::Settled { at_ms: at + 100 }).unwrap()
    }

    fn late(id: &str, at: u64) -> Obligation {
        Obligation::new(id, at, Outcome::Settled { at_ms: at + 5_000 }).unwrap()
    }

    fn timed_out(id: &str, at: u64) -> Obligation {
        Obligation::new(id, at, Outcome::TimedOut).unwrap()
    }

    #[test]
    fn an_operator_meeting_the_promise_does_not_breach() {
        let a = assess(
            &log(vec![on_time("a", 1_000), on_time("b", 2_000)]),
            sla(),
            10_000,
        );
        assert_eq!(a.observed, 2);
        assert_eq!(a.misses, 0);
        assert!(!a.breached);
        assert!(a.missed_ids.is_empty());
    }

    #[test]
    fn misses_inside_the_tolerance_do_not_breach() {
        let a = assess(
            &log(vec![on_time("a", 1_000), late("b", 2_000)]),
            sla(),
            10_000,
        );
        assert_eq!(a.misses, 1);
        assert!(!a.breached, "one miss is tolerated");
        assert_eq!(a.missed_ids, vec!["b"]);
    }

    #[test]
    fn one_past_the_tolerance_breaches() {
        let a = assess(
            &log(vec![late("a", 1_000), timed_out("b", 2_000)]),
            sla(),
            10_000,
        );
        assert_eq!(a.misses, 2);
        assert!(a.breached);
        assert_eq!(a.missed_ids, vec!["a", "b"]);
    }

    #[test]
    fn silence_is_not_a_breach() {
        let a = assess(&ObservationLog::default(), sla(), 10_000);
        assert_eq!(a.observed, 0);
        assert_eq!(a.misses, 0);
        assert!(!a.breached, "no work taken on cannot be work failed");
    }

    #[test]
    fn misses_outside_the_window_do_not_count() {
        // Two timeouts, both older than the 10s window ending at 100_000.
        let a = assess(
            &log(vec![timed_out("old-a", 1_000), timed_out("old-b", 2_000)]),
            sla(),
            100_000,
        );
        assert_eq!(a.observed, 0);
        assert!(
            !a.breached,
            "an operator is judged on the window, not forever"
        );
    }

    #[test]
    fn a_drop_counts_the_same_as_a_timeout() {
        let dropped = Obligation::new("d", 1_000, Outcome::Dropped).unwrap();
        let a = assess(&log(vec![dropped, timed_out("t", 2_000)]), sla(), 10_000);
        assert_eq!(a.misses, 2);
        assert!(a.breached);
    }

    #[test]
    fn a_zero_tolerance_sla_breaches_on_the_first_miss() {
        let strict = Sla::new(1_000, 0, 10_000).unwrap();
        let a = assess(
            &log(vec![on_time("a", 1_000), late("b", 2_000)]),
            strict,
            10_000,
        );
        assert!(a.breached);
    }

    #[test]
    fn the_reported_id_list_is_bounded_but_the_count_is_not() {
        let entries: Vec<Obligation> = (0..100)
            .map(|i| timed_out(&format!("o{i}"), 1_000 + i))
            .collect();
        let a = assess(&log(entries), sla(), 10_000);
        assert_eq!(a.observed, 100);
        assert_eq!(a.misses, 100, "the count is the signal, not the list");
        assert_eq!(a.missed_ids.len(), MAX_REPORTED_IDS);
    }

    #[test]
    fn the_same_record_always_produces_the_same_verdict() {
        let l = log(vec![late("a", 1_000), timed_out("b", 2_000)]);
        assert_eq!(assess(&l, sla(), 10_000), assess(&l, sla(), 10_000));
    }

    #[test]
    fn an_incoherent_sla_is_refused() {
        assert!(Sla::new(0, 1, 10_000).is_err());
        assert!(Sla::new(1_000, 1, 0).is_err());
        // A window shorter than the promise can never be met.
        assert!(Sla::new(10_000, 1, 1_000).is_err());
    }

    #[test]
    fn a_window_wider_than_the_clock_starts_at_zero() {
        let wide = Sla::new(1_000, 0, u64::MAX).unwrap();
        let a = assess(&log(vec![timed_out("a", 10)]), wide, 1_000);
        assert_eq!(a.window_start_ms, 0, "saturating, not wrapping");
        assert!(a.breached);
    }
}
