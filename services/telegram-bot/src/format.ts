// Render the "NEW STAKE" announcement. Telegram HTML parse mode: only the
// numeric/text fields and our own URLs are interpolated (all bot-controlled),
// but everything still routes through escapeHtml as defense in depth.

import { lockTierLabel } from "./chain/constants.js";

export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function groupThousands(digits: string): string {
  return digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/**
 * Format a base-unit token amount as a grouped decimal string with exactly
 * `fractionDigits` places, rounded half-up. Pure bigint math so billion-token
 * figures never lose precision to floats.
 */
export function formatTokenAmount(
  raw: bigint,
  decimals: number,
  fractionDigits: number,
): string {
  const value = raw < 0n ? 0n : raw;
  const fd = Math.max(0, Math.min(fractionDigits, decimals));
  const dropExp = decimals - fd;
  const dropFactor = 10n ** BigInt(dropExp);
  const scaled =
    dropFactor > 1n ? (value + dropFactor / 2n) / dropFactor : value;
  const denom = 10n ** BigInt(fd);
  const whole = scaled / denom;
  const frac = scaled % denom;
  const wholeStr = groupThousands(whole.toString());
  if (fd === 0) return wholeStr;
  return `${wholeStr}.${frac.toString().padStart(fd, "0")}`;
}

/** Bar length for a stake: 1 per `unitUi` whole tokens, clamped to 1..max. */
export function barCount(
  raw: bigint,
  decimals: number,
  unitUi: number,
  max: number,
): number {
  const base = 10n ** BigInt(decimals);
  const wholeUi = Number(raw / base);
  const unit = unitUi > 0 ? unitUi : 1;
  let n = Math.round(wholeUi / unit);
  if (!Number.isFinite(n) || n < 1) n = 1;
  if (n > max) n = max;
  return n;
}

/** A row of 🔥 scaled to the stake size — the fallback when no logo emoji is set. */
export function fireBar(
  raw: bigint,
  decimals: number,
  unitUi: number,
  max = 50,
): string {
  return "🔥".repeat(barCount(raw, decimals, unitUi, max));
}

/** Default cap for the branded custom-emoji bar — lower than fire since the
 * logo chips read denser and a spaced row past ~12 starts to wrap. */
export const LOGO_BAR_MAX = 12;

/**
 * A space-separated row of the Covenant custom emoji, scaled to stake size.
 * `emojiId` is a Telegram custom_emoji_id from a set the bot owns; the inner
 * 🔥 is the fallback a client shows only if it can't render the custom emoji.
 */
export function logoBar(
  raw: bigint,
  decimals: number,
  unitUi: number,
  emojiId: string,
  max = LOGO_BAR_MAX,
): string {
  const one = `<tg-emoji emoji-id="${escapeHtml(emojiId)}">🔥</tg-emoji>`;
  return Array.from({ length: barCount(raw, decimals, unitUi, max) }, () => one).join(" ");
}

export function solscanTxUrl(
  base: string,
  signature: string,
  cluster: string,
): string {
  const root = base.replace(/\/+$/, "");
  const path = `${root}/tx/${encodeURIComponent(signature)}`;
  if (cluster === "mainnet-beta" || cluster === "mainnet") return path;
  return `${path}?cluster=${encodeURIComponent(cluster)}`;
}

export interface NewStakeMessage {
  /** Staked principal, base units. */
  amountRaw: bigint;
  decimals: number;
  multiplierBps: number;
  /** Program-wide figures; omitted from the body when null (e.g. RPC failed). */
  totals: { totalStakedRaw: bigint; pct: number } | null;
  txSignature: string;
  /** Explorer cluster id (`mainnet-beta` | `devnet` | ...). */
  cluster: string;
  symbol: string;
  stakeUrl: string;
  solscanBase: string;
  fireUnit: number;
  /** Telegram custom_emoji_id for the branded bar; falls back to 🔥 when unset. */
  emojiId?: string;
  /** When the post leads with a header image that already says "NEW STAKE",
   * drop the redundant title line (the bar + body stay). */
  bannerMode?: boolean;
}

export function renderNewStake(m: NewStakeMessage): string {
  const sym = escapeHtml(m.symbol);
  const amount = formatTokenAmount(m.amountRaw, m.decimals, 0);
  const lock = lockTierLabel(m.multiplierBps);
  const bar = m.emojiId
    ? logoBar(m.amountRaw, m.decimals, m.fireUnit, m.emojiId)
    : fireBar(m.amountRaw, m.decimals, m.fireUnit);
  const solscan = escapeHtml(solscanTxUrl(m.solscanBase, m.txSignature, m.cluster));
  const stake = escapeHtml(m.stakeUrl);

  const lines: string[] = [];
  if (!m.bannerMode) lines.push("<b>NEW STAKE</b>", "");
  lines.push(
    bar,
    "",
    `${escapeHtml(amount)} $${sym} · ${escapeHtml(lock)} lock`,
  );
  if (m.totals) {
    const total = formatTokenAmount(m.totals.totalStakedRaw, m.decimals, 2);
    lines.push(`Total staked: ${escapeHtml(total)} $${sym}`);
    lines.push(`${escapeHtml(m.totals.pct.toFixed(1))}% of supply staked`);
  }
  lines.push("");
  lines.push(
    `<a href="${solscan}">View on Solscan</a> · <a href="${stake}">Stake $${sym} →</a>`,
  );
  return lines.join("\n");
}
