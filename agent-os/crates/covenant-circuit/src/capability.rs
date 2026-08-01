use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{CircuitError, Result};

/// Process-local capability scope for explicit Circuit spend, checked before the payer.
///
/// Mirrors the guards in the Circuit SDK's `X402Client` — a per-call ceiling, a treasury
/// pin, and a cumulative budget — and adds an endpoint-host allowlist so a grant can only
/// pay the hosts it names. The amount and recipient in a 402 come from the (untrusted)
/// endpoint, so all four checks run before settlement. These checks do not provide durable
/// reservation or crash-recovery semantics and are not a daemon authorization boundary.
#[derive(Debug, Clone, Default)]
pub struct CircuitCapability {
    /// Per-call ceiling in raw CIRC base units. `None` disables the per-call check.
    pub max_spend_raw: Option<u64>,
    /// Cumulative ceiling across every call sharing a [`SpendLedger`]. `None` = unbounded.
    pub max_total_spend_raw: Option<u64>,
    /// Allowed 402 recipients (treasury pubkeys). Empty = allow any (not recommended).
    pub allowed_recipients: HashSet<String>,
    /// Allowed endpoint hosts (e.g. `inference.circuitllm.xyz`). Empty = allow any.
    pub allowed_hosts: HashSet<String>,
}

impl CircuitCapability {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn per_call(mut self, raw: u64) -> Self {
        self.max_spend_raw = Some(raw);
        self
    }

    pub fn budget(mut self, raw: u64) -> Self {
        self.max_total_spend_raw = Some(raw);
        self
    }

    pub fn allow_recipient(mut self, recipient: impl Into<String>) -> Self {
        self.allowed_recipients.insert(recipient.into());
        self
    }

    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        self.allowed_hosts.insert(host.into());
        self
    }

    pub(crate) fn check_per_call(&self, amount_raw: u64) -> Result<()> {
        match self.max_spend_raw {
            Some(cap) if amount_raw > cap => Err(CircuitError::SpendCap { amount_raw, cap }),
            _ => Ok(()),
        }
    }

    pub(crate) fn check_recipient(&self, recipient: &str) -> Result<()> {
        if !self.allowed_recipients.is_empty() && !self.allowed_recipients.contains(recipient) {
            return Err(CircuitError::RecipientNotAllowed(recipient.to_string()));
        }
        Ok(())
    }

    pub(crate) fn check_host(&self, host: &str) -> Result<()> {
        if !self.allowed_hosts.is_empty() && !self.allowed_hosts.contains(host) {
            return Err(CircuitError::EndpointNotAllowed(host.to_string()));
        }
        Ok(())
    }
}

/// Process-local cumulative CIRC spend shared across calls under a capability. Atomic so
/// one ledger can back concurrent clients, but neither durable nor safe across processes or
/// restarts.
#[derive(Debug, Default)]
pub struct SpendLedger {
    spent: AtomicU64,
}

impl SpendLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total raw CIRC settled (and reserved-in-flight) under this ledger.
    pub fn spent_raw(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// Reserve `amount_raw` against `budget` before paying, atomically, so two concurrent
    /// calls can't both slip under the ceiling. The caller [`refund`](Self::refund)s if the
    /// on-chain settle then fails.
    pub(crate) fn reserve(&self, amount_raw: u64, budget: Option<u64>) -> Result<()> {
        let Some(budget) = budget else {
            self.spent.fetch_add(amount_raw, Ordering::SeqCst);
            return Ok(());
        };
        let mut cur = self.spent.load(Ordering::Relaxed);
        loop {
            let next = cur.saturating_add(amount_raw);
            if next > budget {
                return Err(CircuitError::BudgetExceeded {
                    amount_raw,
                    spent: cur,
                    budget,
                });
            }
            match self
                .spent
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return Ok(()),
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn refund(&self, amount_raw: u64) {
        self.spent.fetch_sub(amount_raw, Ordering::SeqCst);
    }
}
