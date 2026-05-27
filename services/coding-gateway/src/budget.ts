import { readFileSync, writeFileSync } from "node:fs";
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
      };
      if (s.day === this.day && typeof s.dailyUsd === "number") this.dailyUsd = s.dailyUsd;
      if (s.month === this.month && typeof s.monthlyUsd === "number") this.monthlyUsd = s.monthlyUsd;
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
    this.save();
  }

  kill(): void {
    this.killed = true;
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
    };
  }
}
