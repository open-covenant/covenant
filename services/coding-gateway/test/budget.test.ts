import { describe, it, expect, vi, afterEach } from 'vitest';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { SpendLedger, modelCostUsd, sandboxCostUsd, type BudgetCaps } from '../src/budget.js';
import { config } from '../src/config.js';

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

describe('SpendLedger', () => {
  it('reserves the per-run max and admits up to the daily cap', () => {
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

  it('commits actual cost and frees the reservation, so cheap runs free headroom', () => {
    const l = new SpendLedger(caps);
    const r = l.reserve();
    expect(r.ok).toBe(true);
    if (r.ok) l.commit(r.id, r.max, 0.1); // reserved $2, actually spent $0.10
    const snap = l.snapshot();
    expect(snap.reserved).toBe(0);
    expect(snap.dailyUsd).toBeCloseTo(0.1);
    expect(snap.active).toBe(0);
  });

  it('clamps active and reserved at zero on a duplicate commit (no underflow)', () => {
    // A double-dispatched completion (retry, missed splice) calls commit twice
    // on the same id. The Math.max(0, ...) floors keep active/reserved from
    // going negative — an underflowed active would silently inflate the
    // concurrency gate's headroom (reserve checks active >= maxConcurrent).
    // Parity with ip-bucket's "release is idempotent on double-release".
    const l = new SpendLedger(caps);
    const r = l.reserve();
    expect(r.ok).toBe(true);
    if (r.ok) {
      l.commit(r.id, r.max, 0.1, 'completed');
      l.commit(r.id, r.max, 0.1, 'completed'); // duplicate
    }
    const snap = l.snapshot();
    expect(snap.active).toBe(0);
    expect(snap.reserved).toBe(0);
    expect(snap.dailyUsd).toBeCloseTo(0.1);
  });

  it('rejects invalid or mismatched committed costs', () => {
    const l = new SpendLedger(caps);
    const reservation = l.reserve();
    expect(reservation.ok).toBe(true);
    if (!reservation.ok) return;
    expect(() => l.commit(reservation.id, reservation.max + 1, 0.1)).toThrow(/reservation amount/);
    expect(() => l.commit(reservation.id, reservation.max, -1)).toThrow(/actual cost/);
    expect(() => l.commit(reservation.id, reservation.max, Number.NaN)).toThrow(/actual cost/);
    expect(l.snapshot().active).toBe(1);
  });

  it('records an overrun without clamping it and persists the kill switch', () => {
    const path = join(tmpdir(), `covenant-ledger-overrun-${Date.now()}.json`);
    try {
      const before = new SpendLedger(caps, path);
      const reservation = before.reserve(0.5);
      expect(reservation.ok).toBe(true);
      if (!reservation.ok) return;
      before.commit(reservation.id, reservation.max, 0.75, 'failed');
      expect(before.snapshot()).toMatchObject({ dailyUsd: 0.75, monthlyUsd: 0.75, killed: true });
      expect(before.reserve(0.1)).toMatchObject({ ok: false, reason: 'kill-switch engaged' });

      const after = new SpendLedger(caps, path);
      expect(after.snapshot()).toMatchObject({ dailyUsd: 0.75, monthlyUsd: 0.75, killed: true });
      expect(after.reserve(0.1)).toMatchObject({ ok: false, reason: 'kill-switch engaged' });
      expect(after.reserve(0.1, true)).toMatchObject({
        ok: false,
        reason: 'kill-switch engaged',
      });
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('persists an operator kill across restart and blocks exempt admission', () => {
    const path = join(tmpdir(), `covenant-ledger-killed-${Date.now()}.json`);
    try {
      const before = new SpendLedger(caps, path);
      before.kill();

      const after = new SpendLedger(caps, path);
      expect(after.snapshot().killed).toBe(true);
      expect(after.reserve(0.1, true)).toMatchObject({
        ok: false,
        reason: 'kill-switch engaged',
      });
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('enforces the concurrency cap independent of spend', () => {
    const l = new SpendLedger({ ...caps, maxConcurrent: 2, dailyUsd: 1000 });
    expect(l.reserve().ok).toBe(true);
    expect(l.reserve().ok).toBe(true);
    const third = l.reserve();
    expect(third.ok).toBe(false);
    if (!third.ok) expect(third.reason).toMatch(/capacity/);
  });

  it('blocks the monthly cap even when the day has headroom', () => {
    const l = new SpendLedger({
      dailyUsd: 100,
      monthlyUsd: 1,
      perRunUsdMax: 2,
      maxConcurrent: 10,
      wallMs: 600_000,
    });
    expect(l.reserve().ok).toBe(false);
  });

  it('kill-switch refuses all reservations', () => {
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

  it('kill-switch keeps tearing down even when a handler throws', () => {
    const l = new SpendLedger(caps);
    const survivor = new AbortController();
    l.onKill(() => {
      throw new Error('flaky handler');
    });
    l.onKill(() => survivor.abort());
    expect(() => l.kill()).not.toThrow();
    expect(survivor.signal.aborted).toBe(true);
  });

  it('onKill after kill fires the handler immediately (no missed teardown race)', () => {
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

  it('records run outcomes in the snapshot for observability', () => {
    const l = new SpendLedger(caps);
    const r1 = l.reserve();
    const r2 = l.reserve();
    const r3 = l.reserve();
    expect(r1.ok && r2.ok && r3.ok).toBe(true);
    if (r1.ok) l.commit(r1.id, r1.max, 0.5, 'completed');
    if (r2.ok) l.commit(r2.id, r2.max, 0.5, 'failed');
    if (r3.ok) l.commit(r3.id, r3.max, 0.5, 'cancelled');
    const snap = l.snapshot();
    expect(snap.outcomes).toEqual({ completed: 1, failed: 1, cancelled: 1 });
  });

  it('prices a run from token usage', () => {
    // Sonnet 4.6: $3/M in, $15/M out → 1M in + 100k out = $3 + $1.5 = $4.50
    const cost = modelCostUsd('claude-sonnet-4-6', {
      inputTokens: 1_000_000,
      outputTokens: 100_000,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    });
    expect(cost).toBeCloseTo(4.5);
  });

  it('falls back to Sonnet pricing for a model absent from the PRICING table', () => {
    // model flows in from config.model = CODER_MODEL, an operator-settable
    // string. A model not yet priced (new release, typo, comparison tier)
    // must bill as Sonnet rather than throw on undefined.input or silently
    // bill zero.
    const usage = {
      inputTokens: 1_000_000,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    };
    const cost = modelCostUsd('claude-future-9', usage);
    expect(cost).toBeCloseTo(modelCostUsd('claude-sonnet-4-6', usage));
    expect(cost).toBeGreaterThan(0);
  });

  it('prices cache reads and creations at their distinct multipliers', () => {
    // Sonnet: cacheRead $0.3/M, cacheWrite $3.75/M (a 12.5x spread). Cached
    // turns dominate a long agent loop, so the two multipliers must stay
    // distinct — transposing them, or dropping either term, mis-accounts the
    // spend the ledger caps. 2M read + 1M write = $0.6 + $3.75 = $4.35.
    const cost = modelCostUsd('claude-sonnet-4-6', {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 2_000_000,
      cacheCreationTokens: 1_000_000,
    });
    expect(cost).toBeCloseTo(4.35);
  });

  it('first-boot ENOENT loads silently — missing path is normal', () => {
    const path = join(tmpdir(), `covenant-ledger-missing-${Date.now()}.json`);
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    try {
      const l = new SpendLedger(caps, path);
      expect(l.snapshot().dailyUsd).toBe(0);
      expect(err).not.toHaveBeenCalled();
    } finally {
      err.mockRestore();
    }
  });

  it('trips the kill switch and refuses admission after a persistence failure', () => {
    const directory = mkdtempSync(join(tmpdir(), 'covenant-ledger-fail-'));
    const path = join(directory, 'ledger.json');
    const ledger = new SpendLedger(caps, path);
    let killed = false;
    ledger.onKill(() => {
      killed = true;
    });
    rmSync(path, { force: true });
    rmSync(directory, { recursive: true, force: true });
    writeFileSync(directory, 'not a directory');
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    try {
      const reservation = ledger.reserve();
      expect(reservation).toMatchObject({ ok: false, reason: 'ledger persistence unavailable' });
      expect(killed).toBe(true);
      expect(ledger.snapshot()).toMatchObject({
        killed: true,
        persistenceReady: false,
        active: 0,
        reserved: 0,
      });
    } finally {
      error.mockRestore();
      rmSync(directory, { force: true });
    }
  });

  it('fails closed on a non-ENOENT ledger read error', () => {
    const dir = mkdtempSync(join(tmpdir(), 'covenant-ledger-dir-'));
    try {
      expect(() => new SpendLedger(caps, dir)).toThrow(/ledger load failed/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it('fails closed on a malformed ledger file', () => {
    const path = join(tmpdir(), `covenant-ledger-bad-${Date.now()}.json`);
    writeFileSync(path, '{ not valid json');
    try {
      expect(() => new SpendLedger(caps, path)).toThrow(/ledger parse failed/);
    } finally {
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
      if (r1.ok) before.commit(r1.id, r1.max, 1.25, 'completed');
      if (r2.ok) before.commit(r2.id, r2.max, 0.5, 'failed');
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

  it('charges crashed reservations at their full maximum on restart', () => {
    const path = join(tmpdir(), `covenant-ledger-warm-${Date.now()}.json`);
    let mockNow = 1_000_000_000_000;
    const tinyCaps: BudgetCaps = { ...caps, dailyUsd: 4, maxConcurrent: 2, wallMs: 60_000 };
    try {
      const before = new SpendLedger(tinyCaps, path, () => mockNow);
      const r1 = before.reserve();
      const r2 = before.reserve();
      expect(r1.ok && r2.ok).toBe(true);
      // crash: skip commit on both

      mockNow += 30_000;
      const after = new SpendLedger(tinyCaps, path, () => mockNow);
      const snap = after.snapshot();
      expect(snap.reserved).toBe(0);
      expect(snap.active).toBe(0);
      expect(snap.dailyUsd).toBe(4);
      expect(snap.monthlyUsd).toBe(4);
      expect(snap.outcomes.failed).toBe(2);
      expect(after.reserve().ok).toBe(false);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('charges crashed reservations even after their wall deadline', () => {
    const path = join(tmpdir(), `covenant-ledger-expire-${Date.now()}.json`);
    let mockNow = 1_000_000_000_000;
    const tinyCaps: BudgetCaps = { ...caps, wallMs: 60_000 };
    try {
      const before = new SpendLedger(tinyCaps, path, () => mockNow);
      const r = before.reserve();
      expect(r.ok).toBe(true);

      mockNow += 120_000;
      const after = new SpendLedger(tinyCaps, path, () => mockNow);
      const snap = after.snapshot();
      expect(snap.reserved).toBe(0);
      expect(snap.active).toBe(0);
      expect(snap.dailyUsd).toBe(2);
      expect(after.reserve().ok).toBe(true);
      const onDisk = JSON.parse(readFileSync(path, 'utf8')) as { pending: unknown[] };
      expect(Array.isArray(onDisk.pending)).toBe(true);
      expect(onDisk.pending.length).toBe(1);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('does not let pending deadlines alter conservative crash accounting', () => {
    const tinyCaps: BudgetCaps = { ...caps, wallMs: 60_000 };
    const T = 1_000_000_000_000;
    const seed = (path: string) => {
      const before = new SpendLedger(tinyCaps, path, () => T);
      expect(before.reserve().ok).toBe(true); // deadlineEpochMs = T + 60_000
    };
    const atPath = join(tmpdir(), `covenant-ledger-deadline-eq-${Date.now()}.json`);
    const ltPath = join(tmpdir(), `covenant-ledger-deadline-lt-${Date.now()}-2.json`);
    try {
      seed(atPath);
      seed(ltPath);

      const atDeadline = new SpendLedger(tinyCaps, atPath, () => T + 60_000).snapshot();
      expect(atDeadline.reserved).toBe(0);
      expect(atDeadline.active).toBe(0);
      expect(atDeadline.dailyUsd).toBe(tinyCaps.perRunUsdMax);

      const oneMsEarlier = new SpendLedger(tinyCaps, ltPath, () => T + 60_000 - 1).snapshot();
      expect(oneMsEarlier.reserved).toBe(0);
      expect(oneMsEarlier.active).toBe(0);
      expect(oneMsEarlier.dailyUsd).toBe(tinyCaps.perRunUsdMax);
    } finally {
      rmSync(atPath, { force: true });
      rmSync(ltPath, { force: true });
    }
  });

  it('removes the pending entry on commit so the file does not grow per-run (failure mode #3)', () => {
    const path = join(tmpdir(), `covenant-ledger-purge-${Date.now()}.json`);
    try {
      const l = new SpendLedger(caps, path);
      for (let i = 0; i < 3; i++) {
        const r = l.reserve();
        expect(r.ok).toBe(true);
        if (r.ok) l.commit(r.id, r.max, 0.1, 'completed');
      }
      const onDisk = JSON.parse(readFileSync(path, 'utf8')) as { pending: unknown[] };
      expect(onDisk.pending).toEqual([]);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('does not let an environment override discard crashed spend', () => {
    const path = join(tmpdir(), `covenant-ledger-reset-${Date.now()}.json`);
    let mockNow = 1_000_000_000_000;
    try {
      const before = new SpendLedger(caps, path, () => mockNow);
      const r = before.reserve();
      expect(r.ok).toBe(true);
      process.env.CODER_LEDGER_RESET_PENDING = '1';
      const after = new SpendLedger(caps, path, () => mockNow);
      expect(after.snapshot().dailyUsd).toBe(caps.perRunUsdMax);
      expect(after.snapshot().outcomes.failed).toBe(1);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('refuses to boot when caps.wallMs is non-positive — silently disabling warm recovery would be worse than crash-loud', () => {
    expect(() => new SpendLedger({ ...caps, wallMs: 0 })).toThrow(/wallMs/i);
    expect(() => new SpendLedger({ ...caps, wallMs: -1 })).toThrow(/wallMs/i);
    expect(() => new SpendLedger({ ...caps, wallMs: Number.NaN })).toThrow(/wallMs/i);
  });

  it('a clean-state boot does not rewrite LEDGER_PATH (no crash-loop disk amplification)', () => {
    const path = join(tmpdir(), `covenant-ledger-clean-boot-${Date.now()}.json`);
    try {
      // Seed a clean file: spend committed, pending empty (the steady state).
      const seed = new SpendLedger(caps, path);
      const r = seed.reserve();
      expect(r.ok).toBe(true);
      if (r.ok) seed.commit(r.id, r.max, 0.5, 'completed');
      const before = readFileSync(path, 'utf8');

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
      const after = readFileSync(path, 'utf8');
      expect(after).toBe(tag); // file untouched — no clean-boot save()
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('fails closed on garbage entries in the persisted pending array', () => {
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
            { id: 'ok', reservedMax: 1, deadlineEpochMs: Date.now() + 60_000 },
            'string',
          ],
        }),
      );
      expect(() => new SpendLedger(caps, path)).toThrow(/invalid pending reservation/);
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('discards a persisted daily counter from a previous day so a new-day boot gets a fresh daily cap', () => {
    // The daily tally is a same-day counter: a restart on a new day must start at
    // $0, not inherit yesterday's spend (which would silently eat today's headroom).
    // month is held at the current month so this isolates the day-equality guard
    // while confirming the monthly tally still carries across the day boundary.
    const path = join(tmpdir(), `covenant-ledger-staleday-${Date.now()}.json`);
    const month = new Date().toISOString().slice(0, 7);
    try {
      writeFileSync(
        path,
        JSON.stringify({
          day: '2000-01-01', // a day that is never today
          month,
          dailyUsd: 5,
          monthlyUsd: 5,
          outcomes: { completed: 0, failed: 0, cancelled: 0 },
          pending: [],
        }),
      );
      const snap = new SpendLedger(caps, path).snapshot();
      expect(snap.dailyUsd).toBe(0); // stale day → daily counter reset
      expect(snap.monthlyUsd).toBeCloseTo(5); // same month → monthly counter carried
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('discards a persisted monthly counter from a previous month so a new-month boot gets a fresh monthly cap', () => {
    // Symmetric to the daily case at the month granularity: a boot in a new month
    // must not inherit last month's spend against the monthly cap, while the daily
    // tally still restores when the persisted day matches today.
    const path = join(tmpdir(), `covenant-ledger-stalemonth-${Date.now()}.json`);
    const day = new Date().toISOString().slice(0, 10);
    try {
      writeFileSync(
        path,
        JSON.stringify({
          day,
          month: '2000-01', // a month that is never now
          dailyUsd: 3,
          monthlyUsd: 99,
          outcomes: { completed: 0, failed: 0, cancelled: 0 },
          pending: [],
        }),
      );
      const snap = new SpendLedger(caps, path).snapshot();
      expect(snap.monthlyUsd).toBe(0); // stale month → monthly counter reset
      expect(snap.dailyUsd).toBeCloseTo(3); // same day → daily counter carried
    } finally {
      rmSync(path, { force: true });
    }
  });

  it('fails closed on a non-numeric persisted counter', () => {
    const path = join(tmpdir(), `covenant-ledger-corrupt-${Date.now()}.json`);
    const day = new Date().toISOString().slice(0, 10);
    const month = new Date().toISOString().slice(0, 7);
    try {
      writeFileSync(
        path,
        JSON.stringify({
          day,
          month,
          dailyUsd: '5', // corrupt: a string where a number is required
          monthlyUsd: 0,
          outcomes: { completed: 0, failed: 0, cancelled: 0 },
          pending: [],
        }),
      );
      expect(() => new SpendLedger(caps, path)).toThrow(/schema is invalid/);
    } finally {
      rmSync(path, { force: true });
    }
  });
});

describe('sandboxCostUsd', () => {
  // The completion commit charges modelCostUsd(model, usage) + sandboxCostUsd(seconds)
  // (server.ts), and that sum is what the daily/monthly caps meter — so the
  // sandbox term bounds how long a run can burn wall clock before admission refuses.
  it('charges nothing for a zero-second run', () => {
    // The cheapest `*`->`+` tripwire: 0 * rate is 0, but 0 + rate would bill a
    // flat per-second charge on a run that used no sandbox time at all.
    expect(sandboxCostUsd(0)).toBe(0);
  });

  it('scales linearly with wall-clock seconds', () => {
    // (2s)*rate == 2*(s*rate) but (2s)+rate != 2*(s+rate): linearity catches a
    // `+` or otherwise non-linear mutation without depending on the exact rate.
    expect(sandboxCostUsd(7200)).toBeCloseTo(2 * sandboxCostUsd(3600));
    expect(sandboxCostUsd(3600)).toBeGreaterThan(0);
  });

  it('meters at the configured default rate of $0.0001/s', () => {
    // A zeroed/garbled CODER_SANDBOX_USD_PER_SEC would silently stop metering
    // sandbox wall-clock entirely. Pin the default against an independent
    // constant (10000s -> $1), not a config-derived self-check.
    expect(config.sandboxUsdPerSec).toBe(0.0001);
    expect(sandboxCostUsd(10_000)).toBeCloseTo(1);
  });
});
