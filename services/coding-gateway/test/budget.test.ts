import { describe, it, expect, vi, afterEach } from "vitest";
import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SpendLedger, modelCostUsd, type BudgetCaps } from "../src/budget.js";

const caps: BudgetCaps = {
  dailyUsd: 6,
  monthlyUsd: 200,
  perRunUsdMax: 2,
  maxConcurrent: 10,
  wallMs: 600_000,
};

afterEach(() => {
  delete process.env.CODER_LEDGER_RESET_PENDING;
});

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
    if (r.ok) l.commit(r.id, r.max, 0.1); // reserved $2, actually spent $0.10
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
    const l = new SpendLedger({
      dailyUsd: 100,
      monthlyUsd: 1,
      perRunUsdMax: 2,
      maxConcurrent: 10,
      wallMs: 600_000,
    });
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
    if (r1.ok) l.commit(r1.id, r1.max, 0.5, "completed");
    if (r2.ok) l.commit(r2.id, r2.max, 0.5, "failed");
    if (r3.ok) l.commit(r3.id, r3.max, 0.5, "cancelled");
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

  it("falls back to Sonnet pricing for a model absent from the PRICING table", () => {
    // model flows in from config.model = CODER_MODEL, an operator-settable
    // string. A model not yet priced (new release, typo, comparison tier)
    // must bill as Sonnet rather than throw on undefined.input or silently
    // bill zero.
    const usage = { inputTokens: 1_000_000, outputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0 };
    const cost = modelCostUsd("claude-future-9", usage);
    expect(cost).toBeCloseTo(modelCostUsd("claude-sonnet-4-6", usage));
    expect(cost).toBeGreaterThan(0);
  });

  it("prices cache reads and creations at their distinct multipliers", () => {
    // Sonnet: cacheRead $0.3/M, cacheWrite $3.75/M (a 12.5x spread). Cached
    // turns dominate a long agent loop, so the two multipliers must stay
    // distinct — transposing them, or dropping either term, mis-accounts the
    // spend the ledger caps. 2M read + 1M write = $0.6 + $3.75 = $4.35.
    const cost = modelCostUsd("claude-sonnet-4-6", {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 2_000_000,
      cacheCreationTokens: 1_000_000,
    });
    expect(cost).toBeCloseTo(4.35);
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
      if (r1.ok) before.commit(r1.id, r1.max, 1.25, "completed");
      if (r2.ok) before.commit(r2.id, r2.max, 0.5, "failed");
      // a fresh ledger sharing the path = a restart: it reloads spend + outcomes
      const after = new SpendLedger(caps, path);
      expect(after.snapshot().dailyUsd).toBeCloseTo(1.75);
      expect(after.snapshot().monthlyUsd).toBeCloseTo(1.75);
      expect(after.snapshot().outcomes).toEqual({ completed: 1, failed: 1, cancelled: 0 });
      expect(after.snapshot().reserved).toBe(0);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it("reinstates unexpired pending reservations on restart (failure mode #1)", () => {
    // Mid-run crash: reserve was persisted, commit never ran. A fresh ledger
    // must treat the leftover microVM as a live reservation against the cap,
    // otherwise the restart admits a fresh max-spend wave on top of the wallet
    // still burning at the provider.
    const path = join(tmpdir(), `covenant-ledger-warm-${Date.now()}.json`);
    let mockNow = 1_000_000_000_000;
    const tinyCaps: BudgetCaps = { ...caps, dailyUsd: 4, maxConcurrent: 2, wallMs: 60_000 };
    try {
      const before = new SpendLedger(tinyCaps, path, () => mockNow);
      const r1 = before.reserve();
      const r2 = before.reserve();
      expect(r1.ok && r2.ok).toBe(true);
      // crash: skip commit on both

      // 30s later — both pending entries are unexpired (deadline at +60s)
      mockNow += 30_000;
      const after = new SpendLedger(tinyCaps, path, () => mockNow);
      const snap = after.snapshot();
      expect(snap.reserved).toBe(4);
      expect(snap.active).toBe(2);
      // The concurrency cap and the daily cap are both saturated — no fresh
      // admission until the leftover deadlines pass or an operator clears them.
      expect(after.reserve().ok).toBe(false);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it("prunes expired pending entries on boot so a clean shutdown doesn't wedge restart (failure mode #2)", () => {
    const path = join(tmpdir(), `covenant-ledger-expire-${Date.now()}.json`);
    let mockNow = 1_000_000_000_000;
    const tinyCaps: BudgetCaps = { ...caps, wallMs: 60_000 };
    try {
      const before = new SpendLedger(tinyCaps, path, () => mockNow);
      const r = before.reserve();
      expect(r.ok).toBe(true);

      // 2x wallMs later — every microVM that admission could have spawned has
      // self-destructed; restart MUST drop the marker, not pin admission at 0.
      mockNow += 120_000;
      const after = new SpendLedger(tinyCaps, path, () => mockNow);
      const snap = after.snapshot();
      expect(snap.reserved).toBe(0);
      expect(snap.active).toBe(0);
      expect(after.reserve().ok).toBe(true);
      // The pruned state is persisted to disk so a third boot doesn't replay
      // the expired markers.
      const onDisk = JSON.parse(readFileSync(path, "utf8")) as { pending: unknown[] };
      // After the post-prune reserve() the file contains exactly the new run,
      // never the expired stub from the crashed run.
      expect(Array.isArray(onDisk.pending)).toBe(true);
      expect(onDisk.pending.length).toBe(1);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it("removes the pending entry on commit so the file does not grow per-run (failure mode #3)", () => {
    const path = join(tmpdir(), `covenant-ledger-purge-${Date.now()}.json`);
    try {
      const l = new SpendLedger(caps, path);
      for (let i = 0; i < 3; i++) {
        const r = l.reserve();
        expect(r.ok).toBe(true);
        if (r.ok) l.commit(r.id, r.max, 0.1, "completed");
      }
      const onDisk = JSON.parse(readFileSync(path, "utf8")) as { pending: unknown[] };
      expect(onDisk.pending).toEqual([]);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it("CODER_LEDGER_RESET_PENDING=1 drops every pending reservation at boot (operator override)", () => {
    const path = join(tmpdir(), `covenant-ledger-reset-${Date.now()}.json`);
    let mockNow = 1_000_000_000_000;
    try {
      const before = new SpendLedger(caps, path, () => mockNow);
      const r = before.reserve();
      expect(r.ok).toBe(true);
      // Operator detects a crash-loop wedge and forces a clean slate.
      process.env.CODER_LEDGER_RESET_PENDING = "1";
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      try {
        const after = new SpendLedger(caps, path, () => mockNow);
        expect(after.snapshot().reserved).toBe(0);
        expect(after.snapshot().active).toBe(0);
        expect(after.reserve().ok).toBe(true);
        expect(warn).toHaveBeenCalledWith(
          expect.stringContaining("CODER_LEDGER_RESET_PENDING"),
        );
      } finally {
        warn.mockRestore();
      }
    } finally {
      rmSync(path, { force: true });
    }
  });

  it("refuses to boot when caps.wallMs is non-positive — silently disabling warm recovery would be worse than crash-loud", () => {
    expect(() => new SpendLedger({ ...caps, wallMs: 0 })).toThrow(/wallMs/i);
    expect(() => new SpendLedger({ ...caps, wallMs: -1 })).toThrow(/wallMs/i);
    expect(() => new SpendLedger({ ...caps, wallMs: Number.NaN })).toThrow(/wallMs/i);
  });

  it("a clean-state boot does not rewrite LEDGER_PATH (no crash-loop disk amplification)", () => {
    const path = join(tmpdir(), `covenant-ledger-clean-boot-${Date.now()}.json`);
    try {
      // Seed a clean file: spend committed, pending empty (the steady state).
      const seed = new SpendLedger(caps, path);
      const r = seed.reserve();
      expect(r.ok).toBe(true);
      if (r.ok) seed.commit(r.id, r.max, 0.5, "completed");
      const before = readFileSync(path, "utf8");

      // Booting on an already-clean ledger must NOT rewrite the file —
      // a crash-loop would otherwise pound the persistent disk every restart.
      // Mark the file with a "stale" mtime by mutating it to a known string and
      // then assert the load did not overwrite it. We assert byte-equality, which
      // is sufficient because save() would emit fresh JSON in a different order
      // only if it ran.
      const tag = `${before.slice(0, -1)},"witness":1}`; // append witness key
      writeFileSync(path, tag);
      const fresh = new SpendLedger(caps, path);
      const snap = fresh.snapshot();
      expect(snap.dailyUsd).toBeCloseTo(0.5); // load happened
      const after = readFileSync(path, "utf8");
      expect(after).toBe(tag); // file untouched — no clean-boot save()
    } finally {
      rmSync(path, { force: true });
    }
  });

  it("ignores garbage entries in the persisted pending array", () => {
    const path = join(tmpdir(), `covenant-ledger-garbage-${Date.now()}.json`);
    try {
      writeFileSync(
        path,
        JSON.stringify({
          day: new Date().toISOString().slice(0, 10),
          month: new Date().toISOString().slice(0, 7),
          dailyUsd: 0,
          monthlyUsd: 0,
          outcomes: { completed: 0, failed: 0, cancelled: 0 },
          pending: [
            null,
            { id: 42, reservedMax: 1, deadlineEpochMs: Date.now() + 60_000 },
            { id: "ok", reservedMax: 1, deadlineEpochMs: Date.now() + 60_000 },
            "string",
          ],
        }),
      );
      const l = new SpendLedger(caps, path);
      expect(l.snapshot().active).toBe(1);
      expect(l.snapshot().reserved).toBe(1);
    } finally {
      rmSync(path, { force: true });
    }
  });
});
