/**
 * Per-source-IP admission gate that fronts the spend ledger on `/v1/runs`.
 *
 * One in-flight reservation per IP by default — a single anonymous client
 * cannot occupy both maxConcurrent slots and burn the entire daily cap
 * alone before Turnstile / edge gates land. Active counts are released
 * when the underlying run ends (success, failure, or cancel) so a legit
 * client can submit a follow-up immediately.
 *
 * Memory growth is bounded: every Nth admission triggers an LRU sweep
 * that drops entries whose active count is zero and have not been seen
 * in `idleTtlMs`. Without this, a scan that walks the entire IPv4 space
 * would leave 4 billion empty entries pinned forever.
 *
 * Rejections are tallied separately so the operator can see abuse
 * volume from the `/v1/budget` snapshot — failure mode 3 of
 * coder-12-gateway-per-ip-token-bucket would otherwise hide every
 * bucket-bounced request from the only monitoring surface this service
 * exposes today.
 */

export type IpBucketTakeOutcome =
  | { ok: true }
  | { ok: false; reason: string; retryAfterMs: number };

export interface IpBucketSnapshot {
  /** Number of IPs with at least one in-flight admission. */
  active: number;
  /** Total in-flight admissions across all IPs. */
  inflight: number;
  /** Rolling count of admissions rejected since the last snapshot reset. */
  rejected: number;
  /** Configured caps surfaced for operator visibility. */
  maxPerIp: number;
  refillMs: number;
}

export interface IpBucketConfig {
  /** Maximum simultaneous admissions per IP. Defaults to 1. */
  maxPerIp: number;
  /**
   * Minimum delay between a release and the next admission for the same
   * IP. Defaults to 60_000 ms. A non-zero value rate-limits a client that
   * cycles short runs to drain the daily cap.
   */
  refillMs: number;
  /**
   * Drop idle entries (zero active, last seen > idleTtlMs ago) during
   * the periodic LRU sweep. Defaults to 10x refillMs.
   */
  idleTtlMs: number;
  /**
   * Trigger an LRU sweep every `pruneEvery` admissions. Defaults to 256
   * — small enough that scan traffic cannot run the map up by more than
   * one chunk before pruning fires, large enough that the per-request
   * overhead is amortised.
   */
  pruneEvery: number;
}

interface Slot {
  active: number;
  // -1 sentinel for "never released" so a clock that returns 0 (any
  // synthetic test clock or a freshly-rebooted process at epoch) does
  // not collide with a legitimate release timestamp.
  lastReleaseMs: number;
  lastSeenMs: number;
}

const DEFAULT_CONFIG: IpBucketConfig = {
  maxPerIp: 1,
  refillMs: 60_000,
  idleTtlMs: 600_000,
  pruneEvery: 256,
};

export class IpBucket {
  private readonly slots = new Map<string, Slot>();
  private readonly config: IpBucketConfig;
  private rejectedSinceSnapshot = 0;
  private takesSinceLastPrune = 0;

  constructor(
    partial: Partial<IpBucketConfig> = {},
    private readonly now: () => number = Date.now,
  ) {
    this.config = {
      ...DEFAULT_CONFIG,
      ...partial,
      // Derive idleTtlMs from refillMs when the caller did not pin it
      // explicitly, so a custom short refill window cannot leave entries
      // pinned for a stale 10-minute default.
      idleTtlMs: partial.idleTtlMs ?? Math.max(DEFAULT_CONFIG.idleTtlMs, (partial.refillMs ?? DEFAULT_CONFIG.refillMs) * 10),
    };
  }

  /**
   * Attempt to admit one in-flight run for `ip`. Returns ok=true and
   * increments the active count when admission is granted; otherwise
   * returns a reason and the millisecond delay until the next attempt
   * can succeed.
   */
  take(ip: string): IpBucketTakeOutcome {
    this.takesSinceLastPrune += 1;
    if (this.takesSinceLastPrune >= this.config.pruneEvery) {
      this.prune();
      this.takesSinceLastPrune = 0;
    }
    const slot = this.slots.get(ip) ?? { active: 0, lastReleaseMs: -1, lastSeenMs: 0 };
    const nowMs = this.now();
    slot.lastSeenMs = nowMs;
    if (slot.active >= this.config.maxPerIp) {
      this.rejectedSinceSnapshot += 1;
      this.slots.set(ip, slot);
      return {
        ok: false,
        reason: `per-IP concurrency cap (${this.config.maxPerIp}) reached for this source`,
        retryAfterMs: this.config.refillMs,
      };
    }
    if (slot.lastReleaseMs >= 0) {
      const since = nowMs - slot.lastReleaseMs;
      if (since < this.config.refillMs) {
        this.rejectedSinceSnapshot += 1;
        this.slots.set(ip, slot);
        return {
          ok: false,
          reason: "rate limited; try again shortly",
          retryAfterMs: this.config.refillMs - since,
        };
      }
    }
    slot.active += 1;
    this.slots.set(ip, slot);
    return { ok: true };
  }

  /**
   * Mark one in-flight run for `ip` as finished. Idempotent on unknown
   * IPs and bottoms out at zero so a double-release does not underflow.
   */
  release(ip: string): void {
    const slot = this.slots.get(ip);
    if (!slot) return;
    if (slot.active > 0) slot.active -= 1;
    slot.lastReleaseMs = this.now();
    slot.lastSeenMs = slot.lastReleaseMs;
    this.slots.set(ip, slot);
  }

  snapshot(): IpBucketSnapshot {
    let inflight = 0;
    let active = 0;
    for (const slot of this.slots.values()) {
      if (slot.active > 0) {
        active += 1;
        inflight += slot.active;
      }
    }
    return {
      active,
      inflight,
      rejected: this.rejectedSinceSnapshot,
      maxPerIp: this.config.maxPerIp,
      refillMs: this.config.refillMs,
    };
  }

  /**
   * Drop entries that have no in-flight admissions and have not been
   * seen recently. Bounds memory under scan traffic.
   */
  prune(): number {
    const cutoff = this.now() - this.config.idleTtlMs;
    let dropped = 0;
    for (const [ip, slot] of this.slots) {
      if (slot.active === 0 && slot.lastSeenMs < cutoff) {
        this.slots.delete(ip);
        dropped += 1;
      }
    }
    return dropped;
  }

  /** Test seam — does not affect rejected counter. */
  size(): number {
    return this.slots.size;
  }
}
