export const CVNT_DECIMALS = 6;

export function formatCvnt(raw: bigint, opts: { maxFrac?: number } = {}): string {
  const maxFrac = opts.maxFrac ?? 2;
  const whole = raw / 10n ** BigInt(CVNT_DECIMALS);
  const frac = raw % 10n ** BigInt(CVNT_DECIMALS);
  const wholeFormatted = formatWithGrouping(whole);
  if (maxFrac === 0) return wholeFormatted;
  const fracStr = frac
    .toString()
    .padStart(CVNT_DECIMALS, "0")
    .slice(0, maxFrac)
    .replace(/0+$/, "");
  return fracStr ? `${wholeFormatted}.${fracStr}` : wholeFormatted;
}

export function parseCvntInput(input: string): bigint | null {
  const t = input.trim().replace(/,/g, "");
  if (!t) return null;
  if (!/^\d+(\.\d{0,6})?$/.test(t)) return null;
  const [whole, frac = ""] = t.split(".");
  const padded = (frac + "000000").slice(0, 6);
  return BigInt(whole) * 10n ** BigInt(CVNT_DECIMALS) + BigInt(padded || "0");
}

export function formatSol(lamports: bigint, opts: { maxFrac?: number } = {}): string {
  const maxFrac = opts.maxFrac ?? 4;
  const whole = lamports / 1_000_000_000n;
  const frac = lamports % 1_000_000_000n;
  const wholeFormatted = formatWithGrouping(whole);
  if (maxFrac === 0) return wholeFormatted;
  const fracStr = frac
    .toString()
    .padStart(9, "0")
    .slice(0, maxFrac)
    .replace(/0+$/, "");
  return fracStr ? `${wholeFormatted}.${fracStr}` : wholeFormatted;
}

export function formatWithGrouping(n: bigint): string {
  const s = n.toString();
  if (s.length <= 3) return s;
  return s.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

export function shortAddr(addr: string, prefix = 4, suffix = 4): string {
  if (addr.length <= prefix + suffix + 3) return addr;
  return `${addr.slice(0, prefix)}…${addr.slice(-suffix)}`;
}

export function lockEndDate(lockEnd: bigint): string {
  const ms = Number(lockEnd) * 1000;
  if (!Number.isFinite(ms) || ms <= 0) return "—";
  return new Date(ms).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
  });
}

export function tierLabel(bps: number): string {
  switch (bps) {
    case 10_000:
      return "30 days · 1.0×";
    case 15_000:
      return "90 days · 1.5×";
    case 20_000:
      return "180 days · 2.0×";
    case 30_000:
      return "365 days · 3.0×";
    default:
      return `${(bps / 10_000).toFixed(2)}×`;
  }
}

export function tierMultiplier(bps: number): string {
  return `${(bps / 10_000).toFixed(2)}×`;
}

export function formatPercent(value: number, opts: { maxFrac?: number } = {}): string {
  const maxFrac = opts.maxFrac ?? 2;
  return `${value.toFixed(maxFrac)}%`;
}

/**
 * Backward-looking annualised rate based on lifetime distributions.
 * Returns null if the protocol has no time, no distributions, or no TVL.
 *
 *   trailing_rate = (cumulative_sol / years_since_init) / tvl_in_sol
 *
 * This is NOT an APY promise — it is the rate the protocol would
 * implicitly run at IF it kept distributing at its lifetime average,
 * which is unlikely to be exact going forward.
 */
export function trailingAnnualisedRate(opts: {
  cumulativeSolLamports: bigint;
  initializedTs: bigint;
  totalWeightCvnt: bigint;
  multiplier?: number;
  solPerCvnt?: number;
}): number | null {
  const now = BigInt(Math.floor(Date.now() / 1000));
  const elapsed = Number(now - opts.initializedTs);
  if (elapsed <= 0) return null;
  if (opts.cumulativeSolLamports === 0n) return null;
  if (opts.totalWeightCvnt === 0n) return null;
  const yearsElapsed = elapsed / (365.25 * 86_400);
  const cumulativeSol = Number(opts.cumulativeSolLamports) / 1e9;
  const annualisedSol = cumulativeSol / yearsElapsed;
  // Convert TVL from raw CVNT (6 decimals) to a SOL-denominated TVL using
  // a sol/CVNT price ratio supplied by the caller (defaults to 0 which
  // makes the rate undefined and we return null).
  const tvlCvnt = Number(opts.totalWeightCvnt) / 1e6;
  if (!opts.solPerCvnt || opts.solPerCvnt <= 0) {
    // No price — return a CVNT-denominated rate instead (annualised SOL
    // per locked-CVNT-weight).
    if (tvlCvnt <= 0) return null;
    const ratePerCvntPerYear = annualisedSol / tvlCvnt;
    // Caller can scale this into a percent. We return as-is.
    return ratePerCvntPerYear;
  }
  const tvlSol = tvlCvnt * opts.solPerCvnt;
  if (tvlSol <= 0) return null;
  return (annualisedSol / tvlSol) * 100;
}

export function relativeFromNow(unixTs: bigint): string {
  const target = Number(unixTs) * 1000;
  const now = Date.now();
  const diffSec = Math.floor((target - now) / 1000);
  if (diffSec <= 0) return "Unlocked";
  const days = Math.floor(diffSec / 86_400);
  if (days > 0) return `${days}d remaining`;
  const hours = Math.floor(diffSec / 3600);
  if (hours > 0) return `${hours}h remaining`;
  const mins = Math.floor(diffSec / 60);
  return `${mins}m remaining`;
}
