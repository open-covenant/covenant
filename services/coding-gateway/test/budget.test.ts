import { describe, it, expect } from "vitest";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SpendLedger, modelCostUsd, type BudgetCaps } from "../src/budget.js";

const caps: BudgetCaps = { dailyUsd: 6, monthlyUsd: 200, perRunUsdMax: 2, maxConcurrent: 10 };

describe("SpendLedger", () => {
  it("reserves the per-run max and admits up to the daily cap", () => {
    const l = new SpendLedger(caps);
    // Reservations alone (nothing committed yet) must not exceed the daily cap:
    // $2 + $2 + $2 = $6 fits; a 4th would be $8 > $6.
    expect(l.reserve().ok).toBe(true);
    expect(l.reserve().ok).toBe(true);
    expect(l.reserve().ok).toBe(true);
    const denied = l.reserve();
    expect(denied.ok).toBe(false);
    if (!denied.ok) expect(denied.reason).toMatch(/daily/);
  });

  it("commits actual cost and frees the reservation, so cheap runs free headroom", () => {
    const l = new SpendLedger(caps);
    const r = l.reserve();
    expect(r.ok).toBe(true);
    if (r.ok) l.commit(r.max, 0.1); // reserved $2, actually spent $0.10
    const snap = l.snapshot();
    expect(snap.reserved).toBe(0);
    expect(snap.dailyUsd).toBeCloseTo(0.1);
    expect(snap.active).toBe(0);
  });

  it("enforces the concurrency cap independent of spend", () => {
    const l = new SpendLedger({ ...caps, maxConcurrent: 2, dailyUsd: 1000 });
    expect(l.reserve().ok).toBe(true);
    expect(l.reserve().ok).toBe(true);
    const third = l.reserve();
    expect(third.ok).toBe(false);
    if (!third.ok) expect(third.reason).toMatch(/capacity/);
  });

  it("blocks the monthly cap even when the day has headroom", () => {
    const l = new SpendLedger({ dailyUsd: 100, monthlyUsd: 1, perRunUsdMax: 2, maxConcurrent: 10 });
    expect(l.reserve().ok).toBe(false);
  });

  it("kill-switch refuses all reservations", () => {
    const l = new SpendLedger(caps);
    l.kill();
    const r = l.reserve();
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toMatch(/kill/);
  });

  it("prices a run from token usage", () => {
    // Sonnet 4.6: $3/M in, $15/M out → 1M in + 100k out = $3 + $1.5 = $4.50
    const cost = modelCostUsd("claude-sonnet-4-6", {
      inputTokens: 1_000_000,
      outputTokens: 100_000,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    });
    expect(cost).toBeCloseTo(4.5);
  });

  it("persists committed spend to LEDGER_PATH so the caps survive a restart", () => {
    const path = join(tmpdir(), `covenant-ledger-${Date.now()}.json`);
    try {
      const before = new SpendLedger(caps, path);
      const r = before.reserve();
      expect(r.ok).toBe(true);
      if (r.ok) before.commit(r.max, 1.25);
      expect(before.snapshot().dailyUsd).toBeCloseTo(1.25);
      // a fresh ledger sharing the path = a restart: it reloads the committed tally
      const after = new SpendLedger(caps, path);
      expect(after.snapshot().dailyUsd).toBeCloseTo(1.25);
      expect(after.snapshot().monthlyUsd).toBeCloseTo(1.25);
      // in-flight reservations are transient and don't carry across the restart
      expect(after.snapshot().reserved).toBe(0);
    } finally {
      rmSync(path, { force: true });
    }
  });
});
