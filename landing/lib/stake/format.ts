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
