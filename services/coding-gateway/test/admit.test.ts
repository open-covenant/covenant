import { describe, it, expect } from "vitest";
import { admitRun } from "../src/admit.js";
import { IpBucket } from "../src/ip-bucket.js";
import { SpendLedger, type BudgetCaps } from "../src/budget.js";

const caps: BudgetCaps = { dailyUsd: 6, monthlyUsd: 200, perRunUsdMax: 2, maxConcurrent: 10 };

describe("admitRun", () => {
  it("admits when both the per-IP bucket and the ledger accept", () => {
    const ipBucket = new IpBucket({ maxPerIp: 1, refillMs: 60_000 });
    const ledger = new SpendLedger(caps);

    const result = admitRun({ ip: "1.1.1.1", ipBucket, ledger, ipMaxPerIp: 1 });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.reservedMax).toBe(caps.perRunUsdMax);
    expect(ipBucket.snapshot().inflight).toBe(1);
  });

  it("releases the IP slot when the ledger rejects, so the rejection does NOT cost a refill window", () => {
    // Closes failure mode 3 from coder-12 explicitly: a ledger-denied
    // run was previously masking abuse from /v1/budget AND counting
    // against the per-IP cap. The release-on-ledger-deny path must
    // run, or a legitimate client whose first try arrived in a
    // saturated minute would be punished for the daemon's overload.
    const ipBucket = new IpBucket({ maxPerIp: 1, refillMs: 60_000 });
    const ledger = new SpendLedger({ ...caps, maxConcurrent: 0 }); // every reserve fails

    const result = admitRun({ ip: "1.1.1.1", ipBucket, ledger, ipMaxPerIp: 1 });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toMatch(/capacity/i);

    // The slot must be released so the same IP can retry without
    // waiting out the refill window — the ledger denial is not the
    // client's fault.
    expect(ipBucket.snapshot().inflight).toBe(0);
  });

  it("returns the bucket's reason when the IP cap fires before the ledger", () => {
    const ipBucket = new IpBucket({ maxPerIp: 1, refillMs: 60_000 });
    const ledger = new SpendLedger(caps);
    admitRun({ ip: "1.1.1.1", ipBucket, ledger, ipMaxPerIp: 1 });

    // Second submission from same IP — bucket rejects before ledger
    // is consulted. Ledger.active must stay at 1, otherwise we
    // double-counted and a future legit client would race the cap.
    const second = admitRun({ ip: "1.1.1.1", ipBucket, ledger, ipMaxPerIp: 1 });
    expect(second.ok).toBe(false);
    if (!second.ok) {
      expect(second.reason).toMatch(/per-IP concurrency cap/i);
      expect(second.retryAfterMs).toBe(60_000);
    }
    expect(ledger.snapshot().active).toBe(1);
  });

  it("skips the per-IP bucket entirely when ipMaxPerIp is zero", () => {
    // Explicit opt-out by the operator. The bucket itself would also
    // refuse (its own maxPerIp=0 short-circuit) but the server-level
    // gate must not even consult it — otherwise the snapshot's
    // `rejected` counter would surge on every public POST.
    const ipBucket = new IpBucket({ maxPerIp: 1, refillMs: 60_000 });
    const ledger = new SpendLedger(caps);

    const first = admitRun({ ip: "1.1.1.1", ipBucket, ledger, ipMaxPerIp: 0 });
    const second = admitRun({ ip: "1.1.1.1", ipBucket, ledger, ipMaxPerIp: 0 });
    expect(first.ok).toBe(true);
    expect(second.ok).toBe(true);
    expect(ipBucket.snapshot().rejected).toBe(0);
  });
});
