import { describe, it, expect, vi } from "vitest";
import { rmSync, writeFileSync } from "node:fs";
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

  it("kill-switch aborts every in-flight run's AbortController (failure mode #3)", () => {
    const l = new SpendLedger(caps);
    const a = new AbortController();
    const b = new AbortController();
    const r1 = l.reserve();
    const r2 = l.reserve();
    expect(r1.ok && r2.ok).toBe(true);
    l.onKill(() => a.abort());
    l.onKill(() => b.abort());
    l.kill();
    expect(a.signal.aborted).toBe(true);
    expect(b.signal.aborted).toBe(true);
    // Idempotent: a second kill on already-aborted controllers must not throw.
    expect(() => l.kill()).not.toThrow();
  });

  it("kill-switch keeps tearing down even when a handler throws", () => {
    const l = new SpendLedger(caps);
    const survivor = new AbortController();
    l.onKill(() => {
      throw new Error("flaky handler");
    });
    l.onKill(() => survivor.abort());
    expect(() => l.kill()).not.toThrow();
    expect(survivor.signal.aborted).toBe(true);
  });

  it("onKill after kill fires the handler immediately (no missed teardown race)", () => {
    const l = new SpendLedger(caps);
    l.kill();
    let fired = false;
    l.onKill(() => {
      fired = true;
    });
    expect(fired).toBe(true);
  });

  it("unsubscribe stops a kill from invoking a finished run's handler", () => {
    const l = new SpendLedger(caps);
    let fired = false;
    const off = l.onKill(() => {
      fired = true;
    });
    off();
    l.kill();
    expect(fired).toBe(false);
  });

  it("records run outcomes in the snapshot for observability", () => {
    const l = new SpendLedger(caps);
    const r1 = l.reserve();
    const r2 = l.reserve();
    const r3 = l.reserve();
    expect(r1.ok && r2.ok && r3.ok).toBe(true);
    if (r1.ok) l.commit(r1.max, 0.5, "completed");
    if (r2.ok) l.commit(r2.max, 0.5, "failed");
    if (r3.ok) l.commit(r3.max, 0.5, "cancelled");
    const snap = l.snapshot();
    expect(snap.outcomes).toEqual({ completed: 1, failed: 1, cancelled: 1 });
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

  it("first-boot ENOENT loads silently — missing path is normal", () => {
    const path = join(tmpdir(), `covenant-ledger-missing-${Date.now()}.json`);
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      const l = new SpendLedger(caps, path);
      expect(l.snapshot().dailyUsd).toBe(0);
      expect(err).not.toHaveBeenCalled();
    } finally {
      err.mockRestore();
    }
  });

  it("malformed ledger file logs a parse error and starts fresh", () => {
    const path = join(tmpdir(), `covenant-ledger-bad-${Date.now()}.json`);
    writeFileSync(path, "{ not valid json");
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      const l = new SpendLedger(caps, path);
      expect(l.snapshot().dailyUsd).toBe(0);
      expect(err).toHaveBeenCalledTimes(1);
      expect(err.mock.calls[0]![0]).toMatch(/ledger parse failed/);
    } finally {
      err.mockRestore();
      rmSync(path, { force: true });
    }
  });

  it("persists committed spend and today's outcomes to LEDGER_PATH", () => {
    const path = join(tmpdir(), `covenant-ledger-${Date.now()}.json`);
    try {
      const before = new SpendLedger(caps, path);
      const r1 = before.reserve();
      const r2 = before.reserve();
      expect(r1.ok && r2.ok).toBe(true);
      if (r1.ok) before.commit(r1.max, 1.25, "completed");
      if (r2.ok) before.commit(r2.max, 0.5, "failed");
      // a fresh ledger sharing the path = a restart: it reloads spend + outcomes
      const after = new SpendLedger(caps, path);
      expect(after.snapshot().dailyUsd).toBeCloseTo(1.75);
      expect(after.snapshot().monthlyUsd).toBeCloseTo(1.75);
      expect(after.snapshot().outcomes).toEqual({ completed: 1, failed: 1, cancelled: 0 });
      // in-flight reservations are transient and don't carry across the restart
      expect(after.snapshot().reserved).toBe(0);
    } finally {
      rmSync(path, { force: true });
    }
  });
});
