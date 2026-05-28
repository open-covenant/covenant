import type { IpBucket } from "./ip-bucket.js";
import type { SpendLedger } from "./budget.js";

export type AdmitOutcome =
  | { ok: true; reservedMax: number; reservationId: string }
  | { ok: false; reason: string; retryAfterMs?: number };

export interface AdmitInputs {
  ip: string;
  ipBucket: IpBucket;
  ledger: SpendLedger;
  /**
   * `0` opts out of the per-IP gate entirely. Surfaces here (not just in
   * the bucket) so the ledger-reject release path can short-circuit when
   * the gate is off — releasing a never-taken IP slot would still be
   * idempotent but the `if` makes the contract explicit.
   */
  ipMaxPerIp: number;
}

/**
 * Atomically reserve a per-IP admission and a per-day spend slot. The
 * per-IP bucket is taken FIRST so a single anonymous client cannot
 * occupy every concurrency slot; if the spend ledger then rejects, the
 * bucket slot is released so a ledger denial does not also count
 * against the per-IP refill window.
 *
 * Extracted from `server.ts` so the take-then-ledger-then-release flow
 * is unit-testable without spinning up an HTTP server — the test seam
 * the security review of coder-12 flagged as missing.
 */
export function admitRun({ ip, ipBucket, ledger, ipMaxPerIp }: AdmitInputs): AdmitOutcome {
  if (ipMaxPerIp > 0) {
    const admission = ipBucket.take(ip);
    if (!admission.ok) {
      return { ok: false, reason: admission.reason, retryAfterMs: admission.retryAfterMs };
    }
  }
  const reservation = ledger.reserve();
  if (!reservation.ok) {
    if (ipMaxPerIp > 0) ipBucket.release(ip);
    return { ok: false, reason: reservation.reason };
  }
  return { ok: true, reservedMax: reservation.max, reservationId: reservation.id };
}
