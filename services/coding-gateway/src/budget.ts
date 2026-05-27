import { config, PRICING } from "./config.js";
import type { TokenUsage } from "./types.js";

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
 * In-memory daily + monthly USD ledger with a concurrency cap and kill-switch.
 *
 * Admission reserves the per-run *maximum* up front and commits the *actual*
 * cost on completion, so a burst of concurrent runs can't collectively
 * overshoot the cap before any finishes. Counters reset on UTC day/month
 * rollover.
 *
 * NOTE: in-memory — a gateway restart resets the day's tally, so the
 * provider-dashboard caps remain the restart-safe backstop. Persisting the
 * ledger (disk/KV) is a follow-on slice.
 */
export class SpendLedger {
  private day = utcDay();
  private month = utcMonth();
  private dailyUsd = 0;
  private monthlyUsd = 0;
  private reserved = 0;
  private active = 0;
  private killed = false;

  constructor(private readonly caps: BudgetCaps = config) {}

  private roll(): void {
    const d = utcDay();
    const m = utcMonth();
    if (d !== this.day) {
      this.day = d;
      this.dailyUsd = 0;
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

  commit(reservedMax: number, actualUsd: number): void {
    this.roll();
    this.reserved = Math.max(0, this.reserved - reservedMax);
    this.dailyUsd += actualUsd;
    this.monthlyUsd += actualUsd;
    this.active = Math.max(0, this.active - 1);
  }

  kill(): void {
    this.killed = true;
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
    };
  }
}
