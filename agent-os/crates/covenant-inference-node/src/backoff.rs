use std::time::Duration;

/// Capped exponential backoff with equal jitter for the tunnel's reconnect loop.
///
/// A flat sleep between reconnects both hammers a gateway that is briefly down
/// and adds needless latency once it recovers. A doubling ceiling clamped to
/// `cap`, with each slot jittering inside its band, spreads a fleet's reconnect
/// attempts instead of synchronising them into a thundering herd against a
/// gateway that just came back.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
}

impl Backoff {
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self { base, cap }
    }

    /// The deterministic upper bound for a zero-based retry attempt: `base`
    /// doubled `attempt` times, clamped to `cap`. Non-decreasing in `attempt` and
    /// never above `cap` — the two properties the reconnect loop relies on.
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let base = self.base.as_millis();
        let cap = self.cap.as_millis();
        let scaled = base.checked_shl(attempt).unwrap_or(u128::MAX).min(cap);
        Duration::from_millis(scaled.min(u128::from(u64::MAX)) as u64)
    }

    /// A jittered delay in `[ceiling/2, ceiling]`. `entropy` is any random `u64`,
    /// injected so the schedule is unit-testable; production draws it from the OS
    /// RNG. Equal jitter keeps a floor under the delay so retries never collapse
    /// back to hammering, while still de-correlating the fleet.
    pub fn sample(&self, attempt: u32, entropy: u64) -> Duration {
        let ceiling = self.ceiling(attempt).as_millis() as u64;
        let floor = ceiling / 2;
        let span = ceiling - floor;
        let jitter = if span == 0 { 0 } else { entropy % (span + 1) };
        Duration::from_millis(floor + jitter)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_secs(30))
    }
}
