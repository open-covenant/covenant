import { readFileSync, writeFileSync } from "node:fs";
import { config, PRICING } from "./config.js";
import type { TokenUsage } from "./types.js";

export type RunOutcome = "completed" | "failed" | "cancelled";

export function modelCostUsd(model: string, u: TokenUsage): number {
  const p = PRICING[model] ?? PRICING["claude-sonnet-4-6"]!;
  return (
    (u.inputTokens * p.input +
      u.outputTokens * p.output +
      u.cacheReadTokens * p.cacheRead +
      u.cacheCreationTokens * p.cacheWrite) /
    1_000_000
  );
}

export function sandboxCostUsd(seconds: number): number {
  return seconds * config.sandboxUsdPerSec;
}

const utcDay = () => new Date().toISOString().slice(0, 10); // YYYY-MM-DD
const utcMonth = () => new Date().toISOString().slice(0, 7); // YYYY-MM

export interface BudgetCaps {
  dailyUsd: number;
  monthlyUsd: number;
  perRunUsdMax: number;
  maxConcurrent: number;
}

export type Reservation = { ok: true; max: number } | { ok: false; reason: string };

/**
 * Daily + monthly USD ledger with a concurrency cap and kill-switch.
 *
 * Admission reserves the per-run *maximum* up front and commits the *actual*
 * cost on completion, so a burst of concurrent runs can't collectively
 * overshoot the cap before any finishes. Counters reset on UTC day/month
 * rollover.
 *
 * Committed spend persists to LEDGER_PATH (a mounted disk) when set, so the
 * caps survive a restart instead of resetting the day's tally to zero; with
 * no path it stays in-memory. Reservations and the active count are transient
 * (in-flight runs don't survive a restart) and aren't persisted.
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

  constructor(
    private readonly caps: BudgetCaps = config,
    private readonly path = process.env.LEDGER_PATH?.trim() || undefined,
  ) {
    this.load();
  }

  private load(): void {
    if (!this.path) return;
    try {
      const s = JSON.parse(readFileSync(this.path, "utf8")) as {
        day?: string;
        month?: string;
        dailyUsd?: number;
        monthlyUsd?: number;
        outcomes?: Partial<Record<RunOutcome, number>>;
      };
      if (s.day === this.day && typeof s.dailyUsd === "number") this.dailyUsd = s.dailyUsd;
      if (s.month === this.month && typeof s.monthlyUsd === "number") this.monthlyUsd = s.monthlyUsd;
      // Outcomes are daily counters — only restore when the persisted day
      // still matches today, so the dashboard reflects today's run mix.
      if (s.day === this.day && s.outcomes) {
        for (const k of ["completed", "failed", "cancelled"] as RunOutcome[]) {
          if (typeof s.outcomes[k] === "number") this.outcomes[k] = s.outcomes[k] as number;
        }
      }
      if (this.dailyUsd > 0 || this.monthlyUsd > 0) {
        console.log(
          `ledger restored from ${this.path}: $${this.dailyUsd.toFixed(4)} today, $${this.monthlyUsd.toFixed(2)} this month`,
        );
      }
    } catch {
      // first boot or unreadable — start fresh
    }
  }

  private save(): void {
    if (!this.path) return;
    try {
      writeFileSync(
        this.path,
        JSON.stringify({
          day: this.day,
          month: this.month,
          dailyUsd: this.dailyUsd,
          monthlyUsd: this.monthlyUsd,
          outcomes: this.outcomes,
        }),
      );
    } catch (e) {
      console.error("ledger persist failed:", e);
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

  reserve(maxUsd: number = this.caps.perRunUsdMax): Reservation {
    this.roll();
    if (this.killed) return { ok: false, reason: "kill-switch engaged" };
    if (this.active >= this.caps.maxConcurrent) {
      return { ok: false, reason: "at capacity — try again shortly" };
    }
    if (this.dailyUsd + this.reserved + maxUsd > this.caps.dailyUsd) {
      return { ok: false, reason: "daily free capacity reached — resets at 00:00 UTC" };
    }
    if (this.monthlyUsd + this.reserved + maxUsd > this.caps.monthlyUsd) {
      return { ok: false, reason: "monthly capacity reached" };
    }
    this.reserved += maxUsd;
    this.active += 1;
    return { ok: true, max: maxUsd };
  }

  commit(reservedMax: number, actualUsd: number, outcome: RunOutcome = "completed"): void {
    this.roll();
    this.reserved = Math.max(0, this.reserved - reservedMax);
    this.dailyUsd += actualUsd;
    this.monthlyUsd += actualUsd;
    this.active = Math.max(0, this.active - 1);
    if (outcome in this.outcomes) this.outcomes[outcome] += 1;
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
    this.killed = true;
    const handlers = [...this.killHandlers];
    this.killHandlers.clear();
    for (const h of handlers) {
      try {
        h();
      } catch (e) {
        console.error("ledger kill handler failed:", e);
      }
    }
    this.save();
  }

  snapshot() {
    this.roll();
    return {
      dailyUsd: this.dailyUsd,
      monthlyUsd: this.monthlyUsd,
      reserved: this.reserved,
      active: this.active,
      killed: this.killed,
      dailyCap: this.caps.dailyUsd,
      monthlyCap: this.caps.monthlyUsd,
      outcomes: { ...this.outcomes },
    };
  }
}
