const YEAR_SECONDS = 365 * 24 * 3600;
const MIN_HISTORY_SECONDS = 600;

export interface ApyInput {
  cumulativeSolDistributedLamports: bigint;
  initializedTsSeconds: bigint;
  nowSeconds: number;
  lockedCvntRawAmount: bigint;
  solPerCvnt: number;
}

export interface ApyResult {
  pct: number;
  annualizedSol: number;
  lockedSolValue: number;
  elapsedDays: number;
}

export function computeApy(input: ApyInput): ApyResult | null {
  const elapsed = input.nowSeconds - Number(input.initializedTsSeconds);
  if (elapsed < MIN_HISTORY_SECONDS) return null;
  if (input.lockedCvntRawAmount === 0n) return null;
  if (!Number.isFinite(input.solPerCvnt) || input.solPerCvnt <= 0) return null;

  const cumulativeSol = Number(input.cumulativeSolDistributedLamports) / 1e9;
  if (cumulativeSol <= 0) return null;

  const annualizedSol = cumulativeSol * (YEAR_SECONDS / elapsed);
  const lockedCvnt = Number(input.lockedCvntRawAmount) / 1e6;
  const lockedSolValue = lockedCvnt * input.solPerCvnt;
  if (lockedSolValue <= 0) return null;

  return {
    pct: (annualizedSol / lockedSolValue) * 100,
    annualizedSol,
    lockedSolValue,
    elapsedDays: elapsed / 86400,
  };
}

export function formatApy(pct: number): string {
  if (!Number.isFinite(pct) || pct < 0) return "—";
  if (pct >= 9999) return "9999%+";
  if (pct >= 100) return `${pct.toFixed(0)}%`;
  if (pct >= 10) return `${pct.toFixed(1)}%`;
  return `${pct.toFixed(2)}%`;
}
