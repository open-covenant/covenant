import { describe, it, expect } from "vitest";
import { IpBucket } from "../src/ip-bucket.js";

function fixedClock(start: number): { now: () => number; advance: (ms: number) => void } {
  let t = start;
  return {
    now: () => t,
    advance: (ms) => {
      t += ms;
    },
  };
}

describe("IpBucket", () => {
  it("admits the first take and rejects the second until refill elapses", () => {
    const clock = fixedClock(1_000);
    const b = new IpBucket({ maxPerIp: 1, refillMs: 60_000 }, clock.now);

    expect(b.take("1.2.3.4").ok).toBe(true);
    const second = b.take("1.2.3.4");
    expect(second.ok).toBe(false);
    if (!second.ok) {
      expect(second.reason).toMatch(/per-IP concurrency cap/i);
      expect(second.retryAfterMs).toBe(60_000);
    }

    // After release + refill, the same IP can come back.
    b.release("1.2.3.4");
    clock.advance(60_000);
    expect(b.take("1.2.3.4").ok).toBe(true);
  });

  it("rate-limits a rapid-cycle client within the refill window", () => {
    const clock = fixedClock(0);
    const b = new IpBucket({ maxPerIp: 1, refillMs: 60_000 }, clock.now);

    expect(b.take("9.9.9.9").ok).toBe(true);
    b.release("9.9.9.9");
    // Same IP retries immediately — must be rate-limited even though
    // the active count is zero. Otherwise a client could burst-cycle a
    // cheap no-op run against the daily cap.
    clock.advance(5_000);
    const retry = b.take("9.9.9.9");
    expect(retry.ok).toBe(false);
    if (!retry.ok) {
      expect(retry.reason).toMatch(/rate limited/i);
      expect(retry.retryAfterMs).toBe(55_000);
    }
  });

  it("isolates IPs — one client's saturation does not affect another", () => {
    const clock = fixedClock(0);
    const b = new IpBucket({ maxPerIp: 1, refillMs: 60_000 }, clock.now);

    expect(b.take("1.1.1.1").ok).toBe(true);
    expect(b.take("1.1.1.1").ok).toBe(false);
    // 2.2.2.2 is independent and must succeed.
    expect(b.take("2.2.2.2").ok).toBe(true);
  });

  it("release is idempotent on unknown and double-release", () => {
    const b = new IpBucket({ maxPerIp: 1, refillMs: 60_000 });
    // Never seen — must not throw.
    b.release("never.seen");
    expect(b.snapshot().active).toBe(0);

    expect(b.take("a.b.c.d").ok).toBe(true);
    b.release("a.b.c.d");
    b.release("a.b.c.d");
    expect(b.snapshot().inflight).toBe(0);
  });

  it("snapshot reports active IPs, in-flight count, and rejected total", () => {
    const clock = fixedClock(0);
    const b = new IpBucket({ maxPerIp: 1, refillMs: 60_000 }, clock.now);

    b.take("1.1.1.1");
    b.take("2.2.2.2");
    b.take("1.1.1.1"); // rejected (cap)
    b.take("2.2.2.2"); // rejected (cap)
    const snap = b.snapshot();
    expect(snap.active).toBe(2);
    expect(snap.inflight).toBe(2);
    expect(snap.rejected).toBe(2);
    expect(snap.maxPerIp).toBe(1);
    expect(snap.refillMs).toBe(60_000);
  });

  it("prune drops idle entries past idleTtlMs, keeps active and recently-released", () => {
    const clock = fixedClock(0);
    const b = new IpBucket(
      { maxPerIp: 1, refillMs: 1_000, idleTtlMs: 5_000 },
      clock.now,
    );

    // IP 1: active, must NOT be pruned.
    b.take("active.host");
    // IP 2: idle and very stale, must be pruned.
    b.take("stale.host");
    b.release("stale.host");
    // IP 3: idle but recent, must NOT be pruned.
    clock.advance(6_000);
    b.take("recent.host");
    b.release("recent.host");

    const dropped = b.prune();
    expect(dropped).toBe(1);
    expect(b.size()).toBe(2);
  });

  it("derives a long idleTtlMs from a long refillMs (refillMs*10 wins over the default)", () => {
    // No idleTtlMs pinned → the constructor derives max(600_000 default,
    // refillMs*10). A 100_000ms refill window extends idleTtlMs to 1_000_000,
    // so an entry idle past the 600_000 default but before 1_000_000 survives.
    const clock = fixedClock(0);
    const b = new IpBucket({ refillMs: 100_000 }, clock.now);

    b.take("long.window");
    b.release("long.window");
    clock.advance(800_000); // past the 600_000 default, before the derived 1_000_000

    expect(b.prune()).toBe(0);
    expect(b.size()).toBe(1);
  });

  it("floors a short refillMs at the default idleTtlMs (max wins)", () => {
    // A 1_000ms refill window must not collapse idleTtlMs to 10_000 — the
    // max() floor keeps it at the 600_000 default, so an entry idle 500_000ms
    // (past 10_000, before 600_000) still survives.
    const clock = fixedClock(0);
    const b = new IpBucket({ refillMs: 1_000 }, clock.now);

    b.take("short.window");
    b.release("short.window");
    clock.advance(500_000); // past refillMs*10 (10_000), before the 600_000 floor

    expect(b.prune()).toBe(0);
    expect(b.size()).toBe(1);
  });

  it("triggers auto-prune every `pruneEvery` admissions", () => {
    const clock = fixedClock(0);
    // pruneEvery counts every take (including the seed). With
    // pruneEvery=5, the sweep fires on the 5th take.
    const b = new IpBucket(
      { maxPerIp: 1, refillMs: 1_000, idleTtlMs: 100, pruneEvery: 5 },
      clock.now,
    );

    // Seed an idle entry that will become stale during the loop.
    b.take("first.host");
    b.release("first.host");
    clock.advance(200);

    b.take("a.b.c.1"); // take #2
    b.take("a.b.c.2"); // take #3
    b.take("a.b.c.3"); // take #4 — no prune yet, first.host still present
    expect(b.size()).toBe(4);
    b.take("a.b.c.4"); // take #5 — crosses pruneEvery
    // first.host went stale (idle past idleTtlMs) before the prune
    // ran; sweep drops it. Active hosts remain.
    expect(b.size()).toBe(4);
  });

  it("rejects every take when maxPerIp is set to zero through the regular path", () => {
    const b = new IpBucket({ maxPerIp: 0, refillMs: 60_000 });
    // Operator opt-out — admission must be refused; the server.ts gate
    // short-circuits the bucket entirely when ipMaxPerIp=0.
    const denied = b.take("any");
    expect(denied.ok).toBe(false);
  });
});
