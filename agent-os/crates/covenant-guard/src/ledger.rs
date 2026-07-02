//! The spend ledger: the cap, enforced from outside the agent.
//!
//! Every metered call is committed here. When committed spend crosses the
//! budget the ledger latches `killed` and wakes the run orchestrator, which
//! signals the agent's process group. The proxy also refuses new metered calls
//! once the latch is set, so overshoot is bounded to the calls already
//! streaming when the cap trips, not the whole run.

use crate::pricing::{cost_usd, Usage};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::Notify;

/// Running totals for one model over the run.
#[derive(Debug, Clone, Default)]
pub struct ModelTotals {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost_usd: f64,
}

#[derive(Default)]
struct Totals {
    spent_usd: f64,
    calls: u64,
    per_model: BTreeMap<String, ModelTotals>,
}

/// Shared spend state. Cloneable handles all point at the same totals.
pub struct Ledger {
    budget_usd: f64,
    totals: Mutex<Totals>,
    killed: AtomicBool,
    kill_reason: Mutex<Option<String>>,
    wake: Notify,
}

impl Ledger {
    pub fn new(budget_usd: f64) -> Self {
        Self {
            budget_usd,
            totals: Mutex::new(Totals::default()),
            killed: AtomicBool::new(false),
            kill_reason: Mutex::new(None),
            wake: Notify::new(),
        }
    }

    pub fn budget(&self) -> f64 {
        self.budget_usd
    }

    /// Commit one call's usage. Returns the new cumulative spend. If this call
    /// takes the run over budget, the ledger latches killed and wakes any
    /// waiter; the caller (the proxy) should stop forwarding the stream.
    pub fn commit(&self, model: &str, usage: &Usage) -> f64 {
        let cost = cost_usd(model, usage);
        let cumulative = {
            let mut t = self.totals.lock().unwrap();
            t.spent_usd += cost;
            t.calls += 1;
            let m = t.per_model.entry(model.to_string()).or_default();
            m.calls += 1;
            m.input += usage.input;
            m.output += usage.output;
            m.cache_read += usage.cache_read;
            m.cache_creation += usage.cache_creation + usage.cache_creation_1h;
            m.cost_usd += cost;
            t.spent_usd
        };
        if cumulative >= self.budget_usd {
            self.trip(format!(
                "spend reached ${cumulative:.2} of ${:.2} cap",
                self.budget_usd
            ));
        }
        cumulative
    }

    /// Latch the kill flag with a reason and wake the orchestrator. Idempotent:
    /// the first reason wins, so a wall-clock trip and a budget trip in the same
    /// instant don't clobber each other.
    pub fn trip(&self, reason: impl Into<String>) {
        if self.killed.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.kill_reason.lock().unwrap() = Some(reason.into());
        self.wake.notify_waiters();
    }

    pub fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    pub fn kill_reason(&self) -> Option<String> {
        self.kill_reason.lock().unwrap().clone()
    }

    /// Resolves once the ledger trips. Used by the run orchestrator to react
    /// the moment the cap is hit rather than polling.
    ///
    /// The waiter is enrolled before the flag is read: `notify_waiters()` stores
    /// no permit, so a `trip()` racing between the read and the enrollment would
    /// otherwise be lost and the waiter would park forever.
    pub async fn killed(&self) {
        let n = self.wake.notified();
        tokio::pin!(n);
        n.as_mut().enable();
        if self.is_killed() {
            return;
        }
        n.await;
    }

    pub fn spent(&self) -> f64 {
        self.totals.lock().unwrap().spent_usd
    }

    pub fn calls(&self) -> u64 {
        self.totals.lock().unwrap().calls
    }

    pub fn per_model(&self) -> BTreeMap<String, ModelTotals> {
        self.totals.lock().unwrap().per_model.clone()
    }

    /// Consistent view of spend, call count, and per-model totals under a single
    /// lock, so a late commit can't make the receipt's fields disagree.
    pub fn snapshot(&self) -> Snapshot {
        let t = self.totals.lock().unwrap();
        Snapshot {
            spent_usd: t.spent_usd,
            calls: t.calls,
            per_model: t.per_model.clone(),
        }
    }
}

/// A point-in-time copy of the ledger totals.
pub struct Snapshot {
    pub spent_usd: f64,
    pub calls: u64,
    pub per_model: BTreeMap<String, ModelTotals>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            ..Default::default()
        }
    }

    #[test]
    fn stays_live_under_budget() {
        let l = Ledger::new(1.00);
        // opus: 1000 in + 1000 out = 0.005 + 0.025 = $0.03
        l.commit("claude-opus-4-8", &usage(1000, 1000));
        assert!(!l.is_killed());
        assert!(l.spent() > 0.0);
    }

    #[test]
    fn trips_when_a_call_crosses_the_cap() {
        let l = Ledger::new(0.05);
        l.commit("claude-opus-4-8", &usage(1000, 1000)); // $0.03, under
        assert!(!l.is_killed());
        l.commit("claude-opus-4-8", &usage(1000, 1000)); // $0.06 cumulative, over
        assert!(l.is_killed());
        assert!(l.kill_reason().unwrap().contains("cap"));
    }

    #[test]
    fn per_model_breakdown_accumulates() {
        let l = Ledger::new(100.0);
        l.commit("claude-opus-4-8", &usage(1000, 1000));
        l.commit("claude-haiku-4-5", &usage(500, 500));
        l.commit("claude-opus-4-8", &usage(1000, 0));
        let pm = l.per_model();
        assert_eq!(pm.get("claude-opus-4-8").unwrap().calls, 2);
        assert_eq!(pm.get("claude-haiku-4-5").unwrap().calls, 1);
    }

    #[tokio::test]
    async fn killed_future_resolves_on_trip() {
        let l = std::sync::Arc::new(Ledger::new(0.01));
        let l2 = l.clone();
        let waiter = tokio::spawn(async move { l2.killed().await });
        // Give the waiter a moment to park, then trip.
        tokio::task::yield_now().await;
        l.trip("test");
        waiter.await.unwrap();
        assert!(l.is_killed());
    }
}
