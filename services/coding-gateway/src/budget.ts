import { readFileSync, writeFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
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
  /**
   * Wall-clock ceiling for a single run, in ms. Used as the reservation
   * deadline horizon: a persisted pending reservation is treated as live
   * on boot until `Date.now()` passes this many ms past its admission, after
   * which the sandbox's own self-destruct guarantees the leftover spend is
   * gone.
   */
  wallMs: number;
}

export type Reservation =
  | { ok: true; max: number; id: string }
  | { ok: false; reason: string };

interface PendingEntry {
  id: string;
  reservedMax: number;
  deadlineEpochMs: number;
}

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
 * no path it stays in-memory.
 *
 * In-flight reservations are also persisted as a `pending` list keyed by
 * deadlineEpochMs (admission time + wallMs). On boot, every unexpired entry
 * is reinstated as a live reservation so a crash mid-run can't admit a
 * fresh max-spend wave on top of microVMs that are still burning wallet
 * until their own wall-clock self-destruct. Expired entries are pruned from
 * disk at load. Set `CODER_LEDGER_RESET_PENDING=1` for a one-shot operator
 * override that drops every pending reservation at boot (e.g. when a
 * crash-loop wedges admission with stale markers).
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

  constructor(
    private readonly caps: BudgetCaps = config,
    private readonly path = process.env.LEDGER_PATH?.trim() || undefined,
    private readonly now: () => number = Date.now,
  ) {
    // wallMs is the warm-recovery deadline horizon AND the gateway abort timer
    // (see config.ts). A zero/negative/NaN value silently disables both — for
    // the abort that's a fast-fail every run; for warm recovery it'd persist
    // markers that expire instantly on the next boot. Loud-fail matches the
    // existing nonNegativeInt pattern for the per-IP gate: don't ship with a
    // silently-disabled control.
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
      raw = readFileSync(this.path, "utf8");
    } catch (e) {
      // First boot is the normal ENOENT path; anything else (EACCES, EIO, a
      // tmpfs that vanished on reboot) is operator misconfiguration that
      // silently resets the daily cap — surface it instead of swallowing.
      const code = (e as NodeJS.ErrnoException).code;
      if (code !== "ENOENT") {
        console.error(
          `ledger load failed (${this.path}, code=${code ?? "unknown"}): ${(e as Error).message} — starting fresh; check LEDGER_PATH points at persistent storage`,
        );
      }
      return;
    }
    try {
      const s = JSON.parse(raw) as {
        day?: string;
        month?: string;
        dailyUsd?: number;
        monthlyUsd?: number;
        outcomes?: Partial<Record<RunOutcome, number>>;
        pending?: unknown;
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
      const pruned = this.restorePending(s.pending);
      if (this.dailyUsd > 0 || this.monthlyUsd > 0 || this.pending.length > 0) {
        console.log(
          `ledger restored from ${this.path}: $${this.dailyUsd.toFixed(4)} today, $${this.monthlyUsd.toFixed(2)} this month, ${this.pending.length} pending reservation(s)`,
        );
      }
      // Only rewrite when prune-on-load actually changed the on-disk state —
      // a crash-loop boot must not amplify disk writes on a clean ledger.
      if (pruned > 0) this.save();
    } catch (e) {
      console.error(
        `ledger parse failed (${this.path}): ${(e as Error).message} — starting fresh; the file may be truncated`,
      );
    }
  }

  /**
   * Reinstate persisted pending reservations whose wall-clock deadline is
   * still in the future, so a fresh process treats the leftover microVMs as
   * live spend instead of admitting on top of them. Operators can force a
   * clean slate at boot with CODER_LEDGER_RESET_PENDING=1 — useful when a
   * crash-loop has wedged admission with stale markers from runs that
   * already died.
   */
  private restorePending(raw: unknown): number {
    if (process.env.CODER_LEDGER_RESET_PENDING === "1") {
      const dropped = Array.isArray(raw) ? raw.length : 0;
      console.warn("CODER_LEDGER_RESET_PENDING=1 — dropping every persisted pending reservation");
      this.pending = [];
      return dropped;
    }
    if (!Array.isArray(raw)) return 0;
    const now = this.now();
    const restored: PendingEntry[] = [];
    let expired = 0;
    let dropped = 0;
    for (const entry of raw) {
      if (
        entry &&
        typeof entry === "object" &&
        typeof (entry as PendingEntry).id === "string" &&
        typeof (entry as PendingEntry).reservedMax === "number" &&
        typeof (entry as PendingEntry).deadlineEpochMs === "number"
      ) {
        const e = entry as PendingEntry;
        if (e.deadlineEpochMs > now) {
          restored.push(e);
          this.reserved += e.reservedMax;
          this.active += 1;
        } else {
          expired += 1;
        }
      } else {
        dropped += 1;
      }
    }
    this.pending = restored;
    if (restored.length > 0 || expired > 0) {
      console.warn(
        `ledger warm-recovery: reinstated ${restored.length} pending reservation(s), pruned ${expired} expired`,
      );
    }
    return expired + dropped;
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
          pending: this.pending,
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
    const id = randomUUID();
    this.pending.push({
      id,
      reservedMax: maxUsd,
      deadlineEpochMs: this.now() + this.caps.wallMs,
    });
    // Persist before returning: the whole point is that a crash AFTER admit
    // returns to the caller still has the marker on disk.
    this.save();
    return { ok: true, max: maxUsd, id };
  }

  commit(id: string, reservedMax: number, actualUsd: number, outcome: RunOutcome = "completed"): void {
    this.roll();
    this.reserved = Math.max(0, this.reserved - reservedMax);
    this.dailyUsd += actualUsd;
    this.monthlyUsd += actualUsd;
    this.active = Math.max(0, this.active - 1);
    if (outcome in this.outcomes) this.outcomes[outcome] += 1;
    // Purge the pending entry so the on-disk file doesn't grow per-run.
    const idx = this.pending.findIndex((p) => p.id === id);
    if (idx >= 0) this.pending.splice(idx, 1);
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
