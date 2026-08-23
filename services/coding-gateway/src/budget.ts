import {
  closeSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { randomUUID } from 'node:crypto';
import { dirname } from 'node:path';
import { config, PRICING } from './config.js';
import type { TokenUsage } from './types.js';

export type RunOutcome = 'completed' | 'failed' | 'cancelled';

export function modelCostUsd(model: string, u: TokenUsage): number {
  const p = PRICING[model] ?? PRICING['claude-sonnet-4-6']!;
  return (
    (u.inputTokens * p.input +
      u.outputTokens * p.output +
      u.cacheReadTokens * p.cacheRead +
      u.cacheCreationTokens * p.cacheWrite) /
    1_000_000
  );
}

export function sandboxCostUsd(seconds: number): number {
  return seconds * config.sandboxWorstCaseUsdPerSec;
}

const utcDay = () => new Date().toISOString().slice(0, 10); // YYYY-MM-DD
const utcMonth = () => new Date().toISOString().slice(0, 7); // YYYY-MM

export interface BudgetCaps {
  dailyUsd: number;
  monthlyUsd: number;
  perRunUsdMax: number;
  maxConcurrent: number;
  /**
   * Wall-clock ceiling for a single run, in ms. Persisted with each pending
   * reservation as audit context; unresolved reservations are charged in full
   * on boot regardless of whether this deadline passed.
   */
  wallMs: number;
}

export type Reservation = { ok: true; max: number; id: string } | { ok: false; reason: string };

interface PendingEntry {
  id: string;
  runId: string;
  reservedMax: number;
  deadlineEpochMs: number;
}

interface PersistedLedger {
  day: string;
  month: string;
  dailyUsd: number;
  monthlyUsd: number;
  outcomes: Record<RunOutcome, number>;
  pending: PendingEntry[];
  killed?: boolean;
}

/**
 * Daily + monthly USD ledger with a concurrency cap and kill-switch.
 *
 * Admission reserves the per-run *maximum* up front and commits the terminal
 * accounting charge on completion. Callers may conservatively retain a
 * provider reservation when authoritative billing evidence is unavailable.
 * A burst of concurrent runs therefore can't collectively overshoot the cap
 * before any finishes. Counters reset on UTC day/month rollover.
 *
 * Committed spend persists to LEDGER_PATH (a mounted disk) when set, so the
 * caps survive a restart instead of resetting the day's tally to zero; with
 * no path it stays in-memory.
 *
 * In-flight reservations are also persisted as a `pending` list. On boot,
 * every pending entry is charged at its full reservation before it is
 * removed. This remains conservative even if the process was down past the
 * sandbox deadline or an obsolete reset environment variable is still set.
 */
export class SpendLedger {
  private day = utcDay();
  private month = utcMonth();
  private dailyUsd = 0;
  private monthlyUsd = 0;
  private reserved = 0;
  private active = 0;
  private killed = false;
  private outcomes: Record<RunOutcome, number> = { completed: 0, failed: 0, cancelled: 0 };
  private killHandlers = new Set<() => void>();
  private pending: PendingEntry[] = [];
  private persistenceError?: string;

  constructor(
    private readonly caps: BudgetCaps = config,
    private readonly path = process.env.LEDGER_PATH?.trim() || undefined,
    private readonly now: () => number = Date.now,
  ) {
    // wallMs is the gateway abort timer and persisted reservation deadline
    // metadata. Loud-fail rather than silently disabling either control.
    if (!Number.isFinite(this.caps.wallMs) || this.caps.wallMs <= 0) {
      throw new Error(
        `SpendLedger: wallMs must be a positive finite number, got ${this.caps.wallMs}; set CODER_WALL_MS to a sane per-run wall-clock ceiling`,
      );
    }
    this.load();
  }

  private load(): void {
    if (!this.path) return;
    let raw: string;
    try {
      raw = readFileSync(this.path, 'utf8');
    } catch (e) {
      const code = (e as NodeJS.ErrnoException).code;
      if (code === 'ENOENT') {
        this.persist();
        return;
      }
      throw new Error(
        `ledger load failed (${this.path}, code=${code ?? 'unknown'}): ${(e as Error).message}`,
      );
    }
    try {
      const parsed: unknown = JSON.parse(raw);
      if (!isPersistedLedger(parsed)) throw new Error('ledger document schema is invalid');
      const s = parsed;
      if (s.day === this.day) this.dailyUsd = s.dailyUsd;
      if (s.month === this.month) this.monthlyUsd = s.monthlyUsd;
      // Outcomes are daily counters — only restore when the persisted day
      // still matches today, so the dashboard reflects today's run mix.
      if (s.day === this.day) {
        for (const k of ['completed', 'failed', 'cancelled'] as RunOutcome[]) {
          this.outcomes[k] = s.outcomes[k];
        }
      }
      this.killed = s.killed === true;
      const recovered = this.reconcilePending(s.pending);
      if (this.dailyUsd > 0 || this.monthlyUsd > 0 || recovered > 0) {
        console.log(
          `ledger restored from ${this.path}: $${this.dailyUsd.toFixed(4)} today, $${this.monthlyUsd.toFixed(2)} this month, ${recovered} crashed reservation(s) charged at maximum`,
        );
      }
      if (recovered > 0) this.persist();
    } catch (e) {
      throw new Error(`ledger parse failed (${this.path}): ${(e as Error).message}`);
    }
  }

  /**
   * Charge every persisted pending reservation at its full maximum. The
   * deadline is retained as audit context but never weakens crash accounting:
   * the absence of a terminal receipt is the ambiguity that matters.
   */
  private reconcilePending(raw: unknown): number {
    if (!Array.isArray(raw)) throw new Error('ledger pending reservations are invalid');
    let recovered = 0;
    for (const entry of raw) {
      if (
        entry &&
        typeof entry === 'object' &&
        typeof (entry as PendingEntry).id === 'string' &&
        typeof (entry as PendingEntry).runId === 'string' &&
        (entry as PendingEntry).runId.length > 0 &&
        typeof (entry as PendingEntry).reservedMax === 'number' &&
        Number.isFinite((entry as PendingEntry).reservedMax) &&
        (entry as PendingEntry).reservedMax > 0 &&
        typeof (entry as PendingEntry).deadlineEpochMs === 'number'
      ) {
        const e = entry as PendingEntry;
        this.dailyUsd += e.reservedMax;
        this.monthlyUsd += e.reservedMax;
        this.outcomes.failed += 1;
        recovered += 1;
      } else {
        throw new Error('ledger contains an invalid pending reservation');
      }
    }
    this.pending = [];
    if (recovered > 0) {
      console.warn(
        `ledger crash recovery: charged ${recovered} pending reservation(s) at their full maximum`,
      );
    }
    return recovered;
  }

  private persist(): void {
    if (!this.path) return;
    mkdirSync(dirname(this.path), { recursive: true });
    const temp = `${this.path}.${process.pid}.${randomUUID()}.tmp`;
    let fd: number | undefined;
    try {
      writeFileSync(
        temp,
        JSON.stringify({
          day: this.day,
          month: this.month,
          dailyUsd: this.dailyUsd,
          monthlyUsd: this.monthlyUsd,
          outcomes: this.outcomes,
          pending: this.pending,
          killed: this.killed,
        }),
        { mode: 0o600, flag: 'wx' },
      );
      fd = openSync(temp, 'r');
      fsyncSync(fd);
      closeSync(fd);
      fd = undefined;
      renameSync(temp, this.path);
      const directory = openSync(dirname(this.path), 'r');
      try {
        fsyncSync(directory);
      } finally {
        closeSync(directory);
      }
      JSON.parse(readFileSync(this.path, 'utf8'));
    } finally {
      if (fd !== undefined) closeSync(fd);
      try {
        unlinkSync(temp);
      } catch (e) {
        const code = (e as NodeJS.ErrnoException).code;
        if (code !== 'ENOENT' && code !== 'ENOTDIR') throw e;
      }
    }
  }

  private save(): boolean {
    if (!this.path) return true;
    try {
      this.persist();
      return true;
    } catch (e) {
      this.persistenceError = (e as Error).message;
      this.engageKill();
      console.error(`ledger persist failed: ${this.persistenceError}`);
      return false;
    }
  }

  private roll(): void {
    const d = utcDay();
    const m = utcMonth();
    if (d !== this.day) {
      this.day = d;
      this.dailyUsd = 0;
      this.outcomes = { completed: 0, failed: 0, cancelled: 0 };
    }
    if (m !== this.month) {
      this.month = m;
      this.monthlyUsd = 0;
    }
  }

  reserve(maxUsd: number = this.caps.perRunUsdMax, runId: string = randomUUID()): Reservation {
    this.roll();
    if (!Number.isFinite(maxUsd) || maxUsd <= 0 || maxUsd > this.caps.perRunUsdMax) {
      return { ok: false, reason: 'invalid per-run spend cap' };
    }
    if (this.persistenceError) return { ok: false, reason: 'ledger persistence unavailable' };
    if (this.killed) return { ok: false, reason: 'kill-switch engaged' };
    if (this.active >= this.caps.maxConcurrent) {
      return { ok: false, reason: 'at capacity — try again shortly' };
    }
    if (this.dailyUsd + this.reserved + maxUsd > this.caps.dailyUsd) {
      return { ok: false, reason: 'daily free capacity reached — resets at 00:00 UTC' };
    }
    if (this.monthlyUsd + this.reserved + maxUsd > this.caps.monthlyUsd) {
      return { ok: false, reason: 'monthly capacity reached' };
    }
    this.reserved += maxUsd;
    this.active += 1;
    const id = randomUUID();
    const pending = {
      id,
      runId,
      reservedMax: maxUsd,
      deadlineEpochMs: this.now() + this.caps.wallMs,
    };
    this.pending.push(pending);
    // Persist before returning: the whole point is that a crash AFTER admit
    // returns to the caller still has the marker on disk.
    if (!this.save()) {
      this.pending.pop();
      this.reserved = Math.max(0, this.reserved - maxUsd);
      this.active = Math.max(0, this.active - 1);
      return { ok: false, reason: 'ledger persistence unavailable' };
    }
    return { ok: true, max: maxUsd, id };
  }

  commit(
    id: string,
    reservedMax: number,
    actualUsd: number,
    outcome: RunOutcome = 'completed',
  ): void {
    this.roll();
    const idx = this.pending.findIndex((p) => p.id === id);
    if (idx < 0) return;
    const pending = this.pending[idx]!;
    if (reservedMax !== pending.reservedMax) {
      throw new Error('reservation amount does not match the persisted ledger entry');
    }
    if (!Number.isFinite(actualUsd) || actualUsd < 0) throw new Error('actual cost is invalid');
    const overrun = actualUsd > pending.reservedMax + 1e-9;
    this.reserved = Math.max(0, this.reserved - pending.reservedMax);
    this.dailyUsd += actualUsd;
    this.monthlyUsd += actualUsd;
    this.active = Math.max(0, this.active - 1);
    if (outcome in this.outcomes) this.outcomes[outcome] += 1;
    // Purge the pending entry so the on-disk file doesn't grow per-run.
    this.pending.splice(idx, 1);
    if (overrun) this.engageKill();
    this.save();
  }

  /**
   * Register a teardown handler invoked when the kill-switch is engaged. The
   * server uses this to abort the in-flight run's AbortController so wall-clock
   * spend stops immediately instead of running to completion. Returns an
   * unsubscribe to call when the run finishes normally.
   */
  onKill(handler: () => void): () => void {
    if (this.killed) {
      handler();
      return () => {};
    }
    this.killHandlers.add(handler);
    return () => this.killHandlers.delete(handler);
  }

  /**
   * Engage the kill-switch: refuse new reservations and tear down in-flight
   * runs by signalling every registered teardown handler. Idempotent.
   */
  kill(): void {
    if (this.killed) return;
    this.engageKill();
    this.save();
  }

  private engageKill(): void {
    if (this.killed) return;
    this.killed = true;
    const handlers = [...this.killHandlers];
    this.killHandlers.clear();
    for (const h of handlers) {
      try {
        h();
      } catch (e) {
        console.error('ledger kill handler failed:', e);
      }
    }
  }

  snapshot() {
    this.roll();
    return {
      dailyUsd: this.dailyUsd,
      monthlyUsd: this.monthlyUsd,
      reserved: this.reserved,
      active: this.active,
      killed: this.killed,
      persistenceReady: this.persistenceError === undefined,
      dailyCap: this.caps.dailyUsd,
      monthlyCap: this.caps.monthlyUsd,
      outcomes: { ...this.outcomes },
    };
  }
}

function isPersistedLedger(value: unknown): value is PersistedLedger {
  if (!value || typeof value !== 'object') return false;
  const ledger = value as Partial<PersistedLedger>;
  const counter = (candidate: unknown): candidate is number =>
    typeof candidate === 'number' && Number.isFinite(candidate) && candidate >= 0;
  const outcome = (candidate: unknown): candidate is number =>
    counter(candidate) && Number.isSafeInteger(candidate);
  return (
    typeof ledger.day === 'string' &&
    /^\d{4}-\d{2}-\d{2}$/.test(ledger.day) &&
    typeof ledger.month === 'string' &&
    /^\d{4}-\d{2}$/.test(ledger.month) &&
    counter(ledger.dailyUsd) &&
    counter(ledger.monthlyUsd) &&
    Boolean(ledger.outcomes) &&
    outcome(ledger.outcomes?.completed) &&
    outcome(ledger.outcomes?.failed) &&
    outcome(ledger.outcomes?.cancelled) &&
    Array.isArray(ledger.pending) &&
    (ledger.killed === undefined || typeof ledger.killed === 'boolean')
  );
}
